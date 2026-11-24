#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <new>
#include <vector>

namespace {
using Bytes = std::vector<uint8_t>;

struct Decoder {
    CMVideoFormatDescriptionRef format = nullptr;
    VTDecompressionSessionRef session = nullptr;
    Bytes sps, pps;

    void reset() {
        if (session) {
            VTDecompressionSessionInvalidate(session);
            CFRelease(session);
            session = nullptr;
        }
        if (format) { CFRelease(format); format = nullptr; }
    }
    ~Decoder() { reset(); }
};
struct Output {
    CVImageBufferRef image = nullptr;
    OSStatus status = noErr;
    ~Output() { if (image) CFRelease(image); }
};

void receive(void*, void* context, OSStatus status, VTDecodeInfoFlags,
             CVImageBufferRef image, CMTime, CMTime) {
    auto& output = *static_cast<Output*>(context);
    output.status = status;
    if (image && status == noErr) {
        if (output.image) CFRelease(output.image);
        output.image = static_cast<CVImageBufferRef>(CFRetain(image));
    }
}

bool prepare(Decoder& d) {
    const uint8_t* sets[]{d.sps.data(), d.pps.data()};
    const size_t lengths[]{d.sps.size(), d.pps.size()};
    if (CMVideoFormatDescriptionCreateFromH264ParameterSets(nullptr, 2, sets, lengths, 4,
            &d.format) != noErr) return false;
    auto size = CMVideoFormatDescriptionGetDimensions(d.format);
    if (size.width < 1 || size.width > 1920 || size.height < 1 || size.height > 1080) return false;
    NSDictionary* attributes = @{(__bridge NSString*)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_32BGRA)};
    VTDecompressionOutputCallbackRecord record{receive, nullptr};
    return VTDecompressionSessionCreate(nullptr, d.format, nullptr,
            (__bridge CFDictionaryRef)attributes, &record, &d.session) == noErr;
}

int searchPrefix(const uint8_t* p, int size, int offset, int& next) {
    for (int i = offset; i + 2 < size; ++i) {
        if (p[i] || p[i+1]) continue;
        if (p[i+2] == 1) { next = i+3; return i; }
        if (i+3 < size && p[i+2] == 0 && p[i+3] == 1) { next = i+4; return i; }
    }
    next = size;
    return size;
}

int decode(Decoder& d, const uint8_t* packet, int length, uint8_t* dst, int capacity, int& w, int& h) {
    Bytes access;
    int start = 0;
    if (searchPrefix(packet,length,0,start) == length) return -1;
    bool picture = false, key = false, changed = false;
    while (start < length) {
        int next = 0;
        int end = searchPrefix(packet,length,start,next);
        int n = end - start;
        const uint8_t* p = packet + start;
        start = next;
        if (!n) continue;
        int type = p[0] & 31;
        if (type == 7 || type == 8) {
            if (n > 16384) return -1;
            auto& previous = type == 7 ? d.sps : d.pps;
            if (previous.size() != static_cast<size_t>(n) || !std::equal(previous.begin(),previous.end(),p)) {
                previous.assign(p,p+n);
                changed = true;
            }
        } else {
            picture |= type == 1 || type == 5;
            key |= type == 5;
            uint32_t be = CFSwapInt32HostToBig(static_cast<uint32_t>(n));
            auto prefix = reinterpret_cast<const uint8_t*>(&be);
            access.insert(access.end(),prefix,prefix+4);
            access.insert(access.end(),p,p+n);
        }
    }
    if (changed) d.reset();
    if (!picture) return 0;
    if (!d.session) {
        if (!key || d.sps.empty() || d.pps.empty()) return 0;
        if (!prepare(d)) { d.reset(); return -1; }
    }
    CMBlockBufferRef block = nullptr;
    CMSampleBufferRef sample = nullptr;
    size_t bytes = access.size();
    OSStatus error = CMBlockBufferCreateWithMemoryBlock(nullptr,access.data(),bytes,kCFAllocatorNull,
        nullptr,0,bytes,0,&block);
    if (error == noErr) error = CMSampleBufferCreateReady(nullptr,block,d.format,1,0,nullptr,1,&bytes,&sample);
    Output output;
    if (error == noErr) {
        error = VTDecompressionSessionDecodeFrame(d.session,sample,0,&output,nullptr);
        auto waited=VTDecompressionSessionWaitForAsynchronousFrames(d.session);
        if (error == noErr) error=waited;
    }
    if (sample) CFRelease(sample);
    if (block) CFRelease(block);
    if (error != noErr || output.status != noErr) return -1;
    if (!output.image) return 0;
    const size_t width = CVPixelBufferGetWidth(output.image);
    const size_t height = CVPixelBufferGetHeight(output.image);
    if (!width || width > 1920 || !height || height > 1080 || width*height*4 > static_cast<size_t>(capacity) ||
        CVPixelBufferGetPixelFormatType(output.image) != kCVPixelFormatType_32BGRA) return -1;
    if (CVPixelBufferLockBaseAddress(output.image,kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) return -1;
    auto source = static_cast<const uint8_t*>(CVPixelBufferGetBaseAddress(output.image));
    auto stride = CVPixelBufferGetBytesPerRow(output.image);
    bool valid = source && stride >= width*4;
    if (valid) for (size_t y=0; y<height; ++y) std::memcpy(dst+y*width*4,source+y*stride,width*4);
    CVPixelBufferUnlockBaseAddress(output.image,kCVPixelBufferLock_ReadOnly);
    if (!valid) return -1;
    w=static_cast<int>(width); h=static_cast<int>(height);
    return w*h*4;
}
}

extern "C" void* ro_codec_new(int encode,int,int,int,int) noexcept {
    return encode ? nullptr : new(std::nothrow) Decoder();
}
extern "C" void ro_codec_free(void* p) noexcept { delete static_cast<Decoder*>(p); }
extern "C" int ro_decode(void* p,const uint8_t* packet,int size,uint8_t* dst,int capacity,int* width,int* height) noexcept {
    if (!p || !packet || size<1 || size>2*1024*1024 || !dst || capacity<1 || !width || !height) return -1;
    @autoreleasepool {
        try { return decode(*static_cast<Decoder*>(p),packet,size,dst,capacity,*width,*height); }
        catch (...) { return -1; }
    }
}
