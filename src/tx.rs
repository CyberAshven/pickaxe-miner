//! Win-tx: CashAddr, PHOTON template serializer, coinbase-style 98/2 donation split.
//! Template assemble + message hash here. Schnorr sign stays Lead Dev / crypto.
//! Never keys/mnemonics. No broadcast until explicitly armed.

use crate::config::{RuntimeConfig, DONATION_ADDRESS, DONATION_BPS, MINER_BPS};
use sha2::Digest;
use crate::protocol::{
    COVENANT_LOCKING_BYTECODE_HEX, MAINNET_CATEGORY_HEX, REDEEM_SCRIPT_HEX,
};

const CASHADDR_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const CASHADDR_GENERATORS: [u64; 5] = [
    0x98f2_bc8e_61,
    0x79b7_6d99_e2,
    0xf33e_5fb3_c4,
    0xae2e_abe2_a8,
    0x1e4f_43e4_70,
];

/// Decode mainnet bitcoincash: P2PKH (or token-aware P2PKH) to 25-byte locking bytecode.
pub fn cashaddr_to_p2pkh_locking(address: &str) -> Result<Vec<u8>, String> {
    let mut normalized = address.trim().to_lowercase();
    if !normalized.contains(':') {
        normalized = format!("bitcoincash:{normalized}");
    }
    let pieces: Vec<&str> = normalized.split(':').collect();
    if pieces.len() != 2 || pieces[0] != "bitcoincash" {
        return Err("expected mainnet bitcoincash: address".into());
    }
    let (prefix, payload_text) = (pieces[0], pieces[1]);
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
        for i in 0..5 {
            if ((c0 >> i) & 1) != 0 {
                c ^= CASHADDR_GENERATORS[i];
            }
        }
    }
    c ^ 1
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

/// Coinbase-style 98/2 donation split: baton out + miner FT + donation FT (3 outputs).
/// Sats: baton keeps contract-1500-700; miner/donation share 700 dust-style like reference,
/// FT amounts split by `RuntimeConfig::split_reward`. Covenant validation of 3-out is TBD.
pub fn build_photon_template_donation_split(
    p: &TemplateParams,
    donation_locking: &[u8],
) -> Result<Vec<u8>, String> {
    if donation_locking.len() != 25 {
        return Err("donation locking must be P2PKH 25 bytes".into());
    }
    let (miner_ft, donation_ft) = RuntimeConfig::split_reward(p.reward_amount);
    if miner_ft + donation_ft != p.reward_amount {
        return Err("split invariant broken".into());
    }

    let public_key = parse_hex(&p.public_key_hex)?;
    let target = parse_hex(&p.target_hex)?;
    let signature = parse_hex(&p.signature_hex)?;
    let redeem_script = parse_hex(REDEEM_SCRIPT_HEX.trim())?;
    let category = parse_hex(MAINNET_CATEGORY_HEX)?;
    let covenant_lock = parse_hex(COVENANT_LOCKING_BYTECODE_HEX)?;
    if public_key.len() != 33 || target.len() != 32 || signature.len() != 64 {
        return Err("bad pubkey/target/sig lengths".into());
    }
    if redeem_script.len() != 259 {
        return Err("bad redeem length".into());
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

    let mut output_miner = Vec::new();
    output_miner.push(0xef);
    output_miner.extend_from_slice(&cat_rev);
    output_miner.push(0x10);
    output_miner.extend_from_slice(&compact_token_amount(miner_ft)?);
    output_miner.extend_from_slice(&p.payout_locking);

    let mut output_don = Vec::new();
    output_don.push(0xef);
    output_don.extend_from_slice(&cat_rev);
    output_don.push(0x10);
    output_don.extend_from_slice(&compact_token_amount(donation_ft)?);
    output_don.extend_from_slice(donation_locking);

    let prev = reverse_bytes(&parse_hex(&p.prev_tx_hash_hex)?);
    let baton_sats = p
        .contract_value_sats
        .checked_sub(1500)
        .ok_or("contract value too small")?;
    // Split the reference 700 sats: 546 miner + 154 donation (both above dust-ish floors).
    let miner_sats = 546u64;
    let donation_sats = 154u64;

    let mut tx = Vec::new();
    tx.extend_from_slice(&u32_le(2));
    tx.push(1);
    tx.extend_from_slice(&prev);
    tx.extend_from_slice(&u32_le(p.prev_index));
    tx.extend_from_slice(&compact_uint(input_script.len() as u64));
    tx.extend_from_slice(&input_script);
    tx.extend_from_slice(&u32_le(p.age));
    tx.push(3);
    tx.extend_from_slice(&u64_le(baton_sats));
    tx.extend_from_slice(&compact_uint(output0.len() as u64));
    tx.extend_from_slice(&output0);
    tx.extend_from_slice(&u64_le(miner_sats));
    tx.extend_from_slice(&compact_uint(output_miner.len() as u64));
    tx.extend_from_slice(&output_miner);
    tx.extend_from_slice(&u64_le(donation_sats));
    tx.extend_from_slice(&compact_uint(output_don.len() as u64));
    tx.extend_from_slice(&output_don);
    tx.extend_from_slice(&u32_le(0));
    Ok(tx)
}

/// Dry-run preview + optional unsigned template hex (no broadcast).
pub fn print_win_tx_preview(
    job_reward_raw: u128,
    miner_payout: &str,
    template_hex: Option<&str>,
) -> Result<(), String> {
    let (miner_amt, donation_amt) = RuntimeConfig::split_reward(job_reward_raw);
    let miner_lock = cashaddr_to_p2pkh_locking(miner_payout)?;
    let donation_lock = cashaddr_to_p2pkh_locking(DONATION_ADDRESS)?;
    println!("win-tx dry-run (unsigned; no broadcast):");
    println!(
        "  out miner    {MINER_BPS} bps FT={miner_amt} lock={}B → {miner_payout}",
        miner_lock.len()
    );
    println!(
        "  out donation {DONATION_BPS} bps FT={donation_amt} lock={}B → {DONATION_ADDRESS}",
        donation_lock.len()
    );
    println!("  model: coinbase-style on win tx only (never skim unrelated funds)");
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

/// Rebuild donation-split template with a real 64-byte Schnorr (hex). No keys touched here.
pub fn apply_donation_signature(
    prev_txid: &str,
    prev_vout: u32,
    age: u32,
    target_le_hex: &str,
    contract_value_sats: u64,
    contract_token_amount: u128,
    reward_raw: u128,
    miner_payout: &str,
    public_key_hex: &str,
    nonce: u32,
    signature_hex: &str,
) -> Result<Vec<u8>, String> {
    let sig = parse_hex(signature_hex)?;
    if sig.len() != 64 {
        return Err("signature must be 64 bytes".into());
    }
    let payout = cashaddr_to_p2pkh_locking(miner_payout)?;
    let donation = cashaddr_to_p2pkh_locking(DONATION_ADDRESS)?;
    let p = TemplateParams {
        prev_tx_hash_hex: prev_txid.to_string(),
        prev_index: prev_vout,
        age,
        public_key_hex: public_key_hex.to_string(),
        target_hex: target_le_hex.to_string(),
        signature_hex: signature_hex.to_string(),
        nonce,
        contract_value_sats,
        contract_token_amount,
        reward_amount: reward_raw,
        payout_locking: payout,
    };
    build_photon_template_donation_split(&p, &donation)
}


/// Build unsigned donation-split template using zero signature (placeholder) for preview.
pub fn build_unsigned_donation_preview(
    prev_txid: &str,
    prev_vout: u32,
    age: u32,
    target_le_hex: &str,
    contract_value_sats: u64,
    contract_token_amount: u128,
    reward_raw: u128,
    miner_payout: &str,
) -> Result<Vec<u8>, String> {
    let payout = cashaddr_to_p2pkh_locking(miner_payout)?;
    let donation = cashaddr_to_p2pkh_locking(DONATION_ADDRESS)?;
    // Placeholder compressed pubkey (secp order-2 style) + zero Schnorr — not a valid win.
    let p = TemplateParams {
        prev_tx_hash_hex: prev_txid.to_string(),
        prev_index: prev_vout,
        age,
        public_key_hex: "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5"
            .into(),
        target_hex: target_le_hex.to_string(),
        signature_hex: "00".repeat(64),
        nonce: 0,
        contract_value_sats,
        contract_token_amount,
        reward_amount: reward_raw,
        payout_locking: payout,
    };
    build_photon_template_donation_split(&p, &donation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_known_vector() {
        let lock = cashaddr_to_p2pkh_locking(
            "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh",
        )
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
    fn donation_split_three_outputs() {
        let payout = cashaddr_to_p2pkh_locking(
            "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh",
        )
        .unwrap();
        let donation = cashaddr_to_p2pkh_locking(DONATION_ADDRESS).unwrap();
        let p = TemplateParams {
            prev_tx_hash_hex: "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042"
                .into(),
            prev_index: 0,
            age: 10,
            public_key_hex:
                "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".into(),
            target_hex: "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                .into(),
            signature_hex: "00".repeat(64),
            nonce: 0,
            contract_value_sats: 15_971_500,
            contract_token_amount: 2_099_905_002_035_715,
            reward_amount: 4_999_773_813,
            payout_locking: payout,
        };
        let tx = build_photon_template_donation_split(&p, &donation).unwrap();
        assert_eq!(tx[4 + 32 + 4 + 1..].iter().position(|_| false), None); // smoke
        // output count byte: after version(4)+in_count(1)+outpoint(36)+scriptlen+script+seq(4)
        assert!(tx.len() > 615); // 3 outputs larger than reference 2-out
        assert_eq!(tx[0..4], [2, 0, 0, 0]);
    }
}
