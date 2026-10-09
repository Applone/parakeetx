use std::{fmt, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WhisperModel {
    Tiny,
    #[default]
    Base,
    Small,
    Medium,
    LargeV2,
    LargeV3,
}

impl WhisperModel {
    pub const ALL: [Self; 6] = [
        Self::Tiny,
        Self::Base,
        Self::Small,
        Self::Medium,
        Self::LargeV2,
        Self::LargeV3,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Base => "base",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::LargeV2 => "large-v2",
            Self::LargeV3 => "large-v3",
        }
    }

    pub fn filename(self) -> String {
        format!("ggml-{}.bin", self.slug())
    }

    pub fn size_label(self) -> &'static str {
        match self {
            Self::Tiny => "75 MiB",
            Self::Base => "142 MiB",
            Self::Small => "466 MiB",
            Self::Medium => "1.5 GiB",
            Self::LargeV2 | Self::LargeV3 => "2.9 GiB",
        }
    }

    pub fn sha1(self) -> &'static str {
        match self {
            Self::Tiny => "bd577a113a864445d4c299885e0cb97d4ba92b5f",
            Self::Base => "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
            Self::Small => "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
            Self::Medium => "fd9727b6e1217c2f614f9b698455c4ffd82463b4",
            Self::LargeV2 => "0f4c8e34f21cf1a914c59d8b3ce882345ad349d6",
            Self::LargeV3 => "ad82bf6a9043ceed055076d0fd39f5f186ff8062",
        }
    }
}

impl fmt::Display for WhisperModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Tiny => "Whisper Tiny",
            Self::Base => "Whisper Base",
            Self::Small => "Whisper Small",
            Self::Medium => "Whisper Medium",
            Self::LargeV2 => "Whisper Large v2",
            Self::LargeV3 => "Whisper Large v3",
        };
        write!(formatter, "{name} ({})", self.size_label())
    }
}

pub const WHISPER_SPEED_WARNING: &str = "OpenAI Whisper models are significantly slower than Parakeet and may transcribe slower than real time, especially with DTW word timings. NVIDIA Parakeet is recommended.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TranscriptionModel {
    #[default]
    ParakeetTdt06bV3,
    #[serde(untagged)]
    Whisper(WhisperModel),
}

impl TranscriptionModel {
    pub const ALL: [Self; 7] = [
        Self::ParakeetTdt06bV3,
        Self::Whisper(WhisperModel::Tiny),
        Self::Whisper(WhisperModel::Base),
        Self::Whisper(WhisperModel::Small),
        Self::Whisper(WhisperModel::Medium),
        Self::Whisper(WhisperModel::LargeV2),
        Self::Whisper(WhisperModel::LargeV3),
    ];

    pub fn is_parakeet(self) -> bool {
        matches!(self, Self::ParakeetTdt06bV3)
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::ParakeetTdt06bV3 => "parakeet-tdt-0.6b-v3",
            Self::Whisper(model) => model.slug(),
        }
    }

    pub fn filename(self) -> String {
        match self {
            Self::ParakeetTdt06bV3 => "parakeet-tdt-0.6b-v3-int8".into(),
            Self::Whisper(model) => model.filename(),
        }
    }

    pub fn speed_warning(self) -> Option<&'static str> {
        match self {
            Self::ParakeetTdt06bV3 => None,
            Self::Whisper(_) => Some(WHISPER_SPEED_WARNING),
        }
    }

    pub fn gpu_available(self) -> bool {
        match self {
            Self::ParakeetTdt06bV3 => cfg!(feature = "cuda"),
            Self::Whisper(_) => cfg!(any(feature = "cuda", feature = "metal", feature = "vulkan")),
        }
    }
}

impl From<WhisperModel> for TranscriptionModel {
    fn from(model: WhisperModel) -> Self {
        Self::Whisper(model)
    }
}

impl fmt::Display for TranscriptionModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParakeetTdt06bV3 => formatter.write_str("NVIDIA Parakeet TDT 0.6B v3 (recommended, 640 MiB)"),
            Self::Whisper(model) => write!(formatter, "{model} (slower)"),
        }
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordingStatus {
    Recording,
    Recorded,
    Transcribing,
    Ready,
    Partial,
    Failed,
}

impl fmt::Display for RecordingStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Recording => "Recording",
            Self::Recorded => "Not transcribed",
            Self::Transcribing => "Transcribing",
            Self::Ready => "Transcribed",
            Self::Partial => "Partial transcript",
            Self::Failed => "Needs attention",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f32,
    pub speaker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
    pub speaker: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineInfo {
    pub model: String,
    pub vad: bool,
    pub dtw: bool,
    #[serde(default)]
    pub word_timestamps: bool,
    pub diarization: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub audio_path: PathBuf,
    pub original_path: Option<PathBuf>,
    pub duration: f64,
    pub status: RecordingStatus,
    pub language: Option<String>,
    pub segments: Vec<Segment>,
    pub summary: String,
    pub summary_prompt: String,
    pub error: Option<String>,
    #[serde(default)]
    pub pipeline: PipelineInfo,
}

impl Recording {
    pub fn new(title: String, directory: &std::path::Path) -> Self {
        let id = Uuid::new_v4().to_string();
        Self {
            audio_path: directory.join(format!("{id}.wav")),
            id,
            title,
            created_at: Utc::now(),
            original_path: None,
            duration: 0.0,
            status: RecordingStatus::Recorded,
            language: None,
            segments: Vec::new(),
            summary: String::new(),
            summary_prompt: String::new(),
            error: None,
            pipeline: PipelineInfo::default(),
        }
    }

    pub fn transcript(&self) -> String {
        self.segments
            .iter()
            .map(|segment| match &segment.speaker {
                Some(speaker) => format!("{speaker}: {}", segment.text.trim()),
                None => segment.text.trim().to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct LibraryItem {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub duration: f64,
    pub status: String,
    pub preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Text,
    Markdown,
    Json,
    Srt,
    Vtt,
}

impl ExportFormat {
    pub const ALL: [Self; 5] = [Self::Text, Self::Markdown, Self::Json, Self::Srt, Self::Vtt];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }
}

impl fmt::Display for ExportFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Text => "Plain text",
            Self::Markdown => "Markdown notes",
            Self::Json => "JSON with word timestamps",
            Self::Srt => "SRT subtitles",
            Self::Vtt => "WebVTT subtitles",
        })
    }
}

pub fn timestamp(seconds: f64) -> String {
    let total = if seconds.is_finite() { seconds.max(0.0) as u64 } else { 0 };
    format!("{:02}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

pub fn export(recording: &Recording, format: ExportFormat) -> anyhow::Result<String> {
    anyhow::ensure!(recording.segments.iter().all(|segment| segment.start.is_finite() && segment.end.is_finite() && segment.start >= 0.0 && segment.end >= segment.start), "Transcript contains invalid timestamps");
    Ok(match format {
        ExportFormat::Text => recording.transcript(),
        ExportFormat::Json => serde_json::to_string_pretty(recording)?,
        ExportFormat::Markdown => {
            let mut result = format!("# {}\n\n", recording.title);
            if !recording.summary.is_empty() {
                result.push_str(&format!("## Notes\n\n{}\n\n", recording.summary));
            }
            result.push_str("## Transcript\n\n");
            for segment in &recording.segments {
                let speaker = segment.speaker.as_deref().unwrap_or("Transcript");
                result.push_str(&format!("**{} | {speaker}**\n\n{}\n\n", timestamp(segment.start), segment.text.trim()));
            }
            result
        }
        ExportFormat::Srt | ExportFormat::Vtt => {
            let separator = if format == ExportFormat::Srt { ',' } else { '.' };
            let mut result = if format == ExportFormat::Vtt { "WEBVTT\n\n".to_owned() } else { String::new() };
            for (index, segment) in recording.segments.iter().enumerate() {
                if format == ExportFormat::Srt {
                    result.push_str(&format!("{}\n", index + 1));
                }
                let subtitle_time = |seconds: f64| {
                    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
                    format!("{:02}:{:02}:{:02}{separator}{:03}", millis / 3_600_000, millis / 60_000 % 60, millis / 1000 % 60, millis % 1000)
                };
                result.push_str(&format!("{} --> {}\n", subtitle_time(segment.start), subtitle_time(segment.end)));
                let plain = match &segment.speaker {
                    Some(speaker) => format!("{speaker}: {}", segment.text),
                    None => segment.text.clone(),
                };
                let text = plain.replace(['\r', '\n'], " ").replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                result.push_str(text.trim());
                result.push_str("\n\n");
            }
            result
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_model_selections_round_trip_including_legacy_whisper_strings() {
        for model in TranscriptionModel::ALL {
            let serialized = serde_json::to_string(&model).unwrap();
            assert_eq!(serde_json::from_str::<TranscriptionModel>(&serialized).unwrap(), model);
        }
        for model in WhisperModel::ALL {
            let legacy = serde_json::to_string(&model).unwrap();
            assert_eq!(serde_json::from_str::<TranscriptionModel>(&legacy).unwrap(), model.into());
        }
        assert!(serde_json::from_str::<TranscriptionModel>("\"unknown-model\"").is_err());
    }

    #[test]
    fn older_pipeline_metadata_stays_readable() {
        let pipeline: PipelineInfo = serde_json::from_str(r#"{"model":"tiny","vad":true,"dtw":true,"diarization":false}"#).unwrap();
        assert!(pipeline.vad && pipeline.dtw);
        assert!(!pipeline.word_timestamps);
    }

    #[test]
    fn subtitle_rounding_carries_into_minutes() {
        let mut recording = Recording::new("Lesson".into(), std::path::Path::new("/tmp"));
        recording.segments.push(Segment { start: 59.9996, end: 61.0, text: "Hello".into(), words: vec![], speaker: Some("SPEAKER_00".into()) });
        assert!(export(&recording, ExportFormat::Srt).unwrap().contains("00:01:00,000 --> 00:01:01,000"));
        assert!(export(&recording, ExportFormat::Vtt).unwrap().starts_with("WEBVTT\n\n"));
    }

    #[test]
    fn subtitle_exports_escape_markup_and_line_breaks() {
        let mut recording = Recording::new("Example".into(), std::path::Path::new("/tmp"));
        recording.segments.push(Segment { start: 0.0, end: 1.0, text: "<script> & text\nnext".into(), words: vec![], speaker: Some("speaker\n1".into()) });
        let exported = export(&recording, ExportFormat::Vtt).unwrap();
        assert!(exported.contains("speaker 1: &lt;script&gt; &amp; text next"));
    }

    #[test]
    fn model_checksums_are_valid_sha1_strings() {
        for model in WhisperModel::ALL {
            assert_eq!(model.sha1().len(), 40);
            assert!(model.sha1().chars().all(|character| character.is_ascii_hexdigit()));
        }
    }
}
