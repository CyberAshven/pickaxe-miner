//! Distribution and runtime config. Donation is split from each PHOTON win by the
//! protocol-valid reward child path; the PHOTON mining transaction remains the
//! authoritative two-output covenant transaction.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Donation share in basis points (200 = 2%). Never call this a "dev fee".
pub const DONATION_BPS: u16 = 200;

/// Locked donation payout for distribution builds (BCH cashaddr).
pub const DONATION_ADDRESS: &str = "bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3";

/// Preferred network transport for explicit submission/diagnostic operations.
/// PHOTON mining jobs come from the covenant CashToken baton through Fulcrum
/// until an equivalent node-native indexed query is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobSource {
    /// Native BCH node RPC for validation/broadcast and BCH block tooling.
    Node,
    /// Fulcrum/Electrum, including the current PHOTON baton index.
    #[default]
    Fulcrum,
}

impl JobSource {
    /// Parses a configured source kind from its persisted name.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "node" | "bchn" | "rpc" => Ok(JobSource::Node),
            "fulcrum" | "electrum" | "wss" => Ok(JobSource::Fulcrum),
            other => Err(format!("unknown source '{other}' (use node|fulcrum)")),
        }
    }

    /// Returns the persisted name of the source kind.
    pub fn as_str(self) -> &'static str {
        match self {
            JobSource::Node => "node",
            JobSource::Fulcrum => "fulcrum",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// GPU work intensity 10..=100. Pause is a separate runtime state.
    pub intensity: u8,
    /// Miner reward cashaddr (user). Empty until set.
    pub payout_address: String,
    /// Optional custom Fulcrum/Electrum URL (ws:// or wss://). Tried before bootstrap.
    /// Example Start9: `wss://start9oslinux.local:50004`
    pub fulcrum_url: Option<String>,
    /// Optional native node JSON-RPC URL (`http://` / `https://`).
    /// Example Start9: `http://127.0.0.1:8332`. Prefer credentials in
    /// `PICKAXE_NODE_RPC_USER` + `PICKAXE_NODE_RPC_PASSWORD` so secrets never
    /// need to appear in command history or runtime endpoint identity.
    pub node_url: Option<String>,
    /// Preferred network transport for submission/diagnostics.
    pub source: JobSource,
    /// When true, the GPU mining loop is running.
    pub mining: bool,
    /// Immutable mining-work generation. Zero means no live job is published yet.
    pub generation_id: u64,
}

impl Default for RuntimeConfig {
    /// Creates the default runtime configuration.
    fn default() -> Self {
        Self {
            intensity: 100,
            payout_address: String::new(),
            fulcrum_url: None,
            node_url: None,
            source: JobSource::Fulcrum,
            mining: false,
            generation_id: 0,
        }
    }
}

impl RuntimeConfig {
    /// Updates the configured GPU intensity within the accepted range.
    pub fn set_intensity(&mut self, value: u8) -> Result<(), String> {
        if !(10..=100).contains(&value) {
            return Err("intensity must be 10..=100".into());
        }
        self.intensity = value;
        Ok(())
    }

    /// Validates and stores the miner payout CashAddr.
    pub fn set_payout(&mut self, addr: String) -> Result<(), String> {
        let trimmed = addr.trim().to_string();
        if trimmed.is_empty() {
            return Err("payout address required".into());
        }
        crate::tx::cashaddr_to_p2pkh_locking(&trimmed)
            .map_err(|e| format!("invalid payout CashAddr: {e}"))?;
        let canonical = if trimmed.contains(':') {
            trimmed.to_ascii_lowercase()
        } else {
            format!("bitcoincash:{}", trimmed.to_ascii_lowercase())
        };
        if self.payout_address != canonical {
            self.bump_generation();
        }
        self.payout_address = canonical;
        Ok(())
    }

    /// Set custom Fulcrum/Electrum endpoint (`ws://` or `wss://`). Empty clears.
    pub fn set_fulcrum_url(&mut self, url: &str) -> Result<(), String> {
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            self.clear_fulcrum_url();
            return Ok(());
        }
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("wss://") || lower.starts_with("ws://")) {
            return Err("fulcrum URL must start with wss:// or ws://".into());
        }
        if self.fulcrum_url.as_deref() != Some(trimmed.as_str()) {
            self.bump_generation();
            self.fulcrum_url = Some(trimmed);
        }
        Ok(())
    }

    /// Removes the custom Fulcrum endpoint.
    pub fn clear_fulcrum_url(&mut self) {
        if self.fulcrum_url.take().is_some() {
            self.bump_generation();
        }
    }

    /// Endpoint try-order: custom (if set), then public bootstrap.
    pub fn electrum_endpoints(&self) -> Vec<String> {
        use crate::protocol::FULCRUM_WSS_BOOTSTRAP;
        let mut out = Vec::new();
        if let Some(u) = &self.fulcrum_url {
            out.push(u.clone());
        }
        for u in FULCRUM_WSS_BOOTSTRAP {
            if !out.iter().any(|x| x == *u) {
                out.push((*u).to_string());
            }
        }
        out
    }

    /// Validates and stores the native node endpoint.
    pub fn set_node_url(&mut self, url: &str) -> Result<(), String> {
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            self.clear_node_url();
            return Ok(());
        }
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err("node URL must start with http:// or https://".into());
        }
        // Never require embedding user:pass in chat logs — accept URL as given.
        if self.node_url.as_deref() != Some(trimmed.as_str()) {
            self.bump_generation();
            self.node_url = Some(trimmed);
        }
        Ok(())
    }

    /// Removes the custom native node endpoint.
    pub fn clear_node_url(&mut self) {
        if self.node_url.take().is_some() {
            self.bump_generation();
        }
    }

    /// Changes the selected job source and increments its generation.
    pub fn set_source(&mut self, s: &str) -> Result<(), String> {
        let source = JobSource::parse(s)?;
        if self.source != source {
            self.source = source;
            self.bump_generation();
        }
        Ok(())
    }

    /// Advances the generation when job-affecting settings change.
    pub fn bump_generation(&mut self) -> u64 {
        self.generation_id = self.generation_id.wrapping_add(1);
        if self.generation_id == 0 {
            self.generation_id = 1;
        }
        self.generation_id
    }

    /// Node try-order: custom (if set), then curated NODE_RPC_BOOTSTRAP.
    pub fn node_endpoints(&self) -> Vec<String> {
        use crate::protocol::NODE_RPC_BOOTSTRAP;
        let mut out = Vec::new();
        if let Some(u) = &self.node_url {
            out.push(u.clone());
        }
        for u in NODE_RPC_BOOTSTRAP {
            if !out.iter().any(|x| x == *u) {
                out.push((*u).to_string());
            }
        }
        out
    }

    /// Split amounts for a reward of `reward_raw` atomic units (floor math).
    pub fn split_reward(reward_raw: u128) -> (u128, u128) {
        let donation = reward_raw.saturating_mul(DONATION_BPS as u128) / 10_000;
        let miner = reward_raw.saturating_sub(donation);
        (miner, donation)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SavedConfig {
    pub backend: Option<String>,
    pub device: Option<u32>,
    pub intensity: Option<u8>,
    pub address: Option<String>,
    pub fulcrum: Option<String>,
    pub node_rpc: Option<String>,
    pub source: Option<String>,
}

impl SavedConfig {
    /// Applies saved configuration values to a running miner.
    pub fn apply_to_runtime(&self, cfg: &mut RuntimeConfig) -> Result<(), String> {
        if let Some(value) = self.intensity {
            cfg.set_intensity(value)?;
        }
        if let Some(value) = &self.address {
            cfg.set_payout(value.clone())?;
        }
        if let Some(value) = &self.fulcrum {
            cfg.set_fulcrum_url(value)?;
        }
        if let Some(value) = &self.node_rpc {
            cfg.set_node_url(value)?;
        }
        if let Some(value) = &self.source {
            cfg.set_source(value)?;
        }
        Ok(())
    }

    /// Rejects inconsistent or unsupported saved configuration values.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(value) = &self.backend {
            crate::backend::BackendKind::parse(value)?;
        }
        let mut runtime = RuntimeConfig::default();
        self.apply_to_runtime(&mut runtime)
    }

    /// Loads and validates the saved miner configuration.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes =
            fs::read(path).map_err(|error| format!("read config {}: {error}", path.display()))?;
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse config {}: {error}", path.display()))?;
        config
            .validate()
            .map_err(|error| format!("validate config {}: {error}", path.display()))?;
        Ok(config)
    }

    /// Loads saved configuration when the file exists.
    pub fn load_optional(path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() {
            return Ok(None);
        }
        Self::load(path).map(Some)
    }

    /// Persists the current miner configuration to disk.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                format!("create config directory {}: {error}", parent.display())
            })?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize config: {error}"))?;
        write_private_config(path, &bytes)
    }

    /// Captures the effective runtime settings for persistence.
    pub fn from_effective(backend: &str, device: Option<u32>, runtime: &RuntimeConfig) -> Self {
        let address = if runtime.payout_address.is_empty() {
            None
        } else {
            Some(runtime.payout_address.clone())
        };
        Self {
            backend: Some(backend.to_string()),
            device,
            intensity: Some(runtime.intensity),
            address,
            fulcrum: runtime.fulcrum_url.clone(),
            node_rpc: runtime.node_url.clone(),
            source: Some(runtime.source.as_str().to_string()),
        }
    }
}

fn write_private_config(path: &Path, bytes: &[u8]) -> Result<(), String> {
    // An existing file is restricted before it is truncated. A failed
    // permission change must leave the previous payout and node URL in place.
    let replacing = path.exists();
    if replacing {
        restrict_private_config(path)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("write config {}: {error}", path.display()))?;
    if !replacing {
        if let Err(error) = restrict_private_config(path) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
    }
    std::io::Write::write_all(&mut file, bytes)
        .map_err(|error| format!("write config {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("write config {}: {error}", path.display()))?;
    Ok(())
}

fn restrict_private_config(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("restrict config {}: {error}", path.display()))?;
    }
    #[cfg(windows)]
    restrict_config_to_current_user(path)?;
    Ok(())
}

/// Saved node URLs may contain RPC credentials. Keep the file owner-only so
/// other local users cannot read `user:pass@host` out of the config.
#[cfg(windows)]
fn restrict_config_to_current_user(path: &Path) -> Result<(), String> {
    let whoami = std::process::Command::new("whoami")
        .output()
        .map_err(|error| format!("identify config owner for {}: {error}", path.display()))?;
    if !whoami.status.success() {
        return Err(format!(
            "identify config owner for {}: whoami failed",
            path.display()
        ));
    }
    let owner = String::from_utf8_lossy(&whoami.stdout).trim().to_string();
    if owner.is_empty() {
        return Err(format!(
            "identify config owner for {}: whoami returned an empty account",
            path.display()
        ));
    }
    let output = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{owner}:(F)"))
        .output()
        .map_err(|error| format!("restrict config {}: {error}", path.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!(
        "restrict config {}: icacls exited with {} {}",
        path.display(),
        output.status,
        detail
    ))
}

/// Returns the location of the miner configuration file.
pub fn config_path(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PICKAXE_CONFIG").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    #[cfg(windows)]
    {
        let base = std::env::var_os("APPDATA")
            .ok_or_else(|| "APPDATA is unavailable; pass --config <path>".to_string())?;
        Ok(PathBuf::from(base).join("Pickaxe").join("config.json"))
    }

    #[cfg(not(windows))]
    {
        if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(base).join("pickaxe").join("config.json"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| "HOME is unavailable; pass --config <path>".to_string())?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join("pickaxe")
            .join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYOUT: &str = "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh";

    #[test]
    fn saved_config_node_credentials_are_owner_only() {
        let dir = std::env::temp_dir().join(format!("pickaxe-config-mode-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        expose_config_directory(&dir);
        let path = dir.join("config.json");
        let mut runtime = RuntimeConfig::default();
        runtime
            .set_node_url("http://user:secret-pass@127.0.0.1:8332")
            .unwrap();
        let saved = SavedConfig::from_effective("cuda", Some(0), &runtime);
        saved.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("secret-pass"), "{text}");
        assert_config_file_is_owner_only(&path);
        runtime
            .set_node_url("http://user:second-secret@127.0.0.1:8332")
            .unwrap();
        let replaced = SavedConfig::from_effective("cuda", Some(0), &runtime);
        replaced.save(&path).unwrap();
        let replaced_text = fs::read_to_string(&path).unwrap();
        assert!(replaced_text.contains("second-secret"), "{replaced_text}");
        assert!(!replaced_text.contains("secret-pass"), "{replaced_text}");
        assert_config_file_is_owner_only(&path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
            saved.save(&path).unwrap();
            assert_config_file_is_owner_only(&path);
            assert!(fs::read_to_string(&path).unwrap().contains("secret-pass"));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    fn expose_config_directory(dir: &Path) {
        let output = std::process::Command::new("icacls")
            .arg(dir)
            .args(["/grant", "*S-1-1-0:(OI)(CI)R"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "icacls grant failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(not(windows))]
    fn expose_config_directory(_dir: &Path) {}

    #[cfg(windows)]
    fn assert_config_file_is_owner_only(path: &Path) {
        let output = std::process::Command::new("icacls")
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "icacls query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("everyone"), "{text}");
        assert!(!lower.contains("builtin\\users"), "{text}");
        assert!(!lower.contains("authenticated users"), "{text}");
        assert!(
            lower.contains(":(f)"),
            "config ACL has no owner full-control entry: {text}"
        );
    }

    #[cfg(unix)]
    fn assert_config_file_is_owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "config mode {mode:o} is not owner-only");
    }

    #[test]
    fn payout_requires_valid_cashaddr_checksum() {
        let mut cfg = RuntimeConfig::default();
        assert!(cfg.set_payout(PAYOUT.into()).is_ok());
        assert!(cfg
            .set_payout("bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frq".into())
            .is_err());
    }

    #[test]
    fn generation_bumps_on_state_changes_but_not_intensity() {
        let mut cfg = RuntimeConfig::default();
        cfg.set_intensity(50).unwrap();
        assert_eq!(cfg.generation_id, 0);

        cfg.set_payout(PAYOUT.into()).unwrap();
        assert_eq!(cfg.generation_id, 1);
        cfg.set_payout(PAYOUT.into()).unwrap();
        assert_eq!(cfg.generation_id, 1);

        cfg.set_source("fulcrum").unwrap();
        assert_eq!(cfg.generation_id, 1);
        cfg.set_source("node").unwrap();
        assert_eq!(cfg.generation_id, 2);

        cfg.set_fulcrum_url("ws://127.0.0.1:50003").unwrap();
        assert_eq!(cfg.generation_id, 3);
        cfg.set_fulcrum_url("ws://127.0.0.1:50003").unwrap();
        assert_eq!(cfg.generation_id, 3);
        cfg.clear_fulcrum_url();
        assert_eq!(cfg.generation_id, 4);

        cfg.set_node_url("http://127.0.0.1:8332").unwrap();
        assert_eq!(cfg.generation_id, 5);
        cfg.set_node_url("http://127.0.0.1:8332").unwrap();
        assert_eq!(cfg.generation_id, 5);
        cfg.clear_node_url();
        assert_eq!(cfg.generation_id, 6);
    }

    #[test]
    fn donation_is_exactly_two_percent_and_config_cannot_override_it() {
        assert_eq!(DONATION_BPS, 200);
        assert_eq!(RuntimeConfig::split_reward(100), (98, 2));
        assert_eq!(
            RuntimeConfig::split_reward(4_999_773_813),
            (4_899_778_337, 99_995_476)
        );
        assert!(serde_json::from_str::<SavedConfig>(r#"{"donation_bps":0}"#).is_err());
        let mut cfg = RuntimeConfig::default();
        cfg.set_payout(PAYOUT.into()).unwrap();
        let saved = SavedConfig::from_effective("cuda", Some(0), &cfg);
        let value = serde_json::to_value(&saved).unwrap();
        assert!(value.get("donation_bps").is_none());
        assert!(value.get("donation").is_none());
        let reloaded: SavedConfig = serde_json::from_value(value).unwrap();
        let mut applied = RuntimeConfig::default();
        reloaded.apply_to_runtime(&mut applied).unwrap();
        assert_eq!(
            RuntimeConfig::split_reward(4_999_773_813),
            (4_899_778_337, 99_995_476)
        );
        assert_eq!(applied.intensity, 100);
    }
}
