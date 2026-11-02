//! #### PR #42: SAFA layout v1 (draft)
//! What: a Rust reference of the SAFA draft covenant, a CashToken whose
//! 80-byte NFT commitment is shaped like a block header so SHA-256 ASICs can
//! mine it: its parameters as data, the commitment's fields, the difficulty
//! adjustment in the covenant's integer order, its compact target encoding,
//! and the checks a claim must pass. No SAFA token is deployed, and nothing
//! mines it yet.
//! Why: the draft has changed three times in five days; a reference with
//! golden vectors lets a final version be checked against Pickaxe's jobs,
//! and it shows the draft's sign and freeze problems with numbers.
//! Look here if: a SAFA job or claim disagrees with the covenant.

use super::super::template::double_sha256;
use super::Hash;
use num_bigint::BigUint;

/// Header times must be above this (the covenant's `ts > 500,000,000`).
pub const MIN_TIME: u32 = 500_000_000;

/// Which bits of the version slot ("virtual") a miner may roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionRule {
    /// The draft as written: all four bytes are fixed. Most current ASICs
    /// roll BIP320 bits and cannot mine this at full speed.
    Fixed,
    /// BIP320's bits 13-28 roll; the scaler stays in bits 0-12.
    Bip320,
}

impl VersionRule {
    /// The bits that may differ from the thread's.
    pub fn mask(self) -> u32 {
        match self {
            Self::Fixed => 0,
            Self::Bip320 => 0x1fff_e000,
        }
    }
}

/// The byte order of the target slot (bytes 72..76).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactOrder {
    /// The canonical template: a 3-byte little-endian mantissa, then the
    /// exponent, as a block header's nBits.
    MantissaFirst,
    /// The `safa.cash` draft: the exponent first.
    ExponentFirst,
}

/// The difficulty adjustment by age (blocks since the thread last moved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Daa {
    /// `next = prev · (age · mul / div + base) / scale`, truncating at each
    /// step.
    Linear {
        mul: u64,
        div: u64,
        base: u64,
        scale: u64,
    },
}

/// Whether a header's time must follow the thread's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeRule {
    /// The draft: any time above 500,000,000 and below the claim's
    /// locktime.
    AnyPast,
    /// Later than the thread's own time.
    Monotonic,
}

/// How a claim's hash is compared with its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofRule {
    /// The draft as written: the hash read as a signed script number, so
    /// about half of all hashes (the negative ones) pass any target.
    Signed,
    /// What Pickaxe claims with: positive (byte 31 below 0x80) and, read
    /// unsigned, at or below the target.
    Positive,
}

/// One SAFA variant, as data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafaParams {
    pub version: VersionRule,
    pub compact: CompactOrder,
    pub daa: Daa,
    /// Satoshis a claim may take from the thread for its fee.
    pub fee_allowance_sats: u64,
    /// The share of the thread's tokens a claim releases (1 in this).
    pub emission_divisor: u128,
    pub max_age: u16,
    pub time: TimeRule,
}

/// The canonical template of 2026-09-24/29.
pub const CANONICAL: SafaParams = SafaParams {
    version: VersionRule::Fixed,
    compact: CompactOrder::MantissaFirst,
    daa: Daa::Linear {
        mul: 5_000,
        div: 71,
        base: 5_000,
        scale: 10_000,
    },
    fee_allowance_sats: 1_500,
    emission_divisor: 420_000,
    max_age: 65_534,
    time: TimeRule::AnyPast,
};

/// A SAFA commitment's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commitment {
    /// Bytes 0..4: the version slot.
    pub virtual_slot: u32,
    /// Bytes 4..36: HASH256 of the thread's previous commitment.
    pub prev: Hash,
    /// Bytes 36..68: HASH256 of the grinder's payout locking bytecode.
    pub payout: Hash,
    /// Bytes 68..72.
    pub time: u32,
    /// Bytes 72..76: the target, compact.
    pub bits: [u8; 4],
    /// Bytes 76..80.
    pub nonce: u32,
}

impl Commitment {
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let bytes: &[u8; 80] = bytes
            .try_into()
            .map_err(|_| "a SAFA commitment is exactly 80 bytes")?;
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Ok(Self {
            virtual_slot: word(0),
            prev: bytes[4..36].try_into().unwrap(),
            payout: bytes[36..68].try_into().unwrap(),
            time: word(68),
            bits: bytes[72..76].try_into().unwrap(),
            nonce: word(76),
        })
    }

    pub fn encode(&self) -> [u8; 80] {
        let mut bytes = [0; 80];
        bytes[..4].copy_from_slice(&self.virtual_slot.to_le_bytes());
        bytes[4..36].copy_from_slice(&self.prev);
        bytes[36..68].copy_from_slice(&self.payout);
        bytes[68..72].copy_from_slice(&self.time.to_le_bytes());
        bytes[72..76].copy_from_slice(&self.bits);
        bytes[76..].copy_from_slice(&self.nonce.to_le_bytes());
        bytes
    }
}

/// The target a compact slot names: `mantissa << 8·(exponent − 3)`, with
/// the exponent from 3 to 32 and a positive mantissa, as the covenant reads
/// it.
pub fn decode_compact(bits: [u8; 4], order: CompactOrder) -> Result<BigUint, &'static str> {
    let (mantissa, exponent) = match order {
        CompactOrder::MantissaFirst => ([bits[0], bits[1], bits[2]], bits[3]),
        CompactOrder::ExponentFirst => ([bits[1], bits[2], bits[3]], bits[0]),
    };
    if mantissa[2] & 0x80 != 0 {
        return Err("a negative SAFA target");
    }
    if !(3..=32).contains(&exponent) {
        return Err("a SAFA target exponent outside 3 to 32");
    }
    Ok(BigUint::from_bytes_le(&mantissa) << (8 * (u32::from(exponent) - 3)))
}

// #### PR #42: the freeze guard
// What: a target whose minimal script number takes 33 bytes (2^255 or
// more) is refused instead of encoded.
// Why: the covenant reads the exponent back with `0 ≤ exp < 33`, so a claim
// that stores exponent 33 makes every later claim on that thread fail: the
// thread's mining path freezes. At the draft's own 0x207fffff target, any
// age of 72 or more does this.
// Look here if: a claim is refused with "would freeze the thread".
/// The covenant's compact form of `target`: the 3 most significant bytes of
/// its minimal script number, then that number's length.
pub fn encode_compact(target: &BigUint, order: CompactOrder) -> Result<[u8; 4], &'static str> {
    let mut number = target.to_bytes_le();
    if number.last().is_some_and(|top| top & 0x80 != 0) {
        number.push(0);
    }
    if number.len() > 32 {
        return Err("the next target would freeze the thread");
    }
    if number.len() < 3 {
        return Err("the next target is too small to encode");
    }
    let length = number.len();
    let mantissa = &number[length - 3..];
    Ok(match order {
        CompactOrder::MantissaFirst => [mantissa[0], mantissa[1], mantissa[2], length as u8],
        CompactOrder::ExponentFirst => [length as u8, mantissa[0], mantissa[1], mantissa[2]],
    })
}

/// The next target after `age` blocks, in the covenant's integer order.
pub fn next_target(previous: &BigUint, age: u16, daa: Daa) -> BigUint {
    match daa {
        Daa::Linear {
            mul,
            div,
            base,
            scale,
        } => previous * (u64::from(age) * mul / div + base) / scale,
    }
}

// #### PR #42: the sign guard
// What: Pickaxe counts a hash as a win only when it is positive (byte 31
// below 0x80) and, read unsigned, at or below the target.
// Why: the draft reads the hash as a signed script number, so every
// negative hash, about half of all hashes, passes any target; its own
// "release" vector passes only that way. A claim that relies on this would
// stop working the day the covenant is fixed.
// Look here if: a SAFA win is refused although the draft would accept it.
/// Whether `hash` (HASH256 of the commitment) passes `target` by `rule`.
pub fn proof_passes(hash: &Hash, target: &BigUint, rule: ProofRule) -> bool {
    let negative = hash[31] & 0x80 != 0;
    match rule {
        ProofRule::Signed if negative => true,
        ProofRule::Positive if negative => false,
        _ => BigUint::from_bytes_le(hash) <= *target,
    }
}

/// Whether `win` (the new commitment) is a valid next step from the
/// thread's `current` commitment after `age` blocks, paid to `payout` (the
/// grinder's locking bytecode): the covenant's mining-path checks, with the
/// proof by the `Positive` rule.
pub fn verify(
    params: &SafaParams,
    current: &[u8; 80],
    win: &[u8; 80],
    age: u16,
    payout: &[u8],
) -> Result<(), &'static str> {
    let thread = Commitment::decode(current)?;
    let next = Commitment::decode(win)?;
    if age > params.max_age {
        return Err("the age is past the covenant's limit");
    }
    let target = next_target(
        &decode_compact(thread.bits, params.compact)?,
        age,
        params.daa,
    );
    let bits = encode_compact(&target, params.compact)?;
    if (next.virtual_slot ^ thread.virtual_slot) & !params.version.mask() != 0 {
        return Err("the version slot differs outside the token's rolling bits");
    }
    if next.prev != double_sha256(current) {
        return Err("the win does not follow the thread's commitment");
    }
    if next.payout != double_sha256(payout) {
        return Err("the win pays another script");
    }
    if next.time <= MIN_TIME {
        return Err("the win's time is not above 500,000,000");
    }
    if params.time == TimeRule::Monotonic && next.time <= thread.time {
        return Err("the win's time is not after the thread's");
    }
    if next.bits != bits {
        return Err("the win's target is not the thread's next");
    }
    if !proof_passes(&double_sha256(win), &target, ProofRule::Positive) {
        return Err("the win's hash is negative or above the target");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::bitcoin::Target;

    const RELEASE_OUTPUT: &str = "a0680600c132147de350e17ac23a00a22c0e7355aafb15f15b3eafcba5f0933cbc8cede3e20b366d1bd5587243263513cd4417468e0d5186c542d07bd832877e8ec0262055c33468ffff7f204d591331";
    /// The scenario's source commitment, 81 bytes in the template (a
    /// malformed fixture: a commitment is 80).
    const RELEASE_SOURCE: &str = "a0680600100dbb7a526f6058a167988e58e7cbdb8cff248d90969ecbdb8cff248d9096ffec83e7363e2d7273303f1cc73e03f233d884af8f048a9f8e9289c8eb51eaf0bf55c3346fffff7f201c4d591331";

    fn bits(value: u32) -> [u8; 4] {
        value.to_le_bytes()
    }

    // #### PR #42
    // What: the canonical template's "release" output decodes to its
    // fields: the scaler, HASH256 of the source commitment, the payout hash,
    // the claim's locktime as its time and target 0x207fffff.
    // Look here if: Commitment or decode_compact changes.
    #[test]
    fn forum_template_vectors_decode() {
        let output = hex::decode(RELEASE_OUTPUT).unwrap();
        let commitment = Commitment::decode(&output).unwrap();
        assert_eq!(commitment.encode().to_vec(), output);
        assert_eq!(commitment.virtual_slot, 0x0006_68a0);
        let source = hex::decode(RELEASE_SOURCE).unwrap();
        assert_eq!(source.len(), 81);
        assert!(Commitment::decode(&source).is_err());
        assert_eq!(commitment.prev, double_sha256(&source));
        assert_eq!(commitment.time, 1_748_288_341);
        assert_eq!(commitment.bits, bits(0x207f_ffff));
        assert_eq!(
            decode_compact(commitment.bits, CompactOrder::MantissaFirst).unwrap(),
            BigUint::from(0x7f_ffffu32) << 232
        );
    }

    // #### PR #42
    // What: the release vector's own hash ends in 0xbb, so as a script
    // number it is negative: the draft's signed compare accepts it, while
    // read unsigned it is above the target, and the Positive rule refuses
    // it.
    // Look here if: proof_passes changes.
    #[test]
    fn release_scenario_passes_only_through_the_sign_bug() {
        let output = hex::decode(RELEASE_OUTPUT).unwrap();
        let hash = double_sha256(&output);
        assert_eq!(
            hex::encode(hash),
            "d0c71dd33f13f1e0f1765c228b377da2264ce0e7750510eedafb343ce36ed3bb"
        );
        let target = decode_compact(bits(0x207f_ffff), CompactOrder::MantissaFirst).unwrap();
        assert!(proof_passes(&hash, &target, ProofRule::Signed));
        assert!(!proof_passes(&hash, &target, ProofRule::Positive));
        assert!(BigUint::from_bytes_le(&hash) > target);
    }

    // #### PR #42
    // What: the DAA from 0x1d00ffff at ages 0, 1, 71, 142 and 65534 gives
    // the targets the covenant's integer order gives (5000/71 truncates to
    // 70, so age 1 is ×0.507).
    // Look here if: next_target changes.
    #[test]
    fn daa_matches_the_covenant_integer_order() {
        let start = decode_compact(bits(0x1d00_ffff), CompactOrder::MantissaFirst).unwrap();
        for (age, expected) in [
            (0, 0x1c7f_ff80u32),
            (1, 0x1d00_81ca),
            (71, 0x1d00_ffff),
            (142, 0x1d01_7ffe),
            (65_534, 0x1e01_cdff),
        ] {
            let next = next_target(&start, age, CANONICAL.daa);
            assert_eq!(
                encode_compact(&next, CompactOrder::MantissaFirst).unwrap(),
                bits(expected),
                "age {age}"
            );
        }
    }

    // #### PR #42
    // What: the covenant's compact form equals Bitcoin's compact target for
    // every size from 3 to 32 bytes, and the two byte orders hold the same
    // fields.
    // Look here if: encode_compact changes.
    #[test]
    fn encode_target_equals_bitcoin_compact_for_sizes_3_to_32() {
        for size in 3..=32usize {
            for top in [0x01u8, 0x7f, 0x80, 0xff] {
                let mut bytes = [0u8; 32];
                for (index, byte) in bytes.iter_mut().enumerate().take(size) {
                    *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
                }
                bytes[size - 1] = top;
                if size == 32 && top >= 0x80 {
                    continue;
                }
                let target = BigUint::from_bytes_le(&bytes);
                let ours = encode_compact(&target, CompactOrder::MantissaFirst).unwrap();
                let bitcoin = Target::from_le_bytes(bytes)
                    .to_compact_lossy()
                    .to_consensus();
                assert_eq!(
                    u32::from_le_bytes(ours),
                    bitcoin,
                    "size {size}, top {top:#x}"
                );
                let other = encode_compact(&target, CompactOrder::ExponentFirst).unwrap();
                assert_eq!(other, [ours[3], ours[0], ours[1], ours[2]]);
                assert!(decode_compact(ours, CompactOrder::MantissaFirst).unwrap() <= target);
            }
        }
    }

    // #### PR #42
    // What: from 0x207fffff, age 71 keeps the target, and age 72 needs a
    // 33-byte script number (exponent 33), which is refused.
    // Look here if: the freeze guard changes.
    #[test]
    fn a_claim_that_would_freeze_the_thread_is_refused() {
        let start = decode_compact(bits(0x207f_ffff), CompactOrder::MantissaFirst).unwrap();
        assert_eq!(
            encode_compact(&next_target(&start, 71, CANONICAL.daa), CANONICAL.compact),
            Ok(bits(0x207f_ffff))
        );
        assert_eq!(
            encode_compact(&next_target(&start, 72, CANONICAL.daa), CANONICAL.compact),
            Err("the next target would freeze the thread")
        );
        assert!(decode_compact([0xff, 0xff, 0x7f, 33], CompactOrder::MantissaFirst).is_err());
    }

    /// A thread at an easy target, and a win on it from `payout` with the
    /// first nonce whose hash passes the Positive rule.
    fn solved(params: &SafaParams, payout: &[u8]) -> ([u8; 80], [u8; 80]) {
        let thread = Commitment {
            virtual_slot: 0x0000_0046,
            prev: [3; 32],
            payout: [4; 32],
            time: 1_700_000_000,
            bits: bits(0x207f_ffff),
            nonce: 0,
        }
        .encode();
        let target = next_target(
            &decode_compact(bits(0x207f_ffff), params.compact).unwrap(),
            71,
            params.daa,
        );
        let mut win = Commitment {
            virtual_slot: 0x0000_0046,
            prev: double_sha256(&thread),
            payout: double_sha256(payout),
            time: 1_700_000_100,
            bits: encode_compact(&target, params.compact).unwrap(),
            nonce: 0,
        };
        win.nonce = (0..10_000u32)
            .find(|nonce| {
                win.nonce = *nonce;
                proof_passes(&double_sha256(&win.encode()), &target, ProofRule::Positive)
            })
            .unwrap();
        (thread, win.encode())
    }

    // #### PR #42
    // What: a win on the thread verifies; changing the version slot, the
    // previous hash, the payout, a time at or below 500,000,000, the
    // target, a nonce whose hash fails the Positive rule, or an age past
    // the limit each fails.
    // Look here if: verify changes.
    #[test]
    fn verify_rejects_every_mutation() {
        let payout = [0x51u8];
        let (thread, win) = solved(&CANONICAL, &payout);
        assert_eq!(verify(&CANONICAL, &thread, &win, 71, &payout), Ok(()));
        let changed = |change: &dyn Fn(&mut Commitment)| {
            let mut commitment = Commitment::decode(&win).unwrap();
            change(&mut commitment);
            commitment.encode()
        };
        for (why, bad) in [
            ("version", changed(&|c| c.virtual_slot ^= 1)),
            ("previous", changed(&|c| c.prev[0] ^= 1)),
            ("payout", changed(&|c| c.payout[0] ^= 1)),
            ("time", changed(&|c| c.time = MIN_TIME)),
            ("target", changed(&|c| c.bits = bits(0x1d00_ffff))),
        ] {
            assert!(
                verify(&CANONICAL, &thread, &bad, 71, &payout).is_err(),
                "{why}"
            );
        }
        let target = decode_compact(bits(0x207f_ffff), CANONICAL.compact).unwrap();
        let failing = (0..10_000u32)
            .map(|nonce| changed(&|c| c.nonce = nonce))
            .find(|bad| !proof_passes(&double_sha256(bad), &target, ProofRule::Positive))
            .unwrap();
        assert_eq!(
            verify(&CANONICAL, &thread, &failing, 71, &payout),
            Err("the win's hash is negative or above the target")
        );
        assert!(verify(&CANONICAL, &thread, &win, 65_535, &payout).is_err());
        assert!(
            verify(&CANONICAL, &thread, &win, 70, &payout).is_err(),
            "another age has another target"
        );
    }

    // #### PR #42
    // What: under the BIP320 rule a win may roll bits 13-28 of the version
    // slot; the fixed rule refuses any change, and bits outside the mask
    // are refused by both.
    // Look here if: VersionRule changes.
    #[test]
    fn bip320_rule_accepts_rolled_bits_and_fixed_rule_refuses_them() {
        let rolling = SafaParams {
            version: VersionRule::Bip320,
            ..CANONICAL
        };
        let payout = [0x51u8];
        for (params, rolled, accepted) in [
            (&rolling, 0x0000_2000u32, true),
            (&rolling, 0x1fff_e000, true),
            (&rolling, 0x0000_0001, false),
            (&CANONICAL, 0x0000_2000, false),
        ] {
            let (thread, win) = solved(params, &payout);
            let mut rolled_win = Commitment::decode(&win).unwrap();
            rolled_win.virtual_slot ^= rolled;
            let target = decode_compact(bits(0x207f_ffff), params.compact).unwrap();
            rolled_win.nonce = (0..10_000u32)
                .find(|nonce| {
                    rolled_win.nonce = *nonce;
                    proof_passes(
                        &double_sha256(&rolled_win.encode()),
                        &target,
                        ProofRule::Positive,
                    )
                })
                .unwrap();
            assert_eq!(
                verify(params, &thread, &rolled_win.encode(), 71, &payout).is_ok(),
                accepted,
                "{rolled:#x}"
            );
        }
    }
}
