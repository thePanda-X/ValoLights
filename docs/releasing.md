# Releasing

Releases are built and published by [`.github/workflows/release.yml`](../.github/workflows/release.yml) when a
version tag is pushed.

## One-time setup: the update signing key

The app only installs updates whose manifest is signed with the project's Ed25519 key. The public key is
compiled into the app (`PUBLIC_KEY` in `crates/vl-app/src/update.rs`); the private key is a GitHub secret.

1. Generate a key pair (once): `target\release\valolights.exe cli release-keygen`
2. Put the public key into `PUBLIC_KEY` in `crates/vl-app/src/update.rs` and commit it.
3. Add the private key as the repository secret **`UPDATE_SIGNING_KEY`** (Settings → Secrets and variables →
   Actions). Keep an offline backup; never commit it.

If the private key is ever lost or leaked, generate a new pair, ship a release with the new public key, and sign
with the new key from then on. Installs older than that release won't accept the new signatures and need one
manual update.

## Making a release

1. Update the version in the root `Cargo.toml` (`[workspace.package] version`).
2. Move the "Unreleased" notes in `CHANGELOG.md` under a new `## [x.y.z] - YYYY-MM-DD` heading. That section
   becomes the GitHub release notes and the "What's new" text in the app's update dialog.
3. Commit, then tag and push:

   ```sh
   git tag v0.3.0
   git push origin main v0.3.0
   ```

The workflow then:

1. checks the tag matches the version in `Cargo.toml`,
2. installs the CUDA toolkit and Inno Setup on the Windows runner,
3. runs `build\installer.ps1` (release build, app folder, `--selftest`, installer),
4. writes `latest.json`: version, release notes, installer URL, size, SHA-256 and the Ed25519 signature,
5. publishes the GitHub release with `ValoLights-Setup-x.y.z.exe` and `latest.json`.

A release takes about 15 minutes when the build cache is warm, and over an hour when whisper.cpp's CUDA kernels
have to be compiled from scratch (the log reports how many of them are done every minute). Build caches are
saved from `main` only, because release tags can restore `main`'s caches but not each other's. The same
workflow runs every Monday on `main` without publishing anything: it builds and self-tests the app, which
keeps the cache from expiring (GitHub deletes caches unused for 7 days) and catches broken release builds
early. To warm the cache before a release (say after updating whisper-rs), run the Release workflow by hand on
`main` with the tag field left empty.

Tags with a suffix (`v0.3.0-beta.1`) are published as pre-releases. GitHub's "latest" release (which the app
checks) skips pre-releases, so testers can install them by hand without offering them to everyone.

## How the app updates

- On launch (unless turned off in Settings) and from Settings → Updates → Check now, the app downloads
  `https://github.com/thePanda-X/ValoLights/releases/latest/download/latest.json`.
- It offers the update only if the version is newer and the signature matches the built-in public key.
- "Update now" downloads the installer, checks its size and SHA-256 against the signed manifest, starts it with
  `/SILENT /RELAUNCH=1 /DIR=<current folder>` and closes the app. The installer updates in place (keeping
  `data\`) and reopens ValoLights.
- `VALOLIGHTS_UPDATE_URL` overrides the manifest URL (a `file://` path works), for testing.
