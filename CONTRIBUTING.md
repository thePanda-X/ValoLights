# Contributing to ValoLights

Thanks for your interest! Bug reports, ideas and pull requests are all welcome.

## Reporting bugs and ideas

Use the [issue templates](https://github.com/thePanda-X/ValoLights/issues/new/choose). For detection problems,
the most useful things are the resolution / frame rate of the recording, your HUD settings, and the log files
from `data\logs`. Please don't post recordings that show other players' names without their OK.

Security problems: please report them privately, see [SECURITY.md](SECURITY.md).

## Working on the code

[docs/development.md](docs/development.md) explains the setup, the workspace layout and the checks. In short:

```powershell
powershell -ExecutionPolicy Bypass -File tools\fetch_binaries.ps1
cargo run --release -p vl-app
```

Before opening a pull request:

```powershell
cargo fmt --all
cargo clippy --workspace --exclude vl-speech --all-targets -- -D warnings
cargo test --release --workspace --exclude vl-speech
```

CI runs the same commands on every pull request.

Guidelines:

- Keep changes focused; one topic per pull request.
- Add or update tests for behaviour you change. For UI changes, the scripted checks
  (`valolights --ui-check <page>`) and a screenshot help reviewers.
- User-facing changes get a line under "Unreleased" in [CHANGELOG.md](CHANGELOG.md).
- Everything runs locally: don't add features that upload users' recordings or data anywhere.
- New third-party binaries or models must be redistributable; pin and checksum them like
  `tools/fetch_binaries.ps1` does and list them in [docs/third-party.md](docs/third-party.md).

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
