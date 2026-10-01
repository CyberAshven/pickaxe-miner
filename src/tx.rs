//! Win-tx: CashAddr and the authoritative two-output PHOTON serializer.
//! Template assemble + message hash here. Schnorr sign stays Lead Dev / crypto.
//! Never keys/mnemonics. No broadcast until explicitly armed.

#[cfg(test)]
use crate::config::DONATION_ADDRESS;
use crate::protocol::{PhotonDeployment, MAINNET_PHOTON};
use sha2::Digest;

const CASHADDR_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const CASHADDR_GENERATORS: [u64; 5] = [
    0x0098_f2bc_8e61,
    0x0079_b76d_99e2,
    0x00f3_3e5f_b3c4,
    0x00ae_2eab_e2a8,
    0x001e_4f43_e470,
];

/// Decode BCH mainnet/testnet P2PKH (or token-aware P2PKH) to 25-byte locking bytecode.
pub fn cashaddr_to_p2pkh_locking(address: &str) -> Result<Vec<u8>, String> {
    let raw = address.trim();
    let has_lower = raw.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = raw.chars().any(|c| c.is_ascii_uppercase());
    if has_lower && has_upper {
        return Err("CashAddr must not mix upper and lower case".into());
    }
    let mut normalized = raw.to_ascii_lowercase();
    if !normalized.contains(':') {
        normalized = format!("bitcoincash:{normalized}");
    }
    let (prefix, payload_text) = normalized
        .split_once(':')
        .ok_or("CashAddr must contain exactly one prefix separator")?;
    if payload_text.contains(':') || !matches!(prefix, "bitcoincash" | "bchtest") {
        return Err("expected bitcoincash: or bchtest: address".into());
    }
    let mut values = Vec::new();
    for ch in payload_text.bytes() {
        let idx = CASHADDR_CHARSET
            .iter()
            .position(|&c| c == ch)
            .ok_or("invalid CashAddr character")?;
        values.push(idx as u8);
    }
    if values.len() < 9 {
        return Err("CashAddr too short".into());
    }
    let polymod_input: Vec<u8> = prefix
        .bytes()
        .map(|c| c & 31)
        .chain(std::iter::once(0))
        .chain(values.iter().copied())
        .collect();
    if cashaddr_polymod(&polymod_input) != 0 {
        return Err("CashAddr checksum invalid".into());
    }
    let payload = &values[..values.len() - 8];
    let decoded = convert_bits(payload, 5, 8, false)?;
    if decoded.len() != 21 {
        return Err("expected 20-byte P2PKH CashAddr".into());
    }
    let version = decoded[0];
    let typ = version >> 3;
    let size_code = version & 7;
    if (typ != 0 && typ != 2) || size_code != 0 {
        return Err("payout must be 20-byte P2PKH / token-aware P2PKH".into());
    }
    let hash = &decoded[1..];
    let mut out = Vec::with_capacity(25);
    out.extend_from_slice(&[0x76, 0xa9, 0x14]);
    out.extend_from_slice(hash);
    out.extend_from_slice(&[0x88, 0xac]);
    Ok(out)
}

/// Computes the CashAddr checksum polynomial.
fn cashaddr_polymod(values: &[u8]) -> u64 {
    let mut c: u64 = 1;
    for &v in values {
        let c0 = c >> 35;
        c = ((c & 0x07_ffff_ffff) << 5) ^ u64::from(v);
        for (i, generator) in CASHADDR_GENERATORS.iter().enumerate() {
            if ((c0 >> i) & 1) != 0 {
                c ^= generator;
            }
        }
    }
    c ^ 1
}

/// Encode a mainnet P2PKH hash as a canonical CashAddr.
pub fn p2pkh_hash_to_cashaddr(hash: &[u8; 20]) -> Result<String, String> {
    p2pkh_hash_to_cashaddr_for_network(hash, crate::config::MiningNetwork::Mainnet)
}

/// Encodes a P2PKH hash with the selected chain's canonical CashAddr prefix.
pub fn p2pkh_hash_to_cashaddr_for_network(
    hash: &[u8; 20],
    network: crate::config::MiningNetwork,
) -> Result<String, String> {
    p2pkh_hash_to_cashaddr_with_type(hash, network, 0)
}

/// Encodes a token-aware P2PKH payout with the selected chain's prefix.
pub fn token_p2pkh_hash_to_cashaddr_for_network(
    hash: &[u8; 20],
    network: crate::config::MiningNetwork,
) -> Result<String, String> {
    p2pkh_hash_to_cashaddr_with_type(hash, network, 2)
}

fn p2pkh_hash_to_cashaddr_with_type(
    hash: &[u8; 20],
    network: crate::config::MiningNetwork,
    address_type: u8,
) -> Result<String, String> {
    let mut decoded = Vec::with_capacity(21);
    decoded.push(address_type << 3); // 160-bit P2PKH or token-aware P2PKH
    decoded.extend_from_slice(hash);
    let payload = convert_bits(&decoded, 8, 5, true)?;
    let prefix = match network {
        crate::config::MiningNetwork::Mainnet => "bitcoincash",
        crate::config::MiningNetwork::Chipnet => "bchtest",
    };
    let mut checksum_input: Vec<u8> = prefix.bytes().map(|c| c & 31).collect();
    checksum_input.push(0);
    checksum_input.extend_from_slice(&payload);
    checksum_input.extend_from_slice(&[0u8; 8]);
    let checksum = cashaddr_polymod(&checksum_input);

    let mut encoded = String::with_capacity(payload.len() + 8);
    for value in payload {
        encoded.push(CASHADDR_CHARSET[value as usize] as char);
    }
    for shift in (0..8).rev() {
        let value = ((checksum >> (shift * 5)) & 31) as usize;
        encoded.push(CASHADDR_CHARSET[value] as char);
    }
    Ok(format!("{prefix}:{encoded}"))
}

/// Converts CashAddr payload groups between bit widths.
fn convert_bits(data: &[u8], from_bits: u32, to_bits: u32, pad: bool) -> Result<Vec<u8>, String> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut result = Vec::new();
    let max_v = (1u32 << to_bits) - 1;
    let max_acc = (1u32 << (from_bits + to_bits - 1)) - 1;
    for &value in data {
        if u32::from(value) >> from_bits != 0 {
            return Err("invalid CashAddr data value".into());
        }
        acc = ((acc << from_bits) | u32::from(value)) & max_acc;
        bits += from_bits;
        while bits >= to_bits {
            bits -= to_bits;
            result.push(((acc >> bits) & max_v) as u8);
        }
    }
    if pad {
        if bits > 0 {
            result.push(((acc << (to_bits - bits)) & max_v) as u8);
        }
    } else if bits >= from_bits || ((acc << (to_bits - bits)) & max_v) != 0 {
        return Err("invalid CashAddr padding".into());
    }
    Ok(result)
}

/// Concatenates serialized transaction byte fragments.
fn concat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// Serializes a 32-bit integer in little-endian order.
fn u32_le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// Serializes a 64-bit integer in little-endian order.
fn u64_le(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

/// Encodes a Bitcoin compact-size integer.
fn compact_uint(v: u64) -> Vec<u8> {
    if v < 0xfd {
        vec![v as u8]
    } else if v <= 0xffff {
        let mut o = vec![0xfd];
        o.extend_from_slice(&(v as u16).to_le_bytes());
        o
    } else if v <= 0xffff_ffff {
        let mut o = vec![0xfe];
        o.extend_from_slice(&(v as u32).to_le_bytes());
        o
    } else {
        let mut o = vec![0xff];
        o.extend_from_slice(&v.to_le_bytes());
        o
    }
}

/// Reverses hash bytes between wire and display order.
fn reverse_bytes(b: &[u8]) -> Vec<u8> {
    b.iter().rev().copied().collect()
}

/// Encodes a positive script number as a push operation.
fn encode_positive_script_number_push(age: u32) -> Result<Vec<u8>, String> {
    if age == 0 {
        return Ok(vec![0x00]);
    }
    if (1..=16).contains(&age) {
        return Ok(vec![0x50 + age as u8]);
    }
    let mut v = age as u64;
    let mut bytes = Vec::new();
    while v > 0 {
        bytes.push((v & 0xff) as u8);
        v >>= 8;
    }
    if bytes.last().copied().unwrap_or(0) & 0x80 != 0 {
        bytes.push(0);
    }
    if bytes.len() > 75 {
        return Err("Age script number unexpectedly large.".into());
    }
    let mut out = vec![bytes.len() as u8];
    out.extend_from_slice(&bytes);
    Ok(out)
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    hex::decode(s.trim()).map_err(|e| e.to_string())
}

/// CompactVarInt for CashToken FT amount (same as Bitcoin Cash varint / CompactSize).
fn compact_token_amount(amount: u128) -> Result<Vec<u8>, String> {
    if amount > u64::MAX as u128 {
        return Err("token amount exceeds u64 compact encoding used by reference".into());
    }
    Ok(compact_uint(amount as u64))
}

/// Byte positions of a PHOTON mining transaction for one baton age.
///
/// The input script pushes the age as a minimal script number, so the
/// transaction is 615 bytes for age 0..=16 and one byte longer per extra
/// number byte: 616 for 17..=127, 617 for 128..=32767, 618 for
/// 32768..=65534. The covenant rejects age >= 65535. Every byte after the
/// age push, including the nonce, target and signature, moves by `shift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhotonLayout {
    shift: usize,
}

impl PhotonLayout {
    /// Layout of the age 0..=16 reference vector.
    pub const BASE: Self = Self { shift: 0 };
    /// Largest layout shift the covenant's age bound allows.
    pub const MAX_SHIFT: usize = 3;
    /// Covenant bound: `age < 65535`.
    pub const MAX_AGE: u32 = 65_534;

    /// Returns the layout for a baton age the covenant accepts.
    pub fn for_age(age: u32) -> Result<Self, String> {
        if age > Self::MAX_AGE {
            return Err(format!(
                "baton age {age} is outside the PHOTON covenant bound (age < 65535)"
            ));
        }
        let push_bytes = encode_positive_script_number_push(age)?.len();
        Ok(Self {
            shift: push_bytes - 1,
        })
    }

    /// Includes the selected redeem script's byte width in the GPU offsets.
    /// Current CUDA binaries provide shifts 0..=3, so wider layouts fail
    /// before a job reaches a GPU kernel.
    pub fn for_age_with_deployment(
        age: u32,
        deployment: &PhotonDeployment,
    ) -> Result<Self, String> {
        let age_shift = Self::for_age(age)?.shift;
        deployment.verify()?;
        let redeem_len = deployment.redeem_script_hex.trim().len() / 2;
        let mainnet_len = MAINNET_PHOTON.redeem_script_hex.trim().len() / 2;
        let redeem_shift = redeem_len
            .checked_sub(mainnet_len)
            .ok_or("PHOTON deployment redeem script is shorter than CUDA's base layout")?;
        let shift = age_shift + redeem_shift;
        if shift > Self::MAX_SHIFT {
            return Err(format!(
                "PHOTON deployment at baton age {age} needs GPU layout shift {shift}; available CUDA kernels support 0..={} only",
                Self::MAX_SHIFT
            ));
        }
        Ok(Self { shift })
    }

    /// Returns the layout of a serialized mining transaction of `len` bytes.
    pub fn for_tx_len(len: usize) -> Result<Self, String> {
        let shift = len
            .checked_sub(Self::BASE.tx_bytes())
            .filter(|shift| *shift <= Self::MAX_SHIFT)
            .ok_or_else(|| {
                format!("PHOTON mining transaction is {len} bytes; expected 615..=618")
            })?;
        Ok(Self { shift })
    }

    /// Bytes the age push adds beyond the one-byte reference layout.
    pub const fn shift(self) -> usize {
        self.shift
    }

    /// Serialized transaction length.
    pub const fn tx_bytes(self) -> usize {
        615 + self.shift
    }

    /// Offset of the 4-byte little-endian commitment nonce.
    pub const fn nonce_offset(self) -> usize {
        390 + self.shift
    }

    /// Offset of the 32-byte little-endian target.
    pub const fn target_offset(self) -> usize {
        394 + self.shift
    }

    /// Offset of the 64-byte Schnorr signature.
    pub const fn signature_offset(self) -> usize {
        426 + self.shift
    }
}

/// Checks that the real transaction bytes equal the serialization the
/// covenant rebuilds for its proof-of-work hash.
///
/// The covenant always writes the remaining baton amount as `0xff` plus 8
/// bytes, and the reward as `0xff` plus 8 bytes once it needs more than a
/// 4-byte script number, with the reward output length fixed at 68 bytes.
/// The CompactSize encoding in the real transaction matches that only when
/// both amounts exceed `u32::MAX`; below that the miner would hash a
/// different preimage than the covenant checks.
pub fn require_covenant_hash_preimage(
    contract_token_amount: u128,
    reward_amount: u128,
) -> Result<(), String> {
    let remaining = contract_token_amount
        .checked_sub(reward_amount)
        .ok_or("reward exceeds contract token amount")?;
    if reward_amount <= u128::from(u32::MAX) || remaining <= u128::from(u32::MAX) {
        return Err(format!(
            "PHOTON reward {reward_amount} or remaining amount {remaining} is at or below 2^32-1; the covenant then hashes a different serialization than the transaction"
        ));
    }
    Ok(())
}

/// Moves `j` tokens from the reward back to the baton while preserving the
/// covenant's fixed nine-byte CompactSize amount serialization.
pub fn t2_reward_amount(
    contract_token_amount: u128,
    reward_amount: u128,
    j: u16,
) -> Result<u128, String> {
    let moved = reward_amount
        .checked_sub(u128::from(j))
        .ok_or("T2 adjustment exceeds the reward")?;
    require_covenant_hash_preimage(contract_token_amount, moved)?;
    Ok(moved)
}

/// Reads the reward in a PHOTON parent, checking T2's bounded, conserved
/// amount change before the caller reconstructs the full signed transaction.
pub fn t2_parent_reward_amount(
    parent: &[u8],
    contract_token_amount: u128,
    base_reward: u128,
) -> Result<u128, String> {
    let shift = PhotonLayout::for_tx_len(parent.len())?.shift();
    if parent[490 + shift] != 0xff || parent[577 + shift] != 0xff {
        return Err("T2 parent token amount markers are not 8-byte CompactSize".into());
    }
    let baton = u128::from(u64::from_le_bytes(
        parent[491 + shift..499 + shift].try_into().unwrap(),
    ));
    let reward = u128::from(u64::from_le_bytes(
        parent[578 + shift..586 + shift].try_into().unwrap(),
    ));
    if baton.checked_add(reward) != Some(contract_token_amount) {
        return Err("T2 parent does not conserve the job token supply".into());
    }
    let j = base_reward
        .checked_sub(reward)
        .ok_or("T2 parent reward exceeds the job reward")?;
    let j = u16::try_from(j).map_err(|_| "T2 parent reward adjustment exceeds 65535")?;
    if t2_reward_amount(contract_token_amount, base_reward, j)? != reward {
        return Err("T2 parent reward adjustment is invalid".into());
    }
    Ok(reward)
}

/// Inputs for the reference single-payout PHOTON template (2 outputs).
pub struct TemplateParams {
    pub prev_tx_hash_hex: String,
    pub prev_index: u32,
    pub age: u32,
    pub public_key_hex: String,
    pub target_hex: String,
    pub signature_hex: String,
    pub nonce: u32,
    pub contract_value_sats: u64,
    pub contract_token_amount: u128,
    pub reward_amount: u128,
    pub payout_locking: Vec<u8>,
}

/// Reference layout from miner.js `buildPhotonTemplateBytes` (single reward output).
pub fn build_photon_template_bytes(p: &TemplateParams) -> Result<Vec<u8>, String> {
    build_photon_template_bytes_for_deployment(p, &MAINNET_PHOTON)
}

/// Builds a PHOTON parent for its selected contract deployment.
pub fn build_photon_template_bytes_for_deployment(
    p: &TemplateParams,
    deployment: &PhotonDeployment,
) -> Result<Vec<u8>, String> {
    deployment.verify()?;
    let layout = PhotonLayout::for_age_with_deployment(p.age, deployment)?;
    let public_key = parse_hex(&p.public_key_hex)?;
    let target = parse_hex(&p.target_hex)?;
    let signature = parse_hex(&p.signature_hex)?;
    let redeem_script = parse_hex(deployment.redeem_script_hex.trim())?;
    let category = parse_hex(deployment.category_hex)?;
    let covenant_lock = parse_hex(deployment.covenant_lock_hex)?;

    if public_key.len() != 33 {
        return Err("Compressed public key must be 33 bytes.".into());
    }
    if target.len() != 32 {
        return Err("PHOTON target must be 32 bytes.".into());
    }
    if signature.len() != 64 {
        return Err("PHOTON commitment signature must be 64 bytes.".into());
    }
    if p.payout_locking.len() != 25 {
        return Err("Payout locking bytecode must be P2PKH (25 bytes).".into());
    }

    let age_push = encode_positive_script_number_push(p.age)?;
    let redeem_len = u16::try_from(redeem_script.len())
        .map_err(|_| "PHOTON redeem script exceeds PUSHDATA2 length")?;
    let input_script = concat(&[
        &[0x21],
        &public_key,
        &age_push,
        &[0x4d, redeem_len as u8, (redeem_len >> 8) as u8],
        &redeem_script,
    ]);

    let mut commitment = Vec::new();
    commitment.extend_from_slice(&u32_le(p.nonce));
    commitment.extend_from_slice(&target);
    commitment.extend_from_slice(&signature);

    let remaining = p
        .contract_token_amount
        .checked_sub(p.reward_amount)
        .ok_or("reward exceeds contract token amount")?;

    let cat_rev = reverse_bytes(&category);
    let mut output0 = Vec::new();
    output0.push(0xef);
    output0.extend_from_slice(&cat_rev);
    output0.push(0x71);
    output0.extend_from_slice(&compact_uint(commitment.len() as u64));
    output0.extend_from_slice(&commitment);
    output0.extend_from_slice(&compact_token_amount(remaining)?);
    output0.extend_from_slice(&covenant_lock);

    let mut output1 = Vec::new();
    output1.push(0xef);
    output1.extend_from_slice(&cat_rev);
    output1.push(0x10);
    output1.extend_from_slice(&compact_token_amount(p.reward_amount)?);
    output1.extend_from_slice(&p.payout_locking);

    let prev = reverse_bytes(&parse_hex(&p.prev_tx_hash_hex)?);
    if prev.len() != 32 {
        return Err("prev tx hash must be 32 bytes".into());
    }

    let max_baton_decrease_sats = deployment.single_input_max_baton_decrease_sats()?;
    let baton_sats = p
        .contract_value_sats
        .checked_sub(max_baton_decrease_sats)
        .ok_or("contract value too small for PHOTON parent covenant budget")?;

    let mut tx = Vec::new();
    tx.extend_from_slice(&u32_le(2)); // version
    tx.push(1); // input count
    tx.extend_from_slice(&prev);
    tx.extend_from_slice(&u32_le(p.prev_index));
    tx.extend_from_slice(&compact_uint(input_script.len() as u64));
    tx.extend_from_slice(&input_script);
    tx.extend_from_slice(&u32_le(p.age)); // sequence = age in reference
    tx.push(2); // output count
    tx.extend_from_slice(&u64_le(baton_sats));
    tx.extend_from_slice(&compact_uint(output0.len() as u64));
    tx.extend_from_slice(&output0);
    tx.extend_from_slice(&u64_le(700));
    tx.extend_from_slice(&compact_uint(output1.len() as u64));
    tx.extend_from_slice(&output1);
    tx.extend_from_slice(&u32_le(0)); // locktime
    if tx.len() != layout.tx_bytes() {
        return Err(format!(
            "PHOTON deployment serialized {} bytes; expected {} at age {}",
            tx.len(),
            layout.tx_bytes(),
            p.age
        ));
    }
    Ok(tx)
}

/// Changes the unsigned BCH value of the miner's P2PKH output.
/// The range respects the 675-satoshi token dust floor and the default
/// 1 sat/byte relay fee for this fixed two-output PHOTON layout.
pub fn set_payout_value_sats(tx: &mut [u8], sats: u16) -> Result<(), String> {
    let layout = PhotonLayout::for_tx_len(tx.len())?;
    let max_sats = 1_500usize
        .checked_sub(tx.len())
        .ok_or("PHOTON transaction exceeds its 1500-satoshi output budget")?;
    if sats < 675 || usize::from(sats) > max_sats {
        return Err(format!(
            "payout BCH value {sats} is outside 675..={max_sats}"
        ));
    }
    let offset = 534 + layout.shift();
    if tx[offset..offset + 8] != 700u64.to_le_bytes() {
        return Err("PHOTON payout BCH value is not the expected 700 satoshis".into());
    }
    tx[offset..offset + 8].copy_from_slice(&u64::from(sats).to_le_bytes());
    Ok(())
}

/// Reads and bounds the unsigned BCH payout value in a PHOTON parent.
pub fn payout_value_sats(tx: &[u8]) -> Result<u16, String> {
    let layout = PhotonLayout::for_tx_len(tx.len())?;
    let offset = 534 + layout.shift();
    let sats = u64::from_le_bytes(tx[offset..offset + 8].try_into().unwrap());
    let max_sats = 1_500usize
        .checked_sub(tx.len())
        .ok_or("PHOTON transaction exceeds its 1500-satoshi output budget")?;
    if sats < 675 || sats > max_sats as u64 {
        return Err(format!(
            "PHOTON payout BCH value {sats} is outside relay bounds"
        ));
    }
    u16::try_from(sats).map_err(|_| "PHOTON payout BCH value exceeds u16".into())
}

/// Historical 3-output 98/2 experiment, now fail-closed because the covenant
/// reference only proves a two-output mining transaction.
#[cfg(test)]
pub const DONATION_SPLIT_BLOCKER: &str =
    "98/2 same-transaction donation is disabled: the proven PHOTON covenant transaction has exactly two outputs, and no 3-output covenant-valid vector has been demonstrated";

#[cfg(test)]
pub fn build_photon_template_donation_split(
    p: &TemplateParams,
    donation_locking: &[u8],
) -> Result<Vec<u8>, String> {
    let _ = (p, donation_locking);
    Err(DONATION_SPLIT_BLOCKER.into())
}

/// Lines for the unsigned two-output parent and the settlement child `mine` pays.
pub fn win_tx_preview_lines(
    job_reward_raw: u128,
    miner_payout: &str,
) -> Result<Vec<String>, String> {
    let miner_lock = cashaddr_to_p2pkh_locking(miner_payout)?;
    let (miner_tokens, donation_tokens) =
        crate::config::RuntimeConfig::split_reward(job_reward_raw);
    Ok(vec![
        "win-tx preview (unsigned two-output parent; no mining or broadcast):".into(),
        format!(
            "  parent reward output FT={job_reward_raw} lock={}B -> {miner_payout}",
            miner_lock.len()
        ),
        "  this template has no donation output".into(),
        format!("  mine settlement pays FT={miner_tokens} -> {miner_payout}; total fee FT={donation_tokens}"),
    ])
}

/// Offline-style preview of the unsigned two-output PHOTON parent template.
pub fn print_win_tx_preview(
    job_reward_raw: u128,
    miner_payout: &str,
    template_hex: Option<&str>,
) -> Result<(), String> {
    for line in win_tx_preview_lines(job_reward_raw, miner_payout)? {
        println!("{line}");
    }
    if let Some(h) = template_hex {
        println!("  template: {} bytes", h.len() / 2);
        let show = h.len().min(80);
        println!("  hex[:{}]: {}…", show / 2, &h[..show]);
    }
    Ok(())
}

/// PHOTON M1 message = nonce_le (4) || target (32). Hash = SHA256(message) for Schnorr msg32.
pub fn photon_message_sha256(nonce: u32, target_hex: &str) -> Result<[u8; 32], String> {
    let target = parse_hex(target_hex)?;
    if target.len() != 32 {
        return Err("PHOTON target must be 32 bytes".into());
    }
    let mut msg = [0u8; 36];
    msg[..4].copy_from_slice(&nonce.to_le_bytes());
    msg[4..].copy_from_slice(&target);
    let dig = sha2::Sha256::digest(msg);
    let mut out = [0u8; 32];
    out.copy_from_slice(&dig);
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceJobContext {
    pub prev_txid: String,
    pub prev_vout: u32,
    pub age: u32,
    pub target_le_hex: String,
    pub contract_value_sats: u64,
    pub contract_token_amount: u128,
    pub reward_raw: u128,
}

/// Build the proven two-output candidate and arm it only if both Schnorr and
/// PHOTON proof-of-work checks pass.
pub fn apply_reference_signature(
    job: &ReferenceJobContext,
    miner_payout: &str,
    public_key_hex: &str,
    nonce: u32,
    signature_hex: &str,
) -> Result<Vec<u8>, String> {
    apply_reference_signature_for_deployment(
        job,
        miner_payout,
        public_key_hex,
        nonce,
        signature_hex,
        &MAINNET_PHOTON,
    )
}

/// Rebuilds and validates a signed parent against its selected covenant.
pub fn apply_reference_signature_for_deployment(
    job: &ReferenceJobContext,
    miner_payout: &str,
    public_key_hex: &str,
    nonce: u32,
    signature_hex: &str,
    deployment: &PhotonDeployment,
) -> Result<Vec<u8>, String> {
    apply_reference_signature_with_payout_sats_for_deployment(
        job,
        miner_payout,
        public_key_hex,
        nonce,
        signature_hex,
        None,
        deployment,
    )
}

/// Rebuilds a V-coordinate winner with its exact unsigned BCH payout value.
#[cfg(test)]
pub fn apply_reference_signature_with_payout_sats(
    job: &ReferenceJobContext,
    miner_payout: &str,
    public_key_hex: &str,
    nonce: u32,
    signature_hex: &str,
    payout_sats: Option<u16>,
) -> Result<Vec<u8>, String> {
    apply_reference_signature_with_payout_sats_for_deployment(
        job,
        miner_payout,
        public_key_hex,
        nonce,
        signature_hex,
        payout_sats,
        &MAINNET_PHOTON,
    )
}

fn apply_reference_signature_with_payout_sats_for_deployment(
    job: &ReferenceJobContext,
    miner_payout: &str,
    public_key_hex: &str,
    nonce: u32,
    signature_hex: &str,
    payout_sats: Option<u16>,
    deployment: &PhotonDeployment,
) -> Result<Vec<u8>, String> {
    let sig = parse_hex(signature_hex)?;
    if sig.len() != 64 {
        return Err("signature must be 64 bytes".into());
    }
    let public_key = parse_hex(public_key_hex)?;
    let public_key: [u8; 33] = public_key
        .try_into()
        .map_err(|_| "compressed public key must be 33 bytes")?;
    let signature: [u8; 64] = sig.try_into().map_err(|_| "signature must be 64 bytes")?;
    let message = photon_message_sha256(nonce, &job.target_le_hex)?;
    if !crate::crypto::bch_schnorr_verify(&public_key, &message, &signature)? {
        return Err("BCH Schnorr signature does not match nonce/target/public key".into());
    }
    let payout = cashaddr_to_p2pkh_locking(miner_payout)?;
    let p = TemplateParams {
        prev_tx_hash_hex: job.prev_txid.clone(),
        prev_index: job.prev_vout,
        age: job.age,
        public_key_hex: public_key_hex.to_string(),
        target_hex: job.target_le_hex.clone(),
        signature_hex: signature_hex.to_string(),
        nonce,
        contract_value_sats: job.contract_value_sats,
        contract_token_amount: job.contract_token_amount,
        reward_amount: job.reward_raw,
        payout_locking: payout,
    };
    let mut tx = build_photon_template_bytes_for_deployment(&p, deployment)?;
    if let Some(sats) = payout_sats {
        set_payout_value_sats(&mut tx, sats)?;
    }
    let target = crate::search::parse_hex32(&job.target_le_hex)?;
    let digest = crate::search::hash256(&tx);
    let network = match deployment.category_hex {
        crate::protocol::MAINNET_CATEGORY_HEX => crate::config::MiningNetwork::Mainnet,
        crate::protocol::CHIPNET_CATEGORY_HEX => crate::config::MiningNetwork::Chipnet,
        _ => return Err("unsupported PHOTON deployment for proof validation".into()),
    };
    if !crate::search::meets_target_le_for_network(&digest, &target, network) {
        return Err(format!(
            "candidate HASH256 {} does not meet PHOTON target {}",
            hex::encode(digest),
            job.target_le_hex
        ));
    }
    Ok(tx)
}

/// Build the authoritative two-output layout with a zero-signature placeholder.
pub fn build_unsigned_reference_preview(
    job: &ReferenceJobContext,
    miner_payout: &str,
) -> Result<Vec<u8>, String> {
    build_unsigned_reference_preview_for_deployment(job, miner_payout, &MAINNET_PHOTON)
}

/// Builds an unsigned parent preview for the selected contract deployment.
pub fn build_unsigned_reference_preview_for_deployment(
    job: &ReferenceJobContext,
    miner_payout: &str,
    deployment: &PhotonDeployment,
) -> Result<Vec<u8>, String> {
    let payout = cashaddr_to_p2pkh_locking(miner_payout)?;
    // Placeholder compressed pubkey (secp order-2 style) + zero Schnorr — not a valid win.
    let p = TemplateParams {
        prev_tx_hash_hex: job.prev_txid.clone(),
        prev_index: job.prev_vout,
        age: job.age,
        public_key_hex: "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5".into(),
        target_hex: job.target_le_hex.clone(),
        signature_hex: "00".repeat(64),
        nonce: 0,
        contract_value_sats: job.contract_value_sats,
        contract_token_amount: job.contract_token_amount,
        reward_amount: job.reward_raw,
        payout_locking: payout,
    };
    build_photon_template_bytes_for_deployment(&p, deployment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chipnet_parent_matches_a_confirmed_on_chain_mining_transaction() {
        // Confirmed on Chipnet as txid 00024a4b0073d8429b3f4796bbfcfcdadd3d938a1c704f68da77d2bdc9e78ad0.
        let confirmed =
            hex::decode(include_str!("../reference/photon_chipnet_confirmed_parent.hex").trim())
                .unwrap();
        let commitment = "286900003b2994b1562930e68115f321cab63a2a055557b82ddce73921b525f6e34104004ef6abff0cd8c4d8f2c11aa3914762e2224e9e4b8a9c785cafce11088eed0f3d5d3c0c25c731f1c454bc292fc307f0506090d2ae3b42470fc1c38e267de29053";
        let built = build_photon_template_bytes_for_deployment(
            &TemplateParams {
                prev_tx_hash_hex:
                    "0000502a821307c5159e2e2033957f12046d8b35dc3e5d4a0e1a9a4619746d77".into(),
                prev_index: 0,
                age: 38,
                public_key_hex:
                    "02789e85a48dccf23f768b2e5c2ce71855a21493558760d54e2b61ef72cdeaced5".into(),
                target_hex: commitment[8..72].into(),
                signature_hex: commitment[72..].into(),
                nonce: 0x6928,
                contract_value_sats: 49_080_500,
                contract_token_amount: 2_096_937_231_989_870,
                reward_amount: 4_992_707_694,
                payout_locking: hex::decode("76a9149d6a0da70e78df8b9d166f330a5e04c15676d42a88ac")
                    .unwrap(),
            },
            &crate::protocol::CHIPNET_PHOTON,
        )
        .unwrap();
        assert_eq!(built, confirmed);
        assert_eq!(
            crate::reward::transaction_id(&built),
            "00024a4b0073d8429b3f4796bbfcfcdadd3d938a1c704f68da77d2bdc9e78ad0"
        );
    }

    #[test]
    fn chipnet_layout_tracks_the_longer_redeem_script_and_gpu_limit() {
        for (age, bytes, shift) in [(10, 617, 2), (38, 618, 3)] {
            let layout =
                PhotonLayout::for_age_with_deployment(age, &crate::protocol::CHIPNET_PHOTON)
                    .unwrap();
            assert_eq!(layout.tx_bytes(), bytes);
            assert_eq!(layout.shift(), shift);
        }
        assert!(
            PhotonLayout::for_age_with_deployment(128, &crate::protocol::CHIPNET_PHOTON,).is_err()
        );
    }

    #[test]
    fn chipnet_signed_parent_reconstruction_uses_its_redeem_script_and_hash_rule() {
        let sk = [0x11; 32];
        let public_key = crate::crypto::compressed_pubkey(&sk).unwrap();
        let payout = crate::config::CHIPNET_DONATION_ADDRESS;
        let job = ReferenceJobContext {
            prev_txid: "aa".repeat(32),
            prev_vout: 0,
            age: 38,
            target_le_hex: format!("{}7f", "ff".repeat(31)),
            contract_value_sats: 49_080_500,
            contract_token_amount: 2_096_937_231_989_870,
            reward_raw: 4_992_707_694,
        };
        let target = crate::search::parse_hex32(&job.target_le_hex).unwrap();
        for nonce in 0..32 {
            let message = photon_message_sha256(nonce, &job.target_le_hex).unwrap();
            let signature = crate::crypto::bch_schnorr_sign(&sk, &message).unwrap();
            let raw = build_photon_template_bytes_for_deployment(
                &TemplateParams {
                    prev_tx_hash_hex: job.prev_txid.clone(),
                    prev_index: job.prev_vout,
                    age: job.age,
                    public_key_hex: hex::encode(public_key),
                    target_hex: job.target_le_hex.clone(),
                    signature_hex: hex::encode(signature),
                    nonce,
                    contract_value_sats: job.contract_value_sats,
                    contract_token_amount: job.contract_token_amount,
                    reward_amount: job.reward_raw,
                    payout_locking: cashaddr_to_p2pkh_locking(payout).unwrap(),
                },
                &crate::protocol::CHIPNET_PHOTON,
            )
            .unwrap();
            if crate::search::meets_target_le_for_network(
                &crate::search::hash256(&raw),
                &target,
                crate::config::MiningNetwork::Chipnet,
            ) {
                assert_eq!(
                    apply_reference_signature_for_deployment(
                        &job,
                        payout,
                        &hex::encode(public_key),
                        nonce,
                        &hex::encode(signature),
                        &crate::protocol::CHIPNET_PHOTON,
                    )
                    .unwrap(),
                    raw
                );
                return;
            }
        }
        panic!("no positive chipnet HASH256 found in 32 signed nonces");
    }

    #[test]
    fn t2_reward_preserves_token_supply_and_fixed_width_encoding() {
        let total = 2_099_905_002_035_715u128;
        let reward = 4_999_773_813u128;
        for j in [0, 1, 255, 65_535] {
            let moved = t2_reward_amount(total, reward, j).unwrap();
            assert_eq!(moved, reward - u128::from(j));
            assert_eq!(moved + (total - moved), total);
            assert_eq!(compact_token_amount(moved).unwrap().len(), 9);
            assert_eq!(compact_token_amount(total - moved).unwrap().len(), 9);
        }
        assert!(t2_reward_amount(total, u128::from(u32::MAX) + 1, 1).is_err());
        assert!(t2_reward_amount(u128::from(u32::MAX) + 1, 1, 0).is_err());
    }

    #[test]
    fn t2_parent_reward_reads_only_bounded_supply_preserving_amounts() {
        let total = 2_099_905_002_035_715u128;
        let reward = 4_999_773_813u128;
        let payout =
            cashaddr_to_p2pkh_locking("zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3").unwrap();
        for age in [10, 17, 128, 32_768] {
            let mut raw = build_photon_template_bytes(&TemplateParams {
                prev_tx_hash_hex: "aa".repeat(32),
                prev_index: 0,
                age,
                public_key_hex:
                    "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5".into(),
                target_hex: "ff".repeat(32),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 15_971_500,
                contract_token_amount: total,
                reward_amount: reward - 65_535,
                payout_locking: payout.clone(),
            })
            .unwrap();
            assert_eq!(
                t2_parent_reward_amount(&raw, total, reward).unwrap(),
                reward - 65_535
            );
            let shift = PhotonLayout::for_age(age).unwrap().shift();
            raw[578 + shift] ^= 1;
            assert!(t2_parent_reward_amount(&raw, total, reward).is_err());
        }
    }

    #[test]
    fn decode_known_vector() {
        let lock =
            cashaddr_to_p2pkh_locking("bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh")
                .expect("decode");
        assert_eq!(
            hex::encode(&lock),
            "76a9146e0810ceea13412b73feb41566a3d2d0ce54e10188ac"
        );
    }

    #[test]
    fn chipnet_p2pkh_cashaddr_encoder_round_trips() {
        let hash = [0x42u8; 20];
        let address =
            p2pkh_hash_to_cashaddr_for_network(&hash, crate::config::MiningNetwork::Chipnet)
                .unwrap();
        assert!(address.starts_with("bchtest:"));
        assert_eq!(
            cashaddr_to_p2pkh_locking(&address).unwrap(),
            [&[0x76, 0xa9, 0x14][..], &hash, &[0x88, 0xac]].concat(),
        );
        assert_eq!(
            p2pkh_hash_to_cashaddr(&hash).unwrap(),
            p2pkh_hash_to_cashaddr_for_network(&hash, crate::config::MiningNetwork::Mainnet,)
                .unwrap()
        );
    }

    #[test]
    fn token_aware_payout_keeps_its_type_when_converted_to_chipnet() {
        let mainnet = "bitcoincash:zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3";
        let locking = cashaddr_to_p2pkh_locking(mainnet).unwrap();
        let hash: [u8; 20] = locking[3..23].try_into().unwrap();
        assert_eq!(
            token_p2pkh_hash_to_cashaddr_for_network(&hash, crate::config::MiningNetwork::Mainnet,)
                .unwrap(),
            mainnet,
        );
        let chipnet =
            token_p2pkh_hash_to_cashaddr_for_network(&hash, crate::config::MiningNetwork::Chipnet)
                .unwrap();
        assert!(chipnet.starts_with("bchtest:z"));
        assert_eq!(cashaddr_to_p2pkh_locking(&chipnet).unwrap(), locking);
        let mut invalid = chipnet.into_bytes();
        *invalid.last_mut().unwrap() = if *invalid.last().unwrap() == b'q' {
            b'p'
        } else {
            b'q'
        };
        assert!(cashaddr_to_p2pkh_locking(&String::from_utf8(invalid).unwrap()).is_err());
    }

    #[test]
    fn donation_address_decodes() {
        let lock = cashaddr_to_p2pkh_locking(DONATION_ADDRESS).expect("donation");
        assert_eq!(lock.len(), 25);
        assert_eq!(lock[0], 0x76);
    }

    #[test]
    fn serializer_matches_reference_vector() {
        let expected = include_str!("../reference/photon_vector_tx.hex").trim();
        let payout = hex::decode("76a9146e0810ceea13412b73feb41566a3d2d0ce54e10188ac").unwrap();
        let p = TemplateParams {
            prev_tx_hash_hex: "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042"
                .into(),
            prev_index: 0,
            age: 10,
            public_key_hex:
                "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into(),
            target_hex: "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                .into(),
            signature_hex: "5b73543b21b74bd47b0dfc4565780e4ed2f0e5c4bb85f2c6dd3546727f84604fc6e8cc2b6b38de1c5630da8356e2e07a403ddeba8835caba0b80d75a5ac471e4".into(),
            nonce: 0x1234_5678,
            contract_value_sats: 15_971_500,
            contract_token_amount: 2_099_905_002_035_715,
            reward_amount: 4_999_773_813,
            payout_locking: payout,
        };
        let built = build_photon_template_bytes(&p).expect("build");
        assert_eq!(hex::encode(&built), expected);
        assert_eq!(built.len(), 615);
    }

    #[test]
    fn layout_follows_the_age_push_width() {
        let payout = hex::decode("76a9146e0810ceea13412b73feb41566a3d2d0ce54e10188ac").unwrap();
        let target = "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000";
        for (age, bytes) in [
            (0u32, 615usize),
            (16, 615),
            (17, 616),
            (127, 616),
            (128, 617),
            (32_767, 617),
            (32_768, 618),
            (65_534, 618),
        ] {
            let layout = PhotonLayout::for_age(age).unwrap();
            assert_eq!(layout.tx_bytes(), bytes, "age {age}");
            let built = build_photon_template_bytes(&TemplateParams {
                prev_tx_hash_hex: "00".repeat(32),
                prev_index: 0,
                age,
                public_key_hex:
                    "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into(),
                target_hex: target.into(),
                signature_hex: "ab".repeat(64),
                nonce: 0x1234_5678,
                contract_value_sats: 15_971_500,
                contract_token_amount: 2_099_905_002_035_715,
                reward_amount: 4_999_773_813,
                payout_locking: payout.clone(),
            })
            .unwrap();
            assert_eq!(built.len(), bytes, "age {age}");
            assert_eq!(PhotonLayout::for_tx_len(built.len()).unwrap(), layout);
            let nonce = layout.nonce_offset();
            assert_eq!(&built[nonce..nonce + 4], &0x1234_5678u32.to_le_bytes());
            let t = layout.target_offset();
            assert_eq!(hex::encode(&built[t..t + 32]), target);
            let s = layout.signature_offset();
            assert_eq!(&built[s..s + 64], &[0xab; 64][..]);
        }
        assert!(PhotonLayout::for_age(65_535).is_err());
        assert!(PhotonLayout::for_tx_len(614).is_err());
        assert!(PhotonLayout::for_tx_len(619).is_err());
    }

    #[test]
    fn covenant_preimage_requires_eight_byte_amounts() {
        let amount = 2_099_905_002_035_715u128;
        assert!(require_covenant_hash_preimage(amount, amount / 420_000).is_ok());
        assert!(require_covenant_hash_preimage(amount, u128::from(u32::MAX)).is_err());
        assert!(require_covenant_hash_preimage(u128::from(u32::MAX) * 2, 5_000_000_000).is_err());
        assert!(require_covenant_hash_preimage(1, 2).is_err());
    }

    #[test]
    fn message_hash_matches_vector() {
        let h = photon_message_sha256(
            0x1234_5678,
            "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000",
        )
        .unwrap();
        assert_eq!(
            hex::encode(h),
            "098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f"
        );
    }

    #[test]
    fn unproven_donation_split_is_disabled() {
        let payout =
            cashaddr_to_p2pkh_locking("bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh")
                .unwrap();
        let donation = cashaddr_to_p2pkh_locking(DONATION_ADDRESS).unwrap();
        let p = TemplateParams {
            prev_tx_hash_hex: "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042"
                .into(),
            prev_index: 0,
            age: 10,
            public_key_hex: "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
                .into(),
            target_hex: "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000".into(),
            signature_hex: "00".repeat(64),
            nonce: 0,
            contract_value_sats: 15_971_500,
            contract_token_amount: 2_099_905_002_035_715,
            reward_amount: 4_999_773_813,
            payout_locking: payout,
        };
        let error = build_photon_template_donation_split(&p, &donation)
            .expect_err("unproven 3-output split must be blocked");
        assert_eq!(error, DONATION_SPLIT_BLOCKER);
    }

    #[test]
    fn preview_names_the_settlement_split_instead_of_a_parent_donation_output() {
        let payout = "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh";
        let reward = 4_999_773_813u128;
        let lines = win_tx_preview_lines(reward, payout).expect("preview");
        let text = lines.join("\n");
        let (miner_tokens, donation_tokens) = crate::config::RuntimeConfig::split_reward(reward);
        assert_ne!(miner_tokens, reward);
        assert!(text.contains(&format!("FT={reward}")));
        assert!(text.contains(&format!("FT={miner_tokens} -> {payout}")));
        assert!(text.contains(&format!("total fee FT={donation_tokens}")));
        assert!(text.contains("this template has no donation output"));
        assert!(!text.contains("10000 bps"));
        print_win_tx_preview(reward, payout, None).expect("preview prints the same report");
    }
    #[test]
    fn payout_value_coordinate_changes_only_output_satoshis() {
        let original =
            hex::decode(include_str!("../reference/photon_vector_tx.hex").trim()).unwrap();
        let mut candidate = original.clone();
        set_payout_value_sats(&mut candidate, 684).unwrap();
        assert_eq!(payout_value_sats(&candidate).unwrap(), 684);
        assert_eq!(&candidate[534..542], &684u64.to_le_bytes());
        assert_eq!(&candidate[586..611], &original[586..611]);
        assert_eq!(&candidate[491..499], &original[491..499]);
        assert_eq!(&candidate[578..586], &original[578..586]);
        assert_eq!(
            candidate
                .iter()
                .zip(&original)
                .filter(|(a, b)| a != b)
                .count(),
            1
        );
        assert!(set_payout_value_sats(&mut candidate, 674).is_err());
        assert!(set_payout_value_sats(&mut candidate, 886).is_err());
    }
}
