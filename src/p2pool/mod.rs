//! #### PR #53: BCH P2Pool as a dependency Pickaxe manages, as Gupaxx does
//! for Monero's P2Pool (docs/p2pool.md). Pickaxe installs the node binary
//! from P2Pool's own releases, writes its configuration from Pickaxe's node
//! and payout, runs it as a child process, restarts it when it exits and
//! shows whether it runs and whether the BCH node answers. It never changes
//! or reimplements P2Pool's logic: P2Pool is updated, not integrated.
pub mod http;
pub mod launch;
pub mod own;
pub mod release;
pub mod screen;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where Pickaxe keeps P2Pool: `p2pool/` beside the configuration. It holds
/// the installed versions, `installed.json`, `settings.json`, the SV2
/// authority key, each network's configuration, share-chain store and log.
pub fn home(config_path: &Path) -> PathBuf {
    config_path.with_file_name("p2pool")
}

/// The Bitcoin Cash share chain: main, or mini with 256 times easier shares
/// (for smaller miners; P2Pool's own default is main).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareChain {
    #[default]
    Mini,
    Main,
}

impl ShareChain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mini => "mini",
            Self::Main => "main",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mini" => Ok(Self::Mini),
            "main" => Ok(Self::Main),
            _ => Err(format!("{value}: the share chain is main or mini")),
        }
    }
}

/// What Pickaxe keeps about P2Pool in `p2pool/settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Install a newer verified release before starting P2Pool (on by
    /// default, as Claude updates itself). Off, the screen shows a newer
    /// release and installs it on `u` or `pickaxe p2pool update`.
    pub auto_update: bool,
    pub share_chain: ShareChain,
    /// Other P2Pool nodes to join, as P2P addresses such as
    /// `/ip4/203.0.113.7/tcp/6884`. With none, this node starts its own
    /// share chain and opens its ports without waiting for peers.
    pub peers: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_update: true,
            share_chain: ShareChain::default(),
            peers: Vec::new(),
        }
    }
}

impl Settings {
    fn path(home: &Path) -> PathBuf {
        home.join("settings.json")
    }

    /// The saved settings, or the defaults when none are saved yet.
    pub fn load(home: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(Self::path(home)) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", Self::path(home).display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", Self::path(home).display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        std::fs::create_dir_all(home).map_err(|error| format!("{}: {error}", home.display()))?;
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        write_atomically(&Self::path(home), text.as_bytes())
    }
}

/// Writes `bytes` to a sibling first, then renames it over `path`, so a
/// crash leaves the old file or the new one, never half of one.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes).map_err(|error| format!("{}: {error}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pickaxe-p2pool-{name}-{:016x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_auto_update_on_the_mini_chain_and_round_trip() {
        let home = test_dir("settings");
        assert_eq!(
            Settings::load(&home).unwrap(),
            Settings {
                auto_update: true,
                share_chain: ShareChain::Mini,
                peers: vec![],
            }
        );
        let changed = Settings {
            auto_update: false,
            share_chain: ShareChain::Main,
            peers: vec!["/ip4/203.0.113.7/tcp/6884".into()],
        };
        changed.save(&home).unwrap();
        assert_eq!(Settings::load(&home).unwrap(), changed);
        std::fs::write(
            home.join("settings.json"),
            r#"{"auto_update": false, "colour": 1}"#,
        )
        .unwrap();
        assert!(Settings::load(&home).unwrap_err().contains("colour"));
        assert_eq!(ShareChain::parse(" Main ").unwrap(), ShareChain::Main);
        assert!(ShareChain::parse("nano").is_err());
        std::fs::remove_dir_all(home).unwrap();
    }
}
