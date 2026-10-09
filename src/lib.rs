pub mod asr;
pub mod audio;
pub mod capture;
pub mod diarize;
pub mod domain;
pub mod download;
pub mod engine;
mod http;
pub mod settings;
pub mod storage;
pub mod summary;
pub mod ui;
pub mod vad;

pub const SAMPLE_RATE: u32 = 16_000;
