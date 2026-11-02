//! #### merge-mining v2: the per-win split
//! What: a leaf's `ext` commits to how each win is shared: a donation slot,
//! then a fee slot, each a rate in hundredths of a percent and the
//! recipient's script hash.
//!
//! ```text
//! ext = 00 x 32                                   no split
//! ext = SHA256(03 ‖ d_bps u16le ‖ d_hash 32 ‖ f_bps u16le ‖ f_hash 32)
//! ```
//!
//! The 69-byte preimage is canonical: each rate is 0 to 10,000; a rate of 0
//! has a zero hash and a rate above 0 a non-zero one; two empty slots are
//! the zero `ext`, never a preimage. A hash is `HASH256` of the recipient's
//! locking bytecode, as the leaf's `payout_hash` is. A claim reveals the
//! preimage and the covenant checks it.
//!
//! Shares of a reward `R` (in u128): `d = floor(R × d_bps / 10000)`, then
//! `f = floor((R − d) × f_bps / 10000)`, and the miner keeps `R − d − f`.
//! A share worth less than [`MIN_SHARE_SATS`] at the vault's AMM price
//! counts as 0 and stays with the miner. The claim's outputs are the vault,
//! then the miner's remainder, the donation and the fee, each present only
//! when above 0; each of them carries exactly [`SPLIT_OUTPUT_SATS`] and
//! fungible tokens of the vault's category alone.
//! Why: a merge-mined token's donation (and a pool's fee) follows BCH's
//! formula: part of each win and part of the work.
//! Look here if: a split leaf, its shares or a claim's outputs disagree
//! with the covenant.

use super::{super::template::double_sha256, sha256, Hash};

/// The domain byte of a split preimage (a leaf's is 0x00, a node's 0x01, a
/// slot's 0x02).
pub const SPLIT_PREFIX: u8 = 0x03;
/// The preimage: the prefix and two slots of a rate and a hash.
pub const PREIMAGE_LEN: usize = 1 + 2 * (2 + 32);
/// The largest rate: the whole reward.
pub const MAX_BPS: u16 = 10_000;
/// A share worth less than this many satoshis at the AMM price is 0.
pub const MIN_SHARE_SATS: u64 = 1_000;
/// The satoshis on each claim output after the vault (the miner's, the
/// donation's and the fee's), exactly.
pub const SPLIT_OUTPUT_SATS: u64 = 800;
/// About how many bytes each split output adds to the claim, which the
/// vault's fee pays for.
pub const SPLIT_OUTPUT_BYTES: u64 = 90;

/// Why a split, its preimage or a claim's outputs are refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    /// A rate above 10,000.
    Rate,
    /// A rate above 0 with a zero hash.
    ZeroHash,
    /// A rate of 0 with a non-zero hash.
    StrayHash,
    /// Two empty slots written as a preimage: that is the zero `ext`.
    EmptyPreimage,
    /// Not 69 bytes starting with 0x03.
    Preimage,
    /// The claim's outputs are not the vault's, the miner's, the donation's
    /// and the fee's, in order, with exactly their shares.
    Outputs,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rate => "a split rate exceeds 100%",
            Self::ZeroHash => "a split slot with a rate has no recipient",
            Self::StrayHash => "an empty split slot names a recipient",
            Self::EmptyPreimage => "an empty split is the zero ext, not a preimage",
            Self::Preimage => "not a split preimage",
            Self::Outputs => "the claim's outputs do not follow its split",
        })
    }
}

impl std::error::Error for SplitError {}

/// One slot: a rate in hundredths of a percent and the recipient's script
/// hash, or empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    bps: u16,
    script_hash: Hash,
}

impl Slot {
    pub const EMPTY: Self = Self {
        bps: 0,
        script_hash: [0; 32],
    };

    /// The slot paying `bps` of what is left to the locking bytecode
    /// `script`; empty at 0.
    pub fn new(bps: u16, script: &[u8]) -> Result<Self, SplitError> {
        if bps == 0 {
            return Ok(Self::EMPTY);
        }
        Self::from_hash(bps, double_sha256(script))
    }

    /// A slot from its rate and script hash, checked canonical.
    pub fn from_hash(bps: u16, script_hash: Hash) -> Result<Self, SplitError> {
        if bps > MAX_BPS {
            return Err(SplitError::Rate);
        }
        match (bps == 0, script_hash == [0; 32]) {
            (false, true) => Err(SplitError::ZeroHash),
            (true, false) => Err(SplitError::StrayHash),
            _ => Ok(Self { bps, script_hash }),
        }
    }

    pub fn bps(&self) -> u16 {
        self.bps
    }

    pub fn script_hash(&self) -> &Hash {
        &self.script_hash
    }

    pub fn is_empty(&self) -> bool {
        self.bps == 0
    }

    /// `floor(left × bps / 10000)`, in u128.
    fn share(&self, left: u64) -> u64 {
        (u128::from(left) * u128::from(self.bps) / u128::from(MAX_BPS)) as u64
    }
}

/// A leaf's split: the donation slot, then the fee slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Split {
    donation: Slot,
    fee: Slot,
}

/// The shares of one reward, in token units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shares {
    pub miner: u64,
    pub donation: u64,
    pub fee: u64,
}

impl Split {
    /// No split: the zero `ext`.
    pub const NONE: Self = Self {
        donation: Slot::EMPTY,
        fee: Slot::EMPTY,
    };

    pub fn new(donation: Slot, fee: Slot) -> Self {
        Self { donation, fee }
    }

    /// The donation slot alone.
    pub fn donation(bps: u16, script: &[u8]) -> Result<Self, SplitError> {
        Ok(Self::new(Slot::new(bps, script)?, Slot::EMPTY))
    }

    pub fn donation_slot(&self) -> &Slot {
        &self.donation
    }

    pub fn fee_slot(&self) -> &Slot {
        &self.fee
    }

    pub fn is_none(&self) -> bool {
        self.donation.is_empty() && self.fee.is_empty()
    }

    /// The 69-byte preimage, also of an empty split (which is never
    /// written: its `ext` is zero).
    pub fn preimage(&self) -> [u8; PREIMAGE_LEN] {
        let mut bytes = [0; PREIMAGE_LEN];
        bytes[0] = SPLIT_PREFIX;
        bytes[1..3].copy_from_slice(&self.donation.bps.to_le_bytes());
        bytes[3..35].copy_from_slice(&self.donation.script_hash);
        bytes[35..37].copy_from_slice(&self.fee.bps.to_le_bytes());
        bytes[37..].copy_from_slice(&self.fee.script_hash);
        bytes
    }

    /// The leaf's `ext`: 32 zero bytes with no split, otherwise the
    /// preimage's SHA-256.
    pub fn ext(&self) -> Hash {
        if self.is_none() {
            [0; 32]
        } else {
            sha256(&self.preimage())
        }
    }

    /// Reads a revealed preimage as a covenant does: exactly 69 bytes, the
    /// prefix, two canonical slots, not both empty.
    pub fn parse(preimage: &[u8]) -> Result<Self, SplitError> {
        let bytes: &[u8; PREIMAGE_LEN] = preimage.try_into().map_err(|_| SplitError::Preimage)?;
        if bytes[0] != SPLIT_PREFIX {
            return Err(SplitError::Preimage);
        }
        let slot = |at: usize| {
            let bps = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
            let mut hash = [0; 32];
            hash.copy_from_slice(&bytes[at + 2..at + 34]);
            Slot::from_hash(bps, hash)
        };
        let split = Self::new(slot(1)?, slot(35)?);
        if split.is_none() {
            return Err(SplitError::EmptyPreimage);
        }
        Ok(split)
    }

    /// The raw shares of `reward`: the donation first, the fee from what
    /// it leaves, the rest to the miner.
    pub fn shares(&self, reward: u64) -> Shares {
        let donation = self.donation.share(reward);
        let fee = self.fee.share(reward - donation);
        Shares {
            miner: reward - donation - fee,
            donation,
            fee,
        }
    }

    // #### merge-mining v2: the minimum share
    // What: after the raw shares, a donation or fee share worth less than
    // MIN_SHARE_SATS at the AMM price is 0 and goes to the miner. The fee is
    // still computed from what the raw donation left, so dropping a small
    // donation never raises the fee.
    // Why: each split output costs the vault 800 satoshis and its bytes; a
    // tiny share would cost more than it is worth, and a 1-unit rate could
    // drain the vault.
    // Look here if: a claim pays a split output worth less than 1,000
    // satoshis, or the fee grows when the donation is dropped.
    /// The shares a claim pays, at the vault's AMM price.
    pub fn claim_shares(&self, reward: u64, amm: &Amm) -> Shares {
        let mut shares = self.shares(reward);
        for share in [&mut shares.donation, &mut shares.fee] {
            if *share > 0 && !amm.at_least_min(*share) {
                shares.miner += *share;
                *share = 0;
            }
        }
        shares
    }
}

/// The vault's AMM, which prices a token share: `share × (cash + virtual) /
/// tokens` satoshis, the covenant's formula.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Amm {
    /// The satoshis the vault holds for the AMM.
    pub cash: u64,
    /// The AMM's virtual satoshis.
    pub virtual_sats: u64,
    /// The tokens the AMM holds.
    pub tokens: u64,
}

impl Amm {
    /// What `share` tokens are worth in satoshis, rounded down (saturating
    /// at u64::MAX); `None` while the AMM holds no tokens, so nothing can be
    /// priced.
    pub fn value(&self, share: u64) -> Option<u64> {
        if self.tokens == 0 {
            return None;
        }
        // Above 2^128 the quotient is far above u64::MAX anyway.
        let sats = (u128::from(self.cash) + u128::from(self.virtual_sats))
            .checked_mul(u128::from(share))
            .map_or(u128::MAX, |product| product / u128::from(self.tokens));
        Some(u64::try_from(sats).unwrap_or(u64::MAX))
    }

    /// Whether `share` is worth at least [`MIN_SHARE_SATS`]; never while
    /// the AMM cannot price it.
    pub fn at_least_min(&self, share: u64) -> bool {
        self.value(share).is_some_and(|sats| sats >= MIN_SHARE_SATS)
    }
}

/// Who a claim output after the vault pays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Miner,
    Donation,
    Fee,
}

/// One claim output after the vault, as the covenant expects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedOutput {
    pub role: Role,
    pub script_hash: Hash,
    pub amount: u64,
}

/// One claim output as introspection shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenOutput {
    pub sats: u64,
    pub script_hash: Hash,
    pub category: Hash,
    /// An NFT's capability byte, if the output carries one.
    pub nft: Option<u8>,
    pub amount: u64,
}

/// The outputs after the vault, in order: the miner's remainder (paying
/// `payout_hash`), the donation and the fee, each only when above 0.
pub fn expected_outputs(split: &Split, payout_hash: Hash, shares: &Shares) -> Vec<ExpectedOutput> {
    [
        (Role::Miner, payout_hash, shares.miner),
        (Role::Donation, split.donation.script_hash, shares.donation),
        (Role::Fee, split.fee.script_hash, shares.fee),
    ]
    .into_iter()
    .filter(|(_, _, amount)| *amount > 0)
    .map(|(role, script_hash, amount)| ExpectedOutput {
        role,
        script_hash,
        amount,
    })
    .collect()
}

/// The split outputs a claim pays (the donation's and the fee's, when
/// above 0).
pub fn split_outputs(shares: &Shares) -> u64 {
    u64::from(shares.donation > 0) + u64::from(shares.fee > 0)
}

/// What the split outputs cost the vault at `sats_per_byte`: each one's
/// 800 satoshis and its bytes.
pub fn split_cost_sats(shares: &Shares, sats_per_byte: u64) -> u64 {
    split_outputs(shares) * (SPLIT_OUTPUT_SATS + SPLIT_OUTPUT_BYTES * sats_per_byte)
}

// #### merge-mining v2: 800 satoshis on every claim output
// What: the miner's output, like the donation's and the fee's, carries
// exactly 800 satoshis.
// Why: BCH P2Pool's SAFA covenant checks all three the same way; a miner
// output with more satoshis would take them from the vault.
// Look here if: a claim's miner output is refused for its satoshis.
/// Checks a claim's outputs after the vault against what its split
/// expects: the same count and order, each paying its script and exactly
/// its amount of the vault's fungible tokens, with no NFT, and each with
/// exactly 800 satoshis.
pub fn check_claim_outputs(
    expected: &[ExpectedOutput],
    outputs: &[TokenOutput],
    category: &Hash,
) -> Result<(), SplitError> {
    if expected.len() != outputs.len() {
        return Err(SplitError::Outputs);
    }
    for (want, got) in expected.iter().zip(outputs) {
        let fits = got.script_hash == want.script_hash
            && got.category == *category
            && got.nft.is_none()
            && got.amount == want.amount
            && got.amount >= 1
            && got.sats == SPLIT_OUTPUT_SATS;
        if !fits {
            return Err(SplitError::Outputs);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(byte: u8) -> Vec<u8> {
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend([byte; 20]);
        script.extend([0x88, 0xac]);
        script
    }

    fn split(d_bps: u16, f_bps: u16) -> Split {
        Split::new(
            Slot::new(d_bps, &script(0xd0)).unwrap(),
            Slot::new(f_bps, &script(0xfe)).unwrap(),
        )
    }

    fn shares(split: &Split, reward: u64) -> (u64, u64, u64) {
        let s = split.shares(reward);
        assert_eq!(s.miner + s.donation + s.fee, reward);
        (s.donation, s.fee, s.miner)
    }

    // #### merge-mining v2: golden vectors (repeated in docs/merge-mining.md)
    // What: shares at the extremes: rewards 1, 99, 100 and 2^63-1 at rates
    // 10,000 and 9,999 in both slots.
    #[test]
    fn shares_round_down_in_turn_at_the_extremes() {
        let max = (1u64 << 63) - 1;
        let all = split(10_000, 10_000);
        let most = split(9_999, 9_999);
        for (reward, at_all, at_most) in [
            (1, (1, 0, 0), (0, 0, 1)),
            (99, (99, 0, 0), (98, 0, 1)),
            (100, (100, 0, 0), (99, 0, 1)),
            (
                max,
                (max, 0, 0),
                (
                    9_222_449_699_651_090_329,
                    922_244_969_965_109,
                    92_233_720_369,
                ),
            ),
        ] {
            assert_eq!(shares(&all, reward), at_all, "{reward} at 10000");
            assert_eq!(shares(&most, reward), at_most, "{reward} at 9999");
        }
        // A donation that rounds to 0 beside a fee that does not.
        assert_eq!(shares(&split(100, 200), 99), (0, 1, 98));
        // A whole donation leaves no fee.
        assert_eq!(shares(&split(10_000, 500), 1_000_000), (1_000_000, 0, 0));
        // Both slots: 1% and then 1% of what is left.
        assert_eq!(
            shares(&split(100, 100), 1_000_000),
            (10_000, 9_900, 980_100)
        );
        // A zero miner remainder: half, then all the rest.
        assert_eq!(shares(&split(5_000, 10_000), 1_001), (500, 501, 0));
        // No split: all to the miner.
        assert_eq!(shares(&Split::NONE, 77), (0, 0, 77));
    }

    // #### merge-mining v2: the canonical slots and preimage
    #[test]
    fn slots_and_preimages_are_canonical() {
        assert_eq!(Slot::from_hash(10_001, [1; 32]), Err(SplitError::Rate));
        assert!(Slot::from_hash(10_000, [1; 32]).is_ok());
        assert_eq!(Slot::from_hash(1, [0; 32]), Err(SplitError::ZeroHash));
        assert_eq!(Slot::from_hash(0, [1; 32]), Err(SplitError::StrayHash));
        assert_eq!(Slot::from_hash(0, [0; 32]), Ok(Slot::EMPTY));
        assert_eq!(Slot::new(0, &script(1)), Ok(Slot::EMPTY));
        // No split is the zero ext, and its preimage is refused.
        assert_eq!(Split::NONE.ext(), [0; 32]);
        assert_eq!(
            Split::parse(&Split::NONE.preimage()),
            Err(SplitError::EmptyPreimage)
        );
        let both = split(100, 250);
        let preimage = both.preimage();
        assert_eq!(preimage.len(), 69);
        assert_eq!(preimage[0], 0x03);
        assert_eq!(preimage[1..3], [100, 0]);
        assert_eq!(preimage[3..35], double_sha256(&script(0xd0)));
        assert_eq!(preimage[35..37], [250, 0]);
        assert_eq!(preimage[37..], double_sha256(&script(0xfe)));
        assert_eq!(both.ext(), sha256(&preimage));
        assert_eq!(Split::parse(&preimage), Ok(both));
        // Another length or prefix, a rate above 10,000, a hash without a
        // rate and a rate without a hash.
        assert_eq!(Split::parse(&preimage[..68]), Err(SplitError::Preimage));
        let mut changed = preimage;
        changed[0] = 0x02;
        assert_eq!(Split::parse(&changed), Err(SplitError::Preimage));
        let mut changed = preimage;
        changed[1..3].copy_from_slice(&10_001u16.to_le_bytes());
        assert_eq!(Split::parse(&changed), Err(SplitError::Rate));
        let mut changed = preimage;
        changed[3..35].fill(0);
        assert_eq!(Split::parse(&changed), Err(SplitError::ZeroHash));
        let mut changed = preimage;
        changed[35..37].fill(0);
        assert_eq!(Split::parse(&changed), Err(SplitError::StrayHash));
        // The fee slot alone is a split too.
        let fee_only = Split::new(Slot::EMPTY, Slot::new(100, &script(0xfe)).unwrap());
        assert_eq!(Split::parse(&fee_only.preimage()), Ok(fee_only));
    }

    // #### merge-mining v2: the minimum share at the AMM price
    #[test]
    fn a_share_below_1000_sats_stays_with_the_miner() {
        // One satoshi per token: 1,000,000 sats (cash and virtual) for
        // 1,000,000 tokens.
        let amm = Amm {
            cash: 600_000,
            virtual_sats: 400_000,
            tokens: 1_000_000,
        };
        assert_eq!(amm.value(999), Some(999));
        assert!(!amm.at_least_min(MIN_SHARE_SATS - 1));
        assert!(amm.at_least_min(MIN_SHARE_SATS));
        // A 1% donation of 99,900 is 999 (dropped); of 100,000 it is 1,000.
        let donation = split(100, 0);
        let dropped = donation.claim_shares(99_900, &amm);
        assert_eq!(
            dropped,
            Shares {
                miner: 99_900,
                donation: 0,
                fee: 0
            }
        );
        let kept = donation.claim_shares(100_000, &amm);
        assert_eq!((kept.donation, kept.miner), (1_000, 99_000));
        // Dropping the donation leaves the fee as computed from the raw
        // remainder, never more.
        let both = split(100, 200);
        let raw = both.shares(99_900);
        let claimed = both.claim_shares(99_900, &amm);
        assert_eq!(claimed.fee, raw.fee);
        assert_eq!(claimed.donation, 0);
        assert_eq!(claimed.miner, raw.miner + raw.donation);
        // An AMM without tokens prices nothing: no split output.
        let empty = Amm { tokens: 0, ..amm };
        assert_eq!(empty.value(1), None);
        assert_eq!(both.claim_shares(1_000_000, &empty).miner, 1_000_000);
        // A huge share saturates instead of wrapping.
        let rich = Amm {
            cash: u64::MAX,
            virtual_sats: u64::MAX,
            tokens: 1,
        };
        assert_eq!(rich.value(u64::MAX), Some(u64::MAX));
    }

    // #### merge-mining v2: the claim's outputs
    #[test]
    fn claim_outputs_follow_the_split_in_order() {
        let category = [0xca; 32];
        let payout = double_sha256(&script(0x12));
        let both = split(100, 100);
        let shares = both.shares(1_000_000);
        let expected = expected_outputs(&both, payout, &shares);
        assert_eq!(
            expected
                .iter()
                .map(|o| (o.role, o.amount))
                .collect::<Vec<_>>(),
            [
                (Role::Miner, 980_100),
                (Role::Donation, 10_000),
                (Role::Fee, 9_900)
            ]
        );
        assert_eq!(split_outputs(&shares), 2);
        assert_eq!(split_cost_sats(&shares, 1), 2 * (800 + 90));
        let output = |want: &ExpectedOutput, sats| TokenOutput {
            sats,
            script_hash: want.script_hash,
            category,
            nft: None,
            amount: want.amount,
        };
        let good: Vec<_> = expected.iter().map(|want| output(want, 800)).collect();
        assert_eq!(check_claim_outputs(&expected, &good, &category), Ok(()));
        // The miner's output carries exactly 800 satoshis too.
        let mut rich = good.clone();
        rich[0].sats = 1_000;
        assert!(check_claim_outputs(&expected, &rich, &category).is_err());
        // Another order, a missing output, 801 sats, an NFT, another
        // category or amount.
        let mut swapped = good.clone();
        swapped.swap(1, 2);
        assert!(check_claim_outputs(&expected, &swapped, &category).is_err());
        assert!(check_claim_outputs(&expected, &good[..2], &category).is_err());
        let changes: [fn(&mut TokenOutput); 4] = [
            |o| o.sats = 801,
            |o| o.nft = Some(0),
            |o| o.category[0] ^= 1,
            |o| o.amount += 1,
        ];
        for change in changes {
            let mut bad = good.clone();
            change(&mut bad[1]);
            assert!(check_claim_outputs(&expected, &bad, &category).is_err());
        }
        // Zero shares have no output: a whole donation pays the donation
        // alone.
        let all = split(10_000, 100);
        let shares = all.shares(5);
        let expected = expected_outputs(&all, payout, &shares);
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].role, Role::Donation);
        assert_eq!(split_cost_sats(&shares, 1), 890);
    }
}
