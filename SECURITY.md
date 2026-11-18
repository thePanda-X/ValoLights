# Security policy

## Supported versions

Only the latest release receives fixes. The app checks for updates on launch; Settings → Updates → Check now
does it on demand.

## Reporting a vulnerability

Please report security issues privately through
[GitHub's private vulnerability reporting](https://github.com/thePanda-X/ValoLights/security/advisories/new),
not in public issues. Include what you found, how to reproduce it and the version you tested.

You can expect a first reply within a week.

## How updates are protected

Update manifests are signed with an Ed25519 key held only by the release workflow; the app verifies the
signature with a built-in public key and checks the installer's size and SHA-256 before running it. The
installer itself downloads only pinned, SHA-256-checked third-party files. See
[docs/releasing.md](docs/releasing.md).
