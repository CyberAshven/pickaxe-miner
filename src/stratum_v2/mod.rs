//! BCH Stratum V2 transport, mining channels and node-backed templates.
//! See implementation-status.md for validation evidence and unfinished roles.
//!
//! Design check: [`docs/stratum-v2.md`](../../docs/stratum-v2.md).
//! Reference crates (`stratum-core`) are optional behind feature `stratum-v2`.

mod bch;
#[cfg(feature = "stratum-v2")]
pub mod channel;
#[cfg(feature = "stratum-v2")]
pub mod command;
#[cfg(feature = "stratum-v2")]
pub mod journal;
#[cfg(all(test, feature = "stratum-v2"))]
mod journal_tests;
#[cfg(all(test, feature = "stratum-v2"))]
mod live_tests;
#[cfg(feature = "stratum-v2")]
pub mod provider;
mod reference;
mod roles;
#[cfg(feature = "stratum-v2")]
pub mod server;
#[cfg(all(test, feature = "stratum-v2"))]
mod server_tests;
mod status;
#[cfg(feature = "stratum-v2")]
pub mod sv1;
#[cfg(feature = "stratum-v2")]
pub mod telemetry;
#[cfg(feature = "stratum-v2")]
pub mod template;
#[cfg(all(test, feature = "stratum-v2"))]
mod template_tests;
#[cfg(feature = "stratum-v2")]
pub mod transport;
#[cfg(feature = "stratum-v2")]
pub mod wire;

pub use bch::{
    asert_target_required, cashaddr_payouts_required, ctor_full_templates_required,
    no_segwit_witness_commitment, respects_adaptive_block_size, BchTemplateConstraints,
};
pub use reference::linked as reference_crates_linked;
pub use roles::Role;
pub use status::{status_report, StratumV2Status};
