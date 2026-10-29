//! #### merge-mining v2: tSAFA, the SAFA AuxPoW covenant's claims
//! What: what Pickaxe needs to claim a merge-mined win from BCH P2Pool's
//! SAFA AuxPoW covenant (the merge-mined variant of the SAFA draft): its
//! deployment, read from a file; a thread's 16-byte state; every rule a
//! claim must pass, in the covenant's own integer order; the one claim
//! transaction it accepts; and an independent check of a claim's bytes
//! before it is broadcast. Written from the covenant and its rules, not
//! copied: Pickaxe has no BCH script VM, so the check below re-reads the
//! transaction the way the covenant's introspection does.
//!
//! A vault (one per thread) is a P2SH32 output of the covenant holding
//! fungible tokens, satoshis and one mutable NFT whose commitment is the
//! thread's state:
//!
//! ```text
//! off len field
//! 0   4   thread         u32 LE
//! 4   4   state_height   u32 LE, the coinbase height of the last claim
//! 8   4   target_bits    u32 LE, the token's canonical compact target
//! 12  4   ref_bits       u32 LE, the last claim header's nBits
//! ```
//!
//! A claim spends the vault alone. Its unlocking bytecode pushes the split
//! preimage (69 bytes, or nothing), `payout_hash` (32 bytes), the
//! `AuxProof` v2 and the redeem script. Its outputs are the vault (same
//! script, category and mutable NFT, the next state, the tokens less the
//! reward, the satoshis less the fee and 800 per other output), then the
//! miner's, the donation's and the fee's shares, each only above 0 and
//! each with exactly 800 satoshis. Its lock time is a height at or above
//! the coinbase height, with a non-final sequence (CLTV).
//! Why: a win is worth nothing until its claim is mined, and a claim the
//! covenant refuses loses the win.
//! Look here if: a node refuses a tSAFA claim, or a deployment file is
//! refused.

use super::{
    super::template::double_sha256,
    commitment::check_head,
    leaf::Leaf,
    proof::AuxProof,
    split::{check_claim_outputs, expected_outputs, Amm, Shares, Split, TokenOutput},
    verify::verify_leaf,
    Hash, OutPoint,
};
use crate::config::MiningNetwork;
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};
use sha2::{Digest, Sha256};

/// A claim pays `floor(vault tokens / 420,000)`.
pub const REWARD_DIVISOR: u64 = 420_000;
/// Satoshis on every claim output after the vault.
pub const OUTPUT_SATS: u64 = super::split::SPLIT_OUTPUT_SATS;
/// The DAA eases the target by at most this many blocks per claim.
pub const EASE_CAP: u32 = 288;
/// Blocks without a claim after which the fallback nBits band applies.
pub const FALLBACK_DELTA: u32 = 4_032;
/// A claim's bytes besides the proof and the redeem script, at their
/// largest: version 4, input count 1, outpoint 36, unlocking length 3,
/// sequence 4, output count 1, the vault output 104, three token outputs of
/// 87 (to scripts of at most 35 bytes), lock time 4, the split push 70, the
/// payout hash push 33, the proof's push prefix 3, and the 53-byte
/// commitment output the proof leaves out.
pub const CLAIM_FIXED_BYTES: u64 = 4 + 1 + 36 + 3 + 4 + 1 + 104 + 3 * 87 + 4 + 70 + 33 + 3 + 53;
/// BCH's standard unlocking bytecode limit.
pub const MAX_UNLOCKING_BYTES: usize = 10_000;
/// Lock times at or above this are times, not heights.
pub const LOCKTIME_THRESHOLD: u32 = 500_000_000;
/// The longest payout script `K` allows for (P2PKH, P2SH20 or P2SH32).
pub const MAX_PAYOUT_SCRIPT: usize = 35;
pub const CLAIM_VERSION: u32 = 2;
/// Non-final, so the lock time (CLTV) applies.
pub const CLAIM_SEQUENCE: u32 = 0xffff_fffe;
/// The vault pays from 1 to 3 satoshis per byte of the size bound.
pub const MAX_FEE_RATE: u64 = 3;
pub const STATE_LEN: usize = 16;
/// The covenant's tallest aux tree.
const MAX_AUX_HEIGHT: u8 = 16;
/// `GetCompact` of the proof-of-work limit, 2^224 - 1.
const POW_LIMIT_BITS: u32 = 0x1d00_ffff;
const MAX_VIRTUAL_SATS: u64 = 1 << 62;
// #### At most 63 threads, as BCH P2Pool's deployment tooling allows: well
// under 288, where even the longest wait (Δh 288) cannot ease a thread's
// target (`⌊288 × 5000 / threads⌋ + 5000 <= 10000`) and the vault freezes.
// A tooling check on both sides, not a covenant rule.
const MAX_THREADS: u64 = 63;
const MAX_DEPLOYMENT_FILE: u64 = 1 << 20;
/// A mutable NFT's capability byte.
const MUTABLE: u8 = 0x01;

/// The covenant's ASERT parameters per network: BCHN's anchor and
/// half-life, and the band P2Pool chose below the computed target.
#[derive(Clone, Copy, Debug)]
struct Band {
    anchor_height: u32,
    anchor_bits: u32,
    anchor_parent_time: i64,
    half_life: i64,
    allow_pow_limit: bool,
    hard_divisor: u32,
    /// The easy side, `computed × easy.0 / easy.1`.
    easy: (u32, u32),
}

fn band_of(network: MiningNetwork) -> Band {
    match network {
        MiningNetwork::Mainnet => Band {
            anchor_height: 661_647,
            anchor_bits: 0x1804_dafe,
            anchor_parent_time: 1_605_447_844,
            half_life: 172_800,
            allow_pow_limit: false,
            hard_divisor: 2,
            easy: (105, 100),
        },
        MiningNetwork::Chipnet => Band {
            anchor_height: 16_844,
            anchor_bits: 0x1d00_ffff,
            anchor_parent_time: 1_605_451_779,
            half_life: 3_600,
            allow_pow_limit: true,
            hard_divisor: 8,
            easy: (8, 1),
        },
    }
}

fn pow_limit() -> BigUint {
    (BigUint::one() << 224u32) - 1u32
}

/// The easiest canonical token target, `0x7fffff << 232` (bits 0x207fffff).
fn max_token_target() -> BigUint {
    BigUint::from(0x007f_ffffu32) << 232u32
}

// ---- targets ----

/// A canonical token target: `3 <= exp <= 32` and `0 < mant < 0x800000`.
pub fn expand_token_target(bits: u32) -> Option<BigUint> {
    let exponent = bits >> 24;
    let mantissa = bits & 0x00ff_ffff;
    if !(3..=32).contains(&exponent) || mantissa == 0 || mantissa >= 0x0080_0000 {
        return None;
    }
    Some(BigUint::from(mantissa) << (8 * (exponent - 3)))
}

/// Canonical token bits for a target from 1 to `0x7fffff << 232`, rounded
/// down as the covenant does: the size is the target's length as a script
/// number (a sign byte included), at least 3.
pub fn encode_token_target(target: &BigUint) -> Option<u32> {
    if target.is_zero() || *target > max_token_target() {
        return None;
    }
    let size = (target.bits() + 1).div_ceil(8).max(3);
    let mantissa = (target >> (8 * (size - 3))).to_u32()?;
    Some(u32::try_from(size).ok()? << 24 | mantissa)
}

/// The claim's target bits: the state's target eased by the blocks since
/// the last claim, `target × (⌊min(dh, 288) × 5000 / threads⌋ + 5000) /
/// 10000`, clamped to 1 and `0x7fffff << 232`. They are also the next
/// state's.
pub fn claim_target_bits(state_bits: u32, height_delta: u32, threads: u32) -> Option<u32> {
    if threads == 0 || height_delta == 0 {
        return None;
    }
    let factor = u64::from(height_delta.min(EASE_CAP)) * 5_000 / u64::from(threads) + 5_000;
    let mut target = expand_token_target(state_bits)? * factor / 10_000u32;
    if target.is_zero() {
        target = BigUint::one();
    }
    let max = max_token_target();
    if target > max {
        target = max;
    }
    encode_token_target(&target)
}

/// A header's nBits as the covenant reads them (BCHN's `SetCompact`):
/// `None` for a negative, zero or above-the-limit target.
pub fn decode_header_bits(bits: u32) -> Option<BigUint> {
    let mantissa = bits & 0x00ff_ffff;
    if mantissa >= 0x0080_0000 {
        return None;
    }
    let shift = (i64::from(bits >> 24) - 3) * 8;
    if shift > 256 {
        return None;
    }
    let value = if shift < 0 {
        BigUint::from(mantissa) >> u32::try_from(-shift).ok()?
    } else {
        BigUint::from(mantissa) << u32::try_from(shift).ok()?
    };
    (!value.is_zero() && value <= pow_limit()).then_some(value)
}

/// BCHN's `CalculateASERT`, integer for integer as the covenant computes
/// it: a truncating division for the exponent, a flooring shift for its
/// integer part, BCHN's cubic for the fraction, then the clamps to
/// `[1, 2^224 - 1]`. Unlike BCHN it has no bound on the time span (the
/// covenant has none).
fn asert_target(reference: &BigUint, time_diff: i64, height_diff: i64, half_life: i64) -> BigUint {
    let span = i128::from(time_diff) - 600 * i128::from(height_diff);
    // #### merge-mining v2: BCHN's two roundings, as the covenant does them
    // What: the exponent's division truncates toward zero (C++ int64 `/`,
    // and Rust's), then `shifts = exponent >> 16` is an arithmetic shift
    // (floor) and `frac = exponent & 0xffff`.
    // Why: BCH P2Pool's covenant and BCHN both round this way; floor
    // division gives another target for a negative exponent that the half
    // life does not divide.
    // Look here if: a header near the band's edge is judged otherwise than
    // the covenant judges it.
    let exponent = (span - 600) * 65_536 / i128::from(half_life);
    let shifts = exponent >> 16;
    let fraction = u128::try_from(exponent & 0xffff).unwrap_or(0);
    let factor = 65_536
        + ((195_766_423_245_049 * fraction
            + 971_821_376 * fraction * fraction
            + 5_127 * fraction * fraction * fraction
            + (1 << 47))
            >> 48);
    let next = reference * BigUint::from(factor);
    let shift = shifts - 16;
    let next = if shift <= 0 {
        next >> u32::try_from((-shift).min(256)).unwrap_or(256)
    } else {
        next << u32::try_from(shift.min(256)).unwrap_or(256)
    };
    if next.is_zero() {
        return BigUint::one();
    }
    next.min(pow_limit())
}

// #### merge-mining v2: the BCH-only checks
// What: a claim's header nBits must sit in the ASERT band at its coinbase
// height (its own time standing in for its parent's): mainnet
// `computed / 2 <= target <= computed × 1.05`, Chipnet `computed / 8` to
// `computed × 8` or exactly the limit's bits. (On a test network a header
// may be dated up to 2 hours before its parent, which moves ASERT's target
// far more than on mainnet, where 2 hours is only × 1.029: × 1.05 refused
// Chipnet headers dated over about 253 seconds before their parent.) After
// 4,032 blocks without
// a claim a header outside it may still pass the fallback band around the
// state's `ref_bits`: `ref >> 4 <= target <= ref × 1.1`.
// Why: so only BCH-shaped headers mine a thread, while a change to BCH's
// difficulty algorithm cannot freeze one.
// Look here if: a claim is refused for its header's nBits.
/// The ASERT band at coinbase height `height` for a header's `bits` and
/// `time`: `None` where the covenant's script fails outright (invalid
/// nBits, a height at or below the anchor).
pub fn asert_band(network: MiningNetwork, bits: u32, time: u32, height: u32) -> Option<bool> {
    let band = band_of(network);
    let target = decode_header_bits(bits)?;
    if band.allow_pow_limit && bits == POW_LIMIT_BITS {
        return Some(true);
    }
    let height_diff = i64::from(height) - 1 - i64::from(band.anchor_height);
    if height_diff < 0 {
        return None;
    }
    let reference = decode_header_bits(band.anchor_bits)?;
    let computed = asert_target(
        &reference,
        i64::from(time) - band.anchor_parent_time,
        height_diff,
        band.half_life,
    );
    Some(target >= &computed / band.hard_divisor && target * band.easy.1 <= computed * band.easy.0)
}

/// The DAA-change fallback band, which applies from 4,032 blocks without a
/// claim: `ref >> 4 <= target <= ref × 1.1` (`None` where either nBits is
/// invalid).
pub fn fallback_band(bits: u32, ref_bits: u32) -> Option<bool> {
    let reference = decode_header_bits(ref_bits)?;
    let target = decode_header_bits(bits)?;
    Some(target >= (&reference >> 4u32) && target * 10u32 <= reference * 11u32)
}

/// Whether a header at `height`, `height_delta` blocks after the state,
/// passes the BCH-only checks.
fn bch_header(network: MiningNetwork, header: &[u8; 80], height: u32, state: &ThreadState) -> bool {
    let time = u32::from_le_bytes([header[68], header[69], header[70], header[71]]);
    let bits = header_bits(header);
    let delta = height.saturating_sub(state.height);
    match asert_band(network, bits, time, height) {
        Some(true) => true,
        Some(false) => delta >= FALLBACK_DELTA && fallback_band(bits, state.ref_bits) == Some(true),
        None => false,
    }
}

fn header_bits(header: &[u8; 80]) -> u32 {
    u32::from_le_bytes([header[72], header[73], header[74], header[75]])
}

/// The BIP34 height of a fixed-shape coinbase head, as the covenant reads
/// it: a minimal, positive push of 1 to 5 bytes at the start of its script.
pub fn coinbase_height(head: &[u8]) -> Option<u64> {
    check_head(head).ok()?;
    let length = usize::from(head[41]);
    let push = usize::from(*head.get(42)?);
    if !(1..=5).contains(&push) || length < 1 + push {
        return None;
    }
    let bytes = head.get(43..43 + push)?;
    let last = bytes[push - 1];
    if last & 0x80 != 0 || (last == 0 && (push == 1 || bytes[push - 2] & 0x80 == 0)) {
        return None;
    }
    Some(
        bytes
            .iter()
            .rev()
            .fold(0u64, |value, byte| value << 8 | u64::from(*byte)),
    )
}

// #### merge-mining v2: which thread a leaf mines
// What: `thread = u32le(HASH256(payout_hash ‖ cb_height u32le)[0..4]) mod
// threads`; when that thread cannot be mined (its state is not older than
// the height, its state is already won, or its vault cannot pay a claim),
// the next index mod threads is tried, and so on.
// Why: agreed with BCH P2Pool so miners spread over the threads instead of
// all racing for one, and so a miner's thread follows from what its leaf
// binds alone.
// Look here if: a job's tSAFA leaf names another thread than this rule.
/// The first thread a leaf paying `payout_hash` at coinbase height
/// `height` tries.
pub fn thread_start(payout_hash: &Hash, height: u32, threads: u32) -> u32 {
    let mut seed = [0u8; 36];
    seed[..32].copy_from_slice(payout_hash);
    seed[32..].copy_from_slice(&height.to_le_bytes());
    let digest = double_sha256(&seed);
    u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) % threads.max(1)
}

// ---- thread state ----

/// A thread's state, its vault NFT's 16-byte commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadState {
    pub thread: u32,
    pub height: u32,
    pub target_bits: u32,
    pub ref_bits: u32,
}

impl ThreadState {
    pub fn parse(commitment: &[u8]) -> Option<Self> {
        let bytes: &[u8; STATE_LEN] = commitment.try_into().ok()?;
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        Some(Self {
            thread: word(0),
            height: word(4),
            target_bits: word(8),
            ref_bits: word(12),
        })
    }

    pub fn encode(&self) -> [u8; STATE_LEN] {
        let mut bytes = [0; STATE_LEN];
        bytes[..4].copy_from_slice(&self.thread.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.height.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.target_bits.to_le_bytes());
        bytes[12..].copy_from_slice(&self.ref_bits.to_le_bytes());
        bytes
    }
}

// ---- deployment ----

// #### merge-mining v2: the tSAFA deployment file
// What: one JSON object naming a Chipnet deployment of the covenant:
//
//   {"version": 1, "name": "tSAFA", "network": "chipnet",
//    "category": "<txid of the genesis input, display order>",
//    "threads": 4, "virtual_sats": "84000000000",
//    "redeem_script": "<hex>"}
//
// with optional checks P2Pool's tool also prints: "fee_k",
// "redeem_script_bytes", "locking_bytecode" and "lock_hash". The redeem
// script fixes the covenant; Pickaxe derives its P2SH32 lock and `K` from
// it, and refuses a script that does not carry this file's `K`, virtual
// reserve and reward divisor.
// Why: tSAFA is not broadcast yet, so nothing about it is compiled in, and
// mainnet has no SAFA row at all.
// Look here if: `--safa-deployment` refuses a file.
/// A covenant deployment: its network, category, threads, AMM reserve and
/// redeem script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub name: String,
    pub network: MiningNetwork,
    /// The token category, in internal byte order (as the covenant and a
    /// leaf read it).
    pub category: Hash,
    pub threads: u32,
    /// The AMM's virtual satoshis.
    pub virtual_sats: u64,
    pub redeem_script: Vec<u8>,
    /// `OP_HASH256 <HASH256(redeem)> OP_EQUAL`, every vault's script.
    pub locking_bytecode: Vec<u8>,
    /// `HASH256` of the locking bytecode: the leaves' `lock_hash`.
    pub lock_hash: Hash,
    /// `K = 577 + the redeem script's push`: the claim's size bound is
    /// `K + |proof|`.
    pub fee_k: u64,
}

impl Deployment {
    /// Reads a deployment file (at most 1 MiB).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|error| format!("the SAFA deployment file cannot be read: {error}"))?;
        let mut text = String::new();
        file.take(MAX_DEPLOYMENT_FILE + 1)
            .read_to_string(&mut text)
            .map_err(|error| format!("the SAFA deployment file cannot be read: {error}"))?;
        if text.len() as u64 > MAX_DEPLOYMENT_FILE {
            return Err("the SAFA deployment file exceeds 1 MiB".into());
        }
        Self::from_json(&text)
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        use serde_json::Value;
        let value: Value = serde_json::from_str(text)
            .map_err(|error| format!("the SAFA deployment file is not JSON: {error}"))?;
        let object = value
            .as_object()
            .ok_or("a SAFA deployment file is one JSON object")?;
        if object.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("a SAFA deployment file needs \"version\": 1".into());
        }
        let network = match object.get("network").and_then(Value::as_str) {
            Some("chipnet") => MiningNetwork::Chipnet,
            Some("mainnet") => {
                return Err(
                    "tSAFA is mined on Chipnet only; no SAFA token is mined on mainnet".into(),
                )
            }
            _ => return Err("the SAFA deployment's network must be \"chipnet\"".into()),
        };
        let name = match object.get("name") {
            None => "tSAFA".to_owned(),
            Some(Value::String(name))
                if !name.trim().is_empty()
                    && name.len() <= 32
                    && !name.chars().any(char::is_control) =>
            {
                name.clone()
            }
            Some(_) => {
                return Err(
                    "the SAFA deployment's name must be 1 to 32 printable characters".into(),
                )
            }
        };
        let category = object
            .get("category")
            .and_then(Value::as_str)
            .and_then(|text| hex::decode(text.trim()).ok())
            .and_then(|bytes| <Hash>::try_from(bytes).ok())
            .filter(|bytes| *bytes != [0; 32])
            .ok_or("the SAFA deployment's category must be its 64-digit txid")?;
        let mut category = category;
        category.reverse();
        let threads = object
            .get("threads")
            .and_then(Value::as_u64)
            .filter(|threads| (1..=MAX_THREADS).contains(threads))
            .ok_or("the SAFA deployment's threads must be from 1 to 63")?;
        let virtual_sats = match object.get("virtual_sats") {
            Some(Value::Number(number)) => number.as_u64(),
            Some(Value::String(text)) if text.bytes().all(|byte| byte.is_ascii_digit()) => {
                text.parse::<u64>().ok()
            }
            _ => None,
        }
        .filter(|sats| *sats <= MAX_VIRTUAL_SATS)
        .ok_or("the SAFA deployment's virtual_sats must be a whole number from 0 to 2^62")?;
        let redeem_script = object
            .get("redeem_script")
            .and_then(Value::as_str)
            .and_then(|text| hex::decode(text.trim()).ok())
            .filter(|bytes| !bytes.is_empty() && bytes.len() < MAX_UNLOCKING_BYTES)
            .ok_or("the SAFA deployment's redeem_script must be the covenant's hex bytes")?;
        let mut locking_bytecode = vec![0xaa, 0x20];
        locking_bytecode.extend_from_slice(&double_sha256(&redeem_script));
        locking_bytecode.push(0x87);
        let lock_hash = double_sha256(&locking_bytecode);
        let fee_k =
            CLAIM_FIXED_BYTES + push_prefix_len(redeem_script.len()) + redeem_script.len() as u64;
        // P2Pool's tool prints these beside the redeem script; when the
        // file carries them they must agree.
        let mismatch =
            |field: &str| format!("the SAFA deployment's {field} does not match its redeem script");
        if let Some(value) = object.get("fee_k") {
            if value.as_u64() != Some(fee_k) {
                return Err(mismatch("fee_k"));
            }
        }
        if let Some(value) = object.get("redeem_script_bytes") {
            if value.as_u64() != Some(redeem_script.len() as u64) {
                return Err(mismatch("redeem_script_bytes"));
            }
        }
        if let Some(value) = object.get("locking_bytecode") {
            if value.as_str().map(str::to_ascii_lowercase) != Some(hex::encode(&locking_bytecode)) {
                return Err(mismatch("locking_bytecode"));
            }
        }
        if let Some(value) = object.get("lock_hash") {
            if value.as_str().map(str::to_ascii_lowercase) != Some(hex::encode(lock_hash)) {
                return Err(mismatch("lock_hash"));
            }
        }
        // The covenant pushes its K, its virtual reserve, its reward
        // divisor and the proof's magic; a script without them is not this
        // deployment's covenant.
        let carries = |needle: &[u8]| {
            redeem_script
                .windows(needle.len())
                .any(|window| window == needle)
        };
        let mut magic = vec![0x05];
        magic.extend_from_slice(b"CTMP\x02");
        for (needle, what) in [
            (number_push(fee_k), "its fee constant K"),
            (number_push(virtual_sats), "the virtual reserve"),
            (number_push(REWARD_DIVISOR), "the reward divisor"),
            (magic, "the AuxProof v2 magic"),
        ] {
            if !carries(&needle) {
                return Err(format!(
                    "the SAFA deployment's redeem script does not carry {what}; is it this deployment's covenant?"
                ));
            }
        }
        Ok(Self {
            name,
            network,
            category,
            threads: u32::try_from(threads).map_err(|_| "too many threads")?,
            virtual_sats,
            redeem_script,
            locking_bytecode,
            lock_hash,
            fee_k,
        })
    }

    /// The category as a txid is shown (display order).
    pub fn category_display(&self) -> String {
        let mut bytes = self.category;
        bytes.reverse();
        hex::encode(bytes)
    }

    /// The vaults' Fulcrum script hash: `SHA256(locking bytecode)`,
    /// reversed.
    pub fn script_hash_hex(&self) -> String {
        let mut hash: Hash = Sha256::digest(&self.locking_bytecode).into();
        hash.reverse();
        hex::encode(hash)
    }

    /// The leaf a miner commits for `vault` in a coinbase at `height`,
    /// paying `payout` with `split`; `None` where the thread cannot be
    /// claimed at that height (its state is not older).
    pub fn leaf(&self, vault: &Vault, height: u32, payout: &[u8], split: &Split) -> Option<Leaf> {
        let state = vault.state()?;
        let delta = height
            .checked_sub(state.height)
            .filter(|delta| *delta > 0)?;
        let target_bits = claim_target_bits(state.target_bits, delta, self.threads)?;
        Some(Leaf::new(
            self.category,
            self.lock_hash,
            vault.state_hash(),
            payout,
            target_bits,
            split.ext(),
        ))
    }
}

/// The length of a minimal data push's opcode and length bytes.
fn push_prefix_len(length: usize) -> u64 {
    match length {
        0..=75 => 1,
        76..=255 => 2,
        256..=65_535 => 3,
        _ => 5,
    }
}

/// A minimal push of a non-negative script number.
fn number_push(value: u64) -> Vec<u8> {
    if value == 0 {
        return vec![0x00];
    }
    if value <= 16 {
        return vec![0x50 + value as u8];
    }
    let mut bytes = value.to_le_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.last().is_some_and(|last| last & 0x80 != 0) {
        bytes.push(0);
    }
    push(&bytes)
}

/// A minimal data push, as the covenant's unlocking arguments are written.
fn push(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 5);
    match data {
        [] => out.push(0x00),
        [value @ 1..=16] => {
            out.push(0x50 + value);
            return out;
        }
        [0x81] => {
            out.push(0x4f);
            return out;
        }
        _ if data.len() <= 75 => out.push(data.len() as u8),
        _ if data.len() <= 255 => out.extend([0x4c, data.len() as u8]),
        _ if data.len() <= 65_535 => {
            out.push(0x4d);
            out.extend((data.len() as u16).to_le_bytes());
        }
        _ => {
            out.push(0x4e);
            out.extend((data.len() as u32).to_le_bytes());
        }
    }
    out.extend_from_slice(data);
    out
}

// ---- vaults ----

/// One thread's vault, as Fulcrum lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vault {
    pub outpoint: OutPoint,
    pub value: u64,
    pub tokens: u64,
    pub commitment: [u8; STATE_LEN],
    /// Whether the output is in a block.
    pub confirmed: bool,
}

impl Vault {
    pub fn state(&self) -> Option<ThreadState> {
        ThreadState::parse(&self.commitment)
    }

    /// `HASH256` of the commitment: a leaf's `state_hash`.
    pub fn state_hash(&self) -> Hash {
        double_sha256(&self.commitment)
    }

    pub fn thread(&self) -> u32 {
        self.state().map_or(u32::MAX, |state| state.thread)
    }
}

/// The deployment's vaults in Fulcrum's `blockchain.scripthash.listunspent`
/// answer (with tokens): outputs of its category with a mutable NFT whose
/// 16-byte state names one of its threads, one per thread (the highest
/// state height wins, should Fulcrum show two).
pub fn vaults_from_listunspent(
    deployment: &Deployment,
    unspent: &serde_json::Value,
) -> Result<Vec<Vault>, String> {
    use serde_json::Value;
    let entries = unspent
        .as_array()
        .ok_or("Fulcrum's listunspent answer is not a list")?;
    let category = deployment.category_display();
    let mut vaults: Vec<Vault> = Vec::new();
    for entry in entries {
        let token = &entry["token_data"];
        if token["category"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref()
            != Some(category.as_str())
            || token["nft"]["capability"] != "mutable"
        {
            continue;
        }
        let Some(commitment) = token["nft"]["commitment"]
            .as_str()
            .and_then(|text| hex::decode(text).ok())
            .and_then(|bytes| <[u8; STATE_LEN]>::try_from(bytes).ok())
        else {
            continue;
        };
        let Some(state) = ThreadState::parse(&commitment) else {
            continue;
        };
        if state.thread >= deployment.threads {
            continue;
        }
        let tokens = match &token["amount"] {
            Value::String(text) => text.parse::<u64>().ok(),
            Value::Number(number) => number.as_u64(),
            _ => None,
        };
        let txid = entry["tx_hash"]
            .as_str()
            .and_then(|text| hex::decode(text).ok())
            .and_then(|bytes| <Hash>::try_from(bytes).ok());
        let (Some(tokens), Some(mut txid), Some(vout), Some(value)) = (
            tokens,
            txid,
            entry["tx_pos"]
                .as_u64()
                .and_then(|vout| u32::try_from(vout).ok()),
            entry["value"].as_u64(),
        ) else {
            return Err(
                "Fulcrum listed a tSAFA vault without its outpoint, value or amount".into(),
            );
        };
        txid.reverse();
        let vault = Vault {
            outpoint: OutPoint { txid, vout },
            value,
            tokens,
            commitment,
            confirmed: entry["height"].as_i64().is_some_and(|height| height > 0),
        };
        match vaults
            .iter_mut()
            .find(|known| known.thread() == state.thread)
        {
            Some(known)
                if known
                    .state()
                    .is_some_and(|known| known.height < state.height) =>
            {
                *known = vault
            }
            Some(_) => {}
            None => vaults.push(vault),
        }
    }
    vaults.sort_by_key(Vault::thread);
    Ok(vaults)
}

// ---- claims ----

/// Who a claim pays: the payout its leaf binds, the split its leaf's `ext`
/// commits to, and the scripts the split's slots name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimTerms {
    pub payout: Vec<u8>,
    pub split: Split,
    pub donation: Option<Vec<u8>>,
    pub fee: Option<Vec<u8>>,
}

/// Everything one claim must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimPlan {
    /// The coinbase's BIP34 height.
    pub height: u32,
    pub height_delta: u32,
    pub target_bits: u32,
    pub leaf: Leaf,
    pub reward: u64,
    pub shares: Shares,
    /// The outputs after the vault: (token amount, locking bytecode).
    pub outputs: Vec<(u64, Vec<u8>)>,
    pub next_state: [u8; STATE_LEN],
    /// `S = K + |proof|`; the vault pays `S` to `3S`.
    pub size_bound: u64,
    pub fee: u64,
    /// The vault's satoshis after the claim.
    pub vault_value: u64,
    pub locktime: u32,
}

/// A claim transaction, ready to broadcast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub raw: Vec<u8>,
    /// Display order.
    pub txid: String,
    pub plan: ClaimPlan,
}

/// The dust threshold BCHN applies to an output of `size` serialized
/// bytes at the default relay fee: `3 × (size + 148)`.
fn dust(size: usize) -> u64 {
    3 * (size as u64 + 148)
}

// #### merge-mining v2: the vault's floor
// What: a claim leaves the vault at least 800 satoshis and at least BCHN's
// dust for its output.
// Why: BCH P2Pool's covenant requires 800; below dust no node relays the
// claim and the thread would wait for a buyer.
// Look here if: a claim is refused for the vault's satoshis.
/// The fewest satoshis a claim may leave the vault.
fn vault_floor(size: usize) -> u64 {
    dust(size).max(OUTPUT_SATS)
}

// #### merge-mining v2: the claim builder
// What: the claim the covenant accepts for `proof` against `vault`, at
// `fee_rate` satoshis per byte of its size bound (1 to 3) and lock time
// `locktime` (the coinbase height by default): one input, the vault, with
// sequence 0xfffffffe and the unlocking pushes split preimage, payout
// hash, proof and redeem script; the vault output with the next state, the
// tokens less the reward and the satoshis less the fee and 800 per share
// output; then the miner's, the donation's and the fee's shares. Every
// rule the covenant checks is checked first, and a claim it would refuse
// is an error.
// Why: the vault pays its own fee and nobody signs, so the transaction is
// fixed by the covenant; anything else is refused on chain.
// Look here if: a tSAFA claim is refused, or its fee or shares disagree
// with the covenant.
/// Plans the claim of `proof` against `vault`.
pub fn plan_claim(
    deployment: &Deployment,
    vault: &Vault,
    proof: &[u8],
    terms: &ClaimTerms,
    fee_rate: u64,
    locktime: Option<u32>,
) -> Result<ClaimPlan, String> {
    if !(1..=MAX_FEE_RATE).contains(&fee_rate) {
        return Err("the claim's fee rate must be 1, 2 or 3 satoshis per byte".into());
    }
    for (script, what) in [
        (Some(&terms.payout), "payout"),
        (terms.donation.as_ref(), "donation"),
        (terms.fee.as_ref(), "fee"),
    ] {
        if script.is_some_and(|script| script.is_empty() || script.len() > MAX_PAYOUT_SCRIPT) {
            return Err(format!("the claim's {what} script must be 1 to 35 bytes"));
        }
    }
    let slot_script = |slot: &super::split::Slot, script: Option<&Vec<u8>>, what: &str| {
        if slot.is_empty() {
            return Ok(None);
        }
        match script {
            Some(script) if double_sha256(script) == *slot.script_hash() => {
                Ok(Some(script.clone()))
            }
            _ => Err(format!("the claim has no {what} script matching its split")),
        }
    };
    let donation = slot_script(
        terms.split.donation_slot(),
        terms.donation.as_ref(),
        "donation",
    )?;
    let fee_script = slot_script(terms.split.fee_slot(), terms.fee.as_ref(), "fee")?;
    let parsed = AuxProof::from_bytes(proof).map_err(|error| error.to_string())?;
    let height = coinbase_height(&parsed.cb_head)
        .ok_or("the proof's coinbase has no BIP34 height the covenant reads")?;
    let height = u32::try_from(height)
        .ok()
        .filter(|height| *height < LOCKTIME_THRESHOLD)
        .ok_or("the proof's coinbase height is in the lock-time domain")?;
    let state = vault.state().ok_or("the vault's state is not 16 bytes")?;
    if state.height >= height {
        return Err("the coinbase is not newer than the thread's state".into());
    }
    let height_delta = height - state.height;
    let target_bits = claim_target_bits(state.target_bits, height_delta, deployment.threads)
        .ok_or("the thread's target bits are not canonical")?;
    let leaf = Leaf::new(
        deployment.category,
        deployment.lock_hash,
        vault.state_hash(),
        &terms.payout,
        target_bits,
        terms.split.ext(),
    );
    verify_leaf(&parsed, &leaf, MAX_AUX_HEIGHT).map_err(|error| error.to_string())?;
    if !bch_header(deployment.network, &parsed.header, height, &state) {
        return Err("the header's nBits are outside the ASERT band (and the fallback band)".into());
    }
    let reward = vault.tokens / REWARD_DIVISOR;
    if reward == 0 {
        return Err("the vault has too few tokens for a reward".into());
    }
    let amm = Amm {
        cash: vault.value,
        virtual_sats: deployment.virtual_sats,
        tokens: vault.tokens,
    };
    let shares = terms.split.claim_shares(reward, &amm);
    let outputs: Vec<(u64, Vec<u8>)> = [
        (shares.miner, Some(terms.payout.clone())),
        (shares.donation, donation),
        (shares.fee, fee_script),
    ]
    .into_iter()
    .filter(|(amount, _)| *amount > 0)
    .map(|(amount, script)| script.map(|script| (amount, script)))
    .collect::<Option<_>>()
    .ok_or("a share has no script to pay")?;
    let next_state = ThreadState {
        thread: state.thread,
        height,
        target_bits,
        ref_bits: header_bits(&parsed.header),
    }
    .encode();
    let size_bound = deployment.fee_k + proof.len() as u64;
    let fee = fee_rate * size_bound;
    let vault_value = vault
        .value
        .checked_sub(fee + OUTPUT_SATS * outputs.len() as u64)
        .ok_or("the vault cannot pay the claim's fee and outputs")?;
    let vault_output = vault_output(deployment, vault_value, vault.tokens - reward, &next_state);
    if vault_value < vault_floor(vault_output.len()) {
        return Err("the vault would keep fewer than 800 satoshis, or dust".into());
    }
    let locktime = locktime.unwrap_or(height);
    if locktime < height || locktime >= LOCKTIME_THRESHOLD {
        return Err(
            "the claim's lock time must be a height at or above the coinbase height".into(),
        );
    }
    Ok(ClaimPlan {
        height,
        height_delta,
        target_bits,
        leaf,
        reward,
        shares,
        outputs,
        next_state,
        size_bound,
        fee,
        vault_value,
        locktime,
    })
}

/// Builds the claim of `proof` against `vault`, then checks its bytes with
/// [`check_claim`] as a node's covenant would.
pub fn build_claim(
    deployment: &Deployment,
    vault: &Vault,
    proof: &[u8],
    terms: &ClaimTerms,
    fee_rate: u64,
    locktime: Option<u32>,
) -> Result<Claim, String> {
    let plan = plan_claim(deployment, vault, proof, terms, fee_rate, locktime)?;
    let preimage = if terms.split.is_none() {
        Vec::new()
    } else {
        terms.split.preimage().to_vec()
    };
    let mut unlocking = push(&preimage);
    unlocking.extend(push(&plan.leaf.payout_hash));
    unlocking.extend(push(proof));
    unlocking.extend(push(&deployment.redeem_script));
    if unlocking.len() > MAX_UNLOCKING_BYTES {
        return Err("the claim exceeds the 10,000-byte unlocking limit".into());
    }
    let mut raw = CLAIM_VERSION.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend_from_slice(&vault.outpoint.txid);
    raw.extend_from_slice(&vault.outpoint.vout.to_le_bytes());
    compact_size(unlocking.len() as u64, &mut raw);
    raw.extend_from_slice(&unlocking);
    raw.extend_from_slice(&CLAIM_SEQUENCE.to_le_bytes());
    compact_size(1 + plan.outputs.len() as u64, &mut raw);
    raw.extend(vault_output(
        deployment,
        plan.vault_value,
        vault.tokens - plan.reward,
        &plan.next_state,
    ));
    for (amount, script) in &plan.outputs {
        let mut prefix = token_prefix(&deployment.category, None, *amount);
        prefix.extend_from_slice(script);
        raw.extend(output(OUTPUT_SATS, &prefix));
    }
    raw.extend_from_slice(&plan.locktime.to_le_bytes());
    if raw.len() as u64 > plan.size_bound {
        return Err("the claim is larger than its size bound".into());
    }
    check_claim(deployment, vault, &raw)?;
    let mut txid = double_sha256(&raw);
    txid.reverse();
    Ok(Claim {
        raw,
        txid: hex::encode(txid),
        plan,
    })
}

fn vault_output(
    deployment: &Deployment,
    value: u64,
    tokens: u64,
    state: &[u8; STATE_LEN],
) -> Vec<u8> {
    let mut bytecode = token_prefix(&deployment.category, Some(state), tokens);
    bytecode.extend_from_slice(&deployment.locking_bytecode);
    output(value, &bytecode)
}

/// A CashTokens prefix: fungible tokens of `category`, and a mutable NFT
/// with `commitment` when one is given.
fn token_prefix(category: &Hash, commitment: Option<&[u8; STATE_LEN]>, amount: u64) -> Vec<u8> {
    let mut prefix = vec![0xef];
    prefix.extend_from_slice(category);
    let amount_flag = if amount > 0 { 0x10 } else { 0 };
    match commitment {
        Some(commitment) => {
            prefix.push(0x40 | 0x20 | amount_flag | MUTABLE);
            compact_size(commitment.len() as u64, &mut prefix);
            prefix.extend_from_slice(commitment);
        }
        None => prefix.push(amount_flag),
    }
    if amount > 0 {
        compact_size(amount, &mut prefix);
    }
    prefix
}

fn output(value: u64, bytecode: &[u8]) -> Vec<u8> {
    let mut out = value.to_le_bytes().to_vec();
    compact_size(bytecode.len() as u64, &mut out);
    out.extend_from_slice(bytecode);
    out
}

fn compact_size(value: u64, out: &mut Vec<u8>) {
    match value {
        0..=0xfc => out.push(value as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend((value as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend((value as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend(value.to_le_bytes());
        }
    }
}

// ---- the independent check ----

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if self.0.len() < count {
            return Err("the claim transaction is truncated".into());
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8])))
    }

    /// A minimally encoded compact size.
    fn compact(&mut self) -> Result<u64, String> {
        let first = self.take(1)?[0];
        let (value, minimum) = match first {
            0xfd => (
                u64::from(u16::from_le_bytes([self.take(1)?[0], self.take(1)?[0]])),
                0xfd,
            ),
            0xfe => (u64::from(self.u32()?), 0x1_0000),
            0xff => (self.u64()?, 0x1_0000_0000),
            byte => (u64::from(byte), 0),
        };
        if value < minimum {
            return Err("the claim transaction has a non-minimal compact size".into());
        }
        Ok(value)
    }
}

/// An output as introspection shows it.
struct TxOutput {
    value: u64,
    locking: Vec<u8>,
    category: Option<Hash>,
    /// The NFT's capability and commitment, if it carries one.
    nft: Option<(u8, Vec<u8>)>,
    amount: u64,
}

struct Tx {
    inputs: Vec<(OutPoint, Vec<u8>, u32)>,
    outputs: Vec<TxOutput>,
    locktime: u32,
}

fn parse_tx(raw: &[u8]) -> Result<Tx, String> {
    let mut reader = Reader(raw);
    reader.u32()?;
    let mut inputs = Vec::new();
    for _ in 0..reader.compact()? {
        let txid: Hash = reader.take(32)?.try_into().map_err(|_| "bad outpoint")?;
        let vout = reader.u32()?;
        let length = usize::try_from(reader.compact()?).map_err(|_| "bad length")?;
        let unlocking = reader.take(length)?.to_vec();
        let sequence = reader.u32()?;
        inputs.push((OutPoint { txid, vout }, unlocking, sequence));
    }
    let mut outputs = Vec::new();
    for _ in 0..reader.compact()? {
        let value = reader.u64()?;
        let length = usize::try_from(reader.compact()?).map_err(|_| "bad length")?;
        outputs.push(parse_output(value, reader.take(length)?)?);
    }
    let locktime = reader.u32()?;
    if !reader.0.is_empty() {
        return Err("the claim transaction has trailing bytes".into());
    }
    Ok(Tx {
        inputs,
        outputs,
        locktime,
    })
}

fn parse_output(value: u64, bytecode: &[u8]) -> Result<TxOutput, String> {
    if bytecode.first() != Some(&0xef) {
        return Ok(TxOutput {
            value,
            locking: bytecode.to_vec(),
            category: None,
            nft: None,
            amount: 0,
        });
    }
    let bad = || "the claim has a malformed token prefix".to_owned();
    let mut reader = Reader(&bytecode[1..]);
    let category: Hash = reader.take(32)?.try_into().map_err(|_| bad())?;
    let bitfield = reader.take(1)?[0];
    let capability = bitfield & 0x0f;
    let (has_commitment, has_nft, has_amount) = (
        bitfield & 0x40 != 0,
        bitfield & 0x20 != 0,
        bitfield & 0x10 != 0,
    );
    if bitfield & 0x80 != 0
        || capability > 2
        || (has_commitment && !has_nft)
        || (!has_nft && capability != 0)
        || (!has_nft && !has_amount)
    {
        return Err(bad());
    }
    let commitment = if has_commitment {
        let length = usize::try_from(reader.compact()?).map_err(|_| bad())?;
        if length == 0 {
            return Err(bad());
        }
        reader.take(length)?.to_vec()
    } else {
        Vec::new()
    };
    let amount = if has_amount {
        let amount = reader.compact()?;
        if amount == 0 || amount > i64::MAX as u64 {
            return Err(bad());
        }
        amount
    } else {
        0
    };
    Ok(TxOutput {
        value,
        locking: reader.0.to_vec(),
        category: Some(category),
        nft: has_nft.then_some((capability, commitment)),
        amount,
    })
}

/// The pushes of a push-only, minimally encoded script.
fn parse_pushes(script: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut reader = Reader(script);
    let mut pushes = Vec::new();
    while !reader.0.is_empty() {
        let start = script.len() - reader.0.len();
        let opcode = reader.take(1)?[0];
        let data = match opcode {
            0x00 => Vec::new(),
            0x01..=0x4b => reader.take(usize::from(opcode))?.to_vec(),
            0x4c => {
                let length = reader.take(1)?[0];
                reader.take(usize::from(length))?.to_vec()
            }
            0x4d => {
                let length = u16::from_le_bytes([reader.take(1)?[0], reader.take(1)?[0]]);
                reader.take(usize::from(length))?.to_vec()
            }
            0x4f => vec![0x81],
            0x51..=0x60 => vec![opcode - 0x50],
            _ => return Err("the claim's unlocking bytecode is not push-only".into()),
        };
        if push(&data) != script[start..script.len() - reader.0.len()] {
            return Err("the claim's unlocking bytecode has a non-minimal push".into());
        }
        pushes.push(data);
    }
    Ok(pushes)
}

// #### merge-mining v2: the claim check before broadcast
// What: a claim's bytes, read back the way the covenant reads them through
// introspection, against every rule it enforces: one input (the vault),
// four minimal pushes ending in this deployment's redeem script, the vault
// returned at output 0 with its script, category, mutable NFT and next
// state, the coinbase height bound by CLTV (a height-type lock time at or
// above it, a non-final sequence), the leaf rebuilt from the vault and the
// unlocking payout hash and split, the proof, the ASERT or fallback band,
// the reward, the shares and their outputs, and a fee from `S` to `3S`.
// Also what makes it relay: at most 10,000 unlocking bytes, no larger
// than its size bound, and a vault above dust.
// Why: Pickaxe has no BCH script VM, and a refused claim loses its win;
// this check is written from the covenant's opcodes, apart from the
// builder.
// Look here if: a built claim fails this check (a builder bug), or a node
// refuses a claim that passed it.
/// Checks a claim's bytes against the covenant's rules for `vault`.
pub fn check_claim(deployment: &Deployment, vault: &Vault, raw: &[u8]) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("the covenant would refuse this claim: {why}"));
    let tx = parse_tx(raw)?;
    let [(outpoint, unlocking, sequence)] = tx.inputs.as_slice() else {
        return refuse("a claim spends the vault alone");
    };
    if *outpoint != vault.outpoint {
        return refuse("it does not spend the vault");
    }
    if unlocking.len() > MAX_UNLOCKING_BYTES {
        return refuse("its unlocking bytecode exceeds 10,000 bytes");
    }
    let pushes = parse_pushes(unlocking)?;
    let [preimage, payout_hash, proof, redeem] = pushes.as_slice() else {
        return refuse("its unlocking bytecode is not split, payout hash, proof and redeem script");
    };
    if *redeem != deployment.redeem_script {
        return refuse("it unlocks another covenant");
    }
    let payout_hash: Hash = payout_hash
        .as_slice()
        .try_into()
        .map_err(|_| "the covenant would refuse this claim: the payout hash is not 32 bytes")?;
    // The vault returns at its own index, with its category and mutable NFT.
    let Some(vault_out) = tx.outputs.first() else {
        return refuse("it has no outputs");
    };
    if vault_out.locking != deployment.locking_bytecode
        || vault_out.category != Some(deployment.category)
        || vault_out.nft.as_ref().map(|(capability, _)| *capability) != Some(MUTABLE)
    {
        return refuse("output 0 is not the vault");
    }
    let next: &[u8] = vault_out
        .nft
        .as_ref()
        .map_or(&[], |(_, commitment)| commitment.as_slice());
    if next == vault.commitment.as_slice() {
        return refuse("an unchanged state is a trade, not a claim");
    }
    let state = vault.state().ok_or("the vault's state is not 16 bytes")?;
    // The proof and its coinbase height, bound by CLTV.
    let parsed = AuxProof::from_bytes(proof).map_err(|error| error.to_string())?;
    let height = coinbase_height(&parsed.cb_head)
        .filter(|height| *height < u64::from(LOCKTIME_THRESHOLD))
        .ok_or("the covenant would refuse this claim: no BIP34 height it reads")?
        as u32;
    if *sequence == u32::MAX || tx.locktime < height || tx.locktime >= LOCKTIME_THRESHOLD {
        return refuse("its lock time does not reach the coinbase height (CLTV)");
    }
    if state.height >= height {
        return refuse("the coinbase is not newer than the thread's state");
    }
    let delta = height - state.height;
    let target_bits = claim_target_bits(state.target_bits, delta, deployment.threads)
        .ok_or("the covenant would refuse this claim: the state's target is not canonical")?;
    let split = if preimage.is_empty() {
        Split::NONE
    } else {
        Split::parse(preimage)
            .map_err(|error| format!("the covenant would refuse this claim: {error}"))?
    };
    let leaf = Leaf {
        category: deployment.category,
        lock_hash: deployment.lock_hash,
        state_hash: vault.state_hash(),
        payout_hash,
        target_bits,
        ext: split.ext(),
    };
    verify_leaf(&parsed, &leaf, MAX_AUX_HEIGHT)
        .map_err(|error| format!("the covenant would refuse this claim: {error}"))?;
    if !bch_header(deployment.network, &parsed.header, height, &state) {
        return refuse("the header's nBits are outside the ASERT and fallback bands");
    }
    let expected_next = ThreadState {
        thread: state.thread,
        height,
        target_bits,
        ref_bits: header_bits(&parsed.header),
    }
    .encode();
    if next != expected_next.as_slice() {
        return refuse("output 0 does not carry the next state");
    }
    // The reward and its shares, in order, 800 satoshis each.
    let reward = vault.tokens / REWARD_DIVISOR;
    if reward == 0 || vault_out.amount != vault.tokens - reward {
        return refuse("the vault does not keep its tokens less the reward");
    }
    let shares = split.claim_shares(
        reward,
        &Amm {
            cash: vault.value,
            virtual_sats: deployment.virtual_sats,
            tokens: vault.tokens,
        },
    );
    let expected = expected_outputs(&split, payout_hash, &shares);
    let seen: Vec<TokenOutput> = tx.outputs[1..]
        .iter()
        .map(|output| TokenOutput {
            sats: output.value,
            script_hash: double_sha256(&output.locking),
            category: output.category.unwrap_or([0; 32]),
            nft: output.nft.as_ref().map(|(capability, _)| *capability),
            amount: output.amount,
        })
        .collect();
    check_claim_outputs(&expected, &seen, &deployment.category)
        .map_err(|error| format!("the covenant would refuse this claim: {error}"))?;
    if tx.outputs[1..]
        .iter()
        .any(|output| output.locking.len() > MAX_PAYOUT_SCRIPT)
    {
        return refuse("a share output's script is longer than 35 bytes");
    }
    // The vault pays S to 3S.
    let size_bound = deployment.fee_k + proof.len() as u64;
    let paid = OUTPUT_SATS * expected.len() as u64;
    let fee = vault
        .value
        .checked_sub(vault_out.value)
        .and_then(|left| left.checked_sub(paid));
    if !fee.is_some_and(|fee| (size_bound..=MAX_FEE_RATE * size_bound).contains(&fee)) {
        return refuse("the vault's fee is outside S to 3S");
    }
    // What a node needs to relay it.
    if raw.len() as u64 > size_bound {
        return Err("the claim is larger than its size bound".into());
    }
    let vault_size = vault_output(
        deployment,
        vault_out.value,
        vault_out.amount,
        &expected_next,
    )
    .len();
    if vault_out.value < vault_floor(vault_size) {
        return refuse("the vault keeps fewer than 800 satoshis, or dust");
    }
    Ok(())
}

// ---- the chain ----

/// What the claim worker asks of the chain; Fulcrum at run time, a fake in
/// tests (no network calls in tests).
pub trait Chain {
    /// The chain tip's height.
    fn tip_height(&mut self) -> Result<u32, String>;
    /// The deployment's vaults.
    fn vaults(&mut self, deployment: &Deployment) -> Result<Vec<Vault>, String>;
    /// Broadcasts a transaction; its txid.
    fn broadcast(&mut self, raw: &[u8]) -> Result<String, String>;
    /// Whether a transaction is in the chain or the mempool.
    fn known(&mut self, txid: &str) -> Result<bool, String>;
}

/// The chain through the configured Fulcrum servers, connected on first
/// use and again after a failure.
pub struct Fulcrum {
    endpoints: Vec<String>,
    network: MiningNetwork,
    session: Option<crate::electrum::ElectrumSession>,
}

impl Fulcrum {
    pub fn new(endpoints: Vec<String>, network: MiningNetwork) -> Self {
        Self {
            endpoints,
            network,
            session: None,
        }
    }

    fn rpc(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        if self.session.is_none() {
            let photon = match self.network {
                MiningNetwork::Mainnet => &crate::protocol::MAINNET_PHOTON,
                MiningNetwork::Chipnet => &crate::protocol::CHIPNET_PHOTON,
            };
            self.session = Some(
                crate::electrum::ElectrumSession::connect_failover_for_deployment(
                    &self.endpoints,
                    photon,
                )?,
            );
        }
        let session = self.session.as_mut().ok_or("no Fulcrum session")?;
        let answer = session.rpc(method, params);
        if answer.is_err() {
            self.session = None;
        }
        answer
    }
}

impl Chain for Fulcrum {
    fn tip_height(&mut self) -> Result<u32, String> {
        let header = self.rpc("blockchain.headers.subscribe", serde_json::json!([]))?;
        header["height"]
            .as_u64()
            .and_then(|height| u32::try_from(height).ok())
            .ok_or_else(|| "Fulcrum gave no tip height".into())
    }

    fn vaults(&mut self, deployment: &Deployment) -> Result<Vec<Vault>, String> {
        let unspent = self.rpc(
            "blockchain.scripthash.listunspent",
            serde_json::json!([deployment.script_hash_hex(), "include_tokens"]),
        )?;
        vaults_from_listunspent(deployment, &unspent)
    }

    fn broadcast(&mut self, raw: &[u8]) -> Result<String, String> {
        let answer = self.rpc(
            "blockchain.transaction.broadcast",
            serde_json::json!([hex::encode(raw)]),
        )?;
        Ok(answer
            .as_str()
            .map_or_else(|| answer.to_string(), str::to_owned))
    }

    fn known(&mut self, txid: &str) -> Result<bool, String> {
        match self.rpc(
            "blockchain.transaction.get",
            serde_json::json!([txid, false]),
        ) {
            Ok(serde_json::Value::Null) => Ok(false),
            Ok(_) => Ok(true),
            Err(error) => {
                let lower = error.to_ascii_lowercase();
                if lower.contains("no such") || lower.contains("not found") {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stratum_v2::merge::{commitment::AuxCommitment, tree};

    pub(crate) fn p2pkh(byte: u8) -> Vec<u8> {
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend([byte; 20]);
        script.extend([0x88, 0xac]);
        script
    }

    /// Real Chipnet block 329,001 (its parent 329,000): its time and nBits
    /// pass the ASERT band at its height.
    const CHIPNET_329001: &str = "e0ffff3f593afa32a74a48eb223471d047bab06ec694e96916e49d466d02000000000000180a3af09db7f4e6e56e5cc3d9744d3b5608367eb54d90a67917bc084de9e23d7d20da6aee06311adac55d98";
    /// Real mainnet block 974,001.
    const MAINNET_974001: &str = "0040aa2249c7a2ccff1dcc7df964811c3fc65fe0c9f2bcd313329c0000000000000000002e79ec0f05c0d315c5b36f9710aaad19a572308f0c9d416c7aead976d3a6f02f83dad96a013102180551d52c";

    fn time_and_bits(header: &str) -> (u32, u32) {
        let bytes = hex::decode(header).unwrap();
        (
            u32::from_le_bytes(bytes[68..72].try_into().unwrap()),
            u32::from_le_bytes(bytes[72..76].try_into().unwrap()),
        )
    }

    pub(crate) fn chipnet_block() -> (u32, u32, u32) {
        let (time, bits) = time_and_bits(CHIPNET_329001);
        (329_001, time, bits)
    }

    /// A synthetic redeem script of `length` bytes that pushes what the
    /// covenant's does (its K, virtual reserve, reward divisor and the
    /// proof magic); the claim treats the rest as opaque.
    pub(crate) fn redeem(length: usize, virtual_sats: u64) -> Vec<u8> {
        let fee_k = CLAIM_FIXED_BYTES + push_prefix_len(length) + length as u64;
        let mut script = number_push(fee_k);
        script.extend(number_push(virtual_sats));
        script.extend(number_push(REWARD_DIVISOR));
        script.extend([0x05, b'C', b'T', b'M', b'P', 0x02]);
        script.resize(length, 0x61);
        script
    }

    pub(crate) fn deployment_json(redeem: &[u8], virtual_sats: u64) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "name": "tSAFA",
            "network": "chipnet",
            "category": hex::encode(double_sha256(b"tSAFA test category")),
            "threads": 4,
            "virtual_sats": virtual_sats.to_string(),
            "redeem_script": hex::encode(redeem),
        })
    }

    pub(crate) fn deployment(virtual_sats: u64) -> Deployment {
        Deployment::from_json(
            &deployment_json(&redeem(1_366, virtual_sats), virtual_sats).to_string(),
        )
        .unwrap()
    }

    /// The fixed-shape coinbase head and tail of a coinbase at `height`.
    pub(crate) fn coinbase(height: u32, payouts: &[(u64, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let mut height_push = vec![];
        let mut value = height;
        while value > 0 {
            height_push.push(value as u8);
            value >>= 8;
        }
        if height_push.last().is_some_and(|last| last & 0x80 != 0) {
            height_push.push(0);
        }
        let mut script = vec![height_push.len() as u8];
        script.extend(&height_push);
        script.extend([7; 8]);
        let mut head = 2u32.to_le_bytes().to_vec();
        head.push(1);
        head.extend([0; 32]);
        head.extend([0xff; 4]);
        head.push(script.len() as u8);
        head.extend(&script);
        head.extend([0xff; 4]);
        head.push(1 + payouts.len() as u8);
        let mut tail = Vec::new();
        for (value, script) in payouts {
            tail.extend(value.to_le_bytes());
            compact_size(script.len() as u64, &mut tail);
            tail.extend(script);
        }
        tail.extend(0u32.to_le_bytes());
        (head, tail)
    }

    /// One claim's inputs, mined.
    pub(crate) struct Scenario {
        pub deployment: Deployment,
        pub vault: Vault,
        pub terms: ClaimTerms,
        pub proof: Vec<u8>,
    }

    #[derive(Clone)]
    pub(crate) struct Setup {
        pub virtual_sats: u64,
        pub state: ThreadState,
        pub height: u32,
        pub time: u32,
        pub bits: u32,
        pub split: Option<(u16, u16)>,
        pub aux_height: u8,
        pub coinbase_depth: usize,
        pub payouts: Vec<(u64, Vec<u8>)>,
        pub value: u64,
        pub tokens: u64,
        /// Mine (and commit) a leaf with these bits instead of the
        /// covenant's.
        pub leaf_bits: Option<u32>,
    }

    impl Default for Setup {
        fn default() -> Self {
            let (height, time, bits) = chipnet_block();
            Self {
                virtual_sats: 42_000_000_000,
                state: ThreadState {
                    thread: 1,
                    height: height - 10,
                    target_bits: 0x2000_ffff,
                    ref_bits: bits,
                },
                height,
                time,
                bits,
                split: None,
                aux_height: 1,
                coinbase_depth: 1,
                payouts: vec![(312_500_000, p2pkh(0x12))],
                value: 2_000_000,
                tokens: 420_000_000_000,
                leaf_bits: None,
            }
        }
    }

    pub(crate) fn scenario(setup: Setup) -> Scenario {
        scenario_with(deployment(setup.virtual_sats), setup)
    }

    /// A claim's inputs, mined, against `deployment`.
    pub(crate) fn scenario_with(deployment: Deployment, setup: Setup) -> Scenario {
        let vault = Vault {
            outpoint: OutPoint {
                txid: double_sha256(b"vault outpoint"),
                vout: 0,
            },
            value: setup.value,
            tokens: setup.tokens,
            commitment: setup.state.encode(),
            confirmed: true,
        };
        let split = match setup.split {
            None => Split::NONE,
            Some((donation, fee)) => Split::new(
                super::super::split::Slot::new(donation, &p2pkh(0xd0)).unwrap(),
                super::super::split::Slot::new(fee, &p2pkh(0xfe)).unwrap(),
            ),
        };
        let terms = ClaimTerms {
            payout: p2pkh(0x12),
            split,
            donation: Some(p2pkh(0xd0)),
            fee: Some(p2pkh(0xfe)),
        };
        let mut leaf = deployment
            .leaf(&vault, setup.height, &terms.payout, &split)
            .expect("a claimable state");
        if let Some(bits) = setup.leaf_bits {
            leaf.target_bits = bits;
        }
        let salt = 7;
        let slot = tree::slot(&leaf.category, salt, setup.aux_height);
        let branch: Vec<Hash> = (0..setup.aux_height)
            .map(|level| double_sha256(&[0xaa, level]))
            .collect();
        let root = tree::fold(leaf.hash(), slot, &branch);
        let output0 = AuxCommitment {
            root,
            height: setup.aux_height,
            salt,
        }
        .output();
        let (head, tail) = coinbase(setup.height, &setup.payouts);
        let cb_branch: Vec<Hash> = (0..setup.coinbase_depth)
            .map(|level| double_sha256(&[0xbb, level as u8]))
            .collect();
        let mut merkle = double_sha256(&[&head[..], &output0, &tail].concat());
        for sibling in &cb_branch {
            merkle = double_sha256(&[&merkle[..], sibling].concat());
        }
        let target = expand_token_target(leaf.target_bits).unwrap();
        let mut header = [0u8; 80];
        header[..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        header[4..36].fill(0x55);
        header[36..68].copy_from_slice(&merkle);
        header[68..72].copy_from_slice(&setup.time.to_le_bytes());
        header[72..76].copy_from_slice(&setup.bits.to_le_bytes());
        for nonce in 0u32.. {
            header[76..].copy_from_slice(&nonce.to_le_bytes());
            if BigUint::from_bytes_le(&double_sha256(&header)) <= target {
                break;
            }
        }
        let proof = AuxProof {
            header,
            height: setup.aux_height,
            salt,
            aux_branch: branch,
            cb_head: head,
            cb_tail: tail,
            cb_branch,
        }
        .to_bytes()
        .unwrap();
        Scenario {
            deployment,
            vault,
            terms,
            proof,
        }
    }

    impl Scenario {
        pub(crate) fn claim(&self, fee_rate: u64) -> Result<Claim, String> {
            build_claim(
                &self.deployment,
                &self.vault,
                &self.proof,
                &self.terms,
                fee_rate,
                None,
            )
        }
    }

    fn amounts(claim: &Claim) -> Vec<(u64, Vec<u8>)> {
        claim.plan.outputs.clone()
    }

    // #### merge-mining v2: the fee constant and the unlocking pushes
    // What: K is 577 plus the redeem script's push: 1,946 for P2Pool's
    // 1,366-byte Chipnet covenant and 1,943 for its 1,363-byte mainnet one;
    // the unlocking bytecode is 1,597 bytes for a proof with h 0, d 0 and no
    // split, and 1,731 for h 1, d 1 and a split, as P2Pool measured in the
    // VM.
    // Look here if: CLAIM_FIXED_BYTES or the unlocking pushes change.
    #[test]
    fn the_fee_constant_and_unlocking_sizes_match_p2pools_measurements() {
        assert_eq!(CLAIM_FIXED_BYTES, 577);
        assert_eq!(deployment(42_000_000_000).fee_k, 1_946);
        let json = deployment_json(&redeem(1_363, 42_000_000_000), 42_000_000_000);
        assert_eq!(
            Deployment::from_json(&json.to_string()).unwrap().fee_k,
            1_943
        );
        let plain = scenario(Setup {
            aux_height: 0,
            coinbase_depth: 0,
            ..Setup::default()
        });
        assert_eq!(plain.proof.len(), 192);
        let claim = plain.claim(1).unwrap();
        let unlocking = |claim: &Claim| {
            let tx = parse_tx(&claim.raw).unwrap();
            tx.inputs[0].1.len()
        };
        assert_eq!(unlocking(&claim), 1_597);
        assert!(claim.raw.len() as u64 <= claim.plan.size_bound);
        let split = scenario(Setup {
            split: Some((100, 100)),
            ..Setup::default()
        });
        assert_eq!(split.proof.len(), 256);
        let claim = split.claim(1).unwrap();
        assert_eq!(unlocking(&claim), 1_731);
    }

    // #### merge-mining v2: a whole claim
    // What: a claim without a split pays the miner the whole reward
    // (1,000,000 of 420,000,000,000) with 800 satoshis, the vault pays S at
    // 1 sat/byte and keeps the rest, and the next state is the thread, the
    // coinbase height, the claim target and the header's nBits.
    #[test]
    fn a_claim_without_a_split_pays_the_miner_the_whole_reward() {
        let scenario = scenario(Setup::default());
        let claim = scenario.claim(1).unwrap();
        let (height, _, bits) = chipnet_block();
        assert_eq!(amounts(&claim), vec![(1_000_000, p2pkh(0x12))]);
        assert_eq!(claim.plan.fee, claim.plan.size_bound);
        assert_eq!(claim.plan.size_bound, 1_946 + scenario.proof.len() as u64);
        assert_eq!(claim.plan.vault_value, 2_000_000 - claim.plan.fee - 800);
        assert_eq!(claim.plan.locktime, height);
        assert_eq!(
            ThreadState::parse(&claim.plan.next_state).unwrap(),
            ThreadState {
                thread: 1,
                height,
                target_bits: claim.plan.target_bits,
                ref_bits: bits
            }
        );
        // dh 10 eases 0x2000ffff by 1.75.
        assert_eq!(claim.plan.target_bits, 0x2001_bffe);
        let tx = parse_tx(&claim.raw).unwrap();
        assert_eq!(tx.outputs.len(), 2);
        assert_eq!(tx.outputs[1].value, 800);
        assert_eq!(tx.outputs[1].amount, 1_000_000);
        assert_eq!(tx.outputs[0].amount, 420_000_000_000 - 1_000_000);
        assert_eq!(tx.locktime, height);
        assert_eq!(tx.inputs[0].2, CLAIM_SEQUENCE);
    }

    // #### merge-mining v2: the claim's bytes
    // What: the claim, written out field by field from the CashTokens and
    // transaction formats (not from the builder): version 2, one input
    // with sequence 0xfffffffe, the vault output (token bitfield 0x71: a
    // mutable NFT with a commitment and an amount), the miner's fungible
    // output (bitfield 0x10) with 800 satoshis, and the lock time.
    #[test]
    fn a_claim_is_byte_exact() {
        let scenario = scenario(Setup::default());
        let claim = scenario.claim(1).unwrap();
        let deployment = &scenario.deployment;
        let unlocking = [
            "00".to_owned(),
            format!("20{}", hex::encode(claim.plan.leaf.payout_hash)),
            format!(
                "4d{}{}",
                hex::encode((scenario.proof.len() as u16).to_le_bytes()),
                hex::encode(&scenario.proof)
            ),
            format!("4d5605{}", hex::encode(&deployment.redeem_script)),
        ]
        .concat();
        let category = hex::encode(deployment.category);
        let vault = format!(
            "{}5fef{category}7110{}ff{}aa20{}87",
            hex::encode(claim.plan.vault_value.to_le_bytes()),
            hex::encode(claim.plan.next_state),
            hex::encode((420_000_000_000u64 - 1_000_000).to_le_bytes()),
            hex::encode(double_sha256(&deployment.redeem_script)),
        );
        let miner = format!(
            "200300000000000040ef{category}10fe40420f0076a914{}88ac",
            "12".repeat(20)
        );
        let expected = format!(
            "0200000001{}00000000fd{}{unlocking}feffffff02{vault}{miner}{}",
            hex::encode(scenario.vault.outpoint.txid),
            hex::encode((unlocking.len() as u16 / 2).to_le_bytes()),
            hex::encode(329_001u32.to_le_bytes()),
        );
        assert_eq!(hex::encode(&claim.raw), expected);
        let mut txid = double_sha256(&claim.raw);
        txid.reverse();
        assert_eq!(claim.txid, hex::encode(txid));
    }

    // #### merge-mining v2: the split shares, as P2Pool's claim tests pay
    // them
    // What: at one satoshi per token a 1%/1% split pays 980,100 / 10,000 /
    // 9,900; at the default reserve a 0.99% donation (9,900 tokens, 990
    // satoshis) stays with the miner and the fee stays 19,802; a whole
    // donation leaves no miner output, its payout hash still in the leaf.
    #[test]
    fn claims_split_their_reward_as_the_covenant_does() {
        let one_sat = scenario(Setup {
            virtual_sats: 419_998_000_000,
            split: Some((100, 100)),
            ..Setup::default()
        });
        let claim = one_sat.claim(1).unwrap();
        assert_eq!(
            amounts(&claim),
            vec![
                (980_100, p2pkh(0x12)),
                (10_000, p2pkh(0xd0)),
                (9_900, p2pkh(0xfe))
            ]
        );
        let small = scenario(Setup {
            split: Some((99, 200)),
            ..Setup::default()
        });
        let claim = small.claim(1).unwrap();
        assert_eq!(
            claim.plan.shares,
            Shares {
                miner: 980_198,
                donation: 0,
                fee: 19_802
            }
        );
        assert_eq!(
            amounts(&claim),
            vec![(980_198, p2pkh(0x12)), (19_802, p2pkh(0xfe))]
        );
        let whole = scenario(Setup {
            virtual_sats: 419_998_000_000,
            split: Some((10_000, 0)),
            ..Setup::default()
        });
        let claim = whole.claim(1).unwrap();
        assert_eq!(amounts(&claim), vec![(1_000_000, p2pkh(0xd0))]);
        assert_eq!(claim.plan.leaf.payout_hash, double_sha256(&p2pkh(0x12)));
        // A split slot needs its script.
        let mut missing = whole.terms.clone();
        missing.donation = None;
        assert!(build_claim(
            &whole.deployment,
            &whole.vault,
            &whole.proof,
            &missing,
            1,
            None
        )
        .unwrap_err()
        .contains("no donation script"));
    }

    // #### merge-mining v2: the claim's fee
    // What: the vault pays 1, 2 or 3 sat/byte of S = K + |proof|; a rate of
    // 4 is refused, and the check refuses a satoshi under S or over 3S.
    #[test]
    fn the_vault_pays_s_to_3s() {
        let scenario = scenario(Setup::default());
        for rate in 1..=3 {
            let claim = scenario.claim(rate).unwrap();
            assert_eq!(claim.plan.fee, rate * claim.plan.size_bound);
        }
        assert!(scenario.claim(4).unwrap_err().contains("fee rate"));
        let low = scenario.claim(1).unwrap();
        let mut raw = low.raw.clone();
        bump_vault_value(&mut raw, &scenario, 1);
        assert!(check_claim(&scenario.deployment, &scenario.vault, &raw).is_err());
        let high = scenario.claim(3).unwrap();
        let mut raw = high.raw.clone();
        bump_vault_value(&mut raw, &scenario, -1);
        assert!(check_claim(&scenario.deployment, &scenario.vault, &raw).is_err());
    }

    /// Changes the vault output's satoshis in a claim's bytes.
    fn bump_vault_value(raw: &mut [u8], scenario: &Scenario, by: i64) {
        let at = vault_value_offset(raw, scenario);
        let value = u64::from_le_bytes(raw[at..at + 8].try_into().unwrap());
        raw[at..at + 8].copy_from_slice(&value.wrapping_add_signed(by).to_le_bytes());
    }

    fn vault_value_offset(raw: &[u8], scenario: &Scenario) -> usize {
        let tx = parse_tx(raw).unwrap();
        let unlocking = tx.inputs[0].1.len();
        let _ = scenario;
        4 + 1 + 36 + 3 + unlocking + 4 + 1
    }

    // #### merge-mining v2: what the covenant refuses
    // What: the check refuses each change P2Pool's claim tests show the
    // covenant refusing: a redirected miner output, with or without a
    // matching payout hash; a token or satoshi more for the miner; an
    // extra output or input; the vault moved, made immutable or given
    // another state; a lock time below the coinbase height or in the time
    // domain; a final sequence; a split output with 801 satoshis; the
    // donation and fee swapped; a hidden split; another redeem script.
    // Look here if: check_claim changes.
    #[test]
    fn the_check_refuses_what_the_covenant_refuses() {
        let scenario = scenario(Setup {
            virtual_sats: 419_998_000_000,
            split: Some((100, 200)),
            ..Setup::default()
        });
        let claim = scenario.claim(1).unwrap();
        let check = |raw: &[u8]| check_claim(&scenario.deployment, &scenario.vault, raw);
        assert_eq!(check(&claim.raw), Ok(()));
        let rebuild = |edit: &dyn Fn(&mut Tx)| {
            let mut tx = parse_tx(&claim.raw).unwrap();
            edit(&mut tx);
            encode_tx(&tx)
        };
        let cases: Vec<(&str, Edit)> = vec![
            (
                "miner redirected",
                Box::new(|tx| tx.outputs[1].locking = p2pkh(0x99)),
            ),
            (
                "a token more for the miner",
                Box::new(|tx| {
                    tx.outputs[0].amount -= 1;
                    tx.outputs[1].amount += 1;
                }),
            ),
            (
                "a satoshi more for the miner",
                Box::new(|tx| {
                    tx.outputs[0].value -= 1;
                    tx.outputs[1].value += 1;
                }),
            ),
            (
                "an extra output",
                Box::new(|tx| {
                    tx.outputs.push(TxOutput {
                        value: 0,
                        locking: vec![0x6a, 0x01, 0x78],
                        category: None,
                        nft: None,
                        amount: 0,
                    })
                }),
            ),
            (
                "a second input",
                Box::new(|tx| {
                    let extra = (
                        OutPoint {
                            txid: [9; 32],
                            vout: 0,
                        },
                        Vec::new(),
                        CLAIM_SEQUENCE,
                    );
                    tx.inputs.push(extra);
                }),
            ),
            (
                "the vault moved",
                Box::new(|tx| tx.outputs[0].locking = p2pkh(0x99)),
            ),
            (
                "an immutable NFT",
                Box::new(|tx| {
                    if let Some(nft) = tx.outputs[0].nft.as_mut() {
                        nft.0 = 0;
                    }
                }),
            ),
            (
                "another thread",
                Box::new(|tx| {
                    if let Some(nft) = tx.outputs[0].nft.as_mut() {
                        nft.1[0] ^= 1;
                    }
                }),
            ),
            (
                "another height",
                Box::new(|tx| {
                    if let Some(nft) = tx.outputs[0].nft.as_mut() {
                        nft.1[4] ^= 1;
                    }
                }),
            ),
            (
                "another target",
                Box::new(|tx| {
                    if let Some(nft) = tx.outputs[0].nft.as_mut() {
                        nft.1[8] ^= 1;
                    }
                }),
            ),
            (
                "another reference",
                Box::new(|tx| {
                    if let Some(nft) = tx.outputs[0].nft.as_mut() {
                        nft.1[12] ^= 1;
                    }
                }),
            ),
            (
                "a lock time below the coinbase height",
                Box::new(|tx| tx.locktime -= 1),
            ),
            ("a time lock", Box::new(|tx| tx.locktime = 1_700_000_000)),
            ("a final sequence", Box::new(|tx| tx.inputs[0].2 = u32::MAX)),
            (
                "801 satoshis on the donation",
                Box::new(|tx| {
                    tx.outputs[2].value += 1;
                    tx.outputs[0].value -= 1;
                }),
            ),
            (
                "donation and fee swapped",
                Box::new(|tx| tx.outputs.swap(2, 3)),
            ),
            (
                "a token moved from the fee to the donation",
                Box::new(|tx| {
                    tx.outputs[2].amount += 1;
                    tx.outputs[3].amount -= 1;
                }),
            ),
            (
                "the split withheld",
                Box::new(|tx| {
                    let mut pushes = parse_pushes(&tx.inputs[0].1).unwrap();
                    pushes[0].clear();
                    tx.inputs[0].1 = pushes.iter().flat_map(|data| push(data)).collect();
                }),
            ),
            (
                "another payout hash",
                Box::new(|tx| {
                    let mut pushes = parse_pushes(&tx.inputs[0].1).unwrap();
                    pushes[1] = double_sha256(&p2pkh(0x99)).to_vec();
                    tx.inputs[0].1 = pushes.iter().flat_map(|data| push(data)).collect();
                    tx.outputs[1].locking = p2pkh(0x99);
                }),
            ),
            (
                "another covenant",
                Box::new(|tx| {
                    let mut pushes = parse_pushes(&tx.inputs[0].1).unwrap();
                    pushes[3][100] ^= 1;
                    tx.inputs[0].1 = pushes.iter().flat_map(|data| push(data)).collect();
                }),
            ),
            (
                "the fee paid in another category",
                Box::new(|tx| {
                    tx.outputs[3].category = Some([0x77; 32]);
                }),
            ),
        ];
        assert_eq!(rebuild(&|_| ()), claim.raw, "the parser round-trips");
        for (why, edit) in cases {
            assert!(check(&rebuild(&*edit)).is_err(), "{why}");
        }
    }

    /// A change to a parsed claim.
    type Edit = Box<dyn Fn(&mut Tx)>;

    /// Writes a parsed transaction back (for the mutation tests).
    fn encode_tx(tx: &Tx) -> Vec<u8> {
        let mut raw = CLAIM_VERSION.to_le_bytes().to_vec();
        compact_size(tx.inputs.len() as u64, &mut raw);
        for (outpoint, unlocking, sequence) in &tx.inputs {
            raw.extend(outpoint.txid);
            raw.extend(outpoint.vout.to_le_bytes());
            compact_size(unlocking.len() as u64, &mut raw);
            raw.extend(unlocking);
            raw.extend(sequence.to_le_bytes());
        }
        compact_size(tx.outputs.len() as u64, &mut raw);
        for output in &tx.outputs {
            let mut bytecode = Vec::new();
            if let Some(category) = output.category {
                bytecode.push(0xef);
                bytecode.extend(category);
                let amount_flag = if output.amount > 0 { 0x10 } else { 0 };
                match &output.nft {
                    Some((capability, commitment)) if !commitment.is_empty() => {
                        bytecode.push(0x60 | amount_flag | capability);
                        compact_size(commitment.len() as u64, &mut bytecode);
                        bytecode.extend(commitment);
                    }
                    Some((capability, _)) => bytecode.push(0x20 | amount_flag | capability),
                    None => bytecode.push(amount_flag),
                }
                if output.amount > 0 {
                    compact_size(output.amount, &mut bytecode);
                }
            }
            bytecode.extend(&output.locking);
            raw.extend(super::output(output.value, &bytecode));
        }
        raw.extend(tx.locktime.to_le_bytes());
        raw
    }

    // #### merge-mining v2: heights
    // What: a coinbase at or below the state's height cannot claim; one
    // block newer can; a later lock time is fine and an earlier one is not.
    #[test]
    fn a_claim_needs_a_newer_coinbase_and_a_lock_time_at_its_height() {
        let (height, _, bits) = chipnet_block();
        for (state_height, ok) in [(height, false), (height + 5, false), (height - 1, true)] {
            let setup = Setup {
                state: ThreadState {
                    thread: 1,
                    height: state_height,
                    target_bits: 0x2000_ffff,
                    ref_bits: bits,
                },
                ..Setup::default()
            };
            if ok {
                assert!(scenario(setup).claim(1).is_ok());
            } else {
                // The covenant's own leaf cannot be built: mine one anyway.
                let mut deployment_setup = setup.clone();
                deployment_setup.state.height = height - 1;
                let mined = scenario(deployment_setup);
                let stale = Vault {
                    commitment: setup.state.encode(),
                    ..mined.vault.clone()
                };
                assert!(build_claim(
                    &mined.deployment,
                    &stale,
                    &mined.proof,
                    &mined.terms,
                    1,
                    None
                )
                .unwrap_err()
                .contains("not newer"));
            }
        }
        let scenario = scenario(Setup::default());
        let later = build_claim(
            &scenario.deployment,
            &scenario.vault,
            &scenario.proof,
            &scenario.terms,
            1,
            Some(height + 100),
        );
        assert!(later.is_ok());
        let earlier = build_claim(
            &scenario.deployment,
            &scenario.vault,
            &scenario.proof,
            &scenario.terms,
            1,
            Some(height - 1),
        );
        assert!(earlier.unwrap_err().contains("lock time"));
    }

    // #### merge-mining v2: the DAA
    // What: the claim target eases by min(dh, 288) / threads, and a leaf
    // committing an easier target than the covenant computes cannot claim.
    #[test]
    fn the_claim_target_eases_with_the_blocks_since_the_state() {
        let (height, _, bits) = chipnet_block();
        let state_bits = 0x1f00_ffff;
        for delta in [1u32, 4, 100, 288, 5_000] {
            let scenario = scenario(Setup {
                state: ThreadState {
                    thread: 1,
                    height: height - delta,
                    target_bits: state_bits,
                    ref_bits: bits,
                },
                ..Setup::default()
            });
            let claim = scenario.claim(1).unwrap();
            let factor = u64::from(delta.min(288)) * 5_000 / 4 + 5_000;
            let expected = encode_token_target(
                &(expand_token_target(state_bits).unwrap() * factor / 10_000u32),
            );
            assert_eq!(Some(claim.plan.target_bits), expected, "dh {delta}");
        }
        assert_eq!(claim_target_bits(0x2000_ffff, 10, 4), Some(0x2001_bffe));
        assert_eq!(claim_target_bits(0x207f_ffff, 288, 1), Some(0x207f_ffff));
        assert_eq!(claim_target_bits(0x0300_0001, 1, 4), Some(0x0300_0001));
        assert_eq!(claim_target_bits(0x2000_ffff, 0, 4), None);
        let easier = scenario(Setup {
            leaf_bits: Some(0x207f_ffff),
            ..Setup::default()
        });
        assert!(easier.claim(1).is_err());
    }

    // #### merge-mining v2: BCH headers only
    // What: a Bitcoin header's nBits fail the band; Chipnet's limit bits
    // pass; after 4,032 blocks without a claim the fallback band opens
    // around the state's reference, one block early it does not, and far
    // easier than the reference it does not.
    #[test]
    fn claims_need_bch_headers() {
        let (height, _, bits) = chipnet_block();
        assert!(scenario(Setup {
            bits: 0x1703_0ecd,
            ..Setup::default()
        })
        .claim(1)
        .unwrap_err()
        .contains("ASERT"));
        assert!(scenario(Setup {
            bits: POW_LIMIT_BITS,
            ..Setup::default()
        })
        .claim(1)
        .is_ok());
        let easy = 0x1b04_04cb;
        let (_, time, _) = chipnet_block();
        assert_eq!(
            asert_band(MiningNetwork::Chipnet, easy, time, height),
            Some(false)
        );
        let at = |delta: u32, ref_bits: u32| {
            scenario(Setup {
                bits: easy,
                state: ThreadState {
                    thread: 1,
                    height: height - delta,
                    target_bits: 0x2000_ffff,
                    ref_bits,
                },
                ..Setup::default()
            })
            .claim(1)
        };
        assert!(at(4_032, easy).is_ok());
        assert!(at(4_031, easy).is_err());
        assert!(at(4_032, bits).is_err());
    }

    // #### merge-mining v2: BCHN's ASERT rounding
    // What: with Chipnet's anchor, 100 blocks after it and one second
    // behind schedule, the exponent is -65536 / 3600: truncated to -18 (not
    // floored to -19), so shifts -1 and frac 65518 give the target
    // 0xfff2800c8 << 188; floor division would give 0xfff2000d << 192.
    #[test]
    fn asert_truncates_its_exponent_then_floors_its_shift() {
        let reference = decode_header_bits(0x1d00_ffff).unwrap();
        let target = asert_target(&reference, 600 * 101 - 1, 100, 3_600);
        let expected = BigUint::parse_bytes(b"fff2800c8", 16).unwrap() << 188u32;
        assert_eq!(target, expected);
        let floored = BigUint::parse_bytes(b"fff2000d", 16).unwrap() << 192u32;
        assert_ne!(target, floored);
        assert_eq!((-65_536i128 / 3_600) >> 16, -1);
        assert_eq!(-18i128 & 0xffff, 65_518);
        // A larger lag: exponent -32786 (floor would give -32787).
        let target = asert_target(&reference, 600 * 101 - 1_801, 100, 3_600);
        assert_eq!(
            target,
            BigUint::parse_bytes(b"b4f7cb078", 16).unwrap() << 188u32
        );
    }

    // #### merge-mining v2: the thread rule's vector (also in the spec)
    // What: a leaf paying P2PKH 0x12…12 at height 329,001 starts at thread
    // 0 of 4 (HASH256 90d29130…), at 329,002 at thread 3 (af5ced2f…).
    #[test]
    fn a_leafs_thread_follows_its_payout_and_height() {
        let payout_hash = double_sha256(&p2pkh(0x12));
        assert_eq!(
            hex::encode(payout_hash),
            "22650758a42ebfd7306e771ad24c442cad795ed9e549392233729f647a2b1be5"
        );
        let mut seed = payout_hash.to_vec();
        seed.extend(329_001u32.to_le_bytes());
        assert_eq!(
            hex::encode(double_sha256(&seed)),
            "90d29130927f1d6975eaa0ecfe310fb3534814632b447f1faa3c81ba67c0d7bf"
        );
        assert_eq!(thread_start(&payout_hash, 329_001, 4), 0);
        assert_eq!(thread_start(&payout_hash, 329_002, 4), 3);
        assert_eq!(thread_start(&payout_hash, 329_001, 3), 814_862_992 % 3);
        assert_eq!(thread_start(&payout_hash, 329_001, 1), 0);
    }

    // #### merge-mining v2: the vault's floor and the 35-byte scripts
    // What: a claim that would leave the vault 799 satoshis is refused by
    // the builder and by the check; a payout script of 36 bytes is refused.
    #[test]
    fn a_claim_leaves_the_vault_800_satoshis_and_pays_short_scripts() {
        let probe = scenario(Setup::default());
        let claim = probe.claim(1).unwrap();
        let spend = claim.plan.fee + OUTPUT_SATS;
        let at = |value: u64| {
            scenario(Setup {
                value,
                ..Setup::default()
            })
        };
        assert!(at(spend + 800).claim(1).is_ok());
        let short = at(spend + 799);
        assert!(short.claim(1).unwrap_err().contains("800"));
        // The check refuses it too, from bytes built with 800 and the
        // vault's value lowered by one.
        let good = at(spend + 800);
        let raw = good.claim(1).unwrap().raw;
        let less = Vault {
            value: good.vault.value - 1,
            ..good.vault.clone()
        };
        assert!(check_claim(&good.deployment, &less, &raw).is_err());
        let mut long = probe.terms.clone();
        long.payout = vec![0x51; 36];
        assert!(build_claim(
            &probe.deployment,
            &probe.vault,
            &probe.proof,
            &long,
            1,
            None
        )
        .unwrap_err()
        .contains("35 bytes"));
    }

    /// #### merge-mining v2: a claim's inputs in BCH P2Pool's
    /// `tools/safa-auxpow/cli.mjs claim` format: its `deployment.json` and
    /// `claim.json` (txids and the category in display order, amounts as
    /// decimal strings, scripts and the proof in hex).
    pub(crate) fn cli_inputs(
        scenario: &Scenario,
        fee_rate: u64,
    ) -> (serde_json::Value, serde_json::Value) {
        let deployment = serde_json::json!({
            "network": "chipnet",
            "threads": scenario.deployment.threads,
            "virtual_sats": scenario.deployment.virtual_sats.to_string(),
            "category": scenario.deployment.category_display(),
        });
        let mut txid = scenario.vault.outpoint.txid;
        txid.reverse();
        let mut claim = serde_json::json!({
            "outpoint": {"txid": hex::encode(txid), "vout": scenario.vault.outpoint.vout},
            "vault": {
                "value_sats": scenario.vault.value.to_string(),
                "token_amount": scenario.vault.tokens.to_string(),
                "commitment": hex::encode(scenario.vault.commitment),
            },
            "payout_script": hex::encode(&scenario.terms.payout),
            "proof": hex::encode(&scenario.proof),
            "fee_rate": fee_rate,
        });
        let split = &scenario.terms.split;
        if !split.is_none() {
            let script = |slot: &super::super::split::Slot, script: &Option<Vec<u8>>| {
                if slot.is_empty() {
                    String::new()
                } else {
                    script.as_deref().map(hex::encode).unwrap_or_default()
                }
            };
            claim["split"] = serde_json::json!({
                "donation_bps": split.donation_slot().bps(),
                "donation_script": script(split.donation_slot(), &scenario.terms.donation),
                "fee_bps": split.fee_slot().bps(),
                "fee_script": script(split.fee_slot(), &scenario.terms.fee),
            });
        }
        (deployment, claim)
    }

    /// #### merge-mining v2: BCH P2Pool's Chipnet tSAFA fixture, copied
    /// verbatim from `tools/safa-auxpow/test_data/tsafa-chipnet-fixture.json`
    /// in CyberAshven/BitcoinCash-p2poolv2 (PR #33, commit 4be6db6c): what
    /// P2Pool's `cli.mjs params` prints for a fake Chipnet deployment
    /// (funding outpoint 11…11:0, 4 threads, 84,000,000,000 virtual
    /// satoshis, 2,000,000 satoshis per thread), never broadcast.
    const P2POOL_FIXTURE: &str = include_str!("test_data/tsafa-chipnet-fixture.json");
    /// Pickaxe's slot for the hex P2Pool builds from the exported claim.
    const P2POOL_CLAIM: &str = include_str!("test_data/tsafa-chipnet-claim.json");

    /// The fixture's deployment, read by Pickaxe's loader from P2Pool's
    /// `params` and its category.
    pub(crate) fn fixture_deployment() -> Deployment {
        let fixture: serde_json::Value = serde_json::from_str(P2POOL_FIXTURE).unwrap();
        let mut json = fixture["params"].clone();
        json["version"] = 1.into();
        json["category"] = fixture["deployment"]["category"].clone();
        Deployment::from_json(&json.to_string()).unwrap()
    }

    // #### merge-mining v2: P2Pool's covenant, read by Pickaxe's loader
    // What: from the fixture's redeem script (1,364 bytes) the loader
    // derives P2Pool's K (1,944), lock, lock_hash and token-aware P2SH32
    // vault address, and accepts the fixture's own copies of them.
    // Look here if: Deployment::from_json or CLAIM_FIXED_BYTES changes.
    #[test]
    fn the_loader_reproduces_p2pools_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(P2POOL_FIXTURE).unwrap();
        let deployment = fixture_deployment();
        assert_eq!(deployment.redeem_script.len(), 1_364);
        assert!(hex::encode(&deployment.redeem_script).starts_with("45557f7c0543544d50"));
        assert!(hex::encode(&deployment.redeem_script).ends_with("6c6ea2695395a169756851"));
        assert_eq!(deployment.fee_k, 1_944);
        assert_eq!(
            hex::encode(&deployment.locking_bytecode),
            "aa20af6a8469e4d1cfca20761a9c3127b5e562d67b7156ad2b72e2919673b9f4de0787"
        );
        assert_eq!(
            hex::encode(deployment.lock_hash),
            "f1a16f57915e458318d4bb0078c7b1f3b753b9770ea3848437eae4abd1135caf"
        );
        assert_eq!(deployment.threads, 4);
        assert_eq!(deployment.virtual_sats, 84_000_000_000);
        assert_eq!(deployment.category, [0x11; 32]);
        // The token-aware P2SH32 CashAddr: type 3, a 32-byte hash.
        let address = crate::tx::cashaddr_with_version(
            0x1b,
            &deployment.locking_bytecode[2..34],
            MiningNetwork::Chipnet,
        );
        assert_eq!(
            address,
            fixture["params"]["vault_address"].as_str().unwrap()
        );
        assert_eq!(
            address,
            "bchtest:rwhk4prfungulj3qwcdfcvf8khjk94nmw9t262mju2gevuae7n0qwnf98a02d"
        );
    }

    /// The claim exported for P2Pool: the fixture's deployment, thread 0
    /// (the agreed rule's thread for P2PKH 0x12…12 at 329,001) ten blocks
    /// after its last claim, the fixture's seed and supply, and a 1%/1%
    /// split to P2PKH 0xd0…d0 and 0xfe…fe.
    fn fixture_scenario() -> Scenario {
        let fixture: serde_json::Value = serde_json::from_str(P2POOL_FIXTURE).unwrap();
        let (height, _, bits) = chipnet_block();
        let thread = thread_start(&double_sha256(&p2pkh(0x12)), height, 4);
        let amount = |key: &str| {
            fixture["genesis"][key]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        scenario_with(
            fixture_deployment(),
            Setup {
                state: ThreadState {
                    thread,
                    height: height - 10,
                    target_bits: 0x2000_ffff,
                    ref_bits: bits,
                },
                value: amount("seed_sats"),
                tokens: amount("supply_per_thread"),
                split: Some((100, 100)),
                ..Setup::default()
            },
        )
    }

    // #### merge-mining v2: the claim P2Pool builds too
    // What: the fixture's claim is built from fixed inputs (the reward is
    // 1,250,000,000: 12,500,000 to the donation, 12,375,000 to the fee).
    // With PICKAXE_SAFA_EXPORT naming a directory, its `deployment.json`
    // and `claim.json` (P2Pool's `cli.mjs claim` input) and Pickaxe's own
    // hex are written there. Once `tsafa-chipnet-claim.json` holds the hex
    // P2Pool builds from them, the two must be equal byte for byte.
    // Look here if: P2Pool's claim and Pickaxe's differ.
    #[test]
    fn the_fixture_claim_matches_p2pools() {
        let scenario = fixture_scenario();
        let claim = scenario.claim(1).unwrap();
        assert_eq!(claim.plan.reward, 1_250_000_000);
        assert_eq!(
            claim.plan.shares,
            Shares {
                miner: 1_225_125_000,
                donation: 12_500_000,
                fee: 12_375_000
            }
        );
        let (deployment_json, claim_json) = cli_inputs(&scenario, 1);
        assert_eq!(deployment_json["category"], "11".repeat(32));
        assert_eq!(claim_json["split"]["donation_bps"], 100);
        if let Some(directory) = std::env::var_os("PICKAXE_SAFA_EXPORT") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let write = |name: &str, value: &serde_json::Value| {
                std::fs::write(
                    directory.join(name),
                    serde_json::to_vec_pretty(value).unwrap(),
                )
                .unwrap()
            };
            write("deployment.json", &deployment_json);
            write("claim.json", &claim_json);
            write(
                "pickaxe-claim.json",
                &serde_json::json!({
                    "txid": claim.txid,
                    "hex": hex::encode(&claim.raw),
                    "size": claim.raw.len(),
                    "size_bound": claim.plan.size_bound,
                    "fee_sats": claim.plan.fee.to_string(),
                    "coinbase_height": claim.plan.height,
                    "target_bits": format!("{:08x}", claim.plan.target_bits),
                    "next_state": hex::encode(claim.plan.next_state),
                }),
            );
        }
        let slot: serde_json::Value = serde_json::from_str(P2POOL_CLAIM).unwrap();
        // BCH P2Pool's `cli.mjs claim` built the same transaction: the same
        // txid, which commits to every byte (and the same hex, where the
        // slot holds it).
        let txid = slot["p2pool_claim_txid"]
            .as_str()
            .expect("P2Pool's claim txid for the fixture");
        assert_eq!(claim.txid, txid);
        if let Some(theirs) = slot["p2pool_claim_hex"].as_str() {
            assert_eq!(hex::encode(&claim.raw), theirs);
        }
    }

    // #### merge-mining v2: the ASERT band's edges (P2Pool's vectors)
    #[test]
    fn the_asert_band_matches_p2pools_edges() {
        let mainnet = band_of(MiningNetwork::Mainnet);
        let at = |bits| {
            asert_band(
                MiningNetwork::Mainnet,
                bits,
                (mainnet.anchor_parent_time + 600) as u32,
                mainnet.anchor_height + 1,
            )
        };
        assert_eq!(at(0x1804_dafe), Some(true));
        assert_eq!(at(0x1802_6d7f), Some(true));
        assert_eq!(at(0x1802_6d7e), Some(false));
        assert_eq!(at(0x1805_1924), Some(true));
        assert_eq!(at(0x1805_1925), Some(false));
        assert_eq!(at(POW_LIMIT_BITS), Some(false));
        let chipnet = band_of(MiningNetwork::Chipnet);
        let height = chipnet.anchor_height + 1 + 120;
        let time = (chipnet.anchor_parent_time + 600) as u32;
        let computed = asert_target(
            &decode_header_bits(chipnet.anchor_bits).unwrap(),
            600,
            i64::from(height - 1 - chipnet.anchor_height),
            chipnet.half_life,
        );
        let at =
            |target: BigUint| asert_band(MiningNetwork::Chipnet, compact(&target), time, height);
        assert_eq!(at(computed.clone()), Some(true));
        assert_eq!(at(&computed / 8u32), Some(true));
        assert_eq!(at(&computed / 16u32), Some(false));
        // #### P2Pool's test-network fix: the easy side is × 8.
        assert_eq!(at(&computed * 2u32), Some(true));
        assert_eq!(at(&computed * 8u32), Some(true));
        assert_eq!(at(&computed * 9u32), Some(false));
        assert_eq!(
            asert_band(MiningNetwork::Chipnet, POW_LIMIT_BITS, time, height),
            Some(true)
        );
        // Real headers pass at their heights.
        let (time, bits) = time_and_bits(CHIPNET_329001);
        assert_eq!(
            asert_band(MiningNetwork::Chipnet, bits, time, 329_001),
            Some(true)
        );
        // #### P2Pool's test-network fix: Chipnet block 329,001 still
        // passes when dated up to 2 hours earlier (BCHN accepts a parent
        // dated up to 2 hours ahead); × 1.05 refused it from 260 seconds.
        for earlier in [200, 260, 600, 3_600, 7_200] {
            assert_eq!(
                asert_band(MiningNetwork::Chipnet, bits, time - earlier, 329_001),
                Some(true),
                "{earlier} s earlier"
            );
        }
        let (time, bits) = time_and_bits(MAINNET_974001);
        assert_eq!(
            asert_band(MiningNetwork::Mainnet, bits, time, 974_001),
            Some(true)
        );
        for btc in [0x1703_0ecd, 0x1702_349e] {
            assert_eq!(
                asert_band(MiningNetwork::Mainnet, btc, time, 974_001),
                Some(false)
            );
        }
        // Invalid nBits and heights at or below the anchor fail outright.
        for invalid in [
            0x0000_0000,
            0x0480_0001,
            0x2300_0001,
            0x2100_ffff,
            0x1d01_0000,
        ] {
            assert_eq!(
                asert_band(MiningNetwork::Mainnet, invalid, time, 974_001),
                None
            );
        }
        assert_eq!(
            asert_band(
                MiningNetwork::Mainnet,
                0x1804_dafe,
                time,
                mainnet.anchor_height
            ),
            None
        );
        // The fallback band.
        let reference = decode_header_bits(0x1804_dafe).unwrap();
        let band = |target: BigUint| fallback_band(compact(&target), 0x1804_dafe);
        assert_eq!(band(reference.clone()), Some(true));
        assert_eq!(band(&reference >> 4u32), Some(true));
        assert_eq!(
            band((&reference >> 4u32) - (BigUint::one() << 160u32)),
            Some(false)
        );
        assert_eq!(band(&reference * 11u32 / 10u32), Some(true));
        assert_eq!(band(&reference * 12u32 / 10u32), Some(false));
        assert_eq!(fallback_band(0, 0x1804_dafe), None);
    }

    /// BCHN's GetCompact, for the band tests.
    fn compact(target: &BigUint) -> u32 {
        let mut size = target.bits().div_ceil(8) as u32;
        let mut word = if size <= 3 {
            (target.to_u64().unwrap() << (8 * (3 - size))) as u32
        } else {
            (target >> (8 * (size - 3))).to_u32().unwrap()
        };
        if word & 0x0080_0000 != 0 {
            word >>= 8;
            size += 1;
        }
        word | size << 24
    }

    // #### merge-mining v2: targets, nBits and BIP34 heights
    #[test]
    fn targets_nbits_and_heights_read_as_the_covenant_reads_them() {
        for bits in [
            0x207f_ffff,
            0x0300_0001,
            0x1d00_ffff,
            0x2001_bffe,
            0x0400_8000,
        ] {
            let target = expand_token_target(bits).unwrap();
            assert_eq!(encode_token_target(&target), Some(bits), "{bits:08x}");
        }
        for invalid in [0x0200_0001, 0x2100_0001, 0x2080_0000, 0x1d00_0000] {
            assert_eq!(expand_token_target(invalid), None, "{invalid:08x}");
        }
        for (target, bits) in [
            (BigUint::one(), 0x0300_0001),
            (BigUint::from(0x7f_ffffu32), 0x037f_ffff),
            (BigUint::from(0x80_0000u32), 0x0400_8000),
            (BigUint::from(0x1_2345_6789u64), 0x0501_2345),
            (max_token_target(), 0x207f_ffff),
        ] {
            assert_eq!(encode_token_target(&target), Some(bits));
            assert!(expand_token_target(bits).unwrap() <= target);
        }
        assert_eq!(encode_token_target(&BigUint::zero()), None);
        assert_eq!(encode_token_target(&(max_token_target() + 1u32)), None);
        for bits in [
            0x1804_dafe,
            0x1d00_ffff,
            0x1a31_06ee,
            0x1703_0ecd,
            0x0101_0000,
            0x0300_ffff,
        ] {
            assert!(decode_header_bits(bits).is_some(), "{bits:08x}");
        }
        assert_eq!(decode_header_bits(0x0101_0000), Some(BigUint::one()));
        for invalid in [
            0,
            0x0480_0001,
            0x2300_0001,
            0x2100_ffff,
            0x1d01_0000,
            0x207f_ffff,
            0x0480_0000,
        ] {
            assert_eq!(decode_header_bits(invalid), None, "{invalid:08x}");
        }
        let head = |push: &str| {
            let mut script = hex::decode(push).unwrap();
            script.extend([7; 8]);
            let mut head = 2u32.to_le_bytes().to_vec();
            head.push(1);
            head.extend([0; 32]);
            head.extend([0xff; 4]);
            head.push(script.len() as u8);
            head.extend(&script);
            head.extend([0xff; 4]);
            head.push(2);
            head
        };
        assert_eq!(coinbase_height(&head("0315f904")), Some(325_909));
        assert_eq!(coinbase_height(&head("0111")), Some(17));
        assert_eq!(coinbase_height(&head("028000")), Some(128));
        for push in ["021100", "0181", "51", "00"] {
            assert_eq!(coinbase_height(&head(push)), None, "{push}");
        }
    }

    // #### merge-mining v2: the deployment file
    // What: a Chipnet file is read, its lock and K derived; mainnet, a bad
    // category, no threads, too large a reserve, a mismatched fee_k or
    // lock, and a redeem script without this file's constants are refused.
    #[test]
    fn deployment_files_are_checked() {
        let redeem = redeem(1_366, 84_000_000_000);
        let good = deployment_json(&redeem, 84_000_000_000);
        let deployment = Deployment::from_json(&good.to_string()).unwrap();
        assert_eq!(deployment.network, MiningNetwork::Chipnet);
        assert_eq!(deployment.threads, 4);
        assert_eq!(deployment.fee_k, 1_946);
        assert_eq!(deployment.locking_bytecode.len(), 35);
        assert_eq!(&deployment.locking_bytecode[2..34], &double_sha256(&redeem));
        assert_eq!(
            deployment.lock_hash,
            double_sha256(&deployment.locking_bytecode)
        );
        assert_eq!(
            deployment.category_display(),
            hex::encode(double_sha256(b"tSAFA test category"))
        );
        let mut scripthash: Hash = Sha256::digest(&deployment.locking_bytecode).into();
        scripthash.reverse();
        assert_eq!(deployment.script_hash_hex(), hex::encode(scripthash));
        let with = |key: &str, value: serde_json::Value| {
            let mut json = good.clone();
            json[key] = value;
            Deployment::from_json(&json.to_string())
        };
        assert!(with("fee_k", 1_946.into()).is_ok());
        assert!(with(
            "locking_bytecode",
            hex::encode(&deployment.locking_bytecode).into()
        )
        .is_ok());
        assert!(with("lock_hash", hex::encode(deployment.lock_hash).into()).is_ok());
        assert!(with("threads", 63.into()).is_ok());
        assert!(with("network", "mainnet".into())
            .unwrap_err()
            .contains("Chipnet only"));
        for (key, value) in [
            ("version", serde_json::json!(2)),
            ("network", "testnet4".into()),
            ("category", "00".repeat(32).into()),
            ("category", "zz".into()),
            ("threads", 0.into()),
            // #### At most 63 threads, as P2Pool's tooling allows.
            ("threads", 64.into()),
            ("virtual_sats", "4611686018427387905".into()),
            ("virtual_sats", "-1".into()),
            ("redeem_script", "".into()),
            ("fee_k", 1_947.into()),
            ("redeem_script_bytes", 1_365.into()),
            ("lock_hash", "00".repeat(32).into()),
            ("name", "".into()),
        ] {
            assert!(with(key, value.clone()).is_err(), "{key} = {value}");
        }
        // Another reserve than the script carries.
        assert!(with("virtual_sats", "42000000000".into())
            .unwrap_err()
            .contains("virtual reserve"));
        assert!(Deployment::from_json("[1]").is_err());
    }

    // #### merge-mining v2: vaults from Fulcrum
    // What: the vaults are this category's outputs with a mutable NFT
    // whose state names a thread; another category, a fungible-only output
    // and a thread beyond the deployment's are left out.
    #[test]
    fn vaults_are_read_from_fulcrums_listunspent() {
        let deployment = deployment(42_000_000_000);
        let category = deployment.category_display();
        let state = |thread: u32| {
            hex::encode(
                ThreadState {
                    thread,
                    height: 100,
                    target_bits: 0x1d00_ffff,
                    ref_bits: 0x1d00_ffff,
                }
                .encode(),
            )
        };
        let unspent = serde_json::json!([
            {"tx_hash": "11".repeat(32), "tx_pos": 0, "height": 10, "value": 2_000_000,
             "token_data": {"category": category, "amount": "525000000000000",
                            "nft": {"capability": "mutable", "commitment": state(2)}}},
            {"tx_hash": "22".repeat(32), "tx_pos": 1, "height": 0, "value": 1_000_000,
             "token_data": {"category": category, "amount": "1",
                            "nft": {"capability": "mutable", "commitment": state(0)}}},
            {"tx_hash": "33".repeat(32), "tx_pos": 0, "height": 10, "value": 800,
             "token_data": {"category": category, "amount": "5"}},
            {"tx_hash": "44".repeat(32), "tx_pos": 0, "height": 10, "value": 800,
             "token_data": {"category": "55".repeat(32), "amount": "5",
                            "nft": {"capability": "mutable", "commitment": state(1)}}},
            {"tx_hash": "66".repeat(32), "tx_pos": 0, "height": 10, "value": 800,
             "token_data": {"category": category, "amount": "5",
                            "nft": {"capability": "mutable", "commitment": state(9)}}},
            {"tx_hash": "77".repeat(32), "tx_pos": 0, "height": 10, "value": 800},
        ]);
        let vaults = vaults_from_listunspent(&deployment, &unspent).unwrap();
        assert_eq!(vaults.iter().map(Vault::thread).collect::<Vec<_>>(), [0, 2]);
        assert!(!vaults[0].confirmed);
        assert!(vaults[1].confirmed);
        assert_eq!(vaults[1].tokens, 525_000_000_000_000);
        assert_eq!(vaults[1].outpoint.txid, [0x11; 32]);
        assert!(vaults_from_listunspent(&deployment, &serde_json::json!({})).is_err());
    }

    // #### merge-mining v2: minimal pushes
    #[test]
    fn pushes_are_minimal() {
        assert_eq!(push(&[]), [0x00]);
        assert_eq!(push(&[5]), [0x55]);
        assert_eq!(push(&[0x81]), [0x4f]);
        assert_eq!(push(&[0x80]), [0x01, 0x80]);
        assert_eq!(push(&[0; 76])[..2], [0x4c, 76]);
        assert_eq!(push(&[0; 256])[..3], [0x4d, 0, 1]);
        assert_eq!(number_push(1_946), [0x02, 0x9a, 0x07]);
        assert_eq!(number_push(420_000), [0x03, 0xa0, 0x68, 0x06]);
        assert_eq!(number_push(128), [0x02, 0x80, 0x00]);
        assert_eq!(
            number_push(84_000_000_000),
            [0x05, 0x00, 0x48, 0xca, 0x8e, 0x13]
        );
        let script = [push(&[]), push(&[0x22; 32]), push(&[0x33; 300])].concat();
        assert_eq!(parse_pushes(&script).unwrap().len(), 3);
        assert!(parse_pushes(&[0x01, 0x05]).is_err(), "a one-byte 5 is OP_5");
        assert!(parse_pushes(&[0x76]).is_err());
    }
}
