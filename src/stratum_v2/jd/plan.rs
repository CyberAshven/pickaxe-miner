//! #### PR #42
//! What a Job Declaration client's local server builds its jobs from while
//! the pool accepts its templates: the pool channel's extranonce prefix, the
//! pool's outputs and rates (from its token), and the pool's share target.

use super::{policy::PayoutRule, token::PoolRates};
use crate::stratum_v2::template::Hash;

/// #### PR #42: what merge-mined token leaves bind: the payout script, and
/// the donation's split (its share in hundredths of a percent and script).
pub type TokenTerms<'a> = (&'a [u8], Option<(u16, &'a [u8])>);

/// One plan per pool session; a new session or token rates make a new one,
/// with a new serial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JdPlan {
    pub serial: u64,
    /// The pool channel's extranonce prefix: the coinbase script carries it
    /// right after the client's prefix.
    pub upstream_prefix: Vec<u8>,
    /// Zero bytes after the job id when the pool lets the client roll more
    /// than the layout needs.
    pub pad: Vec<u8>,
    /// #### PR #56: the pool granted fewer than 16 bytes, so the compact
    /// layout (2-byte job id and lane, 4 bytes per device) is used.
    pub compact: bool,
    /// The pool's outputs, in its order: the miner's own script first.
    pub scripts: Vec<Vec<u8>>,
    pub rates: PoolRates,
    /// Shares meeting this go to the pool.
    pub pool_target: Hash,
    /// PPLNS construction belongs to the independently verified P2Pool node.
    pub p2pool: Option<std::sync::Arc<crate::p2pool::jd::Plan>>,
}

impl JdPlan {
    /// The coinbase outputs of a template worth `value`: the pool's scripts
    /// in its order, the donation and the fee their whole shares (the
    /// pool's rule), the miner the rest.
    pub fn outputs(&self, value: u64) -> Vec<(u64, Vec<u8>)> {
        let script = |index: Option<u8>| {
            index
                .and_then(|index| self.scripts.get(usize::from(index)))
                .cloned()
        };
        let rule = PayoutRule {
            miner: self.scripts.first().cloned().unwrap_or_default(),
            donation: script(self.rates.donation_output)
                .map(|script| (script, self.rates.donation_bps)),
            fee: script(self.rates.fee_output).map(|script| (script, self.rates.fee_bps)),
        };
        let (donation, fee) = rule.minimums(value);
        let mut amounts = vec![0; self.scripts.len()];
        if let Some(index) = self.rates.donation_output {
            amounts[usize::from(index)] += donation;
        }
        if let Some(index) = self.rates.fee_output {
            amounts[usize::from(index)] += fee;
        }
        amounts[0] += value - donation - fee;
        amounts
            .into_iter()
            .zip(self.scripts.iter().cloned())
            .collect()
    }

    /// #### PR #42: what merge-mined token leaves bind under this plan: the
    /// miner's own script (the pool's first output) and the donation's
    /// split, the pool's whole donation rate to its donation output (no
    /// donation work runs under Job Declaration); `None` without outputs.
    pub fn token_terms(&self) -> Option<TokenTerms<'_>> {
        let miner = self.scripts.first()?;
        let split = self
            .rates
            .donation_output
            .and_then(|index| self.scripts.get(usize::from(index)))
            .filter(|_| self.rates.donation_bps > 0)
            .map(|script| (self.rates.donation_bps, script.as_slice()));
        Some((miner.as_slice(), split))
    }

    /// The rollable bytes the pool takes: the job id, the pad, the lane and
    /// the device's.
    pub fn rollable(&self) -> usize {
        let (id, lane, device) = self.layout();
        id + self.pad.len() + lane + device
    }

    /// #### PR #56: the bytes of the job id, the lane and each device: 4, 4
    /// and 8, or 2, 2 and 4 in the compact layout.
    pub fn layout(&self) -> (usize, usize, usize) {
        if self.compact {
            (2, 2, super::COMPACT_DEVICE_EXTRANONCE)
        } else {
            (4, 4, crate::stratum_v2::channel::DEVICE_EXTRANONCE_SIZE)
        }
    }

    /// Whether the pool's outputs name valid indexes: the miner first, the
    /// donation and fee within the list; #### PR #42: and its rates are at
    /// most 100%, so the payout never underflows and a token split fits.
    pub fn is_valid(&self) -> bool {
        if self.p2pool.is_some() {
            return self.upstream_prefix.len() + self.rollable() == 12 && !self.scripts.is_empty();
        }
        let within = |index: Option<u8>| {
            index.is_none_or(|index| usize::from(index) < self.scripts.len() && index > 0)
        };
        !self.scripts.is_empty()
            && within(self.rates.donation_output)
            && within(self.rates.fee_output)
            && self.rates.donation_bps <= 10_000
            && self.rates.fee_bps <= 10_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
    // What: a plan pays the pool's rule exactly: at 1.5% and 1%, 312,500,000
    // satoshis go 304,734,375 to the miner, 3,078,125 to the fee and
    // 4,687,500 to the donation, in the pool's order, and the pool's rule
    // accepts them; indexes outside the outputs make the plan invalid.
    // Look here if: JdPlan::outputs changes.
    #[test]
    fn outputs_for_a_pickaxe_pool_follow_its_rule() {
        let plan = JdPlan {
            serial: 1,
            upstream_prefix: vec![0; 16],
            pad: Vec::new(),
            compact: false,
            scripts: vec![vec![0x51], vec![0x52], vec![0x53]],
            rates: PoolRates {
                donation_bps: 150,
                fee_bps: 100,
                donation_output: Some(2),
                fee_output: Some(1),
            },
            pool_target: [0xff; 32],
            p2pool: None,
        };
        assert!(plan.is_valid());
        assert_eq!(plan.rollable(), 16);
        // #### PR #56: the compact layout fits a pool's 8 bytes, its pad
        // taking any it grants beyond them.
        let compact = JdPlan {
            compact: true,
            ..plan.clone()
        };
        assert_eq!((compact.layout(), compact.rollable()), ((2, 2, 4), 8));
        let wider = JdPlan {
            pad: vec![0; 4],
            ..compact
        };
        assert_eq!(wider.rollable(), 12);
        let outputs = plan.outputs(312_500_000);
        assert_eq!(
            outputs,
            vec![
                (304_734_375, vec![0x51]),
                (3_078_125, vec![0x52]),
                (4_687_500, vec![0x53]),
            ]
        );
        let rule = PayoutRule {
            miner: vec![0x51],
            donation: Some((vec![0x53], 150)),
            fee: Some((vec![0x52], 100)),
        };
        assert_eq!(rule.check(&outputs), Ok(312_500_000));
        let shared = JdPlan {
            scripts: vec![vec![0x51]],
            rates: PoolRates {
                donation_bps: 0,
                fee_bps: 0,
                donation_output: None,
                fee_output: None,
            },
            ..plan.clone()
        };
        assert_eq!(shared.outputs(1_000), vec![(1_000, vec![0x51])]);
        let broken = JdPlan {
            rates: PoolRates {
                donation_output: Some(3),
                ..plan.rates
            },
            ..plan.clone()
        };
        assert!(!broken.is_valid());
        // #### PR #42: rates over 100% are refused.
        for rates in [
            PoolRates {
                donation_bps: 10_001,
                ..plan.rates
            },
            PoolRates {
                fee_bps: 10_001,
                ..plan.rates
            },
        ] {
            assert!(!JdPlan {
                rates,
                ..plan.clone()
            }
            .is_valid());
        }
    }
}
