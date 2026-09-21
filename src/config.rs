//! Distribution and runtime config. Donation is split from each PHOTON win by the
//! protocol-valid reward child path; the PHOTON mining transaction remains the
//! authoritative two-output covenant transaction.

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
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "node" | "bchn" | "rpc" => Ok(JobSource::Node),
            "fulcrum" | "electrum" | "wss" => Ok(JobSource::Fulcrum),
            other => Err(format!("unknown source '{other}' (use node|fulcrum)")),
        }
    }

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
    /// Example Start9: `http://127.0.0.1:8332` (auth via env, never logged).
    pub node_url: Option<String>,
    /// Preferred network transport for submission/diagnostics.
    pub source: JobSource,
    /// When true, the GPU mining loop is running.
    pub mining: bool,
    /// Immutable mining-work generation. Zero means no live job is published yet.
    pub generation_id: u64,
}

impl Default for RuntimeConfig {
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
    pub fn set_intensity(&mut self, value: u8) -> Result<(), String> {
        if !(10..=100).contains(&value) {
            return Err("intensity must be 10..=100".into());
        }
        self.intensity = value;
        Ok(())
    }

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

    pub fn clear_node_url(&mut self) {
        if self.node_url.take().is_some() {
            self.bump_generation();
        }
    }

    pub fn set_source(&mut self, s: &str) -> Result<(), String> {
        let source = JobSource::parse(s)?;
        if self.source != source {
            self.source = source;
            self.bump_generation();
        }
        Ok(())
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    const PAYOUT: &str = "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh";

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
}
