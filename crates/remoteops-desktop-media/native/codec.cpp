#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <windows.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mftransform.h>
#include <mferror.h>
#include <strmif.h>
#include <initguid.h>
#include <codecapi.h>
#include <wrl/client.h>

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <deque>
#include <limits>
#include <memory>
#include <utility>
#include <vector>

#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "mf.lib")
#pragma comment(lib, "mfplat.lib")
#pragma comment(lib, "mfuuid.lib")
#pragma comment(lib, "strmiids.lib")

namespace {

thread_local HRESULT lastStartupError = S_OK;

using Microsoft::WRL::ComPtr;
using Bytes = std::vector<BYTE>;
using U32 = std::uint32_t;

constexpr GUID kH264Encoder{
    0x6ca50344, 0x051a, 0x4ded, {0x97, 0x79, 0xa4, 0x33, 0x05, 0x16, 0x5e, 0x35}
};
constexpr GUID kH264Decoder{
    0x62ce7e72, 0x4c71, 0x4d20, {0xb1, 0x5d, 0x45, 0x28, 0x31, 0xa8, 0x7d, 0x9d}
};

constexpr int kMaxDecodeWidth = 1920;
constexpr int kMaxDecodeHeight = 1080;
constexpr DWORD kMaxPacket = 16u * 1024u * 1024u;
constexpr DWORD kMaxRawBuffer = 32u * 1024u * 1024u;
constexpr std::size_t kMaxParameterSet = 16u * 1024u;
constexpr std::size_t kMaxNals = 16384;
constexpr std::size_t kMaxQueuedFrames = 8;
constexpr LONGLONG kTicksPerSecond = 10'000'000;
constexpr BYTE kStartCode[] = {0, 0, 0, 1};

struct Nal {
    const BYTE* data;
    std::size_t size;

    unsigned Type() const noexcept { return data[0] & 31u; }
};

std::size_t FindStartCode(const BYTE* data, std::size_t size,
                          std::size_t from) noexcept {
    for (std::size_t i = from; i + 2 < size; ++i) {
        if (data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1) {
            return i;
        }
    }
    return size;
}

bool SplitAnnexB(const BYTE* data, std::size_t size, std::vector<Nal>& nals) {
    nals.clear();
    std::size_t prefix = FindStartCode(data, size, 0);
    if (prefix == size) return false;
    for (std::size_t i = 0; i < prefix; ++i) {
        if (data[i] != 0) return false;
    }

    while (prefix != size) {
        const std::size_t begin = prefix + 3;
        const std::size_t next = FindStartCode(data, size, begin);
        std::size_t end = next;
        while (end > begin && data[end - 1] == 0) --end;

        // 保留防竞争字节之后属于 NAL 的零字节。
        if (end < next && end >= begin + 4 &&
            data[end - 3] == 0 && data[end - 2] == 0 && data[end - 1] == 3) {
            ++end;
        }

        if (end <= begin || nals.size() == kMaxNals) return false;
        nals.push_back({data + begin, end - begin});
        prefix = next;
    }
    return true;
}

bool SplitLengthPrefixed(const BYTE* data, std::size_t size, unsigned lengthBytes,
                         std::vector<Nal>& nals) {
    nals.clear();
    std::size_t pos = 0;
    while (pos < size) {
        if (size - pos < lengthBytes || nals.size() == kMaxNals) return false;
        U32 length = 0;
        for (unsigned i = 0; i < lengthBytes; ++i) {
            length = (length << 8) | data[pos++];
        }
        if (length == 0 || length > size - pos) return false;
        nals.push_back({data + pos, length});
        pos += length;
    }
    return !nals.empty();
}

bool AppendNal(Bytes& packet, const BYTE* data, std::size_t size) {
    if (packet.size() > kMaxPacket - sizeof(kStartCode) ||
        size > kMaxPacket - sizeof(kStartCode) - packet.size()) {
        return false;
    }
    packet.insert(packet.end(), kStartCode, kStartCode + sizeof(kStartCode));
    packet.insert(packet.end(), data, data + size);
    return true;
}

bool ValidEbsp(const Nal& nal) noexcept {
    if ((nal.data[0] & 0x80u) != 0) return false;
    unsigned zeros = 0;
    for (std::size_t i = 1; i < nal.size; ++i) {
        const BYTE value = nal.data[i];
        if (zeros == 2) {
            if (value < 3) return false;
            if (value == 3) {
                if (i + 1 == nal.size || nal.data[i + 1] > 3) return false;
                zeros = 0;
                continue;
            }
        }
        zeros = value == 0 ? zeros + 1 : 0;
    }
    return true;
}

class Rbsp {
public:
    explicit Rbsp(const Nal& nal) noexcept : pos_(nal.data + 1), end_(nal.data + nal.size) {}

    bool Read(unsigned count, U32& value) noexcept {
        value = 0;
        while (count-- != 0) {
            if (left_ == 0) {
                if (pos_ == end_) return false;
                BYTE byte = *pos_++;
                if (zeros_ == 2 && byte == 3) {
                    if (pos_ == end_) return false;
                    byte = *pos_++;
                    zeros_ = 0;
                }
                zeros_ = byte == 0 ? zeros_ + 1 : 0;
                byte_ = byte;
                left_ = 8;
            }
            value = (value << 1) | ((byte_ >> --left_) & 1u);
        }
        return true;
    }

    bool Skip(unsigned count) noexcept {
        U32 unused;
        return Read(count, unused);
    }

    bool Flag(bool& value) noexcept {
        U32 bit;
        if (!Read(1, bit)) return false;
        value = bit != 0;
        return true;
    }

    bool Ue(U32& value, U32 limit) noexcept {
        unsigned zeros = 0;
        U32 bit;
        for (;;) {
            if (!Read(1, bit)) return false;
            if (bit != 0) break;
            if (++zeros > 31) return false;
        }
        U32 suffix;
        if (!Read(zeros, suffix)) return false;
        const std::uint64_t decoded = (std::uint64_t{1} << zeros) - 1 + suffix;
        if (decoded > limit) return false;
        value = static_cast<U32>(decoded);
        return true;
    }

    bool Se(int& value, U32 limit) noexcept {
        U32 code;
        if (!Ue(code, 2 * limit)) return false;
        value = (code & 1u) ? static_cast<int>((code + 1) / 2)
                             : -static_cast<int>(code / 2);
        return true;
    }

private:
    const BYTE* pos_;
    const BYTE* end_;
    unsigned zeros_ = 0;
    unsigned left_ = 0;
    unsigned byte_ = 0;
};

struct Picture {
    int codedWidth = 0;
    int codedHeight = 0;
    int width = 0;
    int height = 0;
    int left = 0;
    int top = 0;
    int matrix = -1; // H.264 VUI 矩阵系数
    int fullRange = -1;
};

bool SamePicture(const Picture& a, const Picture& b) noexcept {
    return a.codedWidth == b.codedWidth && a.codedHeight == b.codedHeight &&
           a.width == b.width && a.height == b.height &&
           a.left == b.left && a.top == b.top &&
           a.matrix == b.matrix && a.fullRange == b.fullRange;
}

bool SkipScalingList(Rbsp& bits, int count) noexcept {
    int last = 8;
    int next = 8;
    for (int i = 0; i < count; ++i) {
        if (next != 0) {
            int delta;
            if (!bits.Se(delta, 128) || delta == 128) return false;
            next = (last + delta + 256) % 256;
        }
        if (next != 0) last = next;
    }
    return true;
}

bool SkipHrd(Rbsp& bits) noexcept {
    U32 cpbCountMinusOne;
    U32 unused;
    if (!bits.Ue(cpbCountMinusOne, 31) || !bits.Skip(8)) return false;
    for (U32 i = 0; i <= cpbCountMinusOne; ++i) {
        if (!bits.Ue(unused, 0x7fffffffu) ||
            !bits.Ue(unused, 0x7fffffffu) || !bits.Skip(1)) {
            return false;
        }
    }
    return bits.Skip(20);
}

bool ReadVui(Rbsp& bits, Picture& picture, U32 references) noexcept {
    bool present;
    U32 value;

    if (!bits.Flag(present)) return false;
    if (present) {
        if (!bits.Read(8, value)) return false;
        if (value == 255 && !bits.Skip(32)) return false;
    }
    if (!bits.Flag(present) || (present && !bits.Skip(1))) return false;
    if (!bits.Flag(present)) return false;
    if (present) {
        bool fullRange;
        bool colorDescription;
        if (!bits.Skip(3) || !bits.Flag(fullRange) || !bits.Flag(colorDescription)) {
            return false;
        }
        picture.fullRange = fullRange ? 1 : 0;
        if (colorDescription) {
            if (!bits.Skip(16) || !bits.Read(8, value)) return false;
            picture.matrix = static_cast<int>(value);
        }
    }
    if (!bits.Flag(present)) return false;
    if (present && (!bits.Ue(value, 5) || !bits.Ue(value, 5))) return false;
    if (!bits.Flag(present)) return false;
    if (present) {
        U32 units;
        U32 scale;
        if (!bits.Read(32, units) || !bits.Read(32, scale) ||
            units == 0 || scale == 0 || !bits.Skip(1)) {
            return false;
        }
    }

    bool nalHrd;
    bool vclHrd;
    if (!bits.Flag(nalHrd) || (nalHrd && !SkipHrd(bits)) ||
        !bits.Flag(vclHrd) || (vclHrd && !SkipHrd(bits))) {
        return false;
    }
    if ((nalHrd || vclHrd) && !bits.Skip(1)) return false;
    if (!bits.Skip(1) || !bits.Flag(present)) return false;
    if (present) {
        U32 reorder;
        U32 buffering;
        if (!bits.Skip(1)) return false;
        for (int i = 0; i < 4; ++i) {
            if (!bits.Ue(value, 16)) return false;
        }
        if (!bits.Ue(reorder, 16) || !bits.Ue(buffering, 16) ||
            buffering < references || buffering < reorder) {
            return false;
        }
    }
    return true;
}

// 在提交给 MFT 前检查宏块尺寸、裁剪、位深和参考帧上限。
bool ReadSps(const Nal& nal, int maxWidth, int maxHeight, U32& id,
             Picture& picture) noexcept {
    Rbsp bits(nal);
    U32 profile;
    U32 constraints;
    U32 value;
    if (!bits.Read(8, profile) || !bits.Read(8, constraints) ||
        (constraints & 3u) != 0 || !bits.Skip(8) || !bits.Ue(id, 31)) {
        return false;
    }
    if (profile == 100) {
        bool scaling;
        if (!bits.Ue(value, 1) || value != 1 ||
            !bits.Ue(value, 0) || !bits.Ue(value, 0) ||
            !bits.Skip(1) || !bits.Flag(scaling)) {
            return false;
        }
        if (scaling) {
            for (int i = 0; i < 8; ++i) {
                bool present;
                if (!bits.Flag(present) ||
                    (present && !SkipScalingList(bits, i < 6 ? 16 : 64))) {
                    return false;
                }
            }
        }
    } else if (profile != 66 && profile != 77 && profile != 88) {
        return false;
    }

    U32 pocType;
    if (!bits.Ue(value, 12) || !bits.Ue(pocType, 2)) return false;
    if (pocType == 0 && !bits.Ue(value, 12)) return false;
    if (pocType == 1) {
        int offset;
        U32 cycle;
        constexpr U32 kMaxPocOffset = 0x7fffffffu;
        if (!bits.Skip(1) || !bits.Se(offset, kMaxPocOffset) ||
            !bits.Se(offset, kMaxPocOffset) || !bits.Ue(cycle, 255)) {
            return false;
        }
        for (U32 i = 0; i < cycle; ++i) {
            if (!bits.Se(offset, kMaxPocOffset)) return false;
        }
    }

    U32 references;
    U32 columns;
    U32 rows;
    bool progressive;
    bool crop;
    if (!bits.Ue(references, 16) || !bits.Skip(1) ||
        !bits.Ue(columns, static_cast<U32>((maxWidth - 1) / 16)) ||
        !bits.Ue(rows, static_cast<U32>((maxHeight - 1) / 16)) ||
        !bits.Flag(progressive) || !progressive ||
        !bits.Skip(1) || !bits.Flag(crop)) {
        return false;
    }

    picture.codedWidth = static_cast<int>((columns + 1) * 16);
    picture.codedHeight = static_cast<int>((rows + 1) * 16);
    U32 left = 0;
    U32 right = 0;
    U32 top = 0;
    U32 bottom = 0;
    if (crop) {
        const auto xLimit = static_cast<U32>(picture.codedWidth / 2 - 1);
        const auto yLimit = static_cast<U32>(picture.codedHeight / 2 - 1);
        if (!bits.Ue(left, xLimit) || !bits.Ue(right, xLimit) ||
            !bits.Ue(top, yLimit) || !bits.Ue(bottom, yLimit)) {
            return false;
        }
    }
    picture.left = static_cast<int>(2 * left);
    picture.top = static_cast<int>(2 * top);
    picture.width = picture.codedWidth - static_cast<int>(2 * (left + right));
    picture.height = picture.codedHeight - static_cast<int>(2 * (top + bottom));
    if (picture.width <= 0 || picture.height <= 0 ||
        picture.width > maxWidth || picture.height > maxHeight) {
        return false;
    }

    bool vui;
    return bits.Flag(vui) && (!vui || ReadVui(bits, picture, references));
}

struct Sps {
    Picture picture;
    Bytes bytes;
};

struct Pps {
    U32 spsId = 0;
    Bytes bytes;
};

bool ReadPps(const Nal& nal, U32& id, U32& spsId) noexcept {
    Rbsp bits(nal);
    U32 value;
    return bits.Ue(id, 255) && bits.Ue(spsId, 31) && bits.Skip(2) &&
           bits.Ue(value, 0) && bits.Ue(value, 31) && bits.Ue(value, 31);
}

bool ReadSlicePrefix(const Nal& nal, U32& ppsId, U32& firstMb) noexcept {
    Rbsp bits(nal);
    U32 sliceType;
    return bits.Ue(firstMb, 8191) && bits.Ue(sliceType, 9) &&
           bits.Ue(ppsId, 255);
}

bool IsHd(int width, int height) noexcept {
    return width >= 1280 || height > 576;
}

// 输入指针指向逻辑首行，stride 允许为负。
void BgraToNv12(const BYTE* bgra, std::ptrdiff_t stride, BYTE* nv12,
                int width, int height) noexcept {
    const bool hd = IsHd(width, height);
    const int yr = hd ? 187 : 263;
    const int yg = hd ? 629 : 516;
    const int yb = hd ? 63 : 100;
    const int ur = hd ? -103 : -152;
    const int ug = hd ? -347 : -298;
    const int vg = hd ? -409 : -377;
    const int vb = hd ? -41 : -73;
    BYTE* uv = nv12 + static_cast<std::size_t>(width) * height;

    for (int y = 0; y < height; y += 2) {
        const BYTE* first = bgra + static_cast<std::ptrdiff_t>(y) * stride;
        const BYTE* second = first + stride;
        BYTE* y0 = nv12 + static_cast<std::size_t>(y) * width;
        BYTE* y1 = y0 + width;
        BYTE* chroma = uv + static_cast<std::size_t>(y / 2) * width;

        for (int x = 0; x < width; x += 2) {
            const BYTE* a = first + 4 * x;
            const BYTE* b = a + 4;
            const BYTE* c = second + 4 * x;
            const BYTE* d = c + 4;
            const auto luma = [&](const BYTE* pixel) noexcept {
                return static_cast<BYTE>(
                    16 + (yr * pixel[2] + yg * pixel[1] + yb * pixel[0] + 512) / 1024);
            };
            y0[x] = luma(a);
            y0[x + 1] = luma(b);
            y1[x] = luma(c);
            y1[x + 1] = luma(d);

            const int red = a[2] + b[2] + c[2] + d[2];
            const int green = a[1] + b[1] + c[1] + d[1];
            const int blue = a[0] + b[0] + c[0] + d[0];
            constexpr int bias = 128 * 4096 + 2048;
            chroma[x] = static_cast<BYTE>((bias + ur * red + ug * green + 450 * blue) / 4096);
            chroma[x + 1] = static_cast<BYTE>((bias + 450 * red + vg * green + vb * blue) / 4096);
        }
    }
}

struct Color {
    int yOffset;
    int yScale;
    int redV;
    int greenU;
    int greenV;
    int blueU;
};

Color DecodeColor(int matrix, bool fullRange) noexcept {
    switch (matrix) {
    case 1:
        return fullRange ? Color{0, 1024, 1613, -192, -479, 1900}
                         : Color{16, 1192, 1836, -218, -546, 2163};
    case 7:
        return fullRange ? Color{0, 1024, 1614, -232, -488, 1871}
                         : Color{16, 1192, 1838, -264, -556, 2130};
    case 9:
        return fullRange ? Color{0, 1024, 1510, -169, -585, 1927}
                         : Color{16, 1192, 1719, -192, -666, 2193};
    default:
        return fullRange ? Color{0, 1024, 1436, -352, -731, 1815}
                         : Color{16, 1192, 1634, -401, -833, 2066};
    }
}

BYTE ColorByte(int value) noexcept {
    return static_cast<BYTE>(std::clamp((value + 512) / 1024, 0, 255));
}

struct Layout {
    int frameWidth = 0;
    int frameHeight = 0;
    int stride = 0;
    int left = 0;
    int top = 0;
    Color color{};
};

bool DescribeNv12(IMFMediaType* type, const Picture& picture, Layout& layout) noexcept {
    GUID subtype{};
    UINT32 width;
    UINT32 height;
    if (FAILED(type->GetGUID(MF_MT_SUBTYPE, &subtype)) || subtype != MFVideoFormat_NV12 ||
        FAILED(MFGetAttributeSize(type, MF_MT_FRAME_SIZE, &width, &height))) {
        return false;
    }
    if ((width != static_cast<UINT32>(picture.width) &&
         width != static_cast<UINT32>(picture.codedWidth)) ||
        (height != static_cast<UINT32>(picture.height) &&
         height != static_cast<UINT32>(picture.codedHeight))) {
        return false;
    }
    layout.frameWidth = static_cast<int>(width);
    layout.frameHeight = static_cast<int>(height);
    layout.left = layout.frameWidth == picture.width ? 0 : picture.left;
    layout.top = layout.frameHeight == picture.height ? 0 : picture.top;

    const GUID* apertureKeys[] = {&MF_MT_MINIMUM_DISPLAY_APERTURE, &MF_MT_GEOMETRIC_APERTURE};
    for (const GUID* key : apertureKeys) {
        MFVideoArea area{};
        UINT32 length = 0;
        if (SUCCEEDED(type->GetBlob(*key, reinterpret_cast<UINT8*>(&area),
                                    static_cast<UINT32>(sizeof(area)), &length)) &&
            length == sizeof(area) && area.Area.cx == picture.width &&
            area.Area.cy == picture.height && area.OffsetX.fract == 0 &&
            area.OffsetY.fract == 0) {
            const int x = area.OffsetX.value;
            const int y = area.OffsetY.value;
            if (x >= 0 && y >= 0 && (x & 1) == 0 && (y & 1) == 0 &&
                x <= layout.frameWidth - picture.width &&
                y <= layout.frameHeight - picture.height) {
                layout.left = x;
                layout.top = y;
                break;
            }
        }
    }

    UINT32 stride = width;
    const HRESULT strideResult = type->GetUINT32(MF_MT_DEFAULT_STRIDE, &stride);
    if (FAILED(strideResult) || stride == 0) stride = width;
    if (stride < width || stride > 16384 || (stride & 1u) != 0) return false;
    layout.stride = static_cast<int>(stride);

    int matrix = picture.matrix;
    if (matrix < 0 || matrix == 2) {
        UINT32 attribute = 0;
        type->GetUINT32(MF_MT_YUV_MATRIX, &attribute);
        if (attribute == MFVideoTransferMatrix_BT709) {
            matrix = 1;
        } else if (attribute == MFVideoTransferMatrix_BT601) {
            matrix = 6;
        } else if (attribute == MFVideoTransferMatrix_SMPTE240M) {
            matrix = 7;
        } else {
            matrix = IsHd(picture.width, picture.height) ? 1 : 6;
        }
    }

    UINT32 nominalRange = 0;
    type->GetUINT32(MF_MT_VIDEO_NOMINAL_RANGE, &nominalRange);
    const bool fullRange = picture.fullRange >= 0 ? picture.fullRange == 1
                                                   : nominalRange == MFNominalRange_0_255;
    layout.color = DecodeColor(matrix, fullRange);
    return true;
}

void Nv12ToBgra(const BYTE* nv12, int stride, const Layout& layout,
                const Picture& picture, BYTE* bgra) noexcept {
    const BYTE* uv = nv12 + static_cast<std::size_t>(stride) * layout.frameHeight;
    const Color& color = layout.color;
    for (int y = 0; y < picture.height; ++y) {
        const int row = layout.top + y;
        const BYTE* luma = nv12 + static_cast<std::size_t>(row) * stride + layout.left;
        const BYTE* chroma = uv + static_cast<std::size_t>(row / 2) * stride + layout.left;
        for (int x = 0; x < picture.width; ++x) {
            const int u = chroma[x & ~1] - 128;
            const int v = chroma[(x & ~1) + 1] - 128;
            const int luminance = (luma[x] - color.yOffset) * color.yScale;
            *bgra++ = ColorByte(luminance + color.blueU * u);
            *bgra++ = ColorByte(luminance + color.greenU * u + color.greenV * v);
            *bgra++ = ColorByte(luminance + color.redV * v);
            *bgra++ = 255;
        }
    }
}

class BufferLock {
public:
    explicit BufferLock(IMFMediaBuffer* buffer) noexcept : buffer_(buffer) {}
    ~BufferLock() { if (locked_) buffer_->Unlock(); }

    BufferLock(const BufferLock&) = delete;
    BufferLock& operator=(const BufferLock&) = delete;

    bool Open() noexcept {
        locked_ = SUCCEEDED(buffer_->Lock(&data, &capacity, &length));
        return locked_ && data != nullptr;
    }

    BYTE* data = nullptr;
    DWORD capacity = 0;
    DWORD length = 0;

private:
    IMFMediaBuffer* buffer_;
    bool locked_ = false;
};

bool MakeSample(DWORD size, DWORD alignment, ComPtr<IMFSample>& sample,
                ComPtr<IMFMediaBuffer>& buffer) noexcept {
    if (size == 0 || alignment > 4096) return false;
    DWORD mask = alignment;
    if (mask != 0 && (mask & (mask - 1)) == 0) --mask;
    if ((mask & (mask + 1)) != 0) return false;
    mask = std::max<DWORD>(mask, 15);
    return SUCCEEDED(MFCreateSample(sample.ReleaseAndGetAddressOf())) &&
           SUCCEEDED(MFCreateAlignedMemoryBuffer(size, mask, buffer.ReleaseAndGetAddressOf())) &&
           SUCCEEDED(sample->AddBuffer(buffer.Get()));
}

bool CopyToSample(const Bytes& data, ComPtr<IMFSample>& sample) noexcept {
    if (data.empty() || data.size() > kMaxPacket) return false;
    ComPtr<IMFMediaBuffer> buffer;
    const DWORD length = static_cast<DWORD>(data.size());
    if (!MakeSample(length, 0, sample, buffer)) return false;
    {
        BufferLock lock(buffer.Get());
        if (!lock.Open() || lock.capacity < length) return false;
        std::memcpy(lock.data, data.data(), length);
    }
    return SUCCEEDED(buffer->SetCurrentLength(length));
}

bool VideoType(REFGUID subtype, int width, int height, int fps,
               ComPtr<IMFMediaType>& type) noexcept {
    return SUCCEEDED(MFCreateMediaType(type.ReleaseAndGetAddressOf())) &&
           SUCCEEDED(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video)) &&
           SUCCEEDED(type->SetGUID(MF_MT_SUBTYPE, subtype)) &&
           SUCCEEDED(MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, width, height)) &&
           SUCCEEDED(MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, fps, 1)) &&
           SUCCEEDED(MFSetAttributeRatio(type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1)) &&
           SUCCEEDED(type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive));
}

void SetEncoderColor(IMFMediaType* type, int width, int height) noexcept {
    type->SetUINT32(MF_MT_YUV_MATRIX, IsHd(width, height) ? MFVideoTransferMatrix_BT709
                                                         : MFVideoTransferMatrix_BT601);
    type->SetUINT32(MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235);
}

bool Notify(IMFTransform* transform, MFT_MESSAGE_TYPE message, ULONG_PTR value = 0) noexcept {
    const HRESULT hr = transform->ProcessMessage(message, value);
    return SUCCEEDED(hr) || hr == E_NOTIMPL;
}

void SetCodecUInt(ICodecAPI* api, const GUID& key, ULONG value) noexcept {
    if (api == nullptr) return;
    VARIANT setting{};
    setting.vt = VT_UI4;
    setting.ulVal = value;
    api->SetValue(&key, &setting);
}

struct Output {
    Bytes bytes;
    int width = 0;
    int height = 0;
};

struct Codec {
    Codec(bool isEncoder, int w, int h, int frameRate, int rate) noexcept
        : encode(isEncoder), width(w), height(h), fps(frameRate), bitrate(rate) {}

    ~Codec() {
        CloseTransform();
        if (mfStarted) MFShutdown();
        if (comStarted) CoUninitialize();
    }

    bool Initialize() {
        lastStartupError = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        if (FAILED(lastStartupError)) return false;
        comStarted = true;
        lastStartupError = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        if (FAILED(lastStartupError)) return false;
        mfStarted = true;
        return !encode || OpenEncoder();
    }

    void CloseTransform() noexcept {
        if (transform && notifiedBegin) {
            Notify(transform.Get(), MFT_MESSAGE_NOTIFY_END_OF_STREAM, inputId);
            Notify(transform.Get(), MFT_MESSAGE_COMMAND_FLUSH);
            Notify(transform.Get(), MFT_MESSAGE_NOTIFY_END_STREAMING);
        }
        codecApi.Reset();
        hardwareEvents.Reset();
        needInputEvents = 0;
        transform.Reset();
        if (activation) { activation->ShutdownObject(); activation.Reset(); }
        notifiedBegin = false;
        firstInput = true;
        needIdr = true;
        framesSinceRequest = 0;
        avcLengthBytes = 0;
        encoderSps.clear();
        encoderPps.clear();
        sentSps.clear();
        sentPps.clear();
    }

    void ResetPipeline() noexcept {
        CloseTransform();
        ready.clear();
    }

    bool Activate(REFCLSID clsid) noexcept {
        // 使用支持同步调用的 Windows 系统软件 MFT。
        if (FAILED(CoCreateInstance(clsid, nullptr, CLSCTX_INPROC_SERVER,
                                    IID_PPV_ARGS(transform.ReleaseAndGetAddressOf())))) {
            return false;
        }

        return ConfigureTransform();
    }

    bool ConfigureTransform() noexcept {
        ComPtr<IMFAttributes> attributes;
        if (SUCCEEDED(transform->GetAttributes(attributes.GetAddressOf())) && attributes) {
            UINT32 asynchronous = 0;
            attributes->GetUINT32(MF_TRANSFORM_ASYNC, &asynchronous);
            if (asynchronous != 0 && (FAILED(attributes->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE)) ||
                FAILED(transform.As(&hardwareEvents)))) return false;
            attributes->SetUINT32(MF_LOW_LATENCY, TRUE);
        }
        DWORD inputs;
        DWORD outputs;
        if (FAILED(transform->GetStreamCount(&inputs, &outputs)) ||
            inputs != 1 || outputs != 1) {
            return false;
        }
        inputId = 0;
        outputId = 0;
        const HRESULT ids = transform->GetStreamIDs(1, &inputId, 1, &outputId);
        if (FAILED(ids) && ids != E_NOTIMPL) return false;
        if (ids == E_NOTIMPL) inputId = outputId = 0;

        transform.As(&codecApi);
        if (codecApi) {
            VARIANT lowLatency{};
            lowLatency.vt = VT_BOOL;
            lowLatency.boolVal = VARIANT_TRUE;
            codecApi->SetValue(&CODECAPI_AVLowLatencyMode, &lowLatency);
        }
        return true;
    }

    bool Begin() noexcept {
        if (!Notify(transform.Get(), MFT_MESSAGE_NOTIFY_BEGIN_STREAMING)) return false;
        notifiedBegin = true;
        return Notify(transform.Get(), MFT_MESSAGE_NOTIFY_START_OF_STREAM);
    }

    bool SetEncoderOutput() {
        ComPtr<IMFMediaType> output;
        if (!VideoType(MFVideoFormat_H264, width, height, fps, output) ||
            FAILED(output->SetUINT32(MF_MT_AVG_BITRATE, static_cast<UINT32>(bitrate))) ||
            FAILED(output->SetUINT32(MF_MT_MPEG2_PROFILE, 66))) {
            return false;
        }
        SetEncoderColor(output.Get(), width, height);
        return SUCCEEDED(transform->SetOutputType(outputId, output.Get(), 0));
    }

    bool OpenEncoder() {
        if (!skipHardware) {
            MFT_REGISTER_TYPE_INFO input{MFMediaType_Video, MFVideoFormat_NV12};
            MFT_REGISTER_TYPE_INFO output{MFMediaType_Video, MFVideoFormat_H264};
            IMFActivate** devices = nullptr;
            UINT32 count = 0;
            if (SUCCEEDED(MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER,
                    MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, &input, &output, &devices, &count))) {
                bool opened = false;
                for (UINT32 i = 0; i < count; ++i) {
                    if (!opened) {
                        activation = devices[i];
                        opened = SUCCEEDED(activation->ActivateObject(IID_PPV_ARGS(transform.GetAddressOf()))) &&
                            ConfigureTransform() && ConfigureEncoder();
                        if (!opened) CloseTransform();
                    }
                    devices[i]->Release();
                }
                CoTaskMemFree(devices);
                if (opened) { lastHardwareOutput = GetTickCount64(); return true; }
            }
            skipHardware = true;
        }
        return Activate(kH264Encoder) && ConfigureEncoder();
    }

    bool ConfigureEncoder() {
        SetCodecUInt(codecApi.Get(), CODECAPI_AVEncMPVGOPSize, static_cast<ULONG>(fps * 2));
        if (!SetEncoderOutput()) return false;

        ComPtr<IMFMediaType> input;
        if (!VideoType(MFVideoFormat_NV12, width, height, fps, input) ||
            FAILED(input->SetUINT32(MF_MT_DEFAULT_STRIDE, static_cast<UINT32>(width)))) {
            return false;
        }
        SetEncoderColor(input.Get(), width, height);
        if (FAILED(transform->SetInputType(inputId, input.Get(), 0))) return false;

        SetCodecUInt(codecApi.Get(), CODECAPI_AVEncMPVGOPSize, static_cast<ULONG>(fps * 2));
        RefreshEncoderHeaders();
        return Begin();
    }

    bool UseDecoderOutput(IMFMediaType* type) noexcept {
        Layout candidate{};
        if (!DescribeNv12(type, picture, candidate) ||
            FAILED(transform->SetOutputType(outputId, type, 0))) {
            return false;
        }
        ComPtr<IMFMediaType> current;
        if (SUCCEEDED(transform->GetOutputCurrentType(outputId, current.GetAddressOf())) &&
            current && !DescribeNv12(current.Get(), picture, candidate)) {
            return false;
        }
        layout = candidate;
        return true;
    }

    bool SetDecoderOutput(bool initial) noexcept {
        for (DWORD index = 0; index < 32; ++index) {
            ComPtr<IMFMediaType> type;
            const HRESULT hr = transform->GetOutputAvailableType(outputId, index, type.GetAddressOf());
            if (hr == MF_E_NO_MORE_TYPES || hr == MF_E_TRANSFORM_TYPE_NOT_SET ||
                hr == E_NOTIMPL) {
                break;
            }
            if (FAILED(hr)) return false;
            if (type && UseDecoderOutput(type.Get())) return true;
        }

        ComPtr<IMFMediaType> type;
        return initial &&
               VideoType(MFVideoFormat_NV12, picture.codedWidth, picture.codedHeight, fps, type) &&
               UseDecoderOutput(type.Get());
    }

    bool OpenDecoder(const Picture& next) noexcept {
        picture = next;
        if (!Activate(kH264Decoder)) return false;
        ComPtr<IMFMediaType> input;
        return VideoType(MFVideoFormat_H264, next.codedWidth, next.codedHeight, fps, input) &&
               SUCCEEDED(transform->SetInputType(inputId, input.Get(), 0)) &&
               SetDecoderOutput(true) && Begin();
    }

    void CacheEncoderParameterSets(const std::vector<Nal>& nals) {
        for (const Nal& nal : nals) {
            if (nal.size < 2 || nal.size > kMaxParameterSet) continue;
            if (nal.Type() == 7) encoderSps.assign(nal.data, nal.data + nal.size);
            if (nal.Type() == 8) encoderPps.assign(nal.data, nal.data + nal.size);
        }
    }

    void RefreshEncoderHeaders() {
        ComPtr<IMFMediaType> type;
        UINT32 size = 0;
        if (FAILED(transform->GetOutputCurrentType(outputId, type.GetAddressOf())) || !type ||
            FAILED(type->GetBlobSize(MF_MT_MPEG_SEQUENCE_HEADER, &size)) ||
            size == 0 || size > 128u * 1024u) {
            return;
        }

        Bytes header(size);
        UINT32 actual;
        if (FAILED(type->GetBlob(MF_MT_MPEG_SEQUENCE_HEADER, header.data(), size, &actual)) ||
            actual != size) {
            return;
        }

        std::vector<Nal> nals;
        if (SplitAnnexB(header.data(), header.size(), nals)) {
            avcLengthBytes = 0;
            CacheEncoderParameterSets(nals);
            return;
        }

        // 兼容以 AVCDecoderConfigurationRecord 提供的参数集。
        if (header.size() < 7 || header[0] != 1 ||
            (header[4] & 0xfcu) != 0xfcu || (header[5] & 0xe0u) != 0xe0u) {
            return;
        }
        const unsigned lengthBytes = (header[4] & 3u) + 1;
        if (lengthBytes == 3) return;
        nals.clear();
        std::size_t pos = 6;
        for (unsigned expected : {7u, 8u}) {
            if (pos == header.size()) return;
            const unsigned count = expected == 7 ? header[5] & 31u : header[pos++];
            for (unsigned i = 0; i < count; ++i) {
                if (header.size() - pos < 2) return;
                const std::size_t length = (static_cast<unsigned>(header[pos]) << 8) | header[pos + 1];
                pos += 2;
                if (length < 2 || length > kMaxParameterSet || length > header.size() - pos ||
                    (header[pos] & 31u) != expected) {
                    return;
                }
                nals.push_back({header.data() + pos, length});
                pos += length;
            }
        }
        avcLengthBytes = lengthBytes;
        CacheEncoderParameterSets(nals);
    }

    bool QueueEncoderSample(IMFSample* sample) {
        ComPtr<IMFMediaBuffer> buffer;
        if (FAILED(sample->ConvertToContiguousBuffer(buffer.GetAddressOf())) || !buffer) return false;
        BufferLock lock(buffer.Get());
        if (!lock.Open() || lock.length > lock.capacity || lock.length > kMaxPacket) return false;
        if (lock.length == 0) return true;

        std::vector<Nal> nals;
        bool parsed = false;
        if (avcLengthBytes != 0) {
            parsed = SplitLengthPrefixed(lock.data, lock.length, avcLengthBytes, nals);
        }
        if (!parsed) parsed = SplitAnnexB(lock.data, lock.length, nals);
        if (!parsed && avcLengthBytes == 0) {
            parsed = SplitLengthPrefixed(lock.data, lock.length, 4, nals);
        }
        if (!parsed) return false;

        bool idr = false;
        bool hasVcl = false;
        for (const Nal& nal : nals) {
            if ((nal.data[0] & 0x80u) != 0 || nal.Type() == 0 || nal.Type() > 23) return false;
            idr = idr || nal.Type() == 5;
            hasVcl = hasVcl || (nal.Type() >= 1 && nal.Type() <= 5);
        }

        if (idr && (encoderSps.empty() || encoderPps.empty())) RefreshEncoderHeaders();
        CacheEncoderParameterSets(nals);
        if (!hasVcl || (needIdr && !idr)) return true;
        if (idr && (encoderSps.empty() || encoderPps.empty())) {
            needIdr = true;
            return true;
        }
        if (ready.size() >= kMaxQueuedFrames) return false;

        Output output;
        std::size_t first = 0;
        if (idr) {
            while (first < nals.size() && nals[first].Type() == 9) {
                if (!AppendNal(output.bytes, nals[first].data, nals[first].size)) return false;
                ++first;
            }
            if (!AppendNal(output.bytes, encoderSps.data(), encoderSps.size()) ||
                !AppendNal(output.bytes, encoderPps.data(), encoderPps.size())) {
                return false;
            }
        }
        for (std::size_t i = first; i < nals.size(); ++i) {
            const Nal& nal = nals[i];
            if (idr && (nal.Type() == 7 || nal.Type() == 8)) continue;
            if (!AppendNal(output.bytes, nal.data, nal.size)) return false;
        }
        ready.push_back(std::move(output));
        if (idr) needIdr = false;
        return true;
    }

    bool QueueNv12(const BYTE* data, std::size_t size, int stride) {
        const std::size_t pitch = static_cast<std::size_t>(stride);
        const std::size_t rows = static_cast<std::size_t>(layout.frameHeight);
        const std::size_t required = pitch * (rows + rows / 2 - 1) + layout.frameWidth;
        if (size < required || ready.size() >= kMaxQueuedFrames) return false;

        Output output;
        output.width = picture.width;
        output.height = picture.height;
        output.bytes.resize(static_cast<std::size_t>(picture.width) * picture.height * 4);
        Nv12ToBgra(data, stride, layout, picture, output.bytes.data());
        ready.push_back(std::move(output));
        return true;
    }

    bool QueueDecoderSample(IMFSample* sample) {
        ComPtr<IMFMediaBuffer> buffer;
        if (FAILED(sample->ConvertToContiguousBuffer(buffer.GetAddressOf())) || !buffer) return false;
        ComPtr<IMF2DBuffer> image;
        if (SUCCEEDED(buffer.As(&image))) {
            DWORD size = 0;
            const HRESULT lengthResult = image->GetContiguousLength(&size);
            if (SUCCEEDED(lengthResult)) {
                if (size == 0 || size > kMaxRawBuffer) return false;
                const std::size_t rows = static_cast<std::size_t>(layout.frameHeight) * 3 / 2;
                const std::size_t packed = rows * layout.frameWidth;
                const std::size_t padded = rows * layout.stride;
                if (size != packed && size != padded) return false;
                scratch.resize(size);
                const HRESULT copyResult = image->ContiguousCopyTo(scratch.data(), size);
                if (SUCCEEDED(copyResult)) {
                    return QueueNv12(scratch.data(), size,
                                     size == packed ? layout.frameWidth : layout.stride);
                }
                if (copyResult != E_NOTIMPL) return false;
            } else if (lengthResult != E_NOTIMPL) {
                return false;
            }
        }

        BufferLock lock(buffer.Get());
        if (!lock.Open() || lock.length > lock.capacity || lock.length > kMaxRawBuffer) return false;
        const std::size_t rows = static_cast<std::size_t>(layout.frameHeight);
        const std::size_t packed = (rows + rows / 2) * layout.frameWidth;
        const auto stride = lock.length == packed ? layout.frameWidth : layout.stride;
        return QueueNv12(lock.data, lock.length, stride);
    }

    bool ChangeOutput() {
        if (!encode) return SetDecoderOutput(false);
        if (!SetEncoderOutput()) return false;
        encoderSps.clear();
        encoderPps.clear();
        avcLengthBytes = 0;
        needIdr = true;
        RefreshEncoderHeaders();
        return true;
    }

    bool Pump() {
        DWORD bufferOverride = 0;
        unsigned changes = 0;
        for (unsigned attempt = 0; attempt < 40; ++attempt) {
            MFT_OUTPUT_STREAM_INFO info{};
            if (FAILED(transform->GetOutputStreamInfo(outputId, &info))) return false;

            ComPtr<IMFSample> supplied;
            ComPtr<IMFMediaBuffer> buffer;
            const bool mftAllocates = (info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES) != 0 ||
                ((info.dwFlags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES) != 0 && bufferOverride == 0);
            const DWORD limit = encode ? kMaxPacket : kMaxRawBuffer;
            DWORD size = std::max(info.cbSize, bufferOverride);
            if (!mftAllocates) {
                const DWORD minimum = encode
                    ? static_cast<DWORD>(static_cast<std::size_t>(width) * height * 3 / 2 + 65536)
                    : static_cast<DWORD>(static_cast<std::size_t>(layout.stride) * layout.frameHeight * 3 / 2);
                size = std::max(size, minimum);
                if (size > limit || !MakeSample(size, info.cbAlignment, supplied, buffer)) {
                    return false;
                }
            }

            MFT_OUTPUT_DATA_BUFFER result{};
            result.dwStreamID = outputId;
            result.pSample = supplied.Get();
            DWORD status = 0;
            const HRESULT hr = transform->ProcessOutput(0, 1, &result, &status);
            ComPtr<IMFCollection> events;
            events.Attach(result.pEvents);
            ComPtr<IMFSample> returned;
            if (result.pSample != supplied.Get()) returned.Attach(result.pSample);

            if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return true;
            if (hr == MF_E_BUFFERTOOSMALL && size < limit) {
                bufferOverride = std::min<DWORD>(limit, std::max<DWORD>(65536, size * 2));
                continue;
            }
            if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
                if (++changes > 8 || !ChangeOutput()) return false;
                bufferOverride = 0;
                continue;
            }
            if (FAILED(hr)) return false;

            constexpr DWORD kStatusMask = MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE;
            const DWORD streamStatus = result.dwStatus & kStatusMask;
            if (streamStatus == MFT_OUTPUT_DATA_BUFFER_FORMAT_CHANGE) {
                if (++changes > 8 || !ChangeOutput()) return false;
                bufferOverride = 0;
                continue;
            }
            if (streamStatus == MFT_OUTPUT_DATA_BUFFER_STREAM_END) return true;
            if (streamStatus == MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE || result.pSample == nullptr) {
                if ((result.dwStatus & MFT_OUTPUT_DATA_BUFFER_INCOMPLETE) != 0) continue;
                return true;
            }
            if (encode ? !QueueEncoderSample(result.pSample) : !QueueDecoderSample(result.pSample)) return false;
            if (hardwareEvents) {
                lastHardwareOutput = GetTickCount64();
                if (!(result.dwStatus & MFT_OUTPUT_DATA_BUFFER_INCOMPLETE)) return true;
            }
        }
        return false;
    }

    bool PollHardware() {
        for (unsigned n = 0; n < 32; ++n) {
            ComPtr<IMFMediaEvent> event;
            HRESULT hr = hardwareEvents->GetEvent(MF_EVENT_FLAG_NO_WAIT, event.GetAddressOf());
            if (hr == MF_E_NO_EVENTS_AVAILABLE) return true;
            HRESULT status = E_FAIL;
            MediaEventType type = MEUnknown;
            if (FAILED(hr) || FAILED(event->GetStatus(&status)) || FAILED(status) ||
                FAILED(event->GetType(&type)) || type == MEError) return false;
            if (type == METransformNeedInput) ++needInputEvents;
            if (type == METransformHaveOutput && !Pump()) return false;
        }
        return true;
    }

    bool Submit(IMFSample* sample, bool cleanPoint) {
        const LONGLONG sum = tickRemainder + kTicksPerSecond;
        const LONGLONG duration = sum / fps;
        if (nextTime > (std::numeric_limits<LONGLONG>::max)() - duration ||
            FAILED(sample->SetSampleTime(nextTime)) || FAILED(sample->SetSampleDuration(duration))) {
            return false;
        }
        if (firstInput && FAILED(sample->SetUINT32(MFSampleExtension_Discontinuity, TRUE))) return false;
        if (cleanPoint && FAILED(sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE))) return false;

        if (hardwareEvents) {
            const auto deadline = GetTickCount64() + 60;
            while (needInputEvents == 0) {
                if (!PollHardware() || GetTickCount64() >= deadline) return false;
                if (!needInputEvents) Sleep(1);
            }
            --needInputEvents;
        }
        for (unsigned attempt = 0; attempt < 2; ++attempt) {
            const HRESULT hr = transform->ProcessInput(inputId, sample, 0);
            if (hr == MF_E_NOTACCEPTING) {
                if (hardwareEvents || !Pump()) return false;
                continue;
            }
            if (FAILED(hr)) return false;
            nextTime += duration;
            tickRemainder = sum % fps;
            firstInput = false;
            if (encode) ++framesSinceRequest;
            return hardwareEvents ? PollHardware() : Pump();
        }
        return false;
    }

    bool EncodeInput(const BYTE* bgra, int stride) {
        const bool wasHardware = activation != nullptr;
        if ((!hardwareEvents || GetTickCount64() - lastHardwareOutput < 1500) && EncodeFrame(bgra, stride)) return true;
        if (!wasHardware) return false;
        // 驱动不能连续接收内存输入或超时，从新 IDR 恢复到系统软件编码器。
        ResetPipeline();
        skipHardware = true;
        return EncodeFrame(bgra, stride);
    }

    bool EncodeFrame(const BYTE* bgra, int stride) {
        if (!transform && !OpenEncoder()) {
            ResetPipeline();
            return false;
        }

        const DWORD size = static_cast<DWORD>(static_cast<std::size_t>(width) * height * 3 / 2);
        MFT_INPUT_STREAM_INFO info{};
        const HRESULT hr = transform->GetInputStreamInfo(inputId, &info);
        if (FAILED(hr) && hr != E_NOTIMPL) {
            ResetPipeline();
            return false;
        }
        ComPtr<IMFSample> sample;
        ComPtr<IMFMediaBuffer> buffer;
        const DWORD capacity = std::max(size, info.cbSize);
        if (capacity > kMaxRawBuffer || !MakeSample(capacity, info.cbAlignment, sample, buffer)) {
            return false;
        }
        {
            BufferLock lock(buffer.Get());
            if (!lock.Open() || lock.capacity < size) return false;
            BgraToNv12(bgra, static_cast<std::ptrdiff_t>(stride), lock.data, width, height);
        }
        if (FAILED(buffer->SetCurrentLength(size))) return false;

        if (needIdr || framesSinceRequest >= fps * 2) {
            SetCodecUInt(codecApi.Get(), CODECAPI_AVEncVideoForceKeyFrame, 1);
            framesSinceRequest = 0;
        }
        if (Submit(sample.Get(), false)) return true;
        ResetPipeline();
        return false;
    }

    bool DecodeInput(const BYTE* data, std::size_t size) {
        std::vector<Nal> nals;
        if (!SplitAnnexB(data, size, nals)) return false;

        std::array<Sps, 32> newSps;
        std::array<Pps, 256> newPps;
        bool hasPicture = false;
        bool idr = false;
        U32 activePps = 0;
        U32 maxFirstMb = 0;

        for (const Nal& nal : nals) {
            if (!ValidEbsp(nal)) return false;
            const unsigned type = nal.Type();
            if (type == 7 || type == 8) {
                if (hasPicture || nal.size > kMaxParameterSet || (nal.data[0] & 0x60u) == 0) {
                    return false;
                }
                U32 id;
                if (type == 7) {
                    Picture format{};
                    if (!ReadSps(nal, width, height, id, format)) return false;
                    newSps[id].picture = format;
                    newSps[id].bytes.assign(nal.data, nal.data + nal.size);
                } else {
                    U32 spsId;
                    if (!ReadPps(nal, id, spsId)) return false;
                    newPps[id].spsId = spsId;
                    newPps[id].bytes.assign(nal.data, nal.data + nal.size);
                }
            } else if (type == 1 || type == 5) {
                U32 ppsId;
                U32 firstMb;
                if (!ReadSlicePrefix(nal, ppsId, firstMb) ||
                    (type == 5 && (nal.data[0] & 0x60u) == 0) ||
                    (hasPicture && (ppsId != activePps || idr != (type == 5)))) {
                    return false;
                }
                hasPicture = true;
                activePps = ppsId;
                idr = type == 5;
                maxFirstMb = std::max(maxFirstMb, firstMb);
            } else if (type != 6 && type != 9 && type != 10 && type != 11 && type != 12) {
                return false;
            }
        }

        if (hasPicture) {
            const Pps& pps = newPps[activePps].bytes.empty() ? ppsCache[activePps] : newPps[activePps];
            if (pps.bytes.empty()) return false;
            const Sps& sps = newSps[pps.spsId].bytes.empty() ? spsCache[pps.spsId] : newSps[pps.spsId];
            if (sps.bytes.empty() ||
                maxFirstMb >= static_cast<U32>((sps.picture.codedWidth / 16) *
                                               (sps.picture.codedHeight / 16))) {
                return false;
            }
        }

        for (std::size_t i = 0; i < newSps.size(); ++i) {
            if (!newSps[i].bytes.empty()) spsCache[i] = std::move(newSps[i]);
        }
        for (std::size_t i = 0; i < newPps.size(); ++i) {
            if (!newPps[i].bytes.empty()) ppsCache[i] = std::move(newPps[i]);
        }
        if (!hasPicture) return true;

        const Pps& pps = ppsCache[activePps];
        const Sps& sps = spsCache[pps.spsId];
        const bool reconfigure = !transform || !SamePicture(picture, sps.picture);
        if (reconfigure && !idr) return true;
        if (reconfigure) {
            CloseTransform();
            if (!OpenDecoder(sps.picture)) {
                ResetPipeline();
                return false;
            }
        }

        const bool sendParameters = idr || sps.bytes != sentSps || pps.bytes != sentPps;
        Bytes packet;
        packet.reserve(size);
        std::size_t first = 0;
        while (first < nals.size() && nals[first].Type() == 9) {
            if (!AppendNal(packet, nals[first].data, nals[first].size)) return false;
            ++first;
        }
        if (sendParameters &&
            (!AppendNal(packet, sps.bytes.data(), sps.bytes.size()) ||
             !AppendNal(packet, pps.bytes.data(), pps.bytes.size()))) {
            return false;
        }
        for (std::size_t i = first; i < nals.size(); ++i) {
            const Nal& nal = nals[i];
            if (nal.Type() == 7 || nal.Type() == 8) continue;
            if (!AppendNal(packet, nal.data, nal.size)) return false;
        }

        ComPtr<IMFSample> sample;
        if (!CopyToSample(packet, sample) || !Submit(sample.Get(), idr)) {
            ResetPipeline();
            return false;
        }
        if (sendParameters) {
            sentSps = sps.bytes;
            sentPps = pps.bytes;
        }
        return true;
    }

    int TakeOutput(BYTE* dest, int capacity, int* outWidth, int* outHeight) noexcept {
        if (ready.empty()) return 0;
        Output output = std::move(ready.front());
        ready.pop_front();
        if (outWidth != nullptr) {
            *outWidth = output.width;
            *outHeight = output.height;
        }
        if (output.bytes.size() > static_cast<std::size_t>(capacity)) {
            if (encode) ResetPipeline();
            return -1;
        }
        std::memcpy(dest, output.bytes.data(), output.bytes.size());
        return static_cast<int>(output.bytes.size());
    }

    const bool encode;
    const int width;
    const int height;
    const int fps;
    const int bitrate;

    bool comStarted = false;
    bool mfStarted = false;
    bool notifiedBegin = false;
    bool firstInput = true;
    bool needIdr = true;
    int framesSinceRequest = 0;
    DWORD inputId = 0;
    DWORD outputId = 0;
    LONGLONG nextTime = 0;
    LONGLONG tickRemainder = 0;
    unsigned avcLengthBytes = 0;
    ComPtr<IMFTransform> transform;
    ComPtr<IMFActivate> activation;
    ComPtr<IMFMediaEventGenerator> hardwareEvents;
    unsigned needInputEvents = 0;
    ULONGLONG lastHardwareOutput = 0;
    bool skipHardware = false;
    ComPtr<ICodecAPI> codecApi;
    Bytes encoderSps;
    Bytes encoderPps;
    Bytes sentSps;
    Bytes sentPps;
    Bytes scratch;
    std::array<Sps, 32> spsCache;
    std::array<Pps, 256> ppsCache;
    Picture picture;
    Layout layout;
    std::deque<Output> ready;
};

} // namespace

extern "C" long __cdecl ro_codec_startup_error() noexcept {
    return lastStartupError;
}

// 同一专用 MTA 线程负责创建、调用和销毁。编码尺寸固定，改变尺寸时重建句柄。
// 解码尺寸参数是可见画面上限。解码 fps 为零时按 30 生成时间戳。
extern "C"
void* __cdecl ro_codec_new(int encode, int width, int height, int fps, int bitrate) noexcept {
    const bool isEncoder = encode != 0;
    if (!isEncoder && fps == 0) fps = 30;
    if (width < 2 || height < 2 || fps < 1 || fps > 240) return nullptr;
    if (isEncoder) {
        if (width > 4096 || height > 2160 || (width & 1) != 0 || (height & 1) != 0 ||
            bitrate < 1 || bitrate > 100'000'000) {
            return nullptr;
        }
    } else if (width > kMaxDecodeWidth || height > kMaxDecodeHeight) {
        return nullptr;
    }

    try {
        auto codec = std::make_unique<Codec>(isEncoder, width, height, fps, bitrate);
        if (!codec->Initialize()) return nullptr;
        return codec.release();
    } catch (...) {
        return nullptr;
    }
}

extern "C"
void __cdecl ro_codec_free(void* h) noexcept {
    delete static_cast<Codec*>(h);
}

extern "C" int __cdecl ro_codec_hardware(void* h) noexcept {
    auto* codec = static_cast<Codec*>(h);
    return codec && codec->activation != nullptr;
}

// BGRA 使用字节 stride。空输入和零 stride 用于取已排队的输出。
// 输出容量不足时丢弃该帧并返回 -1，下次编码重新建立 IDR 起点。
extern "C"
int __cdecl ro_encode(void* h, const unsigned char* bgra, int stride,
                      unsigned char* dest, int capacity) noexcept {
    auto* codec = static_cast<Codec*>(h);
    if (codec == nullptr || !codec->encode || dest == nullptr || capacity <= 0) return -1;

    try {
        if (bgra != nullptr) {
            const std::int64_t pitch = stride < 0 ? -static_cast<std::int64_t>(stride) : stride;
            const std::int64_t rowBytes = static_cast<std::int64_t>(codec->width) * 4;
            if (pitch < rowBytes ||
                pitch * (codec->height - 1) + rowBytes >
                    (std::numeric_limits<std::ptrdiff_t>::max)()) {
                return -1;
            }
            if (!codec->EncodeInput(bgra, stride)) return -1;
        } else if (stride != 0) {
            return -1;
        }
        return codec->TakeOutput(dest, capacity, nullptr, nullptr);
    } catch (...) {
        codec->ResetPipeline();
        return -1;
    }
}

// 每次提交一个 Annex B 访问单元，也可单独提交参数集。
// 画面从顶行开始紧密排列为 BGRA。空输入和零长度用于取已排队的输出。
// 容量不足时返回 -1 并报告被丢弃画面的尺寸。
extern "C"
int __cdecl ro_decode(void* h, const unsigned char* annex_b, int size,
                      unsigned char* dest, int capacity, int* width, int* height) noexcept {
    if (width == nullptr || height == nullptr || width == height) return -1;
    *width = 0;
    *height = 0;
    auto* codec = static_cast<Codec*>(h);
    if (codec == nullptr || codec->encode || dest == nullptr || capacity <= 0 ||
        size < 0 || static_cast<unsigned>(size) > kMaxPacket ||
        (annex_b == nullptr) != (size == 0)) {
        return -1;
    }

    try {
        if (size != 0 && !codec->DecodeInput(annex_b, static_cast<std::size_t>(size))) return -1;
        return codec->TakeOutput(dest, capacity, width, height);
    } catch (...) {
        codec->ResetPipeline();
        return -1;
    }
}
