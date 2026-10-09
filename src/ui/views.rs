use iced::{Alignment, Element, Fill, Font, widget::{self, Column, Row, button, checkbox, column, container, opaque, pick_list, progress_bar, row, scrollable, stack, svg, text, text_editor, text_input}};

use super::{App, DetailTab, Field, Message, Page, SettingsTab, Toggle, selected_label, style};
use crate::{domain::{ExportFormat, Recording, TranscriptionModel, timestamp}, settings::Appearance};

#[derive(Clone, Copy)]
enum Icon { Microphone, Library, Import, Settings, Waveform, File, Pause, Play, Stop, Close }

fn icon(kind: Icon, size: u16) -> widget::Svg<'static> {
    let bytes: &'static [u8] = match kind {
        Icon::Microphone => include_bytes!("../../assets/icons/microphone.svg"),
        Icon::Library => include_bytes!("../../assets/icons/list.svg"),
        Icon::Import => include_bytes!("../../assets/icons/upload-simple.svg"),
        Icon::Settings => include_bytes!("../../assets/icons/sliders-horizontal.svg"),
        Icon::Waveform => include_bytes!("../../assets/icons/waveform.svg"),
        Icon::File => include_bytes!("../../assets/icons/file-audio.svg"),
        Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
        Icon::Play => include_bytes!("../../assets/icons/play.svg"),
        Icon::Stop => include_bytes!("../../assets/icons/stop.svg"),
        Icon::Close => include_bytes!("../../assets/icons/x.svg"),
    };
    svg(svg::Handle::from_memory(bytes)).width(f32::from(size)).height(f32::from(size))
        .style(|theme, _| svg::Style { color: Some(style::tokens(theme).muted) })
}

fn muted<'a>(value: impl iced::widget::text::IntoFragment<'a>) -> widget::Text<'a> {
    text(value).size(13).style(style::muted)
}

fn heading<'a>(value: impl iced::widget::text::IntoFragment<'a>) -> widget::Text<'a> {
    text(value).size(26).font(Font { weight: iced::font::Weight::Semibold, ..Font::DEFAULT })
}

fn label<'a>(value: impl iced::widget::text::IntoFragment<'a>) -> widget::Text<'a> {
    text(value).size(14).font(Font { weight: iced::font::Weight::Medium, ..Font::DEFAULT })
}

fn spacer() -> widget::Space { widget::Space::new().width(Fill) }
fn vspace(height: f32) -> widget::Space { widget::Space::new().height(height) }
fn divider() -> widget::Rule<'static> { widget::rule::horizontal(1).style(style::divider) }

fn scroll<'a>(content: impl Into<Element<'a, Message>>) -> widget::Scrollable<'a, Message> {
    scrollable(content).spacing(10).style(style::scroller)
}

fn action<'a>(caption: &'a str, message: Message, enabled: bool) -> widget::Button<'a, Message> {
    button(label(caption)).padding([11, 18]).style(style::primary).on_press_maybe(enabled.then_some(message))
}

fn secondary<'a>(caption: &'a str, message: Message, enabled: bool) -> widget::Button<'a, Message> {
    button(text(caption).size(13)).padding([10, 14]).style(style::secondary).on_press_maybe(enabled.then_some(message))
}

fn quiet<'a>(caption: &'a str, message: Message, enabled: bool) -> widget::Button<'a, Message> {
    button(text(caption).size(13)).padding([10, 12]).style(style::quiet).on_press_maybe(enabled.then_some(message))
}

impl App {
    pub(super) fn view(&self) -> Element<'_, Message> {
        if let Some(error) = &self.fatal {
            return container(column![heading("Cannot open workspace"), text(error).size(14), action("Try again", Message::Retry, true)]
                .spacing(24).max_width(640)).padding(40).center_x(Fill).center_y(Fill).into();
        }
        if self.resources.is_none() {
            return container(muted("Opening workspace...")).center_x(Fill).center_y(Fill).into();
        }
        let content = match self.page {
            Page::Record => self.record_view(),
            Page::Import => self.import_view(),
            Page::Library => self.library_view(),
            Page::Settings => self.settings_view(),
        };
        let mut main = column![content].spacing(16).height(Fill).width(Fill);
        if self.job.is_some() && (!self.is_recording() || self.stopping) {
            main = main.push(container(column![
                row![label(&self.progress_label).width(Fill), quiet(if self.stopping { "Stopping..." } else { "Cancel" }, Message::Cancel, !self.stopping)].align_y(Alignment::Center).spacing(12),
                progress_bar(0.0..=1.0, self.progress).girth(3).style(style::meter),
            ].spacing(8)).padding([10, 16]).style(style::surface));
        }
        if let Some((message, error)) = &self.notice {
            main = main.push(container(row![
                text(message).size(13).width(Fill),
                widget::tooltip(button(icon(Icon::Close, 16)).padding(6).style(style::quiet).on_press(Message::ClearNotice), text("Dismiss").size(12), widget::tooltip::Position::Left),
            ].spacing(12).align_y(Alignment::Center)).padding([8, 14]).style(if *error { style::alert } else { style::notice }));
        }
        let base = row![
            self.sidebar(),
            widget::rule::vertical(1).style(style::divider),
            container(container(main).max_width(1220)).padding(if self.width < 1100.0 { 24 } else { 32 }).center_x(Fill).height(Fill),
        ].height(Fill);
        if let Some(dialog) = self.confirmation() {
            stack![base, opaque(container(dialog).padding(24).center_x(Fill).center_y(Fill).style(style::scrim))].into()
        } else {
            base.into()
        }
    }

    fn confirmation(&self) -> Option<Element<'_, Message>> {
        let content = if self.close_window.is_some() {
            column![
                text(if self.closing { "Saving audio..." } else { "Stop and close?" }).size(22),
                muted("Closing stops the active operation. Saved audio and completed transcript segments are kept."),
                row![secondary("Keep working", Message::DismissClose, !self.closing), action("Stop and close", Message::ConfirmClose, !self.closing)].spacing(12),
            ].spacing(20)
        } else if self.confirm_remove && self.page == Page::Library {
            column![
                text("Remove recording?").size(22),
                muted("The audio file stays on disk. The transcript and notes will be deleted."),
                row![secondary("Cancel", Message::DismissRemove, true), action("Remove", Message::Remove, !self.busy()).style(style::record)].spacing(12),
            ].spacing(20)
        } else if self.confirm_summary && self.page == Page::Library {
            let endpoint = &self.resources.as_ref()?.settings.summary_base_url;
            column![
                text("Send transcript for notes?").size(22),
                muted(format!("Destination: {}", if endpoint.is_empty() { "Not configured" } else { endpoint })),
                muted("Only transcript text is sent, not audio. Remote services may store requests; local endpoints process on this device."),
                row![secondary("Cancel", Message::DismissSummary, true), action("Generate notes", Message::ConfirmSummary, !self.busy())].spacing(12),
            ].spacing(20)
        } else {
            return None;
        };
        Some(container(content).padding(28).width(Fill).max_width(500).style(style::surface).into())
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut navigation = column![
            container(text("parakeetx").size(23).font(Font { weight: iced::font::Weight::Semibold, ..Font::DEFAULT })).padding([0, 10]),
            vspace(24.0),
        ].spacing(6);
        for (page, name, symbol) in [(Page::Record, "Record", Icon::Microphone), (Page::Library, "Library", Icon::Library), (Page::Import, "Import", Icon::Import)] {
            navigation = navigation.push(self.navigation_item(page, name, symbol));
        }
        navigation = navigation.push(widget::Space::new().height(Fill))
            .push(self.navigation_item(Page::Settings, "Settings", Icon::Settings));
        container(navigation).padding([28, 14]).width(if self.width < 1100.0 { 164 } else { 184 }).height(Fill).style(style::sidebar).into()
    }

    fn navigation_item(&self, page: Page, name: &'static str, symbol: Icon) -> Element<'_, Message> {
        let active = page == self.page;
        let graphic = icon(symbol, 18).style(move |theme, _| svg::Style { color: Some(if active { style::tokens(theme).text } else { style::tokens(theme).muted }) });
        button(row![graphic, text(name).size(14)].spacing(12).align_y(Alignment::Center))
            .padding([12, 12]).width(Fill).on_press(Message::Navigate(page))
            .style(move |theme, status| style::navigation(theme, active, status)).into()
    }

    fn record_view(&self) -> Element<'_, Message> {
        let settings = &self.resources.as_ref().unwrap().settings;
        let running = self.is_recording();
        let state = if self.stopping { "Saving..." } else if self.paused { "Paused" } else if running { "Recording" } else if self.live.is_some() { "Saved" } else { "" };
        let body: Element<'_, Message> = if let Some(recording) = &self.live {
            if recording.segments.is_empty() {
                let message = if !running { "No transcript. Transcribe this recording in Library." }
                    else if !settings.live_transcription { "Live transcription is off. Audio is being saved." }
                    else if self.paused { "Recording paused." }
                    else { "Waiting for speech..." };
                container(muted(message)).padding(24).center_x(Fill).center_y(Fill).into()
            } else {
                self.transcript_rows(recording, "live-transcript", false)
            }
        } else {
            let mut empty = column![icon(Icon::Waveform, 32), muted("Your transcript will appear here.")].spacing(18).align_x(Alignment::Center);
            if settings.live_transcription && !crate::download::model_ready(settings.model, &settings.model_path()) {
                empty = empty.push(quiet("Choose a transcription model", Message::SettingsTab(SettingsTab::Transcription), true));
            }
            container(empty).padding(24).center_x(Fill).center_y(Fill).into()
        };
        let transcript = container(column![
            row![label("Transcript"), spacer(), muted(state)].align_y(Alignment::Center),
            body,
        ].spacing(24).height(Fill)).padding(24).height(Fill).style(style::surface);
        let sources = row![
            self.source_meter("Microphone", settings.microphone_enabled, self.microphone_level),
            self.source_meter("System audio", settings.system_enabled, self.system_level),
            spacer(),
            quiet("Audio sources", Message::SettingsTab(SettingsTab::Audio), true),
        ].spacing(24).align_y(Alignment::Center);
        let controls: Element<'_, Message> = if running {
            row![
                text(timestamp(self.elapsed)).size(28).font(Font::MONOSPACE),
                spacer(),
                button(row![icon(if self.paused { Icon::Play } else { Icon::Pause }, 16), text(if self.paused { "Resume" } else { "Pause" }).size(14)].spacing(8).align_y(Alignment::Center))
                    .padding([11, 16]).style(style::secondary).on_press_maybe((!self.stopping).then_some(Message::Pause)),
                button(row![icon(Icon::Stop, 16).style(|theme, _| svg::Style { color: Some(style::tokens(theme).canvas) }), label(if self.stopping { "Saving..." } else { "Finish" })].spacing(8).align_y(Alignment::Center))
                    .padding([11, 18]).style(style::record).on_press_maybe((!self.stopping).then_some(Message::Finish)),
            ].spacing(12).align_y(Alignment::Center).into()
        } else {
            let title = column![label("Recording name"), text_input("Optional", &self.recording_title).on_input(Message::RecordingTitle).padding(12).size(14).style(style::field)].spacing(8).width(Fill);
            let mut controls = row![title].spacing(12).align_y(Alignment::End);
            if self.live.is_some() { controls = controls.push(secondary("View recording", Message::OpenLive, true)); }
            controls.push(button(row![
                icon(Icon::Microphone, 18).style(|theme, _| svg::Style { color: Some(style::tokens(theme).canvas) }),
                label("Start recording"),
            ].spacing(10).align_y(Alignment::Center)).padding([12, 18]).style(style::record).on_press_maybe((!self.busy()).then_some(Message::Start))).into()
        };
        column![heading("Record"), transcript, sources, divider(), controls].spacing(24).height(Fill).into()
    }

    fn source_meter<'a>(&self, name: &'a str, enabled: bool, level: f32) -> Element<'a, Message> {
        let mut source = column![row![muted(name), text(if enabled { "On" } else { "Off" }).size(12).style(if enabled { style::accent } else { style::muted })].spacing(10)].spacing(8).width(145);
        if self.is_recording() {
            source = source.push(progress_bar(0.0..=1.0, if enabled { level.sqrt() } else { 0.0 }).girth(3).style(style::meter));
        }
        source.into()
    }

    fn import_view(&self) -> Element<'_, Message> {
        let picker = column![
            icon(Icon::File, 36),
            text("Import audio or video").size(22),
            muted("MP3, MP4, M4A, WAV, FLAC, OGG, AAC, AIFF, MKV"),
            vspace(4.0),
            action(if self.dialog_open { "Choosing..." } else { "Choose file" }, Message::PickImport, !self.busy()),
        ].spacing(18).align_x(Alignment::Center);
        let mut content = column![container(picker).padding(40).center_x(Fill)].spacing(24).max_width(660);
        if let Some(recording) = &self.selected && recording.original_path.is_some() {
            content = content.push(container(column![
                label(&recording.title),
                muted(format!("{}  ·  {}", timestamp(recording.duration), recording.status)),
                row![action("Transcribe", Message::RunTranscription, !self.busy()), secondary("View recording", Message::Navigate(Page::Library), true)].spacing(12),
            ].spacing(12)).padding(24).width(Fill).style(style::surface));
        }
        column![heading("Import"), container(scroll(content)).center_x(Fill).center_y(Fill)].spacing(24).height(Fill).into()
    }

    fn library_view(&self) -> Element<'_, Message> {
        let header = row![heading("Library"), spacer(), quiet("Open folder", Message::OpenFolder, true)].align_y(Alignment::Center);
        if self.items.is_empty() && self.search.is_empty() && self.selected.is_none() {
            let empty = column![icon(Icon::Library, 32), text("No recordings yet").size(20), row![
                secondary("Record", Message::Navigate(Page::Record), true),
                secondary("Import file", Message::Navigate(Page::Import), true),
            ].spacing(12)].spacing(20).align_x(Alignment::Center);
            return column![header, container(empty).center_x(Fill).center_y(Fill)].spacing(24).height(Fill).into();
        }
        let search = text_input("Search recordings", &self.search).on_input(Message::Search).padding(11).size(13).style(style::field);
        let mut recordings = Column::new().spacing(4);
        if self.items.is_empty() { recordings = recordings.push(container(muted("No matches")).padding(16)); }
        for item in &self.items {
            let active = self.selected.as_ref().is_some_and(|selected| selected.id == item.id);
            let date = chrono::DateTime::parse_from_rfc3339(&item.created_at).map(|date| date.format("%d %b %Y").to_string()).unwrap_or_default();
            let mut entry = column![
                label(&item.title),
                row![muted(date).size(12), spacer(), muted(timestamp(item.duration)).size(12)],
                muted(&item.status).size(12),
            ].spacing(8);
            if !self.search.is_empty() && !item.preview.is_empty() { entry = entry.push(muted(&item.preview)); }
            recordings = recordings.push(button(entry).width(Fill).padding(14).on_press(Message::Select(item.id.clone()))
                .style(move |theme, status| style::selectable(theme, active, status)));
        }
        let list = column![search, scroll(recordings).height(Fill)].spacing(14).width(if self.width < 1100.0 { 210 } else { 250 }).height(Fill);
        let detail = match &self.selected {
            Some(recording) => self.detail_view(recording),
            None => container(muted("Select a recording")).padding(24).center_x(Fill).center_y(Fill).style(style::surface).into(),
        };
        column![header, row![list, detail].spacing(20).height(Fill)].spacing(24).height(Fill).into()
    }

    fn detail_view<'a>(&'a self, recording: &'a Recording) -> Element<'a, Message> {
        let title = column![label("Title"), text_input("Recording title", &self.detail_title).on_input(Message::DetailTitle).size(17).padding(10).style(style::field)].spacing(6).width(Fill);
        let title = row![title, quiet("Save", Message::SaveRecording, !self.busy())].spacing(8).align_y(Alignment::End);
        let info = row![muted(timestamp(recording.duration)), muted(recording.language.as_deref().unwrap_or("Auto language")), spacer(), muted(recording.status.to_string())].spacing(12);
        let controls = row![action("Transcribe", Message::RunTranscription, !self.busy()), secondary("Play audio", Message::OpenAudio, true)].spacing(10);
        let mut tabs = Row::new().spacing(4);
        for (tab, name) in [(DetailTab::Transcript, "Transcript"), (DetailTab::Words, "Word timings"), (DetailTab::Notes, "Notes")] {
            let active = tab == self.detail_tab;
            tabs = tabs.push(button(text(name).size(13)).padding([9, 12]).style(move |theme, status| style::navigation(theme, active, status)).on_press(Message::Tab(tab)));
        }
        let body: Element<'a, Message> = match self.detail_tab {
            DetailTab::Transcript => column![
                text_input("Find in transcript", &self.transcript_filter).on_input(Message::TranscriptFilter).padding(10).size(13).style(style::field),
                self.transcript_rows(recording, "library-transcript", true),
            ].spacing(14).height(Fill).into(),
            DetailTab::Words => self.words_view(recording),
            DetailTab::Notes => self.notes_view(recording),
        };
        let export = row![
            pick_list(ExportFormat::ALL, Some(self.export_format), Message::ExportFormat).padding(9).text_size(12).style(style::select).menu_style(style::menu),
            quiet("Export", Message::Export, !self.dialog_open),
            spacer(),
            quiet("Remove", Message::RequestRemove, !self.busy()),
        ].spacing(6).align_y(Alignment::Center);
        container(column![title, info, controls, tabs, divider(), body, divider(), export].spacing(12).height(Fill))
            .padding(20).width(Fill).height(Fill).style(style::surface).into()
    }

    fn transcript_rows<'a>(&'a self, recording: &'a Recording, id: &'static str, filtering: bool) -> Element<'a, Message> {
        if recording.segments.is_empty() {
            return container(muted("Transcribe this recording to view the text.")).padding(20).center_x(Fill).center_y(Fill).into();
        }
        let filter = if filtering { self.transcript_filter.trim().to_lowercase() } else { String::new() };
        let mut rows = Column::new().spacing(24);
        let matches: Vec<_> = recording.segments.iter().filter(|segment| filter.is_empty() || segment.text.to_lowercase().contains(&filter)).collect();
        if matches.is_empty() { rows = rows.push(muted("No matching text.")); }
        let skip = if filtering { 0 } else { matches.len().saturating_sub(200) };
        if skip > 0 { rows = rows.push(muted("Earlier text is available in Library.")); }
        for segment in matches.into_iter().skip(skip) {
            let header = row![
                muted(timestamp(segment.start)).font(Font::MONOSPACE).size(12),
                text(segment.speaker.as_deref().unwrap_or("")).size(12).style(style::accent),
            ].spacing(14);
            rows = rows.push(column![header, text(&segment.text).size(16).line_height(1.5)].spacing(8));
        }
        let scroll = scroll(rows.padding([4, 0])).id(id).height(Fill);
        if filtering {
            column![row![muted(format!("{} segments", recording.segments.len())), spacer(), quiet("Copy text", Message::CopyTranscript, true)], scroll].spacing(8).height(Fill).into()
        } else { scroll.into() }
    }

    fn words_view<'a>(&'a self, recording: &'a Recording) -> Element<'a, Message> {
        let mut words = column![row![label("Word").width(Fill), label("Start").width(60), label("End").width(60), label("Speaker").width(90)].spacing(8)].spacing(14);
        let mut count = 0usize;
        let filter = self.transcript_filter.trim().to_lowercase();
        for word in recording.segments.iter().flat_map(|segment| &segment.words) {
            if !filter.is_empty() && !word.text.to_lowercase().contains(&filter) { continue; }
            words = words.push(row![
                text(&word.text).size(14).width(Fill),
                muted(format!("{:.2}s", word.start)).width(60),
                muted(format!("{:.2}s", word.end)).width(60),
                muted(word.speaker.as_deref().unwrap_or("Unassigned")).width(90),
            ].spacing(8));
            count += 1;
        }
        if count == 0 { words = words.push(muted(if filter.is_empty() { "Enable word timings in Settings, then transcribe." } else { "No matching words." })); }
        column![text_input("Find a word", &self.transcript_filter).on_input(Message::TranscriptFilter).padding(10).size(13).style(style::field), scroll(words).height(Fill)].spacing(16).height(Fill).into()
    }

    fn notes_view<'a>(&'a self, recording: &'a Recording) -> Element<'a, Message> {
        let prompt = text_editor(&self.recording_prompt).placeholder("Use the default prompt, or add instructions.").on_action(Message::RecordingPrompt).height(100).padding(12).size(13).style(style::editor);
        let mut notes = column![label("Instructions (optional)"), prompt, row![
            action("Generate notes", Message::RequestSummary, !self.busy() && !recording.segments.is_empty()),
            quiet("Copy notes", Message::CopyNotes, !recording.summary.is_empty()),
        ].spacing(10)].spacing(14);
        if recording.summary.is_empty() {
            notes = notes.push(muted("No notes yet."));
        } else {
            notes = notes.push(text(&recording.summary).size(15).line_height(1.5));
        }
        scroll(notes).height(Fill).into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let settings = &self.draft.settings;
        let header = row![heading("Settings"), spacer(), action(if self.saving { "Saving..." } else { "Save settings" }, Message::SaveSettings, !self.busy())].align_y(Alignment::Center);
        let mut tabs = Row::new().spacing(4);
        for (tab, name) in [(SettingsTab::General, "General"), (SettingsTab::Audio, "Audio"), (SettingsTab::Transcription, "Transcription"), (SettingsTab::Speakers, "Speakers"), (SettingsTab::Notes, "Notes")] {
            let active = self.settings_tab == tab;
            tabs = tabs.push(button(text(name).size(14)).padding([10, 14]).style(move |theme, status| style::navigation(theme, active, status)).on_press(Message::SettingsTab(tab)));
        }
        let fields = match self.settings_tab {
            SettingsTab::General => column![
                self.settings_section("Appearance", column![row![
                    label("Theme").width(Fill),
                    pick_list(Appearance::ALL, Some(settings.appearance), Message::Appearance).width(180).padding(12).text_size(14).style(style::select).menu_style(style::menu),
                ].align_y(Alignment::Center)]),
                divider(),
                self.settings_section("Storage", column![
                    self.path_field("Models", Field::ModelsDir),
                    self.path_field("Recordings", Field::RecordingsDir),
                    muted("New files use these folders. Existing recordings stay where they are."),
                ].spacing(20)),
            ].spacing(28),
            SettingsTab::Audio => {
                let mut sources = column![
                    row![self.toggle("Microphone", Toggle::Microphone, settings.microphone_enabled), spacer(), quiet("Refresh devices", Message::RefreshDevices, true)].align_y(Alignment::Center),
                    pick_list(self.microphone_labels.as_slice(), selected_label(&self.devices.microphones, &self.microphone_labels, settings.microphone_device.as_deref()), Message::Microphone)
                        .placeholder("Default microphone").padding(12).width(Fill).text_size(13).style(style::select).menu_style(style::menu),
                    vspace(4.0),
                    self.toggle("System audio", Toggle::System, settings.system_enabled),
                    pick_list(self.system_labels.as_slice(), selected_label(&self.devices.system, &self.system_labels, settings.system_device.as_deref()), Message::System)
                        .placeholder("Default system audio").padding(12).width(Fill).text_size(13).style(style::select).menu_style(style::menu),
                    muted("Both sources are mixed into one recording. Avoid selecting the same audio twice."),
                ].spacing(16);
                if let Some(note) = &self.devices.note { sources = sources.push(muted(note)); }
                if cfg!(target_os = "macos") { sources = sources.push(muted("System audio requires a loopback device. Allow microphone access when prompted.")); }
                column![self.settings_section("Audio sources", sources)]
            }
            SettingsTab::Transcription => {
                let model_path = std::path::PathBuf::from(&self.draft.models_dir).join(settings.model.filename());
                let mut options = column![
                    label("Model"),
                    pick_list(TranscriptionModel::ALL, Some(settings.model), Message::Model).width(Fill).padding(12).text_size(14).style(style::select).menu_style(style::menu),
                    row![muted(if crate::download::model_ready(settings.model, &model_path) { "Downloaded" } else { "Not downloaded" }), spacer(), secondary("Download models", Message::DownloadModels, !self.busy())].align_y(Alignment::Center).spacing(12),
                    self.field("CPU threads", Field::Threads, "4"),
                    self.toggle("GPU acceleration", Toggle::Gpu, settings.use_gpu),
                    muted(if settings.model.gpu_available() { "GPU acceleration is available." } else { "GPU acceleration is unavailable for this model in this build." }),
                    divider(),
                    self.toggle("Live transcription", Toggle::Live, settings.live_transcription),
                    self.field("Live interval (seconds)", Field::ChunkSeconds, "5 to 30"),
                ].spacing(18);
                if settings.model.is_parakeet() {
                    options = options.push(muted("Parakeet detects the language automatically and includes word timings. No separate alignment or speech detector is needed."));
                } else {
                    options = options
                        .push(muted(settings.model.speed_warning().unwrap_or_default()))
                        .push(self.field("Language", Field::Language, "Auto-detect, or en, ru, de..."))
                        .push(self.toggle("Detect speech (Silero)", Toggle::Vad, settings.vad_enabled))
                        .push(self.field("Speech threshold", Field::VadThreshold, "0.1 to 0.9"))
                        .push(self.toggle("Word timings (DTW)", Toggle::Alignment, settings.alignment_enabled));
                }
                column![self.settings_section("Transcription", options)]
            }
            SettingsTab::Speakers => column![self.settings_section("Speaker identification", column![
                self.toggle("Identify speakers", Toggle::Diarization, settings.diarization_enabled),
                self.field("Python interpreter", Field::Python, "python3 or an absolute path"),
                self.field("Speaker model", Field::DiarizationModel, "pyannote/speaker-diarization-community-1"),
                self.field("Hugging Face token", Field::HfToken, "Required for gated models"),
                row![self.field("Minimum speakers", Field::MinSpeakers, "Auto"), self.field("Maximum speakers", Field::MaxSpeakers, "Auto")].spacing(20),
                secondary("Check Python setup", Message::ProbePython, !self.busy()),
                muted("Requires Python 3.10+ and pyannote.audio. Accept the model's terms on Hugging Face before use."),
            ].spacing(20))],
            SettingsTab::Notes => column![self.settings_section("Notes", column![
                muted("Connect an OpenAI-compatible API. Transcripts are only sent when you generate notes."),
                self.field("API base URL", Field::ApiUrl, "Server address including /v1"),
                row![self.field("Model", Field::ApiModel, "Model identifier"), self.field("Output language", Field::SummaryLanguage, "Same as transcript")].spacing(20),
                self.field("API key", Field::ApiKey, "Optional for local servers"),
                label("Default instructions"),
                text_editor(&self.draft.prompt).on_action(Message::SettingsPrompt).height(150).padding(12).size(13).style(style::editor),
                row![self.field("Input characters per chunk", Field::SummaryChunkChars, "12000"), self.field("Maximum output tokens", Field::SummaryMaxTokens, "2048")].spacing(20),
                self.toggle("Save credentials in the OS keychain", Toggle::RememberSecrets, self.draft.remember),
                muted("Otherwise, credentials stay in memory. Use plain HTTP only for trusted local servers."),
            ].spacing(20))],
        };
        column![header, tabs, divider(), scroll(fields.max_width(800).padding([8, 0])).height(Fill)].spacing(24).height(Fill).into()
    }

    fn settings_section<'a>(&self, title: &'a str, fields: Column<'a, Message>) -> Element<'a, Message> {
        column![text(title).size(19).font(Font { weight: iced::font::Weight::Medium, ..Font::DEFAULT }), fields].spacing(24).width(Fill).into()
    }

    fn toggle<'a>(&self, caption: &'a str, toggle: Toggle, value: bool) -> Element<'a, Message> {
        checkbox(value).label(caption).on_toggle(move |value| Message::Toggle(toggle, value)).text_size(14).size(18).into()
    }

    fn field<'a>(&'a self, caption: &'a str, field: Field, placeholder: &'a str) -> Element<'a, Message> {
        let input = text_input(placeholder, self.draft.value(field)).on_input(move |value| Message::Setting(field, value))
            .padding(12).size(13).style(style::field).secure(matches!(field, Field::ApiKey | Field::HfToken));
        column![label(caption), input].spacing(8).width(Fill).into()
    }

    fn path_field<'a>(&'a self, caption: &'a str, field: Field) -> Element<'a, Message> {
        row![self.field(caption, field, "Absolute folder path"), secondary("Browse", Message::PickDirectory(field), !self.dialog_open)].spacing(12).align_y(Alignment::End).into()
    }
}
