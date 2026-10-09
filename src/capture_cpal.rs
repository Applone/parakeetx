use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;

use super::{Control, Device, Devices, Source};
use crate::audio::MonoResampler;

pub fn list() -> Devices {
    let host = cpal::default_host();
    let mut devices = Devices::default();
    match host.input_devices() {
        Ok(inputs) => {
            for device in inputs {
                if let (Ok(description), Ok(id)) = (device.description(), device.id()) {
                    let name = description.name().to_owned();
                    let lowercase = name.to_lowercase();
                    let is_monitor = ["loopback", "blackhole", "soundflower", "monitor", "stereo mix", "aggregate"].iter().any(|marker| lowercase.contains(marker));
                    let entry = Device { id: id.to_string(), label: name, is_monitor };
                    devices.microphones.push(entry.clone());
                    if is_monitor { devices.system.push(entry); }
                }
            }
        }
        Err(error) => devices.note = Some(format!("Cannot list audio devices: {error}")),
    }
    if cfg!(target_os = "windows") {
        for device in host.output_devices().into_iter().flatten() {
            if let (Ok(description), Ok(id)) = (device.description(), device.id()) {
                devices.system.push(Device { id: id.to_string(), label: format!("{} (loopback)", description.name()), is_monitor: true });
            }
        }
    }
    if devices.system.is_empty() {
        devices.note = Some("No system audio device was found. On macOS select a virtual input such as BlackHole or Loopback.".into());
    }
    devices
}

pub fn run(source: Source, device: Option<String>, control: Control, sender: Sender<(Source, Vec<f32>)>) -> Result<()> {
    let host = cpal::default_host();
    let selected = match device {
        Some(id) => host.devices()?.find(|candidate| candidate.id().map(|value| value.to_string() == id).unwrap_or(false))
            .context("The selected audio device is no longer available. Choose another device in Settings.")?,
        None => host.default_input_device().context("No input device is available")?,
    };
    let config = selected.default_input_config().or_else(|_| selected.default_output_config()).context("Cannot read the device configuration")?;
    let channels = config.channels() as usize;
    let mut resampler = MonoResampler::new(config.sample_rate())?;
    let (samples_sender, samples_receiver) = crossbeam_channel::bounded::<Vec<f32>>(64);
    let (error_sender, error_receiver) = crossbeam_channel::bounded::<String>(1);
    let errors = error_sender.clone();
    let error_handler = move |error: cpal::StreamError| { let _ = errors.try_send(error.to_string()); };
    let capture_control = control.clone();
    let callback_errors = error_sender.clone();
    let forward = move |samples: Vec<f32>| {
        if capture_control.is_paused() || capture_control.is_stopped() { return; }
        if samples_sender.try_send(samples).is_err() { let _ = callback_errors.try_send("The audio input buffer overflowed".into()); }
    };
    let stream_config: cpal::StreamConfig = config.clone().into();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => selected.build_input_stream(&stream_config, move |data: &[f32], _| forward(data.to_vec()), error_handler, None)?,
        cpal::SampleFormat::I16 => selected.build_input_stream(&stream_config, move |data: &[i16], _| forward(data.iter().map(|sample| f32::from(*sample) / 32768.0).collect()), error_handler, None)?,
        cpal::SampleFormat::U16 => selected.build_input_stream(&stream_config, move |data: &[u16], _| forward(data.iter().map(|sample| f32::from(*sample) / 32768.0 - 1.0).collect()), error_handler, None)?,
        cpal::SampleFormat::I32 => selected.build_input_stream(&stream_config, move |data: &[i32], _| forward(data.iter().map(|sample| *sample as f32 / 2147483648.0).collect()), error_handler, None)?,
        cpal::SampleFormat::F64 => selected.build_input_stream(&stream_config, move |data: &[f64], _| forward(data.iter().map(|sample| *sample as f32).collect()), error_handler, None)?,
        format => bail!("Unsupported audio sample format: {format}"),
    };
    stream.play().context("Cannot start audio capture")?;
    while !control.is_stopped() {
        if let Ok(error) = error_receiver.try_recv() { bail!("Audio capture failed: {error}"); }
        if let Ok(samples) = samples_receiver.recv_timeout(std::time::Duration::from_millis(20)) {
            let converted = resampler.push(&crate::audio::downmix(&samples, channels))?;
            if !converted.is_empty() && sender.send_timeout((source, converted), std::time::Duration::from_secs(1)).is_err() {
                bail!("The recording writer cannot keep up with audio capture");
            }
        }
    }
    drop(stream);
    let tail = resampler.finish()?;
    if !tail.is_empty() { let _ = sender.send_timeout((source, tail), std::time::Duration::from_millis(100)); }
    Ok(())
}
