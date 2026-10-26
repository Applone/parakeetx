# parakeetx

A local-first desktop app for recording microphone and system audio, transcribing recordings, and creating speaker-aware notes. Built with Rust and Iced for Linux, macOS, and Windows.

- Live transcription and media-file import, with NVIDIA Parakeet TDT 0.6B v3 as the default model and Whisper alternatives.
- Optional speaker identification through a Python pyannote worker.
- Searchable local recording library and exports to text, Markdown, JSON, SRT, and VTT.
- Notes generated through a configurable OpenAI-compatible chat completions endpoint. Generating notes sends transcript text to the configured service.

Parakeet live transcription schedules overlapping windows every three seconds to reduce repeated inference, retaining up to ten seconds of preceding context. Recent words appear muted until they pass a two-second lookahead boundary; stopping the recording triggers final processing without waiting for the next interval. ONNX inference workers sleep when idle to avoid wasting CPU on thread spinning. Actual latency depends on inference speed. The CPU threads setting controls inference parallelism; fewer threads can reduce peak CPU use but may increase latency. Whisper uses the configurable live interval in Settings.

## Build and run

Requires Rust 1.88 or newer and native build tools, including CMake and Clang. Linux also needs PulseAudio, D-Bus, X11, Wayland, and libxkbcommon development libraries; see [.github/actions/setup/action.yml](.github/actions/setup/action.yml) for the dependency list. macOS builds require Xcode Command Line Tools; native system-audio capture requires macOS 13 or newer and recording permission.

```sh
cargo run --locked
cargo run --locked -- --doctor
```

Download the selected transcription model in Settings before transcribing. **Detect speech (Silero)** is available for Parakeet and Whisper and enabled for new configurations; existing saved choices are preserved. **Download models** also downloads Silero when speech detection is enabled. Parakeet skips live windows with no detected speech, retaining the full audio context for windows with speech; recognition can continue briefly into pauses while that context still contains speech. Offline transcription selects padded speech regions. Speaker identification is enabled by default. In Settings → Speakers, the app checks your Python dependencies and detects installed uv. Choose **Set up automatically** to download Python 3.12 if needed and install pyannote.audio into an isolated environment. On Linux and Windows, uv selects a driver-compatible CUDA build of PyTorch when available, falling back to CPU packages on systems without a supported GPU. After validation, choose **Save settings** to use the selected interpreter. If uv is missing, follow the installation link or locate its executable. You can also install `python/requirements.txt` into your own Python environment and select that interpreter manually. Alternatively, disable speaker identification; transcription itself runs natively in Rust.

Accept the speaker model's conditions on Hugging Face and supply an access token, then use **Verify and download speaker model** to check access and download its weights separately. Checking Python dependencies does not download model weights. Automatic setup stores environments under `diarization/envs/` inside the configured models folder, with downloaded Python runtimes in `diarization/python/` and the package cache in `diarization/cache/`; Hugging Face weights use `huggingface/hub` inside the shared models folder. Change that folder in Settings → Speakers or Settings → General; existing downloads are not moved automatically. Downloads can be substantial. Setup can be cancelled; failed or cancelled new environments are removed, while existing environments and Python settings are preserved. Older successful environments are retained. Speaker identification uses CUDA automatically when the selected Python environment supports it, independently of Cargo features and the transcription acceleration setting. **Check setup** reports the PyTorch version, GPU name when available, and whether CPU use is due to a CPU-only build or unavailable CUDA. If you previously used automatic CPU setup, run **Set up automatically** again and **Save settings** to select a new environment; the old environment is retained. CUDA initialization failures are reported instead of silently retrying on CPU. Some diarization stages still run on the CPU, so continuous GPU utilization is not expected.

Settings and the SQLite library use platform-specific application directories. Set `PARAKEETX_HOME` to an absolute path to use a separate workspace. Optional credentials can be supplied with `HF_TOKEN` and `PARAKEETX_API_KEY`, or saved through the OS credential store.

Save audio source changes in Settings before starting a recording. Recording is blocked while those changes are unsaved, so a disabled source cannot remain active through the previous saved configuration.

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

To exercise real automatic speaker setup, run `cargo test --locked --lib real_auto_environment_setup -- --ignored`. This requires installed uv and internet access and downloads Python/PyTorch dependencies; ordinary tests use fake installers and mocked model loading.

To release from a clean working tree, run `./scripts/bump.sh` (patch), or pass `minor`, `major`, or an explicit version such as `1.2.3`. Requires Bash, Git, and Python 3.11+. The script updates `Cargo.toml`, `Cargo.lock`, and the macOS plist, increments the plist build number, commits, creates an annotated `vX.Y.Z` tag, and pushes the current branch and tag atomically to `origin`. The tag triggers the release workflow. If pushing fails, the local commit and tag remain; retry the printed `git push` command after resolving the error.

Licensed under [GPL-3.0-only](LICENSE). Bundled fonts and icons have their own license files under `assets/`.
