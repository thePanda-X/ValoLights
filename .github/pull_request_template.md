## What and why

<!-- What does this change, and what problem does it solve? Link the issue if there is one. -->

## How it was tested

<!-- Unit tests, the scripted UI checks (`valolights --ui-check <page>`), a real clip, screenshots... -->

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --workspace --exclude vl-speech --all-targets -- -D warnings` are clean
- [ ] `cargo test --release --workspace --exclude vl-speech` passes
- [ ] User-facing changes are listed under "Unreleased" in `CHANGELOG.md`
