use crate::scalar::Scalar;
use hmac::{Hmac, Mac};
use sha2::Sha256;

fn hmac(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// BCH's RFC6979 domain tag is the exact 16-byte `Schnorr+SHA256  ` string.
/// Callers must validate the secret in 1..n. No allocation or native crypto.
pub fn bch_rfc6979(secret: &[u8; 32], message: &[u8; 32]) -> [u8; 32] {
    let mut v = [1; 32];
    let mut k = [0; 32];
    let mut data = [0; 113];
    data[33..65].copy_from_slice(secret);
    data[65..97].copy_from_slice(&Scalar::from_be_bytes(*message).to_be_bytes());
    data[97..].copy_from_slice(b"Schnorr+SHA256  ");
    for separator in [0, 1] {
        data[..32].copy_from_slice(&v);
        data[32] = separator;
        k = hmac(&k, &data);
        v = hmac(&k, &v);
    }
    loop {
        v = hmac(&k, &v);
        let scalar = Scalar::from_be_bytes(v);
        if scalar != Scalar::ZERO && scalar.to_be_bytes() == v {
            return v;
        }
        let mut retry = [0; 33];
        retry[..32].copy_from_slice(&v);
        k = hmac(&k, &retry);
        v = hmac(&k, &v);
    }
}
