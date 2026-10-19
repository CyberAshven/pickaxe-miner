//! #### PR #61: P2Pool's releases: the newest one for this platform, its
//! download checked against the SHA-256 published beside it, unpacked into
//! its own version folder, with the previous version kept to roll back to.
use super::http;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The BCH P2Pool fork whose releases Pickaxe installs.
pub const REPOSITORY: &str = "CyberAshven/BitcoinCash-p2poolv2";
/// Every release, newest first: `releases/latest` skips pre-releases, and
/// P2Pool has published only pre-releases so far.
const RELEASES: &str =
    "https://api.github.com/repos/CyberAshven/BitcoinCash-p2poolv2/releases?per_page=30";
/// The largest archive Pickaxe downloads.
const ARCHIVE_LIMIT: usize = 256 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);

/// The node binary inside a release archive.
pub const BINARY: &str = if cfg!(windows) {
    "p2poolv2.exe"
} else {
    "p2poolv2"
};

/// This platform's archive among a release's assets, as P2Pool's release
/// workflow names them.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("p2poolv2_node-x86_64-pc-windows-msvc.zip"),
        ("linux", "x86_64") => Some("p2poolv2_node-x86_64-unknown-linux-gnu.tar.xz"),
        ("linux", "aarch64") => Some("p2poolv2_node-aarch64-unknown-linux-gnu.tar.xz"),
        ("macos", "aarch64") => Some("p2poolv2_node-aarch64-apple-darwin.tar.xz"),
        _ => None,
    }
}

fn platform_asset() -> Result<&'static str, String> {
    asset_name().ok_or_else(|| {
        format!(
            "P2Pool publishes no release for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })
}

/// A release that has this platform's archive and its `.sha256`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub published_at: String,
    pub prerelease: bool,
    pub asset: String,
    pub asset_url: String,
    pub sha256_url: String,
    /// The SHA-256 GitHub itself records for the archive (its `digest`),
    /// a second source besides the published `.sha256` file.
    pub digest: Option<[u8; 32]>,
}

/// The newest release in GitHub's release list with `asset` and its
/// `.sha256` beside it; drafts and releases without them are skipped.
pub fn newest(releases: &serde_json::Value, asset: &str) -> Option<Release> {
    let item = |release: &serde_json::Value, name: &str| {
        release["assets"]
            .as_array()?
            .iter()
            .find(|item| item["name"].as_str() == Some(name))
            .cloned()
    };
    let download = |release: &serde_json::Value, name: &str| {
        item(release, name)?["browser_download_url"]
            .as_str()
            .map(str::to_owned)
    };
    let digest = |release: &serde_json::Value| {
        let text = item(release, asset)?["digest"].as_str()?.to_owned();
        hex::decode(text.strip_prefix("sha256:")?)
            .ok()?
            .try_into()
            .ok()
    };
    releases
        .as_array()?
        .iter()
        .filter(|release| !release["draft"].as_bool().unwrap_or(false))
        .filter_map(|release| {
            Some(Release {
                tag: release["tag_name"].as_str()?.to_owned(),
                published_at: release["published_at"].as_str().unwrap_or("").to_owned(),
                prerelease: release["prerelease"].as_bool().unwrap_or(false),
                asset: asset.to_owned(),
                asset_url: download(release, asset)?,
                sha256_url: download(release, &format!("{asset}.sha256"))?,
                digest: digest(release),
            })
        })
        .max_by(|a, b| a.published_at.cmp(&b.published_at))
}

/// The installed P2Pool, as `installed.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    pub tag: String,
    pub binary: PathBuf,
    /// The archive's SHA-256, as published and checked.
    pub sha256: String,
    pub installed_at: u64,
    /// The version before it, kept to roll back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Box<Installed>>,
}

fn record_path(home: &Path) -> PathBuf {
    home.join("installed.json")
}

/// The installed version, if any.
pub fn installed(home: &Path) -> Result<Option<Installed>, String> {
    match std::fs::read_to_string(record_path(home)) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("{}: {error}", record_path(home).display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", record_path(home).display())),
    }
}

/// What the release list says about the installed version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    UpToDate(String),
    /// A release other than the installed one is the newest (or none is
    /// installed).
    Available {
        current: Option<String>,
        release: Release,
    },
}

/// Compares the newest release with the installed version.
pub fn compare(installed: Option<&Installed>, newest: Release) -> Check {
    match installed {
        Some(current) if current.tag == newest.tag => Check::UpToDate(current.tag.clone()),
        current => Check::Available {
            current: current.map(|current| current.tag.clone()),
            release: newest,
        },
    }
}

/// Reads P2Pool's release list and compares it with the installed version.
pub fn check(home: &Path) -> Result<Check, String> {
    let asset = platform_asset()?;
    let list = http::get(RELEASES, "application/vnd.github+json", 4 << 20, TIMEOUT)?
        .ok("P2Pool's release list")?;
    let list: serde_json::Value =
        serde_json::from_slice(&list).map_err(|error| format!("P2Pool's release list: {error}"))?;
    let newest = newest(&list, asset)
        .ok_or_else(|| format!("no release of {REPOSITORY} has {asset} and its .sha256"))?;
    Ok(compare(installed(home)?.as_ref(), newest))
}

/// Downloads `release`, checks it against its published SHA-256 (and the
/// one GitHub records, when the release list gives it) and installs it.
pub fn install(home: &Path, release: &Release) -> Result<Installed, String> {
    let published =
        http::get(&release.sha256_url, "*/*", 64 << 10, TIMEOUT)?.ok("the release's .sha256")?;
    let expected = parse_sha256(&String::from_utf8_lossy(&published), &release.asset)?;
    if release.digest.is_some_and(|digest| digest != expected) {
        return Err(format!(
            "{}: its .sha256 file and GitHub's own digest disagree; nothing was installed",
            release.asset
        ));
    }
    let archive = http::get(
        &release.asset_url,
        "application/octet-stream",
        ARCHIVE_LIMIT,
        TIMEOUT,
    )?
    .ok(&release.asset)?;
    install_archive(home, &release.tag, &release.asset, &archive, expected)
}

/// The SHA-256 a `.sha256` file publishes: `HEX`, `HEX  NAME` or
/// `HEX *NAME`; a line that names a file must name `asset`.
pub(crate) fn parse_sha256(text: &str, asset: &str) -> Result<[u8; 32], String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(hex_text) = parts.next() else {
            continue;
        };
        if let Some(name) = parts.next() {
            let name = name.trim_start_matches('*');
            let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
            if name != asset {
                continue;
            }
        }
        let bytes = hex::decode(hex_text).map_err(|_| format!("bad SHA-256 {hex_text}"))?;
        return bytes
            .try_into()
            .map_err(|_| format!("bad SHA-256 {hex_text}"));
    }
    Err(format!("the .sha256 file names no checksum for {asset}"))
}

/// Installs a downloaded archive whose SHA-256 must be `expected`: unpacks
/// it into `versions/<tag>`, finds the node binary and records it, keeping
/// the version before it.
pub fn install_archive(
    home: &Path,
    tag: &str,
    asset: &str,
    archive: &[u8],
    expected: [u8; 32],
) -> Result<Installed, String> {
    let actual = sha256(archive);
    if actual != expected {
        return Err(format!(
            "{asset}: the download's SHA-256 is {}, not the published {}; nothing was installed",
            hex::encode(actual),
            hex::encode(expected)
        ));
    }
    let versions = home.join("versions");
    let folder = safe_name(tag);
    let staging = versions.join(format!(".{folder}.partial"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| format!("{}: {error}", staging.display()))?;
    let archive_path = staging.join(safe_name(asset));
    std::fs::write(&archive_path, archive)
        .map_err(|error| format!("{}: {error}", archive_path.display()))?;
    let unpacked = extract(&archive_path, &staging);
    let _ = std::fs::remove_file(&archive_path);
    unpacked?;
    let found = find_binary(&staging, 3)
        .ok_or_else(|| format!("{asset} has no {BINARY}; nothing was installed"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&found, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("{}: {error}", found.display()))?;
    }
    let destination = versions.join(&folder);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)
            .map_err(|error| format!("{}: {error}", destination.display()))?;
    }
    std::fs::rename(&staging, &destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    let binary = destination.join(found.strip_prefix(&staging).unwrap_or(&found));
    let previous = installed(home)?
        .filter(|current| current.tag != tag)
        .map(|mut current| {
            current.previous = None;
            Box::new(current)
        });
    let record = Installed {
        tag: tag.to_owned(),
        binary,
        sha256: hex::encode(expected),
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        previous,
    };
    save(home, &record)?;
    Ok(record)
}

fn save(home: &Path, record: &Installed) -> Result<(), String> {
    let text = serde_json::to_string_pretty(record).map_err(|error| error.to_string())?;
    super::write_atomically(&record_path(home), text.as_bytes())
}

/// Goes back to the version before the installed one; the installed one
/// becomes the version to go back to.
pub fn rollback(home: &Path) -> Result<Installed, String> {
    let current = installed(home)?.ok_or("P2Pool is not installed")?;
    let mut previous = *current
        .previous
        .clone()
        .ok_or("there is no earlier P2Pool version to go back to")?;
    if !previous.binary.is_file() {
        return Err(format!("{} is gone", previous.binary.display()));
    }
    previous.previous = Some(Box::new(Installed {
        previous: None,
        ..current
    }));
    save(home, &previous)?;
    Ok(previous)
}

/// The SHA-256 of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(bytes));
    out
}

/// A tag or file name safe as one path component.
fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    match safe.trim_matches('.') {
        "" => "_".into(),
        trimmed => trimmed.to_owned(),
    }
}

/// Unpacks `archive` (zip or tar.xz) into `into` with the system's `tar`,
/// which Windows 10 and later, Linux and macOS have; it refuses members
/// outside the folder.
fn extract(archive: &Path, into: &Path) -> Result<(), String> {
    let output = std::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|error| format!("unpack with tar: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "unpack {}: {}",
            archive.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The node binary in `dir` or a folder below it, at most `depth` deep.
fn find_binary(dir: &Path, depth: usize) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries
        .iter()
        .find(|entry| entry.file_name() == BINARY && entry.path().is_file())
        .map(|entry| entry.path())
        .or_else(|| {
            (depth > 0)
                .then(|| {
                    entries
                        .iter()
                        .filter(|entry| entry.path().is_dir())
                        .find_map(|entry| find_binary(&entry.path(), depth - 1))
                })
                .flatten()
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    fn asset(name: &str) -> serde_json::Value {
        json!({ "name": name, "browser_download_url": format!("https://example.test/{name}") })
    }

    #[test]
    fn the_newest_release_with_this_platforms_archive_and_checksum_wins() {
        let zip = "p2poolv2_node-x86_64-pc-windows-msvc.zip";
        let list = json!([
            { "tag_name": "v0.0.3", "draft": true, "published_at": "2026-12-01T00:00:00Z",
              "assets": [asset(zip), asset(&format!("{zip}.sha256"))] },
            { "tag_name": "v0.0.2", "prerelease": true, "published_at": "2026-11-01T00:00:00Z",
              "assets": [asset(zip)] },
            { "tag_name": "v0.0.1-alpha", "prerelease": true, "published_at": "2026-10-18T00:00:00Z",
              "assets": [
                  { "name": zip, "browser_download_url": format!("https://example.test/{zip}"),
                    "digest": format!("sha256:{}", "cd".repeat(32)) },
                  asset(&format!("{zip}.sha256")),
                  asset("other.tar.xz")
              ] },
            { "tag_name": "v0.0.0", "published_at": "2026-01-01T00:00:00Z",
              "assets": [asset(zip), asset(&format!("{zip}.sha256"))] }
        ]);
        let release = newest(&list, zip).unwrap();
        assert_eq!(release.tag, "v0.0.1-alpha");
        assert!(release.prerelease);
        assert_eq!(release.asset_url, format!("https://example.test/{zip}"));
        assert_eq!(
            release.sha256_url,
            format!("https://example.test/{zip}.sha256")
        );
        assert_eq!(release.digest, Some([0xcd; 32]));
        assert!(newest(&list, "p2poolv2_node-riscv64.tar.xz").is_none());
        let current = Installed {
            tag: "v0.0.1-alpha".into(),
            binary: "x".into(),
            sha256: String::new(),
            installed_at: 0,
            previous: None,
        };
        assert_eq!(
            compare(Some(&current), release.clone()),
            Check::UpToDate("v0.0.1-alpha".into())
        );
        assert!(matches!(
            compare(None, release),
            Check::Available { current: None, .. }
        ));
    }

    #[test]
    fn a_sha256_file_names_this_archive_or_only_a_checksum() {
        let sum = "ab".repeat(32);
        assert_eq!(parse_sha256(&sum, "a.zip").unwrap(), [0xab; 32]);
        assert_eq!(
            parse_sha256(&format!("{} *dist/a.zip\n", sum), "a.zip").unwrap(),
            [0xab; 32]
        );
        assert!(parse_sha256(&format!("{sum}  b.zip"), "a.zip")
            .unwrap_err()
            .contains("no checksum for a.zip"));
        assert!(parse_sha256("xyz", "a.zip").is_err());
        assert_eq!(safe_name("../v1/../2"), "_v1_.._2");
        assert_eq!(safe_name(".."), "_");
    }

    /// An archive of this platform's kind holding `files` under `top/`.
    pub(crate) fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let dir = super::super::test_dir("archive");
        let top = dir.join("p2poolv2_node");
        std::fs::create_dir_all(&top).unwrap();
        for (name, bytes) in files {
            std::fs::write(top.join(name), bytes).unwrap();
        }
        let name = asset_name().unwrap_or("p2poolv2_node.tar.xz");
        let out = dir.join(name);
        let mut tar = std::process::Command::new("tar");
        if name.ends_with(".zip") {
            tar.arg("-a").arg("-c").arg("-f");
        } else {
            tar.arg("-c").arg("-J").arg("-f");
        }
        let status = tar
            .arg(&out)
            .arg("-C")
            .arg(&dir)
            .arg("p2poolv2_node")
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&out).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        bytes
    }

    #[test]
    fn a_verified_archive_installs_and_rolls_back_and_a_wrong_one_installs_nothing() {
        let home = super::super::test_dir("install");
        let asset = asset_name().unwrap_or("p2poolv2_node.tar.xz");
        let first = archive(&[(BINARY, b"node one"), ("README.md", b"hi")]);
        let sum = sha256(&first);
        // A download that is not the published file installs nothing.
        let error = install_archive(&home, "v1", asset, b"swapped", sum).unwrap_err();
        assert!(error.contains("nothing was installed"), "{error}");
        assert_eq!(installed(&home).unwrap(), None);
        let one = install_archive(&home, "v1", asset, &first, sum).unwrap();
        assert_eq!(std::fs::read(&one.binary).unwrap(), b"node one");
        assert!(one.binary.starts_with(home.join("versions").join("v1")));
        assert_eq!(one.previous, None);
        let second = archive(&[(BINARY, b"node two")]);
        let two = install_archive(&home, "v2", asset, &second, sha256(&second)).unwrap();
        assert_eq!(std::fs::read(&two.binary).unwrap(), b"node two");
        assert_eq!(two.previous.as_ref().unwrap().tag, "v1");
        assert_eq!(installed(&home).unwrap().unwrap(), two);
        // Back to v1; v2 is now the one to go back to.
        let back = rollback(&home).unwrap();
        assert_eq!(back.tag, "v1");
        assert_eq!(back.previous.as_ref().unwrap().tag, "v2");
        assert_eq!(
            std::fs::read(&installed(&home).unwrap().unwrap().binary).unwrap(),
            b"node one"
        );
        // An archive without the binary installs nothing either.
        let empty = archive(&[("README.md", b"no node")]);
        let error = install_archive(&home, "v3", asset, &empty, sha256(&empty)).unwrap_err();
        assert!(error.contains(BINARY), "{error}");
        assert_eq!(installed(&home).unwrap().unwrap().tag, "v1");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[ignore = "reads P2Pool's release list from GitHub"]
    fn p2pools_release_list_has_this_platforms_archive() {
        let home = super::super::test_dir("live-check");
        let check = check(&home).unwrap();
        eprintln!("{check:?}");
        assert!(matches!(check, Check::Available { current: None, .. }));
        std::fs::remove_dir_all(home).unwrap();
    }
}
