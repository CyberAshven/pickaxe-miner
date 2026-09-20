//! Secp256k1 helpers + **BCH Schnorr** (CashVM-style challenge).
//!
//! Product mining is **GPU/CUDA**. This module is a correctness / win-tx signing
//! gate — not a CPU hashrate product.
//!
//! Challenge (postcorps reference): `e = SHA256(r_x || compressed_pubkey || msg)`.

use secp256k1::{PublicKey, Scalar, SecretKey};
use sha2::{Digest, Sha256};

pub fn compressed_pubkey(sk_bytes: &[u8; 32]) -> Result<[u8; 33], String> {
    let sk = SecretKey::from_secret_bytes(*sk_bytes).map_err(|e| e.to_string())?;
    Ok(PublicKey::from_secret_key(&sk).serialize())
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(data));
    out
}

fn secret_from_hash(mut bytes: [u8; 32]) -> SecretKey {
    let mut counter: u32 = 0;
    loop {
        if let Ok(k) = SecretKey::from_secret_bytes(bytes) {
            return k;
        }
        counter = counter.wrapping_add(1);
        let mut buf = [0u8; 36];
        buf[..32].copy_from_slice(&bytes);
        buf[32..].copy_from_slice(&counter.to_le_bytes());
        bytes = sha256(&buf);
    }
}

/// BCH Schnorr sign → 64-byte `r_x || s`.
pub fn bch_schnorr_sign(sk_bytes: &[u8; 32], msg32: &[u8; 32]) -> Result<[u8; 64], String> {
    let sk = SecretKey::from_secret_bytes(*sk_bytes).map_err(|e| e.to_string())?;
    let pk_bytes = PublicKey::from_secret_key(&sk).serialize();

    let mut seed = [0u8; 64];
    seed[..32].copy_from_slice(sk_bytes);
    seed[32..].copy_from_slice(msg32);
    let k = secret_from_hash(sha256(&seed));

    let r_ser = PublicKey::from_secret_key(&k).serialize();
    let mut r_x = [0u8; 32];
    r_x.copy_from_slice(&r_ser[1..33]);

    let mut chal = Vec::with_capacity(97);
    chal.extend_from_slice(&r_x);
    chal.extend_from_slice(&pk_bytes);
    chal.extend_from_slice(msg32);
    let e_bytes = sha256(&chal);
    let e = Scalar::from_be_bytes(e_bytes).map_err(|_| "bad e")?;

    // s = k + e*d  via libsecp tweaks
    let ed = sk.mul_tweak(&e).map_err(|err| err.to_string())?;
    let s_key = k.add_tweak(&Scalar::from(ed)).map_err(|err| err.to_string())?;
    let s_bytes = s_key.to_secret_bytes();

    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&r_x);
    sig[32..].copy_from_slice(&s_bytes);
    Ok(sig)
}

pub fn bch_schnorr_verify(pk33: &[u8; 33], msg32: &[u8; 32], sig64: &[u8; 64]) -> Result<bool, String> {
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
    let r_ser = r_prime.serialize();
    Ok(r_ser[1..33] == r_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_from_known_one() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let pk = compressed_pubkey(&sk).unwrap();
        assert!(pk[0] == 0x02 || pk[0] == 0x03);
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
