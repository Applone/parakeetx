#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreAudio/CoreAudioTypes.h>
#import <CoreGraphics/CoreGraphics.h>
#include <math.h>
#include <stdio.h>
#include <string.h>
#include "screencapturekit.h"

static const double PXSampleRate = 16000.0;

static double hostTime(void) {
    return CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock()));
}

@interface PXAudioPacket : NSObject
@property(nonatomic, strong) NSData *samples;
@property(nonatomic) int64_t start;
@end
@implementation PXAudioPacket
@end

@interface PXAudioCapture : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, strong) NSLock *lock;
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) NSMutableArray<PXAudioPacket *> *packets;
@property(nonatomic, copy) NSString *failure;
@property(nonatomic, strong) dispatch_queue_t audioQueue;
@property(nonatomic, strong) dispatch_queue_t lifecycleQueue;
@property(nonatomic) double origin;
@property(nonatomic) double pausedSeconds;
@property(nonatomic) double pauseStart;
@property(nonatomic) double lastResume;
@property(nonatomic) BOOL paused;
@property(nonatomic) BOOL closed;
@property(nonatomic) BOOL started;
- (void)start;
- (void)close;
- (void)fail:(NSString *)message;
@end

@implementation PXAudioCapture
- (instancetype)init {
    self = [super init];
    if (self) {
        _lock = [NSLock new];
        _packets = [NSMutableArray new];
        _audioQueue = dispatch_queue_create("app.parakeetx.system-audio", DISPATCH_QUEUE_SERIAL);
        _lifecycleQueue = dispatch_queue_create("app.parakeetx.capture-lifecycle", DISPATCH_QUEUE_SERIAL);
        _origin = hostTime();
        _lastResume = _origin;
    }
    return self;
}

- (void)fail:(NSString *)message {
    [self.lock lock];
    if (!self.closed && !self.failure) self.failure = message;
    [self.lock unlock];
}

- (void)start {
    // The Rust caller runs on a recording thread. ScreenCaptureKit completion handlers
    // and audio delivery use background queues; no main-thread semaphore is involved.
    if (@available(macOS 13.0, *)) {
        [SCShareableContent getShareableContentExcludingDesktopWindows:NO onScreenWindowsOnly:YES
            completionHandler:^(SCShareableContent *content, NSError *error) {
            dispatch_async(self.lifecycleQueue, ^{
                @autoreleasepool {
                    [self.lock lock];
                    BOOL closed = self.closed;
                    [self.lock unlock];
                    if (closed) return;
                    if (error) {
                        [self fail:[NSString stringWithFormat:@"Cannot access system audio: %@. Allow parakeetx in System Settings → Privacy & Security → Screen & System Audio Recording, then restart the app.", error.localizedDescription]];
                        return;
                    }
                    SCDisplay *display = content.displays.firstObject;
                    if (!display) {
                        [self fail:@"ScreenCaptureKit found no active display to capture system audio."];
                        return;
                    }
                    SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
                    SCStreamConfiguration *config = [SCStreamConfiguration new];
                    config.capturesAudio = YES;
                    config.excludesCurrentProcessAudio = YES;
                    config.sampleRate = (NSInteger)PXSampleRate;
                    config.channelCount = 1;
                    // Only an audio output is registered. Minimize the unused video stream.
                    config.width = 2;
                    config.height = 2;
                    config.minimumFrameInterval = CMTimeMake(1, 1);
                    config.showsCursor = NO;
                    SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:self];
                    NSError *outputError = nil;
                    if (![stream addStreamOutput:self type:SCStreamOutputTypeAudio sampleHandlerQueue:self.audioQueue error:&outputError]) {
                        [self fail:[NSString stringWithFormat:@"Cannot attach ScreenCaptureKit audio output: %@", outputError.localizedDescription]];
                        return;
                    }
                    [self.lock lock];
                    self.stream = stream;
                    [self.lock unlock];
                    [stream startCaptureWithCompletionHandler:^(NSError *startError) {
                        if (startError) {
                            [self fail:[NSString stringWithFormat:@"Cannot start ScreenCaptureKit audio capture: %@", startError.localizedDescription]];
                        } else {
                            [self.lock lock];
                            self.started = YES;
                            BOOL shouldStop = self.closed;
                            [self.lock unlock];
                            if (shouldStop) [stream stopCaptureWithCompletionHandler:^(NSError *stopError) { (void)stopError; }];
                        }
                    }];
                }
            });
        }];
    } else {
        [self fail:@"ScreenCaptureKit system audio requires macOS 13 or newer."];
    }
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    (void)stream;
    [self fail:[NSString stringWithFormat:@"ScreenCaptureKit audio capture stopped: %@", error.localizedDescription]];
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type {
    (void)stream;
    if (type != SCStreamOutputTypeAudio || !CMSampleBufferDataIsReady(sample)) return;
    @autoreleasepool {
        double timestamp = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sample));
        [self.lock lock];
        BOOL discard = self.closed || self.failure != nil || (self.paused && timestamp >= self.pauseStart);
        [self.lock unlock];
        if (discard) return;

        const AudioStreamBasicDescription *format = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(sample));
        if (!format || format->mFormatID != kAudioFormatLinearPCM ||
            !(format->mFormatFlags & kAudioFormatFlagIsFloat) ||
            !(format->mFormatFlags & kAudioFormatFlagIsPacked) ||
            (format->mFormatFlags & kAudioFormatFlagIsBigEndian) ||
            format->mBitsPerChannel != 32 || format->mChannelsPerFrame != 1 ||
            format->mBytesPerFrame != sizeof(float) || format->mSampleRate != PXSampleRate) {
            [self fail:@"ScreenCaptureKit returned an unexpected format; expected 16 kHz mono float32 audio."];
            return;
        }
        CMItemCount count = CMSampleBufferGetNumSamples(sample);
        if (count <= 0) return;
        if (count > (CMItemCount)PXSampleRate || !isfinite(timestamp)) {
            [self fail:@"ScreenCaptureKit returned an invalid audio packet or timestamp."];
            return;
        }
        AudioBufferList buffers = {0};
        CMBlockBufferRef retainedBuffer = NULL;
        OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample, NULL, &buffers, sizeof(buffers), kCFAllocatorDefault, kCFAllocatorDefault,
            kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, &retainedBuffer);
        if (status != noErr || buffers.mNumberBuffers != 1 ||
            !buffers.mBuffers[0].mData || buffers.mBuffers[0].mDataByteSize < (size_t)count * sizeof(float)) {
            if (retainedBuffer) CFRelease(retainedBuffer);
            [self fail:@"Cannot read the ScreenCaptureKit audio sample buffer."];
            return;
        }
        NSData *data = [NSData dataWithBytes:buffers.mBuffers[0].mData length:(NSUInteger)count * sizeof(float)];
        if (retainedBuffer) CFRelease(retainedBuffer);

        [self.lock lock];
        // Drop queued callbacks from before the last resume, rather than putting paused
        // audio onto the recording's compressed timeline.
        if (!self.closed && timestamp >= self.lastResume && (!self.paused || timestamp < self.pauseStart)) {
            if (self.packets.count >= 64) {
                self.failure = @"The ScreenCaptureKit audio input buffer overflowed.";
            } else {
                PXAudioPacket *packet = [PXAudioPacket new];
                if (self.paused) {
                    NSUInteger validFrames = (NSUInteger)fmax(0.0, floor((self.pauseStart - timestamp) * PXSampleRate));
                    packet.samples = [data subdataWithRange:NSMakeRange(0, MIN(data.length, validFrames * sizeof(float)))];
                } else {
                    packet.samples = data;
                }
                packet.start = llround((timestamp - self.origin - self.pausedSeconds) * PXSampleRate);
                [self.packets addObject:packet];
            }
        }
        [self.lock unlock];
    }
}

- (void)close {
    [self.lock lock];
    self.closed = YES;
    [self.lock unlock];
    // Serialize close against asynchronous stream creation. Pending completion blocks
    // retain this object, so destroying the C handle cannot invalidate a callback.
    dispatch_async(self.lifecycleQueue, ^{
        @autoreleasepool {
            [self.lock lock];
            SCStream *stream = self.stream;
            self.stream = nil;
            [self.lock unlock];
            if (stream) {
                NSError *error = nil;
                [stream removeStreamOutput:self type:SCStreamOutputTypeAudio error:&error];
                [stream stopCaptureWithCompletionHandler:^(NSError *stopError) { (void)stopError; }];
            }
        }
    });
}
@end

int px_sck_available(void) {
    if (@available(macOS 13.0, *)) return 1;
    return 0;
}

int px_sck_permission(int request) {
    if (!px_sck_available()) return 0;
    if (CGPreflightScreenCaptureAccess()) return 1;
    return request && CGRequestScreenCaptureAccess();
}

void *px_sck_create(void) {
    @autoreleasepool {
        PXAudioCapture *capture = [PXAudioCapture new];
        [capture start];
        return (__bridge_retained void *)capture;
    }
}

int px_sck_read(void *handle, float *samples, size_t capacity, size_t *count,
                int64_t *start_frame, char *error, size_t error_capacity) {
    @autoreleasepool {
        PXAudioCapture *capture = (__bridge PXAudioCapture *)handle;
        [capture.lock lock];
        int result = 0;
        *count = 0;
        if (capture.failure) {
            if (error_capacity > 0) snprintf(error, error_capacity, "%s", capture.failure.UTF8String);
            result = -1;
        } else if (capture.packets.count > 0) {
            PXAudioPacket *packet = capture.packets.firstObject;
            size_t frames = packet.samples.length / sizeof(float);
            if (frames > capacity) {
                if (error_capacity > 0) snprintf(error, error_capacity, "ScreenCaptureKit packet exceeds the read buffer.");
                result = -1;
            } else {
                memcpy(samples, packet.samples.bytes, packet.samples.length);
                *count = frames;
                *start_frame = packet.start;
                [capture.packets removeObjectAtIndex:0];
                result = 1;
            }
        }
        [capture.lock unlock];
        return result;
    }
}

int px_sck_started(void *handle) {
    PXAudioCapture *capture = (__bridge PXAudioCapture *)handle;
    [capture.lock lock];
    int started = capture.started;
    [capture.lock unlock];
    return started;
}

int64_t px_sck_clock(void *handle) {
    PXAudioCapture *capture = (__bridge PXAudioCapture *)handle;
    [capture.lock lock];
    double now = capture.paused ? capture.pauseStart : hostTime();
    int64_t frames = (int64_t)floor(fmax(0.0, now - capture.origin - capture.pausedSeconds) * PXSampleRate);
    [capture.lock unlock];
    return frames;
}

void px_sck_pause(void *handle, int paused) {
    PXAudioCapture *capture = (__bridge PXAudioCapture *)handle;
    [capture.lock lock];
    if (capture.paused != (BOOL)paused) {
        double now = hostTime();
        if (paused) {
            capture.pauseStart = now;
        } else {
            capture.pausedSeconds += now - capture.pauseStart;
            capture.lastResume = now;
        }
        capture.paused = (BOOL)paused;
    }
    [capture.lock unlock];
}

void px_sck_close(void *handle) {
    @autoreleasepool {
        PXAudioCapture *capture = (__bridge_transfer PXAudioCapture *)handle;
        [capture close];
    }
}
