#pragma once
#include "clipboard_internal.hpp"
#include <wincodec.h>
#include <wrl/client.h>

namespace roclip {
using Microsoft::WRL::ComPtr;
// PNG 按块传输。分配像素缓冲前先校验尺寸，并只接受单帧 PNG。
constexpr size_t MaxPng = 32 * 1024 * 1024;
inline bool validImageSize(UINT w, UINT h) {
    return w && h && w <= 8192 && h <= 8192 && uint64_t(w) * h <= 16 * 1024 * 1024;
}
inline ComPtr<IWICImagingFactory> imagingFactory() {
    ComPtr<IWICImagingFactory> factory;
    CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory));
    return factory;
}
inline UINT pngFormat() { return RegisterClipboardFormatW(L"PNG"); }

template<class ReleaseClipboard>
Bytes copyClipboardPng(ReleaseClipboard releaseClipboard) {
    if (auto handle = GetClipboardData(pngFormat())) {
        auto size = GlobalSize(handle);
        if (size && size <= MaxPng) {
            Bytes png(size);
            if (auto data = GlobalLock(handle)) {
                memcpy(png.data(), data, size);
                GlobalUnlock(handle);
                return png;
            }
        }
    }
    // Windows 可将 CF_DIB / CF_DIBV5 合成为 CF_BITMAP；发送独立副本。
    auto bitmap = static_cast<HBITMAP>(GetClipboardData(CF_BITMAP));
    BITMAP info{};
    if (!bitmap || !GetObjectW(bitmap, sizeof(info), &info) ||
        !validImageSize(info.bmWidth, info.bmHeight)) return {};
    auto factory = imagingFactory();
    ComPtr<IWICBitmap> source;
    ComPtr<IWICBitmapEncoder> encoder;
    ComPtr<IWICBitmapFrameEncode> frame;
    ComPtr<IStream> stream;
    if (!factory || FAILED(factory->CreateBitmapFromHBITMAP(bitmap, nullptr, WICBitmapIgnoreAlpha, &source))) return {};
    releaseClipboard();
    if (FAILED(CreateStreamOnHGlobal(nullptr, TRUE, &stream)) ||
        FAILED(factory->CreateEncoder(GUID_ContainerFormatPng, nullptr, &encoder)) ||
        FAILED(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache)) ||
        FAILED(encoder->CreateNewFrame(&frame, nullptr)) || FAILED(frame->Initialize(nullptr)) ||
        FAILED(frame->WriteSource(source.Get(), nullptr)) || FAILED(frame->Commit()) || FAILED(encoder->Commit())) return {};
    STATSTG stat{};
    if (FAILED(stream->Stat(&stat, STATFLAG_NONAME)) || !stat.cbSize.QuadPart || stat.cbSize.QuadPart > MaxPng) return {};
    Bytes png(static_cast<size_t>(stat.cbSize.QuadPart));
    ULONG read = 0;
    if (FAILED(stream->Seek({}, STREAM_SEEK_SET, nullptr)) ||
        FAILED(stream->Read(png.data(), static_cast<ULONG>(png.size()), &read)) || read != png.size()) return {};
    return png;
}

// 只把验证并解码完成的像素交给 Windows，行顺序使用自顶向下的 DIB。
inline Bytes decodeClipboardPng(Bytes& png) {
    if (png.empty() || png.size() > MaxPng) return {};
    auto factory = imagingFactory();
    ComPtr<IWICStream> stream;
    ComPtr<IWICBitmapDecoder> decoder;
    ComPtr<IWICBitmapFrameDecode> frame;
    ComPtr<IWICFormatConverter> converter;
    UINT w = 0, h = 0, count = 0;
    if (!factory || FAILED(factory->CreateStream(&stream)) ||
        FAILED(stream->InitializeFromMemory(png.data(), static_cast<DWORD>(png.size()))) ||
        FAILED(factory->CreateDecoder(GUID_ContainerFormatPng, nullptr, &decoder)) ||
        FAILED(decoder->Initialize(stream.Get(), WICDecodeMetadataCacheOnDemand)) ||
        FAILED(decoder->GetFrameCount(&count)) || count != 1 || FAILED(decoder->GetFrame(0, &frame)) ||
        FAILED(frame->GetSize(&w, &h)) || !validImageSize(w, h) || FAILED(factory->CreateFormatConverter(&converter)) ||
        FAILED(converter->Initialize(frame.Get(), GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone,
                                     nullptr, 0, WICBitmapPaletteTypeCustom))) return {};
    BITMAPINFOHEADER header{};
    header.biSize = sizeof(header);
    header.biWidth = static_cast<LONG>(w);
    header.biHeight = -static_cast<LONG>(h);
    header.biPlanes = 1;
    header.biBitCount = 32;
    header.biSizeImage = w * h * 4;
    Bytes dib(sizeof(header) + header.biSizeImage);
    memcpy(dib.data(), &header, sizeof(header));
    if (FAILED(converter->CopyPixels(nullptr, w * 4, header.biSizeImage, dib.data() + sizeof(header)))) return {};
    return dib;
}
}
