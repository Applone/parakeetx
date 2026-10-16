# parakeetx

A local-first desktop app for recording microphone and system audio, transcribing recordings, and creating speaker-aware notes. Built with Rust and Iced for Linux, macOS, and Windows.

- Live transcription and media-file import, with NVIDIA Parakeet TDT 0.6B v3 as the default model and Whisper alternatives.
- Optional speaker identification through a Python pyannote worker.
- Searchable local recording library and exports to text, Markdown, JSON, SRT, and VTT.
- Notes generated through a configurable OpenAI-compatible chat completions endpoint. Generating notes sends transcript text to the configured service.

Parakeet live transcription schedules overlapping windows every second. Recent words appear muted until they pass a two-second lookahead boundary; stopping the recording finalizes the remaining text. Actual latency depends on inference speed. Whisper uses the configurable live interval in Settings.

## Build and run

Requires Rust 1.88 or newer and native build tools, including CMake and Clang. Linux also needs PulseAudio, D-Bus, X11, Wayland, and libxkbcommon development libraries; see [.github/actions/setup/action.yml](.github/actions/setup/action.yml) for the dependency list. macOS builds require Xcode Command Line Tools; native system-audio capture requires macOS 13 or newer and recording permission.

```sh
cargo run --locked
cargo run --locked -- --doctor
```

Download the selected transcription model in Settings before transcribing. Speaker identification is enabled by default. In Settings → Speakers, the app checks your Python dependencies and detects installed uv. Choose **Set up automatically** to download Python 3.12 if needed and install pyannote.audio with CPU dependencies into an isolated environment. After validation, choose **Save settings** to use the selected interpreter. If uv is missing, follow the installation link or locate its executable. You can also install `python/requirements.txt` into your own Python environment and select that interpreter manually. Alternatively, disable speaker identification; transcription itself runs natively in Rust.

Accept the speaker model's conditions on Hugging Face and supply an access token, then use **Verify and download speaker model** to check access and download its weights separately. Checking Python dependencies does not download model weights. Automatic setup stores environments under the application's data directory (`diarization/envs/`); Hugging Face weights stay in the configured models directory. Downloads can be substantial. Setup can be cancelled; failed or cancelled new environments are removed, while existing environments and Python settings are preserved. Older successful environments are retained. Automatic setup installs CPU packages; GPU environments remain a manual option.

Settings and the SQLite library use platform-specific application directories. Set `PARAKEETX_HOME` to an absolute path to use a separate workspace. Optional credentials can be supplied with `HF_TOKEN` and `PARAKEETX_API_KEY`, or saved through the OS credential store.

Settings → Audio → Audio file quality controls storage for new recordings and imports: Compact (MP3, 32 kbps, about 14 MB/hour), Balanced (MP3, 64 kbps, about 29 MB/hour), or Original (uncompressed WAV, about 115 MB/hour, the default). Existing files are unchanged. MP3 compression runs after capture, live transcription, and speaker identification finish; these use the original 16 kHz mono audio. Transcribing a saved MP3 later uses its compressed audio quality. Processing temporarily needs space for the uncompressed WAV; cancellation or compression failure keeps that WAV available in the library. No external audio converter is required.

## Development

```sh
cargo build --locked --release
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 -m unittest discover -s python -p 'test_*.py' -v
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

Rust tests require `python3` on Unix or `python` on Windows. Packaging scripts require Python 3.11 or newer; CI uses 3.12. See [AGENTS.md](AGENTS.md) for architecture, native packaging, and tests requiring audio devices or model fixtures.

To exercise real automatic speaker setup, run `cargo test --locked --lib real_cpu_environment_setup -- --ignored`. This requires installed uv and internet access and downloads Python/CPU dependencies; ordinary tests use fake installers and mocked model loading.

Licensed under [GPL-3.0-only](LICENSE). Bundled fonts and icons have their own license files under `assets/`.
