//! Distribution and runtime config. Donation is a coinbase-style split on the win tx only.

/// Donation share in basis points (200 = 2%). Never call this a "dev fee".
pub const DONATION_BPS: u16 = 200;

/// Locked donation payout for distribution builds (BCH cashaddr).
pub const DONATION_ADDRESS: &str = "bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3";

/// Miner keeps the remainder (9800 bps = 98%).
pub const MINER_BPS: u16 = 10_000 - DONATION_BPS;

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// GPU/CPU work intensity 0..=100 (mirrors site control; stub for now).
    pub intensity: u8,
    /// Miner reward cashaddr (user). Empty until set.
    pub payout_address: String,
    /// Optional custom Fulcrum/Electrum URL (ws:// or wss://). Tried before bootstrap.
    /// Example Start9: `wss://start9oslinux.local:50004`
    pub fulcrum_url: Option<String>,
    /// When true, a stub "mining" loop is considered running.
    pub mining: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            intensity: 50,
            payout_address: String::new(),
            fulcrum_url: None,
            mining: false,
        }
    }
}

impl RuntimeConfig {
    pub fn set_intensity(&mut self, value: u8) -> Result<(), String> {
        if value > 100 {
            return Err("intensity must be 0..=100".into());
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
        use crate::protocol::ELECTRUM_WSS_BOOTSTRAP;
        let mut out = Vec::new();
        if let Some(u) = &self.fulcrum_url {
            out.push(u.clone());
        }
        for u in ELECTRUM_WSS_BOOTSTRAP {
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
