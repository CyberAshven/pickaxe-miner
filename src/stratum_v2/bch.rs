//! Bitcoin Cash constraints that SV2 templates and jobs must respect.

/// Typed constants for BCH-specific Stratum V2 template rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BchTemplateConstraints;

impl BchTemplateConstraints {
    /// Templates must be full and in canonical transaction order (CTOR).
    pub const CTOR_FULL_TEMPLATES: bool = true;
    /// Bitcoin-style segwit / witness commitments are not used on BCH.
    pub const NO_SEGWIT_WITNESS_COMMITMENT: bool = true;
    /// Per-block target comes from ASERT.
    pub const ASERT_TARGET: bool = true;
    /// Payout scripts use CashAddr (or equivalent locking bytecode).
    pub const CASHADDR_PAYOUTS: bool = true;
    /// Template size follows BCH adaptive block size, not a fixed BTC envelope.
    pub const ADAPTIVE_BLOCK_SIZE: bool = true;
}

/// Whether full CTOR templates are required.
pub fn ctor_full_templates_required() -> bool {
    BchTemplateConstraints::CTOR_FULL_TEMPLATES
}

/// Whether segwit/witness commitments must be omitted.
pub fn no_segwit_witness_commitment() -> bool {
    BchTemplateConstraints::NO_SEGWIT_WITNESS_COMMITMENT
}

/// Whether ASERT supplies the per-block target.
pub fn asert_target_required() -> bool {
    BchTemplateConstraints::ASERT_TARGET
}

/// Whether CashAddr payout encoding is required.
pub fn cashaddr_payouts_required() -> bool {
    BchTemplateConstraints::CASHADDR_PAYOUTS
}

/// Whether templates must respect adaptive block size.
pub fn respects_adaptive_block_size() -> bool {
    BchTemplateConstraints::ADAPTIVE_BLOCK_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bch_constraints_are_enabled() {
        assert!(ctor_full_templates_required());
        assert!(no_segwit_witness_commitment());
        assert!(asert_target_required());
        assert!(cashaddr_payouts_required());
        assert!(respects_adaptive_block_size());
    }
}
