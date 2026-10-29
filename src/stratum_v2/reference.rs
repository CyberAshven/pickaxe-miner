//! Link proof for optional stratum-mining reference crates.
//!
//! Present only so `cargo check --features stratum-v2` keeps `stratum-core`
//! (and its re-exports) in the graph. No network I/O, Noise, or share logic.

/// Returns whether the reference crates are compiled into this build.
pub fn linked() -> bool {
    #[cfg(feature = "stratum-v2")]
    {
        // Touch re-exports so the optional dep stays linked.
        let _ = core::mem::size_of::<stratum_core::binary_sv2::U24>();
        let _ = core::mem::size_of::<stratum_core::mining_sv2::OpenStandardMiningChannelOwned>();
        let _ = core::mem::size_of::<stratum_core::framing_sv2::header::Header>();
        let _ = core::mem::size_of::<stratum_core::template_distribution_sv2::NewTemplateOwned>();
        true
    }
    #[cfg(not(feature = "stratum-v2"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_matches_feature_cfg() {
        assert_eq!(linked(), cfg!(feature = "stratum-v2"));
    }
}
