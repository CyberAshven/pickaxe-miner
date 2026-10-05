use pickaxe_shared_hash_proof::pickaxe_t2_assembly_proof;
use spirv_std::glam::UVec3;

#[test]
fn shared_layout_matches_independent_amount_serialization() {
    let mut inputs = Vec::<u32>::new();
    let mut expected = Vec::<u32>::new();
    for shift in 0..=16 {
        for seed in 0..16u64 {
            for j in [0u64, 1, 255, 256, 32768, 65535] {
                let baton = 0xffff_ffffu64 - seed;
                let reward = 0x1_0000_0000u64 + seed;
                let mut tx: Vec<u8> = (0..615 + shift)
                    .map(|i| ((i as u64 * 137 + seed * 73) ^ (i as u64 >> 2)) as u8)
                    .collect();
                tx[491 + shift..499 + shift].copy_from_slice(&baton.to_le_bytes());
                tx[578 + shift..586 + shift].copy_from_slice(&reward.to_le_bytes());
                let mut record = [0u32; 176];
                // Deliberate nonzero garbage after the transaction must never
                // replace SHA padding or the encoded bit length.
                let mut padded_input = tx.clone();
                padded_input.resize(640, 0xa5);
                for (i, chunk) in padded_input.as_chunks::<4>().0.iter().enumerate() {
                    record[i] = u32::from_le_bytes(*chunk);
                }
                record[160] = baton as u32;
                record[161] = (baton >> 32) as u32;
                record[162] = reward as u32;
                record[163] = (reward >> 32) as u32;
                record[164] = j as u32;
                record[165] = shift as u32;
                inputs.extend(record);
                tx[491 + shift..499 + shift].copy_from_slice(&(baton + j).to_le_bytes());
                tx[578 + shift..586 + shift].copy_from_slice(&(reward - j).to_le_bytes());
                let bit_length = tx.len() as u64 * 8;
                tx.push(0x80);
                tx.resize(632, 0);
                tx.extend(bit_length.to_be_bytes());
                expected.extend(
                    tx[448..]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|v| u32::from_be_bytes(*v)),
                );
            }
        }
    }
    let count = inputs.len() / 176;
    let mut outputs = vec![0; expected.len()];
    for i in 0..count + 65 {
        pickaxe_t2_assembly_proof(UVec3::new(i as u32, 0, 0), &inputs, &mut outputs);
    }
    assert_eq!(outputs, expected);
    if let Ok(directory) = std::env::var("PICKAXE_PROOF_FIXTURES") {
        let path = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&path).unwrap();
        for (name, data) in [
            ("assembly-inputs.bin", &inputs),
            ("assembly-expected.bin", &expected),
        ] {
            std::fs::write(
                path.join(name),
                data.iter()
                    .flat_map(|word| word.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
    }
    println!("{count} independent transaction assembly cases passed");
}
