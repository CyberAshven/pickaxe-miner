//! #### PR #38
//! ASIC work is scheduled by eligible channel time, not advertised hashrate or
//! number of winning blocks. Independent phases avoid a reconnect grace period.
//! Job/target refreshes do not reset allocation; unavailable work does not accrue.

use crate::donation::bch::BchDonation;
use std::time::Instant;

const PERIOD_NS: u64 = 600_000_000_000;

pub(super) struct WorkAllocation {
    position: u64,
    last: Option<Instant>,
}

impl WorkAllocation {
    pub fn new(phase: u64) -> Self {
        Self {
            position: phase % PERIOD_NS,
            last: None,
        }
    }

    pub fn update(&mut self, now: Instant, available: bool) {
        if available {
            if let Some(last) = self.last {
                self.advance(now.saturating_duration_since(last).as_nanos());
            }
            self.last = Some(now);
        } else {
            self.last = None;
        }
    }

    fn advance(&mut self, elapsed_ns: u128) {
        self.position = ((u128::from(self.position) + elapsed_ns % u128::from(PERIOD_NS))
            % u128::from(PERIOD_NS)) as u64;
    }

    pub fn donation_work(&self, rate: BchDonation) -> bool {
        self.position < rate.work_units(PERIOD_NS)
    }

    /// #### PR #40: at a remote pool the whole donation is work.
    pub fn pool_donation_work(&self, rate: BchDonation) -> bool {
        self.position < rate.pool_work_units(PERIOD_NS)
    }

    // #### PR #42: the token donation rotation
    // What: on an ASIC-exclusive token the whole donation is mining work:
    // the first `bps` ten-thousandths of each 10-minute cycle mine donation
    // jobs, whose claims the donation's forwarder receives.
    // Why: such a token pays the whole claim to one output, so the BCH
    // split (two thirds from rewards) is impossible; the token's minimum
    // (1.5% unless its row says more) applies.
    // Look here if: token donation jobs take more or less than the setting.
    /// Whether the job now is donation work for a token at `rate`.
    pub fn token_donation_work(&self, rate: crate::donation::TokenDonation) -> bool {
        self.position < u64::from(rate.bps()) * (PERIOD_NS / 10_000)
    }

    /// #### PR #40: a public pool operator's work share, right after the
    /// donation's and a share of what it leaves.
    pub fn fee_work(&self, rate: BchDonation, fee: crate::donation::bch::PoolFee) -> bool {
        let donation = rate.work_units(PERIOD_NS);
        let fee = fee.work_units(PERIOD_NS - donation);
        (donation..donation + fee).contains(&self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn allocation_uses_eligible_time_and_survives_refresh_rate_change_and_pause() {
        let now = Instant::now();
        let rate = BchDonation::default();
        let mut allocation = WorkAllocation::new(0);
        allocation.update(now, true);
        assert!(allocation.donation_work(rate));
        allocation.update(now + Duration::from_millis(2999), true);
        assert!(allocation.donation_work(rate));
        allocation.update(now + Duration::from_secs(3), true);
        assert!(!allocation.donation_work(rate));
        let position = allocation.position;
        assert!(allocation.donation_work("2".parse().unwrap()));
        assert_eq!(allocation.position, position);
        allocation.update(now + Duration::from_secs(3), false);
        allocation.update(now + Duration::from_secs(3603), true);
        assert_eq!(allocation.position, position);
        allocation.update(now + Duration::from_secs(4200), true);
        assert_eq!(allocation.position, 0);
    }

    // #### PR #42
    // What: a token donation of 1.5% is the first 9 seconds of each
    // 10-minute cycle, all of it work.
    // Look here if: token_donation_work changes.
    #[test]
    fn token_donation_is_the_whole_share_of_work() {
        let rate = crate::donation::NEW_TOKEN_DONATION;
        let boundary = 9_000_000_000;
        assert!(WorkAllocation::new(boundary - 1).token_donation_work(rate));
        assert!(!WorkAllocation::new(boundary).token_donation_work(rate));
        assert!(!WorkAllocation::new(0)
            .token_donation_work(crate::donation::TokenDonation::from_bps(0)));
    }

    #[test]
    fn full_cycles_follow_the_ratio_at_every_supported_setting() {
        for bps in 150..=10_000 {
            let rate = BchDonation::try_from(bps).unwrap();
            let boundary = rate.work_units(PERIOD_NS);
            let mut allocation = WorkAllocation::new(boundary - 1);
            assert!(allocation.donation_work(rate));
            allocation.advance(1);
            assert!(!allocation.donation_work(rate));
            allocation.advance(u128::from(PERIOD_NS));
            assert_eq!(allocation.position, boundary);
            allocation.advance(u128::MAX);
            assert!(allocation.position < PERIOD_NS);
        }
    }
}
