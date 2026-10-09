use super::*;

fn app() -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let paths = AppPaths { config: directory.path().join("config"), data: directory.path().join("data") };
    let lock = std::sync::Arc::new(paths.lock().unwrap());
    let settings = Settings::load(&paths).unwrap();
    let library = Library::open(paths.database()).unwrap();
    let (mut app, _) = App::boot();
    let _ = app.update(Message::Loaded(Ok(Loaded {
        resources: Resources { paths, settings, library, secrets: Secrets::default(), _lock: lock },
        items: Vec::new(), devices: Devices::default(), recovered: 0,
    })));
    (app, directory)
}

#[test]
fn every_page_and_recording_tab_renders_without_external_services() {
    let (mut app, _directory) = app();
    for page in [Page::Record, Page::Import, Page::Library, Page::Settings] {
        let _ = app.update(Message::Navigate(page));
        let _ = app.view();
    }
    for model in TranscriptionModel::ALL {
        let _ = app.update(Message::Model(model));
        for tab in [SettingsTab::Transcription, SettingsTab::Speakers] {
            let _ = app.update(Message::SettingsTab(tab));
            let _ = app.view();
        }
    }
    let recording = Recording::new("Test lecture".into(), &app.resources.as_ref().unwrap().settings.recordings_dir);
    app.show_recording(recording.clone());
    app.live = Some(recording);
    for tab in [DetailTab::Transcript, DetailTab::Words, DetailTab::Notes] {
        let _ = app.update(Message::Tab(tab));
        app.page = Page::Library;
        let _ = app.view();
    }
    app.page = Page::Record;
    let _ = app.view();
}

#[test]
fn search_and_selection_ignore_out_of_order_results() {
    let (mut app, _directory) = app();
    let _ = app.update(Message::Search("latest".into()));
    let stale = crate::domain::LibraryItem { id: "old".into(), title: "Old search".into(), created_at: String::new(), duration: 0.0, status: String::new(), preview: String::new() };
    let _ = app.update(Message::SearchResults("stale".into(), Ok(vec![stale])));
    assert!(app.items.is_empty());
    app.requested_id = Some("new".into());
    let recording = Recording::new("Old selection".into(), &app.resources.as_ref().unwrap().settings.recordings_dir);
    let _ = app.update(Message::Selected("old".into(), Ok(recording)));
    assert!(app.selected.is_none());
}

#[test]
fn draft_validates_numbers_and_retains_secret_fields_outside_settings() {
    let (mut app, _directory) = app();
    let _ = app.update(Message::Setting(Field::Threads, "invalid".into()));
    assert!(app.draft.build().is_err());
    let _ = app.update(Message::Setting(Field::Threads, "2".into()));
    let _ = app.update(Message::Setting(Field::ApiKey, "private-key".into()));
    let _ = app.update(Message::Toggle(Toggle::Microphone, false));
    let _ = app.update(Message::Toggle(Toggle::System, true));
    let settings = app.draft.build().unwrap();
    assert_eq!(settings.threads, 2);
    assert!(!settings.microphone_enabled && settings.system_enabled);
    assert!(!serde_json::to_string(&settings).unwrap().contains("private-key"));
    assert_eq!(app.draft.secrets.api_key, "private-key");
}

#[test]
fn worker_events_update_live_transcript_and_keep_finished_recordings() {
    let (mut app, _directory) = app();
    let recording = Recording::new("Finished recording".into(), &app.resources.as_ref().unwrap().settings.recordings_dir);
    let id = recording.id.clone();
    app.start_job(JobKind::Recording, "test", move |reporter| { reporter.recording(&recording); Ok("Saved".into()) });
    while !app.job.as_ref().unwrap().is_finished() { std::thread::sleep(Duration::from_millis(5)); }
    let _ = app.update(Message::Tick);
    assert!(app.job.is_none());
    assert_eq!(app.live.as_ref().unwrap().id, id);
    let _ = app.update(Message::OpenLive);
    assert_eq!(app.page, Page::Library);
    assert_eq!(app.selected.as_ref().unwrap().id, id);
}

#[test]
fn provisional_events_replace_the_tail_ignore_other_recordings_and_clear_on_finish() {
    let (mut app, _directory) = app();
    let recording = Recording::new("Live recording".into(), &app.resources.as_ref().unwrap().settings.recordings_dir);
    let id = recording.id.clone();
    let (commands, input) = crossbeam_channel::unbounded::<(String, Vec<Word>)>();
    let (ready, acknowledgements) = crossbeam_channel::unbounded();
    app.start_job(JobKind::Recording, "test", move |reporter| {
        reporter.recording(&recording);
        ready.send(()).unwrap();
        for (id, words) in input {
            reporter.live_transcript(&id, words);
            ready.send(()).unwrap();
        }
        Ok("Finished".into())
    });
    acknowledgements.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = app.update(Message::Tick);
    let word = |text: &str| Word { text: text.into(), start: 0.2, end: 0.8, confidence: 0.0, speaker: None };
    for (recording_id, text, expected) in [
        (id.as_str(), "first", "first"), ("another-recording", "stale", "first"), (id.as_str(), "revised", "revised"),
    ] {
        commands.send((recording_id.into(), vec![word(text)])).unwrap();
        acknowledgements.recv_timeout(Duration::from_secs(2)).unwrap();
        let _ = app.update(Message::Tick);
        assert_eq!(app.live_provisional, vec![word(expected)]);
        assert!(app.live.as_ref().unwrap().segments.is_empty());
        assert!(app.live.as_ref().unwrap().transcript().is_empty());
        app.page = Page::Record;
        let _ = app.view();
    }
    commands.send((id.clone(), Vec::new())).unwrap();
    acknowledgements.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = app.update(Message::Tick);
    assert!(app.live_provisional.is_empty());
    commands.send((id, vec![word("last")])).unwrap();
    acknowledgements.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = app.update(Message::Tick);
    assert!(!app.live_provisional.is_empty());
    drop(commands);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !app.job.as_ref().unwrap().is_finished() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = app.update(Message::Tick);
    assert!(app.live_provisional.is_empty() && app.job.is_none());
}

#[test]
fn cancelling_a_job_discards_provisional_text_and_whisper_settings_survive_model_switches() {
    let (mut app, _directory) = app();
    let recording = Recording::new("Cancelled recording".into(), &app.resources.as_ref().unwrap().settings.recordings_dir);
    let (resume, wait) = crossbeam_channel::bounded(0);
    let (ready, acknowledgements) = crossbeam_channel::unbounded();
    app.start_job(JobKind::Recording, "test", move |reporter| {
        let word = Word { text: "pending".into(), start: 0.1, end: 0.5, confidence: 0.0, speaker: None };
        reporter.recording(&recording);
        reporter.live_transcript(&recording.id, vec![word.clone()]);
        ready.send(()).unwrap();
        wait.recv().unwrap();
        // Simulate a prediction already in flight when cancellation was requested.
        reporter.live_transcript(&recording.id, vec![word]);
        ready.send(()).unwrap();
        wait.recv().unwrap();
        Ok("Stopped".into())
    });
    acknowledgements.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = app.update(Message::Tick);
    assert_eq!(app.live_provisional.len(), 1);
    let _ = app.update(Message::Cancel);
    assert!(app.live_provisional.is_empty());
    resume.send(()).unwrap();
    acknowledgements.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = app.update(Message::Tick);
    assert!(app.live_provisional.is_empty());
    resume.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !app.job.as_ref().unwrap().is_finished() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = app.update(Message::Tick);
    let _ = app.update(Message::Setting(Field::ChunkSeconds, "9".into()));
    let _ = app.update(Message::Model(TranscriptionModel::default()));
    assert_eq!(app.draft.build().unwrap().chunk_seconds, 9);
    let _ = app.view();
    let _ = app.update(Message::Model(crate::domain::WhisperModel::Tiny.into()));
    assert_eq!(app.draft.build().unwrap().chunk_seconds, 9);
    let _ = app.view();
}
