//! BCH Schnorr signing gate (Codex PERFORMANCE CONTRACT sec18).
//!
//! Matches postcorps/photon reference:
//! - RFC6979 nonce with algo tag `Schnorr+SHA256  `
//! - challenge `e = SHA256(r_x || compressed_pubkey || msg)`
//! - if R.y is not a quadratic residue mod p, use `k' = n - k` (r_x unchanged)
//!
//! The signer is shared by deterministic vector tests, live job setup, and rare
//! returned-winner reconstruction. Mining identities remain runtime-only.

use k256::elliptic_curve::{ops::Reduce, sec1::ToEncodedPoint, Group, PrimeField};
use k256::{ProjectivePoint, PublicKey, Scalar, SecretKey, U256};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use sha2::{Digest, Sha256};

const SECP_P_BE: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFF, 0xFF, 0xFC, 0x2F,
];

#[cfg(test)]
const SECP_N_BE: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE,
    0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
];

/// Derives a compressed public key using RustCrypto's constant-time arithmetic.
pub fn compressed_pubkey(sk_bytes: &[u8; 32]) -> Result<[u8; 33], String> {
    let secret = SecretKey::from_slice(sk_bytes).map_err(|e| e.to_string())?;
    Ok(secret
        .public_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .expect("compressed SEC1 length"))
}

pub fn uncompressed_pubkey(sk_bytes: &[u8; 32]) -> Result<[u8; 65], String> {
    let secret = SecretKey::from_slice(sk_bytes).map_err(|e| e.to_string())?;
    Ok(secret
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .try_into()
        .expect("uncompressed SEC1 length"))
}

pub fn random_keypair() -> ([u8; 32], [u8; 33]) {
    loop {
        let secret = rand::random::<[u8; 32]>();
        if let Ok(public) = compressed_pubkey(&secret) {
            return (secret, public);
        }
    }
}

/// Computes the SHA-256 digest of input bytes.
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(data));
    out
}

/// RFC6979 nonce for BCH Schnorr, shared with the Rust GPU kernel.
pub fn bch_rfc6979_nonce(sk_bytes: &[u8; 32], msg32: &[u8; 32]) -> Result<[u8; 32], String> {
    SecretKey::from_slice(sk_bytes).map_err(|error| error.to_string())?;
    Ok(pickaxe_rust_engine::nonce::bch_rfc6979(sk_bytes, msg32))
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
    let sk = SecretKey::from_slice(sk_bytes).map_err(|e| e.to_string())?;
    let pk_bytes = compressed_pubkey(sk_bytes)?;

    let k = SecretKey::from_slice(&k_bytes).map_err(|e| e.to_string())?;
    let r_unc = uncompressed_pubkey(&k_bytes)?;
    let mut r_x = [0u8; 32];
    r_x.copy_from_slice(&r_unc[1..33]);
    let mut r_y = [0u8; 32];
    r_y.copy_from_slice(&r_unc[33..65]);

    let k_adj = if y_is_quadratic_residue(&r_y) {
        *k.to_nonzero_scalar()
    } else {
        -*k.to_nonzero_scalar()
    };

    let mut chal = Vec::with_capacity(97);
    chal.extend_from_slice(&r_x);
    chal.extend_from_slice(&pk_bytes);
    chal.extend_from_slice(msg32);
    let e_bytes = sha256(&chal);
    let e = <Scalar as Reduce<U256>>::reduce_bytes(&e_bytes.into());
    let s_bytes = (k_adj + e * *sk.to_nonzero_scalar()).to_bytes();

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
    let pk = PublicKey::from_sec1_bytes(pk33).map_err(|e| e.to_string())?;
    let mut r_x = [0u8; 32];
    let mut s_bytes = [0u8; 32];
    r_x.copy_from_slice(&sig64[..32]);
    s_bytes.copy_from_slice(&sig64[32..]);

    let mut chal = Vec::with_capacity(97);
    chal.extend_from_slice(&r_x);
    chal.extend_from_slice(pk33);
    chal.extend_from_slice(msg32);
    let e_bytes = sha256(&chal);
    let e = <Scalar as Reduce<U256>>::reduce_bytes(&e_bytes.into());
    let Some(s) = Option::<Scalar>::from(Scalar::from_repr(s_bytes.into())) else {
        return Ok(false);
    };
    let r_prime = ProjectivePoint::GENERATOR * s - ProjectivePoint::from(*pk.as_affine()) * e;
    if bool::from(r_prime.is_identity()) {
        return Ok(false);
    }
    let r_encoded = r_prime.to_affine().to_encoded_point(false);
    let r_unc = r_encoded.as_bytes();
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
    fn rust_signer_matches_independent_native_points_and_integer_scalars() {
        let n = BigUint::from_bytes_be(&SECP_N_BE);
        for index in 0..128u32 {
            let key = sha256(&index.to_le_bytes());
            let message = sha256(&index.to_be_bytes());
            let nonce = bch_rfc6979_nonce(&key, &message).unwrap();
            let native_key = secp256k1::SecretKey::from_secret_bytes(key).unwrap();
            let public = secp256k1::PublicKey::from_secret_key(&native_key).serialize();
            assert_eq!(compressed_pubkey(&key).unwrap(), public);
            let r = secp256k1::PublicKey::from_secret_key(
                &secp256k1::SecretKey::from_secret_bytes(nonce).unwrap(),
            )
            .serialize_uncompressed();
            let mut challenge = r[1..33].to_vec();
            challenge.extend_from_slice(&public);
            challenge.extend_from_slice(&message);
            let k = BigUint::from_bytes_be(&nonce);
            let k = if y_is_quadratic_residue(r[33..].try_into().unwrap()) {
                k
            } else {
                &n - k
            };
            let s = (k + BigUint::from_bytes_be(&sha256(&challenge))
                * BigUint::from_bytes_be(&key))
                % &n;
            let mut expected = [0; 64];
            expected[..32].copy_from_slice(&r[1..33]);
            let bytes = s.to_bytes_be();
            expected[64 - bytes.len()..].copy_from_slice(&bytes);
            let actual = bch_schnorr_sign(&key, &message).unwrap();
            assert_eq!(actual, expected);
            assert!(bch_schnorr_verify(&public, &message, &actual).unwrap());
            let mut bad = actual;
            bad[63] ^= 1;
            assert!(!bch_schnorr_verify(&public, &message, &bad).unwrap());
            bad[32..].copy_from_slice(&SECP_N_BE);
            assert!(!bch_schnorr_verify(&public, &message, &bad).unwrap());
        }
        assert!(compressed_pubkey(&[0; 32]).is_err());
        assert!(compressed_pubkey(&SECP_N_BE).is_err());
    }

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
