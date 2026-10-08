//! Token-selected fee policy; a work recipient is fixed before hashing.
use crate::config::{self, MiningNetwork};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

pub mod bch;

/// #### PR #32
/// What: a token's donation, as a share of the mining work itself in
/// hundredths of a percent. Each token has a minimum a miner can raise in
/// Advanced settings but not lower: 4% for PHOTON, and 1.5% (also the default)
/// for every token added later. BCH and its merge-mined tokens keep their own
/// policy (`bch::BchDonation`), which can go down to 0%.
/// Why: the operator's rule for using the software (2026-10-08).
/// Check: a saved or typed value below the token's minimum mines at the
/// minimum; raising it moves only the operator's share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct TokenDonation(u16);

/// The minimum and default of every token added after PHOTON.
pub const NEW_TOKEN_DONATION: TokenDonation = TokenDonation(150);
/// The most a miner can donate: the miner keeps a share of the work.
const MAX_TOKEN_DONATION_BPS: u16 = 9_950;
/// Steps of the Advanced setting: 0.5%.
const TOKEN_DONATION_STEP_BPS: u16 = 50;

impl TokenDonation {
    pub const fn from_bps(bps: u16) -> Self {
        Self(if bps > MAX_TOKEN_DONATION_BPS {
            MAX_TOKEN_DONATION_BPS
        } else {
            bps
        })
    }

    pub const fn bps(self) -> u16 {
        self.0
    }

    /// This value, or `minimum` when it is lower.
    pub fn at_least(self, minimum: Self) -> Self {
        self.max(minimum)
    }

    /// The next 0.5% step up or down, never below `minimum`.
    pub fn adjusted(self, increase: bool, minimum: Self) -> Self {
        let next = if increase {
            (self.0 / TOKEN_DONATION_STEP_BPS + 1).saturating_mul(TOKEN_DONATION_STEP_BPS)
        } else {
            self.0.div_ceil(TOKEN_DONATION_STEP_BPS).saturating_sub(1) * TOKEN_DONATION_STEP_BPS
        };
        Self::from_bps(next).at_least(minimum)
    }
}

impl TryFrom<u16> for TokenDonation {
    type Error = String;
    fn try_from(bps: u16) -> Result<Self, String> {
        if bps > MAX_TOKEN_DONATION_BPS {
            return Err("a token donation must be below 100%".into());
        }
        Ok(Self(bps))
    }
}

impl From<TokenDonation> for u16 {
    fn from(value: TokenDonation) -> Self {
        value.0
    }
}

impl FromStr for TokenDonation {
    type Err = String;
    /// A percentage such as "4", "4.5" or "6.25".
    fn from_str(value: &str) -> Result<Self, String> {
        let invalid = || "use a donation percentage with at most two decimal places".to_owned();
        let (whole, fraction) = value.trim().split_once('.').unwrap_or((value.trim(), ""));
        if whole.is_empty()
            || whole.len() > 3
            || fraction.len() > 2
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let whole: u16 = whole.parse().map_err(|_| invalid())?;
        let fraction: u16 = format!("{fraction:0<2}").parse().map_err(|_| invalid())?;
        let bps = whole
            .checked_mul(100)
            .and_then(|bps| bps.checked_add(fraction))
            .ok_or_else(invalid)?;
        Self::try_from(bps)
    }
}

impl fmt::Display for TokenDonation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}%", self.0 / 100, self.0 % 100)
    }
}

/// Basis points for project and collaborator. A hybrid's reward split applies
/// only to personal work, never to developer wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Work([u16; 2]),
    #[allow(dead_code)] // Selected by a token once its payout adapter supports splits.
    RewardSplit([u16; 2]),
    #[allow(dead_code)] // Selected independently by each token.
    Hybrid {
        work: [u16; 2],
        reward: [u16; 2],
    },
}
impl Scheme {
    pub fn work(self) -> [u16; 2] {
        match self {
            Self::Work(shares) | Self::Hybrid { work: shares, .. } => shares,
            Self::RewardSplit(_) => [0, 0],
        }
    }
    pub fn reward(self) -> [u16; 2] {
        match self {
            Self::RewardSplit(shares) | Self::Hybrid { reward: shares, .. } => shares,
            Self::Work(_) => [0, 0],
        }
    }
    pub fn validate(self) -> Result<(), String> {
        for shares in [self.work(), self.reward()] {
            if u32::from(shares[0]) + u32::from(shares[1]) >= 10_000 {
                return Err("fee shares must leave a positive miner allocation".into());
            }
        }
        Ok(())
    }
    /// Integer conservation without intermediate overflow, including u128::MAX.
    pub fn split_reward(self, amount: u128, recipient: Recipient) -> Result<[u128; 3], String> {
        self.validate()?;
        if recipient != Recipient::Miner {
            let mut amounts = [0; 3];
            amounts[recipient as usize] = amount;
            return Ok(amounts);
        }
        let [a, b] = self.reward().map(|bps| {
            amount / 10_000 * u128::from(bps) + amount % 10_000 * u128::from(bps) / 10_000
        });
        Ok([amount - a - b, a, b])
    }
    pub fn description(self) -> String {
        let format = |shares: [u16; 2]| {
            format!(
                "{}%",
                (u32::from(shares[0]) + u32::from(shares[1])) as f64 / 100.0
            )
        };
        match self {
            Self::Work(shares) => format!("Donation: {}", format(shares)),
            Self::RewardSplit(shares) => format!("Reward fee: {}", format(shares)),
            Self::Hybrid { work, reward } => format!(
                "Work fee: {}; personal-reward fee: {}",
                format(work),
                format(reward)
            ),
        }
    }
}

/// Selected by the token registry, never inferred from network or difficulty.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub scheme: Scheme,
    pub addresses: [&'static str; 2],
}
impl Policy {
    pub fn payouts(self, network: MiningNetwork, miner: &str) -> Result<[String; 3], String> {
        self.scheme.validate()?;
        let [a, b] = self.addresses;
        Ok([
            config::validate_payout_address(network, miner)?,
            config::reprefix_p2pkh_payout(a, network)?,
            config::reprefix_p2pkh_payout(b, network)?,
        ])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipient {
    Miner,
    Project,
    Collaborator,
}
impl Recipient {
    pub const ALL: [Self; 3] = [Self::Miner, Self::Project, Self::Collaborator];
}

/// Counts completed hashes, retaining allocation across pauses, jobs and keys.
pub struct Schedule {
    unit: u64,
    weights: [u64; 3],
    position: u64,
}
impl Schedule {
    pub fn new(scheme: Scheme, unit: u64, random: u64) -> Result<Self, String> {
        scheme.validate()?;
        let [a, b] = scheme.work().map(u64::from);
        let weights = [10_000 - a - b, a, b];
        let gcd = |mut a: u64, mut b: u64| {
            while b != 0 {
                (a, b) = (b, a % b);
            }
            a
        };
        let divisor = gcd(gcd(weights[0], a), b);
        let weights = weights.map(|weight| weight / divisor);
        let cycle = weights
            .iter()
            .sum::<u64>()
            .checked_mul(unit)
            .filter(|cycle| *cycle > 0)
            .ok_or("invalid work allocation period")?;
        Ok(Self {
            unit,
            weights,
            position: random % cycle,
        })
    }
    pub fn recipient(&self) -> Recipient {
        let slot = self.position / self.unit;
        if slot < self.weights[0] {
            Recipient::Miner
        } else if slot < self.weights[0] + self.weights[1] {
            Recipient::Project
        } else {
            Recipient::Collaborator
        }
    }
    pub fn limit_batch(&self, requested: u32) -> u32 {
        u64::from(requested).min(self.unit - self.position % self.unit) as u32
    }
    pub fn record(&mut self, completed: u32) -> Result<(), String> {
        if completed > self.limit_batch(completed) {
            return Err("GPU batch crossed its mining-work allocation".into());
        }
        let cycle = self.weights.iter().sum::<u64>() * self.unit;
        self.position =
            ((u128::from(self.position) + u128::from(completed)) % u128::from(cycle)) as u64;
        Ok(())
    }
}

pub(crate) fn require_direct_reward_policy(scheme: crate::donation::Scheme) -> Result<(), String> {
    scheme.validate()?;
    if scheme.reward() != [0, 0] {
        return Err(
            "this PHOTON deployment supports work fees only; a reward-split adapter is required"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #32
    #[test]
    fn a_token_donation_moves_in_half_percent_steps_above_its_minimum() {
        let photon = TokenDonation::from_bps(400);
        assert_eq!("4".parse::<TokenDonation>().unwrap(), photon);
        assert_eq!("4.5".parse::<TokenDonation>().unwrap().bps(), 450);
        assert_eq!("6.25".parse::<TokenDonation>().unwrap().bps(), 625);
        for bad in ["", "-1", "4.555", "100", "1e2", "4,5", "1000"] {
            assert!(bad.parse::<TokenDonation>().is_err(), "{bad}");
        }
        assert_eq!(photon.to_string(), "4.00%");
        assert_eq!(photon.adjusted(true, photon).bps(), 450);
        assert_eq!(photon.adjusted(false, photon), photon);
        assert_eq!(
            TokenDonation::from_bps(475).adjusted(false, photon).bps(),
            450
        );
        assert_eq!(TokenDonation::from_bps(100).at_least(photon), photon);
        assert_eq!(
            TokenDonation::from_bps(9_950).adjusted(true, photon).bps(),
            9_950
        );
        assert_eq!(NEW_TOKEN_DONATION.to_string(), "1.50%");
        assert_eq!(
            NEW_TOKEN_DONATION.adjusted(false, NEW_TOKEN_DONATION),
            NEW_TOKEN_DONATION
        );
        assert!(serde_json::from_str::<TokenDonation>("10000").is_err());
        assert_eq!(serde_json::to_string(&photon).unwrap(), "400");
    }

    #[test]
    fn fee_description_reports_only_totals() {
        assert_eq!(Scheme::Work([200, 200]).description(), "Donation: 4%");
        assert_eq!(
            Scheme::Hybrid {
                work: [150, 100],
                reward: [100, 50],
            }
            .description(),
            "Work fee: 2.5%; personal-reward fee: 1.5%"
        );
    }

    #[test]
    fn policy_modes_conserve_rewards_and_actual_work() {
        for scheme in [
            Scheme::Work([200, 200]),
            Scheme::Work([400, 0]),
            Scheme::RewardSplit([100, 100]),
            Scheme::Hybrid {
                work: [200, 100],
                reward: [150, 50],
            },
        ] {
            for amount in [0, 1, 99, 10_000, u128::MAX] {
                for recipient in Recipient::ALL {
                    let split = scheme.split_reward(amount, recipient).unwrap();
                    assert_eq!(split.iter().sum::<u128>(), amount);
                    if recipient != Recipient::Miner {
                        assert_eq!(split[recipient as usize], amount);
                    }
                }
            }
            for start in [0, 1, 4799, 4900, u64::MAX] {
                let mut schedule = Schedule::new(scheme, 101, start).unwrap();
                let period = schedule.weights.iter().sum::<u64>() * schedule.unit;
                let mut remaining = period;
                let mut counts = [0u64; 3];
                while remaining > 0 {
                    let count = schedule.limit_batch(37.min(remaining as u32));
                    counts[schedule.recipient() as usize] += u64::from(count);
                    schedule.record(count).unwrap();
                    remaining -= u64::from(count);
                }
                assert_eq!(counts, schedule.weights.map(|weight| weight * 101));
                assert_eq!(schedule.position, start % period);
            }
        }
        assert!(Scheme::Work([u16::MAX, 1]).validate().is_err());
        assert!(Scheme::Hybrid {
            work: [0, 0],
            reward: [5000, 5000]
        }
        .validate()
        .is_err());
        assert!(Schedule::new(Scheme::Work([200, 200]), u64::MAX, 0).is_err());
        let mut schedule = Schedule::new(Scheme::Work([200, 200]), 100, 4799).unwrap();
        assert!(schedule.record(2).is_err());
        schedule.record(0).unwrap();
        assert_eq!(schedule.position, 4799);
    }
}
