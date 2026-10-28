use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::{
    SAMPLE_RATE,
    asr::{self, TranscribeOptions, Transcriber, Transcript},
    audio,
    capture::{self, Control, Source},
    diarize,
    domain::{PipelineInfo, Recording, RecordingStatus, Word},
    settings::{AudioQuality, Secrets, Settings},
    storage::Library,
    vad::{self, Speech, Vad, VadOptions},
};

mod live;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Recording,
    Transcription,
    Diarization,
    Import,
    Summary,
    Download,
    PythonCheck,
    PythonSetup,
    SpeakerModelCheck,
}

#[derive(Debug, Clone)]
pub enum Event {
    Recording(Box<Recording>),
    LiveTranscript { recording_id: String, provisional: Vec<Word> },
    AudioSaved,
    QueueDiarization { recording_id: String, settings: Box<Settings>, secrets: Secrets },
    Levels { duration: f64, microphone: f32, system: f32 },
    Progress { label: String, fraction: f32 },
    Warning(String),
    SpeakerDetection { revision: u64, result: crate::diarization_setup::Detection },
    SpeakerSetup { revision: u64, result: crate::diarization_setup::SetupResult },
    SpeakerModelReady { revision: u64 },
    Finished(std::result::Result<String, String>),
}

pub struct Job {
    pub kind: JobKind,
    pub events: Receiver<Event>,
    pub cancel: Arc<AtomicBool>,
    pub capture: Control,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct Reporter {
    sender: Sender<Event>,
    pub cancel: Arc<AtomicBool>,
    pub capture: Control,
}

impl Reporter {
    pub fn speaker_detection(&self, revision: u64, result: crate::diarization_setup::Detection) {
        let _ = self.sender.send(Event::SpeakerDetection { revision, result });
    }

    pub fn speaker_setup(&self, revision: u64, result: crate::diarization_setup::SetupResult) {
        let _ = self.sender.send(Event::SpeakerSetup { revision, result });
    }

    pub fn speaker_model_ready(&self, revision: u64) {
        let _ = self.sender.send(Event::SpeakerModelReady { revision });
    }

    pub fn progress(&self, label: impl Into<String>, fraction: f32) {
        let _ = self.sender.send(Event::Progress { label: label.into(), fraction: fraction.clamp(0.0, 1.0) });
    }

    pub fn warning(&self, message: impl Into<String>) {
        let _ = self.sender.send(Event::Warning(message.into()));
    }

    pub fn recording(&self, recording: &Recording) {
        let _ = self.sender.send(Event::Recording(Box::new(recording.clone())));
    }

    pub fn live_transcript(&self, recording_id: &str, provisional: Vec<Word>) {
        let _ = self.sender.send(Event::LiveTranscript { recording_id: recording_id.into(), provisional });
    }

    pub(crate) fn queue_diarization(&self, recording: &Recording, settings: Settings, secrets: Secrets) {
        if settings.diarization_enabled && !recording.segments.is_empty() && !self.cancel.load(Ordering::Relaxed) {
            let _ = self.sender.send(Event::QueueDiarization { recording_id: recording.id.clone(), settings: Box::new(settings), secrets });
        }
    }

    pub fn check_cancelled(&self) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "Operation cancelled");
        Ok(())
    }
}

impl Job {
    pub fn spawn(kind: JobKind, work: impl FnOnce(Reporter) -> Result<String> + Send + 'static) -> Self {
        let (sender, events) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let capture = Control::new();
        let reporter = Reporter { sender, cancel: cancel.clone(), capture: capture.clone() };
        let handle = thread::spawn(move || {
            let sender = reporter.sender.clone();
            let control = reporter.capture.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(reporter)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("The background operation stopped unexpectedly. Saved audio is still in the library.")))
                .map_err(|error| format!("{error:#}"));
            control.stop();
            let _ = sender.send(Event::Finished(outcome));
        });
        Self { kind, events, cancel, capture, thread: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.capture.stop();
    }

    pub fn finish_recording(&self) {
        self.capture.stop();
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) && let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn import(source: PathBuf, settings: Settings, library: Library, reporter: Reporter) -> Result<String> {
    settings.prepare_directories()?;
    let title = source.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported recording".into());
    let mut recording = Recording::new(title, &settings.recordings_dir);
    reporter.progress("Decoding and resampling audio", 0.0);
    let mut last = -1.0;
    recording.duration = audio::import_media(&source, &recording.audio_path, &reporter.cancel, |fraction| {
        if fraction - last >= 0.01 {
            reporter.progress("Decoding and resampling audio", fraction);
            last = fraction;
        }
    })?;
    recording.original_path = Some(source);
    library.save(&recording).context("Audio was imported but library metadata could not be saved")?;
    store_audio(&mut recording, &settings, &library, &reporter);
    reporter.recording(&recording);
    Ok("Recording imported. Ready to transcribe.".into())
}

pub fn transcribe(mut recording: Recording, settings: Settings, secrets: Secrets, library: Library, reporter: Reporter) -> Result<String> {
    settings.validate()?;
    reporter.check_cancelled()?;
    reporter.progress("Loading speech recognition model", 0.0);
    let mut transcriber = Transcriber::load_with_threads(&settings.model_path(), settings.model, settings.use_gpu, settings.alignment_enabled, settings.threads)?;
    let mut vad = if settings.uses_vad() { Some(Vad::load(&settings.vad_path())?) } else { None };
    let previous = recording.clone();
    let replacing_transcript = !previous.segments.is_empty();
    recording.language = None;
    recording.status = RecordingStatus::Transcribing;
    recording.error = None;
    recording.segments.clear();
    recording.summary.clear();
    recording.pipeline = pipeline_info(&settings);
    if !replacing_transcript { library.save(&recording)?; }
    reporter.recording(&recording);
    let result = (|| -> Result<()> {
        let prepared = audio::PreparedAudio::open(&recording.audio_path, &reporter.cancel, |fraction| reporter.progress("Preparing saved audio", fraction))?;
        let audio_path = prepared.path();
        let reader = audio::open_wave(audio_path)?;
        let samples = reader.duration() as usize;
        recording.duration = samples as f64 / f64::from(SAMPLE_RATE);
        ensure!(samples > 0, "This recording has no audio");
        let regions = if let Some(detector) = vad.as_mut() {
            reporter.progress("Finding speech with Silero VAD", 0.0);
            let mut reader = audio::open_wave(audio_path)?;
            let mut input = reader.samples::<i16>();
            let mut probabilities = Vec::with_capacity(samples / vad::WINDOW + 1);
            let mut processed = 0usize;
            loop {
                reporter.check_cancelled()?;
                let mut frame = Vec::with_capacity(vad::WINDOW);
                for _ in 0..vad::WINDOW {
                    match input.next() {
                        Some(sample) => frame.push(f32::from(sample?) / 32768.0),
                        None => break,
                    }
                }
                if frame.is_empty() { break; }
                processed += frame.len();
                frame.resize(vad::WINDOW, 0.0);
                probabilities.push(detector.probability(&frame)?);
                if probabilities.len() % 100 == 0 {
                    reporter.progress("Finding speech with Silero VAD", processed as f32 / samples as f32);
                }
            }
            vad::collect_speech(&probabilities, samples, VadOptions { threshold: settings.vad_threshold, ..Default::default() })
        } else {
            vec![Speech { start: 0.0, end: recording.duration }]
        };
        let chunks = if regions.is_empty() { Vec::new() } else { vad::merge_for_asr(&regions, 28.0, recording.duration) };
        for (index, chunk) in chunks.iter().enumerate() {
            reporter.check_cancelled()?;
            reporter.progress(format!("Transcribing speech region {} of {}", index + 1, chunks.len()), index as f32 / chunks.len() as f32);
            let start = (chunk.start * f64::from(SAMPLE_RATE)).round() as u64;
            let length = ((chunk.end - chunk.start) * f64::from(SAMPLE_RATE)).ceil() as usize;
            let samples = audio::read_range(audio_path, start, length)?;
            if samples.len() < SAMPLE_RATE as usize / 10 { continue; }
            let options = TranscribeOptions { language: language_for(&settings, &recording), threads: settings.threads, offset: start as f64 / f64::from(SAMPLE_RATE), ..Default::default() };
            let result = transcriber.run(&samples, &options, &reporter.cancel, |_| {})?;
            append_transcript(&mut recording, result);
            if !replacing_transcript { library.save(&recording)?; }
            reporter.recording(&recording);
        }
        recording.status = RecordingStatus::Ready;
        if !replacing_transcript { library.save(&recording)?; }
        Ok(())
    })();
    if let Err(error) = result {
        if !previous.segments.is_empty() {
            recording = previous;
        } else {
            recording.status = if recording.segments.is_empty() { RecordingStatus::Recorded } else { RecordingStatus::Partial };
        }
        recording.error = Some(format!("{error:#}"));
        library.save(&recording)?;
        reporter.recording(&recording);
        return Err(error);
    }
    library.save(&recording)?;
    reporter.recording(&recording);
    reporter.queue_diarization(&recording, settings, secrets);
    Ok(if recording.segments.is_empty() { "Transcription complete. No speech was detected.".into() } else { "Transcription complete.".into() })
}

pub fn summarize(mut recording: Recording, settings: Settings, secrets: Secrets, library: Library, reporter: Reporter) -> Result<String> {
    let mut summary_settings = settings;
    if !recording.summary_prompt.trim().is_empty() {
        summary_settings.summary_prompt = recording.summary_prompt.clone();
    }
    reporter.progress("Generating notes", 0.0);
    let summary = crate::summary::summarize(&summary_settings, &secrets.api_key, &recording.transcript(), &reporter.cancel, |fraction| reporter.progress("Generating notes", fraction))?;
    reporter.check_cancelled()?;
    recording.summary = summary;
    library.save(&recording)?;
    reporter.recording(&recording);
    Ok("Notes saved to the recording.".into())
}

fn pipeline_info(settings: &Settings) -> PipelineInfo {
    PipelineInfo { model: settings.model.slug().into(), vad: settings.uses_vad(), dtw: settings.uses_dtw(), word_timestamps: settings.model.is_parakeet() || settings.alignment_enabled, diarization: false }
}

fn language_for(settings: &Settings, recording: &Recording) -> String {
    if settings.language.is_empty() { recording.language.clone().unwrap_or_default() } else { settings.language.clone() }
}

fn append_transcript(recording: &mut Recording, transcript: Transcript) {
    if recording.language.is_none() && !transcript.segments.is_empty() {
        recording.language = transcript.language;
    }
    asr::merge(&mut recording.segments, transcript.segments);
}

pub fn identify_speakers(mut recording: Recording, settings: Settings, secrets: Secrets, library: Library, reporter: Reporter) -> Result<String> {
    ensure!(!recording.segments.is_empty(), "Transcribe this recording before identifying speakers");
    let result = (|| -> Result<()> {
        reporter.check_cancelled()?;
        let prepared = audio::PreparedAudio::open(&recording.audio_path, &reporter.cancel, |fraction| reporter.progress("Preparing saved audio", fraction))?;
        reporter.progress("Identifying speakers in the Python worker", 0.0);
        let turns = diarize::run(&settings, &secrets.huggingface_token, prepared.path(), recording.duration, &reporter.cancel)?;
        reporter.check_cancelled()?;
        diarize::assign(&mut recording.segments, &turns);
        recording.segments = diarize::split_on_speaker_change(std::mem::take(&mut recording.segments));
        recording.pipeline.diarization = true;
        Ok(())
    })();
    if let Err(error) = &result {
        recording.error = Some(format!("Transcript saved, but speaker identification failed: {error:#}"));
    } else if recording.error.as_ref().is_some_and(|error| error.starts_with("Transcript saved, but speaker identification failed:")) {
        recording.error = None;
    }
    library.save(&recording)?;
    reporter.recording(&recording);
    result?;
    Ok("Speaker identification complete.".into())
}

struct SourceBuffer {
    queue: VecDeque<f32>,
    debt: usize,
    enabled: bool,
}

impl SourceBuffer {
    fn new(enabled: bool) -> Self { Self { queue: VecDeque::new(), debt: 0, enabled } }

    fn push(&mut self, samples: Vec<f32>) {
        let discard = self.debt.min(samples.len());
        self.debt -= discard;
        self.queue.extend(samples.into_iter().skip(discard));
    }

    fn take(&mut self, count: usize, pad: bool) -> Vec<f32> {
        let available = count.min(self.queue.len());
        let mut samples: Vec<f32> = self.queue.drain(..available).collect();
        if pad && self.enabled {
            self.debt += count - available;
        }
        samples.resize(count, 0.0);
        samples
    }
}

enum LiveUpdate { Whisper(Transcript), Parakeet(live::Update) }

type LiveResult = std::result::Result<LiveUpdate, String>;

fn live_worker(settings: Settings, path: PathBuf, ranges: Receiver<(u64, usize)>, results: Sender<LiveResult>, cancel: Arc<AtomicBool>) -> Result<()> {
    let mut transcriber = Transcriber::load_with_threads(&settings.model_path(), settings.model, settings.use_gpu, settings.alignment_enabled, settings.threads)?;
    let mut detector = if settings.uses_vad() { Some(Vad::load(&settings.vad_path())?) } else { None };
    let mut language = settings.language.clone();
    for (start, length) in ranges {
        ensure!(!cancel.load(Ordering::Relaxed), "Live transcription cancelled");
        let samples = audio::read_range(&path, start, length)?;
        if samples.len() < SAMPLE_RATE as usize / 10 { continue; }
        let regions = if let Some(detector) = detector.as_mut() {
            detector.segments(&samples, VadOptions { threshold: settings.vad_threshold, ..Default::default() })?
        } else {
            vec![Speech { start: 0.0, end: samples.len() as f64 / f64::from(SAMPLE_RATE) }]
        };
        for region in regions {
            let begin = (region.start * f64::from(SAMPLE_RATE)) as usize;
            let end = ((region.end * f64::from(SAMPLE_RATE)).ceil() as usize).min(samples.len());
            if end.saturating_sub(begin) < SAMPLE_RATE as usize / 10 { continue; }
            let options = TranscribeOptions {
                language: language.clone(), threads: settings.threads, offset: (start + begin as u64) as f64 / f64::from(SAMPLE_RATE), single_pass: true, ..Default::default()
            };
            let transcript = transcriber.run(&samples[begin..end], &options, &cancel, |_| {})?;
            if language.is_empty() && !transcript.segments.is_empty() {
                language = transcript.language.clone().unwrap_or_default();
            }
            if results.send(Ok(LiveUpdate::Whisper(transcript))).is_err() { return Ok(()); }
        }
    }
    Ok(())
}

fn parakeet_live_worker(settings: Settings, path: PathBuf, input: live::Input, results: Sender<LiveResult>, cancel: Arc<AtomicBool>) -> Result<()> {
    let mut transcriber = Transcriber::load_with_threads(&settings.model_path(), settings.model, settings.use_gpu, settings.alignment_enabled, settings.threads)?;
    let mut detector = if settings.uses_vad() { Some(Vad::load(&settings.vad_path())?) } else { None };
    live::run(input, &cancel, |start, length| {
        let samples = audio::read_range(&path, start, length)?;
        ensure!(samples.len() == length, "Live audio window was not completely flushed");
        // Gate the entire context window, rather than trimming speech boundaries.
        // Retained speech keeps the decoder running long enough to finalize its tail.
        if let Some(detector) = detector.as_mut()
            && !detector.contains_speech(&samples, settings.vad_threshold)? {
            return Ok(Transcript { segments: Vec::new(), language: None });
        }
        let options = TranscribeOptions { threads: settings.threads, offset: start as f64 / f64::from(SAMPLE_RATE), single_pass: true, ..Default::default() };
        transcriber.run(&samples, &options, &cancel, |_| {})
    }, |update| {
        results.send(Ok(LiveUpdate::Parakeet(update))).map_err(|_| anyhow::anyhow!("Live transcript receiver closed"))
    })
}

pub fn record(title: String, settings: Settings, secrets: Secrets, library: Library, reporter: Reporter) -> Result<String> {
    settings.validate()?;
    settings.prepare_directories()?;
    ensure!(settings.microphone_enabled || settings.system_enabled, "Enable a microphone or system-audio source in Settings");
    let devices = capture::devices();
    let microphone = if settings.microphone_enabled { Some(resolve_device(&devices.microphones, settings.microphone_device.as_deref(), "microphone")?) } else { None };
    let system = if settings.system_enabled { Some(resolve_device(&devices.system, settings.system_device.as_deref(), "system audio")?) } else { None };
    // Request system audio permission before starting either capture thread or creating audio.
    capture::check_permissions(system.as_deref())?;
    let title = if title.trim().is_empty() { format!("Recording {}", chrono::Local::now().format("%Y-%m-%d %H:%M")) } else { title.trim().into() };
    let mut recording = Recording::new(title, &settings.recordings_dir);
    recording.status = RecordingStatus::Recording;
    recording.pipeline = pipeline_info(&settings);
    let mut writer = hound::WavWriter::create(&recording.audio_path, audio::wave_spec())?;
    writer.flush()?;
    library.save(&recording)?;
    reporter.recording(&recording);
    let (audio_sender, audio_receiver) = crossbeam_channel::bounded(128);
    let (error_sender, error_receiver) = unbounded();
    let mut capture_threads = Vec::new();
    for (source, device) in [(Source::Microphone, microphone), (Source::System, system)] {
        if let Some(device) = device {
            let control = reporter.capture.clone();
            let sender = audio_sender.clone();
            let errors = error_sender.clone();
            capture_threads.push(thread::spawn(move || {
                if let Err(error) = capture::capture(source, Some(device), control, sender) {
                    let _ = errors.send(format!("{source:?}: {error:#}"));
                }
            }));
        }
    }
    drop(audio_sender);
    drop(error_sender);
    let (range_sender, range_receiver) = unbounded();
    let (live_schedule, live_input) = live::channel();
    let (result_sender, result_receiver) = unbounded();
    let live_thread = if settings.live_transcription {
        let config = settings.clone();
        let path = recording.audio_path.clone();
        let cancel = reporter.cancel.clone();
        Some(thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if config.model.is_parakeet() {
                    parakeet_live_worker(config, path, live_input, result_sender.clone(), cancel)
                } else {
                    live_worker(config, path, range_receiver, result_sender.clone(), cancel)
                }
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => { let _ = result_sender.send(Err(format!("{error:#}"))); }
                Err(_) => { let _ = result_sender.send(Err("Speech recognition stopped unexpectedly".into())); }
            }
        }))
    } else { None };
    let mut first = SourceBuffer::new(settings.microphone_enabled);
    let mut second = SourceBuffer::new(settings.system_enabled);
    let mut written = 0u64;
    let mut queued = 0u64;
    let mut flush_at = Instant::now();
    let mut waiting_since = Instant::now();
    let mut level_at = Instant::now();
    let mut microphone_peak = 0.0f32;
    let mut system_peak = 0.0f32;
    let mut live_failed = !settings.live_transcription;
    let chunk_size = if settings.model.is_parakeet() { live::UPDATE_SAMPLES } else { u64::from(settings.chunk_seconds * SAMPLE_RATE) };
    let mut capture_error = None;
    let mut was_paused = false;
    let outcome = (|| -> Result<()> {
        while !reporter.capture.is_stopped() {
            if let Ok(error) = error_receiver.try_recv() {
                capture_error = Some(error);
                break;
            }
            if reporter.cancel.load(Ordering::Relaxed) { break; }
            if let Ok((source, samples)) = audio_receiver.recv_timeout(Duration::from_millis(10)) {
                match source {
                    Source::Microphone => first.push(samples),
                    Source::System => second.push(samples),
                }
            }
            for (source, samples) in audio_receiver.try_iter() {
                match source {
                    Source::Microphone => first.push(samples),
                    Source::System => second.push(samples),
                }
            }
            if was_paused != reporter.capture.is_paused() {
                was_paused = reporter.capture.is_paused();
                first.debt = 0;
                second.debt = 0;
                waiting_since = Instant::now();
            }
            let count = 320usize;
            while first.queue.len().max(second.queue.len()) >= count {
                let complete = (!first.enabled || first.queue.len() >= count) && (!second.enabled || second.queue.len() >= count);
                let buffered = first.queue.len().max(second.queue.len());
                ensure!(buffered <= SAMPLE_RATE as usize * 3, "Audio input buffering exceeded three seconds; capture was stopped to protect timing");
                if !complete && waiting_since.elapsed() < Duration::from_millis(160) && buffered < SAMPLE_RATE as usize / 5 { break; }
                let microphone = first.take(count, !complete);
                let system = second.take(count, !complete);
                microphone_peak = microphone_peak.max(capture::peak(&microphone));
                system_peak = system_peak.max(capture::peak(&system));
                audio::write_samples(&mut writer, &capture::mix(&microphone, &system))?;
                written += count as u64;
                waiting_since = Instant::now();
            }
            recording.duration = written as f64 / f64::from(SAMPLE_RATE);
            if flush_at.elapsed() >= Duration::from_secs(1) || (!live_failed && written - queued >= chunk_size) {
                writer.flush()?;
                if !live_failed && written - queued >= chunk_size {
                    if settings.model.is_parakeet() {
                        live_schedule.submit(written, false);
                        queued = written;
                    } else if range_sender.send((queued, chunk_size as usize)).is_ok() {
                        queued += chunk_size;
                    }
                }
                flush_at = Instant::now();
            }
            collect_live(&result_receiver, &mut recording, &library, &reporter, &mut live_failed)?;
            if level_at.elapsed() >= Duration::from_millis(80) {
                let _ = reporter.sender.send(Event::Levels { duration: recording.duration, microphone: microphone_peak, system: system_peak });
                microphone_peak = 0.0;
                system_peak = 0.0;
                level_at = Instant::now();
            }
            ensure!(written < 2_000_000_000, "Reached the WAV file size limit; this recording has been saved");
        }
        Ok(())
    })();
    reporter.capture.stop();
    for _ in 0..1000 {
        for (source, samples) in audio_receiver.try_iter() {
            match source {
                Source::Microphone => first.push(samples),
                Source::System => second.push(samples),
            }
        }
        if capture_threads.iter().all(JoinHandle::is_finished) { break; }
        thread::sleep(Duration::from_millis(10));
    }
    drop(audio_receiver);
    for handle in capture_threads {
        if handle.is_finished() { let _ = handle.join(); }
    }
    if outcome.is_ok() {
        let count = first.queue.len().max(second.queue.len());
        if count > 0 {
            let microphone = first.take(count, false);
            let system = second.take(count, false);
            audio::write_samples(&mut writer, &capture::mix(&microphone, &system))?;
            written += count as u64;
        }
    }
    writer.finalize().context("Cannot finish writing the recording")?;
    recording.duration = written as f64 / f64::from(SAMPLE_RATE);
    recording.status = if settings.live_transcription && !live_failed { RecordingStatus::Transcribing } else { RecordingStatus::Recorded };
    library.save(&recording)?;
    reporter.recording(&recording);
    let _ = reporter.sender.send(Event::AudioSaved);
    if settings.model.is_parakeet() {
        // Finalization is durable even when no new samples arrived since the last update.
        live_schedule.submit(written, true);
    } else if !live_failed && written > queued {
        let _ = range_sender.send((queued, (written - queued) as usize));
    }
    drop(range_sender);
    if let Some(handle) = live_thread {
        reporter.progress("Audio saved. Finishing queued transcription", 0.0);
        while !handle.is_finished() {
            collect_live(&result_receiver, &mut recording, &library, &reporter, &mut live_failed)?;
            thread::sleep(Duration::from_millis(50));
        }
        let _ = handle.join();
        collect_live(&result_receiver, &mut recording, &library, &reporter, &mut live_failed)?;
    }
    recording.status = if settings.live_transcription && !live_failed { RecordingStatus::Ready } else if recording.segments.is_empty() { RecordingStatus::Recorded } else { RecordingStatus::Partial };
    if reporter.cancel.load(Ordering::Relaxed) {
        recording.status = if recording.segments.is_empty() { RecordingStatus::Recorded } else { RecordingStatus::Partial };
        recording.error = Some("Processing stopped. The recorded audio was saved.".into());
    }
    if capture_error.is_none() { capture_error = error_receiver.try_recv().ok(); }
    if let Some(error) = capture_error {
        recording.error = Some(format!("Audio capture stopped: {error}"));
    }
    if let Err(error) = outcome {
        recording.error = Some(format!("Recording interrupted: {error:#}"));
    }
    library.save(&recording)?;
    store_audio(&mut recording, &settings, &library, &reporter);
    reporter.recording(&recording);
    if let Some(error) = &recording.error { reporter.warning(error); }
    reporter.queue_diarization(&recording, settings, secrets);
    Ok("Recording saved to your library.".into())
}

fn store_audio(recording: &mut Recording, settings: &Settings, library: &Library, reporter: &Reporter) {
    if settings.audio_quality == AudioQuality::Original || recording.duration <= 0.0 || reporter.cancel.load(Ordering::Relaxed) { return; }
    let result = (|| -> Result<()> {
        let original = recording.audio_path.clone();
        let destination = original.with_extension("mp3");
        reporter.progress("Compressing saved audio", 0.0);
        let temporary = audio::compress_wave(&original, settings.audio_quality, &reporter.cancel, |fraction| reporter.progress("Compressing saved audio", fraction))?;
        temporary.persist_noclobber(&destination).map_err(|error| error.error)?;
        let mut compressed = recording.clone();
        compressed.audio_path = destination.clone();
        if let Err(error) = library.save(&compressed) {
            let _ = std::fs::remove_file(&destination);
            return Err(error).context("Cannot save compressed audio metadata");
        }
        *recording = compressed;
        if let Err(error) = std::fs::remove_file(&original) {
            reporter.warning(format!("Compressed audio saved, but the original WAV could not be removed at {}: {error}", original.display()));
        }
        Ok(())
    })();
    if let Err(error) = result {
        reporter.warning(format!("Audio kept as WAV because compression did not finish: {error:#}"));
    }
}

fn resolve_device(devices: &[capture::Device], selected: Option<&str>, label: &str) -> Result<String> {
    let device = match selected {
        Some(id) => devices.iter().find(|device| device.id == id),
        None => devices.first(),
    };
    device.map(|device| device.id.clone()).with_context(|| format!("The selected {label} device is not available. Refresh devices and choose a source in Settings."))
}

fn collect_live(results: &Receiver<LiveResult>, recording: &mut Recording, library: &Library, reporter: &Reporter, failed: &mut bool) -> Result<()> {
    let mut changed = false;
    let mut provisional = None;
    for result in results.try_iter() {
        match result {
            Ok(LiveUpdate::Whisper(transcript)) => { append_transcript(recording, transcript); changed = true; }
            Ok(LiveUpdate::Parakeet(update)) => {
                if !update.committed.is_empty() {
                    let mut words: Vec<_> = recording.segments.iter().flat_map(|segment| segment.words.iter().cloned()).collect();
                    words.extend(update.committed);
                    recording.segments = asr::segments_from_words(words);
                    changed = true;
                }
                provisional = Some(update.provisional);
            }
            Err(error) => {
                *failed = true;
                recording.error = Some(format!("Live transcription stopped: {error}. Audio continues to be saved."));
                reporter.warning(recording.error.clone().unwrap_or_default());
                provisional = Some(Vec::new());
                changed = true;
            }
        }
    }
    if changed {
        library.save(recording)?;
        reporter.recording(recording);
    }
    if let Some(words) = provisional { reporter.live_transcript(&recording.id, words); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speaker_recording(directory: &std::path::Path) -> Recording {
        let mut recording = Recording::new("Speaker test".into(), directory);
        let mut writer = hound::WavWriter::create(&recording.audio_path, audio::wave_spec()).unwrap();
        audio::write_samples(&mut writer, &vec![0.0; SAMPLE_RATE as usize * 2]).unwrap();
        writer.finalize().unwrap();
        recording.duration = 2.0;
        recording.status = RecordingStatus::Ready;
        recording.summary = "Existing notes".into();
        recording.summary_prompt = "Existing instructions".into();
        recording.language = Some("en".into());
        recording.pipeline = PipelineInfo { model: "original-asr".into(), word_timestamps: true, ..Default::default() };
        recording.segments = vec![crate::domain::Segment {
            start: 0.1, end: 1.8, text: "Hello world".into(), speaker: Some("Old speaker".into()),
            words: vec![
                Word { text: "Hello".into(), start: 0.1, end: 0.8, confidence: 0.9, speaker: Some("Old speaker".into()) },
                Word { text: "world".into(), start: 1.1, end: 1.8, confidence: 0.8, speaker: Some("Old speaker".into()) },
            ],
        }];
        recording
    }

    #[cfg(unix)]
    #[test]
    fn speaker_only_processing_preserves_transcription_and_notes_without_asr_models() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        let mut recording = speaker_recording(directory.path());
        recording.error = Some("Audio capture stopped: test device disconnected".into());
        library.save(&recording).unwrap();
        let executable = directory.path().join("fake-python");
        std::fs::write(&executable, r#"#!/usr/bin/env python3
import json,sys
request = json.load(sys.stdin)
assert request['audio'].endswith('.wav')
print(json.dumps({'ok':True,'turns':[{'start':0,'end':1,'speaker':'A'},{'start':1,'end':2,'speaker':'B'}]}))
"#).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Speaker-only processing is available even when automatic identification is disabled.
        let settings = Settings { python_path: executable.to_string_lossy().into_owned(), models_dir: directory.path().join("models"), diarization_enabled: false, ..Default::default() };
        assert!(!settings.model_path().exists());
        let (sender, events) = unbounded();
        let reporter = Reporter { sender, cancel: Arc::new(AtomicBool::new(false)), capture: Control::new() };
        identify_speakers(recording.clone(), settings.clone(), Secrets::default(), library.clone(), reporter).unwrap();
        let saved = library.get(&recording.id).unwrap();
        let before: Vec<_> = recording.segments.iter().flat_map(|segment| &segment.words).map(|word| (&word.text, word.start, word.end, word.confidence)).collect();
        let after: Vec<_> = saved.segments.iter().flat_map(|segment| &segment.words).map(|word| (&word.text, word.start, word.end, word.confidence)).collect();
        assert_eq!(before, after);
        assert_eq!(saved.segments.len(), 2);
        assert_eq!(saved.segments[0].speaker.as_deref(), Some("A"));
        assert_eq!(saved.segments[1].speaker.as_deref(), Some("B"));
        assert_eq!(saved.summary, recording.summary);
        assert_eq!(saved.summary_prompt, recording.summary_prompt);
        assert_eq!(saved.language, recording.language);
        assert_eq!(saved.pipeline.model, recording.pipeline.model);
        assert!(saved.pipeline.diarization);
        assert_eq!(saved.error, recording.error);
        assert!(events.try_iter().any(|event| matches!(event, Event::Recording(_))));
        assert_eq!(library.search("Hello").unwrap().len(), 1);
        let mut retry = saved;
        retry.error = Some("Transcript saved, but speaker identification failed: previous attempt".into());
        let (sender, _events) = unbounded();
        let reporter = Reporter { sender, cancel: Arc::new(AtomicBool::new(false)), capture: Control::new() };
        identify_speakers(retry, settings, Secrets::default(), library.clone(), reporter).unwrap();
        assert!(library.get(&recording.id).unwrap().error.is_none());
    }

    #[test]
    fn failed_or_cancelled_speaker_processing_keeps_existing_text_notes_and_assignments() {
        for cancelled in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let library = Library::open(directory.path().join("library.db")).unwrap();
            let mut recording = speaker_recording(directory.path());
            recording.pipeline.diarization = true;
            library.save(&recording).unwrap();
            let settings = Settings { python_path: directory.path().join("missing-python").to_string_lossy().into_owned(), models_dir: directory.path().join("models"), ..Default::default() };
            let (sender, _events) = unbounded();
            let reporter = Reporter { sender, cancel: Arc::new(AtomicBool::new(cancelled)), capture: Control::new() };
            assert!(identify_speakers(recording.clone(), settings, Secrets::default(), library.clone(), reporter).is_err());
            let saved = library.get(&recording.id).unwrap();
            assert_eq!(saved.segments, recording.segments);
            assert_eq!(saved.summary, recording.summary);
            assert_eq!(saved.summary_prompt, recording.summary_prompt);
            assert_eq!(saved.status, RecordingStatus::Ready);
            assert!(saved.pipeline.diarization && saved.error.is_some());
            assert!(saved.audio_path.exists());
        }
    }

    #[test]
    fn automatic_speaker_work_is_deferred_only_for_nonempty_uncancelled_transcripts() {
        let directory = tempfile::tempdir().unwrap();
        let mut recording = speaker_recording(directory.path());
        let (sender, events) = unbounded();
        let reporter = Reporter { sender, cancel: Arc::new(AtomicBool::new(false)), capture: Control::new() };
        let settings = Settings { diarization_enabled: true, ..Default::default() };
        reporter.queue_diarization(&recording, settings.clone(), Secrets::default());
        assert!(matches!(events.recv().unwrap(), Event::QueueDiarization { recording_id, .. } if recording_id == recording.id));
        reporter.cancel.store(true, Ordering::Relaxed);
        reporter.queue_diarization(&recording, settings.clone(), Secrets::default());
        reporter.cancel.store(false, Ordering::Relaxed);
        reporter.queue_diarization(&recording, Settings { diarization_enabled: false, ..settings.clone() }, Secrets::default());
        recording.segments.clear();
        reporter.queue_diarization(&recording, settings, Secrets::default());
        assert!(events.is_empty());
    }

    #[test]
    fn imports_follow_audio_quality_and_saved_compressed_audio_can_be_prepared() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        let mut writer = hound::WavWriter::create(&source, audio::wave_spec()).unwrap();
        audio::write_samples(&mut writer, &vec![0.1; SAMPLE_RATE as usize + 173]).unwrap();
        writer.finalize().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        for quality in AudioQuality::ALL {
            let settings = Settings { audio_quality: quality, recordings_dir: directory.path().join(format!("{quality:?}")), models_dir: directory.path().join("models"), ..Default::default() };
            let (sender, events) = unbounded();
            let reporter = Reporter { sender, cancel: Arc::new(AtomicBool::new(false)), capture: Control::new() };
            import(source.clone(), settings, library.clone(), reporter).unwrap();
            let recording = events.try_iter().filter_map(|event| if let Event::Recording(recording) = event { Some(*recording) } else { None }).last().unwrap();
            assert_eq!(library.get(&recording.id).unwrap().audio_path, recording.audio_path);
            assert_eq!(recording.audio_path.extension().unwrap(), if quality == AudioQuality::Original { "wav" } else { "mp3" });
            assert!(recording.audio_path.exists());
            if quality != AudioQuality::Original { assert!(!recording.audio_path.with_extension("wav").exists()); }
            let prepared = audio::PreparedAudio::open(&recording.audio_path, &AtomicBool::new(false), |_| {}).unwrap();
            assert_eq!(audio::open_wave(prepared.path()).unwrap().duration(), SAMPLE_RATE + 173);
            assert_eq!(recording.original_path.as_deref(), Some(source.as_path()));
        }
        assert!(source.exists());
    }

    #[test]
    fn failed_or_cancelled_storage_compression_keeps_the_library_wav() {
        let directory = tempfile::tempdir().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        let mut recording = Recording::new("Storage failure".into(), directory.path());
        let mut writer = hound::WavWriter::create(&recording.audio_path, audio::wave_spec()).unwrap();
        audio::write_samples(&mut writer, &vec![0.1; SAMPLE_RATE as usize]).unwrap();
        writer.finalize().unwrap();
        recording.duration = 1.0;
        library.save(&recording).unwrap();
        let original = recording.audio_path.clone();
        let destination = original.with_extension("mp3");
        std::fs::write(&destination, b"existing audio").unwrap();
        let settings = Settings { audio_quality: AudioQuality::Compact, ..Default::default() };
        let (sender, events) = unbounded();
        let reporter = Reporter { sender, cancel: Arc::new(AtomicBool::new(false)), capture: Control::new() };
        store_audio(&mut recording, &settings, &library, &reporter);
        assert!(events.try_iter().any(|event| matches!(event, Event::Warning(_))));
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing audio");
        assert_eq!(recording.audio_path, original);
        assert_eq!(library.get(&recording.id).unwrap().audio_path, original);
        assert_eq!(audio::open_wave(&original).unwrap().duration(), SAMPLE_RATE);
        reporter.cancel.store(true, Ordering::Relaxed);
        store_audio(&mut recording, &settings, &library, &reporter);
        assert!(events.is_empty());
        assert_eq!(recording.audio_path, original);
    }

    #[test]
    fn provisional_predictions_never_enter_the_library_and_failure_keeps_committed_words() {
        let directory = tempfile::tempdir().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        let mut recording = Recording::new("Live test".into(), directory.path());
        let (sender, results) = unbounded();
        let (events, receiver) = unbounded();
        let reporter = Reporter { sender: events, cancel: Arc::new(AtomicBool::new(false)), capture: Control::new() };
        let word = |text: &str, start, end| Word { text: text.into(), start, end, confidence: 0.0, speaker: None };
        let stable = word("committed", 0.1, 0.5);
        sender.send(Ok(LiveUpdate::Parakeet(live::Update {
            committed: vec![stable.clone()], provisional: vec![word("temporary", 1.0, 1.5)],
        }))).unwrap();
        let mut failed = false;
        collect_live(&results, &mut recording, &library, &reporter, &mut failed).unwrap();
        assert_eq!(library.get(&recording.id).unwrap().transcript(), "committed");
        assert!(library.search("temporary").unwrap().is_empty());
        assert!(!crate::domain::export(&recording, crate::domain::ExportFormat::Json).unwrap().contains("temporary"));
        assert!(matches!(receiver.try_iter().last().unwrap(), Event::LiveTranscript { provisional, .. } if provisional[0].text == "temporary"));

        sender.send(Ok(LiveUpdate::Parakeet(live::Update { committed: Vec::new(), provisional: vec![word("revised", 1.1, 1.6)] }))).unwrap();
        collect_live(&results, &mut recording, &library, &reporter, &mut failed).unwrap();
        assert_eq!(receiver.len(), 1, "provisional-only changes should not publish saved recording updates");
        assert!(matches!(receiver.recv().unwrap(), Event::LiveTranscript { provisional, .. } if provisional[0].text == "revised"));

        sender.send(Err("no timestamps".into())).unwrap();
        collect_live(&results, &mut recording, &library, &reporter, &mut failed).unwrap();
        assert!(failed);
        let saved = library.get(&recording.id).unwrap();
        assert_eq!(saved.segments[0].words, vec![stable]);
        assert!(saved.error.unwrap().contains("no timestamps"));
        assert!(matches!(receiver.try_iter().last().unwrap(), Event::LiveTranscript { provisional, .. } if provisional.is_empty()));
    }

    #[test]
    fn pipeline_metadata_distinguishes_native_timestamps_from_dtw() {
        let settings = Settings { vad_enabled: true, alignment_enabled: false, ..Default::default() };
        let info = pipeline_info(&settings);
        assert_eq!(info.model, "parakeet-tdt-0.6b-v3");
        assert!(info.word_timestamps);
        assert!(info.vad && !info.dtw && !info.diarization);
        let settings = Settings { model: crate::domain::WhisperModel::Tiny.into(), alignment_enabled: true, ..settings };
        let info = pipeline_info(&settings);
        assert!(info.dtw && info.vad && info.word_timestamps);
    }

    #[test]
    fn mixer_discards_late_samples_instead_of_shifting_time() {
        let mut source = SourceBuffer::new(true);
        source.push(vec![0.2; 3]);
        assert_eq!(source.take(5, true), vec![0.2, 0.2, 0.2, 0.0, 0.0]);
        source.push(vec![0.5, 0.6, 0.7, 0.8]);
        assert_eq!(source.take(2, false), vec![0.7, 0.8]);
    }

    #[test]
    fn job_reports_failure_and_completes_without_panicking() {
        let job = Job::spawn(JobKind::Import, |_| anyhow::bail!("test failure"));
        let event = job.events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(event, Event::Finished(Err(message)) if message.contains("test failure")));
    }

    #[test]
    fn selecting_a_missing_device_never_uses_a_different_source() {
        let devices = vec![capture::Device { id: "mic".into(), label: "Microphone".into(), is_monitor: false }];
        assert_eq!(resolve_device(&devices, None, "microphone").unwrap(), "mic");
        assert!(resolve_device(&devices, Some("missing"), "microphone").is_err());
    }
}
