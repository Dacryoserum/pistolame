//! Self-update from GitHub releases (docs/plans/self-update.md).
//!
//! 1. *Check*: list the repository's published releases, keep the newest version that ships a
//!    `latest.json` manifest and is newer than this build.
//! 2. *Download and verify*: fetch this platform's archive, check its SHA-256 and its ed25519
//!    signature against the public key built into the app. The private key never leaves the
//!    release workflow's secrets, so a tampered download is refused.
//! 3. *Install*: unpack next to the running copy, swap by renames (the old copy is kept until
//!    the new one has started), relaunch.
//!
//! Network access goes through the system's `curl` (macOS, Windows 10+, Linux): no TLS stack is
//! linked into the app. Only the GitHub API and release downloads are contacted.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail, ensure};
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Version of this build (Cargo workspace version).
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Published releases, newest first (the API also lists prereleases, which `/latest` skips).
const RELEASES_API_URL: &str =
    "https://api.github.com/repos/Dacryoserum/pistolame/releases?per_page=20";
/// Name of the manifest asset every release ships.
const MANIFEST_ASSET_NAME: &str = "latest.json";
/// Public half of the release signing key (ed25519, 32 bytes, base64). The private half is the
/// `UPDATE_SIGNING_KEY` secret of the release workflow.
const UPDATE_PUBLIC_KEY_BASE64: &str = "7yP+K5HmqFT4mFXHnwmuMkSb0rood3JzLnnXZT5qpS4=";
/// Longest time one HTTP request may take, in seconds (archives are ~10 MB).
const DOWNLOAD_TIMEOUT_SECONDS: &str = "300";
/// Suffix of the previous copy kept until the new one has started.
const PREVIOUS_COPY_SUFFIX: &str = "old";

/// The archive name of this platform in a release, as `package_release.py` names it.
pub(crate) fn platform_asset_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos-universal"
    } else if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

/// `latest.json`, written by the release workflow.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct UpdateManifest {
    /// Version without the leading `v`, e.g. `0.1.0-preview.4`.
    pub(crate) version: String,
    /// Release notes shown in the update panel.
    pub(crate) notes: String,
    /// One archive per platform key.
    pub(crate) assets: std::collections::HashMap<String, UpdateAsset>,
}

/// One downloadable archive.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct UpdateAsset {
    /// Download URL.
    pub(crate) url: String,
    /// Lowercase hexadecimal SHA-256 of the archive.
    pub(crate) sha256: String,
    /// Base64 ed25519 signature of the archive bytes.
    pub(crate) signature: String,
}

/// A newer version this platform can install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AvailableUpdate {
    pub(crate) version: String,
    pub(crate) notes: String,
    pub(crate) asset: UpdateAsset,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Looks for a newer release. `Ok(None)` when this build is the newest.
pub(crate) fn check_for_update() -> anyhow::Result<Option<AvailableUpdate>> {
    let releases: Vec<GithubRelease> = serde_json::from_slice(&fetch(RELEASES_API_URL)?)
        .context("liste des versions illisible")?;
    let Some(manifest_url) = newest_manifest_url(&releases, CURRENT_VERSION) else {
        return Ok(None);
    };
    let manifest: UpdateManifest =
        serde_json::from_slice(&fetch(&manifest_url)?).context("latest.json illisible")?;
    Ok(update_for_platform(
        manifest,
        CURRENT_VERSION,
        platform_asset_key(),
    ))
}

/// The `latest.json` URL of the newest published release newer than `current_version`.
fn newest_manifest_url(releases: &[GithubRelease], current_version: &str) -> Option<String> {
    let current = semver::Version::parse(current_version).ok()?;
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = semver::Version::parse(release.tag_name.trim_start_matches('v')).ok()?;
            let manifest = release
                .assets
                .iter()
                .find(|asset| asset.name == MANIFEST_ASSET_NAME)?;
            (version > current).then(|| (version, manifest.browser_download_url.clone()))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, url)| url)
}

/// The update for `platform`, if the manifest is newer and ships an archive for it.
fn update_for_platform(
    manifest: UpdateManifest,
    current_version: &str,
    platform: &str,
) -> Option<AvailableUpdate> {
    let is_newer = semver::Version::parse(&manifest.version).ok()?
        > semver::Version::parse(current_version).ok()?;
    let asset = manifest.assets.get(platform)?.clone();
    is_newer.then_some(AvailableUpdate {
        version: manifest.version,
        notes: manifest.notes,
        asset,
    })
}

/// Downloads the archive of `update` into `folder` and checks its hash and signature.
pub(crate) fn download_and_verify(
    update: &AvailableUpdate,
    folder: &Path,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let file_name = update
        .asset
        .url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && !name.contains(['\\', ':']))
        .context("adresse de téléchargement invalide")?;
    let archive = folder.join(file_name);
    run(Command::new("curl")
        .args(["-fsSL", "--max-time", DOWNLOAD_TIMEOUT_SECONDS, "-o"])
        .arg(&archive)
        .arg(&update.asset.url))
    .context("téléchargement impossible")?;
    let bytes = std::fs::read(&archive)?;
    verify_archive(&bytes, &update.asset, UPDATE_PUBLIC_KEY_BASE64)?;
    Ok(archive)
}

/// Checks the SHA-256 and the ed25519 signature of an archive.
fn verify_archive(
    bytes: &[u8],
    asset: &UpdateAsset,
    public_key_base64: &str,
) -> anyhow::Result<()> {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ensure!(
        digest.eq_ignore_ascii_case(asset.sha256.trim()),
        "l’empreinte SHA-256 ne correspond pas"
    );
    let engine = base64::engine::general_purpose::STANDARD;
    let key: [u8; 32] = engine
        .decode(public_key_base64)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("clé publique invalide"))?;
    let signature: [u8; 64] = engine
        .decode(asset.signature.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature invalide"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&key)?
        .verify_strict(bytes, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| anyhow::anyhow!("la signature de la mise à jour est invalide"))
}

/// What to start once the new copy is in place.
#[derive(Debug)]
pub(crate) struct InstalledUpdate {
    /// The `.app` bundle (macOS) or the executable (Windows, Linux).
    pub(crate) relaunch_target: PathBuf,
}

/// Why the running copy cannot replace itself.
pub(crate) fn installation_blocker() -> Option<&'static str> {
    let executable = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        if executable.to_string_lossy().contains("/AppTranslocation/") {
            return Some(
                "Déplacez Pistolame dans le dossier Applications pour pouvoir le mettre à jour.",
            );
        }
        if app_bundle_of(&executable).is_none() {
            return Some("Cette copie de développement ne se met pas à jour toute seule.");
        }
    }
    None
}

/// Unpacks `archive` and puts the new copy in place of the running one. The running copy is
/// renamed with the `.old` suffix (deleted at the next start); if anything fails before the
/// swap, the running copy is untouched.
pub(crate) fn install(archive: &Path, work_folder: &Path) -> anyhow::Result<InstalledUpdate> {
    if let Some(reason) = installation_blocker() {
        bail!(reason);
    }
    let unpacked = work_folder.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked)?;
    unpack(archive, &unpacked)?;
    let executable = std::env::current_exe()?;
    if cfg!(target_os = "macos") {
        let bundle = app_bundle_of(&executable).context("bundle introuvable")?;
        let new_bundle = find(&unpacked, |path| {
            path.extension().is_some_and(|extension| extension == "app")
        })
        .context("Pistolame.app absent de l’archive")?;
        let staged = bundle.with_extension("app.update");
        let _ = std::fs::remove_dir_all(&staged);
        // ditto keeps the bundle's permissions, symlinks and signature intact.
        run(Command::new("ditto").arg(&new_bundle).arg(&staged))?;
        swap(&bundle, &staged)?;
        Ok(InstalledUpdate {
            relaunch_target: bundle,
        })
    } else {
        let name = executable
            .file_name()
            .context("exécutable sans nom")?
            .to_owned();
        let new_executable = find(&unpacked, |path| path.file_name() == Some(&name))
            .context("exécutable absent de l’archive")?;
        let staged = executable.with_extension("update");
        std::fs::copy(&new_executable, &staged)?;
        swap(&executable, &staged)?;
        Ok(InstalledUpdate {
            relaunch_target: executable,
        })
    }
}

/// Starts the new copy. The caller then quits.
pub(crate) fn relaunch(installed: &InstalledUpdate) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        run(Command::new("open")
            .arg("-n")
            .arg(&installed.relaunch_target))
    } else {
        Command::new(&installed.relaunch_target)
            .spawn()
            .map(|_| ())
            .context("redémarrage impossible")
    }
}

/// Deletes the copy an update replaced. Called at start-up, once the new copy runs.
pub(crate) fn clean_up_previous_copy() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let current = if cfg!(target_os = "macos") {
        match app_bundle_of(&executable) {
            Some(bundle) => bundle,
            None => return,
        }
    } else {
        executable
    };
    let previous = previous_copy_path(&current);
    if previous.is_dir() {
        let _ = std::fs::remove_dir_all(&previous);
    } else if previous.exists() {
        let _ = std::fs::remove_file(&previous);
    }
}

fn previous_copy_path(current: &Path) -> PathBuf {
    let mut name = current.as_os_str().to_owned();
    name.push(".");
    name.push(PREVIOUS_COPY_SUFFIX);
    PathBuf::from(name)
}

/// `current` becomes `current.old`, `staged` becomes `current`. Renames within one folder, so
/// each step is atomic; a failed second step puts the old copy back.
fn swap(current: &Path, staged: &Path) -> anyhow::Result<()> {
    let previous = previous_copy_path(current);
    if previous.is_dir() {
        std::fs::remove_dir_all(&previous)?;
    } else if previous.exists() {
        std::fs::remove_file(&previous)?;
    }
    std::fs::rename(current, &previous).context("impossible de remplacer l’application")?;
    if let Err(error) = std::fs::rename(staged, current) {
        let _ = std::fs::rename(&previous, current);
        return Err(error).context("impossible d’installer la nouvelle version");
    }
    Ok(())
}

fn unpack(archive: &Path, destination: &Path) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        run(Command::new("ditto")
            .args(["-x", "-k"])
            .arg(archive)
            .arg(destination))
    } else {
        // bsdtar (Windows 10+, macOS) and GNU tar read the zip and tar.gz archives we publish.
        run(Command::new("tar")
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(destination))
    }
}

/// The `.app` bundle containing `executable` (`…/Pistolame.app/Contents/MacOS/Pistolame`).
fn app_bundle_of(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

/// First entry under `root` (depth-first) matching `predicate`.
fn find(root: &Path, predicate: impl Fn(&Path) -> bool + Copy) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if predicate(&path) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find(&path, predicate)
        {
            return Some(found);
        }
    }
    None
}

/// The bytes of `url`, through the system's curl.
fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "30",
            "-H",
            "Accept: application/vnd.github+json",
        ])
        .arg(url)
        .output()
        .context("curl introuvable")?;
    ensure!(output.status.success(), "connexion à GitHub impossible");
    Ok(output.stdout)
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status().context("commande introuvable")?;
    ensure!(status.success(), "la commande a échoué ({status})");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    fn release(tag: &str, draft: bool, has_manifest: bool) -> GithubRelease {
        GithubRelease {
            tag_name: tag.into(),
            draft,
            assets: if has_manifest {
                vec![GithubAsset {
                    name: MANIFEST_ASSET_NAME.into(),
                    browser_download_url: format!("https://example.test/{tag}/latest.json"),
                }]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn the_newest_published_release_with_a_manifest_wins() {
        let releases = vec![
            release("v0.1.0-preview.5", true, true),
            release("v0.1.0-preview.4", false, false),
            release("v0.1.0-preview.3", false, true),
            release("v0.1.0-preview.10", false, true),
        ];
        assert_eq!(
            newest_manifest_url(&releases, "0.1.0-preview.2"),
            Some("https://example.test/v0.1.0-preview.10/latest.json".into())
        );
        assert_eq!(newest_manifest_url(&releases, "0.1.0-preview.10"), None);
        // A stable version is newer than its previews.
        assert!(
            newest_manifest_url(&[release("v0.1.0", false, true)], "0.1.0-preview.9").is_some()
        );
    }

    #[test]
    fn a_manifest_without_this_platform_offers_nothing() {
        let manifest: UpdateManifest = serde_json::from_str(
            r#"{"version":"0.1.0-preview.4","notes":"Notes","assets":{"linux-x64":{"url":"u","sha256":"s","signature":"g"}}}"#,
        )
        .unwrap();
        assert!(
            update_for_platform(manifest.clone(), "0.1.0-preview.3", "macos-universal").is_none()
        );
        assert!(update_for_platform(manifest.clone(), "0.1.0-preview.4", "linux-x64").is_none());
        assert_eq!(
            update_for_platform(manifest, "0.1.0-preview.3", "linux-x64")
                .map(|update| update.version),
            Some("0.1.0-preview.4".into())
        );
    }

    fn signed_asset(bytes: &[u8]) -> (UpdateAsset, String) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let engine = base64::engine::general_purpose::STANDARD;
        let asset = UpdateAsset {
            url: "https://example.test/Pistolame.zip".into(),
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            signature: engine.encode(key.sign(bytes).to_bytes()),
        };
        (asset, engine.encode(key.verifying_key().to_bytes()))
    }

    #[test]
    fn a_correctly_signed_archive_is_accepted() {
        let (asset, public_key) = signed_asset(b"archive bytes");
        verify_archive(b"archive bytes", &asset, &public_key).unwrap();
    }

    #[test]
    fn a_tampered_archive_or_signature_is_refused() {
        let (mut asset, public_key) = signed_asset(b"archive bytes");
        assert!(verify_archive(b"tampered bytes", &asset, &public_key).is_err());
        // Matching hash but a signature from another key.
        let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        asset.signature = base64::engine::general_purpose::STANDARD
            .encode(other.sign(b"archive bytes").to_bytes());
        assert!(verify_archive(b"archive bytes", &asset, &public_key).is_err());
    }

    #[test]
    fn the_built_in_public_key_is_a_valid_ed25519_key() {
        let bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(UPDATE_PUBLIC_KEY_BASE64)
            .unwrap()
            .try_into()
            .unwrap();
        assert!(ed25519_dalek::VerifyingKey::from_bytes(&bytes).is_ok());
    }

    #[test]
    fn swapping_keeps_the_previous_copy_and_restores_nothing_on_success() {
        let folder = std::env::temp_dir().join(format!("pistolame-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let current = folder.join("Pistolame");
        let staged = folder.join("Pistolame.update");
        std::fs::write(&current, "old").unwrap();
        std::fs::write(&staged, "new").unwrap();
        swap(&current, &staged).unwrap();
        assert_eq!(std::fs::read_to_string(&current).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(previous_copy_path(&current)).unwrap(),
            "old"
        );
        assert!(!staged.exists());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
