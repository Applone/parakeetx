#![cfg(target_os = "macos")]

use std::{process::Command, thread, time::{Duration, Instant}};

use parakeetx::{
    audio, capture,
    domain::{Recording, RecordingStatus},
    engine::{self, Event, Job, JobKind},
    settings::{Secrets, Settings},
    storage::Library,
};

fn consume(job: &Job, current: &mut Option<Recording>, elapsed: &mut f64) {
    if let Ok(event) = job.events.recv_timeout(Duration::from_millis(100)) {
        match event {
            Event::Recording(recording) => *current = Some(*recording),
            Event::Levels { duration, .. } => *elapsed = duration,
            Event::Warning(message) => panic!("Capture warning: {message}"),
            Event::Finished(result) => panic!("Capture stopped too early: {result:?}"),
            _ => {}
        }
    }
}

#[test]
#[ignore = "requires macOS 13+, an active display, afplay, and Screen & System Audio Recording permission"]
fn native_system_audio_preserves_silence_pause_resume_and_final_audio() {
    let devices = capture::devices();
    let source = devices.system.iter().find(|device| device.id == "screencapturekit:system-audio")
        .expect("The native system audio source should be listed");
    capture::check_permissions(Some(&source.id)).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let settings = Settings {
        models_dir: directory.path().join("models"), recordings_dir: directory.path().join("recordings"),
        microphone_enabled: false, system_enabled: true, system_device: Some(source.id.clone()),
        live_transcription: false, diarization_enabled: false, ..Default::default()
    };
    let library = Library::open(directory.path().join("library.sqlite3")).unwrap();
    let worker_library = library.clone();
    let job = Job::spawn(JobKind::Recording, move |reporter| {
        engine::record("ScreenCaptureKit test".into(), settings, Secrets::default(), worker_library, reporter)
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut current = None;
    let mut elapsed = 0.0;
    // Recording must advance even when no applications are producing sound.
    while elapsed < 1.0 {
        assert!(Instant::now() < deadline, "Native capture startup timed out");
        consume(&job, &mut current, &mut elapsed);
    }
    job.capture.set_paused(true);
    // Let the pre-pause delivery window and UI events drain.
    let settling = Instant::now() + Duration::from_millis(350);
    while Instant::now() < settling { consume(&job, &mut current, &mut elapsed); }
    let paused_at = elapsed;
    let paused = Instant::now() + Duration::from_millis(500);
    while Instant::now() < paused { consume(&job, &mut current, &mut elapsed); }
    assert!((elapsed - paused_at).abs() < 0.05, "Recording clock advanced during pause");
    job.capture.set_paused(false);

    let tone = directory.path().join("tone.wav");
    let mut writer = hound::WavWriter::create(&tone, audio::wave_spec()).unwrap();
    let samples: Vec<_> = (0..32_000).map(|index| (index as f32 * 440.0 * std::f32::consts::TAU / 16_000.0).sin() * 0.1).collect();
    audio::write_samples(&mut writer, &samples).unwrap();
    writer.finalize().unwrap();
    // Playback is in a separate process because capture excludes parakeetx's own audio.
    let mut playback = Command::new("afplay").arg(&tone).spawn().unwrap();
    while playback.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = playback.kill();
            let _ = playback.wait();
            panic!("Audio playback timed out");
        }
        consume(&job, &mut current, &mut elapsed);
    }
    thread::sleep(Duration::from_millis(150));
    job.finish_recording();
    loop {
        match job.events.recv_timeout(Duration::from_secs(15)).unwrap() {
            Event::Recording(recording) => current = Some(*recording),
            Event::Warning(message) => panic!("Capture warning: {message}"),
            Event::Finished(result) => { result.unwrap(); break; }
            _ => {}
        }
    }
    let recording = current.unwrap();
    assert_eq!(recording.status, RecordingStatus::Recorded);
    assert!(recording.error.is_none(), "{:?}", recording.error);
    assert!(recording.duration >= 3.0);
    let reader = audio::open_wave(&recording.audio_path).unwrap();
    assert!((f64::from(reader.duration()) / 16_000.0 - recording.duration).abs() < 0.001);
    let recorded = audio::read_range(&recording.audio_path, 0, reader.duration() as usize).unwrap();
    assert!(capture::peak(&recorded) > 0.01, "System playback was not captured; check output volume and permission");
    assert_eq!(library.get(&recording.id).unwrap().duration, recording.duration);
}
