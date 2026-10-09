#![cfg(target_os = "linux")]

use std::{path::PathBuf, process::{Child, Command, Stdio}, thread, time::{Duration, Instant}};

use parakeetx::{
    domain::{Recording, RecordingStatus, WhisperModel},
    engine::{self, Event, Job, JobKind},
    settings::{Secrets, Settings},
    storage::Library,
};

struct VirtualSink(String);

impl Drop for VirtualSink {
    fn drop(&mut self) {
        let _ = Command::new("pactl").args(["unload-module", &self.0]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

struct Playback(Child);

impl Drop for Playback {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

#[test]
#[ignore = "requires PulseAudio/PipeWire, pactl, paplay, and the native inference fixtures"]
fn virtual_system_capture_pause_resume_live_transcript_and_final_flush() {
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".test-cache");
    let sink = format!("parakeetx_test_{}", std::process::id());
    let output = Command::new("pactl").args(["load-module", "module-null-sink", &format!("sink_name={sink}"), "rate=48000", "channels=2"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let _module = VirtualSink(String::from_utf8(output.stdout).unwrap().trim().into());
    let directory = tempfile::tempdir().unwrap();
    let library = Library::open(directory.path().join("library.sqlite3")).unwrap();
    let settings = Settings {
        models_dir: cache.clone(), recordings_dir: directory.path().join("recordings"),
        model: WhisperModel::Tiny.into(), vad_enabled: true, diarization_enabled: false, threads: 2, microphone_enabled: false,
        system_enabled: true, system_device: Some(format!("{sink}.monitor")),
        use_gpu: false, chunk_seconds: 5, ..Default::default()
    };
    let worker_library = library.clone();
    let job = Job::spawn(JobKind::Recording, move |reporter| engine::record("Virtual loopback test".into(), settings, Secrets::default(), worker_library, reporter));
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut current = None::<Recording>;
    let mut duration = 0.0;
    while duration < 0.3 {
        assert!(Instant::now() < deadline, "Capture startup timed out");
        consume(&job, &mut current, &mut duration);
    }
    job.capture.set_paused(true);
    thread::sleep(Duration::from_millis(350));
    while let Ok(event) = job.events.try_recv() { apply(event, &mut current, &mut duration); }
    let paused_at = duration;
    thread::sleep(Duration::from_millis(600));
    while let Ok(event) = job.events.try_recv() { apply(event, &mut current, &mut duration); }
    assert!((duration - paused_at).abs() < 0.1, "Duration advanced while paused: {paused_at} -> {duration}");
    job.capture.set_paused(false);
    let mut playback = Playback(Command::new("paplay").arg(format!("--device={sink}")).arg(cache.join("jfk.wav")).spawn().unwrap());
    let mut live_text_seen = false;
    while playback.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "Playback or capture timed out");
        consume(&job, &mut current, &mut duration);
        live_text_seen |= current.as_ref().is_some_and(|recording| !recording.segments.is_empty());
    }
    job.finish_recording();
    loop {
        match job.events.recv_timeout(Duration::from_secs(20)).unwrap() {
            Event::Finished(result) => { result.unwrap(); break; }
            event => apply(event, &mut current, &mut duration),
        }
    }
    assert!(live_text_seen, "Transcription did not appear during recording");
    let result = current.unwrap();
    assert_eq!(result.status, RecordingStatus::Ready);
    assert!(result.duration > 10.0);
    assert!(result.transcript().to_lowercase().contains("country"), "{}", result.transcript());
    let stored = library.get(&result.id).unwrap();
    assert_eq!(stored.segments.len(), result.segments.len());
    assert!(stored.pipeline.vad && stored.pipeline.dtw);
    let reader = parakeetx::audio::open_wave(&result.audio_path).unwrap();
    assert!((f64::from(reader.duration()) / 16_000.0 - result.duration).abs() < 0.001);
}

fn consume(job: &Job, current: &mut Option<Recording>, duration: &mut f64) {
    if let Ok(event) = job.events.recv_timeout(Duration::from_millis(100)) { apply(event, current, duration); }
}

fn apply(event: Event, current: &mut Option<Recording>, duration: &mut f64) {
    match event {
        Event::Recording(recording) => *current = Some(*recording),
        Event::Levels { duration: elapsed, .. } => *duration = elapsed,
        Event::Warning(warning) => panic!("unexpected warning: {warning}"),
        Event::Finished(result) => panic!("Recording ended too early: {result:?}"),
        _ => {}
    }
}
