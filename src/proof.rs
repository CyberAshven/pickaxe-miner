//! Pure candidate hashing and covenant proof checks shared across platforms.
use crate::protocol::ProofRule;
use sha2::{Digest, Sha256};

/// Double SHA-256 (HASH256) for host verification.
pub fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Decodes an exactly 32-byte hexadecimal value.
pub fn parse_hex32(hex: &str) -> Result<[u8; 32], String> {
    let h = hex.trim();
    if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("target must be 64 hex chars (32 bytes)".into());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] =
            u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).map_err(|_| "invalid hex".to_string())?;
    }
    Ok(out)
}

/// Applies the covenant's proof-of-work rule to a little-endian digest.
///
/// The covenant checks `ABS(BIN2NUM(HASH256(tx))) < target`. BIN2NUM reads
/// the digest as a little-endian script number whose top bit is the sign
/// and ABS drops it, so bit 255 never matters. Equality is not a win.
pub fn meets_target_le(digest: &[u8; 32], target_le: &[u8; 32]) -> bool {
    let top = digest[31] & 0x7f;
    if top != target_le[31] {
        return top < target_le[31];
    }
    for i in (0..31).rev() {
        if digest[i] < target_le[i] {
            return true;
        }
        if digest[i] > target_le[i] {
            return false;
        }
    }
    false
}

/// Applies the selected covenant's proof rule.
pub fn meets_target_le_for_rule(digest: &[u8; 32], target_le: &[u8; 32], rule: ProofRule) -> bool {
    if rule == ProofRule::Positive
        && (digest[31] & 0x80 != 0
            || digest.iter().all(|byte| *byte == 0)
            || target_le[31] & 0x80 != 0
            || target_le.iter().all(|byte| *byte == 0))
    {
        return false;
    }
    meets_target_le(digest, target_le)
}
