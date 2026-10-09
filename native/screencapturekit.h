#pragma once
#include <stddef.h>
#include <stdint.h>

// All handles own their Objective-C session. No native callback borrows Rust memory.
int px_sck_available(void);
int px_sck_permission(int request);
void *px_sck_create(void);
// 0: no packet, 1: packet copied, -1: capture failed. Samples are mono float32 at 16 kHz.
int px_sck_read(void *handle, float *samples, size_t capacity, size_t *count,
                int64_t *start_frame, char *error, size_t error_capacity);
int px_sck_started(void *handle);
int64_t px_sck_clock(void *handle);
void px_sck_pause(void *handle, int paused);
void px_sck_close(void *handle);
