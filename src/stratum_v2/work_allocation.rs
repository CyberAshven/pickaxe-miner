//! #### PR #38
//! BCH allocation follows difficulty-weighted validated shares. Until a device's
//! first share, eligible time supplies a randomized startup phase; tokens retain
//! their time policy. Neither advertised hashrate nor winning-block count is work.

use crate::donation::bch::BchDonation;
use std::time::Instant;

const PERIOD_NS: u64 = 600_000_000_000;

pub(super) struct WorkAllocation {
    position: u64,
    last: Option<Instant>,
    balance: Option<WorkBalance>,
}

impl WorkAllocation {
    pub fn new(phase: u64) -> Self {
        Self {
            position: phase % PERIOD_NS,
            last: None,
            balance: None,
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
        self.balance.as_ref().map_or_else(
            || self.position < rate.work_units(PERIOD_NS),
            |balance| u16::from(rate) != 0 && balance.donation > 0.0,
        )
    }

    /// #### PR #40: at a remote pool the whole donation is work.
    pub fn pool_donation_work(&self, rate: BchDonation) -> bool {
        self.balance.as_ref().map_or_else(
            || self.position < rate.pool_work_units(PERIOD_NS),
            |balance| u16::from(rate) == 10_000 || (u16::from(rate) != 0 && balance.donation > 0.0),
        )
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

    // #### PR #48: validated work, not elapsed time, repays BCH work quotas.
    // Old jobs keep their own rates and recipient, including late valid shares.
    pub fn accepted(
        &mut self,
        target: &super::template::Hash,
        payout: crate::donation::bch::BchPayout,
    ) {
        let donation = f64::from(u16::from(payout.donation)) / 30_000.0;
        let fee = payout.fee.map_or(0.0, |fee| {
            use crate::donation::bch::FeeMode;
            let divisor = match fee.mode {
                FeeMode::Coinbase => return 0.0,
                FeeMode::Work => 10_000.0,
                FeeMode::Both => 30_000.0,
            };
            (1.0 - donation) * f64::from(u16::from(fee.rate)) / divisor
        });
        self.credit(target, donation, fee, payout.donation_work, payout.fee_work);
    }

    pub fn pool_accepted(
        &mut self,
        target: &super::template::Hash,
        rate: BchDonation,
        donation: bool,
    ) {
        self.credit(
            target,
            f64::from(u16::from(rate)) / 10_000.0,
            0.0,
            donation,
            false,
        );
    }

    fn credit(
        &mut self,
        target: &super::template::Hash,
        donation: f64,
        fee: f64,
        donated: bool,
        charged: bool,
    ) {
        let hashes = super::telemetry::expected_hashes(target);
        let phase = self.position as f64 / PERIOD_NS as f64;
        let balance = self.balance.get_or_insert(WorkBalance {
            donation: -phase * hashes,
            fee: -(1.0 - phase) * hashes,
        });
        balance.donation += hashes * (donation - f64::from(u8::from(donated)));
        balance.fee += hashes * (fee - f64::from(u8::from(charged)));
    }

    /// #### PR #40: a public pool operator's work share, right after the
    /// donation's and a share of what it leaves.
    pub fn fee_work(&self, rate: BchDonation, fee: crate::donation::bch::PoolFee) -> bool {
        if let Some(balance) = &self.balance {
            return fee.work_units(PERIOD_NS) != 0 && balance.fee > 0.0;
        }
        let donation = rate.work_units(PERIOD_NS);
        let fee = fee.work_units(PERIOD_NS - donation);
        (donation..donation + fee).contains(&self.position)
    }
}

// Shares estimate work; firmware does not report every hash. Quotas converge
// over accepted work, with a boundary error of roughly one share, not per block.
struct WorkBalance {
    donation: f64,
    fee: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bch_quota_follows_weighted_work_across_speed_changes_and_pauses() {
        use crate::donation::bch::{BchPayout, FeeMode, PoolFee};
        let rate = BchDonation::default();
        let fee = PoolFee {
            rate: "3".parse().unwrap(),
            mode: FeeMode::Both,
        };
        for phase in [0, PERIOD_NS / 2, PERIOD_NS - 1] {
            let mut allocation = WorkAllocation::new(phase);
            let mut total = 0.0;
            let mut donated = 0.0;
            let mut charged = 0.0;
            let start = Instant::now();
            for n in 0..100_000 {
                let mut target = [255; 32];
                // Different difficulty and wildly different elapsed time must
                // not turn the quota into a share count or a wall-clock ratio.
                target[31] >>= (n / 137) % 5;
                let payout = BchPayout {
                    donation: rate,
                    donation_work: allocation.donation_work(rate),
                    fee: Some(fee),
                    fee_work: !allocation.donation_work(rate) && allocation.fee_work(rate, fee),
                };
                let work = super::super::telemetry::expected_hashes(&target);
                total += work;
                donated += if payout.donation_work { work } else { 0.0 };
                charged += if payout.fee_work { work } else { 0.0 };
                allocation.accepted(&target, payout);
                let choice = allocation.donation_work(rate);
                allocation.update(start + Duration::from_secs(n * 17), n % 7 != 0);
                assert_eq!(allocation.donation_work(rate), choice);
            }
            assert!((donated / total - 0.005).abs() < 0.00005);
            assert!((charged / total - 0.995 * 0.01).abs() < 0.00005);
        }
    }

    #[test]
    fn late_work_uses_its_original_policy_and_never_rounds_a_payout() {
        use crate::donation::bch::BchPayout;
        let rate = BchDonation::default();
        let mut allocation = WorkAllocation::new(0);
        let personal = BchPayout {
            donation: rate,
            ..Default::default()
        };
        allocation.accepted(&[255; 32], personal);
        assert!(allocation.donation_work(rate));
        // A late share from the old personal job is still personal work.
        allocation.accepted(&[255; 32], personal);
        assert!((allocation.balance.as_ref().unwrap().donation - 0.01).abs() < 1e-12);
        allocation.accepted(
            &[255; 32],
            BchPayout {
                donation_work: true,
                ..personal
            },
        );
        assert!(!allocation.donation_work(rate));
        assert!(!allocation.donation_work("0".parse().unwrap()));
        assert_eq!(personal.amounts(99), [99, 0, 0]);
    }

    #[test]
    fn pool_quota_weights_the_accepted_targets_and_handles_zero_and_all_work() {
        let rate: BchDonation = "1.5".parse().unwrap();
        let mut allocation = WorkAllocation::new(PERIOD_NS / 3);
        let mut total = 0.0;
        let mut donated = 0.0;
        for n in 0..100_000 {
            let donate = allocation.pool_donation_work(rate);
            let mut target = [255; 32];
            target[31] >>= if donate { 3 } else { n % 3 };
            let work = super::super::telemetry::expected_hashes(&target);
            total += work;
            donated += if donate { work } else { 0.0 };
            allocation.pool_accepted(&target, rate, donate);
        }
        assert!((donated / total - 0.015).abs() < 0.00005);
        assert!(!allocation.pool_donation_work("0".parse().unwrap()));
        assert!(allocation.pool_donation_work("100".parse().unwrap()));
        allocation.pool_accepted(&[0; 32], "100".parse().unwrap(), true);
        assert!(allocation.balance.as_ref().unwrap().donation.is_finite());
    }

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
