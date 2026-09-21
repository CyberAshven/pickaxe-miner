//! Shared network endpoint catalog.

pub(crate) const MAX_SOURCES: usize = 64;

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
    pub(crate) capabilities: Vec<SourceCapability>,
    pub(crate) health: SourceHealth,
    pub(crate) enabled: bool,
    pub(crate) banned: bool,
    pub(crate) last_success_ms: Option<u64>,
    pub(crate) failure_count: u8,
    pub(crate) retry_after_ms: Option<u64>,
    pub(crate) latency_ms: Option<u32>,
}

impl SourceEntry {
    fn capabilities_for(kind: SourceKind) -> Vec<SourceCapability> {
        match kind {
            SourceKind::Fulcrum => vec![
                SourceCapability::PhotonState,
                SourceCapability::ChainHeight,
                SourceCapability::TokenState,
                SourceCapability::TransactionLookup,
                SourceCapability::SubmitTransaction,
            ],
            SourceKind::NativeNode => vec![
                SourceCapability::ChainHeight,
                SourceCapability::TransactionLookup,
                SourceCapability::SubmitTransaction,
                SourceCapability::PolicyCheck,
                SourceCapability::Diagnostics,
            ],
        }
    }

    fn new(kind: SourceKind, endpoint: &str, label: String, provenance: SourceProvenance) -> Self {
        Self {
            kind,
            endpoint: endpoint.trim().to_string(),
            label,
            provenance,
            capabilities: Self::capabilities_for(kind),
            health: SourceHealth::Unknown,
            enabled: true,
            banned: false,
            last_success_ms: None,
            failure_count: 0,
            retry_after_ms: None,
            latency_ms: None,
        }
    }

    pub(crate) fn built_in(kind: SourceKind, endpoint: &str, label: String) -> Self {
        Self::new(kind, endpoint, label, SourceProvenance::BuiltIn)
    }

    pub(crate) fn user(kind: SourceKind, endpoint: &str, label: String) -> Self {
        Self::new(kind, endpoint, label, SourceProvenance::User)
    }

    pub(crate) fn supports(&self, capability: SourceCapability) -> bool {
        self.capabilities.contains(&capability)
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
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceCatalog {
    entries: Vec<SourceEntry>,
}

impl SourceCatalog {
    pub(crate) fn mainnet() -> Self {
        let mut catalog = Self::default();
        for (index, endpoint) in crate::protocol::FULCRUM_WSS_BOOTSTRAP.iter().enumerate() {
            catalog.entries.push(SourceEntry::built_in(
                SourceKind::Fulcrum,
                endpoint,
                format!("Fulcrum bootstrap {}", index + 1),
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
            catalog.add_user(SourceKind::NativeNode, endpoint, "Configured node")?;
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

    pub(crate) fn router(&self) -> SourceRouter<'_> {
        SourceRouter { catalog: self }
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
            .filter(|entry| entry.supports(capability) && entry.available_at(now_ms))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|entry| source_rank(entry));
        candidates
    }
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
        assert!(!entry.supports(SourceCapability::PhotonState));
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
        catalog.entries[0].health = SourceHealth::Unhealthy;
        catalog.entries[0].retry_after_ms = Some(500);
        catalog.entries[1].health = SourceHealth::Healthy;
        let router = catalog.router();
        let selected = router.select(SourceCapability::ChainHeight, 100).unwrap();
        assert_eq!(selected.endpoint, "b");
    }
}
