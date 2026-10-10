//! Distribution and runtime config. Each token selects its own fee policy.

use crate::backend_kind::DeviceSelection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Historical reward split, retained to recover older pending payouts.
/// New PHOTON mining uses the separate 4% policy in `donation`.
pub const DONATION_BPS: u16 = 200;

/// Historical addresses: frozen for legacy payout recovery and its VM proof.
pub const DONATION_ADDRESS: &str = "bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3";
pub const CHIPNET_DONATION_ADDRESS: &str = "bchtest:qrzq5f9ltv70u4su7d40agd4nlnp8qlgqcma6x2tvp";

/// Chain selected for mining. Each token resolves its own deployment on this chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MiningNetwork {
    #[default]
    Mainnet,
    Chipnet,
}

impl MiningNetwork {
    /// #### PR #42
    /// What: the curated node RPCs of this network, tried after the miner's
    /// own (none yet on either network).
    /// Why: Chipnet and mainnet are one code path; the network chooses data,
    /// never a different path.
    /// Look here if: a network gets curated nodes.
    pub fn node_rpc_bootstrap(self) -> &'static [&'static str] {
        match self {
            Self::Mainnet => crate::protocol::NODE_RPC_BOOTSTRAP,
            Self::Chipnet => crate::protocol::CHIPNET_NODE_RPC_BOOTSTRAP,
        }
    }

    /// #### PR #42: the SV2 Template Distribution port a server listens on
    /// by default on this network.
    pub fn template_port(self) -> u16 {
        match self {
            Self::Mainnet => crate::protocol::TEMPLATE_PORT,
            Self::Chipnet => crate::protocol::CHIPNET_TEMPLATE_PORT,
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mainnet" => Ok(Self::Mainnet),
            "chipnet" => Ok(Self::Chipnet),
            _ => Err("network must be mainnet or chipnet".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Chipnet => "chipnet",
        }
    }

    fn cashaddr_prefix(self) -> &'static str {
        match self {
            Self::Mainnet => "bitcoincash",
            Self::Chipnet => "bchtest",
        }
    }

    /// Prevents a selected chain from using a published endpoint of the other chain.
    pub(crate) fn accepts_known_electrum_endpoint(self, url: &str) -> bool {
        let foreign = match self {
            Self::Mainnet => crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP,
            Self::Chipnet => crate::protocol::FULCRUM_WSS_BOOTSTRAP,
        };
        !foreign.iter().any(|entry| entry.eq_ignore_ascii_case(url))
    }
}

/// Supported GPU-minable token identities, in alphabetical display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MiningToken {
    #[default]
    Photon,
}

impl MiningToken {
    pub const GPU_SUPPORTED: &[Self] = &[Self::Photon];

    /// #### PR #32
    /// The least a miner donates to mine this token: PHOTON's 4% work share.
    /// Every token added later uses `donation::NEW_TOKEN_DONATION` (1.5%).
    pub fn donation_minimum(self) -> crate::donation::TokenDonation {
        match self {
            Self::Photon => crate::donation::TokenDonation::from_bps(400),
        }
    }

    /// The token's policy at a donation the miner chose: anything above the
    /// minimum goes to the project's donation address, as more of the work.
    pub fn fee_policy_at(
        self,
        network: MiningNetwork,
        donation: crate::donation::TokenDonation,
    ) -> crate::donation::Policy {
        let mut policy = self.fee_policy(network);
        let extra = donation
            .at_least(self.donation_minimum())
            .bps()
            .saturating_sub(self.donation_minimum().bps());
        if let crate::donation::Scheme::Work([project, _]) = &mut policy.scheme {
            *project = project.saturating_add(extra);
        }
        policy
    }

    /// Maintainer-editable mode, shares and recipients. The protocol must support
    /// its selected payout scheme before mining starts. This is the minimum;
    /// `fee_policy_at` adds what a miner chose above it.
    pub fn fee_policy(self, network: MiningNetwork) -> crate::donation::Policy {
        use crate::donation::{Policy, Scheme};
        match self {
            Self::Photon => match network {
                MiningNetwork::Mainnet => Policy {
                    scheme: Scheme::Work([200, 200]),
                    addresses: [DONATION_ADDRESS, SHREC_DONATION_ADDRESS],
                },
                MiningNetwork::Chipnet => Policy {
                    scheme: Scheme::Work([400, 0]),
                    addresses: [CHIPNET_DONATION_ADDRESS, CHIPNET_DONATION_ADDRESS],
                },
            },
        }
    }

    pub fn photon_deployment(
        self,
        network: MiningNetwork,
    ) -> &'static crate::protocol::PhotonDeployment {
        match (self, network) {
            (Self::Photon, MiningNetwork::Mainnet) => &crate::protocol::MAINNET_PHOTON,
            (Self::Photon, MiningNetwork::Chipnet) => &crate::protocol::CHIPNET_PHOTON,
        }
    }

    pub fn parse(query: &str, network: MiningNetwork) -> Result<Self, String> {
        let query = query.trim();
        let hex = query
            .strip_prefix("0x")
            .or_else(|| query.strip_prefix("0X"))
            .unwrap_or(query);
        let deployment = Self::Photon.photon_deployment(network);
        if query.eq_ignore_ascii_case("photon")
            || hex.eq_ignore_ascii_case(deployment.category_hex)
            || hex.eq_ignore_ascii_case(deployment.covenant_lock_hex)
            || query.eq_ignore_ascii_case(deployment.covenant_address)
        {
            Ok(Self::Photon)
        } else {
            Err("unknown token name, category ID, or covenant bytecode; try PHOTON".into())
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Photon => "PHOTON",
        }
    }

    pub fn ensure_supported(self, network: MiningNetwork) -> Result<(), String> {
        match (self, network) {
            (Self::Photon, MiningNetwork::Mainnet | MiningNetwork::Chipnet) => Ok(()),
        }
    }
}

/// shrec's share of the existing 2% donation.
pub const SHREC_DONATION_ADDRESS: &str = "bitcoincash:zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3";

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
    /// BCH ASIC policy only. Token policies are selected separately.
    pub bch_donation: crate::donation::bch::BchDonation,
    /// #### PR #32: the token donation a miner chose in Advanced settings;
    /// `None` mines at the token's minimum.
    pub token_donation: Option<crate::donation::TokenDonation>,
    /// Mining chain; mainnet is the safe default.
    pub network: MiningNetwork,
    /// Selected token. PHOTON is currently the only supported GPU token.
    pub token: MiningToken,
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
            bch_donation: Default::default(),
            token_donation: None,
            network: MiningNetwork::Mainnet,
            token: MiningToken::Photon,
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
    /// #### PR #32: the token donation in effect, never below the minimum.
    pub fn token_donation(&self) -> crate::donation::TokenDonation {
        let minimum = self.token.donation_minimum();
        self.token_donation.unwrap_or(minimum).at_least(minimum)
    }

    /// The selected token's policy at the donation in effect.
    pub fn fee_policy(&self) -> crate::donation::Policy {
        self.token
            .fee_policy_at(self.network, self.token_donation())
    }

    pub fn set_network(&mut self, network: MiningNetwork) {
        if self.network != network {
            // Keep the entered address intact; validation requires an address for the new chain.
            // Configured sources are chain-specific. CLI overrides are applied after the network.
            self.fulcrum_url = None;
            self.node_url = None;
            self.network = network;
            self.bump_generation();
        }
    }

    pub fn set_token(&mut self, query: &str) -> Result<(), String> {
        let token = MiningToken::parse(query, self.network)?;
        if self.token != token {
            self.token = token;
            self.bump_generation();
        }
        Ok(())
    }

    pub fn validate_payout_network(&self) -> Result<(), String> {
        if self.payout_address.is_empty() {
            Ok(())
        } else {
            validate_payout_address(self.network, &self.payout_address).map(|_| ())
        }
    }

    pub fn ensure_mining_supported(&self) -> Result<(), String> {
        self.validate_payout_network()?;
        self.token.ensure_supported(self.network)
    }

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
        let selected = validate_payout_address(self.network, &addr)?;
        if self.payout_address != selected {
            self.bump_generation();
        }
        self.payout_address = selected;
        Ok(())
    }

    /// Set custom Fulcrum/Electrum endpoint (`ws://` or `wss://`). Empty clears.
    pub fn set_fulcrum_url(&mut self, url: &str) -> Result<(), String> {
        let Some(endpoints) =
            normalize_endpoint_list(url, "fulcrum", &["wss://", "ws://", "tcp://"])?
        else {
            self.clear_fulcrum_url();
            return Ok(());
        };
        // #### PR #42: plain-TCP Fulcrum servers (tcp://HOST:PORT), such as
        // Umbrel's or StartOS's, only on this computer or the home network.
        for endpoint in endpoints.split(',').map(str::trim) {
            if endpoint.len() > 6 && endpoint[..6].eq_ignore_ascii_case("tcp://") {
                check_tcp_electrum(&endpoint[6..])?;
            }
        }
        if endpoints.split(',').any(|endpoint| {
            !self
                .network
                .accepts_known_electrum_endpoint(endpoint.trim())
        }) {
            return Err(format!(
                "a published Fulcrum endpoint belongs to another network, not {}",
                self.network.as_str()
            ));
        }
        if self.fulcrum_url.as_deref() != Some(endpoints.as_str()) {
            self.bump_generation();
            self.fulcrum_url = Some(endpoints);
        }
        Ok(())
    }

    pub fn custom_fulcrum_endpoints(&self) -> Vec<&str> {
        self.fulcrum_url
            .as_deref()
            .map(|value| value.split(',').map(str::trim).collect())
            .unwrap_or_default()
    }

    /// Removes the custom Fulcrum endpoint.
    pub fn clear_fulcrum_url(&mut self) {
        if self.fulcrum_url.take().is_some() {
            self.bump_generation();
        }
    }

    /// Endpoint try-order: custom (if set), then public bootstrap.
    pub fn electrum_endpoints(&self) -> Vec<String> {
        use crate::protocol::{CHIPNET_FULCRUM_WSS_BOOTSTRAP, FULCRUM_WSS_BOOTSTRAP};
        let mut out = Vec::new();
        for url in self.custom_fulcrum_endpoints() {
            if self.network.accepts_known_electrum_endpoint(url) {
                out.push(url.to_string());
            }
        }
        let bootstrap = match self.network {
            MiningNetwork::Mainnet => FULCRUM_WSS_BOOTSTRAP,
            MiningNetwork::Chipnet => CHIPNET_FULCRUM_WSS_BOOTSTRAP,
        };
        for u in bootstrap {
            if !out.iter().any(|x| x.as_str() == *u) {
                out.push((*u).to_string());
            }
        }
        out
    }

    /// Validates and stores the native node endpoint.
    pub fn set_node_url(&mut self, url: &str) -> Result<(), String> {
        let Some(endpoints) = normalize_endpoint_list(url, "node", &["https://", "http://"])?
        else {
            self.clear_node_url();
            return Ok(());
        };
        if self.node_url.as_deref() != Some(endpoints.as_str()) {
            self.bump_generation();
            self.node_url = Some(endpoints);
        }
        Ok(())
    }

    pub fn custom_node_endpoints(&self) -> Vec<&str> {
        self.node_url
            .as_deref()
            .map(|value| value.split(',').map(str::trim).collect())
            .unwrap_or_default()
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
        let mut out = Vec::new();
        for url in self.custom_node_endpoints() {
            out.push(url.to_string());
        }
        // #### PR #42: the curated list is the network's own data.
        for u in self.network.node_rpc_bootstrap().iter().copied() {
            if !out.iter().any(|x| x.as_str() == u) {
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

    /// Divide the existing donation equally; the original recipient gets an odd remainder.
    pub fn split_donation(donation_raw: u128) -> (u128, u128) {
        let shrec = donation_raw / 2;
        (donation_raw - shrec, shrec)
    }
}

/// Validates a payout for the selected chain and returns its canonical CashAddr.
/// An omitted prefix uses the selected chain's checksum, never another network's.
/// #### PR #40
/// A BCH coinbase payout for `network`: P2PKH (q) or P2SH (p), such as a pool
/// operator's multisig, in its canonical lower-case CashAddr form.
pub fn validate_coinbase_address(network: MiningNetwork, address: &str) -> Result<String, String> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err("payout address required".into());
    }
    let canonical = if trimmed.contains(':') {
        trimmed.to_ascii_lowercase()
    } else {
        format!(
            "{}:{}",
            network.cashaddr_prefix(),
            trimmed.to_ascii_lowercase()
        )
    };
    crate::tx::cashaddr_to_coinbase_locking(trimmed)
        .map_err(|e| format!("invalid payout CashAddr: {e}"))?;
    if !canonical.starts_with(&format!("{}:", network.cashaddr_prefix())) {
        return Err(format!(
            "payout address must use {}: on {}",
            network.cashaddr_prefix(),
            network.as_str()
        ));
    }
    Ok(canonical)
}

pub fn validate_payout_address(network: MiningNetwork, address: &str) -> Result<String, String> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err("payout address required".into());
    }
    if trimmed.chars().any(|ch| ch.is_ascii_lowercase())
        && trimmed.chars().any(|ch| ch.is_ascii_uppercase())
    {
        return Err("invalid payout CashAddr: CashAddr must not mix upper and lower case".into());
    }
    let canonical = if trimmed.contains(':') {
        trimmed.to_ascii_lowercase()
    } else {
        format!(
            "{}:{}",
            network.cashaddr_prefix(),
            trimmed.to_ascii_lowercase()
        )
    };
    crate::tx::cashaddr_to_p2pkh_locking(&canonical)
        .map_err(|e| format!("invalid payout CashAddr: {e}"))?;
    if !canonical.starts_with(&format!("{}:", network.cashaddr_prefix())) {
        return Err(format!(
            "payout address must use {}: on {}",
            network.cashaddr_prefix(),
            network.as_str()
        ));
    }
    Ok(canonical)
}

/// Re-encodes an internal P2PKH recipient for another chain without changing its key hash.
/// User-entered payouts must use `validate_payout_address` instead.
pub(crate) fn reprefix_p2pkh_payout(
    address: &str,
    network: MiningNetwork,
) -> Result<String, String> {
    let locking = crate::tx::cashaddr_to_p2pkh_locking(address)?;
    let hash: [u8; 20] = locking
        .get(3..23)
        .ok_or("P2PKH locking bytecode omitted its 20-byte hash")?
        .try_into()
        .map_err(|_| "P2PKH locking bytecode has an invalid hash length")?;
    let payload = address
        .split_once(':')
        .map(|(_, payload)| payload)
        .ok_or("P2PKH CashAddr omitted its network prefix")?;
    if payload.starts_with('z') {
        crate::tx::token_p2pkh_hash_to_cashaddr_for_network(&hash, network)
    } else {
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&hash, network)
    }
}

// #### PR #42: plain TCP only at home
// What: a Fulcrum server over plain TCP (tcp://HOST:PORT, as Umbrel and
// StartOS publish theirs) is taken only on this computer or the home
// network: loopback, private, link-local and Tailscale's shared range, IPv6
// unique-local and link-local, `localhost` or a `.local` name.
// Why: plain TCP has no encryption, so a server reached over the internet
// could be impersonated to feed a false baton; servers there use wss://.
// Look here if: a home Fulcrum server is refused, or a public one accepted
// over tcp://.
/// Checks a plain-TCP Electrum endpoint's `HOST:PORT`.
fn check_tcp_electrum(address: &str) -> Result<(), String> {
    let authority = address.split('/').next().unwrap_or(address);
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once("]:") {
            Some((host, port)) => (host, port),
            None => (bracketed.trim_end_matches(']'), ""),
        },
        None => authority.rsplit_once(':').unwrap_or((authority, "")),
    };
    if port.parse::<u16>().ok().filter(|port| *port != 0).is_none() {
        return Err(
            "a plain-TCP Fulcrum server needs its port, such as tcp://umbrel.local:50001".into(),
        );
    }
    if !private_host(host) {
        return Err(
            "a plain-TCP Fulcrum server (tcp://) must be on this computer or your home network; \
             servers on the internet need wss://"
                .into(),
        );
    }
    Ok(())
}

/// Whether `host` is this computer or on the home network.
fn private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || (lower.len() > 6 && lower.ends_with(".local")) {
        return true;
    }
    match host.parse::<std::net::IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(std::net::IpAddr::V4(ip)) => {
            let [first, second, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (first == 100 && second & 0xc0 == 0x40)
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => false,
    }
}
// #### end PR #42 ####

fn normalize_endpoint_list(
    input: &str,
    kind: &str,
    schemes: &[&str],
) -> Result<Option<String>, String> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    let urls = input.split(',').map(str::trim).collect::<Vec<_>>();
    if urls.len() > 8 || urls.iter().any(|url| url.is_empty()) {
        return Err(format!("{kind} needs 1–8 URLs separated by commas"));
    }
    for url in &urls {
        let lower = url.to_ascii_lowercase();
        if !schemes.iter().any(|scheme| lower.starts_with(scheme))
            || url.contains([';', ' ', '\n', '\r', '\t'])
        {
            return Err(format!(
                "invalid {kind} URL; separate URLs with commas and use {}",
                schemes.join(" or ")
            ));
        }
    }
    let mut unique = Vec::new();
    for url in urls {
        if !unique.contains(&url) {
            unique.push(url);
        }
    }
    Ok(Some(unique.join(", ")))
}

/// The GPUs a saved configuration mines on: one GPU number (the form older
/// versions saved) or a list. Absent means every discrete GPU.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SavedDevices {
    One(u32),
    List(Vec<u32>),
}

impl SavedDevices {
    /// The saved form of a GPU choice; `None` for every GPU.
    pub fn from_selection(selection: &DeviceSelection) -> Option<Self> {
        match selection {
            DeviceSelection::Indices(indices) => match indices.as_slice() {
                [index] => Some(Self::One(*index)),
                _ => Some(Self::List(indices.clone())),
            },
            DeviceSelection::Default | DeviceSelection::WithIntegrated => None,
        }
    }

    /// The saved GPU numbers.
    pub fn indices(&self) -> &[u32] {
        match self {
            Self::One(index) => std::slice::from_ref(index),
            Self::List(indices) => indices,
        }
    }

    /// The GPU choice this saved value names.
    pub fn selection(&self) -> DeviceSelection {
        DeviceSelection::Indices(self.indices().to_vec())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SavedConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bch_donation_bps: Option<crate::donation::bch::BchDonation>,
    /// #### PR #32: a token donation raised above its minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_donation_bps: Option<crate::donation::TokenDonation>,
    pub network: Option<String>,
    pub token: Option<String>,
    pub backend: Option<String>,
    pub device: Option<SavedDevices>,
    /// Mine on integrated GPUs too when `device` names no GPU.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_integrated: Option<bool>,
    pub intensity: Option<u8>,
    pub address: Option<String>,
    pub fulcrum: Option<String>,
    pub node_rpc: Option<String>,
    pub source: Option<String>,
    /// #### PR #42: what the profile starts besides plain GPU mining. Plain
    /// GPU profiles leave it out, so their saved bytes do not change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<SavedServer>,
}

// #### PR #42: profiles keep their mode
// What: a profile saves the setup's mode (a rig joining a GPU pool or farm,
// ASIC solo, ASIC at a pool, an ASIC pool or a GPU pool) and its pool values,
// and reopens in that mode.
// Why: every profile reopened as GPU mining, so a saved pool or ASIC profile
// lost its pool rows and settings.
// Look here if: an older Pickaxe refuses the profiles file; it does not know
// the `server` field (`deny_unknown_fields`).
/// The kind of miner a profile starts, besides plain GPU mining.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SavedMode {
    /// This computer's GPUs join a GPU pool or farm as a rig.
    GpuRig,
    /// ASICs mine BCH on the miner's own node.
    AsicSolo,
    /// ASICs mine at a remote pool.
    AsicJoin,
    /// A public ASIC pool for other miners.
    AsicPool,
    /// A public GPU pool for other miners' rigs.
    GpuPool,
}

impl SavedMode {
    /// Whether the mode joins someone else's pool or coordinator.
    pub fn joins(self) -> bool {
        matches!(self, Self::GpuRig | Self::AsicJoin)
    }

    /// Whether the mode runs a pool with a fee.
    pub fn runs_a_pool(self) -> bool {
        matches!(self, Self::AsicPool | Self::GpuPool)
    }
}

/// #### PR #42: how a profile that joins a pool declares its node's
/// templates there (SV2 Job Declaration).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SavedDeclaration {
    /// Full-Template: each template with its transactions.
    Full,
    /// Coinbase-only: the coinbase alone; this node submits the blocks.
    Coinbase,
}

/// #### PR #42: the Job Declaration modes a profile's ASIC pool accepts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SavedAccept {
    Full,
    Coinbase,
    Both,
}

/// A profile's mode and the pool values the setup asked for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedServer {
    pub mode: SavedMode,
    /// The pool or coordinator to join: `HOST:PORT`, or a one-line SV2
    /// address with its key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<String>,
    /// The pool's or coordinator's key, when `join` does not carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_fee: Option<crate::donation::bch::BchDonation>,
    /// Where an ASIC pool's fee comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_mode: Option<crate::donation::bch::FeeMode>,
    /// Where the pool fee goes; left out, it goes to the payout address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_address: Option<String>,
    /// An ASIC pool's name in its blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_tag: Option<String>,
    /// #### PR #42: an ASIC server's Advanced values; left out at their
    /// defaults (3333, 3336, the server's start difficulty, no backup pools,
    /// the payout address at the pool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv1_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv2_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_difficulty: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_user: Option<String>,
    /// #### PR #42: the port an ASIC server with its own node serves its
    /// templates on (SV2 Template Distribution); left out, it serves none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tp_port: Option<u16>,
    /// #### PR #42: Join a pool declares this node's templates to the pool
    /// (SV2 Job Declaration); left out, devices mine the pool's own jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_declaration: Option<SavedDeclaration>,
    /// #### PR #42: the Job Declaration modes an ASIC pool accepts from its
    /// miners; left out, it accepts none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_job_declaration: Option<SavedAccept>,
    /// #### PR #42: the pools ASIC solo mining falls back on while its node
    /// gives no work, each a one-line SV2 address with its key, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_pools: Vec<String>,
    /// #### PR #42: ASIC solo mines this ASIC-exclusive token instead of BCH,
    /// by its registry name; left out, BCH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asic_token: Option<String>,
}

impl SavedServer {
    /// Rejects values the setup would refuse for this mode on `network`.
    pub fn validate(&self, network: MiningNetwork) -> Result<(), String> {
        let join = self.join.as_deref().map(str::trim).unwrap_or("");
        if self.mode.joins() {
            if join.is_empty() {
                return Err("a profile that joins a pool needs the pool's address".into());
            }
        } else if self.join.is_some() || self.join_key.is_some() {
            return Err("only a profile that joins a pool saves a pool address".into());
        }
        for value in [join, self.join_key.as_deref().unwrap_or("")] {
            if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err("a pool address or key has no spaces".into());
            }
        }
        if !self.mode.runs_a_pool()
            && (self.pool_fee.is_some() || self.fee_mode.is_some() || self.fee_address.is_some())
        {
            return Err("only a profile that runs a pool saves a pool fee".into());
        }
        if self.mode != SavedMode::AsicPool && (self.fee_mode.is_some() || self.pool_tag.is_some())
        {
            return Err("only an ASIC pool saves where its fee comes from and its name".into());
        }
        if let Some(address) = &self.fee_address {
            match self.mode {
                // A GPU pool's fee is a token claim, which pays a q address.
                SavedMode::GpuPool => validate_payout_address(network, address),
                _ => validate_coinbase_address(network, address),
            }
            .map_err(|error| format!("pool fee address: {error}"))?;
        }
        // #### PR #42: the Advanced values, checked as the setup checks them.
        let asic = matches!(
            self.mode,
            SavedMode::AsicSolo | SavedMode::AsicJoin | SavedMode::AsicPool
        );
        let advanced = self.sv1_port.is_some()
            || self.sv2_port.is_some()
            || self.start_difficulty.is_some()
            || !self.backups.is_empty()
            || self.pool_user.is_some()
            || self.tp_port.is_some();
        if advanced && !asic {
            return Err(
                "only an ASIC server saves ports, a start difficulty or backup pools".into(),
            );
        }
        if self.sv1_port == Some(0) || self.sv2_port == Some(0) {
            return Err("a port is 1 to 65535".into());
        }
        if self.sv1_port.unwrap_or(3333) == self.sv2_port.unwrap_or(3336) {
            return Err("the SV1 and SV2 ports must differ".into());
        }
        // #### PR #42: templates come from this server's own node.
        if let Some(port) = self.tp_port {
            if !matches!(self.mode, SavedMode::AsicSolo | SavedMode::AsicPool) {
                return Err("only an ASIC server with its own node serves templates".into());
            }
            if port == 0
                || port == self.sv1_port.unwrap_or(3333)
                || port == self.sv2_port.unwrap_or(3336)
            {
                return Err(
                    "the template port is 1 to 65535 and differs from the SV1 and SV2 \
                            ports"
                        .into(),
                );
            }
        }
        if self
            .start_difficulty
            .is_some_and(|difficulty| difficulty == 0 || difficulty > 1 << 48)
        {
            return Err("a start difficulty is 1 to 2^48".into());
        }
        if self.mode != SavedMode::AsicJoin
            && (!self.backups.is_empty() || self.pool_user.is_some())
        {
            return Err(
                "only a profile that joins a pool saves backup pools or a pool username".into(),
            );
        }
        if self.backups.len() > 8
            || self.backups.iter().any(|pool| {
                pool.is_empty() || pool.chars().any(|c| c.is_whitespace() || c.is_control())
            })
        {
            return Err("at most 8 backup pools, each without spaces".into());
        }
        // #### PR #42: Job Declaration and fallback pools, each in its mode.
        if self.job_declaration.is_some() && self.mode != SavedMode::AsicJoin {
            return Err("only ASICs joining a pool declare their node's templates".into());
        }
        if self.accept_job_declaration.is_some() && self.mode != SavedMode::AsicPool {
            return Err("only an ASIC pool accepts miners' own templates".into());
        }
        if !self.fallback_pools.is_empty() && self.mode != SavedMode::AsicSolo {
            return Err("only ASIC solo mining saves fallback pools".into());
        }
        if self.fallback_pools.len() > 8
            || self.fallback_pools.iter().any(|pool| {
                pool.is_empty() || pool.chars().any(|c| c.is_whitespace() || c.is_control())
            })
        {
            return Err("at most 8 fallback pools, each without spaces".into());
        }
        // #### PR #42: an ASIC-exclusive token instead of BCH, mined solo
        // with no templates to serve and no fallback pools.
        if let Some(name) = &self.asic_token {
            if self.mode != SavedMode::AsicSolo {
                return Err("only ASIC solo mining saves an ASIC-exclusive token".into());
            }
            if self.tp_port.is_some() || !self.fallback_pools.is_empty() {
                return Err(
                    "an ASIC-exclusive token serves no templates and has no fallback pools".into(),
                );
            }
            if name.trim().is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                return Err(
                    "an ASIC-exclusive token's name is 1 to 64 printable characters".into(),
                );
            }
        }
        if self.pool_user.as_deref().is_some_and(|user| {
            user.is_empty() || user.len() > 255 || user.chars().any(char::is_control)
        }) {
            return Err("a pool username is 1 to 255 printable characters".into());
        }
        if let Some(tag) = &self.pool_tag {
            let tag = tag.trim();
            if tag.is_empty()
                || tag.len() > 20
                || !tag.chars().all(|c| c.is_ascii_graphic() || c == ' ')
            {
                return Err("a pool's name is 1 to 20 printable characters".into());
            }
        }
        Ok(())
    }
}
// #### end PR #42 ####

impl SavedConfig {
    /// Applies saved configuration values to a running miner.
    pub fn apply_to_runtime(&self, cfg: &mut RuntimeConfig) -> Result<(), String> {
        if let Some(donation) = self.bch_donation_bps {
            cfg.bch_donation = donation;
        }
        if let Some(donation) = self.token_donation_bps {
            cfg.token_donation = Some(donation);
        }
        if let Some(value) = &self.network {
            cfg.set_network(MiningNetwork::parse(value)?);
        }
        if let Some(value) = &self.token {
            cfg.set_token(value)?;
        }
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
            crate::backend_kind::BackendKind::parse(value)?;
        }
        if let Some(SavedDevices::List(indices)) = &self.device {
            if indices.is_empty() {
                return Err("device list is empty; omit device to mine on every GPU".into());
            }
            if let Some(twice) = indices
                .iter()
                .enumerate()
                .find_map(|(position, index)| indices[..position].contains(index).then_some(index))
            {
                return Err(format!("device lists GPU {twice} twice"));
            }
        }
        let mut runtime = RuntimeConfig::default();
        self.apply_to_runtime(&mut runtime)?;
        // #### PR #42: a saved mode's pool values are checked as the setup
        // checks them.
        if let Some(server) = &self.server {
            server.validate(runtime.network)?;
        }
        runtime.validate_payout_network()
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

    /// The saved GPU choice, integrated GPUs included when saved so.
    pub fn device_selection(&self) -> DeviceSelection {
        self.device
            .as_ref()
            .map_or_else(DeviceSelection::default, SavedDevices::selection)
            .with_integrated(self.include_integrated == Some(true))
    }

    /// Captures the effective runtime settings for persistence.
    pub fn from_effective(
        backend: &str,
        devices: &DeviceSelection,
        runtime: &RuntimeConfig,
    ) -> Self {
        let address = if runtime.payout_address.is_empty() {
            None
        } else {
            Some(runtime.payout_address.clone())
        };
        Self {
            network: Some(runtime.network.as_str().to_string()),
            bch_donation_bps: (runtime.bch_donation != Default::default())
                .then_some(runtime.bch_donation),
            token_donation_bps: (runtime.token_donation() != runtime.token.donation_minimum())
                .then(|| runtime.token_donation()),
            token: Some(runtime.token.as_str().to_string()),
            backend: Some(backend.to_string()),
            device: SavedDevices::from_selection(devices),
            include_integrated: (*devices == DeviceSelection::WithIntegrated).then_some(true),
            intensity: Some(runtime.intensity),
            address,
            fulcrum: runtime.fulcrum_url.clone(),
            node_rpc: runtime.node_url.clone(),
            source: Some(runtime.source.as_str().to_string()),
            server: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MiningProfile {
    pub name: String,
    pub settings: SavedConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MiningProfiles {
    pub profiles: Vec<MiningProfile>,
}

pub fn profiles_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("profiles.json")
}

pub fn sources_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("sources.json")
}

/// Most user-added connections kept per network and kind.
const MAX_SHARED_SOURCES: usize = 16;

/// A user-added connection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionKind {
    Fulcrum,
    Node,
}

/// Fulcrum servers and nodes the user added for one network.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkSources {
    pub fulcrum: Vec<String>,
    pub node_rpc: Vec<String>,
}

/// User-added connections, kept per network and shared by every profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SharedSources {
    pub mainnet: NetworkSources,
    pub chipnet: NetworkSources,
}

impl SharedSources {
    /// Loads the shared connections, or none when the file does not exist.
    pub fn load_optional(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)
            .map_err(|error| format!("read connections {}: {error}", path.display()))?;
        let store: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse connections {}: {error}", path.display()))?;
        for network in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
            for kind in [ConnectionKind::Fulcrum, ConnectionKind::Node] {
                let list = store.list(network, kind);
                if list.len() > MAX_SHARED_SOURCES {
                    return Err(format!("too many saved connections in {}", path.display()));
                }
                for entry in list {
                    Self::validate_entry(network, kind, entry)?;
                }
            }
        }
        Ok(store)
    }

    /// Persists the shared connections with private file permissions, since
    /// node URLs may carry credentials.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                format!("create connections directory {}: {error}", parent.display())
            })?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize connections: {error}"))?;
        write_private_config(path, &bytes)
    }

    fn sources(&self, network: MiningNetwork) -> &NetworkSources {
        match network {
            MiningNetwork::Mainnet => &self.mainnet,
            MiningNetwork::Chipnet => &self.chipnet,
        }
    }

    /// Returns the user-added connections of one kind for a network.
    pub fn list(&self, network: MiningNetwork, kind: ConnectionKind) -> &[String] {
        let sources = self.sources(network);
        match kind {
            ConnectionKind::Fulcrum => &sources.fulcrum,
            ConnectionKind::Node => &sources.node_rpc,
        }
    }

    fn list_mut(&mut self, network: MiningNetwork, kind: ConnectionKind) -> &mut Vec<String> {
        let sources = match network {
            MiningNetwork::Mainnet => &mut self.mainnet,
            MiningNetwork::Chipnet => &mut self.chipnet,
        };
        match kind {
            ConnectionKind::Fulcrum => &mut sources.fulcrum,
            ConnectionKind::Node => &mut sources.node_rpc,
        }
    }

    /// Validates one connection for a network and returns its normalized form.
    pub fn validate_entry(
        network: MiningNetwork,
        kind: ConnectionKind,
        value: &str,
    ) -> Result<String, String> {
        let value = value.trim();
        if value.contains(',') {
            return Err("enter one connection at a time".into());
        }
        let mut runtime = RuntimeConfig::default();
        runtime.set_network(network);
        let normalized = match kind {
            ConnectionKind::Fulcrum => {
                runtime.set_fulcrum_url(value)?;
                runtime.fulcrum_url
            }
            ConnectionKind::Node => {
                runtime.set_node_url(value)?;
                runtime.node_url
            }
        };
        normalized.ok_or_else(|| "connection URL is empty".into())
    }

    /// Adds a connection, or replaces the one at `index`, for every profile on
    /// that network.
    pub fn put(
        &mut self,
        network: MiningNetwork,
        kind: ConnectionKind,
        index: Option<usize>,
        value: &str,
    ) -> Result<(), String> {
        let entry = Self::validate_entry(network, kind, value)?;
        let list = self.list_mut(network, kind);
        if list
            .iter()
            .enumerate()
            .any(|(other, existing)| Some(other) != index && existing.eq_ignore_ascii_case(&entry))
        {
            return Err("that connection is already saved".into());
        }
        match index.filter(|index| *index < list.len()) {
            Some(index) => list[index] = entry,
            None if list.len() >= MAX_SHARED_SOURCES => {
                return Err(format!(
                    "at most {MAX_SHARED_SOURCES} saved connections per network"
                ))
            }
            None => list.push(entry),
        }
        Ok(())
    }

    /// Removes a saved connection from every profile on that network.
    pub fn remove(&mut self, network: MiningNetwork, kind: ConnectionKind, index: usize) {
        let list = self.list_mut(network, kind);
        if index < list.len() {
            list.remove(index);
        }
    }

    /// Puts this network's saved connections on `cfg`, tried before the
    /// built-in servers; clears them when none are saved.
    pub fn apply_to_runtime(&self, cfg: &mut RuntimeConfig) -> Result<(), String> {
        cfg.set_fulcrum_url(&self.list(cfg.network, ConnectionKind::Fulcrum).join(","))?;
        cfg.set_node_url(&self.list(cfg.network, ConnectionKind::Node).join(","))
    }

    /// Adds each entry of a comma-separated legacy list; returns whether any
    /// was new. Invalid or duplicate entries are skipped rather than blocking
    /// startup.
    fn adopt_list(&mut self, network: MiningNetwork, kind: ConnectionKind, list: &str) -> bool {
        let mut added = false;
        for entry in list
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            added |= self.put(network, kind, None, entry).is_ok();
        }
        added
    }

    /// Moves servers and nodes saved inside profiles into the shared lists of
    /// each profile's network. Returns whether any profile changed.
    pub fn adopt_profile_sources(&mut self, profiles: &mut MiningProfiles) -> bool {
        let mut moved = false;
        for profile in &mut profiles.profiles {
            let network = profile
                .settings
                .network
                .as_deref()
                .and_then(|value| MiningNetwork::parse(value).ok())
                .unwrap_or(MiningNetwork::Mainnet);
            for (kind, field) in [
                (ConnectionKind::Fulcrum, profile.settings.fulcrum.take()),
                (ConnectionKind::Node, profile.settings.node_rpc.take()),
            ] {
                if let Some(field) = field {
                    moved = true;
                    self.adopt_list(network, kind, &field);
                }
            }
        }
        moved
    }

    /// Copies servers and nodes from the saved base configuration into the
    /// shared lists. Returns whether any entry was new.
    pub fn adopt_saved_config(&mut self, saved: &SavedConfig) -> bool {
        let network = saved
            .network
            .as_deref()
            .and_then(|value| MiningNetwork::parse(value).ok())
            .unwrap_or(MiningNetwork::Mainnet);
        let fulcrum = saved
            .fulcrum
            .as_deref()
            .is_some_and(|list| self.adopt_list(network, ConnectionKind::Fulcrum, list));
        let node = saved
            .node_rpc
            .as_deref()
            .is_some_and(|list| self.adopt_list(network, ConnectionKind::Node, list));
        fulcrum || node
    }
}

fn valid_profile_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        Err("profile name must contain 1–40 printable characters".into())
    } else {
        Ok(name)
    }
}

impl MiningProfiles {
    pub fn load_optional(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes =
            fs::read(path).map_err(|error| format!("read profiles {}: {error}", path.display()))?;
        let store: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse profiles {}: {error}", path.display()))?;
        if store.profiles.len() > 64 {
            return Err("too many mining profiles (maximum 64)".into());
        }
        for (index, profile) in store.profiles.iter().enumerate() {
            valid_profile_name(&profile.name)?;
            profile.settings.validate()?;
            if store.profiles[..index]
                .iter()
                .any(|other| other.name.eq_ignore_ascii_case(&profile.name))
            {
                return Err("duplicate mining profile name".into());
            }
        }
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                format!("create profiles directory {}: {error}", parent.display())
            })?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize profiles: {error}"))?;
        write_private_config(path, &bytes)
    }

    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), String> {
        let name = valid_profile_name(name)?;
        if self
            .profiles
            .iter()
            .enumerate()
            .any(|(other_index, profile)| {
                other_index != index && profile.name.eq_ignore_ascii_case(name)
            })
        {
            return Err("a mining profile already has that name".into());
        }
        self.profiles
            .get_mut(index)
            .ok_or("profile no longer exists")?
            .name = name.into();
        Ok(())
    }

    /// Deletes a saved profile and returns its name.
    pub fn remove(&mut self, index: usize) -> Result<String, String> {
        if index >= self.profiles.len() {
            return Err("profile no longer exists".into());
        }
        Ok(self.profiles.remove(index).name)
    }

    pub fn upsert(
        &mut self,
        index: Option<usize>,
        name: &str,
        settings: SavedConfig,
    ) -> Result<String, String> {
        settings.validate()?;
        let name = if !name.trim().is_empty() {
            valid_profile_name(name)?.to_string()
        } else if let Some(existing) = index.and_then(|index| self.profiles.get(index)) {
            existing.name.clone()
        } else {
            loop {
                let candidate = format!("Miner {:08X}", rand::random::<u32>());
                if !self
                    .profiles
                    .iter()
                    .any(|profile| profile.name == candidate)
                {
                    break candidate;
                }
            }
        };
        if let Some(index) = index {
            self.rename(index, &name)?;
            self.profiles[index].settings = settings;
        } else {
            if self.profiles.len() >= 64 {
                return Err("too many mining profiles (maximum 64)".into());
            }
            if self
                .profiles
                .iter()
                .any(|profile| profile.name.eq_ignore_ascii_case(&name))
            {
                return Err("a mining profile already has that name".into());
            }
            self.profiles.push(MiningProfile {
                name: name.clone(),
                settings,
            });
        }
        Ok(name)
    }
}

/// #### PR #42
/// What: a private file written whole: a temporary file beside it is made
/// owner-only before anything is written, synced, then renamed over the old
/// one, so a crash leaves the old file or the new one, never half of one.
/// Why: device logins (and later other private server files) are rewritten
/// while the server runs.
/// Look here if: a private file is readable by other users, or a crash
/// leaves a half-written one.
#[cfg(feature = "stratum-v2")]
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{:016x}", rand::random::<u64>()));
    let temp = PathBuf::from(temp);
    let written = write_private_config(&temp, bytes).and_then(|()| {
        fs::rename(&temp, path).map_err(|error| format!("write {}: {error}", path.display()))
    });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
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
            #[cfg(not(target_arch = "wasm32"))]
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

pub(crate) fn restrict_private_config(path: &Path) -> Result<(), String> {
    #[cfg(not(any(unix, windows)))]
    let _ = path;
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

    // #### PR #32
    #[test]
    fn a_raised_photon_donation_adds_only_to_the_project_share() {
        use crate::donation::TokenDonation;
        let six = TokenDonation::from_bps(600);
        let mainnet = MiningToken::Photon.fee_policy_at(MiningNetwork::Mainnet, six);
        assert_eq!(mainnet.scheme.work(), [400, 200]);
        assert_eq!(mainnet.scheme.description(), "Donation: 6%");
        let chipnet = MiningToken::Photon.fee_policy_at(MiningNetwork::Chipnet, six);
        assert_eq!(chipnet.scheme.work(), [600, 0]);
        // Below the minimum, the minimum applies.
        let low =
            MiningToken::Photon.fee_policy_at(MiningNetwork::Mainnet, TokenDonation::from_bps(100));
        assert_eq!(low.scheme.work(), [200, 200]);
        let mut cfg = RuntimeConfig::default();
        assert_eq!(cfg.token_donation().bps(), 400);
        cfg.token_donation = Some(TokenDonation::from_bps(150));
        assert_eq!(cfg.token_donation().bps(), 400);
        assert_eq!(cfg.fee_policy().scheme.work(), [200, 200]);
        // The saved setting only holds a raised value.
        cfg.set_payout(PAYOUT.into()).unwrap();
        let saved = SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &cfg);
        assert_eq!(saved.token_donation_bps, None);
        cfg.token_donation = Some(six);
        let saved = SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &cfg);
        assert_eq!(saved.token_donation_bps, Some(six));
        assert_eq!(
            MiningToken::Photon.donation_minimum(),
            TokenDonation::from_bps(400)
        );
        assert_eq!(crate::donation::NEW_TOKEN_DONATION.bps(), 150);
    }

    #[test]
    fn photon_donation_routes_by_network() {
        let mainnet = MiningToken::Photon.fee_policy(MiningNetwork::Mainnet);
        assert_eq!(mainnet.scheme.work(), [200, 200]);
        assert_eq!(
            mainnet.addresses,
            [DONATION_ADDRESS, SHREC_DONATION_ADDRESS]
        );
        let chipnet = MiningToken::Photon.fee_policy(MiningNetwork::Chipnet);
        assert_eq!(chipnet.scheme.work(), [400, 0]);
        assert_eq!(
            chipnet.addresses,
            [CHIPNET_DONATION_ADDRESS, CHIPNET_DONATION_ADDRESS]
        );
        let miner = crate::tx::token_p2pkh_hash_to_cashaddr_for_network(
            &[0x42; 20],
            MiningNetwork::Chipnet,
        )
        .unwrap();
        let payouts = chipnet.payouts(MiningNetwork::Chipnet, &miner).unwrap();
        assert_eq!(payouts[0], miner);
        assert_eq!(payouts[1], CHIPNET_DONATION_ADDRESS);
        assert_eq!(payouts[2], CHIPNET_DONATION_ADDRESS);
    }

    #[test]
    fn shared_connections_are_validated_kept_per_network_and_persisted() {
        let mut sources = SharedSources::default();
        sources
            .put(
                MiningNetwork::Mainnet,
                ConnectionKind::Fulcrum,
                None,
                "wss://one.test:50004",
            )
            .unwrap();
        assert!(sources
            .put(
                MiningNetwork::Mainnet,
                ConnectionKind::Fulcrum,
                None,
                "WSS://ONE.test:50004"
            )
            .unwrap_err()
            .contains("already saved"));
        assert!(sources
            .put(
                MiningNetwork::Mainnet,
                ConnectionKind::Fulcrum,
                None,
                "https://one.test"
            )
            .is_err());
        assert!(sources
            .put(
                MiningNetwork::Mainnet,
                ConnectionKind::Fulcrum,
                None,
                "wss://a.test, wss://b.test"
            )
            .unwrap_err()
            .contains("one connection at a time"));
        // A published server of the other chain is refused.
        assert!(sources
            .put(
                MiningNetwork::Chipnet,
                ConnectionKind::Fulcrum,
                None,
                crate::protocol::FULCRUM_WSS_BOOTSTRAP[0],
            )
            .is_err());
        sources
            .put(
                MiningNetwork::Chipnet,
                ConnectionKind::Node,
                None,
                "http://127.0.0.1:18332",
            )
            .unwrap();
        sources
            .put(
                MiningNetwork::Mainnet,
                ConnectionKind::Fulcrum,
                Some(0),
                "wss://two.test:50004",
            )
            .unwrap();
        assert_eq!(
            sources.list(MiningNetwork::Mainnet, ConnectionKind::Fulcrum),
            ["wss://two.test:50004"]
        );

        let mut mainnet = RuntimeConfig::default();
        sources.apply_to_runtime(&mut mainnet).unwrap();
        assert_eq!(mainnet.fulcrum_url.as_deref(), Some("wss://two.test:50004"));
        assert!(mainnet.node_url.is_none());
        let mut chipnet = RuntimeConfig::default();
        chipnet.set_network(MiningNetwork::Chipnet);
        sources.apply_to_runtime(&mut chipnet).unwrap();
        assert!(chipnet.fulcrum_url.is_none());
        assert_eq!(chipnet.node_url.as_deref(), Some("http://127.0.0.1:18332"));

        let path =
            std::env::temp_dir().join(format!("pickaxe-sources-{}.json", std::process::id()));
        sources.save(&path).unwrap();
        assert_eq!(SharedSources::load_optional(&path).unwrap(), sources);
        sources.remove(MiningNetwork::Mainnet, ConnectionKind::Fulcrum, 0);
        assert!(sources
            .list(MiningNetwork::Mainnet, ConnectionKind::Fulcrum)
            .is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn profile_and_base_config_connections_move_to_the_shared_lists() {
        let mut chipnet = RuntimeConfig::default();
        chipnet.set_network(MiningNetwork::Chipnet);
        chipnet
            .set_fulcrum_url("wss://mine.test:50004, wss://second.test:50004")
            .unwrap();
        let mut mainnet = RuntimeConfig::default();
        mainnet.set_node_url("http://127.0.0.1:8332").unwrap();
        let mut profiles = MiningProfiles::default();
        profiles
            .upsert(
                None,
                "Chip",
                SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &chipnet),
            )
            .unwrap();
        profiles
            .upsert(
                None,
                "Main",
                SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &mainnet),
            )
            .unwrap();

        let mut sources = SharedSources::default();
        assert!(sources.adopt_profile_sources(&mut profiles));
        assert_eq!(
            sources.list(MiningNetwork::Chipnet, ConnectionKind::Fulcrum),
            ["wss://mine.test:50004", "wss://second.test:50004"]
        );
        assert_eq!(
            sources.list(MiningNetwork::Mainnet, ConnectionKind::Node),
            ["http://127.0.0.1:8332"]
        );
        assert!(profiles.profiles.iter().all(
            |profile| profile.settings.fulcrum.is_none() && profile.settings.node_rpc.is_none()
        ));
        assert!(!sources.adopt_profile_sources(&mut profiles));

        let base =
            SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &chipnet);
        assert!(
            !sources.adopt_saved_config(&base),
            "already saved entries add nothing"
        );
        let mut other = mainnet.clone();
        other.set_fulcrum_url("wss://base.test:50004").unwrap();
        assert!(sources.adopt_saved_config(&SavedConfig::from_effective(
            "cuda",
            &DeviceSelection::Indices(vec![0]),
            &other
        )));
        assert_eq!(
            sources.list(MiningNetwork::Mainnet, ConnectionKind::Fulcrum),
            ["wss://base.test:50004"]
        );
    }

    #[test]
    fn profiles_can_be_removed_by_index() {
        let mut profiles = MiningProfiles::default();
        profiles
            .upsert(
                None,
                "Only",
                SavedConfig::from_effective(
                    "cuda",
                    &DeviceSelection::Indices(vec![0]),
                    &RuntimeConfig::default(),
                ),
            )
            .unwrap();
        assert!(profiles.remove(1).is_err());
        assert_eq!(profiles.remove(0).unwrap(), "Only");
        assert!(profiles.profiles.is_empty());
    }

    #[test]
    fn comma_separated_connections_keep_order_and_reject_bad_entries() {
        let mut cfg = RuntimeConfig::default();
        cfg.set_fulcrum_url("wss://one.test:50004, wss://two.test:50004")
            .unwrap();
        cfg.set_node_url("http://one.test:8332, https://two.test:8332")
            .unwrap();
        assert_eq!(
            cfg.custom_fulcrum_endpoints(),
            ["wss://one.test:50004", "wss://two.test:50004"]
        );
        assert_eq!(
            cfg.custom_node_endpoints(),
            ["http://one.test:8332", "https://two.test:8332"]
        );
        assert_eq!(cfg.node_endpoints().len(), 2);
        assert!(cfg.set_node_url("http://one.test:8332,").is_err());
        assert!(cfg
            .set_fulcrum_url("wss://one.test:50004;wss://two.test:50004")
            .is_err());
    }

    #[test]
    fn mining_profiles_save_settings_and_support_rename() {
        let path =
            std::env::temp_dir().join(format!("pickaxe-profiles-{}.json", std::process::id()));
        let mut runtime = RuntimeConfig::default();
        runtime.set_payout(PAYOUT.into()).unwrap();
        runtime.set_intensity(70).unwrap();
        runtime
            .set_node_url("http://127.0.0.1:8332, http://localhost:8332")
            .unwrap();
        let mut profiles = MiningProfiles::default();
        let random_name = profiles
            .upsert(
                None,
                "",
                SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &runtime),
            )
            .unwrap();
        assert!(random_name.starts_with("Miner "));
        profiles.save(&path).unwrap();
        let mut loaded = MiningProfiles::load_optional(&path).unwrap();
        loaded.rename(0, "Home rig").unwrap();
        loaded.save(&path).unwrap();
        let loaded = MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(loaded.profiles[0].name, "Home rig");
        assert_eq!(loaded.profiles[0].settings.address.as_deref(), Some(PAYOUT));
        assert_eq!(loaded.profiles[0].settings.intensity, Some(70));
        assert_eq!(
            loaded.profiles[0].settings.node_rpc.as_deref(),
            Some("http://127.0.0.1:8332, http://localhost:8332")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn mining_selection_uses_the_token_deployment_on_each_chain() {
        for query in [
            "photon",
            crate::protocol::MAINNET_CATEGORY_HEX,
            crate::protocol::COVENANT_LOCKING_BYTECODE_HEX,
        ] {
            assert_eq!(
                MiningToken::parse(query, MiningNetwork::Mainnet).unwrap(),
                MiningToken::Photon
            );
        }
        assert!(MiningToken::parse("wrong", MiningNetwork::Mainnet).is_err());
        let mut cfg = RuntimeConfig::default();
        assert!(cfg.ensure_mining_supported().is_ok());
        cfg.set_network(MiningNetwork::Chipnet);
        assert!(cfg.ensure_mining_supported().is_ok());
        for query in [
            "PHOTON",
            crate::protocol::CHIPNET_CATEGORY_HEX,
            crate::protocol::CHIPNET_COVENANT_LOCKING_BYTECODE_HEX,
            crate::protocol::CHIPNET_COVENANT_ADDRESS,
        ] {
            assert_eq!(
                MiningToken::parse(query, cfg.network).unwrap(),
                MiningToken::Photon
            );
        }
        assert!(MiningToken::parse(crate::protocol::MAINNET_CATEGORY_HEX, cfg.network).is_err());
        assert_eq!(
            cfg.electrum_endpoints(),
            crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP
                .iter()
                .map(|url| (*url).to_string())
                .collect::<Vec<_>>()
        );
        assert!(cfg.electrum_endpoints().iter().all(|endpoint| {
            !crate::protocol::FULCRUM_WSS_BOOTSTRAP.contains(&endpoint.as_str())
        }));
        assert!(cfg.node_endpoints().is_empty());
    }

    #[test]
    fn network_switch_never_routes_to_foreign_published_fulcrum() {
        let mut cfg = RuntimeConfig::default();
        cfg.set_fulcrum_url(crate::protocol::FULCRUM_WSS_BOOTSTRAP[0])
            .unwrap();
        cfg.set_network(MiningNetwork::Chipnet);
        assert!(cfg.fulcrum_url.is_none());
        assert_eq!(
            cfg.electrum_endpoints(),
            crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP
                .iter()
                .map(|url| (*url).to_string())
                .collect::<Vec<_>>()
        );
        assert!(cfg
            .set_fulcrum_url(crate::protocol::FULCRUM_WSS_BOOTSTRAP[0])
            .is_err());
    }

    #[test]
    fn network_switch_clears_old_sources_and_accepts_new_explicit_overrides() {
        let mut cfg = RuntimeConfig::default();
        cfg.set_fulcrum_url("wss://old-network.invalid:50004")
            .unwrap();
        cfg.set_node_url("http://old-network.invalid:8332").unwrap();
        cfg.set_network(MiningNetwork::Chipnet);
        assert!(cfg.custom_fulcrum_endpoints().is_empty());
        assert!(cfg.custom_node_endpoints().is_empty());
        assert_eq!(
            cfg.electrum_endpoints(),
            crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP
                .iter()
                .map(|url| (*url).to_string())
                .collect::<Vec<_>>()
        );
        cfg.set_fulcrum_url("wss://explicit-chipnet.invalid:50004")
            .unwrap();
        cfg.set_node_url("http://explicit-chipnet.invalid:18332")
            .unwrap();
        assert_eq!(
            cfg.custom_fulcrum_endpoints(),
            ["wss://explicit-chipnet.invalid:50004"]
        );
        assert_eq!(
            cfg.custom_node_endpoints(),
            ["http://explicit-chipnet.invalid:18332"]
        );
    }

    #[test]
    fn payout_address_must_match_selected_network() {
        for (network, other) in [
            (MiningNetwork::Mainnet, MiningNetwork::Chipnet),
            (MiningNetwork::Chipnet, MiningNetwork::Mainnet),
        ] {
            for encode in [
                crate::tx::p2pkh_hash_to_cashaddr_for_network,
                crate::tx::token_p2pkh_hash_to_cashaddr_for_network,
            ] {
                let address = encode(&[0x42; 20], network).unwrap();
                let foreign = encode(&[0x42; 20], other).unwrap();
                assert!(MiningToken::Photon
                    .fee_policy(network)
                    .payouts(network, &foreign)
                    .is_err());
                let mut cfg = RuntimeConfig {
                    network,
                    ..Default::default()
                };
                cfg.set_payout(address.clone()).unwrap();
                let generation = cfg.generation_id;
                for invalid in [foreign.clone(), foreign.split_once(':').unwrap().1.into()] {
                    assert!(cfg.set_payout(invalid).is_err());
                    assert_eq!(cfg.payout_address, address);
                    assert_eq!(cfg.generation_id, generation);
                }
                for valid in [
                    address.to_ascii_uppercase(),
                    address.split_once(':').unwrap().1.into(),
                    format!("  {address}  "),
                ] {
                    cfg.set_payout(valid).unwrap();
                    assert_eq!(cfg.payout_address, address);
                    assert!(cfg.validate_payout_network().is_ok());
                }
                cfg.set_network(other);
                assert_eq!(
                    cfg.payout_address, address,
                    "network switching must not rewrite the wallet"
                );
                assert!(cfg.ensure_mining_supported().is_err());
                cfg.set_payout(foreign).unwrap();
                assert!(cfg.ensure_mining_supported().is_ok());
            }
        }
    }

    #[test]
    fn saved_config_and_profiles_reject_foreign_payouts() {
        for (network, other) in [
            (MiningNetwork::Mainnet, MiningNetwork::Chipnet),
            (MiningNetwork::Chipnet, MiningNetwork::Mainnet),
        ] {
            let saved = SavedConfig {
                network: Some(network.as_str().into()),
                address: Some(
                    crate::tx::token_p2pkh_hash_to_cashaddr_for_network(&[0x42; 20], other)
                        .unwrap(),
                ),
                ..SavedConfig::default()
            };
            assert!(saved
                .apply_to_runtime(&mut RuntimeConfig::default())
                .is_err());
            assert!(saved.validate().is_err());
            assert!(MiningProfiles::default()
                .upsert(None, "Wrong network", saved)
                .is_err());
        }
    }

    // #### PR #42
    #[cfg(feature = "stratum-v2")]
    #[test]
    fn private_atomic_write_replaces_whole_and_leaves_no_temporary_file() {
        let dir =
            std::env::temp_dir().join(format!("pickaxe-private-{:016x}", rand::random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("logins.json");
        write_private_atomic(&path, b"first").unwrap();
        write_private_atomic(&path, b"second, longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second, longer");
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(dir);
    }

    // #### PR #42
    #[test]
    fn a_plain_gpu_profile_saves_no_server_and_bad_pool_values_are_refused() {
        let plain = SavedConfig {
            network: Some("chipnet".into()),
            ..SavedConfig::default()
        };
        assert!(!serde_json::to_string(&plain).unwrap().contains("server"));
        let with = |server: SavedServer| SavedConfig {
            network: Some("chipnet".into()),
            server: Some(server),
            ..SavedConfig::default()
        };
        let pool = SavedServer {
            mode: SavedMode::AsicPool,
            join: None,
            join_key: None,
            pool_fee: Some("2".parse().unwrap()),
            fee_mode: Some(crate::donation::bch::FeeMode::Both),
            fee_address: None,
            pool_tag: Some("/MyPool/".into()),
            sv1_port: Some(3338),
            sv2_port: None,
            start_difficulty: Some(65_536),
            backups: Vec::new(),
            pool_user: None,
            tp_port: Some(48442),
            job_declaration: None,
            accept_job_declaration: Some(SavedAccept::Both),
            fallback_pools: Vec::new(),
            asic_token: None,
        };
        with(pool.clone()).validate().unwrap();
        let text = serde_json::to_string(&with(pool.clone())).unwrap();
        assert!(text.contains("\"mode\":\"asic-pool\""), "{text}");
        let back: SavedConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.server, Some(pool.clone()));
        let joining = SavedServer {
            mode: SavedMode::AsicJoin,
            join: Some("stratum2+tcp://pool.example:3336/KEY".into()),
            join_key: None,
            pool_fee: None,
            fee_mode: None,
            fee_address: None,
            pool_tag: None,
            sv1_port: None,
            sv2_port: None,
            start_difficulty: None,
            backups: vec!["stratum2+tcp://backup.example:3336/KEY".into()],
            pool_user: Some("rig-owner".into()),
            tp_port: None,
            job_declaration: Some(SavedDeclaration::Full),
            accept_job_declaration: None,
            fallback_pools: Vec::new(),
            asic_token: None,
        };
        with(joining.clone()).validate().unwrap();
        // #### PR #42: Job Declaration and fallback pools round trip.
        let text = serde_json::to_string(&with(joining.clone())).unwrap();
        assert!(text.contains("\"job_declaration\":\"full\""), "{text}");
        let back: SavedConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.server, Some(joining.clone()));
        let solo = SavedServer {
            mode: SavedMode::AsicSolo,
            join: None,
            backups: Vec::new(),
            pool_user: None,
            job_declaration: None,
            fallback_pools: vec!["stratum2+tcp://fallback.example:3336/KEY".into()],
            ..joining.clone()
        };
        with(solo.clone()).validate().unwrap();
        let text = serde_json::to_string(&with(solo.clone())).unwrap();
        let back: SavedConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.server, Some(solo.clone()));
        // #### PR #42: an ASIC-exclusive token round trips too.
        let token = SavedServer {
            fallback_pools: Vec::new(),
            asic_token: Some("Pickaxe ASIC test token".into()),
            ..solo.clone()
        };
        with(token.clone()).validate().unwrap();
        let text = serde_json::to_string(&with(token.clone())).unwrap();
        assert!(
            text.contains("\"asic_token\":\"Pickaxe ASIC test token\""),
            "{text}"
        );
        let back: SavedConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.server, Some(token.clone()));
        for (bad, why) in [
            (
                SavedServer {
                    pool_tag: Some("x".repeat(21)),
                    ..pool.clone()
                },
                "a 21-character pool name",
            ),
            (
                SavedServer {
                    join: None,
                    ..joining.clone()
                },
                "joining without an address",
            ),
            (
                SavedServer {
                    join: Some("pool.example 3336".into()),
                    ..joining.clone()
                },
                "a space in the address",
            ),
            (
                SavedServer {
                    mode: SavedMode::AsicSolo,
                    ..pool.clone()
                },
                "a fee on solo mining",
            ),
            (
                SavedServer {
                    fee_address: Some("not-an-address".into()),
                    ..pool.clone()
                },
                "a bad fee address",
            ),
            (
                SavedServer {
                    mode: SavedMode::GpuPool,
                    ..pool.clone()
                },
                "a GPU pool with an ASIC pool's name and fee source",
            ),
            (
                SavedServer {
                    sv1_port: Some(3336),
                    ..pool.clone()
                },
                "the SV1 port on the SV2 port",
            ),
            (
                SavedServer {
                    start_difficulty: Some(0),
                    ..pool.clone()
                },
                "a start difficulty of 0",
            ),
            (
                SavedServer {
                    backups: vec!["a b".into()],
                    ..joining.clone()
                },
                "a backup pool with a space",
            ),
            (
                SavedServer {
                    pool_user: Some("rig".into()),
                    ..pool.clone()
                },
                "a pool username on an ASIC pool",
            ),
            // #### PR #42
            (
                SavedServer {
                    tp_port: Some(48442),
                    ..joining.clone()
                },
                "templates served while joining a pool",
            ),
            (
                SavedServer {
                    tp_port: Some(3338),
                    ..pool.clone()
                },
                "the template port on the SV1 port",
            ),
            (
                SavedServer {
                    tp_port: Some(0),
                    ..pool.clone()
                },
                "template port 0",
            ),
            (
                SavedServer {
                    job_declaration: Some(SavedDeclaration::Coinbase),
                    ..solo.clone()
                },
                "Job Declaration without a pool to join",
            ),
            (
                SavedServer {
                    accept_job_declaration: Some(SavedAccept::Full),
                    ..joining.clone()
                },
                "accepting miners' templates while joining a pool",
            ),
            (
                SavedServer {
                    fallback_pools: vec!["stratum2+tcp://fallback.example:3336/KEY".into()],
                    ..joining.clone()
                },
                "fallback pools while joining a pool",
            ),
            (
                SavedServer {
                    fallback_pools: vec!["a b".into()],
                    ..solo.clone()
                },
                "a fallback pool with a space",
            ),
            (
                SavedServer {
                    fallback_pools: vec!["stratum2+tcp://f.example:3336/KEY".into(); 9],
                    ..solo.clone()
                },
                "nine fallback pools",
            ),
            (
                SavedServer {
                    asic_token: Some("Pickaxe ASIC test token".into()),
                    ..joining.clone()
                },
                "an ASIC-exclusive token while joining a pool",
            ),
            (
                SavedServer {
                    tp_port: Some(48442),
                    ..token.clone()
                },
                "an ASIC-exclusive token serving templates",
            ),
            (
                SavedServer {
                    fallback_pools: vec!["stratum2+tcp://f.example:3336/KEY".into()],
                    ..token.clone()
                },
                "an ASIC-exclusive token with fallback pools",
            ),
            (
                SavedServer {
                    asic_token: Some(" ".into()),
                    ..token.clone()
                },
                "a blank token name",
            ),
        ] {
            assert!(with(bad).validate().is_err(), "{why}");
        }
    }

    // #### PR #42
    // What: a plain-TCP Fulcrum server is taken on this computer, the home
    // network, Tailscale or a .local name, with its port; a public host or a
    // missing port is refused, in the runtime and in the shared sources.
    // Look here if: check_tcp_electrum or private_host changes.
    #[test]
    fn tcp_fulcrum_is_refused_for_public_hosts() {
        for home in [
            "tcp://192.168.1.5:50001",
            "tcp://10.0.0.2:50001",
            "tcp://127.0.0.1:50001",
            "tcp://localhost:50001",
            "tcp://umbrel.local:50001",
            "tcp://100.101.102.103:50001",
            "tcp://[fd00::5]:50001",
            "TCP://192.168.1.5:50002",
        ] {
            let mut cfg = RuntimeConfig::default();
            cfg.set_fulcrum_url(home)
                .unwrap_or_else(|error| panic!("{home}: {error}"));
        }
        for (public, why) in [
            ("tcp://fulcrum.example.com:50001", "home network"),
            ("tcp://8.8.8.8:50001", "home network"),
            ("tcp://[2001:db8::1]:50001", "home network"),
            ("tcp://100.200.1.1:50001", "home network"),
            ("tcp://192.168.1.5", "needs its port"),
            ("tcp://192.168.1.5:0", "needs its port"),
        ] {
            let mut cfg = RuntimeConfig::default();
            let error = cfg.set_fulcrum_url(public).unwrap_err();
            assert!(error.contains(why), "{public}: {error}");
        }
        assert!(SharedSources::validate_entry(
            MiningNetwork::Mainnet,
            ConnectionKind::Fulcrum,
            "tcp://fulcrum.example.com:50001"
        )
        .is_err());
        assert_eq!(
            SharedSources::validate_entry(
                MiningNetwork::Chipnet,
                ConnectionKind::Fulcrum,
                "tcp://umbrel.local:50001"
            )
            .unwrap(),
            "tcp://umbrel.local:50001"
        );
    }

    #[test]
    fn payout_validation_rechecks_checksum_case_and_type() {
        for address in [
            "bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6q",
            "bitcoincash:Qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a",
            "bitcoincash:ppm2qsznhks23z7629mms6s4cwef74vcwvn0h829pq",
            "bchreg:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a",
        ] {
            let mut cfg = RuntimeConfig {
                payout_address: address.into(),
                ..Default::default()
            };
            assert!(cfg.ensure_mining_supported().is_err(), "{address}");
            assert!(cfg.set_payout(address.into()).is_err(), "{address}");
        }
    }

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
        let saved =
            SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &runtime);
        saved.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("secret-pass"), "{text}");
        assert_config_file_is_owner_only(&path);
        runtime
            .set_node_url("http://user:second-secret@127.0.0.1:8332")
            .unwrap();
        let replaced =
            SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &runtime);
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
        assert!(cfg.set_payout(PAYOUT.replace(":z", ":Z")).is_err());
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
        let saved = SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![0]), &cfg);
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

    #[test]
    fn donation_is_shared_equally_with_odd_remainder_to_original_recipient() {
        assert_eq!(RuntimeConfig::split_donation(0), (0, 0));
        assert_eq!(RuntimeConfig::split_donation(2), (1, 1));
        assert_eq!(RuntimeConfig::split_donation(5), (3, 2));
        let (_, total_donation) = RuntimeConfig::split_reward(4_999_773_813);
        let (original, shrec) = RuntimeConfig::split_donation(total_donation);
        assert_eq!(original + shrec, total_donation);
        assert_eq!(original, shrec);
    }

    #[test]
    fn saved_gpu_choice_reads_old_numbers_and_writes_lists() {
        // Configs and profiles saved before several GPUs could mine.
        let old: SavedConfig = serde_json::from_str(r#"{"backend":"cuda","device":0}"#).unwrap();
        assert_eq!(old.device, Some(SavedDevices::One(0)));
        assert_eq!(old.device_selection(), DeviceSelection::Indices(vec![0]));

        let save = |devices: DeviceSelection| {
            let saved = SavedConfig::from_effective("auto", &devices, &RuntimeConfig::default());
            (serde_json::to_value(&saved).unwrap(), saved)
        };
        let (one, _) = save(DeviceSelection::Indices(vec![3]));
        assert_eq!(one["device"], 3);
        let (pair, saved) = save(DeviceSelection::Indices(vec![0, 2]));
        assert_eq!(pair["device"], serde_json::json!([0, 2]));
        assert!(pair.get("include_integrated").is_none());
        assert_eq!(
            saved.device_selection(),
            DeviceSelection::Indices(vec![0, 2])
        );
        let (every, saved) = save(DeviceSelection::WithIntegrated);
        assert!(every["device"].is_null());
        assert_eq!(every["include_integrated"], true);
        assert_eq!(saved.device_selection(), DeviceSelection::WithIntegrated);
        let (default, saved) = save(DeviceSelection::Default);
        assert!(default["device"].is_null());
        assert_eq!(saved.device_selection(), DeviceSelection::Default);

        let twice: SavedConfig = serde_json::from_str(r#"{"device":[1,1]}"#).unwrap();
        assert!(twice.validate().unwrap_err().contains("twice"));
        let empty: SavedConfig = serde_json::from_str(r#"{"device":[]}"#).unwrap();
        assert!(empty.validate().is_err());
    }
}
