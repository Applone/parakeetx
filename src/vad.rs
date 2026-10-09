use std::path::Path;

use anyhow::{Context, Result, ensure};
use ort::{session::{Session, builder::GraphOptimizationLevel}, value::Tensor};

use crate::SAMPLE_RATE;

pub const WINDOW: usize = 512;
pub const CONTEXT: usize = 64;
const STATE_SIZE: usize = 2 * 128;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speech {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct VadOptions {
    pub threshold: f32,
    pub minimum_speech: f64,
    pub minimum_silence: f64,
    pub padding: f64,
}

impl Default for VadOptions {
    fn default() -> Self {
        Self { threshold: 0.5, minimum_speech: 0.25, minimum_silence: 0.6, padding: 0.2 }
    }
}

pub struct Vad {
    session: Session,
    state: Vec<f32>,
    context: Vec<f32>,
}

impl Vad {
pub fn load(path: &Path) -> Result<Self> {
        ensure!(path.is_file(), "Download the Silero voice-activity model in Settings first");
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3).map_err(|error| anyhow::anyhow!(error.to_string()))?
            .with_intra_threads(1).map_err(|error| anyhow::anyhow!(error.to_string()))?
            .commit_from_file(path)
            .context("Cannot load the Silero voice-activity model. Download it again in Settings.")?;
        ensure!(["input", "state", "sr"].iter().all(|name| session.inputs().iter().any(|input| input.name() == *name)), "Unexpected voice-activity model inputs; download the model again");
        ensure!(["output", "stateN"].iter().all(|name| session.outputs().iter().any(|output| output.name() == *name)), "Unexpected voice-activity model outputs; download the model again");
        Ok(Self { session, state: vec![0.0; STATE_SIZE], context: vec![0.0; CONTEXT] })
    }

    pub fn reset(&mut self) {
        self.state.iter_mut().for_each(|value| *value = 0.0);
        self.context.iter_mut().for_each(|value| *value = 0.0);
    }

    pub fn probability(&mut self, frame: &[f32]) -> Result<f32> {
        ensure!(frame.len() == WINDOW, "Voice-activity frames must contain {WINDOW} samples");
        let mut input = Vec::with_capacity(CONTEXT + WINDOW);
        input.extend_from_slice(&self.context);
        input.extend(frame.iter().map(|sample| if sample.is_finite() { *sample } else { 0.0 }));
        let outputs = self.session.run(ort::inputs![
            "input" => Tensor::from_array(([1usize, input.len()], input.clone()))?,
            "state" => Tensor::from_array(([2usize, 1, 128], self.state.clone()))?,
            "sr" => Tensor::from_array(([1usize], vec![i64::from(SAMPLE_RATE)]))?,
        ])?;
        let (_, updated) = outputs["stateN"].try_extract_tensor::<f32>()?;
        ensure!(updated.len() == STATE_SIZE, "Voice-activity model returned an unexpected state");
        self.state.copy_from_slice(updated);
        self.context.copy_from_slice(&input[input.len() - CONTEXT..]);
        let (_, probability) = outputs["output"].try_extract_tensor::<f32>()?;
        let probability = probability.first().copied().context("Voice-activity model returned no probability")?;
        ensure!(probability.is_finite(), "Voice-activity model returned an invalid probability");
        Ok(probability.clamp(0.0, 1.0))
    }

    pub fn segments(&mut self, samples: &[f32], options: VadOptions) -> Result<Vec<Speech>> {
        self.reset();
        let mut probabilities = Vec::with_capacity(samples.len() / WINDOW + 1);
        for index in (0..samples.len()).step_by(WINDOW) {
            let end = (index + WINDOW).min(samples.len());
            let mut frame = samples[index..end].to_vec();
            frame.resize(WINDOW, 0.0);
            probabilities.push(self.probability(&frame)?);
        }
        Ok(collect_speech(&probabilities, samples.len(), options))
    }
}

pub fn collect_speech(probabilities: &[f32], total_samples: usize, options: VadOptions) -> Vec<Speech> {
    let frame_duration = WINDOW as f64 / f64::from(SAMPLE_RATE);
    let total = total_samples as f64 / f64::from(SAMPLE_RATE);
    let mut segments: Vec<Speech> = Vec::new();
    let mut start = None;
    let mut silence = 0.0;
    for (index, probability) in probabilities.iter().enumerate() {
        let time = index as f64 * frame_duration;
        if *probability >= options.threshold {
            silence = 0.0;
            start.get_or_insert(time);
        } else if *probability >= (options.threshold - 0.15).max(0.0) {
            silence = 0.0;
        } else if let Some(begin) = start {
            silence += frame_duration;
            if silence >= options.minimum_silence {
                push_segment(&mut segments, begin, time - silence + frame_duration, total, options);
                start = None;
                silence = 0.0;
            }
        }
    }
    if let Some(begin) = start {
        push_segment(&mut segments, begin, (total - silence).max(begin), total, options);
    }
    segments
}

fn push_segment(segments: &mut Vec<Speech>, start: f64, end: f64, total: f64, options: VadOptions) {
    if end - start < options.minimum_speech {
        return;
    }
    let candidate = Speech { start: (start - options.padding).max(0.0), end: (end + options.padding).min(total) };
    match segments.last_mut() {
        Some(previous) if candidate.start <= previous.end => previous.end = candidate.end.max(previous.end),
        _ => segments.push(candidate),
    }
}

pub fn merge_for_asr(segments: &[Speech], maximum: f64, total: f64) -> Vec<Speech> {
    if !maximum.is_finite() || maximum <= 0.0 || !total.is_finite() || total <= 0.0 { return Vec::new(); }
    if segments.is_empty() {
        return if total > 0.0 { vec![Speech { start: 0.0, end: total }] } else { Vec::new() };
    }
    let mut chunks: Vec<Speech> = Vec::new();
    for segment in segments {
        if !segment.start.is_finite() || !segment.end.is_finite() || segment.end <= segment.start { continue; }
        let mut current = Speech { start: segment.start.max(0.0), end: segment.end.min(total) };
        if current.end <= current.start { continue; }
        while current.end - current.start > maximum {
            let split = Speech { start: current.start, end: current.start + maximum };
            chunks.push(split);
            current.start += maximum;
        }
        match chunks.last_mut() {
            Some(previous) if current.end - previous.start <= maximum => previous.end = current.end,
            _ => chunks.push(current),
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_regions_merge_with_padding_and_minimum_lengths() {
        let options = VadOptions { threshold: 0.5, minimum_speech: 0.1, minimum_silence: 0.2, padding: 0.1 };
        let mut probabilities = vec![0.0f32; 100];
        probabilities[10..30].fill(0.9);
        probabilities[33..35].fill(0.9);
        probabilities[70..90].fill(0.9);
        let segments = collect_speech(&probabilities, 100 * WINDOW, options);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].start < 0.33 && segments[0].end > 1.0);
        assert!(segments[1].start > 2.0);
        assert!(segments.iter().all(|segment| segment.start >= 0.0 && segment.end <= 100.0 * WINDOW as f64 / 16_000.0));
    }

    #[test]
    fn silence_produces_no_segments_and_fallback_covers_audio() {
        assert!(collect_speech(&[0.1; 50], 50 * WINDOW, VadOptions::default()).is_empty());
        assert_eq!(merge_for_asr(&[], 30.0, 12.0), vec![Speech { start: 0.0, end: 12.0 }]);
        assert!(merge_for_asr(&[], 30.0, 0.0).is_empty());
    }

    #[test]
    fn long_regions_split_below_the_window_limit() {
        let merged = merge_for_asr(&[Speech { start: 0.0, end: 95.0 }], 30.0, 95.0);
        assert_eq!(merged.len(), 4);
        assert!(merged.iter().all(|chunk| chunk.end - chunk.start <= 30.0 + 1e-9));
        assert!((merged.last().unwrap().end - 95.0).abs() < 1e-9);
    }
}
