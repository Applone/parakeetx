use std::{ffi::{c_char, c_void}, ptr::NonNull, thread, time::{Duration, Instant}};

use anyhow::{Context, Result, bail, ensure};
use crossbeam_channel::Sender;

use super::{Control, Device, Source, timeline::AudioTimeline};
use crate::SAMPLE_RATE;

pub const SYSTEM_DEVICE_ID: &str = "screencapturekit:system-audio";

unsafe extern "C" {
    fn px_sck_available() -> i32;
    fn px_sck_permission(request: i32) -> i32;
    fn px_sck_create() -> *mut c_void;
    fn px_sck_read(handle: *mut c_void, samples: *mut f32, capacity: usize, count: *mut usize,
        start_frame: *mut i64, error: *mut c_char, error_capacity: usize) -> i32;
    fn px_sck_started(handle: *mut c_void) -> i32;
    fn px_sck_clock(handle: *mut c_void) -> i64;
    fn px_sck_pause(handle: *mut c_void, paused: i32);
    fn px_sck_close(handle: *mut c_void);
}

pub fn device() -> Option<Device> {
    // This query checks the OS version only; listing devices never prompts for permission.
    (unsafe { px_sck_available() } != 0).then(|| Device {
        id: SYSTEM_DEVICE_ID.into(), label: "System audio (ScreenCaptureKit)".into(), is_monitor: true,
    })
}

pub fn check_permission() -> Result<()> {
    ensure!(unsafe { px_sck_available() } != 0, "ScreenCaptureKit system audio requires macOS 13 or newer");
    ensure!(unsafe { px_sck_permission(1) } != 0,
        "Allow parakeetx in System Settings → Privacy & Security → Screen & System Audio Recording (Screen Recording on older macOS), then restart the app");
    Ok(())
}

struct NativeCapture(NonNull<c_void>);

impl Drop for NativeCapture {
    fn drop(&mut self) {
        // Native asynchronous work owns its session; it never holds Rust pointers.
        unsafe { px_sck_close(self.0.as_ptr()) };
    }
}

impl NativeCapture {
    fn clock(&self) -> u64 { unsafe { px_sck_clock(self.0.as_ptr()) }.max(0) as u64 }

    fn drain(&self, timeline: &mut AudioTimeline, buffer: &mut [f32]) -> Result<()> {
        for _ in 0..64 {
            let mut count = 0;
            let mut start = 0;
            let mut error = [0u8; 2048];
            // The bridge copies at most buffer.len() initialized float samples. Error text is
            // copied to the bounded byte buffer, and no pointers survive this synchronous call.
            let status = unsafe { px_sck_read(self.0.as_ptr(), buffer.as_mut_ptr(), buffer.len(),
                &mut count, &mut start, error.as_mut_ptr().cast(), error.len()) };
            match status {
                0 => return Ok(()),
                1 => {
                    ensure!(count <= buffer.len(), "ScreenCaptureKit returned an oversized audio packet");
                    timeline.push(start, &buffer[..count])?;
                }
                _ => {
                    let end = error.iter().position(|byte| *byte == 0).unwrap_or(error.len());
                    bail!("{}", String::from_utf8_lossy(&error[..end]));
                }
            }
        }
        Ok(())
    }
}

fn forward(timeline: &mut AudioTimeline, target: u64, sender: &Sender<(Source, Vec<f32>)>) -> Result<()> {
    loop {
        let samples = timeline.take_until(target, SAMPLE_RATE as usize / 50);
        if samples.is_empty() { return Ok(()); }
        sender.send_timeout((Source::System, samples), Duration::from_secs(1))
            .context("The recording writer cannot keep up with system audio capture")?;
    }
}

pub fn run(control: Control, sender: Sender<(Source, Vec<f32>)>) -> Result<()> {
    ensure!(unsafe { px_sck_permission(0) } != 0, "System audio permission is unavailable. Allow Screen & System Audio Recording and restart parakeetx.");
    let native = NativeCapture(NonNull::new(unsafe { px_sck_create() }).context("Cannot create the ScreenCaptureKit session")?);
    let mut timeline = AudioTimeline::new();
    let mut buffer = vec![0.0; SAMPLE_RATE as usize];
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut paused = false;
    // Leave a short delivery window for asynchronous packets before filling gaps with silence.
    let latency = u64::from(SAMPLE_RATE) / 10;
    while !control.is_stopped() {
        let changed = paused != control.is_paused();
        if changed {
            paused = control.is_paused();
            unsafe { px_sck_pause(native.0.as_ptr(), i32::from(paused)) };
            // Let already sampled pre-pause packets reach the native queue before filling
            // the end of the interval with silence. The native clock is frozen throughout.
            if paused { thread::sleep(Duration::from_millis(100)); }
        }
        native.drain(&mut timeline, &mut buffer)?;
        let target = native.clock();
        if paused {
            forward(&mut timeline, target, &sender)?;
            // A packet can extend beyond the exact pause boundary.
            timeline.discard_pending();
        } else {
            forward(&mut timeline, target.saturating_sub(latency), &sender)?;
        }
        ensure!(unsafe { px_sck_started(native.0.as_ptr()) } != 0 || Instant::now() < deadline,
            "Timed out starting ScreenCaptureKit system audio. Check Screen & System Audio Recording permission.");
        thread::sleep(Duration::from_millis(5));
    }
    // Freeze the native clock and drain all remaining pre-stop audio before releasing the stream.
    unsafe { px_sck_pause(native.0.as_ptr(), 1) };
    if !paused { thread::sleep(Duration::from_millis(100)); }
    native.drain(&mut timeline, &mut buffer)?;
    forward(&mut timeline, native.clock(), &sender)
}
