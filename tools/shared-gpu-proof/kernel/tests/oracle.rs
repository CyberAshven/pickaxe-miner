use pickaxe_shared_hash_proof::{pickaxe_t2_hash_proof, sha256};
use sha2::{Digest, Sha256};
use spirv_std::glam::UVec3;

/// Full double SHA-256 by RustCrypto is independent of the resumed-round path.
#[test]
fn production_compression_chain_matches_full_transactions() {
    let mut inputs = Vec::new();
    let mut expected = Vec::new();
    for shift in 0..=16 {
        for seed in 0..32u64 {
            for j in [0u64, 1, 65535] {
                let mut transaction: Vec<u8> = (0..615 + shift)
                    .map(|i| ((i as u64 * 137 + seed * 73) ^ (i as u64 >> 2)) as u8)
                    .collect();
                let baton = 0xffff_ffffu64 + j;
                let reward = 5_000_000_000_000_000_000u64 - j;
                transaction[491 + shift..499 + shift].copy_from_slice(&baton.to_le_bytes());
                transaction[578 + shift..586 + shift].copy_from_slice(&reward.to_le_bytes());
                let digest: [u8; 32] = Sha256::digest(Sha256::digest(&transaction)).into();
                let bit_length = transaction.len() as u64 * 8;
                transaction.push(0x80);
                transaction.resize(632, 0);
                transaction.extend_from_slice(&bit_length.to_be_bytes());
                let mut prefix = sha256::INITIAL;
                for block in transaction[..448].chunks_exact(64) {
                    sha2::compress256(&mut prefix, &[(*block.first_chunk::<64>().unwrap()).into()]);
                }
                let block_words = |offset| {
                    core::array::from_fn::<_, 16, _>(|i| {
                        u32::from_be_bytes(
                            transaction[offset + i * 4..offset + i * 4 + 4]
                                .try_into()
                                .unwrap(),
                        )
                    })
                };
                let first = block_words(448);
                let middle = block_words(512);
                let last = block_words(576);
                let head = sha256::head10(prefix, first);
                let mut schedule = [0u32; 64];
                schedule[..16].copy_from_slice(&middle);
                for i in 16..64 {
                    let x = schedule[i - 15];
                    let y = schedule[i - 2];
                    schedule[i] = schedule[i - 16]
                        .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
                        .wrapping_add(schedule[i - 7])
                        .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10));
                }
                for strict in [false, true] {
                    for limit in [0, 1, 127, 255] {
                        let mut record = [0u32; 128];
                        record[..8].copy_from_slice(&prefix);
                        record[8..16].copy_from_slice(&head);
                        record[16..32].copy_from_slice(&first);
                        record[32..96].copy_from_slice(&schedule);
                        record[96..112].copy_from_slice(&last);
                        record[112] = limit;
                        record[113] = u32::from(strict);
                        inputs.extend(record);
                        let comparison_byte = if strict {
                            digest[31]
                        } else {
                            digest[31] & 0x7f
                        };
                        let accepted = u32::from(comparison_byte) <= limit;
                        expected.push((accepted, digest));
                    }
                }
            }
        }
    }
    let mut outputs = vec![0u32; expected.len() * 9];
    for index in 0..expected.len() + 65 {
        pickaxe_t2_hash_proof(UVec3::new(index as u32, 0, 0), &inputs, &mut outputs);
    }
    for (index, (accepted, digest)) in expected.iter().enumerate() {
        let actual = &outputs[index * 9..index * 9 + 9];
        assert_eq!(actual[8], u32::from(*accepted), "filter at {index}");
        if *accepted {
            let bytes: Vec<u8> = actual[..8]
                .iter()
                .flat_map(|word| word.to_be_bytes())
                .collect();
            assert_eq!(bytes.as_slice(), digest, "digest at {index}");
        }
    }
    if let Ok(directory) = std::env::var("PICKAXE_PROOF_FIXTURES") {
        let path = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&path).unwrap();
        let input_bytes: Vec<u8> = inputs.iter().flat_map(|word| word.to_le_bytes()).collect();
        let output_bytes: Vec<u8> = outputs.iter().flat_map(|word| word.to_le_bytes()).collect();
        std::fs::write(path.join("inputs.bin"), input_bytes).unwrap();
        std::fs::write(path.join("expected.bin"), output_bytes).unwrap();
    }
    println!(
        "{} independent full-transaction hash cases passed",
        expected.len()
    );
}
