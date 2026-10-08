//! #### PR #38
//! BCH's adjustable donation policy: 1.5% by default, adjustable from 0% to
//! 100% in 0.5% steps. One third of it is mining work and two thirds is the
//! block reward. Integer ratios stay exact until the payout boundary; other
//! assets retain their own policies.

use crate::config::MiningNetwork;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// Where the BCH ASIC donation goes on mainnet.
pub const MAINNET_ADDRESS: &str = "bitcoincash:qze95tc2dqrnltvxfa5yhunwx5952f34fvw4g6365k";
/// Where the BCH ASIC donation goes on Chipnet.
pub const CHIPNET_ADDRESS: &str = "bchtest:qrzq5f9ltv70u4su7d40agd4nlnp8qlgqcma6x2tvp";

/// The BCH donation address of a network. Only the network chooses it; the
/// shared payout guard checks it like any other payout.
pub fn address(network: MiningNetwork) -> &'static str {
    match network {
        MiningNetwork::Mainnet => MAINNET_ADDRESS,
        MiningNetwork::Chipnet => CHIPNET_ADDRESS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct BchDonation(u16);

impl Default for BchDonation {
    fn default() -> Self {
        Self(150)
    }
}

impl TryFrom<u16> for BchDonation {
    type Error = String;
    fn try_from(bps: u16) -> Result<Self, String> {
        if bps > 10_000 {
            return Err("BCH donation must be between 0% and 100%".into());
        }
        Ok(Self(bps))
    }
}

impl From<BchDonation> for u16 {
    fn from(value: BchDonation) -> Self {
        value.0
    }
}

impl FromStr for BchDonation {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, String> {
        let invalid = || "use a donation percentage with at most two decimal places".to_owned();
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || fraction.len() > 2
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let whole: u16 = whole.parse().map_err(|_| invalid())?;
        let fraction: u16 = if fraction.is_empty() {
            0
        } else {
            fraction.parse().map_err(|_| invalid())?
        };
        let bps = whole
            .checked_mul(100)
            .and_then(|v| {
                v.checked_add(
                    fraction
                        * if value.rsplit_once('.').is_some_and(|(_, f)| f.len() == 1) {
                            10
                        } else {
                            1
                        },
                )
            })
            .ok_or_else(invalid)?;
        Self::try_from(bps)
    }
}

/// A percentage in hundredths of a percent, rounded up, as "1.50%".
fn percent(hundredths: u32) -> String {
    format!("{}.{:02}%", hundredths / 100, hundredths % 100)
}

impl fmt::Display for BchDonation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&percent(u32::from(self.0)))
    }
}

/// Steps of the advanced setting: 0.5%.
const STEP_BPS: u16 = 50;

impl BchDonation {
    /// The next 0.5% step up or down, between 0% and 100%.
    pub fn adjusted(self, increase: bool) -> Self {
        Self(if increase {
            (self.0 / STEP_BPS + 1).saturating_mul(STEP_BPS).min(10_000)
        } else {
            self.0.div_ceil(STEP_BPS).saturating_sub(1) * STEP_BPS
        })
    }

    /// The work share: one third of the total (0.5% of mining work at the
    /// 1.5% default). The work adapter chooses units (e.g. eligible
    /// nanoseconds); only that final integer is rounded.
    pub fn work_units(self, units: u64) -> u64 {
        (u128::from(units) * u128::from(self.0)).div_ceil(30_000) as u64
    }

    /// #### PR #40
    /// The work share when the whole donation is work, as at a remote pool,
    /// where Pickaxe cannot add a coinbase output: 1.5% at the default.
    pub fn pool_work_units(self, units: u64) -> u64 {
        (u128::from(units) * u128::from(self.0)).div_ceil(10_000) as u64
    }

    /// The block-reward share: two thirds of the total (1% of each block
    /// reward at the 1.5% default). Any fractional satoshi stays with the
    /// miner.
    pub fn reward_units(self, units: u64) -> u64 {
        (u128::from(units) * u128::from(self.0) * 2 / 30_000) as u64
    }

    /// The work and block-reward shares as shown to the miner, each rounded
    /// up to two decimals: "0.50%" and "1.00%" at 1.5%, "0.67%" and "1.34%"
    /// at 2%.
    pub fn shares(self) -> (String, String) {
        let bps = u32::from(self.0);
        (percent(bps.div_ceil(3)), percent((bps * 2).div_ceil(3)))
    }
}

/// #### PR #40
/// Where a public pool's operator takes its fee from: the coinbase of every
/// block, the mining work, or both (one third work, two thirds coinbase, the
/// donation's own split).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeeMode {
    Coinbase,
    Work,
    Both,
}

impl FromStr for FeeMode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "coinbase" => Ok(Self::Coinbase),
            "work" => Ok(Self::Work),
            "both" | "hybrid" => Ok(Self::Both),
            _ => Err("use coinbase, work or both".into()),
        }
    }
}

impl fmt::Display for FeeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Coinbase => "coinbase",
            Self::Work => "work",
            Self::Both => "both",
        })
    }
}

/// #### PR #40
/// A public pool operator's fee, in hundredths of a percent of what is left
/// after the Pickaxe donation: the donation is taken in full first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolFee {
    pub rate: BchDonation,
    pub mode: FeeMode,
}

impl PoolFee {
    /// The work share of `units` of work left after the donation's.
    pub fn work_units(self, units: u64) -> u64 {
        match self.mode {
            FeeMode::Coinbase => 0,
            FeeMode::Work => self.rate.pool_work_units(units),
            FeeMode::Both => self.rate.work_units(units),
        }
    }

    /// The coinbase share of `units` of reward left after the donation's;
    /// any fractional satoshi stays with the miner.
    pub fn reward_units(self, units: u64) -> u64 {
        match self.mode {
            FeeMode::Coinbase => {
                (u128::from(units) * u128::from(u16::from(self.rate)) / 10_000) as u64
            }
            FeeMode::Work => 0,
            FeeMode::Both => self.rate.reward_units(units),
        }
    }
}

/// Immutable policy attached to a job and its durable solved-block record.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BchPayout {
    pub donation: BchDonation,
    pub donation_work: bool,
    /// #### PR #40: a public pool's fee, and whether this job is its work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee: Option<PoolFee>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fee_work: bool,
}

impl BchPayout {
    /// The miner's, the donation's and the pool operator's amounts. The
    /// donation comes off first; the operator's fee comes off what is left.
    pub fn amounts(self, reward: u64) -> [u64; 3] {
        if self.donation_work {
            return [0, reward, 0];
        }
        if self.fee_work {
            return [0, 0, reward];
        }
        let donation = self.donation.reward_units(reward);
        let left = reward - donation;
        let fee = self.fee.map_or(0, |fee| fee.reward_units(left));
        [left - fee, donation, fee]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #40
    #[test]
    fn a_pool_fee_comes_off_what_the_donation_leaves() {
        let donation: BchDonation = "1.5".parse().unwrap();
        let fee = |mode| PoolFee {
            rate: "2".parse().unwrap(),
            mode,
        };
        let reward = 312_500_000;
        let payout = |fee| BchPayout {
            donation,
            fee: Some(fee),
            ..BchPayout::default()
        };
        // Coinbase: 1% of the reward to the donation, then 2% of the rest.
        let [miner, given, taken] = payout(fee(FeeMode::Coinbase)).amounts(reward);
        assert_eq!(given, 3_125_000);
        assert_eq!(taken, 6_187_500);
        assert_eq!(miner + given + taken, reward);
        // Work: no coinbase fee; the operator's work jobs pay it in full.
        assert_eq!(payout(fee(FeeMode::Work)).amounts(reward)[2], 0);
        let work_job = BchPayout {
            fee_work: true,
            ..payout(fee(FeeMode::Work))
        };
        assert_eq!(work_job.amounts(reward), [0, 0, reward]);
        // Both: two thirds of the fee from the coinbase, a third as work.
        assert_eq!(payout(fee(FeeMode::Both)).amounts(reward)[2], 4_125_000);
        assert_eq!(fee(FeeMode::Both).work_units(30_000), 200);
        assert_eq!(fee(FeeMode::Work).work_units(10_000), 200);
        assert_eq!(fee(FeeMode::Coinbase).work_units(10_000), 0);
        // A donation work job pays the donation in full, fee or not.
        let donation_job = BchPayout {
            donation_work: true,
            ..payout(fee(FeeMode::Coinbase))
        };
        assert_eq!(donation_job.amounts(reward), [0, reward, 0]);
        // Old journal records without a fee still read.
        let old: BchPayout =
            serde_json::from_str(r#"{"donation":150,"donation_work":false}"#).unwrap();
        assert_eq!(old.fee, None);
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"donation":150,"donation_work":false}"#
        );
        assert_eq!("hybrid".parse::<FeeMode>().unwrap(), FeeMode::Both);
        assert!("half".parse::<FeeMode>().is_err());
    }

    #[test]
    fn each_network_donates_to_its_own_address_through_the_shared_guard() {
        for (network, other) in [
            (MiningNetwork::Mainnet, MiningNetwork::Chipnet),
            (MiningNetwork::Chipnet, MiningNetwork::Mainnet),
        ] {
            let donation = address(network);
            assert!(crate::config::validate_payout_address(network, donation).is_ok());
            assert!(crate::config::validate_payout_address(other, donation).is_err());
            assert_eq!(
                crate::tx::cashaddr_to_p2pkh_locking(donation)
                    .unwrap()
                    .len(),
                25
            );
        }
    }

    #[test]
    fn decimal_setting_round_trips_and_rejects_invalid_values() {
        for (text, bps, display) in [
            ("0", 0, "0.00%"),
            ("1.5", 150, "1.50%"),
            ("2", 200, "2.00%"),
            ("2.01", 201, "2.01%"),
            ("100", 10_000, "100.00%"),
        ] {
            let rate: BchDonation = text.parse().unwrap();
            assert_eq!(u16::from(rate), bps);
            assert_eq!(rate.to_string(), display);
            assert_eq!(
                serde_json::from_str::<BchDonation>(&bps.to_string()).unwrap(),
                rate
            );
        }
        for text in [
            "", "100.01", "NaN", "inf", "2e0", "-2", "+2", "2.001", "2.0.0", "65535", " 2",
        ] {
            assert!(text.parse::<BchDonation>().is_err());
        }
        for value in ["10001", "65536", "1.5"] {
            assert!(serde_json::from_str::<BchDonation>(value).is_err());
        }
    }

    #[test]
    fn work_is_a_third_and_the_reward_two_thirds_keeping_fractions_for_the_miner() {
        for bps in 0..=10_000 {
            let donation = BchDonation::try_from(bps).unwrap();
            for value in [0, 1, 99, 100, 1001, 312_500_001, u64::MAX] {
                let numerator = u128::from(value) * u128::from(bps);
                let work = donation.work_units(value);
                let reward = donation.reward_units(value);
                assert!(u128::from(work) * 30_000 >= numerator);
                assert!(u128::from(work) * 30_000 - numerator < 30_000);
                assert!(u128::from(reward) * 30_000 <= numerator * 2);
                assert!(numerator * 2 - u128::from(reward) * 30_000 < 30_000);
                for donation_work in [false, true] {
                    let split = BchPayout {
                        donation,
                        donation_work,
                        ..BchPayout::default()
                    }
                    .amounts(value);
                    assert_eq!(split[0] + split[1], value);
                    assert_eq!(split[2], 0);
                    if donation_work {
                        assert_eq!(split, [0, value, 0]);
                    }
                }
            }
            // Per 30,000 units, the work share is T and the reward share 2T.
            assert_eq!(donation.work_units(30_000), u64::from(bps));
            assert_eq!(donation.reward_units(30_000), u64::from(bps) * 2);
        }
        assert_eq!(BchDonation::default().reward_units(312_500_000), 3_125_000);
        assert_eq!(BchDonation::default().reward_units(1), 0);
        assert_eq!(
            BchDonation::try_from(0).unwrap().reward_units(312_500_000),
            0
        );
    }

    #[test]
    fn shown_shares_round_up_to_two_decimals() {
        for (bps, work, reward) in [
            (0, "0.00%", "0.00%"),
            (150, "0.50%", "1.00%"),
            (200, "0.67%", "1.34%"),
            (250, "0.84%", "1.67%"),
            (10_000, "33.34%", "66.67%"),
        ] {
            let shares = BchDonation::try_from(bps).unwrap().shares();
            assert_eq!(shares, (work.to_owned(), reward.to_owned()));
        }
    }

    #[test]
    fn the_setting_moves_in_half_percent_steps_from_zero_to_one_hundred() {
        let step = |bps: u16, up: bool| u16::from(BchDonation::try_from(bps).unwrap().adjusted(up));
        assert_eq!(step(150, true), 200);
        assert_eq!(step(150, false), 100);
        assert_eq!(step(50, false), 0);
        assert_eq!(step(0, false), 0);
        assert_eq!(step(0, true), 50);
        assert_eq!(step(10_000, true), 10_000);
        // A setting saved between steps snaps to the neighbouring steps.
        assert_eq!(step(201, true), 250);
        assert_eq!(step(201, false), 200);
        assert_eq!(u16::from(BchDonation::default()), 150);
    }
}
