//! Deterministic M29 16-bit fixed-base generator table.
//!
//! The M67.38 reference uses a 16-window x 65,536-entry table. Each entry is
//! 64 bytes: affine X then Y, each encoded as eight little-endian `u32` limbs.
//! Entry zero is unused/zero; entry `d` is `d * 2^(16*w) * G`.

use secp256k1::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub const M29_G16_BYTES: usize = 67_108_864;
pub const M29_G16_WINDOWS: usize = 16;
pub const M29_G16_ENTRIES: usize = 65_536;
pub const M29_G16_POINT_BYTES: usize = 64;
pub const M29_G16_SHA256: &str = "f6238556c4cf380be0479c14511c97cf996290eed88ce220c9dd28301c87b0e1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum M29TableSource {
    Cache,
    Generated,
}

fn cache_path() -> PathBuf {
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base)
            .join("Pickaxe")
            .join("cache")
            .join("photon-generator-table-m29-16-v1.bin");
    }
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(base)
            .join("pickaxe")
            .join("photon-generator-table-m29-16-v1.bin");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("pickaxe")
            .join("photon-generator-table-m29-16-v1.bin");
    }
    std::env::temp_dir()
        .join("pickaxe")
        .join("photon-generator-table-m29-16-v1.bin")
}

fn table_hash_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_table(bytes: &[u8]) -> bool {
    bytes.len() == M29_G16_BYTES && table_hash_hex(bytes) == M29_G16_SHA256
}

fn store_coordinate_words(dst: &mut [u8], coordinate_be: &[u8]) {
    debug_assert_eq!(dst.len(), 32);
    debug_assert_eq!(coordinate_be.len(), 32);
    for limb in 0..8 {
        let src = 28 - limb * 4;
        let word = u32::from_be_bytes(coordinate_be[src..src + 4].try_into().unwrap());
        dst[limb * 4..limb * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
}

fn store_point(table: &mut [u8], window: usize, digit: usize, point: &PublicKey) {
    let offset = (window * M29_G16_ENTRIES + digit) * M29_G16_POINT_BYTES;
    let uncompressed = point.serialize_uncompressed();
    store_coordinate_words(&mut table[offset..offset + 32], &uncompressed[1..33]);
    store_coordinate_words(&mut table[offset + 32..offset + 64], &uncompressed[33..65]);
}

pub fn generate_m29_g16() -> Result<Vec<u8>, String> {
    let one = SecretKey::from_secret_bytes({
        let mut key = [0u8; 32];
        key[31] = 1;
        key
    })
    .map_err(|error| format!("M29 generator scalar: {error}"))?;
    let mut base = PublicKey::from_secret_key(&one);
    let mut table = vec![0u8; M29_G16_BYTES];

    for window in 0..M29_G16_WINDOWS {
        let mut point = base;
        for digit in 1..M29_G16_ENTRIES {
            store_point(&mut table, window, digit, &point);
            if digit + 1 != M29_G16_ENTRIES {
                point = point
                    .combine(&base)
                    .map_err(|error| format!("M29 window {window} digit {digit}: {error}"))?;
            }
        }
        if window + 1 != M29_G16_WINDOWS {
            for _ in 0..16 {
                base = base
                    .combine(&base)
                    .map_err(|error| format!("M29 window {window} base doubling: {error}"))?;
            }
        }
    }

    let hash = table_hash_hex(&table);
    if hash != M29_G16_SHA256 {
        return Err(format!(
            "generated M29 table SHA-256 {hash} does not match authoritative {M29_G16_SHA256}"
        ));
    }
    Ok(table)
}

pub fn load_or_generate_m29_g16() -> Result<(Vec<u8>, M29TableSource), String> {
    let path = cache_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if valid_table(&bytes) {
            return Ok((bytes, M29TableSource::Cache));
        }
    }

    let bytes = generate_m29_g16()?;
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_ok() {
            let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
            if std::fs::write(&temporary, &bytes).is_ok() {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::rename(&temporary, &path);
                let _ = std::fs::remove_file(&temporary);
            }
        }
    }
    Ok((bytes, M29TableSource::Generated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_encoding_matches_m29_little_limb_layout() {
        let one = SecretKey::from_secret_bytes({
            let mut key = [0u8; 32];
            key[31] = 1;
            key
        })
        .unwrap();
        let g = PublicKey::from_secret_key(&one).serialize_uncompressed();
        let mut encoded = [0u8; 64];
        store_coordinate_words(&mut encoded[..32], &g[1..33]);
        store_coordinate_words(&mut encoded[32..], &g[33..65]);

        let x0 = u32::from_le_bytes(encoded[0..4].try_into().unwrap());
        let x7 = u32::from_le_bytes(encoded[28..32].try_into().unwrap());
        assert_eq!(x0, 0x16f8_1798);
        assert_eq!(x7, 0x79be_667e);
    }
}
