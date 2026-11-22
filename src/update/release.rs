//! Pickaxe's releases on GitHub: the newest full release, this platform's
//! archive in it, the checksum file and its signature, and the one request
//! that checks for a newer one (ETag-aware, so an unchanged answer costs
//! GitHub nothing).
use crate::http::{self, Fetch};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The repository whose releases Pickaxe updates from.
pub const REPOSITORY: &str = "CyberAshven/pickaxe-miner";
/// Release tags: `pickaxe-miner-v0.0.5` (release.yml).
pub const TAG_PREFIX: &str = "pickaxe-miner-v";
/// The binary in a release archive (release.yml renames it to this).
pub const BINARY: &str = if cfg!(windows) {
    "pickaxe.exe"
} else {
    "pickaxe"
};
const TIMEOUT: Duration = Duration::from_secs(30);
/// The largest release description taken.
const LIST_LIMIT: usize = 1 << 20;
/// The largest checksum file and signature taken.
const SUMS_LIMIT: usize = 64 << 10;
const SIGNATURE_LIMIT: usize = 4 << 10;

/// A version as Pickaxe's tags and Cargo.toml give it: MAJOR.MINOR.PATCH.
/// Pre-releases (a `-` suffix) never parse, so they are never offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// `0.0.5`, `v0.0.5` or `pickaxe-miner-v0.0.5`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(TAG_PREFIX).unwrap_or(text);
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut number = || -> Option<u64> {
            let part = parts.next()?;
            // No signs, spaces or leading zeros: one spelling per version.
            if part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return None;
            }
            part.parse().ok()
        };
        let version = Self(number()?, number()?, number()?);
        parts.next().is_none().then_some(version)
    }

    /// The version running now.
    pub fn running() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self(0, 0, 0))
    }

    /// Its release tag.
    pub fn tag(self) -> String {
        format!("{TAG_PREFIX}{}.{}.{}", self.0, self.1, self.2)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}.{}.{}", self.0, self.1, self.2)
    }
}

/// This platform's archive name for a tag, as release.yml names it.
pub fn asset_name(tag: &str) -> Option<String> {
    let suffix = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64.zip",
        ("windows", "aarch64") => "windows-arm64.zip",
        ("linux", "x86_64") => "linux-x86_64.tar.gz",
        ("linux", "aarch64") => "linux-arm64.tar.gz",
        ("macos", "aarch64") => "macos-arm64.tar.gz",
        _ => return None,
    };
    Some(format!("{tag}-{suffix}"))
}

/// A release with this platform's archive and the checksum file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    /// The release's page: its notes.
    pub notes_url: String,
    pub asset: String,
    pub asset_url: String,
    /// The archive's size as GitHub gives it; a larger download is refused.
    pub asset_size: u64,
    /// The archive's SHA-256 as GitHub itself records it, when it does.
    #[serde(default)]
    pub digest: Option<String>,
    pub sums_url: String,
    /// The checksum file's minisign signature, when the release has one.
    #[serde(default)]
    pub signature_url: Option<String>,
}

/// The release in GitHub's `releases/latest` answer, when it is a full
/// release (not a draft or pre-release) with a Pickaxe tag, this
/// platform's archive and the checksum file.
pub fn parse_latest(release: &serde_json::Value) -> Result<Release, String> {
    if release["draft"].as_bool().unwrap_or(true) || release["prerelease"].as_bool().unwrap_or(true)
    {
        return Err("the newest release is a draft or pre-release".into());
    }
    let tag = release["tag_name"]
        .as_str()
        .ok_or("the newest release has no tag")?;
    if !tag.starts_with(TAG_PREFIX) {
        return Err(format!("{tag} is not a Pickaxe release tag"));
    }
    let version = Version::parse(tag).ok_or_else(|| format!("{tag}: not a release version"))?;
    // The tag is written from the version, so the archive name in the
    // signed checksum file carries the version that is offered.
    let tag = version.tag();
    let asset = asset_name(&tag).ok_or_else(|| {
        format!(
            "Pickaxe publishes no release for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let assets = release["assets"]
        .as_array()
        .ok_or("the release lists no files")?;
    let find = |name: &str| {
        assets
            .iter()
            .find(|item| item["name"].as_str() == Some(name))
    };
    let url = |item: &serde_json::Value| item["browser_download_url"].as_str().map(str::to_owned);
    let archive = find(&asset).ok_or_else(|| format!("{tag} has no {asset}"))?;
    let (sums_name, sums) = super::trust::SUMS_NAMES
        .iter()
        .find_map(|name| find(name).map(|item| (*name, item)))
        .ok_or_else(|| format!("{tag} has no SHA256SUMS.txt"))?;
    Ok(Release {
        version,
        notes_url: release["html_url"].as_str().map_or_else(
            || format!("https://github.com/{REPOSITORY}/releases"),
            str::to_owned,
        ),
        asset_url: url(archive).ok_or_else(|| format!("{asset} has no download"))?,
        asset_size: archive["size"].as_u64().unwrap_or(0),
        digest: archive["digest"]
            .as_str()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase),
        sums_url: url(sums).ok_or_else(|| format!("{sums_name} has no download"))?,
        signature_url: find(&super::trust::signature_name(sums_name)).and_then(url),
        asset,
        tag,
    })
}

/// Where releases come from and where their downloads may go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// GitHub's `releases/latest` for the repository.
    pub latest_url: String,
    /// Hosts any request or redirect may reach.
    pub hosts: Vec<String>,
    /// Plain http is refused (only tests turn this off, for a local server).
    pub https_only: bool,
}

impl Source {
    /// Pickaxe's GitHub releases: the API, github.com's download links and
    /// GitHub's file hosts they redirect to. Nothing else.
    pub fn github() -> Self {
        Self {
            latest_url: format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
            hosts: [
                "api.github.com",
                "github.com",
                "release-assets.githubusercontent.com",
                "objects.githubusercontent.com",
            ]
            .map(str::to_owned)
            .to_vec(),
            https_only: true,
        }
    }

    /// Whether `url` may be fetched: https (unless a test allows http) on
    /// one of the hosts.
    pub fn allows(&self, url: &str) -> Result<(), String> {
        let (secure, host) = http::host_of(url)?;
        if self.https_only && !secure {
            return Err(format!("{url}: updates come over https only"));
        }
        if !self
            .hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&host))
        {
            return Err(format!("{host} is not one of GitHub's release hosts"));
        }
        Ok(())
    }

    /// GETs `url` within these hosts.
    pub(crate) fn get(
        &self,
        url: &str,
        accept: &str,
        limit: usize,
        etag: Option<&str>,
        progress: Option<&dyn Fn(usize)>,
    ) -> Result<http::Response, String> {
        http::fetch(
            url,
            &Fetch {
                accept,
                limit,
                timeout: TIMEOUT,
                etag,
                allowed: &|url| self.allows(url),
                progress,
            },
        )
    }
}

/// What the last check found, kept in `check.json` so the next check sends
/// its ETag and an unchanged answer is read from here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    pub etag: Option<String>,
    /// When GitHub was last asked (seconds since 1970).
    pub checked_at: u64,
    pub release: Option<Release>,
    /// Why the newest release cannot be offered, when it cannot.
    pub problem: Option<String>,
    /// The release's checksum file and signature, once downloaded.
    pub sums: Option<String>,
    pub signature: Option<String>,
}

impl Cache {
    fn path(home: &Path) -> PathBuf {
        home.join("check.json")
    }

    /// The saved check, or none when there is none or it does not read.
    pub fn load(home: &Path) -> Self {
        std::fs::read_to_string(Self::path(home))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        super::write_file(&Self::path(home), text.as_bytes())
    }
}

/// Asks GitHub for the newest release, sending the saved ETag; saves the
/// answer. The newest release, or why there is none to offer.
pub fn check(source: &Source, home: &Path, now: u64) -> Result<Release, String> {
    let mut cache = Cache::load(home);
    let etag = cache
        .etag
        .as_deref()
        .filter(|_| cache.release.is_some() || cache.problem.is_some());
    let response = source.get(
        &source.latest_url,
        "application/vnd.github+json",
        LIST_LIMIT,
        etag,
        None,
    )?;
    cache.checked_at = now;
    match response.status {
        304 => {}
        200 => {
            let previous = cache.release.as_ref().map(|release| release.tag.clone());
            cache.etag = response.header("etag").map(str::to_owned);
            let parsed = serde_json::from_slice(&response.body)
                .map_err(|error| format!("GitHub's answer: {error}"))
                .and_then(|json| parse_latest(&json));
            match parsed {
                Ok(release) => {
                    if previous.as_deref() != Some(release.tag.as_str()) {
                        cache.sums = None;
                        cache.signature = None;
                    }
                    cache.release = Some(release);
                    cache.problem = None;
                }
                Err(problem) => {
                    cache.release = None;
                    cache.problem = Some(problem);
                }
            }
        }
        404 => {
            cache.etag = None;
            cache.release = None;
            cache.problem = Some("Pickaxe has no published release yet".into());
        }
        status => return Err(format!("GitHub's release list: HTTP {status}")),
    }
    cache.save(home)?;
    match (cache.release, cache.problem) {
        (Some(release), _) => Ok(release),
        (None, Some(problem)) => Err(problem),
        (None, None) => Err("GitHub said nothing changed, but nothing is saved".into()),
    }
}

/// The release's checksum file and signature (if it has one), from the
/// cache or downloaded once and saved.
pub fn signature_files(
    source: &Source,
    home: &Path,
    release: &Release,
) -> Result<(String, Option<String>), String> {
    let mut cache = Cache::load(home);
    let current = cache.release.as_ref() == Some(release);
    if current {
        if let Some(sums) = &cache.sums {
            return Ok((sums.clone(), cache.signature.clone()));
        }
    }
    let sums = source
        .get(&release.sums_url, "*/*", SUMS_LIMIT, None, None)?
        .ok("the release's SHA256SUMS")?;
    let sums = String::from_utf8(sums).map_err(|_| "the release's SHA256SUMS is not text")?;
    let signature = match &release.signature_url {
        Some(url) => {
            let body = source
                .get(url, "*/*", SIGNATURE_LIMIT, None, None)?
                .ok("the release's signature")?;
            Some(String::from_utf8(body).map_err(|_| "the release's signature is not text")?)
        }
        None => None,
    };
    if current {
        cache.sums = Some(sums.clone());
        cache.signature = signature.clone();
        cache.save(home)?;
    }
    Ok((sums, signature))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// GitHub's `releases/latest` answer for `tag` with this platform's
    /// archive at `base`, the checksum file and, if `signed`, its signature.
    pub(crate) fn latest_json(base: &str, tag: &str, size: u64, signed: bool) -> serde_json::Value {
        let asset = asset_name(tag).unwrap_or_else(|| format!("{tag}-other.tar.gz"));
        let mut assets = vec![
            json!({ "name": asset, "size": size,
                    "browser_download_url": format!("{base}/download/{asset}") }),
            json!({ "name": "SHA256SUMS.txt", "size": 100,
                    "browser_download_url": format!("{base}/download/SHA256SUMS.txt") }),
            json!({ "name": "pickaxe-hiveos-0.0.5.tar.gz", "size": 1,
                    "browser_download_url": format!("{base}/download/hive") }),
        ];
        if signed {
            assets.push(json!({ "name": "SHA256SUMS.txt.minisig", "size": 300,
                "browser_download_url": format!("{base}/download/SHA256SUMS.txt.minisig") }));
        }
        json!({
            "tag_name": tag, "draft": false, "prerelease": false,
            "html_url": format!("{base}/releases/tag/{tag}"),
            "assets": assets,
        })
    }

    #[test]
    fn versions_compare_by_number_and_pre_releases_never_parse() {
        assert_eq!(
            Version::parse("pickaxe-miner-v0.0.5"),
            Some(Version(0, 0, 5))
        );
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.10.0"), Some(Version(0, 10, 0)));
        assert!(Version(0, 10, 0) > Version(0, 9, 99));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert!(Version(0, 0, 5) > Version(0, 0, 4));
        for bad in [
            "0.0.5-rc1",
            "0.0",
            "0.0.5.1",
            "v",
            "0.0.x",
            "00.0.5",
            "+1.0.0",
            "0.0. 5",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version(0, 0, 5).tag(), "pickaxe-miner-v0.0.5");
        assert_eq!(Version(0, 0, 5).to_string(), "v0.0.5");
        assert_eq!(
            Version::running(),
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
        );
    }

    #[test]
    fn the_latest_full_release_gives_this_platforms_archive_and_signature() {
        let json = latest_json("https://github.com/x", "pickaxe-miner-v0.0.5", 51, true);
        let release = parse_latest(&json).unwrap();
        assert_eq!(release.version, Version(0, 0, 5));
        assert_eq!(release.tag, "pickaxe-miner-v0.0.5");
        assert_eq!(release.asset, asset_name("pickaxe-miner-v0.0.5").unwrap());
        assert_eq!(release.asset_size, 51);
        assert!(release.sums_url.ends_with("/SHA256SUMS.txt"));
        assert!(release
            .signature_url
            .as_deref()
            .unwrap()
            .ends_with("SHA256SUMS.txt.minisig"));
        assert!(release
            .notes_url
            .ends_with("/releases/tag/pickaxe-miner-v0.0.5"));
        let unsigned = latest_json("https://github.com/x", "pickaxe-miner-v0.0.5", 51, false);
        assert_eq!(parse_latest(&unsigned).unwrap().signature_url, None);
        // Drafts, pre-releases, other tags and releases without the
        // archive or the checksum file are not offered.
        for (field, value) in [("draft", json!(true)), ("prerelease", json!(true))] {
            let mut changed = json.clone();
            changed[field] = value;
            assert!(parse_latest(&changed).is_err(), "{field}");
        }
        let mut other = json.clone();
        other["tag_name"] = json!("p2pool-v0.0.5");
        assert!(parse_latest(&other).is_err());
        let mut no_sums = json.clone();
        no_sums["assets"]
            .as_array_mut()
            .unwrap()
            .retain(|item| item["name"] != "SHA256SUMS.txt");
        assert!(parse_latest(&no_sums).unwrap_err().contains("SHA256SUMS"));
        let mut no_archive = json;
        no_archive["assets"].as_array_mut().unwrap().remove(0);
        assert!(parse_latest(&no_archive).is_err());
    }

    #[test]
    fn only_https_to_githubs_release_hosts_is_fetched() {
        let github = Source::github();
        assert!(github
            .allows("https://api.github.com/repos/CyberAshven/pickaxe-miner/releases/latest")
            .is_ok());
        assert!(github
            .allows(
                "https://release-assets.githubusercontent.com/github-production-release-asset/1/2"
            )
            .is_ok());
        assert!(github
            .allows("http://github.com/CyberAshven/pickaxe-miner/releases/download/a/b")
            .unwrap_err()
            .contains("https only"));
        assert!(github
            .allows("https://evil.example/pickaxe.zip")
            .unwrap_err()
            .contains("not one of GitHub's"));
        assert!(github.allows("https://github.com.evil.example/x").is_err());
        assert!(github.allows("https://user@github.com/x").is_err());
    }
}
