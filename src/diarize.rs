use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    domain::{Segment, Word},
    settings::{Settings, private_directory},
};

const WORKER: &str = include_str!("../python/diarize.py");

#[derive(Clone, Serialize)]
struct Request {
    audio: String,
    model: String,
    token: Option<String>,
    min_speakers: Option<u32>,
    max_speakers: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}

#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    turns: Vec<Turn>,
    #[serde(default)]
    error: String,
    #[serde(default)]
    hint: String,
    #[serde(default)]
    pyannote: String,
    #[serde(default)]
    python: String,
    #[serde(default)]
    cuda: bool,
}

pub fn worker_path(settings: &Settings) -> Result<PathBuf> {
    let directory = settings.models_dir.join("python");
    private_directory(&directory)?;
    let path = directory.join("diarize.py");
    let needs_write = std::fs::read_to_string(&path).map(|existing| existing != WORKER).unwrap_or(true);
    if needs_write {
        crate::settings::atomic_write(&path, WORKER.as_bytes())?;
    }
    Ok(path)
}

fn spawn(settings: &Settings, arguments: &[&str]) -> Result<std::process::Child> {
    ensure!(!settings.python_path.trim().is_empty(), "Set the Python interpreter path in Settings");
    let script = worker_path(settings)?;
    let mut command = Command::new(settings.python_path.trim());
    command.arg("-I").arg(&script).args(arguments)
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("PYANNOTE_METRICS_ENABLED", "0")
        .env_remove("PARAKEETX_API_KEY")
        .env("TOKENIZERS_PARALLELISM", "false")
        .env_remove("HF_TOKEN")
        .env_remove("HUGGING_FACE_HUB_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !settings.models_dir.as_os_str().is_empty() {
        command.env("HF_HOME", settings.models_dir.join("huggingface"));
    }
    command.spawn().with_context(|| format!("Cannot start Python at '{}'. Install Python 3.10 or newer, or correct the path in Settings.", settings.python_path.trim()))
}

fn finish(mut child: std::process::Child, payload: Option<String>, cancel: &AtomicBool, timeout: Duration) -> Result<Response> {
    use std::io::Read;
    let stdout = child.stdout.take().context("Cannot read the Python worker output")?;
    let stderr = child.stderr.take().context("Cannot read the Python worker diagnostics")?;
    let (sender, receiver) = crossbeam_channel::unbounded();
    for (is_output, mut pipe) in [(true, Box::new(stdout) as Box<dyn Read + Send>), (false, Box::new(stderr) as Box<dyn Read + Send>)] {
        let sender = sender.clone();
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            let mut captured = Vec::new();
            let limit = if is_output { 32 * 1024 * 1024 } else { 16 * 1024 };
            let mut overflow = false;
            while let Ok(count) = pipe.read(&mut buffer) {
                if count == 0 { break; }
                if is_output {
                    let available = limit - captured.len();
                    captured.extend_from_slice(&buffer[..count.min(available)]);
                    overflow |= count > available;
                } else {
                    captured.extend_from_slice(&buffer[..count]);
                    if captured.len() > limit { captured.drain(..captured.len() - limit); }
                }
            }
            let _ = sender.send((is_output, captured, overflow));
        });
    }
    drop(sender);
    if let Some(payload) = payload {
        let write = child.stdin.as_mut().context("Cannot reach the diarization process")?.write_all(payload.as_bytes());
        if let Err(error) = write {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Cannot send the diarization request");
        }
    }
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut output = Vec::new();
            let mut diagnostics = Vec::new();
            for _ in 0..2 {
                let (is_output, bytes, overflow) = receiver.recv_timeout(Duration::from_secs(5)).context("The Python worker did not close its output pipes")?;
                ensure!(!overflow, "The Python worker response exceeds 32 MiB");
                if is_output { output = bytes; } else { diagnostics = bytes; }
            }
            let stdout = String::from_utf8_lossy(&output);
            let stderr = String::from_utf8_lossy(&diagnostics);
            let line = stdout.lines().rev().find(|line| line.trim_start().starts_with('{'));
            let line = line.with_context(|| format!("The Python worker produced no result (exit {status}). {}", tail(&stderr)))?;
            let response: Response = serde_json::from_str(line).context("Unexpected diarization response")?;
            if !response.ok { bail!("{} {}", response.error, response.hint); }
            ensure!(status.success(), "The Python worker returned a failure exit status");
            ensure!(response.turns.iter().all(|turn| turn.start.is_finite() && turn.end.is_finite() && turn.start >= 0.0 && turn.end > turn.start && !turn.speaker.is_empty()), "The diarization result has invalid timestamps or speaker labels");
            return Ok(response);
        }
        if cancel.load(Ordering::Relaxed) || std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{}", if cancel.load(Ordering::Relaxed) { "Diarization cancelled" } else { "Diarization timed out" });
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn tail(stderr: &str) -> String {
    let lines: Vec<_> = stderr.lines().filter(|line| !line.trim().is_empty()).collect();
    match lines.len() {
        0 => String::new(),
        length => lines[length.saturating_sub(4)..].join(" | "),
    }
}

pub fn probe(settings: &Settings, cancel: &AtomicBool) -> Result<String> {
    ensure!(!cancel.load(Ordering::Relaxed), "Python check cancelled");
    let child = spawn(settings, &["--probe"])?;
    let response = finish(child, None, cancel, Duration::from_secs(120))?;
    let device = if response.cuda { "GPU" } else { "CPU" };
    Ok(format!("pyannote.audio {} on Python {} ({device})", response.pyannote, response.python))
}

pub fn run(settings: &Settings, token: &str, audio: &Path, duration: f64, cancel: &AtomicBool) -> Result<Vec<Turn>> {
    ensure!(audio.is_file(), "Recording audio is missing");
    let audio_path = audio.to_str().context("Audio path contains invalid characters")?.to_owned();
    let request = Request {
        audio: audio_path,
        model: settings.diarization_model.trim().to_owned(),
        token: Some(token.to_owned()).filter(|value| !value.is_empty()),
        min_speakers: settings.min_speakers,
        max_speakers: settings.max_speakers,
    };
    ensure!(!request.model.is_empty(), "Set a diarization model in Settings");
    let child = spawn(settings, &[])?;
    let timeout = Duration::from_secs(600 + (duration.max(0.0) as u64).saturating_mul(4));
    let response = finish(child, Some(serde_json::to_string(&request)?), cancel, timeout).map_err(|error| {
        let message = format!("{error:#}");
        anyhow::anyhow!(if token.is_empty() { message } else { message.replace(token, "[redacted]") })
    })?;
    Ok(response.turns)
}

pub fn assign(segments: &mut [Segment], turns: &[Turn]) {
    for segment in segments.iter_mut() {
        for word in segment.words.iter_mut() {
            word.speaker = dominant(turns, word.start, word.end);
        }
        segment.speaker = speaker_for(segment, turns);
    }
}

fn speaker_for(segment: &Segment, turns: &[Turn]) -> Option<String> {
    let mut totals: Vec<(String, f64)> = Vec::new();
    for word in &segment.words {
        if let Some(speaker) = &word.speaker {
            add(&mut totals, speaker, (word.end - word.start).max(0.05));
        }
    }
    if totals.is_empty() {
        return dominant(turns, segment.start, segment.end);
    }
    totals.into_iter().max_by(|first, second| first.1.total_cmp(&second.1)).map(|(speaker, _)| speaker)
}

fn dominant(turns: &[Turn], start: f64, end: f64) -> Option<String> {
    let mut totals: Vec<(String, f64)> = Vec::new();
    let span_end = end.max(start);
    for turn in turns {
        let overlap = turn.end.min(span_end) - turn.start.max(start);
        if overlap > 0.0 {
            add(&mut totals, &turn.speaker, overlap);
        }
    }
    if totals.is_empty() {
        let middle = (start + span_end) / 2.0;
        return turns.iter().min_by(|first, second| {
            distance(first, middle).total_cmp(&distance(second, middle))
        }).filter(|turn| distance(turn, middle) < 2.0).map(|turn| turn.speaker.clone());
    }
    totals.into_iter().max_by(|first, second| first.1.total_cmp(&second.1)).map(|(speaker, _)| speaker)
}

fn distance(turn: &Turn, time: f64) -> f64 {
    if time < turn.start { turn.start - time } else if time > turn.end { time - turn.end } else { 0.0 }
}

fn add(totals: &mut Vec<(String, f64)>, speaker: &str, amount: f64) {
    match totals.iter_mut().find(|(name, _)| name == speaker) {
        Some(entry) => entry.1 += amount,
        None => totals.push((speaker.to_owned(), amount)),
    }
}

pub fn split_on_speaker_change(segments: Vec<Segment>) -> Vec<Segment> {
    let mut result = Vec::new();
    for segment in segments {
        if segment.words.len() < 2 || segment.words.iter().all(|word| word.speaker == segment.words[0].speaker) {
            result.push(segment);
            continue;
        }
        let mut current: Vec<Word> = Vec::new();
        for word in segment.words {
            if current.first().is_some_and(|first| first.speaker != word.speaker) {
                result.push(from_words(std::mem::take(&mut current)));
            }
            current.push(word);
        }
        if !current.is_empty() {
            result.push(from_words(current));
        }
    }
    result
}

fn from_words(words: Vec<Word>) -> Segment {
    let start = words.first().map_or(0.0, |word| word.start);
    let end = words.last().map_or(start, |word| word.end);
    Segment {
        start,
        end,
        text: words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" "),
        speaker: words.first().and_then(|word| word.speaker.clone()),
        words,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, confidence: 0.9, speaker: None }
    }

    #[test]
    fn speakers_follow_the_largest_overlap() {
        let turns = vec![
            Turn { start: 0.0, end: 2.0, speaker: "SPEAKER_00".into() },
            Turn { start: 2.0, end: 6.0, speaker: "SPEAKER_01".into() },
        ];
        let mut segments = vec![Segment {
            start: 0.0, end: 6.0, text: "a b c".into(), speaker: None,
            words: vec![word("a", 0.1, 1.0), word("b", 2.5, 3.0), word("c", 4.0, 5.5)],
        }];
        assign(&mut segments, &turns);
        assert_eq!(segments[0].speaker.as_deref(), Some("SPEAKER_01"));
        assert_eq!(segments[0].words[0].speaker.as_deref(), Some("SPEAKER_00"));
        let split = split_on_speaker_change(segments);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].text, "a");
        assert_eq!(split[1].text, "b c");
    }

    #[test]
    fn words_outside_all_turns_snap_to_the_closest_speaker_or_stay_empty() {
        let turns = vec![Turn { start: 10.0, end: 12.0, speaker: "SPEAKER_00".into() }];
        let mut segments = vec![
            Segment { start: 9.0, end: 9.5, text: "near".into(), speaker: None, words: vec![word("near", 9.0, 9.5)] },
            Segment { start: 0.0, end: 0.5, text: "far".into(), speaker: None, words: vec![word("far", 0.0, 0.5)] },
        ];
        assign(&mut segments, &turns);
        assert_eq!(segments[0].speaker.as_deref(), Some("SPEAKER_00"));
        assert_eq!(segments[1].speaker, None);
    }

    #[test]
    fn draining_both_subprocess_pipes_prevents_deadlocks() {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let child = Command::new(python).args(["-c", "import sys;sys.stderr.write('x'*200000);sys.stdout.write('{\"ok\":true,\"turns\":[]}\\n')"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let result = finish(child, None, &AtomicBool::new(false), Duration::from_secs(5)).unwrap();
        assert!(result.ok);
    }

    #[test]
    fn cancellation_terminates_the_python_worker() {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let child = Command::new(python).args(["-c", "import time;time.sleep(60)"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let started = std::time::Instant::now();
        assert!(finish(child, None, &AtomicBool::new(true), Duration::from_secs(90)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn worker_script_is_written_once_and_repaired() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings { models_dir: directory.path().to_owned(), recordings_dir: directory.path().to_owned(), ..Settings::default() };
        let path = worker_path(&settings).unwrap();
        assert!(path.is_file());
        std::fs::write(&path, "tampered").unwrap();
        worker_path(&settings).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), WORKER);
    }
}
