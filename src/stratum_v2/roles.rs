//! Roles Pickaxe plays on the BCH Stratum V2 path.

use std::fmt;

/// Stratum V2 roles Pickaxe will implement for ASIC-facing mining.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Pull full templates from the miner's node (BCHN or Knuth).
    TemplateProviderClient,
    /// Serve SV2 mining channels to devices and check shares.
    MiningServer,
    /// Translate SV1 firmware onto the same mining server.
    Sv1Translator,
    /// #### PR #42: serve this node's templates to SV2 pools, Job
    /// Declaration clients and P2Pool (Template Distribution).
    TemplateProviderServer,
}

impl Role {
    /// Canonical snake-ish label used in status output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TemplateProviderClient => "template_provider_client",
            Self::MiningServer => "mining_server",
            Self::Sv1Translator => "sv1_translator",
            Self::TemplateProviderServer => "template_provider_server",
        }
    }

    /// Every role in planned order.
    pub fn all() -> &'static [Role] {
        &[
            Self::TemplateProviderClient,
            Self::MiningServer,
            Self::Sv1Translator,
            Self::TemplateProviderServer,
        ]
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_as_str() {
        for role in Role::all() {
            assert_eq!(role.to_string(), role.as_str());
        }
    }

    #[test]
    fn planned_roles_are_distinct() {
        let labels: Vec<_> = Role::all().iter().map(|r| r.as_str()).collect();
        assert_eq!(labels.len(), 4);
        assert!(labels.contains(&"template_provider_client"));
        assert!(labels.contains(&"mining_server"));
        assert!(labels.contains(&"sv1_translator"));
        assert!(labels.contains(&"template_provider_server"));
    }
}
