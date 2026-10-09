use std::{cell::{Cell, RefCell}, rc::Rc, thread, time::{Duration, Instant}};

use anyhow::{Context, Result, bail, ensure};
use crossbeam_channel::Sender;
use libpulse_binding::{
    callbacks::ListResult,
    context::{Context as PulseContext, FlagSet as ContextFlags, State},
    def::BufferAttr,
    mainloop::standard::{IterateResult, Mainloop},
    proplist::{Proplist, properties},
    sample::{Format, Spec},
    stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream},
};

use super::{Control, Device, Devices, Source};
use crate::audio::MonoResampler;

fn iterate(mainloop: &mut Mainloop) -> Result<()> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) => bail!("The audio server disconnected"),
        IterateResult::Err(error) => Err(error).context("PulseAudio event loop failed"),
    }
}

fn connect() -> Result<(Mainloop, PulseContext)> {
    let mut properties = Proplist::new().context("PulseAudio is unavailable")?;
    properties.set_str(properties::APPLICATION_NAME, "parakeetx").map_err(|_| anyhow::anyhow!("Cannot describe audio client"))?;
    let mut mainloop = Mainloop::new().context("Cannot start the PulseAudio event loop")?;
    let mut context = PulseContext::new_with_proplist(&mainloop, "parakeetx", &properties).context("Cannot create a PulseAudio context")?;
    context.connect(None, ContextFlags::NOFLAGS, None).context("Cannot reach PulseAudio or PipeWire")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        ensure!(Instant::now() < deadline, "Timed out connecting to the audio server");
        iterate(&mut mainloop)?;
        match context.get_state() {
            State::Ready => return Ok((mainloop, context)),
            State::Failed | State::Terminated => bail!("Cannot connect to PulseAudio or PipeWire"),
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
}

pub fn list() -> Devices {
    collect().unwrap_or_else(|error| Devices { note: Some(format!("{error:#}")), ..Default::default() })
}

fn collect() -> Result<Devices> {
    let (mut mainloop, context) = connect()?;
    let collected = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let sink = collected.clone();
    let finished = done.clone();
    let error = failed.clone();
    let operation = context.introspect().get_source_info_list(move |result| match result {
        ListResult::Item(info) => {
            if let Some(name) = info.name.as_deref() {
                sink.borrow_mut().push(Device {
                    id: name.to_owned(), label: info.description.as_deref().unwrap_or(name).to_owned(),
                    is_monitor: info.monitor_of_sink.is_some() || name.ends_with(".monitor"),
                });
            }
        }
        ListResult::End => finished.set(true),
        ListResult::Error => { error.set(true); finished.set(true); }
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    while !done.get() {
        ensure!(Instant::now() < deadline, "Timed out listing audio sources");
        iterate(&mut mainloop)?;
        thread::sleep(Duration::from_millis(5));
    }
    drop(operation);
    ensure!(!failed.get(), "The audio server could not list capture sources");
    let mut devices = Devices::default();
    for device in collected.borrow().iter().cloned() {
        if device.is_monitor { devices.system.push(device); } else { devices.microphones.push(device); }
    }
    if devices.system.is_empty() { devices.note = Some("No system-audio monitor source was found. Check your PulseAudio or PipeWire output configuration.".into()); }
    Ok(devices)
}

pub fn run(source: Source, device: Option<String>, control: Control, sender: Sender<(Source, Vec<f32>)>) -> Result<()> {
    ensure!(!device.as_deref().is_some_and(|name| name.contains('\0')), "Device name contains an invalid character");
    let (mut mainloop, mut context) = connect()?;
    let specification = Spec { format: Format::F32le, channels: 2, rate: 48_000 };
    let mut stream = Stream::new(&mut context, "capture", &specification, None).context("Cannot create a capture stream")?;
    let attributes = BufferAttr { maxlength: u32::MAX, fragsize: 4 * u32::from(specification.channels) * specification.rate / 25, ..Default::default() };
    stream.connect_record(device.as_deref(), Some(&attributes), StreamFlags::ADJUST_LATENCY).context("Cannot open the selected audio source")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if control.is_stopped() { return Ok(()); }
        ensure!(Instant::now() < deadline, "Timed out opening the capture device");
        iterate(&mut mainloop)?;
        match stream.get_state() {
            StreamState::Ready => break,
            StreamState::Failed | StreamState::Terminated => bail!("The selected capture source could not be opened"),
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut resampler = MonoResampler::new(specification.rate)?;
    let mut paused = false;
    while !control.is_stopped() {
        iterate(&mut mainloop)?;
        ensure!(!matches!(stream.get_state(), StreamState::Failed | StreamState::Terminated), "The capture device disconnected");
        if control.is_paused() {
            if !paused { stream.cork(None); stream.flush(None); paused = true; }
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if paused { stream.flush(None); stream.uncork(None); paused = false; }
        let mono = match stream.peek()? {
            PeekResult::Data(bytes) => {
                let samples: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|value| f32::from_le_bytes(*value)).collect();
                let mono = crate::audio::downmix(&samples, specification.channels as usize);
                stream.discard()?;
                mono
            }
            PeekResult::Hole(bytes) => {
                let silence = vec![0.0; bytes / 4 / specification.channels as usize];
                stream.discard()?;
                silence
            }
            PeekResult::Empty => { thread::sleep(Duration::from_millis(5)); continue; }
        };
        let converted = resampler.push(&mono)?;
        if !converted.is_empty() {
            sender.send_timeout((source, converted), Duration::from_secs(1)).context("The recording writer cannot keep up with capture")?;
        }
    }
    let tail = resampler.finish()?;
    if !tail.is_empty() { let _ = sender.send_timeout((source, tail), Duration::from_millis(100)); }
    Ok(())
}
