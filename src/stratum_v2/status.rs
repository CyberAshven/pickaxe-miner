//! Scaffold status report for CLI and tests (no live SV2 session yet).

use super::bch::BchTemplateConstraints;
use super::reference;
use super::roles::Role;

/// Snapshot of the Stratum V2 scaffold state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StratumV2Status {
    /// Feature name reserved for reference crates.
    pub feature: &'static str,
    /// Whether reference crates are linked in this build.
    pub reference_crates_linked: bool,
    /// Planned roles.
    pub roles: &'static [Role],
    /// Design-doc relative path.
    pub design_doc: &'static str,
    /// Network intended for first live tests.
    pub first_network: &'static str,
}

impl Default for StratumV2Status {
    fn default() -> Self {
        Self {
            feature: "stratum-v2",
            reference_crates_linked: reference::linked(),
            roles: Role::all(),
            design_doc: "docs/stratum-v2.md",
            first_network: "chipnet",
        }
    }
}

/// Human-readable status for `pickaxe stratum-v2 status`.
pub fn status_report() -> String {
    let status = StratumV2Status::default();
    let roles = status
        .roles
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "pickaxe stratum-v2 scaffold\n\
         feature: {feature} (reference crates: {crates})\n\
         roles: {roles}\n\
         bch: CTOR={ctor} no_segwit={no_segwit} ASERT={asert} CashAddr={cashaddr} adaptive_size={adaptive}\n\
         first_network: {network}\n\
         design: {design}\n\
         live: Noise handshake, share validation, dashboard devices — out of scope for this scaffold\n",
        feature = status.feature,
        crates = if status.reference_crates_linked {
            "linked"
        } else {
            "not linked yet"
        },
        roles = roles,
        ctor = BchTemplateConstraints::CTOR_FULL_TEMPLATES,
        no_segwit = BchTemplateConstraints::NO_SEGWIT_WITNESS_COMMITMENT,
        asert = BchTemplateConstraints::ASERT_TARGET,
        cashaddr = BchTemplateConstraints::CASHADDR_PAYOUTS,
        adaptive = BchTemplateConstraints::ADAPTIVE_BLOCK_SIZE,
        network = status.first_network,
        design = status.design_doc,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_report_mentions_scaffold_and_chipnet() {
        let report = status_report();
        assert!(report.contains("stratum-v2"));
        assert!(report.contains("chipnet"));
        assert!(report.contains("template_provider_client"));
        assert!(report.contains("mining_server"));
        assert!(report.contains("docs/stratum-v2.md"));
        if cfg!(feature = "stratum-v2") {
            assert!(report.contains("linked"));
            assert!(!report.contains("not linked yet"));
        } else {
            assert!(report.contains("not linked yet"));
        }
    }

    #[test]
    fn default_status_lists_three_roles() {
        assert_eq!(StratumV2Status::default().roles.len(), 3);
    }

    #[test]
    fn reference_crates_linked_matches_feature() {
        assert_eq!(
            StratumV2Status::default().reference_crates_linked,
            cfg!(feature = "stratum-v2")
        );
    }
}
