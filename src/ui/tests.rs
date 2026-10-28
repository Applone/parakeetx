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
    // Rendering/state tests supply detection results rather than invoking user Python.
    app.speaker_detection = Some(Detection { uv: None, uv_status: "uv not installed".into(), dependencies: Ok("test Python".into()), managed_python: None });
    (app, directory)
}

#[test]
fn speaker_setup_selects_only_the_draft_and_requires_explicit_settings_save() {
    let (mut app, directory) = app();
    let resources = app.resources.as_ref().unwrap();
    resources.settings.save(&resources.paths).unwrap();
    let settings_path = resources.paths.config.join("settings.json");
    let persisted = std::fs::read(&settings_path).unwrap();
    let original = resources.settings.python_path.clone();
    let interpreter = directory.path().join("managed/bin/python");
    let expected = interpreter.to_string_lossy().into_owned();
    let revision = app.speaker_revision;
    app.start_job(JobKind::PythonSetup, "test", move |reporter| {
        reporter.speaker_setup(revision, diarization_setup::SetupResult { interpreter, description: "validated CPU setup".into() });
        Ok("Ready".into())
    });
    while !app.job.as_ref().unwrap().is_finished() { std::thread::sleep(Duration::from_millis(5)); }
    let _ = app.update(Message::Tick);
    assert_eq!(app.draft.settings.python_path, expected);
    assert_eq!(app.resources.as_ref().unwrap().settings.python_path, original);
    assert_eq!(std::fs::read(settings_path).unwrap(), persisted);
    assert!(app.speaker_detection.as_ref().unwrap().dependencies.is_ok());
    assert!(app.speaker_model.is_none());
    let _ = app.update(Message::SettingsTab(SettingsTab::Speakers));
    let _ = app.view();
    app.speaker_detection.as_mut().unwrap().uv = Some(directory.path().join("uv"));
    let _ = app.update(Message::RequestSpeakerSetup);
    assert!(app.confirm_speaker_setup);
    let _ = app.view();
    let _ = app.update(Message::DismissSpeakerSetup);
    assert!(!app.confirm_speaker_setup);
}

#[test]
fn speaker_results_ignore_old_settings_and_model_verification_is_invalidated_by_edits() {
    let (mut app, _directory) = app();
    let stale = app.speaker_revision;
    let _ = app.update(Message::Setting(Field::DiarizationModel, "org/new-model".into()));
    app.start_job(JobKind::SpeakerModelCheck, "test", move |reporter| {
        reporter.speaker_model_ready(stale);
        reporter.speaker_detection(stale, Detection { uv: None, uv_status: "stale".into(), dependencies: Err("stale".into()), managed_python: None });
        Ok("Ready".into())
    });
    while !app.job.as_ref().unwrap().is_finished() { std::thread::sleep(Duration::from_millis(5)); }
    let _ = app.update(Message::Tick);
    assert!(app.speaker_model.is_none());
    assert_ne!(app.speaker_detection.as_ref().unwrap().uv_status, "stale");
    let revision = app.speaker_revision;
    app.start_job(JobKind::SpeakerModelCheck, "test", move |reporter| { reporter.speaker_model_ready(revision); Ok("Ready".into()) });
    while !app.job.as_ref().unwrap().is_finished() { std::thread::sleep(Duration::from_millis(5)); }
    let _ = app.update(Message::Tick);
    assert!(app.speaker_model.as_ref().unwrap().is_ok());
    let _ = app.update(Message::Setting(Field::HfToken, "secret-token".into()));
    assert!(app.speaker_model.is_none());
    assert!(app.speaker_detection.is_some());
    let _ = app.update(Message::Setting(Field::Python, "custom-python".into()));
    assert!(app.speaker_detection.is_none());
}

#[test]
fn cancelling_setup_or_receiving_a_stale_setup_result_preserves_the_interpreter() {
    let (mut app, directory) = app();
    let original = app.draft.settings.python_path.clone();
    for stale in [false, true] {
        let revision = if stale { app.speaker_revision.wrapping_sub(1) } else { app.speaker_revision };
        let interpreter = directory.path().join("unused-python");
        app.start_job(JobKind::PythonSetup, "test", move |reporter| {
            reporter.speaker_setup(revision, diarization_setup::SetupResult { interpreter, description: "ready".into() });
            Ok("Ready".into())
        });
        while !app.job.as_ref().unwrap().is_finished() { std::thread::sleep(Duration::from_millis(5)); }
        if !stale { app.job.as_ref().unwrap().cancel(); }
        let _ = app.update(Message::Tick);
        assert_eq!(app.draft.settings.python_path, original);
    }
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
        for tab in [SettingsTab::General, SettingsTab::Audio, SettingsTab::Transcription, SettingsTab::Speakers, SettingsTab::Notes] {
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
    let _ = app.update(Message::AudioQuality(AudioQuality::Balanced));
    let settings = app.draft.build().unwrap();
    assert_eq!(settings.threads, 2);
    assert_eq!(settings.audio_quality, AudioQuality::Balanced);
    assert!(!settings.microphone_enabled && settings.system_enabled);
    assert!(!serde_json::to_string(&settings).unwrap().contains("private-key"));
    assert_eq!(app.draft.secrets.api_key, "private-key");
}

#[test]
fn recording_requires_saving_audio_source_changes() {
    for field in 0..4 {
        let (mut app, _directory) = app();
        let saved = app.resources.as_ref().unwrap().settings.clone();
        match field {
            0 => { let _ = app.update(Message::Toggle(Toggle::Microphone, false)); }
            1 => { let _ = app.update(Message::Toggle(Toggle::System, true)); }
            2 => app.draft.settings.microphone_device = Some("different-microphone".into()),
            _ => app.draft.settings.system_device = Some("different-system-source".into()),
        }
        let _ = app.update(Message::Start);
        assert!(app.job.is_none());
        assert_eq!(app.page, Page::Settings);
        assert_eq!(app.settings_tab, SettingsTab::Audio);
        assert!(app.notice.as_ref().unwrap().0.contains("Save audio source changes"));
        assert_eq!(app.resources.as_ref().unwrap().settings.microphone_enabled, saved.microphone_enabled);
    }
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

fn tick_until(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !done(app) {
        assert!(std::time::Instant::now() < deadline, "background work did not finish");
        let _ = app.update(Message::Tick);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn processing_is_fifo_and_recording_controls_remain_independent() {
    let (mut app, _directory) = app();
    let (release, wait) = crossbeam_channel::bounded(1);
    let (started, order) = crossbeam_channel::unbounded();
    let first = started.clone();
    app.enqueue(JobKind::Diarization, Some("first".into()), "First".into(), move |_| {
        first.send(1).unwrap();
        wait.recv().unwrap();
        anyhow::bail!("speaker test failure")
    });
    app.enqueue(JobKind::Transcription, Some("second".into()), "Second".into(), move |_| {
        started.send(2).unwrap();
        Ok("second finished".into())
    });
    app.start_next();
    assert_eq!(order.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
    assert!(order.is_empty());
    assert!(app.can_record());
    assert!(app.recording_busy("first") && app.recording_busy("second"));
    assert!(!app.recording_busy("other"));
    app.start_job(JobKind::Recording, "Recording", move |reporter| {
        while !reporter.capture.is_stopped() { std::thread::sleep(Duration::from_millis(5)); }
        Ok("recording saved".into())
    });
    assert!(app.is_recording());
    assert!(app.can_process());
    let _ = app.update(Message::Pause);
    assert!(app.paused);
    app.page = Page::Library;
    let _ = app.view();
    release.send(()).unwrap();
    tick_until(&mut app, |app| app.processing.is_none() && app.queue.is_empty());
    assert_eq!(order.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
    assert!(app.is_recording() && app.paused);
    assert!(app.notice.as_ref().unwrap().0.contains("speaker test failure"));
    let _ = app.update(Message::Finish);
    tick_until(&mut app, |app| app.job.is_none());
}

#[test]
fn cancelling_processing_and_clearing_pending_work_preserves_foreground_capture() {
    let (mut app, _directory) = app();
    app.enqueue(JobKind::Diarization, Some("active".into()), "Active".into(), move |reporter| {
        while !reporter.cancel.load(std::sync::atomic::Ordering::Relaxed) { std::thread::sleep(Duration::from_millis(5)); }
        reporter.check_cancelled()?;
        Ok("done".into())
    });
    let (started, ran) = crossbeam_channel::unbounded();
    app.enqueue(JobKind::Diarization, Some("pending".into()), "Pending".into(), move |_| { started.send(()).unwrap(); Ok("done".into()) });
    app.start_next();
    let _ = app.update(Message::ClearQueue);
    assert!(!app.recording_busy("pending"));
    let _ = app.update(Message::CancelProcessing);
    assert!(app.processing.as_ref().unwrap().cancelling);
    tick_until(&mut app, |app| app.processing.is_none());
    assert!(ran.is_empty());
    assert!(app.can_record());
}

#[test]
fn queued_entries_cannot_be_changed_or_queued_twice_and_results_do_not_change_selection() {
    let (mut app, _directory) = app();
    let library = app.resources.as_ref().unwrap().library.clone();
    let first = Recording::new("First".into(), _directory.path());
    let second = Recording::new("Second".into(), _directory.path());
    library.save(&first).unwrap();
    library.save(&second).unwrap();
    app.show_recording(first.clone());
    let (release, wait) = crossbeam_channel::bounded(1);
    let first_id = first.id.clone();
    app.enqueue(JobKind::Diarization, Some(first_id), first.title.clone(), move |reporter| {
        wait.recv().unwrap();
        reporter.recording(&first);
        Ok("done".into())
    });
    app.start_next();
    assert!(!app.can_edit_selected());
    for message in [Message::RunDiarization, Message::RunTranscription, Message::SaveRecording, Message::RequestRemove, Message::Remove] {
        let _ = app.update(message);
    }
    assert!(app.queue.is_empty() && !app.confirm_remove && !app.saving);
    app.show_recording(second.clone());
    app.detail_title = "Unsaved title".into();
    assert!(app.can_edit_selected());
    release.send(()).unwrap();
    tick_until(&mut app, |app| app.processing.is_none());
    assert_eq!(app.selected.as_ref().unwrap().id, second.id);
    assert_eq!(app.detail_title, "Unsaved title");
}

#[test]
fn closing_cancels_both_workers_and_discards_pending_work() {
    let (mut app, _directory) = app();
    app.start_job(JobKind::Recording, "Recording", move |reporter| {
        while !reporter.capture.is_stopped() { std::thread::sleep(Duration::from_millis(5)); }
        Ok("saved".into())
    });
    app.enqueue(JobKind::Diarization, Some("active".into()), "Active".into(), move |reporter| {
        while !reporter.cancel.load(std::sync::atomic::Ordering::Relaxed) { std::thread::sleep(Duration::from_millis(5)); }
        reporter.check_cancelled()?;
        Ok("done".into())
    });
    app.start_next();
    app.enqueue(JobKind::Diarization, Some("pending".into()), "Pending".into(), |_| panic!("pending work must not start"));
    let _ = app.update(Message::Close(iced::window::Id::unique()));
    assert!(app.close_window.is_some());
    let _ = app.update(Message::ConfirmClose);
    assert!(app.queue.is_empty() && app.closing);
    assert!(app.job.as_ref().unwrap().cancel.load(std::sync::atomic::Ordering::Relaxed));
    assert!(app.processing.as_ref().unwrap().job.cancel.load(std::sync::atomic::Ordering::Relaxed));
    tick_until(&mut app, |app| app.job.is_none() && app.processing.is_none());
}

fn saved_transcript(app: &App, directory: &std::path::Path) -> Recording {
    let mut recording = Recording::new("Existing transcript".into(), directory);
    let mut writer = hound::WavWriter::create(&recording.audio_path, crate::audio::wave_spec()).unwrap();
    crate::audio::write_samples(&mut writer, &vec![0.0; crate::SAMPLE_RATE as usize]).unwrap();
    writer.finalize().unwrap();
    recording.duration = 1.0;
    recording.status = crate::domain::RecordingStatus::Ready;
    recording.segments = vec![crate::domain::Segment { start: 0.0, end: 1.0, text: "Saved transcript".into(), words: Vec::new(), speaker: None }];
    app.resources.as_ref().unwrap().library.save(&recording).unwrap();
    recording
}

#[test]
fn manual_speaker_action_loads_current_metadata_and_does_not_require_retranscription() {
    let (mut app, directory) = app();
    let mut recording = saved_transcript(&app, directory.path());
    app.show_recording(recording.clone());
    recording.summary = "Latest notes from the library".into();
    app.resources.as_ref().unwrap().library.save(&recording).unwrap();
    let resources = app.resources.as_mut().unwrap();
    resources.settings.python_path = directory.path().join("missing-python").to_string_lossy().into_owned();
    resources.settings.diarization_enabled = false;
    assert!(!resources.settings.model_path().exists());
    let _ = app.update(Message::RunDiarization);
    assert_eq!(app.processing.as_ref().unwrap().job.kind, JobKind::Diarization);
    assert!(app.can_record());
    let _ = app.update(Message::RunDiarization);
    assert!(app.queue.is_empty());
    tick_until(&mut app, |app| app.processing.is_none());
    let saved = app.resources.as_ref().unwrap().library.get(&recording.id).unwrap();
    assert_eq!(saved.summary, recording.summary);
    assert_eq!(saved.segments, recording.segments);
    assert!(saved.error.unwrap().contains("Python"));
    assert_eq!(app.selected.as_ref().unwrap().summary, recording.summary);
}

#[test]
fn recording_completion_hands_speakers_to_the_queue_and_releases_the_recording_slot() {
    let (mut app, directory) = app();
    let recording = saved_transcript(&app, directory.path());
    let settings = Settings { diarization_enabled: true, python_path: directory.path().join("missing-python").to_string_lossy().into_owned(), ..app.resources.as_ref().unwrap().settings.clone() };
    app.start_job(JobKind::Recording, "Recording", move |reporter| {
        reporter.recording(&recording);
        reporter.queue_diarization(&recording, settings, Secrets::default());
        Ok("saved".into())
    });
    while !app.job.as_ref().unwrap().is_finished() { std::thread::sleep(Duration::from_millis(5)); }
    let _ = app.update(Message::Tick);
    assert!(app.job.is_none() && app.can_record());
    assert_eq!(app.processing.as_ref().unwrap().job.kind, JobKind::Diarization);
    assert!(app.live.is_some());
    tick_until(&mut app, |app| app.processing.is_none());
    assert_eq!(app.live.as_ref().unwrap().segments[0].text, "Saved transcript");
}

#[test]
fn transcript_partial_redraw_matches_a_clean_frame_after_text_changes_and_scrolling() {
    use iced::{Color, Font, Point, Rectangle, Size, Vector, advanced::{Layout, Renderer, graphics::{Viewport, damage, text::font_system}, layout, mouse, renderer, widget::Tree}};

    font_system().write().unwrap().load_font(std::borrow::Cow::Borrowed(include_bytes!("../../assets/fonts/FiraSans-Regular.ttf")));
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let viewport = Viewport::with_physical_size(Size::new(640, 360), scale);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = iced::Renderer::new(Font::with_name("Fira Sans"), 16.0.into());
        let mut incremental = tiny_skia::Pixmap::new(640, 360).unwrap();
        let mut mask = tiny_skia::Mask::new(640, 360).unwrap();
        let mut previous = Vec::new();
        for (index, (content, offset)) in [
            ("То есть, когда у нас дробь в", 0.0),
            ("Больше 0, но меньше единицы.", 0.0),
            ("То есть, когда у нас дробь в", 13.25),
            ("First line wraps across the available width. То есть, когда у нас дробь в", 27.5),
            ("Больше 0, но меньше единицы.", 4.75),
        ].into_iter().enumerate() {
            renderer.reset(bounds);
            let mut element = views::transcript_text(content, index % 2 == 0);
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, Size::new(230.0, 300.0)))
                .move_to(Point::new(40.25, 45.5));
            renderer.with_layer(bounds, |renderer| renderer.with_translation(Vector::new(0.0, -offset), |renderer| {
                element.as_widget().draw(&tree, renderer, &iced::Theme::Dark, &renderer::Style { text_color: Color::WHITE }, Layout::new(&node), mouse::Cursor::Unavailable, &bounds);
            }));
            let layers = renderer.layers().to_vec();
            let dirty = if index == 0 { vec![bounds] } else {
                damage::group(damage::diff(&previous, &layers, |layer| vec![layer.bounds], iced_tiny_skia::Layer::damage), bounds)
            };
            renderer.draw(&mut incremental.as_mut(), &mut mask, &viewport, &dirty, Color::BLACK);
            let mut clean = tiny_skia::Pixmap::new(640, 360).unwrap();
            renderer.draw(&mut clean.as_mut(), &mut mask, &viewport, &[bounds], Color::BLACK);
            let mismatches = incremental.data().iter().zip(clean.data()).filter(|(a, b)| a != b).count();
            assert_eq!(mismatches, 0, "stale glyph pixels at scale {scale}, frame {index}");
            previous = layers;
        }
    }
}
