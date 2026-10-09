mod style;
mod views;
#[cfg(test)]
mod tests;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use iced::{Subscription, Task, widget::text_editor};

use crate::{
    capture::{self, Device, Devices},
    domain::{ExportFormat, LibraryItem, Recording, TranscriptionModel},
    engine::{self, Event, Job, JobKind},
    settings::{AppPaths, Appearance, Secrets, Settings},
    storage::Library,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page { Record, Library, Import, Settings }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab { Transcript, Words, Notes }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab { General, Audio, Transcription, Speakers, Notes }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    ModelsDir, RecordingsDir, Language, Threads, ChunkSeconds, VadThreshold,
    Python, DiarizationModel, MinSpeakers, MaxSpeakers,
    ApiUrl, ApiModel, ApiKey, HfToken, SummaryLanguage, SummaryChunkChars, SummaryMaxTokens,
}

#[derive(Debug, Clone, Copy)]
pub enum Toggle { Microphone, System, Live, Vad, Alignment, Diarization, Gpu, RememberSecrets }

pub struct Draft {
    settings: Settings,
    models_dir: String,
    recordings_dir: String,
    threads: String,
    chunk_seconds: String,
    vad_threshold: String,
    min_speakers: String,
    max_speakers: String,
    summary_chunk_chars: String,
    summary_max_tokens: String,
    prompt: text_editor::Content,
    secrets: Secrets,
    remember: bool,
}

impl Draft {
    fn new(settings: Settings, secrets: Secrets) -> Self {
        Self {
            models_dir: settings.models_dir.to_string_lossy().into_owned(),
            recordings_dir: settings.recordings_dir.to_string_lossy().into_owned(),
            threads: settings.threads.to_string(),
            chunk_seconds: settings.chunk_seconds.to_string(),
            vad_threshold: settings.vad_threshold.to_string(),
            min_speakers: settings.min_speakers.map(|count| count.to_string()).unwrap_or_default(),
            max_speakers: settings.max_speakers.map(|count| count.to_string()).unwrap_or_default(),
            summary_chunk_chars: settings.summary_chunk_chars.to_string(),
            summary_max_tokens: settings.summary_max_tokens.to_string(),
            prompt: text_editor::Content::with_text(&settings.summary_prompt), settings, secrets, remember: false,
        }
    }

    fn value(&self, field: Field) -> &str {
        match field {
            Field::ModelsDir => &self.models_dir,
            Field::RecordingsDir => &self.recordings_dir,
            Field::Language => &self.settings.language,
            Field::Threads => &self.threads,
            Field::ChunkSeconds => &self.chunk_seconds,
            Field::VadThreshold => &self.vad_threshold,
            Field::Python => &self.settings.python_path,
            Field::DiarizationModel => &self.settings.diarization_model,
            Field::MinSpeakers => &self.min_speakers,
            Field::MaxSpeakers => &self.max_speakers,
            Field::ApiUrl => &self.settings.summary_base_url,
            Field::ApiModel => &self.settings.summary_model,
            Field::ApiKey => &self.secrets.api_key,
            Field::HfToken => &self.secrets.huggingface_token,
            Field::SummaryLanguage => &self.settings.summary_language,
            Field::SummaryChunkChars => &self.summary_chunk_chars,
            Field::SummaryMaxTokens => &self.summary_max_tokens,
        }
    }

    fn edit(&mut self, field: Field, value: String) {
        let target = match field {
            Field::ModelsDir => &mut self.models_dir,
            Field::RecordingsDir => &mut self.recordings_dir,
            Field::Language => &mut self.settings.language,
            Field::Threads => &mut self.threads,
            Field::ChunkSeconds => &mut self.chunk_seconds,
            Field::VadThreshold => &mut self.vad_threshold,
            Field::Python => &mut self.settings.python_path,
            Field::DiarizationModel => &mut self.settings.diarization_model,
            Field::MinSpeakers => &mut self.min_speakers,
            Field::MaxSpeakers => &mut self.max_speakers,
            Field::ApiUrl => &mut self.settings.summary_base_url,
            Field::ApiModel => &mut self.settings.summary_model,
            Field::ApiKey => &mut self.secrets.api_key,
            Field::HfToken => &mut self.secrets.huggingface_token,
            Field::SummaryLanguage => &mut self.settings.summary_language,
            Field::SummaryChunkChars => &mut self.summary_chunk_chars,
            Field::SummaryMaxTokens => &mut self.summary_max_tokens,
        };
        *target = value;
    }

    fn build(&self) -> Result<Settings> {
        let mut settings = self.settings.clone();
        settings.models_dir = PathBuf::from(self.models_dir.trim());
        settings.recordings_dir = PathBuf::from(self.recordings_dir.trim());
        settings.threads = self.threads.trim().parse().context("CPU threads must be a whole number")?;
        settings.chunk_seconds = self.chunk_seconds.trim().parse().context("Chunk duration must be a whole number")?;
        settings.vad_threshold = self.vad_threshold.trim().parse().context("VAD threshold must be a number")?;
        settings.summary_chunk_chars = self.summary_chunk_chars.trim().parse().context("Summary chunk size must be a whole number")?;
        settings.summary_max_tokens = self.summary_max_tokens.trim().parse().context("Summary token limit must be a whole number")?;
        settings.min_speakers = optional_count(&self.min_speakers)?;
        settings.max_speakers = optional_count(&self.max_speakers)?;
        settings.language = settings.language.trim().to_lowercase();
        settings.summary_prompt = self.prompt.text();
        settings.validate()?;
        if !settings.summary_base_url.trim().is_empty() {
            crate::summary::endpoint(&settings.summary_base_url)?;
        }
        Ok(settings)
    }
}

fn optional_count(value: &str) -> Result<Option<u32>> {
    if value.trim().is_empty() { Ok(None) } else { Ok(Some(value.trim().parse().context("Speaker counts must be whole numbers, or empty for automatic detection")?)) }
}

#[derive(Clone)]
pub struct Resources {
    paths: AppPaths,
    settings: Settings,
    secrets: Secrets,
    library: Library,
    _lock: std::sync::Arc<std::fs::File>,
}

#[derive(Clone)]
pub struct Loaded {
    resources: Resources,
    items: Vec<LibraryItem>,
    devices: Devices,
    recovered: usize,
}

#[derive(Clone)]
pub enum Message {
    Loaded(std::result::Result<Loaded, String>),
    Retry,
    Navigate(Page),
    Tick,
    Search(String),
    SearchResults(String, std::result::Result<Vec<LibraryItem>, String>),
    Select(String),
    Selected(String, std::result::Result<Recording, String>),
    RecordingTitle(String),
    Start,
    Pause,
    Finish,
    Cancel,
    OpenLive,
    PickImport,
    ImportPicked(Option<PathBuf>),
    RunTranscription,
    RequestSummary,
    ConfirmSummary,
    DismissSummary,
    Tab(DetailTab),
    TranscriptFilter(String),
    DetailTitle(String),
    RecordingPrompt(text_editor::Action),
    SaveRecording,
    RecordingSaved(std::result::Result<Recording, String>),
    CopyTranscript,
    CopyNotes,
    ExportFormat(ExportFormat),
    Export,
    ExportPicked(Option<PathBuf>, String),
    OpenAudio,
    OpenFolder,
    Done(std::result::Result<String, String>),
    RequestRemove,
    Remove,
    DismissRemove,
    Setting(Field, String),
    Appearance(Appearance),
    SettingsTab(SettingsTab),
    Toggle(Toggle, bool),
    Model(TranscriptionModel),
    Microphone(String),
    System(String),
    SettingsPrompt(text_editor::Action),
    PickDirectory(Field),
    DirectoryPicked(Field, Option<PathBuf>),
    SaveSettings,
    SettingsSaved(std::result::Result<(Settings, Secrets, Option<String>), String>),
    RefreshDevices,
    Devices(Devices),
    DownloadModels,
    ProbePython,
    ClearNotice,
    Close(iced::window::Id),
    DismissClose,
    ConfirmClose,
    Resize(f32),
}

impl std::fmt::Debug for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("Message").field(&std::mem::discriminant(self)).finish()
    }
}

pub struct App {
    resources: Option<Resources>,
    fatal: Option<String>,
    page: Page,
    settings_tab: SettingsTab,
    draft: Draft,
    devices: Devices,
    microphone_labels: Vec<String>,
    system_labels: Vec<String>,
    items: Vec<LibraryItem>,
    search: String,
    requested_id: Option<String>,
    selected: Option<Recording>,
    live: Option<Recording>,
    recording_title: String,
    detail_title: String,
    recording_prompt: text_editor::Content,
    detail_tab: DetailTab,
    transcript_filter: String,
    export_format: ExportFormat,
    job: Option<Job>,
    progress: f32,
    progress_label: String,
    notice: Option<(String, bool)>,
    paused: bool,
    stopping: bool,
    audio_saved: bool,
    elapsed: f64,
    microphone_level: f32,
    system_level: f32,
    confirm_summary: bool,
    confirm_remove: bool,
    close_window: Option<iced::window::Id>,
    closing: bool,
    saving: bool,
    dialog_open: bool,
    width: f32,
}

pub fn run() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("parakeetx")
        .theme(App::theme)
        .subscription(App::subscription)
        .font(include_bytes!("../assets/fonts/FiraSans-Regular.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/FiraSans-Medium.ttf").as_slice())
        .font(include_bytes!("../assets/fonts/FiraSans-SemiBold.ttf").as_slice())
        .default_font(iced::Font::with_name("Fira Sans"))
        .window(iced::window::Settings {
            size: iced::Size::new(1280.0, 850.0),
            min_size: Some(iced::Size::new(900.0, 640.0)),
            exit_on_close_request: false,
            ..Default::default()
        })
        .centered()
        .run()
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let state = Self {
            resources: None, fatal: None, page: Page::Record, settings_tab: SettingsTab::General,
            draft: Draft::new(Settings::default(), Secrets::default()),
            devices: Devices::default(), microphone_labels: Vec::new(), system_labels: Vec::new(),
            items: Vec::new(), search: String::new(), requested_id: None, selected: None, live: None,
            recording_title: String::new(), detail_title: String::new(), recording_prompt: text_editor::Content::new(),
            detail_tab: DetailTab::Transcript, transcript_filter: String::new(), export_format: ExportFormat::Markdown,
            job: None, progress: 0.0, progress_label: String::new(), notice: None,
            paused: false, stopping: false, audio_saved: false, elapsed: 0.0, microphone_level: 0.0, system_level: 0.0,
            confirm_summary: false, confirm_remove: false, close_window: None, closing: false, saving: false, dialog_open: false,
            width: 1280.0,
        };
        (state, load())
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(80)).map(|_| Message::Tick),
            iced::window::close_requests().map(Message::Close),
            iced::window::resize_events().map(|(_, size)| Message::Resize(size.width)),
        ])
    }

    fn theme(&self) -> iced::Theme { style::theme(self.draft.settings.appearance) }

    fn busy(&self) -> bool { self.job.is_some() || self.saving || self.dialog_open }

    fn is_recording(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.kind == JobKind::Recording) && !self.audio_saved
    }

    fn set_notice(&mut self, message: impl Into<String>, error: bool) {
        self.notice = Some((message.into(), error));
    }

    fn start_job(&mut self, kind: JobKind, label: &str, task: impl FnOnce(engine::Reporter) -> Result<String> + Send + 'static) {
        self.notice = None;
        self.progress = 0.0;
        self.progress_label = label.into();
        self.job = Some(Job::spawn(kind, task));
    }

    fn reload_library(&self) -> Task<Message> {
        let Some(resources) = &self.resources else { return Task::none() };
        let library = resources.library.clone();
        let query = self.search.clone();
        let result_query = query.clone();
        blocking(move || library.search(&query), move |result| Message::SearchResults(result_query, result))
    }

    fn set_devices(&mut self, devices: Devices) {
        self.microphone_labels = device_labels(&devices.microphones);
        self.system_labels = device_labels(&devices.system);
        self.devices = devices;
    }

    fn show_recording(&mut self, recording: Recording) {
        self.detail_title = recording.title.clone();
        self.recording_prompt = text_editor::Content::with_text(&recording.summary_prompt);
        self.requested_id = Some(recording.id.clone());
        self.selected = Some(recording);
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(result) => match result {
                Ok(loaded) => {
                    self.draft = Draft::new(loaded.resources.settings.clone(), loaded.resources.secrets.clone());
                    self.items = loaded.items;
                    self.set_devices(loaded.devices);
                    if loaded.recovered > 0 { self.set_notice(format!("Recovered {} interrupted recording(s). Their audio remains in the library.", loaded.recovered), false); }
                    self.resources = Some(loaded.resources);
                    self.fatal = None;
                }
                Err(error) => self.fatal = Some(error),
            },
            Message::Retry => { self.fatal = None; return load(); }
            Message::Navigate(page) => { self.page = page; if page == Page::Library { return self.reload_library(); } }
            Message::Tick => {
                let (events, kind) = match &self.job {
                    Some(job) => (job.events.try_iter().take(200).collect::<Vec<_>>(), job.kind),
                    None => (Vec::new(), JobKind::Import),
                };
                let mut finished = false;
                let mut scroll = false;
                for event in events {
                    match event {
                        Event::Recording(recording) => {
                            if kind == JobKind::Recording {
                                self.live = Some((*recording).clone());
                                scroll = true;
                            }
                            if kind == JobKind::Import || self.selected.as_ref().is_some_and(|selected| selected.id == recording.id) {
                                self.show_recording(*recording);
                            }
                        }
                        Event::AudioSaved => { self.audio_saved = true; self.microphone_level = 0.0; self.system_level = 0.0; }
                        Event::Levels { duration, microphone, system } => {
                            self.elapsed = duration;
                            self.microphone_level = self.microphone_level * 0.35 + microphone * 0.65;
                            self.system_level = self.system_level * 0.35 + system * 0.65;
                        }
                        Event::Progress { label, fraction } => { self.progress_label = label; self.progress = fraction; }
                        Event::Warning(warning) => self.set_notice(warning, true),
                        Event::Finished(result) => {
                            finished = true;
                            match result {
                                Ok(message) => if !self.notice.as_ref().is_some_and(|(_, error)| *error) { self.set_notice(message, false); },
                                Err(error) => self.set_notice(error, true),
                            }
                        }
                    }
                }
                if finished {
                    self.job.take();
                    self.paused = false;
                    self.stopping = false;
                    self.audio_saved = false;
                    self.microphone_level = 0.0;
                    self.system_level = 0.0;
                    if self.closing && let Some(window) = self.close_window { return iced::window::close(window); }
                    return self.reload_library();
                }
                if scroll { return iced::widget::operation::snap_to_end("live-transcript"); }
            }
            Message::Search(query) => { self.search = query; return self.reload_library(); }
            Message::SearchResults(query, result) => if query == self.search {
                match result { Ok(items) => self.items = items, Err(error) => self.set_notice(error, true) }
            },
            Message::Select(id) => {
                let Some(resources) = &self.resources else { return Task::none() };
                let library = resources.library.clone();
                self.requested_id = Some(id.clone());
                let result_id = id.clone();
                return blocking(move || library.get(&id), move |result| Message::Selected(result_id, result));
            }
            Message::Selected(id, result) => if self.requested_id.as_deref() == Some(&id) {
                match result { Ok(recording) => { self.show_recording(recording); self.detail_tab = DetailTab::Transcript; self.transcript_filter.clear(); }, Err(error) => self.set_notice(error, true) }
            },
            Message::RecordingTitle(title) => self.recording_title = title,
            Message::Start if !self.busy() => {
                let Some(resources) = self.resources.clone() else { return Task::none() };
                let title = self.recording_title.clone();
                self.live = None; self.elapsed = 0.0; self.paused = false; self.audio_saved = false; self.stopping = false;
                self.start_job(JobKind::Recording, "Opening audio sources", move |reporter| engine::record(title, resources.settings, resources.secrets, resources.library, reporter));
            }
            Message::Pause if self.is_recording() && !self.stopping => {
                self.paused = !self.paused;
                if let Some(job) = &self.job { job.capture.set_paused(self.paused); }
            }
            Message::Finish if self.is_recording() => {
                self.stopping = true;
                if let Some(job) = &self.job { job.finish_recording(); }
                self.progress_label = "Saving audio and finishing transcription".into();
            }
            Message::Cancel => {
                if let Some(job) = &self.job { job.cancel(); }
                self.stopping = true;
                self.progress_label = "Stopping safely; keeping saved audio".into();
            }
            Message::OpenLive => {
                if let Some(recording) = self.live.clone() { self.show_recording(recording); self.page = Page::Library; return self.reload_library(); }
            }
            Message::PickImport if !self.busy() => {
                self.dialog_open = true;
                return Task::perform(async {
                    rfd::AsyncFileDialog::new().set_title("Import an audio or video recording")
                        .add_filter("Recordings", &["mp3", "mp4", "m4a", "wav", "flac", "ogg", "mkv", "aac", "aiff"])
                        .pick_file().await.map(|handle| handle.path().to_owned())
                }, Message::ImportPicked);
            }
            Message::ImportPicked(path) => {
                self.dialog_open = false;
                if let (Some(path), Some(resources)) = (path, self.resources.clone()) && self.job.is_none() {
                    self.start_job(JobKind::Import, "Importing recording", move |reporter| engine::import(path, resources.settings, resources.library, reporter));
                }
            }
            Message::RunTranscription if !self.busy() => {
                if let (Some(recording), Some(resources)) = (self.selected.clone(), self.resources.clone()) {
                    self.page = Page::Library;
                    self.detail_tab = DetailTab::Transcript;
                    self.start_job(JobKind::Transcription, "Preparing transcription", move |reporter| engine::transcribe(recording, resources.settings, resources.secrets, resources.library, reporter));
                }
            }
            Message::RequestSummary if !self.busy() => self.confirm_summary = true,
            Message::DismissSummary => self.confirm_summary = false,
            Message::ConfirmSummary if !self.busy() => {
                self.confirm_summary = false;
                if let (Some(mut recording), Some(resources)) = (self.selected.clone(), self.resources.clone()) {
                    recording.summary_prompt = self.recording_prompt.text();
                    self.detail_tab = DetailTab::Notes;
                    self.start_job(JobKind::Summary, "Generating notes", move |reporter| engine::summarize(recording, resources.settings, resources.secrets, resources.library, reporter));
                }
            }
            Message::Tab(tab) => self.detail_tab = tab,
            Message::TranscriptFilter(filter) => self.transcript_filter = filter,
            Message::DetailTitle(title) => self.detail_title = title,
            Message::RecordingPrompt(action) => self.recording_prompt.perform(action),
            Message::SaveRecording if !self.busy() => {
                if let (Some(mut recording), Some(resources)) = (self.selected.clone(), self.resources.clone()) {
                    if self.detail_title.trim().is_empty() { self.set_notice("The recording needs a title.", true); return Task::none(); }
                    recording.title = self.detail_title.trim().into();
                    recording.summary_prompt = self.recording_prompt.text();
                    self.saving = true;
                    return blocking(move || { resources.library.save(&recording)?; Ok(recording) }, Message::RecordingSaved);
                }
            }
            Message::RecordingSaved(result) => {
                self.saving = false;
                match result { Ok(recording) => { self.show_recording(recording); self.set_notice("Recording details saved.", false); return self.reload_library(); }, Err(error) => self.set_notice(error, true) }
            }
            Message::CopyTranscript => if let Some(recording) = &self.selected { return iced::clipboard::write(recording.transcript()); },
            Message::CopyNotes => if let Some(recording) = &self.selected { return iced::clipboard::write(recording.summary.clone()); },
            Message::ExportFormat(format) => self.export_format = format,
            Message::Export if !self.dialog_open => if let Some(recording) = &self.selected {
                match crate::domain::export(recording, self.export_format) {
                    Ok(content) => {
                        let extension = self.export_format.extension();
                        let name: String = recording.title.chars().map(|character| if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_') { character } else { '_' }).take(100).collect();
                        self.dialog_open = true;
                        return Task::perform(async move {
                            let path = rfd::AsyncFileDialog::new().set_file_name(format!("{name}.{extension}"))
                                .add_filter("Export", &[extension]).save_file().await.map(|file| file.path().to_owned());
                            (path, content)
                        }, |(path, content)| Message::ExportPicked(path, content));
                    }
                    Err(error) => self.set_notice(format!("{error:#}"), true),
                }
            },
            Message::ExportPicked(path, content) => {
                self.dialog_open = false;
                if let Some(path) = path {
                    return blocking(move || { crate::settings::atomic_write(&path, content.as_bytes())?; Ok(format!("Export saved to {}", path.display())) }, Message::Done);
                }
            }
            Message::OpenAudio => if let Some(recording) = &self.selected {
                let path = recording.audio_path.clone();
                return blocking(move || { ensure!(path.is_file(), "Recording audio is missing"); open::that_detached(path)?; Ok("Audio opened in your default player.".into()) }, Message::Done);
            },
            Message::OpenFolder => if let Some(resources) = &self.resources {
                let path = resources.settings.recordings_dir.clone();
                return blocking(move || { open::that_detached(path)?; Ok("Recording folder opened.".into()) }, Message::Done);
            },
            Message::Done(result) => match result { Ok(message) => self.set_notice(message, false), Err(error) => self.set_notice(error, true) },
            Message::RequestRemove if !self.busy() => self.confirm_remove = true,
            Message::DismissRemove => self.confirm_remove = false,
            Message::Remove if !self.busy() => {
                self.confirm_remove = false;
                if let (Some(recording), Some(resources)) = (self.selected.take(), self.resources.clone()) {
                    self.requested_id = None;
                    let query = self.search.clone();
                    let result_query = query.clone();
                    return blocking(move || { resources.library.remove_from_library(&recording.id)?; resources.library.search(&query) }, move |result| Message::SearchResults(result_query, result));
                }
            }
            Message::Setting(field, value) => self.draft.edit(field, value),
            Message::Appearance(appearance) if !self.saving => self.draft.settings.appearance = appearance,
            Message::SettingsTab(tab) => { self.settings_tab = tab; self.page = Page::Settings; }
            Message::Toggle(toggle, value) => match toggle {
                Toggle::Microphone => self.draft.settings.microphone_enabled = value,
                Toggle::System => self.draft.settings.system_enabled = value,
                Toggle::Live => self.draft.settings.live_transcription = value,
                Toggle::Vad => self.draft.settings.vad_enabled = value,
                Toggle::Alignment => self.draft.settings.alignment_enabled = value,
                Toggle::Diarization => self.draft.settings.diarization_enabled = value,
                Toggle::Gpu => self.draft.settings.use_gpu = value,
                Toggle::RememberSecrets => self.draft.remember = value,
            },
            Message::Model(model) => self.draft.settings.model = model,
            Message::Microphone(label) => self.draft.settings.microphone_device = selected_device(&self.devices.microphones, &self.microphone_labels, &label),
            Message::System(label) => self.draft.settings.system_device = selected_device(&self.devices.system, &self.system_labels, &label),
            Message::SettingsPrompt(action) => self.draft.prompt.perform(action),
            Message::PickDirectory(field) if !self.dialog_open => {
                self.dialog_open = true;
                return Task::perform(async { rfd::AsyncFileDialog::new().pick_folder().await.map(|file| file.path().to_owned()) }, move |path| Message::DirectoryPicked(field, path));
            }
            Message::DirectoryPicked(field, path) => { self.dialog_open = false; if let Some(path) = path { self.draft.edit(field, path.to_string_lossy().into_owned()); } }
            Message::SaveSettings if !self.busy() => {
                match (self.draft.build(), self.resources.clone()) {
                    (Ok(settings), Some(resources)) => {
                        let secrets = self.draft.secrets.clone();
                        let remember = self.draft.remember;
                        self.saving = true;
                        return blocking(move || {
                            settings.save(&resources.paths)?;
                            let warning = if remember { secrets.save().err().map(|error| format!("Settings saved. Credentials are session-only: {error:#}")) } else { None };
                            Ok((settings, secrets, warning))
                        }, Message::SettingsSaved);
                    }
                    (Err(error), _) => self.set_notice(format!("{error:#}"), true),
                    _ => {}
                }
            }
            Message::SettingsSaved(result) => {
                self.saving = false;
                match result {
                    Ok((settings, secrets, warning)) => {
                        if let Some(resources) = self.resources.as_mut() { resources.settings = settings; resources.secrets = secrets; }
                        match warning { Some(warning) => self.set_notice(warning, true), None => self.set_notice("Settings saved.", false) }
                    }
                    Err(error) => self.set_notice(error, true),
                }
            }
            Message::RefreshDevices => return blocking(|| Ok(capture::devices()), |result| Message::Devices(result.unwrap_or_else(|error| Devices { note: Some(error), ..Default::default() }))),
            Message::Devices(devices) => self.set_devices(devices),
            Message::DownloadModels if !self.busy() => match self.draft.build() {
                Ok(settings) => self.start_job(JobKind::Download, "Downloading model weights", move |reporter| {
                    settings.prepare_directories()?;
                    crate::download::fetch_model(settings.model, &settings.model_path(), &reporter.cancel, |fraction| reporter.progress(format!("Downloading {}", settings.model), fraction))?;
                    if settings.uses_vad() {
                        crate::download::fetch(crate::download::VAD_URL, &settings.vad_path(), Some(crate::download::VAD_SHA1), &reporter.cancel, |fraction| reporter.progress("Downloading Silero VAD", fraction))?;
                    }
                    Ok("Model weights are ready. Save settings to use this configuration.".into())
                }),
                Err(error) => self.set_notice(format!("{error:#}"), true),
            },
            Message::ProbePython if !self.busy() => match self.draft.build() {
                Ok(settings) => self.start_job(JobKind::PythonCheck, "Checking Python and pyannote.audio", move |reporter| crate::diarize::probe(&settings, &reporter.cancel)),
                Err(error) => self.set_notice(format!("{error:#}"), true),
            },
            Message::ClearNotice => self.notice = None,
            Message::Close(window) => {
                if !self.busy() { return iced::window::close(window); }
                self.close_window = Some(window);
            }
            Message::DismissClose => self.close_window = None,
            Message::ConfirmClose => {
                self.closing = true;
                if let Some(job) = &self.job { job.cancel(); self.progress_label = "Saving audio before closing".into(); }
                else if let Some(window) = self.close_window { return iced::window::close(window); }
            }
            Message::Resize(width) => self.width = width,
            _ => {}
        }
        Task::none()
    }
}

fn load() -> Task<Message> {
    blocking(|| {
        let paths = AppPaths::discover()?;
        let lock = std::sync::Arc::new(paths.lock()?);
        let settings = Settings::load(&paths)?;
        let library = Library::open(paths.database())?;
        let recovered = library.recover_interrupted()?;
        let items = library.search("")?;
        let secrets = Secrets::load();
        let devices = capture::devices();
        Ok(Loaded { resources: Resources { paths, settings, library, secrets, _lock: lock }, items, devices, recovered })
    }, Message::Loaded)
}

fn blocking<Output: Send + 'static>(work: impl FnOnce() -> Result<Output> + Send + 'static, map: impl FnOnce(std::result::Result<Output, String>) -> Message + Send + 'static) -> Task<Message> {
    Task::perform(async move {
        tokio::task::spawn_blocking(work).await.map_err(|error| error.to_string()).and_then(|result| result.map_err(|error| format!("{error:#}")))
    }, map)
}

fn device_labels(devices: &[Device]) -> Vec<String> {
    devices.iter().enumerate().map(|(index, device)| format!("{}  [{}]", device.label, index + 1)).collect()
}

fn selected_device(devices: &[Device], labels: &[String], selected: &str) -> Option<String> {
    labels.iter().position(|label| label == selected).and_then(|index| devices.get(index)).map(|device| device.id.clone())
}

fn selected_label(devices: &[Device], labels: &[String], id: Option<&str>) -> Option<String> {
    id.and_then(|id| devices.iter().position(|device| device.id == id)).and_then(|index| labels.get(index)).cloned()
}
