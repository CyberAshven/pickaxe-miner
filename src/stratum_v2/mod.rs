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
pub mod device_api;
#[cfg(feature = "stratum-v2")]
pub mod fleet;
#[cfg(feature = "stratum-v2")]
pub mod journal;
#[cfg(all(test, feature = "stratum-v2"))]
mod journal_tests;
#[cfg(all(test, feature = "stratum-v2"))]
mod live_tests;
// #### PR #42: the Device panel (row selection and one device's controls).
#[cfg(feature = "stratum-v2")]
mod logins;
#[cfg(feature = "stratum-v2")]
mod panel;
// #### PR #42: merge mining for BCH covenant tokens (no token registered yet).
#[cfg(feature = "stratum-v2")]
pub mod merge;
#[cfg(feature = "stratum-v2")]
mod payout;
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
#[cfg(feature = "stratum-v2")]
mod work_allocation;

pub use bch::{
    asert_target_required, cashaddr_payouts_required, ctor_full_templates_required,
    no_segwit_witness_commitment, respects_adaptive_block_size, BchTemplateConstraints,
};
pub use reference::linked as reference_crates_linked;

/// #### PR #40
/// A pool address as SV2 pools publish it, `stratum2+tcp://HOST:PORT/KEY`
/// (the form ckpool and Braiins use, and this server's Connection info),
/// split into `HOST:PORT` and the pool's authority key; a bare `HOST:PORT`
/// has no key. An SV1 address is refused: Pickaxe joins SV2 pools only, and
/// translates SV1 for the devices on this side.
pub fn split_pool_address(text: &str) -> Result<(String, Option<String>), String> {
    let text = text.trim();
    if text.starts_with("stratum+") {
        return Err(
            "that is an SV1 pool address; joining needs the pool's SV2 address \
             (stratum2+tcp://HOST:PORT/KEY)"
                .into(),
        );
    }
    let text = text.strip_prefix("stratum2+tcp://").unwrap_or(text);
    let (address, key) = match text.split_once('/') {
        Some((address, key)) => {
            let key = key.trim().trim_end_matches('/');
            (address, (!key.is_empty()).then(|| key.to_owned()))
        }
        None => (text, None),
    };
    if address
        .rsplit_once(':')
        .is_none_or(|(host, port)| host.is_empty() || port.parse::<u16>().is_err())
    {
        return Err("enter the pool as HOST:PORT or stratum2+tcp://HOST:PORT/KEY".into());
    }
    Ok((address.to_owned(), key))
}

pub use roles::Role;
pub use status::{status_report, StratumV2Status};

#[cfg(test)]
mod pool_address_tests {
    use super::split_pool_address;

    #[test]
    fn a_pool_address_may_carry_its_key() {
        assert_eq!(
            split_pool_address(" stratum2+tcp://pool.example:3336/KEY ").unwrap(),
            ("pool.example:3336".into(), Some("KEY".into()))
        );
        assert_eq!(
            split_pool_address("pool.example:3336/KEY/").unwrap(),
            ("pool.example:3336".into(), Some("KEY".into()))
        );
        assert_eq!(
            split_pool_address("[::1]:3336").unwrap(),
            ("[::1]:3336".into(), None)
        );
        assert_eq!(
            split_pool_address("stratum2+tcp://pool.example:3336/").unwrap(),
            ("pool.example:3336".into(), None)
        );
        assert!(split_pool_address("stratum+tcp://pool.example:3333")
            .unwrap_err()
            .contains("SV1"));
        assert!(split_pool_address("pool.example").is_err());
        assert!(split_pool_address("stratum2+tcp://:3336/KEY").is_err());
    }
}
