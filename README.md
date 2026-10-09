# parakeetx

A local Rust desktop application for recording, transcription, speaker labels,
and notes. The default pipeline uses NVIDIA **parakeet-tdt-0.6b-v3** with native
word timestamps and **pyannote** speaker diarization. Recognition runs through
`parakeet-rs` and ONNX Runtime using an INT8 export of NVIDIA's model; it does
not run Whisper DTW alignment or Silero VAD.

Whisper remains available in Settings for languages or workloads that need it.
Its optional Silero and DTW stages only run when a Whisper model is selected.

## Features

- Record microphone audio, system audio, or both; pause and resume on a
  continuous recording timeline.
- Transcribe while recording in short chunks, with word timestamps.
- Import MP3, MP4, M4A, WAV, FLAC, OGG, AAC, AIFF, and MKV, normalized to
  16 kHz mono using native decoding and resampling.
- Assign pyannote speaker labels to words and split transcripts at speaker
  changes. Speaker identification runs after file transcription or recording.
- Generate notes using any OpenAI-compatible chat API, including local servers.
- Search transcripts with SQLite full-text search.
- Export text, Markdown, JSON with word timings, SRT, or WebVTT.

Audio and transcripts stay local. Model setup downloads weights from Hugging
Face. Notes send transcript text to the configured API when requested.

## Pipeline

| Capability | Implementation |
| --- | --- |
| Default recognition | NVIDIA Parakeet TDT 0.6B v3, INT8 ONNX through `parakeet-rs` |
| Word timestamps | Native TDT token durations, grouped into words and sentences |
| Default speaker diarization | `pyannote/speaker-diarization-community-1` in an isolated Python worker |
| Alternative recognition | `whisper-rs`, with optional DTW alignment and Silero VAD |
| Audio capture | PulseAudio/PipeWire on Linux; `cpal` on macOS and Windows |
| Decoding/resampling | `symphonia` and `rubato` |
| Library/search | SQLite with FTS5 |
| Interface | `iced` |

Parakeet detects languages automatically; it supports 25 European languages,
including English and Russian. It does not expose a language ID through this
ONNX decoder, so the application leaves that metadata unset. Language overrides
only apply to Whisper. See [NVIDIA's model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
for supported languages and evaluation results.

Parakeet timestamps are always available, independently of the Whisper word
alignment setting. JSON records identify the engine and distinguish native word
timestamps from DTW. Word confidence is `0.0` for Parakeet because this decoder
does not supply probabilities.

## Getting started

Requirements: Rust 1.88+, a C/C++ toolchain and CMake for the Whisper fallback,
and Linux development headers for `libpulse` when building on Linux.

```sh
cargo run --release
```

In **Settings → Transcription**, keep **NVIDIA Parakeet TDT 0.6B v3** selected
and choose **Download models**. The download is approximately 640 MiB and
includes the encoder, decoder/joint model, and vocabulary. Artifacts come from
a pinned [ONNX export](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/tree/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce),
are SHA-256 verified, and are installed together. Silero is only downloaded
when enabled for a Whisper model.

For the default speaker identification pipeline, create a Python environment:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r python/requirements.txt
```

On Windows use `.venv\Scripts\python.exe` instead of `.venv/bin/python`.
Set the absolute path to this interpreter in **Settings → Speakers**, accept
the [community-1 model conditions](https://huggingface.co/pyannote/speaker-diarization-community-1),
and provide a Hugging Face token. **Check Python setup** verifies dependencies.
The worker prefers community-1's exclusive speaker turns for word assignment,
with compatibility for older pyannote result formats. It uses CUDA when
available and falls back to CPU.

Choose audio sources, save settings, and start recording. Speaker identification
is enabled for new installations and can be disabled for transcript-only use.
If Python setup or model access fails, the transcript is saved with a warning.
Existing settings retain their selected model and speaker preferences; select
Parakeet explicitly when updating an existing installation.

```sh
cargo run --release -- --doctor    # paths, selected model readiness, audio devices
cargo run --release -- --version
cargo run --release -- --help
```

## Acceleration

The default Rust build uses CPU inference. Use release builds for transcription.
NVIDIA acceleration can be enabled with:

```sh
cargo build --release --features cuda
```

Parakeet uses the selected CPU thread count for its encoder and one CPU thread
for its small decoder/joint graph. CUDA builds allow encoder acceleration,
with ONNX Runtime's CPU fallback. Speed depends on the hardware and audio;
this repository does not guarantee a particular real-time factor.

The `metal` and `vulkan` build features accelerate the Whisper fallback only:

```sh
cargo build --release --features metal
cargo build --release --features vulkan
```

## Notes and credentials

Configure a notes API base URL ending in `/v1`, a model identifier, and
optionally an API key. Long transcripts are summarized and merged in bounded
passes. Credentials are session-only or stored in the OS keychain on request;
they are never written to `settings.json`. Environment variables
`PARAKEETX_API_KEY` and `HF_TOKEN` are also supported.

## Storage

Paths follow platform conventions and can be changed in Settings.
`PARAKEETX_HOME` overrides both roots and must be an absolute path.

```text
<config>/parakeetx/settings.json
<data>/parakeetx/library.sqlite3
<data>/parakeetx/workspace.lock
<data>/parakeetx/recordings/
<data>/parakeetx/models/parakeet-tdt-0.6b-v3-int8/
<data>/parakeetx/models/huggingface/
<data>/parakeetx/models/python/diarize.py
```

When moving an existing installation, copy its config and data into these new
roots or point `PARAKEETX_HOME` to a directory containing `config/` and `data/`.
Update the model and recording directories in Settings if those files moved.
Saved Whisper model selections and existing recording metadata remain readable.
Credentials stored under a previous application name must be entered again.

## Reliability

- Audio flushes to disk every second; interrupted recordings recover on launch.
- Live inference failures keep audio capture running and report the error.
- Failed re-transcription preserves the previous transcript.
- Bounded capture backpressure prevents timeline drift.
- Failed or cancelled downloads preserve installed weights.
- Parakeet cancellation is checked before and after each inference chunk;
  an in-flight ONNX call must finish. Downloads and the Python worker support
  cancellation during their work.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
python3 -m unittest discover -s python
```

Hardware and model tests are ignored by default. For Parakeet inference, put
the three artifacts listed in `src/download.rs` in
`.test-cache/parakeet-tdt-0.6b-v3-int8/`, and place `jfk.wav` in `.test-cache/`:

```sh
cargo test --test native_inference real_parakeet -- --ignored
```

The separate Whisper fallback test needs `.test-cache/ggml-tiny.bin` and
`.test-cache/silero_vad.onnx`. Media and capture tests also require `ffmpeg` or
a running sound server as described by their ignore messages. Unit and Python
protocol tests do not require model weights. Network tests need permission to
bind local sockets.

## Platform notes

- Linux system audio uses PulseAudio or PipeWire monitor sources.
- Windows uses WASAPI loopback or an input device such as Stereo Mix.
- macOS system audio requires a virtual input such as BlackHole or Loopback;
  grant microphone access to the application.

## License

Application code: MIT. Model weights have separate licenses; NVIDIA Parakeet
v3 and pyannote community-1 are distributed under CC BY 4.0. See their model
cards linked above for attribution and terms.
