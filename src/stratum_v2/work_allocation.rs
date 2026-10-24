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
            |balance| u16::from(rate) != 0 && balance.donation_due(),
        )
    }

    /// #### PR #40: at a remote pool the whole donation is work.
    pub fn pool_donation_work(&self, rate: BchDonation) -> bool {
        self.balance.as_ref().map_or_else(
            || self.position < rate.pool_work_units(PERIOD_NS),
            |balance| u16::from(rate) == 10_000 || (u16::from(rate) != 0 && balance.donation_due()),
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

    // #### merge-mining v2: a token without split support donates in work
    // What: a job that merge-mines a token of this server's own whose
    // covenant verifies no split has that token's leaves pay the donation
    // on the whole setting's share of work (1.5% at the default), not BCH's
    // work third: every BCH donation-work job, and enough other jobs to make
    // up the rest. Without accepted shares it is the first `rate` of each
    // 10-minute cycle (which holds BCH's donation window); with them, a
    // balance of validated work on those jobs alone. Split tokens take two
    // thirds of the setting from each win and only BCH's work third.
    // Why: without a split, work is such a token's donation's only part.
    // Look here if: merge-mined token donation jobs take more or less work
    // than the donation setting.
    /// Whether the job now pays merge-mined tokens' donation, at `rate`.
    pub fn merge_token_donation_work(&self, rate: BchDonation) -> bool {
        self.balance.as_ref().map_or_else(
            || self.position < rate.pool_work_units(PERIOD_NS),
            |balance| u16::from(rate) != 0 && balance.token_due(),
        )
    }

    // #### merge-mining v2: a token without split support pays the fee in
    // work
    // What: on a public pool, a job with such a token has its leaves pay the
    // operator on the whole fee's share of what the token's donation leaves
    // ((1 − donation) × fee, at any fee mode): every BCH fee-work job, and
    // enough other jobs to make up the rest. Without accepted shares it is
    // the window right after the token donation's in each 10-minute cycle;
    // with them, a balance of validated work on those jobs alone.
    // Why: the operator's fee applies to tokens and the miner cannot remove
    // it; without a split it can only be work.
    // Look here if: such a token's fee jobs take more or less work than the
    // fee.
    /// Whether the job now pays a public pool's fee for tokens without split
    /// support, after the token donation at `rate`.
    pub fn merge_token_fee_work(
        &self,
        rate: BchDonation,
        fee: crate::donation::bch::PoolFee,
    ) -> bool {
        if let Some(balance) = &self.balance {
            return u16::from(fee.rate) != 0 && balance.token_fee_due();
        }
        let donation = rate.pool_work_units(PERIOD_NS);
        let fee = fee.rate.pool_work_units(PERIOD_NS - donation);
        (donation..donation + fee).contains(&self.position)
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
        // #### merge-mining v2: shares on token jobs repay the token
        // donation at the whole rate (see `merge_token_donation_work`).
        if let (Some(token_work), Some(balance)) = (payout.token_donation_work, &mut self.balance) {
            let rate = f64::from(u16::from(payout.donation)) / 10_000.0;
            let donated = token_work || payout.donation_work;
            balance.token += balance.share * (rate - f64::from(u8::from(donated)));
        }
        // #### merge-mining v2: and the fee at its whole rate, of what the
        // whole donation leaves (see `merge_token_fee_work`).
        if let (Some(token_work), Some(fee), Some(balance)) =
            (payout.token_fee_work, payout.fee, &mut self.balance)
        {
            let donation = f64::from(u16::from(payout.donation)) / 10_000.0;
            let rate = (1.0 - donation) * f64::from(u16::from(fee.rate)) / 10_000.0;
            let charged = token_work || payout.fee_work;
            balance.token_fee += balance.share * (rate - f64::from(u8::from(charged)));
        }
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
            donation: 0.0,
            fee: 0.0,
            token: 0.0,
            token_fee: 0.0,
            phase,
            share: hashes,
        });
        balance.share = hashes;
        balance.donation += hashes * (donation - f64::from(u8::from(donated)));
        balance.fee += hashes * (fee - f64::from(u8::from(charged)));
    }

    /// #### PR #40: a public pool operator's work share, right after the
    /// donation's and a share of what it leaves.
    pub fn fee_work(&self, rate: BchDonation, fee: crate::donation::bch::PoolFee) -> bool {
        if let Some(balance) = &self.balance {
            return fee.work_units(PERIOD_NS) != 0 && balance.fee_due();
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
    /// #### merge-mining v2: the merge-mined token donation's balance.
    token: f64,
    /// #### merge-mining v2: and a public pool fee's, for the same tokens.
    token_fee: f64,
    // #### PR #48: the randomized startup phase is a fraction of the latest
    // share, not of the first. Vardiff can ease the target far below the
    // start difficulty; an offset fixed in first-share hashes would then
    // hold back donation and fee jobs for many later shares.
    phase: f64,
    share: f64,
}

impl WorkBalance {
    fn donation_due(&self) -> bool {
        self.donation > self.phase * self.share
    }

    fn fee_due(&self) -> bool {
        self.fee > (1.0 - self.phase) * self.share
    }

    fn token_due(&self) -> bool {
        self.token > self.phase * self.share
    }

    fn token_fee_due(&self) -> bool {
        self.token_fee > (1.0 - self.phase) * self.share
    }
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
                    token_donation_work: None,
                    token_fee_work: None,
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

    // #### PR #48
    // What: the first accepted share is 1024 times the settled share (a
    // start difficulty that vardiff then eases). Across startup phases, the
    // donation and the fee still get their share of the session's work.
    // Look here if: the startup phase or the work balance changes.
    #[test]
    fn startup_phase_follows_the_settled_share_after_vardiff_eases() {
        use crate::donation::bch::{BchPayout, FeeMode, PoolFee};
        let rate = BchDonation::default();
        let fee = PoolFee {
            rate: "1".parse().unwrap(),
            mode: FeeMode::Work,
        };
        let easy = [255; 32];
        let mut hard = [255; 32];
        hard[31] = 0;
        hard[30] = 0x3f;
        let first = super::super::telemetry::expected_hashes(&hard);
        assert_eq!(first, 1024.0);
        let later: u16 = 4096;
        let total = first + f64::from(later);
        let (phases, mut donated, mut charged) = (200, 0.0, 0.0);
        for k in 0..phases {
            // Past the time-based donation and fee windows, so the first
            // share is personal work for every phase checked here.
            let phase = 0.02 + 0.98 * f64::from(k) / f64::from(phases);
            let mut allocation = WorkAllocation::new((phase * PERIOD_NS as f64) as u64);
            for target in
                std::iter::once(&hard).chain(std::iter::repeat_n(&easy, usize::from(later)))
            {
                let donation_work = allocation.donation_work(rate);
                let payout = BchPayout {
                    donation: rate,
                    donation_work,
                    fee: Some(fee),
                    fee_work: !donation_work && allocation.fee_work(rate, fee),
                    token_donation_work: None,
                    token_fee_work: None,
                };
                let work = super::super::telemetry::expected_hashes(target);
                donated += if payout.donation_work { work } else { 0.0 };
                charged += if payout.fee_work { work } else { 0.0 };
                allocation.accepted(target, payout);
            }
        }
        // Within one settled share of each policy's whole-session work.
        let donated = donated / f64::from(phases);
        let charged = charged / f64::from(phases);
        assert!((donated - 0.005 * total).abs() < 1.0, "donated {donated}");
        assert!(
            (charged - 0.995 * 0.01 * total).abs() < 1.0,
            "charged {charged}"
        );
    }

    // #### merge-mining v2: the merge-mined token donation is all work
    // What: on jobs that merge-mine tokens, the token leaves pay the donation
    // on the whole setting's share of validated work (1.5%, 2% and 4.5%
    // here, across difficulties and start phases), while BCH's donation work
    // stays a third of it; before any share, the token window is the first
    // `rate` of the cycle and holds BCH's.
    // Look here if: `merge_token_donation_work` or the token balance changes.
    // #### merge-mining v2: a token without split support pays the fee in
    // work
    // What: on a public pool with a 2% fee (coinbase mode), such a token's
    // leaves pay the operator on the whole fee's share of what its 1.5%
    // donation leaves, while its donation work stays the whole 1.5%.
    // Look here if: `merge_token_fee_work` or the token fee balance changes.
    #[test]
    fn a_public_pool_fee_reaches_tokens_without_split_support_as_work() {
        use crate::donation::bch::{BchPayout, FeeMode, PoolFee};
        let rate = BchDonation::default();
        let fee = PoolFee {
            rate: "2".parse().unwrap(),
            mode: FeeMode::Coinbase,
        };
        let mut allocation = WorkAllocation::new(PERIOD_NS / 7);
        let (mut total, mut donated, mut charged) = (0.0, 0.0, 0.0);
        for n in 0..100_000u64 {
            let mut target = [255; 32];
            target[31] >>= (n / 137) % 5;
            let donation_work = allocation.donation_work(rate);
            let token_donation = donation_work || allocation.merge_token_donation_work(rate);
            let payout = BchPayout {
                donation: rate,
                donation_work,
                fee: Some(fee),
                token_donation_work: Some(token_donation),
                token_fee_work: Some(!token_donation && allocation.merge_token_fee_work(rate, fee)),
                ..BchPayout::default()
            };
            let work = super::super::telemetry::expected_hashes(&target);
            total += work;
            if payout.token_donation_work == Some(true) {
                donated += work;
            }
            if payout.token_fee_work == Some(true) {
                charged += work;
            }
            allocation.accepted(&target, payout);
        }
        assert!(
            (donated / total - 0.015).abs() < 0.0001,
            "{}",
            donated / total
        );
        let fee_share = (1.0 - 0.015) * 0.02;
        assert!(
            (charged / total - fee_share).abs() < 0.0001,
            "{}",
            charged / total
        );
        // Before any share: the window right after the token donation's.
        let donation = rate.pool_work_units(PERIOD_NS);
        assert!(!WorkAllocation::new(donation - 1).merge_token_fee_work(rate, fee));
        assert!(WorkAllocation::new(donation).merge_token_fee_work(rate, fee));
    }

    #[test]
    fn merge_mined_token_donation_work_is_the_whole_configured_rate() {
        use crate::donation::bch::BchPayout;
        for (rate, phase) in [("1.5", 0), ("2", PERIOD_NS / 2), ("4.5", PERIOD_NS - 1)] {
            let rate: BchDonation = rate.parse().unwrap();
            let configured = f64::from(u16::from(rate)) / 10_000.0;
            let mut allocation = WorkAllocation::new(phase);
            let (mut total, mut bch, mut token) = (0.0, 0.0, 0.0);
            for n in 0..100_000u64 {
                let mut target = [255; 32];
                target[31] >>= (n / 137) % 5;
                let donation_work = allocation.donation_work(rate);
                let payout = BchPayout {
                    donation: rate,
                    donation_work,
                    token_donation_work: Some(
                        donation_work || allocation.merge_token_donation_work(rate),
                    ),
                    ..BchPayout::default()
                };
                let work = super::super::telemetry::expected_hashes(&target);
                total += work;
                bch += if payout.donation_work { work } else { 0.0 };
                token += if payout.token_donation_work == Some(true) {
                    work
                } else {
                    0.0
                };
                allocation.accepted(&target, payout);
            }
            assert!(
                (token / total - configured).abs() < 0.0001,
                "{rate}: tokens {}",
                token / total
            );
            assert!(
                (bch / total - configured / 3.0).abs() < 0.0001,
                "{rate}: BCH {}",
                bch / total
            );
        }
        let rate = BchDonation::default();
        let window = rate.pool_work_units(PERIOD_NS);
        assert!(WorkAllocation::new(0).merge_token_donation_work(rate));
        assert!(WorkAllocation::new(window - 1).merge_token_donation_work(rate));
        assert!(!WorkAllocation::new(window).merge_token_donation_work(rate));
        assert!(rate.work_units(PERIOD_NS) < window);
        let none: BchDonation = "0".parse().unwrap();
        assert!(!WorkAllocation::new(0).merge_token_donation_work(none));
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
