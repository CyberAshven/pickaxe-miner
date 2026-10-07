//! #### PR #38
//! BCH's adjustable donation policy. Integer ratios stay exact until the
//! payout boundary; other assets retain their own policies.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

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
        if !(150..=10_000).contains(&bps) {
            return Err("BCH donation must be between 1.5% and 100%".into());
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

impl fmt::Display for BchDonation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_multiple_of(10) {
            write!(f, "{}.{:01}%", self.0 / 100, self.0 % 100 / 10)
        } else {
            write!(f, "{}.{:02}%", self.0 / 100, self.0 % 100)
        }
    }
}

impl BchDonation {
    pub fn adjusted(self, increase: bool) -> Self {
        Self(if increase {
            self.0.saturating_add(10).min(10_000)
        } else {
            self.0.saturating_sub(10).max(150)
        })
    }

    /// The work adapter chooses units (e.g. eligible nanoseconds). Round only
    /// that final integer, never the internal percentage.
    pub fn work_units(self, units: u64) -> u64 {
        (u128::from(units) * u128::from(self.0)).div_ceil(30_000) as u64
    }

    /// The selected total includes work already dedicated to donation. The
    /// reward portion applies only to the remaining personal work: with
    /// w = T/3, r = (T-w)/(1-w), so w + (1-w)*r = T. This avoids reporting
    /// 1.5% while allocating only 1.495%. Any fractional satoshi remains with
    /// the miner; the percentage correction does not round up a satoshi charge.
    pub fn reward_units(self, units: u64) -> u64 {
        (u128::from(units) * u128::from(self.0) * 2 / (30_000 - u128::from(self.0))) as u64
    }
}

/// Immutable policy attached to a job and its durable solved-block record.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BchPayout {
    pub donation: BchDonation,
    pub donation_work: bool,
}

impl BchPayout {
    pub fn amounts(self, reward: u64) -> [u64; 2] {
        let donation = if self.donation_work {
            reward
        } else {
            self.donation.reward_units(reward)
        };
        [reward - donation, donation]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_setting_round_trips_and_rejects_invalid_values() {
        for (text, bps, display) in [
            ("1.5", 150, "1.5%"),
            ("2", 200, "2.0%"),
            ("2.01", 201, "2.01%"),
            ("100", 10_000, "100.0%"),
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
            "", "1.49", "0", "100.01", "NaN", "inf", "2e0", "-2", "+2", "2.001", "2.0.0", "65535",
            " 2",
        ] {
            assert!(text.parse::<BchDonation>().is_err());
        }
        for value in ["0", "149", "10001", "65536", "1.5"] {
            assert!(serde_json::from_str::<BchDonation>(value).is_err());
        }
    }

    #[test]
    fn rational_split_preserves_fractional_satoshis_for_the_miner_and_conserves_value() {
        for bps in 150..=10_000 {
            let donation = BchDonation::try_from(bps).unwrap();
            for value in [0, 1, 99, 100, 1001, 312_500_001, u64::MAX] {
                let numerator = u128::from(value) * u128::from(bps);
                let work = donation.work_units(value);
                let reward = donation.reward_units(value);
                assert!(u128::from(work) * 30_000 >= numerator);
                assert!(u128::from(work) * 30_000 - numerator < 30_000);
                let denominator = 30_000 - u128::from(bps);
                assert!(u128::from(reward) * denominator <= numerator * 2);
                assert!(numerator * 2 - u128::from(reward) * denominator < denominator);
                for donation_work in [false, true] {
                    let split = BchPayout {
                        donation,
                        donation_work,
                    }
                    .amounts(value);
                    assert_eq!(split[0] + split[1], value);
                    if donation_work {
                        assert_eq!(split, [0, value]);
                    }
                }
            }
        }
        assert_eq!(BchDonation::default().reward_units(312_500_001), 3_140_703);
        assert_eq!(BchDonation::default().reward_units(1), 0);
        let two: BchDonation = "2".parse().unwrap();
        assert_eq!(two.work_units(30_000), 200);
        assert_eq!(two.reward_units(30_000), 402);
        assert_eq!(
            BchDonation::default().adjusted(false),
            BchDonation::default()
        );
        assert_eq!(u16::from(two.adjusted(true)), 210);
    }

    #[test]
    fn combined_expected_donation_matches_selected_total_instead_of_undershooting() {
        for bps in 150..=10_000 {
            let policy = BchDonation::try_from(bps).unwrap();
            // A 30,000-unit ensemble contains bps donor-work units and
            // 30,000-bps personal-work units. It must donate 3*bps units,
            // exactly T of the ensemble, including at non-round settings.
            let work = policy.work_units(30_000);
            let personal = 30_000 - work;
            assert_eq!(work + policy.reward_units(personal), u64::from(bps) * 3);
        }
        let default = BchDonation::default();
        assert_eq!(
            default.work_units(30_000) + default.reward_units(29_850),
            450
        );
    }
}
