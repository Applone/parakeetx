use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use parakeet_rs::{ExecutionConfig, ExecutionProvider, ParakeetTDT, TimestampMode, Transcriber as _};
use whisper_rs::{DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{
    SAMPLE_RATE,
    domain::{Segment, TranscriptionModel, WhisperModel, Word},
};

struct WhisperTranscriber {
    context: WhisperContext,
    alignment: bool,
}

enum Backend {
    Parakeet(Box<ParakeetTDT>),
    Whisper(WhisperTranscriber),
}

pub struct Transcriber {
    backend: Backend,
    model: TranscriptionModel,
    alignment: bool,
}

impl Transcriber {
    pub fn load(path: &Path, model: impl Into<TranscriptionModel>, use_gpu: bool, alignment: bool) -> Result<Self> {
        Self::load_with_threads(path, model.into(), use_gpu, alignment, 4)
    }

    pub fn load_with_threads(path: &Path, model: TranscriptionModel, use_gpu: bool, alignment: bool, threads: usize) -> Result<Self> {
        let backend = match model {
            TranscriptionModel::ParakeetTdt06bV3 => {
                ensure!(crate::download::model_ready(model, path), "Download the complete {model} weights in Settings first");
                let provider = ExecutionProvider::Cpu;
                #[cfg(feature = "cuda")]
                let provider = if use_gpu { ExecutionProvider::Cuda } else { provider };
                #[cfg(not(feature = "cuda"))]
                let _ = use_gpu;
                let encoder = ExecutionConfig::new().with_execution_provider(provider).with_intra_threads(threads.clamp(1, 128));
                // The joint graph runs for every token; a single CPU thread avoids scheduling overhead.
                let joint = ExecutionConfig::new().with_intra_threads(1);
                Backend::Parakeet(Box::new(ParakeetTDT::from_pretrained_with_joint_config(path, Some(encoder), Some(joint))
                    .context("Cannot load Parakeet. Re-download its weights in Settings.")?))
            }
            TranscriptionModel::Whisper(model) => Backend::Whisper(WhisperTranscriber::load(path, model, use_gpu, alignment)?),
        };
        Ok(Self { backend, model, alignment: model.is_parakeet() || alignment })
    }

    pub fn model(&self) -> TranscriptionModel { self.model }

    pub fn alignment_enabled(&self) -> bool { self.alignment }

    pub fn run(&mut self, samples: &[f32], options: &TranscribeOptions, cancel: &Arc<AtomicBool>, progress: impl Fn(f32) + Send + 'static) -> Result<Transcript> {
        ensure!(samples.len() >= SAMPLE_RATE as usize / 10, "Audio is too short to transcribe");
        ensure!(!cancel.load(Ordering::Relaxed), "Transcription cancelled");
        ensure!(options.offset.is_finite() && options.offset >= 0.0, "Invalid transcript offset");
        match &mut self.backend {
            Backend::Whisper(backend) => backend.run(samples, options, cancel, progress),
            Backend::Parakeet(backend) => {
                progress(0.0);
                let result = backend.transcribe_samples(samples.to_vec(), SAMPLE_RATE, 1, Some(TimestampMode::Tokens))
                    .context("Parakeet failed to process the audio")?;
                ensure!(!cancel.load(Ordering::Relaxed), "Transcription cancelled");
                let transcript = parakeet_transcript(result, options.offset, samples.len() as f64 / f64::from(SAMPLE_RATE))?;
                progress(1.0);
                Ok(transcript)
            }
        }
    }
}

fn parakeet_transcript(result: parakeet_rs::TranscriptionResult, offset: f64, duration: f64) -> Result<Transcript> {
    let mut words: Vec<Word> = Vec::new();
    let mut boundary = true;
    for token in result.tokens {
        ensure!(token.start.is_finite() && token.end.is_finite(), "Parakeet returned invalid word timestamps");
        // Group native SentencePiece timestamps without the library's repeated-word deduplication.
        // Punctuation and contractions stay attached to their word.
        for character in token.text.chars() {
            if character.is_whitespace() || character == '▁' {
                boundary = true;
                continue;
            }
            if boundary || words.is_empty() {
                words.push(Word {
                    text: String::new(), start: offset + f64::from(token.start), end: offset + f64::from(token.end),
                    // This decoder does not return probabilities; zero represents unavailable confidence.
                    confidence: 0.0, speaker: None,
                });
            }
            let word = words.last_mut().expect("word was created");
            word.text.push(character);
            word.end = word.end.max(offset + f64::from(token.end));
            boundary = false;
        }
    }
    sanitize_words(&mut words, offset, offset + duration);
    let mut segments = segments_from_words(words);
    if segments.is_empty() && !result.text.trim().is_empty() {
        segments.push(Segment { start: offset, end: offset + duration, text: result.text.trim().into(), words: Vec::new(), speaker: None });
    }
    // Parakeet recognizes languages automatically but its ONNX decoder does not expose a language ID.
    Ok(Transcript { segments, language: None })
}

pub(crate) fn segments_from_words(words: Vec<Word>) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut pending: Vec<Word> = Vec::new();
    for word in words {
        if pending.first().is_some_and(|first| word.end - first.start > 12.0)
            || pending.last().is_some_and(|last| word.start - last.end > 1.0)
            || pending.len() >= 40 {
            segments.push(segment_from_words(std::mem::take(&mut pending)));
        }
        let sentence_end = word.text.ends_with(['.', '?', '!']);
        pending.push(word);
        if sentence_end { segments.push(segment_from_words(std::mem::take(&mut pending))); }
    }
    if !pending.is_empty() { segments.push(segment_from_words(pending)); }
    segments
}

fn segment_from_words(words: Vec<Word>) -> Segment {
    Segment {
        start: words.first().expect("nonempty words").start,
        end: words.iter().map(|word| word.end).fold(0.0, f64::max),
        text: words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" "),
        words, speaker: None,
    }
}

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    pub language: String,
    pub threads: usize,
    pub offset: f64,
    pub context_prompt: String,
    pub single_pass: bool,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self { language: String::new(), threads: 4, offset: 0.0, context_prompt: String::new(), single_pass: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    pub language: Option<String>,
}

impl WhisperTranscriber {
    pub fn load(path: &Path, model: WhisperModel, use_gpu: bool, alignment: bool) -> Result<Self> {
        ensure!(path.is_file(), "Download the {model} weights in Settings first");
        whisper_rs::install_logging_hooks();
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(use_gpu);
        parameters.flash_attn(false);
        if alignment {
            parameters.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: preset(model) }, dtw_mem_size: 256 * 1024 * 1024 });
        }
        let path = path.to_str().context("Model path contains invalid characters")?;
        let context = WhisperContext::new_with_params(path, parameters)
            .with_context(|| format!("Cannot load {model}. Re-download the weights in Settings."))?;
        Ok(Self { context, alignment })
    }

    pub fn run(&self, samples: &[f32], options: &TranscribeOptions, cancel: &Arc<AtomicBool>, progress: impl Fn(f32) + Send + 'static) -> Result<Transcript> {
        ensure!(samples.len() >= SAMPLE_RATE as usize / 10, "Audio is too short to transcribe");
        ensure!(!cancel.load(Ordering::Relaxed), "Transcription cancelled");
        ensure!(options.offset.is_finite() && options.offset >= 0.0, "Invalid transcript offset");
        let mut state = self.context.create_state().context("Cannot allocate a Whisper decoding state")?;
        let mut parameters = if options.single_pass {
            FullParams::new(SamplingStrategy::Greedy { best_of: 1 })
        } else {
            FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 })
        };
        parameters.set_n_threads(options.threads.clamp(1, 128) as i32);
        parameters.set_translate(false);
        parameters.set_no_context(true);
        parameters.set_print_special(false);
        parameters.set_print_progress(false);
        parameters.set_print_realtime(false);
        parameters.set_print_timestamps(false);
        parameters.set_suppress_blank(true);
        parameters.set_token_timestamps(self.alignment);
        parameters.set_split_on_word(self.alignment);
        parameters.set_max_len(if self.alignment { 90 } else { 0 });
        parameters.set_temperature(0.0);
        parameters.set_temperature_inc(0.2);
        parameters.set_no_speech_thold(0.6);
        parameters.set_detect_language(false);
        if options.language.is_empty() {
            parameters.set_language(None);
        } else {
            ensure!(whisper_rs::get_lang_id(&options.language).is_some(), "Unknown language code: {}", options.language);
            parameters.set_language(Some(&options.language));
        }
        let prompt = sanitize_prompt(&options.context_prompt);
        if !prompt.is_empty() {
            parameters.set_initial_prompt(&prompt);
        }
        unsafe {
            parameters.set_abort_callback(Some(abort_requested));
            parameters.set_abort_callback_user_data(Arc::as_ptr(cancel).cast_mut().cast());
        }
        let duration = samples.len() as f64 / f64::from(SAMPLE_RATE);
        let mut padded = Vec::new();
        let input = if samples.len() < SAMPLE_RATE as usize {
            padded.extend_from_slice(samples);
            padded.resize(SAMPLE_RATE as usize, 0.0);
            &padded
        } else {
            samples
        };
        progress(0.0);
        state.full(parameters, input).map_err(|error| match cancel.load(Ordering::Relaxed) {
            true => anyhow::anyhow!("Transcription cancelled"),
            false => anyhow::anyhow!("Whisper failed to process the audio: {error}"),
        })?;
        ensure!(!cancel.load(Ordering::Relaxed), "Transcription cancelled");
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment.to_str_lossy()?.trim().to_owned();
            let start = options.offset + centiseconds(segment.start_timestamp()).min(duration);
            let end = options.offset + centiseconds(segment.end_timestamp()).min(duration);
            if text.is_empty() || start >= options.offset + duration {
                continue;
            }
            let mut words = Vec::new();
            if self.alignment {
                let mut pieces = Vec::new();
                for index in 0..segment.n_tokens() {
                    let Some(token) = segment.get_token(index) else { continue };
                    let data = token.token_data();
                    if data.id >= self.context.token_eot() {
                        continue;
                    }
                    let anchor = if data.t_dtw >= 0 { data.t_dtw } else { data.t0 };
                    pieces.push((token.to_bytes()?.to_vec(), options.offset + centiseconds(anchor), options.offset + centiseconds(data.t1), data.p));
                }
                words = aligned_words(&pieces, start, end.max(start));
            }
            segments.push(Segment { start, end: end.max(start), text, words, speaker: None });
        }
        let language = if options.language.is_empty() {
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_owned)
        } else {
            Some(options.language.clone())
        };
        progress(1.0);
        Ok(Transcript { segments, language })
    }
}

unsafe extern "C" fn abort_requested(data: *mut std::ffi::c_void) -> bool {
    if data.is_null() {
        return false;
    }
    unsafe { &*data.cast::<AtomicBool>() }.load(Ordering::Relaxed)
}

fn aligned_words(pieces: &[(Vec<u8>, f64, f64, f32)], start: f64, end: f64) -> Vec<Word> {
    let mut words = Vec::new();
    let mut pending = Vec::new();
    let mut pending_start = start;
    let mut pending_end = start;
    let mut confidence = 1.0f32;
    for (index, (bytes, anchor, fallback_end, probability)) in pieces.iter().enumerate() {
        if bytes.first().is_some_and(u8::is_ascii_whitespace) && !pending.is_empty() && std::str::from_utf8(&pending).is_ok() {
            push_word(&mut words, &String::from_utf8_lossy(&pending), pending_start, pending_end, confidence);
            pending.clear();
            confidence = 1.0;
        }
        if pending.is_empty() {
            pending_start = *anchor;
        }
        let next = pieces.get(index + 1).map(|piece| piece.1);
        pending_end = next.filter(|next| *next >= *anchor).unwrap_or(*fallback_end).max(*anchor);
        confidence = confidence.min(*probability);
        pending.extend_from_slice(bytes);
    }
    push_word(&mut words, &String::from_utf8_lossy(&pending), pending_start, pending_end, confidence);
    sanitize_words(&mut words, start, end);
    words
}

fn centiseconds(value: i64) -> f64 {
    value.max(0) as f64 / 100.0
}

fn push_word(words: &mut Vec<Word>, text: &str, start: f64, end: f64, confidence: f32) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    words.push(Word { text: trimmed.to_owned(), start, end: end.max(start), confidence: confidence.clamp(0.0, 1.0), speaker: None });
}

fn sanitize_words(words: &mut [Word], start: f64, end: f64) {
    let mut previous = start;
    for word in words.iter_mut() {
        if !word.start.is_finite() || word.start < previous {
            word.start = previous;
        }
        if !word.end.is_finite() || word.end < word.start {
            word.end = word.start;
        }
        word.start = word.start.clamp(start, end);
        word.end = word.end.clamp(word.start, end);
        previous = word.start;
    }
}

pub fn sanitize_prompt(prompt: &str) -> String {
    let cleaned: String = prompt.chars().filter(|character| *character != '\0' && (!character.is_control() || *character == ' ')).collect();
    let cleaned = cleaned.trim();
    match cleaned.char_indices().nth(880) {
        Some((index, _)) => cleaned[..index].to_owned(),
        None => cleaned.to_owned(),
    }
}

pub fn preset(model: WhisperModel) -> DtwModelPreset {
    match model {
        WhisperModel::Tiny => DtwModelPreset::Tiny,
        WhisperModel::Base => DtwModelPreset::Base,
        WhisperModel::Small => DtwModelPreset::Small,
        WhisperModel::Medium => DtwModelPreset::Medium,
        WhisperModel::LargeV2 => DtwModelPreset::LargeV2,
        WhisperModel::LargeV3 => DtwModelPreset::LargeV3,
    }
}

pub fn merge(target: &mut Vec<Segment>, incoming: Vec<Segment>) {
    for segment in incoming {
        if segment.text.trim().is_empty() {
            continue;
        }
        let duplicate = target.iter().any(|existing| {
            (existing.start - segment.start).abs() < 0.35 && existing.text.trim() == segment.text.trim()
        });
        if !duplicate {
            target.push(segment);
        }
    }
    target.sort_by(|first, second| first.start.total_cmp(&second.start));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parakeet_result(tokens: &[(&str, f32, f32)]) -> parakeet_rs::TranscriptionResult {
        parakeet_rs::TranscriptionResult {
            text: tokens.iter().map(|token| token.0).collect::<String>().trim().into(),
            tokens: tokens.iter().map(|(text, start, end)| parakeet_rs::TimedToken { text: (*text).into(), start: *start, end: *end }).collect(),
        }
    }

    #[test]
    fn parakeet_preserves_repetition_punctuation_and_subword_timings() {
        let result = parakeet_result(&[(" Very", 0.0, 0.2), (" very", 0.2, 0.4), (" use", 0.4, 0.6), ("ful", 0.6, 0.8), (".", 0.8, 0.9), (" Isn", 1.0, 1.1), ("'t", 1.1, 1.2), (" it", 1.2, 1.4), ("?", 1.4, 1.5)]);
        let transcript = parakeet_transcript(result, 12.0, 2.0).unwrap();
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].text, "Very very useful.");
        assert_eq!(transcript.segments[0].words.len(), 3);
        assert_eq!(transcript.segments[1].text, "Isn't it?");
        assert_eq!(transcript.segments[0].start, 12.0);
        assert!((transcript.segments[0].end - 12.9).abs() < 1e-6);
        assert!(transcript.language.is_none());
    }

    #[test]
    fn parakeet_clamps_native_word_times_to_the_recording() {
        let result = parakeet_result(&[(" first", -1.0, 0.5), (" second", 0.1, 1.0), (" last", 2.0, 8.0)]);
        let transcript = parakeet_transcript(result, 5.0, 2.0).unwrap();
        let words: Vec<_> = transcript.segments.iter().flat_map(|segment| &segment.words).collect();
        assert!(words.iter().all(|word| word.start >= 5.0 && word.end <= 7.0 && word.end >= word.start));
        assert!(words.windows(2).all(|pair| pair[0].start <= pair[1].start));
        assert!(parakeet_transcript(parakeet_result(&[(" word", f32::NAN, 1.0)]), 0.0, 2.0).is_err());
    }

    #[test]
    fn parakeet_handles_silence_and_bounds_long_subtitle_segments() {
        assert!(parakeet_transcript(parakeet_result(&[]), 0.0, 2.0).unwrap().segments.is_empty());
        let result = parakeet_result(&[(" one", 0.0, 1.0), (" two", 1.0, 2.0), (" three", 12.0, 13.0)]);
        let transcript = parakeet_transcript(result, 0.0, 15.0).unwrap();
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[1].text, "three");
    }

    #[test]
    fn incomplete_parakeet_bundle_reports_a_download_instruction() {
        let directory = tempfile::tempdir().unwrap();
        let error = Transcriber::load(directory.path(), TranscriptionModel::default(), false, true).err().unwrap();
        assert!(error.to_string().contains("Download the complete"));
    }

    #[test]
    fn alignment_reassembles_split_utf8_tokens() {
        let pieces = vec![(vec![b' ', 0xe6], 0.0, 0.1, 0.9), (vec![0x97, 0xa5], 0.1, 0.2, 0.8), (b" word".to_vec(), 0.2, 0.5, 0.7)];
        let words = aligned_words(&pieces, 0.0, 0.5);
        assert_eq!(words[0].text, "日");
        assert_eq!(words[1].text, "word");
        assert_eq!(words[0].end, 0.2);
    }

    #[test]
    fn prompts_are_truncated_and_stripped_of_control_characters() {
        assert_eq!(sanitize_prompt("  hello\0\nworld\t "), "helloworld");
        let long = "словарь ".repeat(400);
        let sanitized = sanitize_prompt(&long);
        assert_eq!(sanitized.chars().count(), 880);
        assert!(sanitized.is_char_boundary(sanitized.len()));
    }

    #[test]
    fn word_timings_stay_inside_segment_bounds() {
        let mut words = vec![
            Word { text: "один".into(), start: -5.0, end: f64::NAN, confidence: 0.9, speaker: None },
            Word { text: "два".into(), start: 2.0, end: 99.0, confidence: 0.4, speaker: None },
        ];
        sanitize_words(&mut words, 1.0, 3.0);
        assert!(words.iter().all(|word| word.start >= 1.0 && word.end <= 3.0 && word.end >= word.start));
    }

    #[test]
    fn merging_live_chunks_drops_duplicates_and_sorts() {
        let mut collected = vec![Segment { start: 5.0, end: 6.0, text: "later".into(), words: vec![], speaker: None }];
        merge(&mut collected, vec![
            Segment { start: 5.1, end: 6.0, text: "later".into(), words: vec![], speaker: None },
            Segment { start: 0.0, end: 1.0, text: "  ".into(), words: vec![], speaker: None },
            Segment { start: 1.0, end: 2.0, text: "earlier".into(), words: vec![], speaker: None },
        ]);
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].text, "earlier");
    }
}
