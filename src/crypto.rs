//! Secp256k1 / Schnorr scaffolding for the PHOTON candidate path.
//!
//! IMPORTANT: BCH Schnorr (CashVM / libauth) is not automatically identical to
//! every BIP340 helper. Wire byte-identical checks against reference/miner.js
//! CPU path before claiming wins. GPU kernels must match that same transcript.

use secp256k1::{PublicKey, Secp256k1, SecretKey};

/// Create a throwaway secp context (testing / scaffolding only).
pub fn secp_ctx() -> Secp256k1<secp256k1::All> {
    Secp256k1::new()
}

/// Derive compressed pubkey bytes (33) from a 32-byte secret. Never log the secret.
pub fn compressed_pubkey(sk_bytes: &[u8; 32]) -> Result<[u8; 33], String> {
    let _secp = secp_ctx();
    let sk = SecretKey::from_byte_array(*sk_bytes).map_err(|e| e.to_string())?;
    let pk = PublicKey::from_secret_key(&sk);
    Ok(pk.serialize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubkey_from_known_one() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let pk = compressed_pubkey(&sk).expect("sk=1");
        assert!(pk[0] == 0x02 || pk[0] == 0x03, "compressed prefix");
        assert_eq!(pk.len(), 33);
    }
}
