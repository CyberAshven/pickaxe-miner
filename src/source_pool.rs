//! Shared network endpoint catalog.

pub(crate) const MAX_SOURCES: usize = 64;
pub(crate) const AUTO_PROBE_LIMIT: usize = 2;
pub(crate) const DEFAULT_CAPABILITY_TTL_MS: u64 = 5 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SourceKind {
    Fulcrum,
    NativeNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceProvenance {
    BuiltIn,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceTransport {
    Wss,
    ElectrumTls,
    NodeHttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceOrigin {
    Pickaxe,
    ElectronCash,
    Selene,
    ElectronCashAndSelene,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SourceCapability {
    PhotonState,
    ChainHeight,
    TokenState,
    TransactionLookup,
    SubmitTransaction,
    PolicyCheck,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapabilityEvidence {
    pub(crate) capability: SourceCapability,
    pub(crate) verified_at_ms: u64,
    pub(crate) expires_at_ms: u64,
}

impl CapabilityEvidence {
    fn is_current(self, now_ms: u64) -> bool {
        now_ms <= self.expires_at_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SourceHealth {
    Healthy,
    Unhealthy,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceEntry {
    pub(crate) kind: SourceKind,
    pub(crate) endpoint: String,
    pub(crate) label: String,
    pub(crate) provenance: SourceProvenance,
    pub(crate) transport: SourceTransport,
    pub(crate) origin: SourceOrigin,
    pub(crate) capabilities: Vec<CapabilityEvidence>,
    pub(crate) health: SourceHealth,
    pub(crate) enabled: bool,
    pub(crate) banned: bool,
    pub(crate) last_success_ms: Option<u64>,
    pub(crate) failure_count: u8,
    pub(crate) retry_after_ms: Option<u64>,
    pub(crate) latency_ms: Option<u32>,
    native_photon_proof: Option<super::NativePhotonEquivalenceProof>,
}

impl SourceEntry {
    fn new(
        kind: SourceKind,
        endpoint: &str,
        label: String,
        provenance: SourceProvenance,
        transport: SourceTransport,
        origin: SourceOrigin,
    ) -> Self {
        Self {
            kind,
            endpoint: endpoint.trim().to_string(),
            label,
            provenance,
            transport,
            origin,
            capabilities: Vec::new(),
            health: SourceHealth::Unknown,
            enabled: true,
            banned: false,
            last_success_ms: None,
            failure_count: 0,
            retry_after_ms: None,
            latency_ms: None,
            native_photon_proof: None,
        }
    }

    pub(crate) fn built_in(kind: SourceKind, endpoint: &str, label: String) -> Self {
        let transport = match kind {
            SourceKind::Fulcrum => SourceTransport::Wss,
            SourceKind::NativeNode => SourceTransport::NodeHttp,
        };
        Self::new(
            kind,
            endpoint,
            label,
            SourceProvenance::BuiltIn,
            transport,
            SourceOrigin::Pickaxe,
        )
    }

    fn published(
        kind: SourceKind,
        endpoint: &str,
        label: String,
        transport: SourceTransport,
        origin: SourceOrigin,
    ) -> Self {
        Self::new(
            kind,
            endpoint,
            label,
            SourceProvenance::BuiltIn,
            transport,
            origin,
        )
    }

    pub(crate) fn user(kind: SourceKind, endpoint: &str, label: String) -> Self {
        let transport = match kind {
            SourceKind::Fulcrum => SourceTransport::Wss,
            SourceKind::NativeNode => SourceTransport::NodeHttp,
        };
        Self::new(
            kind,
            endpoint,
            label,
            SourceProvenance::User,
            transport,
            SourceOrigin::User,
        )
    }

    pub(crate) fn supports_at(&self, capability: SourceCapability, now_ms: u64) -> bool {
        self.capabilities
            .iter()
            .any(|evidence| evidence.capability == capability && evidence.is_current(now_ms))
    }

    pub(crate) fn verify_capability(
        &mut self,
        capability: SourceCapability,
        now_ms: u64,
        ttl_ms: u64,
    ) {
        let evidence = CapabilityEvidence {
            capability,
            verified_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        if let Some(existing) = self
            .capabilities
            .iter_mut()
            .find(|existing| existing.capability == capability)
        {
            *existing = evidence;
        } else {
            self.capabilities.push(evidence);
        }
    }

    fn revoke_capability(&mut self, capability: SourceCapability) {
        self.capabilities
            .retain(|evidence| evidence.capability != capability);
        if capability == SourceCapability::PhotonState {
            self.native_photon_proof = None;
        }
    }

    pub(crate) fn record_success(&mut self, now_ms: u64, latency_ms: u32) {
        self.health = SourceHealth::Healthy;
        self.last_success_ms = Some(now_ms);
        self.failure_count = 0;
        self.retry_after_ms = None;
        self.latency_ms = Some(latency_ms);
    }

    pub(crate) fn record_failure(&mut self, now_ms: u64) {
        self.health = SourceHealth::Unhealthy;
        self.failure_count = self.failure_count.saturating_add(1).min(16);
        let shift = u32::from(self.failure_count.saturating_sub(1)).min(31);
        let delay = 400u64.saturating_mul(1u64 << shift).min(8_000);
        self.retry_after_ms = Some(now_ms.saturating_add(delay));
    }

    fn available_at(&self, now_ms: u64) -> bool {
        self.enabled
            && !self.banned
            && self
                .retry_after_ms
                .is_none_or(|retry_after| retry_after <= now_ms)
    }

    fn supported_by_current_client(&self) -> bool {
        matches!(
            (self.kind, self.transport),
            (SourceKind::Fulcrum, SourceTransport::Wss)
                | (SourceKind::NativeNode, SourceTransport::NodeHttp)
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceCatalog {
    entries: Vec<SourceEntry>,
}

impl SourceCatalog {
    pub(crate) fn mainnet() -> Self {
        let mut catalog = Self::default();
        for endpoint in crate::protocol::FULCRUM_WSS_BOOTSTRAP {
            let host = endpoint_host(endpoint).unwrap_or(endpoint);
            let in_electron_cash = crate::protocol::ELECTRON_CASH_TLS_BOOTSTRAP
                .iter()
                .any(|(candidate, _)| candidate.eq_ignore_ascii_case(host));
            let in_selene = !matches!(
                *endpoint,
                "wss://electrum.imaginary.cash:50004"
                    | "wss://electroncash.dk:50004"
                    | "wss://fulcrum.greyh.at:50004"
            );
            let origin = match (in_electron_cash, in_selene) {
                (true, true) => SourceOrigin::ElectronCashAndSelene,
                (true, false) => SourceOrigin::ElectronCash,
                (false, true) => SourceOrigin::Selene,
                (false, false) => SourceOrigin::Pickaxe,
            };
            catalog.entries.push(SourceEntry::published(
                SourceKind::Fulcrum,
                endpoint,
                format!("{host} WSS"),
                SourceTransport::Wss,
                origin,
            ));
        }
        for (host, port) in crate::protocol::ELECTRON_CASH_TLS_BOOTSTRAP {
            if catalog.entries.iter().any(|entry| {
                entry.kind == SourceKind::Fulcrum
                    && endpoint_host(&entry.endpoint)
                        .is_some_and(|known| known.eq_ignore_ascii_case(host))
            }) {
                continue;
            }
            catalog.entries.push(SourceEntry::published(
                SourceKind::Fulcrum,
                &format!("tls://{host}:{port}"),
                format!("{host} Electron Cash TLS"),
                SourceTransport::ElectrumTls,
                SourceOrigin::ElectronCash,
            ));
        }
        for (index, endpoint) in crate::protocol::NODE_RPC_BOOTSTRAP.iter().enumerate() {
            catalog.entries.push(SourceEntry::built_in(
                SourceKind::NativeNode,
                endpoint,
                format!("BCH node bootstrap {}", index + 1),
            ));
        }
        debug_assert!(catalog.entries.len() <= MAX_SOURCES);
        catalog
    }

    pub(crate) fn configured(cfg: &crate::config::RuntimeConfig) -> Result<Self, String> {
        let mut catalog = Self::mainnet();
        if let Some(endpoint) = cfg.fulcrum_url.as_deref() {
            catalog.add_user(SourceKind::Fulcrum, endpoint, "Configured Fulcrum")?;
        }
        if let Some(endpoint) = cfg.node_url.as_deref() {
            catalog.add_user(
                SourceKind::NativeNode,
                &crate::node::redact_url(endpoint),
                "Configured node",
            )?;
        }
        Ok(catalog)
    }

    pub(crate) fn entries(&self) -> &[SourceEntry] {
        &self.entries
    }

    pub(crate) fn add_user(
        &mut self,
        kind: SourceKind,
        endpoint: &str,
        label: impl Into<String>,
    ) -> Result<(), String> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            return Err("source endpoint cannot be empty".into());
        }
        let label = label.into();
        if let Some(existing) = self.entry_mut(kind, endpoint) {
            if existing.provenance == SourceProvenance::BuiltIn {
                return Err("source already exists in the built-in catalog".into());
            }
            existing.label = label;
            existing.enabled = true;
            return Ok(());
        }
        if self.entries.len() >= MAX_SOURCES {
            return Err(format!(
                "source catalog is limited to {MAX_SOURCES} entries"
            ));
        }
        self.entries.push(SourceEntry::user(kind, endpoint, label));
        Ok(())
    }

    pub(crate) fn remove(&mut self, kind: SourceKind, endpoint: &str) -> Result<(), String> {
        let Some(index) = self.find_index(kind, endpoint) else {
            return Err("source not found".into());
        };
        if self.entries[index].provenance == SourceProvenance::BuiltIn {
            return Err("built-in sources cannot be deleted".into());
        }
        self.entries.remove(index);
        Ok(())
    }

    pub(crate) fn set_enabled(
        &mut self,
        kind: SourceKind,
        endpoint: &str,
        enabled: bool,
    ) -> Result<(), String> {
        let entry = self
            .entry_mut(kind, endpoint)
            .ok_or_else(|| "source not found".to_string())?;
        entry.enabled = enabled;
        Ok(())
    }

    pub(crate) fn set_banned(
        &mut self,
        kind: SourceKind,
        endpoint: &str,
        banned: bool,
    ) -> Result<(), String> {
        let entry = self
            .entry_mut(kind, endpoint)
            .ok_or_else(|| "source not found".to_string())?;
        entry.banned = banned;
        Ok(())
    }

    pub(crate) fn record_success(
        &mut self,
        kind: SourceKind,
        endpoint: &str,
        now_ms: u64,
        latency_ms: u32,
    ) -> Result<(), String> {
        let entry = self
            .entry_mut(kind, endpoint)
            .ok_or_else(|| "source not found".to_string())?;
        entry.record_success(now_ms, latency_ms);
        Ok(())
    }

    pub(crate) fn verify_capability(
        &mut self,
        kind: SourceKind,
        endpoint: &str,
        capability: SourceCapability,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<(), String> {
        if kind == SourceKind::NativeNode && capability == SourceCapability::PhotonState {
            return Err(
                "native-node PHOTON capability requires a successful canonical equivalence proof"
                    .into(),
            );
        }
        let entry = self
            .entry_mut(kind, endpoint)
            .ok_or_else(|| "source not found".to_string())?;
        entry.verify_capability(capability, now_ms, ttl_ms);
        Ok(())
    }

    pub(crate) fn verify_native_photon_capability(
        &mut self,
        proof: &super::NativePhotonEquivalenceProof,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<(), String> {
        if ttl_ms == 0 {
            return Err("native-node PHOTON capability proof TTL must be non-zero".into());
        }
        let entry = self
            .entry_mut(SourceKind::NativeNode, &proof.endpoint)
            .ok_or_else(|| {
                "native PHOTON proof endpoint is not present in the source catalog".to_string()
            })?;
        entry.verify_capability(SourceCapability::PhotonState, now_ms, ttl_ms);
        entry.native_photon_proof = Some(proof.clone());
        Ok(())
    }

    pub(crate) fn revoke_native_photon_capability(&mut self, endpoint: &str) -> Result<(), String> {
        let entry = self
            .entry_mut(SourceKind::NativeNode, endpoint)
            .ok_or_else(|| "source not found".to_string())?;
        entry.revoke_capability(SourceCapability::PhotonState);
        Ok(())
    }

    pub(crate) fn native_photon_proof_at(
        &self,
        endpoint: &str,
        now_ms: u64,
    ) -> Option<&super::NativePhotonEquivalenceProof> {
        self.entries
            .iter()
            .find(|entry| {
                entry.kind == SourceKind::NativeNode
                    && entry.endpoint == endpoint.trim()
                    && entry.supports_at(SourceCapability::PhotonState, now_ms)
            })
            .and_then(|entry| entry.native_photon_proof.as_ref())
    }

    pub(crate) fn record_failure(
        &mut self,
        kind: SourceKind,
        endpoint: &str,
        now_ms: u64,
    ) -> Result<(), String> {
        let entry = self
            .entry_mut(kind, endpoint)
            .ok_or_else(|| "source not found".to_string())?;
        entry.record_failure(now_ms);
        Ok(())
    }

    pub(crate) fn supports_at(
        &self,
        kind: SourceKind,
        endpoint: &str,
        capability: SourceCapability,
        now_ms: u64,
    ) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.kind == kind && entry.endpoint == endpoint.trim())
            .is_some_and(|entry| {
                entry.supports_at(capability, now_ms) && entry.available_at(now_ms)
            })
    }

    pub(crate) fn available_at(&self, kind: SourceKind, endpoint: &str, now_ms: u64) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.kind == kind && entry.endpoint == endpoint.trim())
            .is_some_and(|entry| entry.available_at(now_ms))
    }

    pub(crate) fn router(&self) -> SourceRouter<'_> {
        SourceRouter { catalog: self }
    }

    pub(crate) fn probe_candidates(
        &self,
        kind: SourceKind,
        now_ms: u64,
        limit: usize,
        rotation_key: u64,
    ) -> Vec<&SourceEntry> {
        if limit == 0 {
            return Vec::new();
        }

        let mut user = self
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == kind
                    && entry.provenance == SourceProvenance::User
                    && entry.supported_by_current_client()
                    && entry.available_at(now_ms)
            })
            .collect::<Vec<_>>();
        let mut built_in = self
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == kind
                    && entry.provenance == SourceProvenance::BuiltIn
                    && entry.supported_by_current_client()
                    && entry.available_at(now_ms)
            })
            .collect::<Vec<_>>();
        user.sort_by_key(|entry| source_rank(entry));
        built_in.sort_by_key(|entry| source_rank(entry));
        if !built_in.is_empty() {
            let offset = (rotation_key as usize) % built_in.len();
            built_in.rotate_left(offset);
        }
        user.into_iter().chain(built_in).take(limit).collect()
    }

    pub(crate) fn reconcile_builtins(&mut self, refreshed: Vec<SourceEntry>) -> Result<(), String> {
        let mut next = self
            .entries
            .iter()
            .filter(|entry| entry.provenance == SourceProvenance::User)
            .cloned()
            .collect::<Vec<_>>();
        for mut candidate in refreshed {
            if candidate.provenance != SourceProvenance::BuiltIn {
                return Err("catalog refresh may only contain built-in sources".into());
            }
            if let Some(existing) = self.entries.iter().find(|entry| {
                entry.provenance == SourceProvenance::BuiltIn
                    && entry.kind == candidate.kind
                    && entry.endpoint == candidate.endpoint
            }) {
                preserve_runtime_state(&mut candidate, existing);
            }
            if next.len() >= MAX_SOURCES {
                return Err(format!(
                    "source catalog is limited to {MAX_SOURCES} entries"
                ));
            }
            next.push(candidate);
        }
        self.entries = next;
        Ok(())
    }

    fn find_index(&self, kind: SourceKind, endpoint: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.kind == kind && entry.endpoint == endpoint.trim())
    }

    fn entry_mut(&mut self, kind: SourceKind, endpoint: &str) -> Option<&mut SourceEntry> {
        let index = self.find_index(kind, endpoint)?;
        self.entries.get_mut(index)
    }
}

pub(crate) struct SourceRouter<'a> {
    catalog: &'a SourceCatalog,
}

impl SourceRouter<'_> {
    pub(crate) fn select(&self, capability: SourceCapability, now_ms: u64) -> Option<&SourceEntry> {
        self.candidates(capability, now_ms).into_iter().next()
    }

    pub(crate) fn candidates(
        &self,
        capability: SourceCapability,
        now_ms: u64,
    ) -> Vec<&SourceEntry> {
        let mut candidates = self
            .catalog
            .entries
            .iter()
            .filter(|entry| {
                entry.health == SourceHealth::Healthy
                    && entry.supported_by_current_client()
                    && entry.supports_at(capability, now_ms)
                    && entry.available_at(now_ms)
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|entry| source_rank(entry));
        candidates
    }
}

fn endpoint_host(endpoint: &str) -> Option<&str> {
    let after_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let authority = after_scheme.split('/').next()?;
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    (!host.is_empty()).then_some(host)
}

fn source_rank(entry: &SourceEntry) -> (u8, u8, u8, u32) {
    let health = match entry.health {
        SourceHealth::Healthy => 0,
        SourceHealth::Unknown => 1,
        SourceHealth::Unhealthy => 2,
    };
    let provenance = match entry.provenance {
        SourceProvenance::User => 0,
        SourceProvenance::BuiltIn => 1,
    };
    (
        health,
        provenance,
        entry.failure_count,
        entry.latency_ms.unwrap_or(u32::MAX),
    )
}

fn preserve_runtime_state(candidate: &mut SourceEntry, existing: &SourceEntry) {
    candidate.enabled = existing.enabled;
    candidate.banned = existing.banned;
    candidate.health = existing.health;
    candidate.last_success_ms = existing.last_success_ms;
    candidate.failure_count = existing.failure_count;
    candidate.retry_after_ms = existing.retry_after_ms;
    candidate.latency_ms = existing.latency_ms;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_fulcrum(catalog: &mut SourceCatalog, endpoint: &str) {
        catalog
            .add_user(SourceKind::Fulcrum, endpoint, endpoint)
            .unwrap();
    }

    fn add_node(catalog: &mut SourceCatalog, endpoint: &str) {
        catalog
            .add_user(SourceKind::NativeNode, endpoint, endpoint)
            .unwrap();
    }

    #[test]
    fn mixed_pool_routes_only_to_proven_capabilities() {
        let mut catalog = SourceCatalog::default();
        add_node(&mut catalog, "node-a");
        add_fulcrum(&mut catalog, "fulcrum-a");
        catalog
            .record_success(SourceKind::NativeNode, "node-a", 1_000, 1)
            .unwrap();
        catalog
            .record_success(SourceKind::Fulcrum, "fulcrum-a", 1_000, 50)
            .unwrap();
        catalog
            .verify_capability(
                SourceKind::Fulcrum,
                "fulcrum-a",
                SourceCapability::PhotonState,
                1_000,
                DEFAULT_CAPABILITY_TTL_MS,
            )
            .unwrap();

        let router = catalog.router();
        let selected = router.select(SourceCapability::PhotonState, 1_000).unwrap();
        assert_eq!(selected.kind, SourceKind::Fulcrum);
        assert_eq!(selected.endpoint, "fulcrum-a");
    }

    #[test]
    fn configured_catalog_can_contain_both_source_kinds() {
        let cfg = crate::config::RuntimeConfig {
            node_url: Some("http://node.invalid".into()),
            ..crate::config::RuntimeConfig::default()
        };
        let catalog = SourceCatalog::configured(&cfg).unwrap();
        assert!(catalog
            .entries()
            .iter()
            .any(|entry| entry.kind == SourceKind::Fulcrum));
        assert!(catalog
            .entries()
            .iter()
            .any(|entry| entry.kind == SourceKind::NativeNode));
    }

    #[test]
    fn native_node_does_not_claim_photon_state() {
        let entry = SourceEntry::user(SourceKind::NativeNode, "node-a", "node".into());
        assert!(!entry.supports_at(SourceCapability::PhotonState, 0));
    }

    #[test]
    fn native_photon_capability_requires_typed_equivalence_proof_and_expires() {
        let mut catalog = SourceCatalog::default();
        add_node(&mut catalog, "node-a");
        catalog
            .record_success(SourceKind::NativeNode, "node-a", 1_000, 3)
            .unwrap();

        let direct = catalog.verify_capability(
            SourceKind::NativeNode,
            "node-a",
            SourceCapability::PhotonState,
            1_000,
            100,
        );
        assert!(direct.is_err());
        assert!(!catalog.supports_at(
            SourceKind::NativeNode,
            "node-a",
            SourceCapability::PhotonState,
            1_000
        ));

        let proof = super::super::NativePhotonEquivalenceProof {
            endpoint: "node-a".into(),
            tip_hash: "11".repeat(32),
            proven_work: Default::default(),
        };
        catalog
            .verify_native_photon_capability(&proof, 1_000, 100)
            .unwrap();
        assert!(catalog.supports_at(
            SourceKind::NativeNode,
            "node-a",
            SourceCapability::PhotonState,
            1_100
        ));
        assert!(catalog.native_photon_proof_at("node-a", 1_100).is_some());
        assert!(!catalog.supports_at(
            SourceKind::NativeNode,
            "node-a",
            SourceCapability::PhotonState,
            1_101
        ));
        assert!(catalog.native_photon_proof_at("node-a", 1_101).is_none());

        catalog
            .verify_native_photon_capability(&proof, 2_000, 100)
            .unwrap();
        catalog.revoke_native_photon_capability("node-a").unwrap();
        assert!(catalog.native_photon_proof_at("node-a", 2_000).is_none());
        assert!(!catalog.supports_at(
            SourceKind::NativeNode,
            "node-a",
            SourceCapability::PhotonState,
            2_000
        ));
    }

    #[test]
    fn bootstrap_presence_does_not_imply_verified_capability() {
        let catalog = SourceCatalog::mainnet();
        assert!(catalog
            .entries()
            .iter()
            .all(|entry| entry.capabilities.is_empty()));
        assert!(catalog
            .router()
            .candidates(SourceCapability::PhotonState, 0)
            .is_empty());
    }

    #[test]
    fn health_without_verified_capability_is_not_routable() {
        let mut catalog = SourceCatalog::default();
        add_fulcrum(&mut catalog, "fulcrum-a");
        catalog
            .record_success(SourceKind::Fulcrum, "fulcrum-a", 1_000, 5)
            .unwrap();
        assert!(catalog
            .router()
            .select(SourceCapability::PhotonState, 1_000)
            .is_none());

        catalog
            .verify_capability(
                SourceKind::Fulcrum,
                "fulcrum-a",
                SourceCapability::PhotonState,
                1_000,
                DEFAULT_CAPABILITY_TTL_MS,
            )
            .unwrap();
        assert_eq!(
            catalog
                .router()
                .select(SourceCapability::PhotonState, 1_000)
                .unwrap()
                .endpoint,
            "fulcrum-a"
        );
    }

    #[test]
    fn auto_probe_is_bounded_and_rotates_builtins() {
        let mut catalog = SourceCatalog::default();
        for endpoint in ["a", "b", "c", "d"] {
            catalog.entries.push(SourceEntry::built_in(
                SourceKind::Fulcrum,
                endpoint,
                endpoint.into(),
            ));
        }
        let first = catalog.probe_candidates(SourceKind::Fulcrum, 0, 2, 0);
        let rotated = catalog.probe_candidates(SourceKind::Fulcrum, 0, 2, 1);
        assert_eq!(first.len(), 2);
        assert_eq!(rotated.len(), 2);
        assert_ne!(first[0].endpoint, rotated[0].endpoint);
    }

    #[test]
    fn mainnet_auto_probe_does_not_cover_entire_bootstrap_catalog() {
        let catalog = SourceCatalog::mainnet();
        let all_fulcrum = catalog
            .entries()
            .iter()
            .filter(|entry| entry.kind == SourceKind::Fulcrum)
            .count();
        let probes = catalog.probe_candidates(SourceKind::Fulcrum, 0, AUTO_PROBE_LIMIT, 0);
        assert!(probes.len() <= AUTO_PROBE_LIMIT);
        assert!(probes.len() < all_fulcrum);
    }

    #[test]
    fn published_mainnet_catalog_deduplicates_hosts_and_keeps_tls_only_sources() {
        let catalog = SourceCatalog::mainnet();
        let mut hosts = catalog
            .entries()
            .iter()
            .filter(|entry| entry.kind == SourceKind::Fulcrum)
            .map(|entry| endpoint_host(&entry.endpoint).unwrap().to_ascii_lowercase())
            .collect::<Vec<_>>();
        let before = hosts.len();
        hosts.sort();
        hosts.dedup();
        assert_eq!(hosts.len(), before);

        let tls_only = catalog
            .entries()
            .iter()
            .find(|entry| entry.endpoint == "tls://bch.crypto.mldlabs.com:50002")
            .expect("Electron Cash TLS-only server remains cataloged");
        assert_eq!(tls_only.transport, SourceTransport::ElectrumTls);
        assert_eq!(tls_only.origin, SourceOrigin::ElectronCash);

        let wss = catalog
            .entries()
            .iter()
            .find(|entry| entry.endpoint == "wss://cashnode.bch.ninja:50004")
            .expect("Selene WSS server is cataloged");
        assert_eq!(wss.transport, SourceTransport::Wss);
        assert_eq!(wss.origin, SourceOrigin::ElectronCashAndSelene);
    }

    #[test]
    fn tls_only_published_sources_are_never_wss_probe_candidates() {
        let catalog = SourceCatalog::mainnet();
        let candidates = catalog.probe_candidates(SourceKind::Fulcrum, 0, MAX_SOURCES, 0);
        assert!(candidates
            .iter()
            .all(|entry| entry.transport == SourceTransport::Wss));
        assert!(candidates
            .iter()
            .all(|entry| !entry.endpoint.starts_with("tls://")));
    }

    #[test]
    fn disabled_source_is_not_available() {
        let mut entry = SourceEntry::user(SourceKind::Fulcrum, "a", "a".into());
        entry.enabled = false;
        assert!(!entry.available_at(0));
    }

    #[test]
    fn policy_blocked_entry_is_not_available() {
        let mut entry = SourceEntry::user(SourceKind::Fulcrum, "a", "a".into());
        entry.banned = true;
        assert!(!entry.available_at(0));
    }

    #[test]
    fn router_excludes_disabled_and_policy_blocked_entries() {
        let mut catalog = SourceCatalog::default();
        add_fulcrum(&mut catalog, "disabled");
        add_fulcrum(&mut catalog, "blocked");
        add_fulcrum(&mut catalog, "eligible");
        for endpoint in ["disabled", "blocked", "eligible"] {
            catalog
                .record_success(SourceKind::Fulcrum, endpoint, 0, 1)
                .unwrap();
            catalog
                .verify_capability(
                    SourceKind::Fulcrum,
                    endpoint,
                    SourceCapability::ChainHeight,
                    0,
                    DEFAULT_CAPABILITY_TTL_MS,
                )
                .unwrap();
        }
        catalog
            .set_enabled(SourceKind::Fulcrum, "disabled", false)
            .unwrap();
        catalog
            .set_banned(SourceKind::Fulcrum, "blocked", true)
            .unwrap();
        let router = catalog.router();
        let selected = router.select(SourceCapability::ChainHeight, 0).unwrap();
        assert_eq!(selected.endpoint, "eligible");
    }

    #[test]
    fn built_in_cannot_be_removed_but_user_entry_can() {
        let mut catalog = SourceCatalog::default();
        catalog.entries.push(SourceEntry::built_in(
            SourceKind::Fulcrum,
            "built-in",
            "built-in".into(),
        ));
        add_fulcrum(&mut catalog, "user");
        assert!(catalog.remove(SourceKind::Fulcrum, "built-in").is_err());
        catalog.remove(SourceKind::Fulcrum, "user").unwrap();
        assert_eq!(catalog.entries().len(), 1);
    }

    #[test]
    fn catalog_refresh_preserves_existing_policy() {
        let mut catalog = SourceCatalog::default();
        catalog.entries.push(SourceEntry::built_in(
            SourceKind::Fulcrum,
            "built-in",
            "old".into(),
        ));
        catalog.entries[0].banned = true;
        let replacement = SourceEntry::built_in(SourceKind::Fulcrum, "built-in", "new".into());
        catalog.reconcile_builtins(vec![replacement]).unwrap();
        assert!(catalog.entries()[0].banned);
        assert_eq!(catalog.entries()[0].label, "new");
    }

    #[test]
    fn retry_delay_and_failure_counter_are_bounded() {
        let mut entry = SourceEntry::user(SourceKind::Fulcrum, "a", "a".into());
        for now_ms in 0..32 {
            entry.record_failure(now_ms);
        }
        assert_eq!(entry.failure_count, 16);
        assert!(entry.retry_after_ms.unwrap() <= 31 + 8_000);
        entry.record_success(10_000, 7);
        assert_eq!(entry.failure_count, 0);
        assert_eq!(entry.retry_after_ms, None);
        assert_eq!(entry.latency_ms, Some(7));
    }

    #[test]
    fn catalog_size_is_bounded() {
        let mut catalog = SourceCatalog::default();
        for index in 0..MAX_SOURCES {
            add_fulcrum(&mut catalog, &format!("entry-{index}"));
        }
        assert_eq!(catalog.entries().len(), MAX_SOURCES);
        assert!(catalog
            .add_user(SourceKind::Fulcrum, "overflow", "overflow")
            .is_err());
    }

    #[test]
    fn router_prefers_healthy_available_candidate() {
        let mut catalog = SourceCatalog::default();
        add_fulcrum(&mut catalog, "a");
        add_fulcrum(&mut catalog, "b");
        catalog
            .verify_capability(
                SourceKind::Fulcrum,
                "a",
                SourceCapability::ChainHeight,
                0,
                DEFAULT_CAPABILITY_TTL_MS,
            )
            .unwrap();
        catalog
            .verify_capability(
                SourceKind::Fulcrum,
                "b",
                SourceCapability::ChainHeight,
                0,
                DEFAULT_CAPABILITY_TTL_MS,
            )
            .unwrap();
        catalog.entries[0].health = SourceHealth::Unhealthy;
        catalog.entries[0].retry_after_ms = Some(500);
        catalog.entries[1].health = SourceHealth::Healthy;
        let router = catalog.router();
        let selected = router.select(SourceCapability::ChainHeight, 100).unwrap();
        assert_eq!(selected.endpoint, "b");
    }

    #[test]
    fn verified_capability_expires_and_must_be_refreshed() {
        let mut catalog = SourceCatalog::default();
        add_fulcrum(&mut catalog, "a");
        catalog
            .record_success(SourceKind::Fulcrum, "a", 1_000, 5)
            .unwrap();
        catalog
            .verify_capability(
                SourceKind::Fulcrum,
                "a",
                SourceCapability::PhotonState,
                1_000,
                100,
            )
            .unwrap();

        assert!(catalog
            .router()
            .select(SourceCapability::PhotonState, 1_100)
            .is_some());
        assert!(catalog
            .router()
            .select(SourceCapability::PhotonState, 1_101)
            .is_none());
    }
}
