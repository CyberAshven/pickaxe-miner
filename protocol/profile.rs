//! Build-time identity and fixture checks. VM tests establish spending semantics.
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    category_hex: String,
    redeem_script_hex: String,
    mining_vector_hex: String,
}

pub fn constants(source: &str) -> Result<String, String> {
    let profile: Profile = serde_json::from_str(source).map_err(|e| e.to_string())?;
    let decode = |value: &str| hex::decode(value).map_err(|e| e.to_string());
    let category = decode(&profile.category_hex)?;
    let redeem = decode(&profile.redeem_script_hex)?;
    let vector = decode(&profile.mining_vector_hex)?;
    if category.len() != 32 || category.iter().all(|b| *b == 0) {
        return Err("category must be a nonzero 32-byte display-order token ID".into());
    }
    if redeem.len() != 259 || vector.len() != 615 {
        return Err("unsupported protocol layout: need a 259-byte redeem script and 615-byte base vector; update and validate GPU layouts before adoption".into());
    }
    let mut lock = vec![0xaa, 0x20];
    lock.extend_from_slice(&Sha256::digest(Sha256::digest(&redeem)));
    lock.push(0x87);
    let mut category_le = category.clone();
    category_le.reverse();
    if vector[82..341] != redeem
        || vector[499..534] != lock
        || vector[356..388] != category_le
        || vector[544..576] != category_le
    {
        return Err("mining vector does not match profile redeem script, P2SH32 lock and both token categories".into());
    }
    let mut script_hash = Sha256::digest(&lock).to_vec();
    script_hash.reverse();
    let category = hex::encode(category);
    let script_hash = hex::encode(script_hash);
    Ok(format!(
        "pub const MAINNET_CATEGORY_HEX: &str = {category:?};\n\
         pub const COVENANT_LOCKING_BYTECODE_HEX: &str = {:?};\n\
         pub const EXPECTED_SCRIPT_HASH_HEX: &str = {script_hash:?};\n\
         pub const REDEEM_SCRIPT_HEX: &str = {:?};\n\
         pub const MINING_VECTOR_HEX: &str = {:?};\n\
         pub const CONTRACT_ID: &str = {:?};\n",
        hex::encode(lock),
        hex::encode(redeem),
        hex::encode(vector),
        format!("{category}:{script_hash}")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_binds_identity_and_rejects_partial_or_unsupported_swaps() {
        let source = include_str!("photon.json");
        let output = constants(source).unwrap();
        assert!(output.contains(crate::protocol::EXPECTED_SCRIPT_HASH_HEX));
        for field in ["category_hex", "redeem_script_hex", "mining_vector_hex"] {
            let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
            let mut bytes = hex::decode(value[field].as_str().unwrap()).unwrap();
            let index = if field == "mining_vector_hex" { 356 } else { 0 };
            bytes[index] ^= 1;
            value[field] = hex::encode(bytes).into();
            assert!(constants(&value.to_string()).is_err(), "{field}");
        }
        let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
        value["redeem_script_hex"] = "51".into();
        assert!(constants(&value.to_string())
            .unwrap_err()
            .contains("layout"));
        value["unknown"] = true.into();
        assert!(constants(&value.to_string()).is_err());
    }
}
