# Working in parakeetx

## Commands and prerequisites

This is a single Rust 2024 package, with a Rust 1.88 minimum. Run commands from the repository root. `Cargo.lock` is committed; CI uses `--locked`.

```sh
cargo run --locked                                  # Desktop application
cargo run --locked -- --doctor                      # Paths, model availability, audio devices
cargo build --locked --release
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 -m unittest discover -s python -p 'test_*.py' -v
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

CI runs the Rust commands with `--target "$BUILD_TARGET" --profile "$BUILD_PROFILE"`, where the profile is `dev` or `release`. There is no separate formatter check in the workflows. Existing Rust often uses compact methods and one-line branches; follow surrounding style when editing.

- Native dependencies and platform setup are defined in `.github/actions/setup/action.yml`. Linux needs C/C++ tools, Clang, CMake, pkg-config, and development libraries for PulseAudio, D-Bus, libxkbcommon, X11, and Wayland. macOS needs Xcode Command Line Tools and CMake.
- Ordinary Rust tests spawn `python3` on Unix and `python` on Windows, even when pyannote is not installed. The Python worker requires Python 3.10+; packaging scripts use `tomllib` and require 3.11+. CI uses Python 3.12.
- `python/requirements.txt` installs pyannote for actual speaker identification. Python protocol unit tests use mocks and do not require those heavy dependencies.
- Default Cargo features are empty. `cuda` enables both ASR backends; `metal` and `vulkan` enable Whisper acceleration only. GPU availability in Settings depends on compiled features.
- Development builds disable debug info and incremental compilation, while optimizing selected inference/FFT dependencies. CI limits build jobs to two and sets `GGML_NATIVE=OFF` to avoid CPU-specific Whisper binaries.

## Architecture and flow

`src/main.rs` handles `--help`, `--version`, and `--doctor`; launching without arguments starts the Iced desktop app. It is not a transcription command-line interface.

| Area | Responsibility and integration points |
| --- | --- |
| `src/ui.rs` | Application state, messages, settings drafts, async tasks, and job lifecycle. |
| `src/ui/views.rs`, `src/ui/style.rs` | Widget construction and shared light/dark theme tokens. Fonts and SVG icons are embedded at compile time. |
| `src/engine.rs` | Import, recording, transcription, diarization, and summary orchestration; background `Job` workers report `Event`s through crossbeam channels. |
| `src/capture/` | Platform capture, device discovery, mixing, pause/stop controls, and timestamp-aware audio delivery. |
| `src/audio.rs` | Symphonia decoding, mono resampling, PCM WAV writing, and range reads. |
| `src/asr.rs`, `src/vad.rs` | Native Parakeet/Whisper recognition and optional Silero VAD. |
| `src/diarize.rs`, `python/diarize.py` | Isolated Python subprocess protocol and assignment of speakers to transcript words/segments. |
| `src/domain.rs` | Persisted recording/transcript types, pipeline metadata, and export serialization. |
| `src/storage.rs`, `src/settings.rs` | SQLite library, workspace paths/locking, settings validation, atomic writes, and credential storage. |
| `src/download.rs`, `src/summary.rs`, `src/http.rs` | Verified model downloads, compatible summary API requests, and shared TLS/cancellation handling. |

Startup uses the UI's `blocking` helper (`tokio::task::spawn_blocking`) to lock the workspace, load settings, open/recover the library, and discover devices. The lock lives in `Resources` for the session. The UI polls worker events every 80 ms. Long engine operations run on threads; filesystem/library work uses blocking tasks. Search and selection results carry their originating query/ID so stale results can be ignored.

Imported media is decoded and resampled into a local WAV before metadata is saved. Recording captures enabled sources, mixes them, writes WAV audio, and optionally sends file ranges to a separate live ASR worker. The WAV is flushed before live range reads. Stop finalizes audio and processes the final queued range, then optionally assigns speakers. Offline transcription reads chunks of at most 28 seconds and merges results with recording-relative timestamps. Summaries use transcript text and the recording's prompt override when present, then save the resulting notes.

Parakeet live scheduling and timestamp commitment live in `src/engine/live.rs`. An automatic preset submits a flushed audio endpoint every recorded second, keeps two seconds of right lookahead and up to ten seconds of left context, and caps inference windows at 28 seconds. Pending endpoints replace each other; backlogs advance through bounded overlapping windows without skipping uncommitted audio. Whisper retains disjoint ranges and the persisted `chunk_seconds` interval; that setting is hidden for Parakeet but remains valid when switching models.

Only a contiguous prefix of complete words ending before the lookahead barrier is committed. Word midpoint ownership excludes predictions at/before the previous frontier, including retained left context; no string matching is used. Silence advances the frontier without crossing unresolved words. Provisional words travel in `Event::LiveTranscript` and replace UI-only state; they are absent from recording JSON, search, exports, and notes. Normal stop decodes the final tail with no withheld lookahead; cancellation/failure discards provisional text and keeps saved audio/committed words. Timestamp drift can still affect boundary recognition. Parakeet live updates bypass `asr::merge`; committed words are segmented with `asr::segments_from_words`.

## Audio and inference invariants

- The shared audio format is **16 kHz mono PCM16 WAV** (`SAMPLE_RATE` in `src/lib.rs`). Capture buffers and ASR samples use floating-point values; saved audio and the Python worker agree on PCM16. Keep offsets in seconds relative to the recording, including chunk offsets.
- Linux capture uses PulseAudio, including monitor sources; macOS/Windows use CPAL, with a native ScreenCaptureKit system-audio bridge on macOS. Windows system sources include output-device loopback. Platform modules are selected with `cfg`.
- macOS bridge code is in `native/screencapturekit.m` and `.h`. `build.rs` compiles it with ARC, blocks, and warnings as errors, targets macOS 13, and links Apple frameworks. System permission is checked before capture threads or WAV creation. Native capture excludes the app's own audio.
- A selected device that disappeared must produce an error rather than silently substitute another device.
- Mixing pads missing input and tracks sample debt, discarding late input to avoid shifting the timeline. Capture buffering above three seconds stops recording to protect timing. Pause, silence, resampler delay, and final tail flushing have dedicated tests; preserve these behaviors when modifying capture.
- Parakeet is the default, using an INT8 bundle of three artifacts listed in `download::PARAKEET_FILES`. Its native word timestamps bypass Silero and DTW even when their settings switches are on. Use `Settings::uses_vad()` and `uses_dtw()` for effective pipeline behavior. Parakeet does not return a detected language.
- Whisper uses GGML model files, optional Silero VAD, and optional DTW word alignment. Model choice, artifact names/checksums, settings compatibility, and pipeline metadata span `domain`, `download`, `settings`, `asr`, and `engine`.

## Persistence, cancellation, and credentials

- `PARAKEETX_HOME` must be absolute; it creates `config/` and `data/` underneath that path. Otherwise `ProjectDirs` supplies OS-specific paths. Settings are `config/settings.json`; the library is `data/library.sqlite3`. Model and recording paths are configurable and must also be absolute.
- `Settings` uses serde defaults for older configuration files. Legacy Whisper model serialization is covered by tests. Invalid existing settings are reported without being overwritten. Writes use a same-directory temporary file, sync, and persist.
- `Library` is a cloneable path wrapper, opening a fresh connection for each operation with a ten-second busy timeout. SQLite uses WAL and schema `user_version=1`, rejecting newer versions. Each row stores a JSON `Recording` document alongside searchable title/transcript fields. FTS5 triggers maintain the index. Keep those representations consistent through `Library::save`.
- Startup recovery turns interrupted `Recording`/`Transcribing` rows into recoverable recorded/partial entries when audio exists. Removing an entry from the library removes metadata; the UI explicitly leaves the audio file on disk.
- Failed or cancelled retranscription restores an existing transcript. First-time transcription saves partial progress. Diarization failure is a warning with the transcript retained. Preserve saved audio and usable text across failures.
- `Job::finish_recording()` stops capture while allowing queued processing to finish; `Job::cancel()` also sets the processing cancellation flag. `Event::AudioSaved` distinguishes stopped capture from the remaining processing phase. Closing the window is handled explicitly rather than Iced's automatic exit.
- Secrets are separate from serializable `Settings` and have redacted debug output. `PARAKEETX_API_KEY` and `HF_TOKEN` take precedence over OS keyring credentials. Keyring failure leaves credentials session-only and reports a warning.
- Summary generation sends transcript text to the configured `/chat/completions` endpoint. UI confirmation gates that action. Requests split large inputs into bounded passes, and reject truncated or empty completions. Error handling must not expose credentials.
- Build HTTP clients through `src/http.rs`: reqwest uses `rustls-no-provider`, so this helper installs the ring provider. It also bridges cancellable async requests into engine threads. Downloads verify checksums and preserve existing artifacts on failure; bundle downloads stage replacements before committing them.

## Python worker contract

The worker source is embedded with `include_str!` and written/repaired at `<models_dir>/python/diarize.py`; edit `python/diarize.py`, not the generated copy. Rust launches the configured interpreter with `-I`, sends the JSON request (including the token) through stdin, and reads a JSON response from stdout. Diagnostics and third-party prints go to stderr. Keep stdout machine-readable and keep both pipes draining concurrently to prevent deadlocks. Rust bounds captured output and terminates the process on cancellation/timeout.

The subprocess strips inherited credential variables and uses `<models_dir>/huggingface` as `HF_HOME`. The default model is `pyannote/speaker-diarization-community-1`, requiring accepted Hugging Face conditions and a token. The worker supports modern and legacy pyannote output objects, preferring exclusive speaker turns. Rust assigns speakers at word level and splits segments on speaker changes.

## Testing

Rust unit tests live alongside modules; UI tests are in `src/ui/tests.rs`. Tests use temporary workspaces and synthetic audio rather than the user's library. UI rendering/state tests avoid external services. `tests/network.rs` runs local TCP servers for summary and download behavior, so ordinary tests need loopback socket access but no hosted API credentials.

Real inference, compressed media, and audio-device integration tests are explicitly ignored. Run only the relevant suite once its prerequisites exist:

```sh
cargo test --locked --test native_inference -- --ignored
cargo test --locked --lib real_parakeet_overlapping_windows_and_final_commitment -- --ignored
cargo test --locked --test media -- --ignored
cargo test --locked --test recording -- --ignored       # Linux
cargo test --locked --test macos_capture -- --ignored   # macOS
```

- `native_inference`: `.test-cache/jfk.wav` plus the Parakeet INT8 directory, or `ggml-tiny.bin` and `silero_vad.onnx`, depending on the test.
- `media`: FFmpeg and `.test-cache/jfk.wav`; FFmpeg generates compressed fixtures, while application import uses Symphonia.
- `recording`: PulseAudio/PipeWire, `pactl`, `paplay`, and native Whisper/VAD fixtures; creates a temporary null sink.
- `macos_capture`: macOS 13+, an active display, `afplay`, and Screen & System Audio Recording permission.

Python worker tests check protocol validation and output compatibility with mocks. Packaging tests cover archive executable permissions, release assets/checksums, and glibc symbol parsing. Match regression coverage to the affected contract rather than requiring model downloads for ordinary changes.

## Packaging and releases

`.github/workflows/build.yml` builds Linux x64 in AlmaLinux 9, macOS ARM64, and Windows x64. Debug runs on branch pushes/PRs; Release runs on tags. The release tag must match `Cargo.toml`'s version, with an optional `v` prefix.

Packaging consumes an existing build, requires the native target OS, and supports only the three targets listed in `scripts/package.py`. Example for a Linux target build:

```sh
cargo build --locked --release --target x86_64-unknown-linux-gnu --bin parakeetx
python3 scripts/package.py --configuration Release --target x86_64-unknown-linux-gnu --verify
```

- A host build goes into `target/release`, whereas packaging defaults to `target/<target>/release`; supply `--build-dir target/release` when packaging a host build. Debug uses `--configuration Debug` and `debug` output directories.
- `--installers` is allowed only for Release and adds DEB/RPM/Arch packages, an NSIS installer, or a DMG. Linux uses nFPM (`scripts/install-packaging-tools.py`); Windows requires NSIS. Tool versions are in `packaging/tools.json`. Output defaults to ignored `artifacts/`.
- ONNX Runtime is a native dependency. Linux CI deliberately uses Microsoft's shared CPU runtime 1.24.4 with `ORT_LIB_LOCATION` and `ORT_PREFER_DYNAMIC_LINK=1`; the default ort-sys static distribution needs a newer ABI than AlmaLinux 9. Keep runtime libraries beside the executable: `build.rs` supplies `$ORIGIN`/`@executable_path` rpaths, and packaging dereferences library symlinks.
- Linux installers enforce glibc 2.34 for strong imports; weak imports do not set the compatibility floor. Build distributable Linux installers in the CI-compatible environment. nFPM's license field is literal and checked against the Cargo manifest.
- macOS packages use ad-hoc signing for local execution. Distribution signing/notarization is a separate concern, not implemented by the packaging script.
- `--verify` extracts and smoke-tests archives with library-path overrides removed. Release CI also installs/runs/uninstalls Linux packages in four distribution containers, verifies all eight expected assets, and checks hashes before publishing.
- Verify a prepared release with `python3 scripts/verify-release.py --artifacts artifacts --tag v0.1.0` (use the actual manifest version). Its tag-only mode is `--tag v0.1.0 --tag-only`.

No existing Cursor, Copilot, Claude, or agent rule files were present when this guide was created. Project licensing is GPL-3.0-only; asset-specific licenses are under `assets/fonts/` and `assets/icons/`.
