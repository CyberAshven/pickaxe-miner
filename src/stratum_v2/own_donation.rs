//! #### one donation, the higher rate: verified own donation
//! What a Public Solo Endpoint credits a miner whose own Pickaxe already
//! mines the Pickaxe donation on the same SV2 connection: the SV1 adapter's
//! donation channel, opened beside the device's own channel under the
//! donation's address. Only that channel's accepted work counts; what a
//! login says never does.

use crate::donation::bch::BchDonation;

/// Seconds in one bucket of the measuring window.
const BUCKET_SECONDS: u32 = 300;
/// Closed buckets the own donation is measured over: the last hour.
const WINDOW: usize = 12;
/// The ring: the open bucket and the closed ones.
const SLOTS: u32 = WINDOW as u32 + 1;

// #### one donation, the higher rate: verified own donation
// What: per connection, accepted work (expected hashes, the work allocation's
// measure) in five-minute buckets, the connection's donation channels' and
// all of its channels'. When a bucket closes, the own donation's share of the
// last hour's closed buckets becomes the credit for the next five minutes,
// rounded down to a hundredth of a percent. A bank keeps the credit honest:
// it gains each own donation share's work and loses, on each of the miner's
// shares, the work the server did not take because of the credit; a bucket
// that closes with the bank spent gives no credit, and the bank never holds
// more than the window's own donation work.
// Why: a miner's own Pickaxe donates through its own channel; the server
// takes only the rest of the effective rate, so the miner pays once. The
// bank bounds what the credit can ever be worth by the donation work the
// server saw, even for a client that varies its hash rate around the window.
// Look here if: a miner with their own Pickaxe pays the donation twice, or
// a credit outlives the donation work behind it.
/// The verified own donation of one connection.
#[derive(Clone, Debug, Default)]
pub(super) struct OwnDonation {
    /// Accepted work per bucket, (own donation, all), by bucket number
    /// modulo `SLOTS`.
    buckets: [(f64, f64); SLOTS as usize],
    /// The open bucket's number: unix time / `BUCKET_SECONDS`.
    open: Option<u32>,
    /// The own donation's share of the closed buckets' work, in hundredths
    /// of a percent, as measured when the open bucket began.
    measured: u16,
    /// Own donation work not yet used as credit.
    bank: f64,
}

impl OwnDonation {
    /// Records an accepted share worth `work` at unix time `now`: on a
    /// donation channel (`own`), or on the miner's own channel, where the
    /// server took `forgone` hundredths of a percent less than the
    /// effective rate because of the credit.
    pub fn record(&mut self, now: u32, work: f64, own: bool, forgone: u16) {
        if !work.is_finite() || work <= 0.0 {
            return;
        }
        let open = self.roll(now);
        let bucket = &mut self.buckets[(open % SLOTS) as usize];
        bucket.1 += work;
        if own {
            bucket.0 += work;
            self.bank += work;
        } else {
            self.bank -= work * f64::from(forgone) / 10_000.0;
        }
    }

    /// Moves to the bucket of `now` (never back), measuring the window
    /// when a new bucket opens; the open bucket's number.
    fn roll(&mut self, now: u32) -> u32 {
        let bucket = now / BUCKET_SECONDS;
        let Some(open) = self.open.filter(|open| bucket > *open) else {
            return *self.open.get_or_insert(bucket);
        };
        for step in 1..=(bucket - open).min(SLOTS) {
            self.buckets[((open + step) % SLOTS) as usize] = (0.0, 0.0);
        }
        self.open = Some(bucket);
        let skip = (bucket % SLOTS) as usize;
        let (own, all) = self
            .buckets
            .iter()
            .enumerate()
            .filter(|(slot, _)| *slot != skip)
            .fold((0.0, 0.0), |(own, all), (_, (o, a))| (own + o, all + a));
        self.bank = self.bank.min(own);
        self.measured = if all > 0.0 && self.bank > 0.0 {
            (own * 10_000.0 / all).floor().clamp(0.0, 10_000.0) as u16
        } else {
            0
        };
        bucket
    }

    /// The credit at the effective rate `effective`: the measured own
    /// donation, never above `effective`.
    pub fn credit(&self, effective: BchDonation) -> BchDonation {
        if self.measured < u16::from(effective) {
            BchDonation::try_from(self.measured).unwrap_or(effective)
        } else {
            effective
        }
    }
}

/// The rate the server takes on the miner's own channels, at the effective
/// rate `effective` with `credit` of the miner's work already donated on
/// their donation channel: the rest of the effective rate, as a share of
/// the work left on their own channels (rounded up), so that the two
/// together are the effective rate of all their work.
pub fn charged(effective: BchDonation, credit: BchDonation) -> BchDonation {
    let (whole, credit) = (
        u32::from(u16::from(effective)),
        u32::from(u16::from(credit)),
    );
    let rest = if credit >= whole {
        0
    } else {
        ((whole - credit) * 10_000)
            .div_ceil(10_000 - credit)
            .min(whole)
    };
    // `rest` is at most the effective rate, so it is always a rate.
    BchDonation::try_from(rest as u16).unwrap_or(effective)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(text: &str) -> BchDonation {
        text.parse().unwrap()
    }

    /// The start of a bucket.
    const START: u32 = 1_800_000_000 - 1_800_000_000 % BUCKET_SECONDS;

    /// `seconds` of shares worth 1 each from `start`, every `every`th on the
    /// donation channel (none at 0), with the server charging the rest of
    /// `effective` after the credit in force; where it ends.
    fn mine(
        meter: &mut OwnDonation,
        start: u32,
        seconds: u32,
        every: u32,
        effective: BchDonation,
    ) -> u32 {
        for second in 0..seconds {
            let own = every != 0 && second % every == 0;
            let taken = charged(effective, meter.credit(effective));
            let forgone = u16::from(effective) - u16::from(taken);
            meter.record(start + second, 1.0, own, forgone);
        }
        start + seconds
    }

    // #### one donation, the higher rate: verified own donation
    // What: host 2%, the miner's own Pickaxe visibly mines 1.5%: once a
    // bucket has closed the credit is 1.5%, and the host takes 0.51% of the
    // work on the miner's own channels, which is 0.5% of all of it, so the
    // two make 2%. A login alone is credited nothing, and the credit never
    // exceeds the effective rate.
    #[test]
    fn visible_own_donation_is_credited_up_to_the_effective_rate() {
        let host = rate("2");
        assert_eq!(u16::from(charged(host, rate("1.5"))), 51);
        assert_eq!(charged(host, rate("0")), host);
        assert_eq!(u16::from(charged(host, host)), 0);
        assert_eq!(u16::from(charged(rate("100"), rate("100"))), 0);
        // 1.5% donated plus 0.51% of the other 98.5% is 2% (rounded up).
        let total = 150.0 + 51.0 * (1.0 - 0.015);
        assert!((200.0..200.5).contains(&total), "{total}");
        let mut meter = OwnDonation::default();
        meter.record(START, 1.5, true, 0);
        meter.record(START + 1, 98.5, false, 0);
        assert_eq!(u16::from(meter.credit(host)), 0, "before a bucket closes");
        meter.roll(START + BUCKET_SECONDS);
        assert_eq!(u16::from(meter.credit(host)), 150);
        assert_eq!(u16::from(charged(host, meter.credit(host))), 51);
        // A host at 1% credits 1% and takes nothing more.
        assert_eq!(meter.credit(rate("1")), rate("1"));
        assert_eq!(u16::from(charged(rate("1"), meter.credit(rate("1")))), 0);
        // `,pickaxe-donation=0` with no donation work: no credit.
        let mut plain = OwnDonation::default();
        let end = mine(&mut plain, START, BUCKET_SECONDS * 3, 0, host);
        plain.roll(end);
        assert_eq!(u16::from(plain.credit(host)), 0);
        assert_eq!(charged(host, plain.credit(host)), host);
    }

    // #### one donation, the higher rate: verified own donation
    // What: the credit follows the last hour of work: a miner whose own
    // donation (4%, above the host's 2%) stops is credited the host's whole
    // rate while the hour still holds 2%, less after, and nothing once the
    // hour holds none. A bank spent on credit gives none at all, whatever
    // share was measured.
    #[test]
    fn the_credit_ends_with_the_donation_work_behind_it() {
        let host = rate("2");
        let mut meter = OwnDonation::default();
        let now = mine(&mut meter, START, 3600, 25, host);
        meter.roll(now);
        assert_eq!(u16::from(meter.credit(host)), 200);
        // Three quarters of an hour without: a quarter of the 4% is left.
        let now = mine(&mut meter, now, 2700, 0, host);
        meter.roll(now);
        assert_eq!(u16::from(meter.credit(host)), 100);
        assert!(meter.bank > 0.0);
        let now = mine(&mut meter, now, 900, 0, host);
        meter.roll(now);
        assert_eq!(u16::from(meter.credit(host)), 0);
        // One own share worth 1, then a share worth 100 charged 2% less:
        // the bank is spent, so the next bucket gives no credit.
        let mut spent = OwnDonation::default();
        spent.record(START, 1.0, true, 0);
        spent.record(START + 1, 1.0, false, 0);
        spent.roll(START + BUCKET_SECONDS);
        assert_eq!(u16::from(spent.credit(host)), 200);
        spent.record(START + BUCKET_SECONDS + 1, 100.0, false, 200);
        spent.roll(START + 2 * BUCKET_SECONDS);
        assert_eq!(u16::from(spent.credit(host)), 0);
        // A clock that steps back stays in the open bucket.
        spent.record(START, 1.0, true, 0);
        assert_eq!(spent.open, Some(START / BUCKET_SECONDS + 2));
    }
}
