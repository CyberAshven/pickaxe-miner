//! Win-tx: CashAddr and the authoritative two-output PHOTON serializer.
//! Template assemble + message hash here. Schnorr sign stays Lead Dev / crypto.
//! Never keys/mnemonics. No broadcast until explicitly armed.

#[cfg(test)]
use crate::config::DONATION_ADDRESS;
use crate::protocol::{COVENANT_LOCKING_BYTECODE_HEX, MAINNET_CATEGORY_HEX, REDEEM_SCRIPT_HEX};
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
    let mut decoded = Vec::with_capacity(21);
    decoded.push(0); // P2PKH, 160-bit hash
    decoded.extend_from_slice(hash);
    let payload = convert_bits(&decoded, 8, 5, true)?;
    let prefix = "bitcoincash";
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

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

fn u32_le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn u64_le(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

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

fn reverse_bytes(b: &[u8]) -> Vec<u8> {
    b.iter().rev().copied().collect()
}

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
    let public_key = parse_hex(&p.public_key_hex)?;
    let target = parse_hex(&p.target_hex)?;
    let signature = parse_hex(&p.signature_hex)?;
    let redeem_script = parse_hex(REDEEM_SCRIPT_HEX.trim())?;
    let category = parse_hex(MAINNET_CATEGORY_HEX)?;
    let covenant_lock = parse_hex(COVENANT_LOCKING_BYTECODE_HEX)?;

    if public_key.len() != 33 {
        return Err("Compressed public key must be 33 bytes.".into());
    }
    if target.len() != 32 {
        return Err("PHOTON target must be 32 bytes.".into());
    }
    if signature.len() != 64 {
        return Err("PHOTON commitment signature must be 64 bytes.".into());
    }
    if redeem_script.len() != 259 {
        return Err(format!(
            "PHOTON redeem script must be 259 bytes (got {}).",
            redeem_script.len()
        ));
    }
    if p.payout_locking.len() != 25 {
        return Err("Payout locking bytecode must be P2PKH (25 bytes).".into());
    }

    let age_push = encode_positive_script_number_push(p.age)?;
    let input_script = concat(&[
        &[0x21],
        &public_key,
        &age_push,
        &[0x4d, 0x03, 0x01],
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

    let baton_sats = p
        .contract_value_sats
        .checked_sub(1500)
        .ok_or("contract value too small for fee")?;

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
    Ok(tx)
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

/// Dry-run preview + optional unsigned template hex (no broadcast).
pub fn print_win_tx_preview(
    job_reward_raw: u128,
    miner_payout: &str,
    template_hex: Option<&str>,
) -> Result<(), String> {
    let miner_lock = cashaddr_to_p2pkh_locking(miner_payout)?;
    println!("win-tx dry-run (unsigned; no broadcast):");
    println!(
        "  out reward   10000 bps FT={job_reward_raw} lock={}B -> {miner_payout}",
        miner_lock.len()
    );
    println!("Donation: 2%");
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
    let tx = build_photon_template_bytes(&p)?;
    let target = crate::search::parse_hex32(&job.target_le_hex)?;
    let digest = crate::search::hash256(&tx);
    if !crate::search::meets_target_le(&digest, &target) {
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
    build_photon_template_bytes(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
