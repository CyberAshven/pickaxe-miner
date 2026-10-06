//! BCH Stratum V2 scaffold: roles, constraints, and status (no network I/O yet).
//!
//! Design check: [`docs/stratum-v2.md`](../../docs/stratum-v2.md).
//! Reference crates (`stratum-core` / `binary_sv2` / …) land behind feature
//! `stratum-v2` in a follow-up PR after that design lock.

mod bch;
mod roles;
mod status;

pub use bch::{
    BchTemplateConstraints, asert_target_required, cashaddr_payouts_required,
    ctor_full_templates_required, no_segwit_witness_commitment, respects_adaptive_block_size,
};
pub use roles::Role;
pub use status::{StratumV2Status, status_report};
