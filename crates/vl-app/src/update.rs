//! Updates: check GitHub Releases for a newer version, download it, verify it, install it.
//!
//! Every release publishes `latest.json` next to its installer:
//!
//! ```json
//! { "version": "0.3.0", "notes": "...", "published": "2026-11-20",
//!   "platforms": { "windows-x86_64": { "url": "...ValoLights-Setup-0.3.0.exe", "sha256": "...",
//!                                      "size": 204857344, "signature": "<base64 Ed25519>" } } }
//! ```
//!
//! The signature covers [`signed_message`] (product, version, platform and the installer's SHA-256) and is made
//! by the release workflow with a private key that only CI has; the app carries the public key
//! ([`PUBLIC_KEY`]). An update is only offered when the signature verifies, and only installed when the
//! downloaded file has the signed SHA-256: a compromised download location can't push a different file.
//!
//! Platforms: the manifest is keyed by [`platform_key`]. Installing is implemented for Windows (the Inno Setup
//! installer, run silently into the current folder, which relaunches the app). Elsewhere the update is shown
//! with a link to the release page until an installer step exists for that platform.
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Where the newest release's manifest lives (`VALOLIGHTS_UPDATE_URL` overrides it, for tests).
pub const MANIFEST_URL: &str = "https://github.com/thePanda-X/ValoLights/releases/latest/download/latest.json";
/// The release page, for platforms without an automatic install step.
pub const RELEASES_URL: &str = "https://github.com/thePanda-X/ValoLights/releases/latest";
/// Public half of the release signing key (Ed25519, base64). The private half is the `UPDATE_SIGNING_KEY`
/// secret of the release workflow.
pub const PUBLIC_KEY: &str = "VOoPUcmG8XRSne9mYz4j22xY/xskDnz40qlJ5k+v6FM=";

pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver")
}

/// The manifest key of this build: `<os>-<arch>` (windows-x86_64, linux-x86_64, macos-aarch64, ...).
pub fn platform_key() -> String {
    let os = std::env::consts::OS;
    format!("{os}-{}", std::env::consts::ARCH)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub published: String,
    pub platforms: BTreeMap<String, PlatformRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlatformRelease {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    pub signature: String,
}

/// A verified, newer release for this platform.
#[derive(Debug, Clone, PartialEq)]
pub struct Available {
    pub version: semver::Version,
    pub notes: String,
    pub published: String,
    pub release: PlatformRelease,
    /// an install step exists for this platform (else: link to the release page)
    pub installable: bool,
}

/// What the signature covers.
pub fn signed_message(version: &str, platform: &str, sha256: &str) -> String {
    format!("valolights-update\n{version}\n{platform}\n{}", sha256.to_ascii_lowercase())
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn verifying_key(public: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = b64().decode(public.trim())?.try_into().map_err(|_| anyhow!("public key must be 32 bytes"))?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

/// Check one platform entry's signature against `public` (base64).
pub fn verify(public: &str, version: &str, platform: &str, rel: &PlatformRelease) -> Result<()> {
    let key = verifying_key(public)?;
    let sig: [u8; 64] = b64().decode(rel.signature.trim())?.try_into().map_err(|_| anyhow!("signature must be 64 bytes"))?;
    key.verify(signed_message(version, platform, &rel.sha256).as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| anyhow!("the update's signature doesn't match"))
}

/// The update in `m` for this platform, if newer than `current` and correctly signed.
pub fn evaluate(m: &Manifest, current: &semver::Version, platform: &str, public: &str) -> Result<Option<Available>> {
    let version = semver::Version::parse(m.version.trim_start_matches('v')).with_context(|| format!("bad version {:?}", m.version))?;
    if version <= *current {
        return Ok(None);
    }
    let Some(rel) = m.platforms.get(platform) else { return Ok(None) };
    verify(public, &m.version, platform, rel)?;
    Ok(Some(Available {
        version,
        notes: m.notes.clone(),
        published: m.published.clone(),
        release: rel.clone(),
        installable: cfg!(windows),
    }))
}

fn agent() -> ureq::Agent {
    let ua = format!("ValoLights/{} ({})", env!("CARGO_PKG_VERSION"), platform_key());
    ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20))).user_agent(ua).build().into()
}

fn manifest_url() -> String {
    std::env::var("VALOLIGHTS_UPDATE_URL").unwrap_or_else(|_| MANIFEST_URL.to_string())
}

/// Read a URL (http/https) or a local path (tests: VALOLIGHTS_UPDATE_URL may point at a file).
fn fetch_text(url: &str) -> Result<String> {
    if let Some(p) = url.strip_prefix("file://") {
        return Ok(std::fs::read_to_string(p)?);
    }
    let mut res = agent().get(url).call().with_context(|| format!("could not reach {url}"))?;
    Ok(res.body_mut().read_to_string()?)
}

/// Ask the release server whether a newer version exists.
pub fn check() -> Result<Option<Available>> {
    let text = fetch_text(&manifest_url())?;
    let m: Manifest = serde_json::from_str(&text).context("the update information is unreadable")?;
    evaluate(&m, &current_version(), &platform_key(), PUBLIC_KEY)
}

/// Download the installer into `dir`, checking size and SHA-256. `progress` gets 0..1.
pub fn download(av: &Available, dir: &Path, progress: &dyn Fn(f64)) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = av.release.url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("ValoLights-Setup.exe");
    let out = dir.join(name);
    let part = dir.join(format!("{name}.part"));
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::create(&part)?;
    let mut done: u64 = 0;
    let total = av.release.size.max(1);
    let mut copy = |r: &mut dyn Read| -> Result<()> {
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress((done as f64 / total as f64).min(1.0));
        }
    };
    if let Some(p) = av.release.url.strip_prefix("file://") {
        copy(&mut std::fs::File::open(p)?)?;
    } else {
        let mut res = agent().get(&av.release.url).config().timeout_global(None).build().call().context("the download failed")?;
        copy(&mut res.body_mut().as_reader())?;
    }
    drop(file);
    let got = hex(&hasher.finalize());
    if av.release.size > 0 && done != av.release.size {
        let _ = std::fs::remove_file(&part);
        bail!("the download is incomplete ({done} of {} bytes)", av.release.size);
    }
    if !got.eq_ignore_ascii_case(&av.release.sha256) {
        let _ = std::fs::remove_file(&part);
        bail!("the downloaded file doesn't match the signed checksum");
    }
    let _ = std::fs::remove_file(&out);
    std::fs::rename(&part, &out)?;
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 of a file, lowercase hex.
pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Start the downloaded installer so it updates this installation and relaunches the app; the caller then
/// quits. Windows only (Inno Setup: /SILENT shows just a progress window).
#[cfg(windows)]
pub fn start_install(installer: &Path) -> Result<()> {
    let dir = vl_core::paths::app_root().to_path_buf();
    std::process::Command::new(installer)
        .arg("/SILENT")
        .arg("/SUPPRESSMSGBOXES")
        .arg("/NORESTART")
        .arg("/CLOSEAPPLICATIONS")
        .arg(format!("/DIR={}", dir.display()))
        .arg("/RELAUNCH=1")
        .spawn()
        .context("could not start the installer")?;
    Ok(())
}

#[cfg(not(windows))]
pub fn start_install(_installer: &Path) -> Result<()> {
    bail!("automatic installs aren't available on this platform yet; download the new version from {RELEASES_URL}")
}

/// A build from a source checkout (Cargo.toml next to the app root): no automatic checks, no installs.
pub fn is_dev_build() -> bool {
    vl_core::paths::app_root().join("Cargo.toml").exists()
}

// ------------------------------------------------------------------ release tooling (CLI)

/// A new signing key: (private seed, public key), both base64.
pub fn keygen() -> Result<(String, String)> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| anyhow!("no randomness: {e}"))?;
    let sk = SigningKey::from_bytes(&seed);
    Ok((b64().encode(seed), b64().encode(sk.verifying_key().to_bytes())))
}

/// Sign `installer` for `platform` and write the manifest for a release.
pub fn make_manifest(
    installer: &Path,
    version: &str,
    platform: &str,
    url: &str,
    notes: &str,
    published: &str,
    private_b64: &str,
) -> Result<Manifest> {
    let seed: [u8; 32] = b64().decode(private_b64.trim())?.try_into().map_err(|_| anyhow!("signing key must be 32 bytes"))?;
    let sk = SigningKey::from_bytes(&seed);
    let sha256 = sha256_file(installer)?;
    let size = std::fs::metadata(installer)?.len();
    use ed25519_dalek::Signer;
    let sig = sk.sign(signed_message(version, platform, &sha256).as_bytes());
    let rel = PlatformRelease { url: url.to_string(), sha256, size, signature: b64().encode(sig.to_bytes()) };
    Ok(Manifest {
        version: version.to_string(),
        notes: notes.to_string(),
        published: published.to_string(),
        platforms: BTreeMap::from([(platform.to_string(), rel)]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(dir: &Path, version: &str, private: &str) -> (Manifest, PathBuf) {
        let f = dir.join("setup.exe");
        std::fs::write(&f, b"installer bytes").unwrap();
        let m = make_manifest(&f, version, "windows-x86_64", &format!("file://{}", f.display()), "notes", "2026-11-18", private).unwrap();
        (m, f)
    }

    #[test]
    fn signed_newer_release_is_offered_and_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let (private, public) = keygen().unwrap();
        let (m, _) = manifest(dir.path(), "0.3.0", &private);
        let cur = semver::Version::parse("0.2.0").unwrap();
        let av = evaluate(&m, &cur, "windows-x86_64", &public).unwrap().expect("newer");
        assert_eq!(av.version.to_string(), "0.3.0");
        // same or older: nothing; another platform: nothing
        assert!(evaluate(&m, &semver::Version::parse("0.3.0").unwrap(), "windows-x86_64", &public).unwrap().is_none());
        assert!(evaluate(&m, &cur, "linux-x86_64", &public).unwrap().is_none());
        let got = download(&av, &dir.path().join("dl"), &|_| {}).unwrap();
        assert_eq!(std::fs::read(got).unwrap(), b"installer bytes");
    }

    #[test]
    fn tampering_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (private, public) = keygen().unwrap();
        let (_, other_public) = keygen().unwrap();
        let (m, f) = manifest(dir.path(), "0.3.0", &private);
        let cur = semver::Version::parse("0.2.0").unwrap();
        // signed by someone else
        assert!(evaluate(&m, &cur, "windows-x86_64", &other_public).is_err());
        // version bumped without re-signing
        let mut bumped = m.clone();
        bumped.version = "9.0.0".into();
        assert!(evaluate(&bumped, &cur, "windows-x86_64", &public).is_err());
        // the file changed after signing: offered (manifest is fine) but the download is rejected
        let av = evaluate(&m, &cur, "windows-x86_64", &public).unwrap().unwrap();
        std::fs::write(&f, b"something else!").unwrap();
        assert!(download(&av, &dir.path().join("dl"), &|_| {}).is_err());
    }

    #[test]
    fn platform_key_has_os_and_arch() {
        let k = platform_key();
        assert!(k.contains('-') && !k.starts_with('-'));
        #[cfg(all(windows, target_arch = "x86_64"))]
        assert_eq!(k, "windows-x86_64");
    }
}
