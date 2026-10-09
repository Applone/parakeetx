//! Buffered Parakeet inference. Only the timestamp-owned prefix is persisted;
//! the right-hand hypothesis is replaced after each decode.
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

use anyhow::{Result, ensure};
use crossbeam_channel::{Receiver, Sender, RecvTimeoutError, bounded};

use crate::{SAMPLE_RATE, asr::Transcript, domain::Word};

pub(super) const UPDATE_SAMPLES: u64 = SAMPLE_RATE as u64;
const LOOKAHEAD: f64 = 2.0;
const LEFT_SAMPLES: u64 = 10 * SAMPLE_RATE as u64;
const MAX_SAMPLES: u64 = 28 * SAMPLE_RATE as u64;

#[derive(Debug, Default)]
pub(super) struct Update {
    pub committed: Vec<Word>,
    pub provisional: Vec<Word>,
}

#[derive(Clone, Copy, Default)]
struct Endpoint { samples: u64, finished: bool }

pub(super) struct Schedule {
    endpoint: Arc<Mutex<Endpoint>>,
    wake: Sender<()>,
}

pub(super) struct Input {
    endpoint: Arc<Mutex<Endpoint>>,
    wake: Receiver<()>,
}

pub(super) fn channel() -> (Schedule, Input) {
    let endpoint = Arc::new(Mutex::new(Endpoint::default()));
    let (sender, receiver) = bounded(1);
    (Schedule { endpoint: endpoint.clone(), wake: sender }, Input { endpoint, wake: receiver })
}

impl Schedule {
    /// Called only after the WAV header and samples have been flushed.
    pub fn submit(&self, samples: u64, finished: bool) {
        let mut endpoint = self.endpoint.lock().expect("live endpoint lock poisoned");
        endpoint.samples = endpoint.samples.max(samples);
        endpoint.finished |= finished;
        drop(endpoint);
        // The durable endpoint replaces pending work; a full wake channel is fine.
        let _ = self.wake.try_send(());
    }
}

impl Input {
    fn latest(&self) -> Endpoint { *self.endpoint.lock().expect("live endpoint lock poisoned") }

    fn wait(&self, cancel: &AtomicBool) -> Result<()> {
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "Live transcription cancelled");
            match self.wake.recv_timeout(Duration::from_millis(50)) {
                Ok(()) => return Ok(()),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => anyhow::bail!("Live audio input closed before finalization"),
            }
        }
    }
}

#[derive(Default)]
struct Commitment { frontier: f64, owns_prefix: bool }

impl Commitment {
    fn apply(&mut self, transcript: Transcript, end: u64, final_window: bool) -> Result<Update> {
        ensure!(transcript.segments.iter().all(|segment| segment.text.trim().is_empty() || !segment.words.is_empty()),
            "Parakeet returned text without word timestamps");
        let duration = end as f64 / f64::from(SAMPLE_RATE);
        let mut words: Vec<_> = transcript.segments.into_iter().flat_map(|segment| segment.words).collect();
        ensure!(words.iter().all(|word| word.start.is_finite() && word.end.is_finite()
            && word.start >= 0.0 && word.end >= word.start && word.end <= duration + 0.001),
            "Parakeet returned invalid live word timestamps");
        // Timestamp ownership excludes the retained left context, without text matching.
        words.retain(|word| !self.owns_prefix || (word.start + word.end) * 0.5 > self.frontier);
        words.sort_by(|first, second| first.start.total_cmp(&second.start));
        let barrier = if final_window { duration } else { (duration - LOOKAHEAD).max(0.0) };
        let count = words.iter().take_while(|word| final_window || word.end < barrier).count();
        let provisional = words.split_off(count);
        let committed_end = words.iter().map(|word| word.end).fold(self.frontier, f64::max);
        // Move through silence, but never across the start of an unresolved word.
        let silence_end = provisional.first().map_or(barrier, |word| barrier.min(word.start));
        self.frontier = committed_end.max(silence_end).max(self.frontier);
        self.owns_prefix |= self.frontier > 0.0 || !words.is_empty();
        Ok(Update { committed: words, provisional })
    }
}

/// The inference closure makes scheduling and commitment testable without models/audio devices.
pub(super) fn run(
    input: Input,
    cancel: &AtomicBool,
    mut infer: impl FnMut(u64, usize) -> Result<Transcript>,
    mut publish: impl FnMut(Update) -> Result<()>,
) -> Result<()> {
    let mut state = Commitment::default();
    let mut decoded = 0u64;
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "Live transcription cancelled");
        let endpoint = input.latest();
        if !endpoint.finished && endpoint.samples <= decoded {
            input.wait(cancel)?;
            continue;
        }
        let frontier = (state.frontier * f64::from(SAMPLE_RATE)).floor() as u64;
        let start = frontier.saturating_sub(LEFT_SAMPLES);
        let end = endpoint.samples.min(start + MAX_SAMPLES);
        let final_window = endpoint.finished && end == endpoint.samples;
        if end - start < u64::from(SAMPLE_RATE) / 10 {
            if final_window { publish(Update::default())?; return Ok(()); }
            decoded = end;
            continue;
        }
        let previous = state.frontier;
        let transcript = infer(start, (end - start) as usize)?;
        ensure!(!cancel.load(Ordering::Relaxed), "Live transcription cancelled");
        let update = state.apply(transcript, end, final_window)?;
        ensure!(end == endpoint.samples || state.frontier > previous,
            "Live transcription could not advance through buffered audio");
        publish(update)?;
        if final_window { return Ok(()); }
        decoded = end;
        // Re-read the endpoint after inference so stale pending updates are coalesced.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{asr, domain::Segment};

    fn samples(seconds: f64) -> u64 { (seconds * f64::from(SAMPLE_RATE)).round() as u64 }

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, confidence: 0.0, speaker: None }
    }

    fn transcript(words: Vec<Word>) -> Transcript {
        Transcript { segments: asr::segments_from_words(words), language: None }
    }

    #[test]
    fn commitment_preserves_repetition_and_replaces_the_crossing_tail() {
        let mut state = Commitment::default();
        let first = state.apply(transcript(vec![
            word("yes", 0.1, 0.4), word("yes", 0.5, 0.9),
            word("unfinished", 2.7, 3.2), word("later", 3.3, 3.7),
        ]), samples(5.0), false).unwrap();
        assert_eq!(first.committed.iter().map(|word| word.text.as_str()).collect::<Vec<_>>(), ["yes", "yes"]);
        assert_eq!(first.provisional.len(), 2);
        assert_eq!(state.frontier, 2.7);

        let second = state.apply(transcript(vec![
            // A revised prediction in the left context cannot change committed words.
            word("no", 0.12, 0.43), word("no", 0.52, 0.92),
            word("finished", 2.65, 3.15), word("replacement", 3.4, 4.2),
        ]), samples(6.0), false).unwrap();
        assert_eq!(second.committed, vec![word("finished", 2.65, 3.15)]);
        assert_eq!(second.provisional, vec![word("replacement", 3.4, 4.2)]);
        assert_eq!(first.committed[0].text, "yes");
        assert_eq!(state.frontier, 3.4);
    }

    #[test]
    fn exact_barrier_and_overlapping_words_stay_in_a_contiguous_tail() {
        let mut state = Commitment::default();
        let update = state.apply(transcript(vec![
            word("stable", 0.5, 1.0), word("at-barrier", 2.5, 3.0),
            word("overlapping", 2.6, 2.9),
        ]), samples(5.0), false).unwrap();
        assert_eq!(update.committed.len(), 1);
        assert_eq!(update.provisional.len(), 2);
        assert_eq!(state.frontier, 2.5);
        let finished = state.apply(transcript(vec![
            word("stable", 0.5, 1.0), word("at-barrier", 2.5, 3.0),
            word("overlapping", 2.6, 2.9),
        ]), samples(5.0), true).unwrap();
        assert_eq!(finished.committed.len(), 2);
        assert!(finished.provisional.is_empty());
        assert_eq!(state.frontier, 5.0);
    }

    #[test]
    fn silence_advances_without_retracting_the_frontier() {
        let mut state = Commitment::default();
        let empty = state.apply(transcript(Vec::new()), samples(1.0), false).unwrap();
        assert!(empty.committed.is_empty() && empty.provisional.is_empty());
        assert_eq!(state.frontier, 0.0);
        state.apply(transcript(Vec::new()), samples(20.0), false).unwrap();
        assert_eq!(state.frontier, 18.0);
        state.apply(transcript(Vec::new()), samples(20.0), true).unwrap();
        assert_eq!(state.frontier, 20.0);
    }

    #[test]
    fn a_word_at_recording_zero_is_not_mistaken_for_left_context() {
        let mut state = Commitment::default();
        let first = state.apply(transcript(vec![word("zero", 0.0, 0.0)]), samples(1.0), false).unwrap();
        assert_eq!(first.provisional.len(), 1);
        let stable = state.apply(transcript(vec![word("zero", 0.0, 0.0)]), samples(3.0), false).unwrap();
        assert_eq!(stable.committed.len(), 1);
        let last = state.apply(transcript(vec![word("zero", 0.0, 0.0)]), samples(3.0), true).unwrap();
        assert!(last.committed.is_empty());
    }

    #[test]
    fn unpositioned_text_and_invalid_timestamps_fail_without_advancing() {
        let mut state = Commitment::default();
        let fallback = Transcript { segments: vec![Segment {
            text: "unpositioned".into(), start: 0.0, end: 1.0, words: Vec::new(), speaker: None,
        }], language: None };
        assert!(state.apply(fallback, samples(5.0), false).is_err());
        for (start, end) in [(f64::NAN, 1.0), (0.0, f64::INFINITY), (-1.0, 1.0), (2.0, 1.0), (0.0, 6.0)] {
            assert!(state.apply(transcript(vec![word("invalid", start, end)]), samples(5.0), false).is_err());
        }
        assert_eq!(state.frontier, 0.0);
    }

    #[test]
    fn slow_inference_coalesces_endpoints_and_finishes_the_same_audio_again() {
        let (schedule, input) = channel();
        schedule.submit(samples(1.0), false);
        let mut windows = Vec::new();
        let mut updates = Vec::new();
        run(input, &AtomicBool::new(false), |start, length| {
            let end = start + length as u64;
            windows.push((start, end));
            match windows.len() {
                1 => {
                    for second in 2..=6 { schedule.submit(samples(f64::from(second)), false); }
                }
                2 => schedule.submit(end, true),
                _ => {}
            }
            Ok(transcript(vec![word("hello", 0.1, 0.7), word("tail", 5.2, 5.8)].into_iter()
                .filter(|word| word.end <= end as f64 / f64::from(SAMPLE_RATE)).collect()))
        }, |update| { updates.push(update); Ok(()) }).unwrap();
        assert_eq!(windows, [(0, samples(1.0)), (0, samples(6.0)), (0, samples(6.0))]);
        assert!(updates[0].committed.is_empty());
        assert_eq!(updates[0].provisional[0].text, "hello");
        assert_eq!(updates[1].committed[0].text, "hello");
        assert_eq!(updates[1].provisional[0].text, "tail");
        assert_eq!(updates[2].committed[0].text, "tail");
        assert!(updates[2].provisional.is_empty());
    }

    #[test]
    fn backlog_is_bounded_and_keeps_all_uncommitted_speech() {
        let (schedule, input) = channel();
        schedule.submit(samples(90.0), true);
        let expected: Vec<_> = (0..90).map(|second| word("repeat", f64::from(second) + 0.2, f64::from(second) + 0.7)).collect();
        let mut windows = Vec::new();
        let mut committed = Vec::new();
        run(input, &AtomicBool::new(false), |start, length| {
            windows.push((start, start + length as u64));
            assert!(length <= MAX_SAMPLES as usize);
            let begin = start as f64 / f64::from(SAMPLE_RATE);
            let end = (start + length as u64) as f64 / f64::from(SAMPLE_RATE);
            Ok(transcript(expected.iter().filter(|word| word.start >= begin && word.end <= end).cloned().collect()))
        }, |update| { committed.extend(update.committed); Ok(()) }).unwrap();
        assert!(windows.len() > 1);
        assert!(windows[1].0 < windows[0].1, "left and right audio context must overlap");
        assert_eq!(windows.last().unwrap().1, samples(90.0));
        assert_eq!(committed, expected);
    }

    #[test]
    fn short_stop_finalizes_without_waiting_for_an_update() {
        for duration in [0.0, 0.05, 0.5] {
            let (schedule, input) = channel();
            schedule.submit(samples(duration), true);
            let mut updates = Vec::new();
            let mut calls = 0;
            run(input, &AtomicBool::new(false), |_, _| {
                calls += 1;
                Ok(transcript(vec![word("short", 0.05, duration)]))
            }, |update| { updates.push(update); Ok(()) }).unwrap();
            assert_eq!(calls, usize::from(duration >= 0.1));
            assert_eq!(updates.len(), 1);
            assert!(updates[0].provisional.is_empty());
            assert_eq!(updates[0].committed.len(), usize::from(duration >= 0.1));
        }
    }

    #[test]
    fn paused_audio_does_not_trigger_inference_and_resume_keeps_the_timeline() {
        let (schedule, input) = channel();
        schedule.submit(samples(3.0), false);
        let (entered, ready) = bounded(0);
        let (published, updates) = bounded(0);
        let handle = std::thread::spawn(move || run(input, &AtomicBool::new(false), |start, length| {
            entered.send((start, length)).unwrap();
            Ok(transcript(Vec::new()))
        }, |update| { published.send(update).unwrap(); Ok(()) }));
        assert_eq!(ready.recv_timeout(Duration::from_secs(2)).unwrap(), (0, samples(3.0) as usize));
        updates.recv_timeout(Duration::from_secs(2)).unwrap();
        schedule.submit(samples(3.0), false);
        assert!(matches!(ready.recv_timeout(Duration::from_millis(80)), Err(RecvTimeoutError::Timeout)));
        schedule.submit(samples(4.0), true);
        assert_eq!(ready.recv_timeout(Duration::from_secs(2)).unwrap(), (0, samples(4.0) as usize));
        updates.recv_timeout(Duration::from_secs(2)).unwrap();
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn cancellation_or_inference_failure_does_not_publish_unfinished_predictions() {
        let (schedule, input) = channel();
        schedule.submit(samples(3.0), true);
        assert!(run(input, &AtomicBool::new(true), |_, _| panic!("cancelled work decoded"), |_| panic!("cancelled work published")).is_err());
        let (schedule, input) = channel();
        schedule.submit(samples(3.0), true);
        assert!(run(input, &AtomicBool::new(false), |_, _| anyhow::bail!("inference failed"), |_| panic!("failure published")).is_err());
        let (schedule, input) = channel();
        schedule.submit(samples(3.0), true);
        let cancel = AtomicBool::new(false);
        assert!(run(input, &cancel, |_, _| {
            cancel.store(true, Ordering::Relaxed);
            Ok(transcript(vec![word("cancelled", 0.5, 1.0)]))
        }, |_| panic!("in-flight cancellation published")).is_err());
    }

    #[test]
    fn an_unresolvable_backlog_boundary_fails_instead_of_looping_or_skipping_audio() {
        let (schedule, input) = channel();
        schedule.submit(samples(90.0), true);
        let mut calls = 0;
        let error = run(input, &AtomicBool::new(false), |_, _| {
            calls += 1;
            Ok(transcript(vec![word("unresolved", 0.0, 27.0)]))
        }, |_| panic!("an unresolvable window should not publish")).unwrap_err();
        assert!(error.to_string().contains("could not advance"));
        assert_eq!(calls, 1);
    }

    #[test]
    #[ignore = "requires .test-cache/parakeet-tdt-0.6b-v3-int8/ and jfk.wav"]
    fn real_parakeet_overlapping_windows_and_final_commitment() {
        use crate::{asr::{Transcriber, TranscribeOptions}, audio, domain::TranscriptionModel};
        let cache = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".test-cache");
        let path = cache.join("jfk.wav");
        let model = TranscriptionModel::default();
        let mut transcriber = Transcriber::load_with_threads(&cache.join(model.filename()), model, false, false, 2).unwrap();
        let available = u64::from(audio::open_wave(&path).unwrap().duration());
        let (schedule, input) = channel();
        schedule.submit(available.min(UPDATE_SAMPLES), false);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut committed = Vec::new();
        let mut calls = 0;
        let started = std::time::Instant::now();
        run(input, &cancel, |start, length| {
            calls += 1;
            let samples = audio::read_range(&path, start, length)?;
            let result = transcriber.run(&samples, &TranscribeOptions {
                offset: start as f64 / f64::from(SAMPLE_RATE), threads: 2, ..Default::default()
            }, &cancel, |_| {})?;
            let next = (start + length as u64 + UPDATE_SAMPLES).min(available);
            schedule.submit(next, next == available);
            Ok(result)
        }, |update| { committed.extend(update.committed); Ok(()) }).unwrap();
        eprintln!("Parakeet: {calls} overlapping windows in {:?}", started.elapsed());
        assert!(calls > 1 && committed.len() > 10);
        assert!(committed.iter().any(|word| word.text.to_lowercase().contains("country")));
        assert!(committed.iter().all(|word| word.start >= 0.0 && word.end <= available as f64 / f64::from(SAMPLE_RATE)));
        assert!(committed.windows(2).all(|pair| pair[1].start >= pair[0].start));
    }
}
