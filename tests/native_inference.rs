use std::{path::PathBuf, sync::{Arc, atomic::AtomicBool}, time::Duration};

use parakeetx::{
    asr::{TranscribeOptions, Transcriber},
    audio,
    domain::{RecordingStatus, TranscriptionModel, WhisperModel},
    engine::{self, Event, Job, JobKind},
    settings::{Secrets, Settings},
    storage::Library,
    vad::{Vad, VadOptions},
};

fn cache() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".test-cache") }

#[test]
#[ignore = "requires .test-cache/parakeet-tdt-0.6b-v3-int8/ and jfk.wav"]
fn real_parakeet_native_timestamps_and_library_pipeline() {
    let cache = cache();
    let model = TranscriptionModel::default();
    let audio_path = cache.join("jfk.wav");
    let samples = audio::read_range(&audio_path, 0, 16_000 * 30).unwrap();
    let mut transcriber = Transcriber::load_with_threads(&cache.join(model.filename()), model, false, false, 2).unwrap();
    assert!(transcriber.alignment_enabled());
    let cancel = Arc::new(AtomicBool::new(false));
    let options = TranscribeOptions { offset: 10.0, ..Default::default() };
    let transcript = transcriber.run(&samples, &options, &cancel, |_| {}).unwrap();
    let text = transcript.segments.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>().join(" ").to_lowercase();
    assert!(text.contains("country"), "Unexpected Parakeet transcription: {text}");
    assert!(transcript.language.is_none());
    let words: Vec<_> = transcript.segments.iter().flat_map(|segment| &segment.words).collect();
    assert!(words.len() > 10);
    assert!(words.iter().all(|word| word.start >= 10.0 && word.end >= word.start && word.end <= 10.0 + samples.len() as f64 / 16_000.0));

    let temp = tempfile::tempdir().unwrap();
    let settings = Settings {
        models_dir: cache, recordings_dir: temp.path().join("recordings"),
        // A Parakeet pipeline must bypass the legacy stages even with their switches enabled.
        vad_enabled: true, alignment_enabled: true, diarization_enabled: false,
        threads: 2, use_gpu: false, ..Default::default()
    };
    settings.prepare_directories().unwrap();
    let library = Library::open(temp.path().join("library.db")).unwrap();
    let import_settings = settings.clone();
    let import_library = library.clone();
    wait(Job::spawn(JobKind::Import, move |reporter| engine::import(audio_path, import_settings, import_library, reporter)));
    let entry = library.search("").unwrap().remove(0);
    let recording = library.get(&entry.id).unwrap();
    let transcribe_library = library.clone();
    wait(Job::spawn(JobKind::Transcription, move |reporter| engine::transcribe(recording, settings, Secrets::default(), transcribe_library, reporter)));
    let result = library.get(&entry.id).unwrap();
    assert_eq!(result.status, RecordingStatus::Ready);
    assert_eq!(result.pipeline.model, "parakeet-tdt-0.6b-v3");
    assert!(result.pipeline.word_timestamps);
    assert!(!result.pipeline.vad && !result.pipeline.dtw);
    assert!(!library.search("country").unwrap().is_empty());
}

#[test]
#[ignore = "requires .test-cache/ggml-tiny.bin, silero_vad.onnx, and jfk.wav"]
fn real_silero_whisper_dtw_and_library_pipeline() {
    let cache = cache();
    let audio_path = cache.join("jfk.wav");
    let samples = audio::read_range(&audio_path, 0, 16_000 * 30).unwrap();
    let mut detector = Vad::load(&cache.join("silero_vad.onnx")).unwrap();
    let speech = detector.segments(&samples, VadOptions::default()).unwrap();
    assert!(!speech.is_empty(), "Silero must detect speech in the JFK sample");
    assert!(detector.segments(&vec![0.0; 32_000], VadOptions::default()).unwrap().is_empty());
    let mut transcriber = Transcriber::load(&cache.join("ggml-tiny.bin"), WhisperModel::Tiny, false, true).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let transcript = transcriber.run(&samples, &TranscribeOptions { threads: 2, ..Default::default() }, &cancel, |_| {}).unwrap();
    let text = transcript.segments.iter().map(|segment| segment.text.clone()).collect::<Vec<_>>().join(" ").to_lowercase();
    assert!(text.contains("country"), "Unexpected real transcription: {text}");
    assert_eq!(transcript.language.as_deref(), Some("en"));
    assert!(transcript.segments.iter().flat_map(|segment| &segment.words).count() > 10);
    for word in transcript.segments.iter().flat_map(|segment| &segment.words) {
        assert!(word.start >= 0.0 && word.end >= word.start && word.end <= samples.len() as f64 / 16_000.0);
        assert!(!word.text.contains('\u{fffd}'));
    }
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings { models_dir: cache, recordings_dir: temp.path().join("recordings"), model: WhisperModel::Tiny.into(), vad_enabled: true, diarization_enabled: false, threads: 2, use_gpu: false, ..Default::default() };
    settings.prepare_directories().unwrap();
    let library = Library::open(temp.path().join("library.db")).unwrap();
    let import_settings = settings.clone();
    let import_library = library.clone();
    let job = Job::spawn(JobKind::Import, move |reporter| engine::import(audio_path, import_settings, import_library, reporter));
    wait(job);
    let entry = library.search("").unwrap().remove(0);
    let recording = library.get(&entry.id).unwrap();
    let transcribe_library = library.clone();
    let job = Job::spawn(JobKind::Transcription, move |reporter| engine::transcribe(recording, settings, Secrets::default(), transcribe_library, reporter));
    wait(job);
    let result = library.get(&entry.id).unwrap();
    assert_eq!(result.status, RecordingStatus::Ready);
    assert!(result.pipeline.vad && result.pipeline.dtw);
    assert!(!library.search("country").unwrap().is_empty());
}

fn wait(job: Job) {
    loop {
        match job.events.recv_timeout(Duration::from_secs(180)).unwrap() {
            Event::Finished(result) => { result.unwrap(); break; }
            Event::Warning(warning) => panic!("unexpected warning: {warning}"),
            _ => {}
        }
    }
}
