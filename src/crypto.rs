//! BCH Schnorr signing gate (Codex PERFORMANCE CONTRACT sec18).
//!
//! Matches postcorps/photon reference:
//! - RFC6979 nonce with algo tag `Schnorr+SHA256  `
//! - challenge `e = SHA256(r_x || compressed_pubkey || msg)`
//! - if R.y is not a quadratic residue mod p, use `k' = n - k` (r_x unchanged)
//!
//! The signer is shared by deterministic vector tests, live job setup, and rare
//! returned-winner reconstruction. Mining identities remain runtime-only.

use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use secp256k1::{PublicKey, Scalar, SecretKey};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const SECP_P_BE: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF, 0xFF, 0xFC, 0x2F,
];

const SECP_N_BE: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
    0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
];

#[cfg(test)]
/// Derives a compressed public key from test secret key bytes.
pub fn compressed_pubkey(sk_bytes: &[u8; 32]) -> Result<[u8; 33], String> {
    let sk = SecretKey::from_secret_bytes(*sk_bytes).map_err(|e| e.to_string())?;
    Ok(PublicKey::from_secret_key(&sk).serialize())
}

/// Computes the SHA-256 digest of input bytes.
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(data));
    out
}

/// Computes HMAC-SHA256 for nonce generation.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// Reduces a message modulo the secp256k1 group order.
fn reduce_msg_mod_n(msg32: &[u8; 32]) -> [u8; 32] {
    let m = BigUint::from_bytes_be(msg32);
    let n = BigUint::from_bytes_be(&SECP_N_BE);
    let r = m % n;
    let bytes = r.to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    out
}

/// RFC6979 nonce for BCH Schnorr (algo tag ASCII `Schnorr+SHA256  `).
pub fn bch_rfc6979_nonce(sk_bytes: &[u8; 32], msg32: &[u8; 32]) -> Result<[u8; 32], String> {
    let reduced = reduce_msg_mod_n(msg32);
    let algo = b"Schnorr+SHA256  ";
    let mut v = [1u8; 32];
    let mut k = [0u8; 32];

    let mut buf = Vec::with_capacity(113);
    buf.extend_from_slice(&v);
    buf.push(0);
    buf.extend_from_slice(sk_bytes);
    buf.extend_from_slice(&reduced);
    buf.extend_from_slice(algo);
    k = hmac_sha256(&k, &buf);
    v = hmac_sha256(&k, &v);

    buf.clear();
    buf.extend_from_slice(&v);
    buf.push(1);
    buf.extend_from_slice(sk_bytes);
    buf.extend_from_slice(&reduced);
    buf.extend_from_slice(algo);
    k = hmac_sha256(&k, &buf);
    v = hmac_sha256(&k, &v);

    let n = BigUint::from_bytes_be(&SECP_N_BE);
    loop {
        v = hmac_sha256(&k, &v);
        let cand = BigUint::from_bytes_be(&v);
        if cand > BigUint::zero() && cand < n {
            return Ok(v);
        }
        let mut t = Vec::with_capacity(33);
        t.extend_from_slice(&v);
        t.push(0);
        k = hmac_sha256(&k, &t);
        v = hmac_sha256(&k, &v);
    }
}

/// Tests a curve point’s Y coordinate for quadratic residuosity.
fn y_is_quadratic_residue(y_be: &[u8; 32]) -> bool {
    let y = BigUint::from_bytes_be(y_be);
    let p = BigUint::from_bytes_be(&SECP_P_BE);
    if y.is_zero() {
        return true;
    }
    let exp = (&p - BigUint::one()) >> 1;
    y.modpow(&exp, &p) == BigUint::one()
}

/// Creates a deterministic BCH Schnorr signature for a message.
pub fn bch_schnorr_sign(sk_bytes: &[u8; 32], msg32: &[u8; 32]) -> Result<[u8; 64], String> {
    bch_schnorr_sign_with_k(sk_bytes, msg32, bch_rfc6979_nonce(sk_bytes, msg32)?)
}

/// Reconstruct a PHOTON search signature. Public k exposes sk: NEVER use a funded key.
pub(crate) fn bch_schnorr_sign_search_candidate(
    sk_bytes: &[u8; 32],
    msg32: &[u8; 32],
    scalar: u64,
) -> Result<[u8; 64], String> {
    if !(1..=1u64 << 32).contains(&scalar) {
        return Err("incremental search scalar is outside 1..=2^32".into());
    }
    let mut k = [0u8; 32];
    k[24..].copy_from_slice(&scalar.to_be_bytes());
    bch_schnorr_sign_with_k(sk_bytes, msg32, k)
}

fn bch_schnorr_sign_with_k(
    sk_bytes: &[u8; 32],
    msg32: &[u8; 32],
    k_bytes: [u8; 32],
) -> Result<[u8; 64], String> {
    let sk = SecretKey::from_secret_bytes(*sk_bytes).map_err(|e| e.to_string())?;
    let pk_bytes = PublicKey::from_secret_key(&sk).serialize();

    let k = SecretKey::from_secret_bytes(k_bytes).map_err(|e| e.to_string())?;
    let r_pk = PublicKey::from_secret_key(&k);
    let r_unc = r_pk.serialize_uncompressed();
    let mut r_x = [0u8; 32];
    r_x.copy_from_slice(&r_unc[1..33]);
    let mut r_y = [0u8; 32];
    r_y.copy_from_slice(&r_unc[33..65]);

    let k_adj = if y_is_quadratic_residue(&r_y) {
        k
    } else {
        k.negate()
    };

    let mut chal = Vec::with_capacity(97);
    chal.extend_from_slice(&r_x);
    chal.extend_from_slice(&pk_bytes);
    chal.extend_from_slice(msg32);
    let e_bytes = sha256(&chal);
    let e = Scalar::from_be_bytes(e_bytes).map_err(|_| "bad e")?;

    let ed = sk.mul_tweak(&e).map_err(|err| err.to_string())?;
    let s_key = k_adj
        .add_tweak(&Scalar::from(ed))
        .map_err(|err| err.to_string())?;
    let s_bytes = s_key.to_secret_bytes();

    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&r_x);
    sig[32..].copy_from_slice(&s_bytes);
    Ok(sig)
}

/// Verifies a BCH Schnorr signature against a compressed public key.
pub fn bch_schnorr_verify(
    pk33: &[u8; 33],
    msg32: &[u8; 32],
    sig64: &[u8; 64],
) -> Result<bool, String> {
    let pk = PublicKey::from_slice(pk33).map_err(|e| e.to_string())?;
    let mut r_x = [0u8; 32];
    let mut s_bytes = [0u8; 32];
    r_x.copy_from_slice(&sig64[..32]);
    s_bytes.copy_from_slice(&sig64[32..]);

    let mut chal = Vec::with_capacity(97);
    chal.extend_from_slice(&r_x);
    chal.extend_from_slice(pk33);
    chal.extend_from_slice(msg32);
    let e_bytes = sha256(&chal);
    let e = Scalar::from_be_bytes(e_bytes).map_err(|_| "bad e")?;

    let s_key = SecretKey::from_secret_bytes(s_bytes).map_err(|e| e.to_string())?;
    let s_g = PublicKey::from_secret_key(&s_key);
    let e_p = pk.mul_tweak(&e).map_err(|e| e.to_string())?;
    let r_prime = s_g.combine(&e_p.negate()).map_err(|e| e.to_string())?;
    let r_unc = r_prime.serialize_uncompressed();
    if r_unc[1..33] != r_x {
        return Ok(false);
    }
    let mut r_y = [0u8; 32];
    r_y.copy_from_slice(&r_unc[33..65]);
    Ok(y_is_quadratic_residue(&r_y))
}

#[cfg(test)]
/// Checks that BCH Schnorr primitives match production test vectors.
pub fn schnorr_production_gate_ok() -> bool {
    let mut sk = [0u8; 32];
    sk[31] = 1;
    let msg = hex_32("098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f");
    let expect_k = hex_32("615da2b700fbb1ae10a72a391ce49b17cd4d0f614311ac7d216281217fd7797a");
    let expect_sig = hex_64(
        "5b73543b21b74bd47b0dfc4565780e4ed2f0e5c4bb85f2c6dd3546727f84604fc6e8cc2b6b38de1c5630da8356e2e07a403ddeba8835caba0b80d75a5ac471e4",
    );
    match (bch_rfc6979_nonce(&sk, &msg), bch_schnorr_sign(&sk, &msg)) {
        (Ok(k), Ok(sig)) => k == expect_k && sig == expect_sig,
        _ => false,
    }
}

#[cfg(test)]
/// Decodes a 32-byte hexadecimal test vector.
fn hex_32(s: &str) -> [u8; 32] {
    let b = hex::decode(s).unwrap();
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    o
}
#[cfg(test)]
/// Decodes a 64-byte hexadecimal test vector.
fn hex_64(s: &str) -> [u8; 64] {
    let b = hex::decode(s).unwrap();
    let mut o = [0u8; 64];
    o.copy_from_slice(&b);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_from_known_one() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let pk = compressed_pubkey(&sk).unwrap();
        assert_eq!(
            hex::encode(pk),
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );
    }

    #[test]
    fn bch_rfc6979_matches_photon_vector() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let msg = hex_32("098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f");
        let k = bch_rfc6979_nonce(&sk, &msg).unwrap();
        assert_eq!(
            hex::encode(k),
            "615da2b700fbb1ae10a72a391ce49b17cd4d0f614311ac7d216281217fd7797a"
        );
    }

    #[test]
    fn bch_schnorr_matches_photon_vectors() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let msg = hex_32("098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f");
        let pk = compressed_pubkey(&sk).unwrap();
        let sig = bch_schnorr_sign(&sk, &msg).unwrap();
        assert_eq!(
            hex::encode(sig),
            "5b73543b21b74bd47b0dfc4565780e4ed2f0e5c4bb85f2c6dd3546727f84604fc6e8cc2b6b38de1c5630da8356e2e07a403ddeba8835caba0b80d75a5ac471e4"
        );
        assert!(bch_schnorr_verify(&pk, &msg, &sig).unwrap());
        assert!(schnorr_production_gate_ok());
    }

    #[test]
    fn verify_requires_qr_ry_same_as_sign() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let msg = hex_32("098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f");
        let pk = compressed_pubkey(&sk).unwrap();
        let sig = bch_schnorr_sign(&sk, &msg).unwrap();
        assert!(bch_schnorr_verify(&pk, &msg, &sig).unwrap());

        // Keep r_x unchanged while replacing R with -R.  Because secp256k1's
        // field prime is 3 mod 4, exactly one of R.y and (-R).y is a QR.
        // For sG - eP = R, s' = 2ed - s gives s'G - eP = -R.
        let mut chal = Vec::with_capacity(97);
        chal.extend_from_slice(&sig[..32]);
        chal.extend_from_slice(&pk);
        chal.extend_from_slice(&msg);
        let e = BigUint::from_bytes_be(&sha256(&chal));
        let d = BigUint::from_bytes_be(&sk);
        let s = BigUint::from_bytes_be(&sig[32..]);
        let n = BigUint::from_bytes_be(&SECP_N_BE);
        let twin_s = (BigUint::from(2u8) * e * d + &n - s) % &n;
        let twin_s_bytes = twin_s.to_bytes_be();
        let mut twin = sig;
        twin[32..].fill(0);
        twin[64 - twin_s_bytes.len()..].copy_from_slice(&twin_s_bytes);

        assert_eq!(&twin[..32], &sig[..32]);
        assert!(!bch_schnorr_verify(&pk, &msg, &twin).unwrap());
    }

    #[test]
    fn bch_schnorr_sign_verify_roundtrip() {
        let mut sk = [0u8; 32];
        sk[31] = 7;
        let pk = compressed_pubkey(&sk).unwrap();
        let msg = sha256(b"pickaxe-photon-gate");
        let sig = bch_schnorr_sign(&sk, &msg).unwrap();
        assert!(bch_schnorr_verify(&pk, &msg, &sig).unwrap());
    }
}
