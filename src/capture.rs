use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use anyhow::Result;
use crossbeam_channel::Sender;

#[cfg(target_os = "linux")]
#[path = "capture_pulse.rs"]
mod backend;
#[cfg(not(target_os = "linux"))]
#[path = "capture_cpal.rs"]
mod backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub label: String,
    pub is_monitor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source { Microphone, System }

#[derive(Debug, Clone, Default)]
pub struct Devices {
    pub microphones: Vec<Device>,
    pub system: Vec<Device>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Control {
    paused: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}

impl Control {
    pub fn new() -> Self { Self::default() }
    pub fn set_paused(&self, paused: bool) { self.paused.store(paused, Ordering::Relaxed); }
    pub fn is_paused(&self) -> bool { self.paused.load(Ordering::Relaxed) }
    pub fn stop(&self) { self.stopped.store(true, Ordering::Relaxed); }
    pub fn is_stopped(&self) -> bool { self.stopped.load(Ordering::Relaxed) }
}

pub fn mix(primary: &[f32], secondary: &[f32]) -> Vec<f32> {
    (0..primary.len().max(secondary.len())).map(|index| {
        soft_clip(primary.get(index).copied().unwrap_or(0.0) + secondary.get(index).copied().unwrap_or(0.0))
    }).collect()
}

fn soft_clip(sample: f32) -> f32 {
    if !sample.is_finite() { return 0.0; }
    if sample.abs() <= 0.8 { sample } else { sample.signum() * (0.8 + (sample.abs() - 0.8).tanh() * 0.2) }
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().filter(|sample| sample.is_finite()).fold(0.0f32, |maximum, sample| maximum.max(sample.abs()))
}

pub fn devices() -> Devices { backend::list() }

pub fn capture(source: Source, device: Option<String>, control: Control, sender: Sender<(Source, Vec<f32>)>) -> Result<()> {
    backend::run(source, device, control, sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_limits_output_and_handles_length_mismatch() {
        let mixed = mix(&[0.9, 0.9, 0.5], &[0.9, -0.9]);
        assert_eq!(mixed.len(), 3);
        assert!(mixed.iter().all(|sample| sample.abs() <= 1.0));
        assert!(mixed[1].abs() < 1e-6);
        assert!((mixed[2] - 0.5).abs() < 1e-6);
        assert_eq!(mix(&[f32::INFINITY], &[0.0]), vec![0.0]);
    }

    #[test]
    fn control_tracks_pause_and_stop() {
        let control = Control::new();
        assert!(!control.is_paused() && !control.is_stopped());
        control.set_paused(true);
        control.stop();
        assert!(control.is_paused() && control.is_stopped());
        assert!((peak(&[0.2, -0.7, f32::NAN]) - 0.7).abs() < 1e-6);
    }
}
