# Development

ValoLights is a Rust workspace. Windows is the only supported platform today; the code is kept portable
(egui, libmpv and ffmpeg all run elsewhere) so Linux and macOS builds can follow.

## Requirements

- Windows 10 or 11, 64-bit
- [Rust](https://rustup.rs/) stable with the MSVC toolchain (Visual Studio Build Tools 2022 or newer, "Desktop
  development with C++")
- [CMake](https://cmake.org/) (whisper.cpp is built from source)
- For the GPU speech-to-text helper: the [CUDA toolkit 13.2](https://developer.nvidia.com/cuda-downloads) and an
  NVIDIA GPU. Without CUDA you can still build everything with the speech helper on the CPU (see below).

Everything else (ffmpeg, libmpv, the RIFE frame interpolator, the OCR and sound models, libclang) is downloaded
by one script into `bin\`, `assets\` and `build\tools\` (all git-ignored), pinned and SHA-256 checked:

```powershell
powershell -ExecutionPolicy Bypass -File tools\fetch_binaries.ps1
```

## Build and run

```powershell
cargo run --release -p vl-app                         # the app; data goes to .\data
cargo build --release -p vl-speech                    # speech helper with CUDA (first build ~25 min: GPU kernels)
cargo build --release -p vl-speech --no-default-features   # ... or without CUDA (CPU only)
```

`.cargo/config.toml` points the whisper.cpp build at the downloaded libclang and the CUDA 13.2 toolkit and selects
the GPU architectures (RTX 20-series and newer; older GPUs transcribe on the CPU).

A build run from the source tree uses the repository's `bin\`, `assets\` and `data\` folders: the app root is the
first folder above the executable that has both `assets\` and `bin\`. Set `VALOLIGHTS_ROOT` to use another one.

## Tests and checks

```powershell
cargo fmt --all
cargo clippy --workspace --exclude vl-speech --all-targets -- -D warnings
cargo test --release --workspace --exclude vl-speech   # unit tests (release reuses the optimised dependencies)
cargo test --release -p vl-speech                      # speech helper tests (builds whisper.cpp)
```

The app binary has a few developer modes:

```powershell
target\release\valolights.exe --selftest                    # packaging smoke test (ffmpeg, OCR, mpv, speech, GPU)
target\release\valolights.exe --ui-check [trim|layout|subs|export]   # scripted GUI checks on scratch copies
target\release\valolights.exe --perf                        # playback and export timings (read-only)
target\release\valolights.exe --shot <project> <page> out.png   # screenshot of a page (the app's own frame)
target\release\valolights.exe cli eval                      # detection accuracy on tests\eval\labels
```

The UI checks drive the real window with injected input, save the app's own frames (never the desktop) and only
write to `data\projects\rustqa_*` copies, which they delete afterwards. `cli eval` needs the sample recordings
the labels refer to; they aren't in the repository.

Command-line tools (same executable):

```powershell
target\release\valolights.exe cli analyze my_match.mp4 --alias "MyName"
target\release\valolights.exe cli summary data\projects\<id>
target\release\valolights.exe cli render data\projects\<id> --cut smart --format 9:16 -o out.mp4
target\release\valolights.exe cli update                    # look for a newer release
```

## Layout

```
crates/
  vl-core/     project files (JSON models), paths, config, ffmpeg helpers and frame reader, job protocol
  vl-engine/   image processing, OCR (ONNX Runtime + PP-OCRv6), kill feed / HUD detectors, tracker,
               player identity, rounds, the analysis pipeline, sound events (YAMNet), the evaluator
  vl-render/   highlights and quick cuts (incl. Smart cut), edit list, layouts, subtitles (ASS), overlays,
               looks, super smooth (optical-flow blur or RIFE), music and beat sync, the ffmpeg render,
               editor exports (Resolve EDL, Premiere XML, JSON)
  vl-speech/   vl-speech.exe: voice-chat transcription with whisper.cpp (CUDA, CPU fallback)
  vl-app/      valolights.exe: the GUI (eframe/egui + libmpv), background worker, CLI, updater, self-test
assets/        fonts (OFL), icons (lucide, ISC), templates, app icon; downloaded models land here
build/         build.ps1 (build + assemble the app), installer.ps1 + installer.iss (Inno Setup), installer artwork
tools/         fetch_binaries.ps1, installer_shots.ps1
tests/eval/    hand-checked labels for the detection evaluator
docs/          this file, releasing, third-party components, images
```

Design notes live next to the code: [`crates/vl-app/ARCHITECTURE.md`](../crates/vl-app/ARCHITECTURE.md) and
`crates/*/NOTES.md`.

### Processes

The GUI starts `valolights.exe --worker job.json` for analysis, renders and the Smart cut sound pass, and the
worker starts `vl-speech.exe --worker job.json` for transcriptions. Both report JSON lines on stdout. The CUDA
libraries are only ever loaded by `vl-speech.exe`, so a broken or missing GPU driver can't take the GUI down.

## Packaging

```powershell
powershell -ExecutionPolicy Bypass -File build\installer.ps1   # dist\ValoLights-Setup-<version>.exe (Inno Setup 6.7+)
powershell -ExecutionPolicy Bypass -File build\build.ps1       # only build\out\ValoLights (the app folder, self-tested)
```

The installer script runs `build.ps1` first: it builds the release binaries, assembles the app folder and runs its `--selftest`. The installer is small
(about 200 MB): it downloads ffmpeg, mpv and NVIDIA cuBLAS during setup from their official releases, pinned and
SHA-256 checked. Keep those pins in `build\installer.iss` and `tools\fetch_binaries.ps1` in step.

Releases are built by GitHub Actions; see [releasing.md](releasing.md).

## Other platforms

Not supported yet. What is already portable, and what isn't:

- GUI, analysis, rendering and the updater's check/verify/download code are platform-independent Rust.
- Windows-specific: the installer (Inno Setup), `update::start_install`, process helpers in `vl-app/src/sys.rs`,
  `tools\*.ps1`, the CUDA build settings in `.cargo/config.toml`, and the third-party binaries (ffmpeg, libmpv,
  RIFE are Windows builds).
- The update manifest is already keyed by platform (`windows-x86_64`, later e.g. `linux-x86_64`,
  `macos-aarch64`); a platform without an install step shows a link to the release page instead.
