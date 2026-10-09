//! Implementation status, distinct from live node or ASIC validation.

use super::bch::BchTemplateConstraints;
use super::reference;
use super::roles::Role;

/// Snapshot of the Stratum V2 build capabilities.
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
        "pickaxe stratum-v2 implementation in progress\n\
         feature: {feature} (reference crates: {crates})\n\
         roles: {roles}\n\
         bch: CTOR={ctor} no_segwit={no_segwit} ASERT={asert} CashAddr={cashaddr} adaptive_size={adaptive}\n\
         first_network: {network}\n\
         design: {design}\n\
         available with feature: check-node; serve (Noise, standard/extended channels, full BCH templates, optional SV1 adapter, optional template server for SV2 pools with --tp-listen, Coinbase-only Job Declaration at a public pool with --accept-job-declaration)\n\
         validation: local TCP/CPU experiments; live Chipnet and ASIC validation pending\n\
         merge mining: commitment v1 (draft), both cases in one coinbase; no token registered (Chipnet test token for tests)\n\
         pending: physical ASIC validation, vardiff/device rates, Template Distribution client, Job Declaration client and Full-Template, distributed rigs and pool routing\n\
         evidence: docs/implementation-status.md\n",
        feature = status.feature,
        crates = if status.reference_crates_linked {
            "linked"
        } else {
            "not included in this build"
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
    fn status_report_distinguishes_local_evidence_from_live_validation() {
        let report = status_report();
        assert!(report.contains("stratum-v2"));
        assert!(report.contains("chipnet"));
        assert!(report.contains("template_provider_client"));
        assert!(report.contains("mining_server"));
        assert!(report.contains("docs/stratum-v2.md"));
        if cfg!(feature = "stratum-v2") {
            assert!(report.contains("linked"));
            assert!(!report.contains("not included in this build"));
        } else {
            assert!(report.contains("not included in this build"));
        }
    }

    #[test]
    fn default_status_lists_five_roles() {
        assert_eq!(StratumV2Status::default().roles.len(), 5);
        assert!(status_report().contains("template_provider_server"));
        assert!(status_report().contains("job_declarator_server"));
    }

    #[test]
    fn reference_crates_linked_matches_feature() {
        assert_eq!(
            StratumV2Status::default().reference_crates_linked,
            cfg!(feature = "stratum-v2")
        );
    }
}
