//! Win-tx helpers: CashAddr → P2PKH lock + coinbase-style 98/2 donation preview.
//! Full signed PHOTON template lands after Lead Dev Schnorr path; no keys here.

use crate::config::{RuntimeConfig, DONATION_ADDRESS, DONATION_BPS, MINER_BPS};

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

/// Dry-run preview of coinbase-style reward split (no tx bytes / no broadcast).
pub fn print_win_tx_preview(job_reward_raw: u128, miner_payout: &str) -> Result<(), String> {
    let (miner_amt, donation_amt) = RuntimeConfig::split_reward(job_reward_raw);
    let miner_lock = cashaddr_to_p2pkh_locking(miner_payout)?;
    let donation_lock = cashaddr_to_p2pkh_locking(DONATION_ADDRESS)?;
    println!("win-tx dry-run (unsigned preview; no broadcast):");
    println!("  out miner    {MINER_BPS} bps FT={miner_amt} lock={}B → {miner_payout}", miner_lock.len());
    println!(
        "  out donation {DONATION_BPS} bps FT={donation_amt} lock={}B → {DONATION_ADDRESS}",
        donation_lock.len()
    );
    println!("  model: coinbase-style on win tx only (never skim unrelated funds)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_known_vector() {
        // From reference miner.js deterministicSerializerRegression
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
}
