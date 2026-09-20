//! Distribution and runtime config. Donation is a coinbase-style split on the win tx only.

/// Donation share in basis points (200 = 2%). Never call this a "dev fee".
pub const DONATION_BPS: u16 = 200;

/// Locked donation payout for distribution builds (BCH cashaddr).
pub const DONATION_ADDRESS: &str = "bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3";

/// Miner keeps the remainder (9800 bps = 98%).
pub const MINER_BPS: u16 = 10_000 - DONATION_BPS;


/// Where mining **templates / block submit** come from.
/// Bandar/CoS lock: node RPC is first-class; Fulcrum is auxiliary (UTXO/wallet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSource {
    /// BCHN `getblocktemplatelight` / `submitblocklight` (GBT fallback).
    Node,
    /// Fulcrum/Electrum — ancillary only (PHOTON baton index until node path exists).
    Fulcrum,
}

impl Default for JobSource {
    fn default() -> Self {
        JobSource::Node
    }
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
    /// Template/submit source (default: node).
    pub source: JobSource,
    /// When true, the GPU mining loop is running.
    pub mining: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            intensity: 100,
            payout_address: String::new(),
            fulcrum_url: None,
            node_url: None,
            source: JobSource::Node,
            mining: false,
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
        if !(trimmed.starts_with("bitcoincash:") || trimmed.starts_with("bchtest:")) {
            return Err("expected bitcoincash:… (or bchtest: for tests)".into());
        }
        self.payout_address = trimmed;
        Ok(())
    }

    /// Set custom Fulcrum/Electrum endpoint (`ws://` or `wss://`). Empty clears.
    pub fn set_fulcrum_url(&mut self, url: &str) -> Result<(), String> {
        let trimmed = url.trim().to_string();
        if trimmed.is_empty() {
            self.fulcrum_url = None;
            return Ok(());
        }
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("wss://") || lower.starts_with("ws://")) {
            return Err("fulcrum URL must start with wss:// or ws://".into());
        }
        self.fulcrum_url = Some(trimmed);
        Ok(())
    }

    pub fn clear_fulcrum_url(&mut self) {
        self.fulcrum_url = None;
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
            self.node_url = None;
            return Ok(());
        }
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err("node URL must start with http:// or https://".into());
        }
        // Never require embedding user:pass in chat logs — accept URL as given.
        self.node_url = Some(trimmed);
        Ok(())
    }

    pub fn clear_node_url(&mut self) {
        self.node_url = None;
    }

    pub fn set_source(&mut self, s: &str) -> Result<(), String> {
        self.source = JobSource::parse(s)?;
        Ok(())
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
