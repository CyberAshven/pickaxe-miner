//! Stage B: k*G (fixed-base).
//!
//! v1: **host secp256k1** for correctness + API that GPU will match.
//! Product hashrate path = CUDA windowed fixed-base (next slice). Not a CPU miner product.

use secp256k1::{PublicKey, SecretKey};

/// Compute compressed pubkey for scalar `k` (32 BE bytes) as k*G.
pub fn kg_compressed(scalar_be32: &[u8; 32]) -> Result<[u8; 33], String> {
    let sk = SecretKey::from_secret_bytes(*scalar_be32).map_err(|e| e.to_string())?;
    Ok(PublicKey::from_secret_key(&sk).serialize())
}

/// Batch k*G on host (temporary Stage B until CUDA windows land).
pub fn kg_batch_host(scalars: &[[u8; 32]]) -> Result<Vec<[u8; 33]>, String> {
    let mut out = Vec::with_capacity(scalars.len());
    for s in scalars {
        out.push(kg_compressed(s)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kg_one() {
        let mut k = [0u8; 32];
        k[31] = 1;
        let pk = kg_compressed(&k).unwrap();
        assert!(pk[0] == 0x02 || pk[0] == 0x03);
        // secp256k1 G compressed known prefix
        assert_eq!(
            hex::encode(pk),
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );
    }
}
