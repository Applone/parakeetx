# parakeetx

A local-first desktop app for recording microphone and system audio, transcribing recordings, and creating speaker-aware notes. Built with Rust and Iced for Linux, macOS, and Windows.

- Live transcription and media-file import, with NVIDIA Parakeet TDT 0.6B v3 as the default model and Whisper alternatives.
- Optional speaker identification through a Python pyannote worker.
- Searchable local recording library and exports to text, Markdown, JSON, SRT, and VTT.
- Notes generated through a configurable OpenAI-compatible chat completions endpoint. Generating notes sends transcript text to the configured service.

## Build and run

Requires Rust 1.88 or newer and native build tools, including CMake and Clang. Linux also needs PulseAudio, D-Bus, X11, Wayland, and libxkbcommon development libraries; see [.github/actions/setup/action.yml](.github/actions/setup/action.yml) for the dependency list. macOS builds require Xcode Command Line Tools; native system-audio capture requires macOS 13 or newer and recording permission.

```sh
cargo run --locked
cargo run --locked -- --doctor
```

Download the selected transcription model in Settings before transcribing. Speaker identification is enabled by default: install `python/requirements.txt` into your chosen Python environment, select that interpreter in Settings, and supply a Hugging Face token after accepting the diarization model's conditions. Alternatively, disable speaker identification; transcription itself runs natively in Rust.

Settings and the SQLite library use platform-specific application directories. Set `PARAKEETX_HOME` to an absolute path to use a separate workspace. Optional credentials can be supplied with `HF_TOKEN` and `PARAKEETX_API_KEY`, or saved through the OS credential store.

## Development

```sh
cargo build --locked --release
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 -m unittest discover -s python -p 'test_*.py' -v
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

Rust tests require `python3` on Unix or `python` on Windows. Packaging scripts require Python 3.11 or newer; CI uses 3.12. See [AGENTS.md](AGENTS.md) for architecture, native packaging, and tests requiring audio devices or model fixtures.

Licensed under [GPL-3.0-only](LICENSE). Bundled fonts and icons have their own license files under `assets/`.
