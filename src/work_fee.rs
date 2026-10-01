//! Token-selected fee policy; a work recipient is fixed before hashing.
use crate::config::{self, MiningNetwork};

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
                "{:.2}% ({:.2}% project / {:.2}% collaborator)",
                (u32::from(shares[0]) + u32::from(shares[1])) as f64 / 100.0,
                f64::from(shares[0]) / 100.0,
                f64::from(shares[1]) / 100.0
            )
        };
        match self {
            Self::Work(shares) => format!("Work fee: {}", format(shares)),
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
            config::reprefix_p2pkh_payout(miner, network)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_modes_conserve_rewards_and_actual_work() {
        for scheme in [
            Scheme::Work([200, 200]),
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
