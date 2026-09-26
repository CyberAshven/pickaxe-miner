//! Offline-only incremental-k experiment. Never compiled into the miner CLI.
//! Known k leaks the signing key: synthetic, unfunded identities only.
//! The live worker rotates its unfunded search identity after each 2^32 sweep.
use super::incremental::Incremental;
use super::*;
use crate::{crypto, search, tx};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const NONCE: u32 = 0x1234_5678;
const BATCH: u32 = 262_144;

#[test]
#[ignore = "serial Montgomery scalar arithmetic CUDA oracle"]
fn incremental_k_montgomery_scalar_oracle() {
    let ctx = CudaContext::new(0).unwrap();
    let stream = ctx.default_stream();
    let kernel =
        load_function(&ctx, "scalar-check.ptx", "pickaxe_scalar_montgomery_check").unwrap();
    let n = BigUint::from_bytes_be(
        &hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141").unwrap(),
    );
    let mut values = vec![
        BigUint::from(0u32),
        BigUint::from(1u32),
        &n - 1u32,
        &n - 2u32,
    ];
    for bits in (32..256).step_by(32) {
        values.push((BigUint::from(1u32) << bits) - 1u32);
        values.push(BigUint::from(1u32) << bits);
    }
    for i in 0..512u32 {
        values.push(BigUint::from_bytes_be(&Sha256::digest(i.to_le_bytes())) % &n);
    }
    let words = |value: &BigUint| {
        let mut out = value.to_u32_digits();
        out.resize(8, 0);
        out
    };
    let inputs: Vec<u32> = values.iter().flat_map(words).collect();
    let mut inputs_gpu = stream.alloc_zeros::<u32>(inputs.len()).unwrap();
    stream.memcpy_htod(&inputs, &mut inputs_gpu).unwrap();
    let mut table_gpu = stream.alloc_zeros::<u32>(FIXED_D_WORDS).unwrap();
    let mut output_gpu = stream.alloc_zeros::<u32>(inputs.len()).unwrap();
    let mut keys = vec![
        BigUint::from(1u32),
        BigUint::from(2u32),
        &n - 1u32,
        &n - 2u32,
    ];
    keys.extend(values.iter().skip(18).take(32).cloned());
    for d in &keys {
        let mut key = [0; 32];
        let bytes = d.to_bytes_be();
        key[32 - bytes.len()..].copy_from_slice(&bytes);
        stream
            .memcpy_htod(&fixed_d_table(&key), &mut table_gpu)
            .unwrap();
        let count = values.len() as u32;
        unsafe {
            stream
                .launch_builder(&kernel)
                .arg(&inputs_gpu)
                .arg(&table_gpu)
                .arg(&mut output_gpu)
                .arg(&count)
                .launch(LaunchConfig {
                    grid_dim: (count.div_ceil(64), 1, 1),
                    block_dim: (64, 1, 1),
                    shared_mem_bytes: 0,
                })
                .unwrap();
        }
        let outputs = stream.clone_dtoh(&output_gpu).unwrap();
        for (e, out) in values.iter().zip(outputs.as_chunks::<8>().0) {
            assert_eq!(out.as_slice(), words(&((e * d) % &n)), "e={e} d={d}");
        }
    }
    eprintln!(
        "PASS {} independent scalar products including zero, order and limb boundaries",
        keys.len() * values.len()
    );
}

#[test]
#[ignore = "serial C1 comparison; requires old photon_c1_reference.ptx and current kernels"]
fn incremental_k_c1_comparison() {
    let mut engine = CudaPhotonEngine::new(0, BATCH, 8).unwrap();
    let mut incremental = Incremental::new(&engine, 32).unwrap();
    let mut target = [0u8; 32];
    target[28] = 1;
    engine
        .set_job(&template(10, &target, &[0x11; 32]), &target, &[0x11; 32])
        .unwrap();
    incremental.set_message(&engine, &target, NONCE).unwrap();
    let kernels = [
        load_function(
            &engine._ctx,
            "photon_c1_reference.ptx",
            "pickaxe_photon_c1_schnorr_dual_batched",
        )
        .unwrap(),
        engine.stage_c1.clone(),
    ];
    let reverse = usize::from(std::env::var_os("PICKAXE_C1_REVERSE").is_some());
    engine.stage_c1 = kernels[reverse].clone();
    let mut records = Vec::new();
    let thermal_warmup = Instant::now();
    while thermal_warmup.elapsed() < Duration::from_secs(45) {
        incremental.batch(&mut engine, 0, BATCH).unwrap();
    }
    for (round, variant) in [0, 1, 1, 0, 0, 1, 1, 0].into_iter().enumerate() {
        let variant = variant ^ reverse;
        engine.stage_c1 = kernels[variant].clone();
        let mut base = 0u32;
        let mut run = || {
            let batch = incremental.batch(&mut engine, base, BATCH).unwrap();
            assert_eq!(batch.candidates, BATCH);
            base = base.wrapping_add(BATCH);
        };
        let warm = Instant::now();
        while warm.elapsed() < Duration::from_secs(1) {
            run();
        }
        let start = Instant::now();
        let mut candidates = 0u64;
        while start.elapsed() < Duration::from_secs(8) {
            run();
            candidates += u64::from(BATCH);
        }
        let seconds = start.elapsed().as_secs_f64();
        let rate = candidates as f64 / seconds / 1e6;
        eprintln!("round={round} variant={variant} M_candidates_per_s={rate:.6}");
        let telemetry = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=temperature.gpu,power.draw,clocks.sm,utilization.gpu",
                "--format=csv,noheader,nounits",
            ])
            .output()
            .unwrap();
        eprintln!(
            "telemetry={}",
            String::from_utf8_lossy(&telemetry.stdout).trim()
        );
        records.push(serde_json::json!({"round":round,"variant":variant,"candidates":candidates,"seconds":seconds,"million_candidates_per_second":rate,"telemetry_csv":String::from_utf8_lossy(&telemetry.stdout).trim()}));
    }
    std::fs::write(
        "artifacts/incremental-k/round3/comparison.json",
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
}

fn secret(value: u64) -> [u8; 32] {
    let mut out = [0; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

fn template(age: u32, target: &[u8; 32], key: &[u8; 32]) -> Vec<u8> {
    tx::build_photon_template_bytes(&tx::TemplateParams {
        prev_tx_hash_hex: "aa".repeat(32),
        prev_index: 0,
        age,
        public_key_hex: hex::encode(crypto::compressed_pubkey(key).unwrap()),
        target_hex: hex::encode(target),
        signature_hex: "00".repeat(64),
        nonce: NONCE,
        contract_value_sats: 15_971_500,
        contract_token_amount: 2_099_905_002_035_715,
        reward_amount: 4_999_773_813,
        payout_locking: hex::decode("76a9146e0810ceea13412b73feb41566a3d2d0ce54e10188ac").unwrap(),
    })
    .unwrap()
}

// Independent CPU oracle: libsecp256k1 point multiplication + BigUint scalars.
fn signature(key: &[u8; 32], message: &[u8; 32], k: u64) -> [u8; 64] {
    let point = PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret(k)).unwrap())
        .serialize_uncompressed();
    let p = BigUint::from_bytes_be(
        &hex::decode("fffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2f").unwrap(),
    );
    let n = BigUint::from_bytes_be(
        &hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141").unwrap(),
    );
    let square = BigUint::from_bytes_be(&point[33..]).modpow(&((&p - 1u32) / 2u32), &p)
        == BigUint::from(1u32);
    let adjusted = if square { BigUint::from(k) } else { &n - k };
    let public = crypto::compressed_pubkey(key).unwrap();
    let challenge = Sha256::digest([&point[1..33], &public, message].concat());
    let s = (adjusted + BigUint::from_bytes_be(&challenge) * BigUint::from_bytes_be(key)) % n;
    let mut out = [0; 64];
    out[..32].copy_from_slice(&point[1..33]);
    let s = s.to_bytes_be();
    out[64 - s.len()..].copy_from_slice(&s);
    assert!(crypto::bch_schnorr_verify(&public, message, &out).unwrap());
    assert_eq!(
        crypto::bch_schnorr_sign_search_candidate(key, message, k).unwrap(),
        out
    );
    out
}

#[test]
#[ignore = "requires built experimental PTX and a CUDA GPU; offline synthetic keys only"]
fn incremental_k_correctness() {
    let mut engine = CudaPhotonEngine::new(0, 257, 257).unwrap();
    let mut incremental = Incremental::new(&engine, 32).unwrap();
    incremental.c1_per_thread = 8;
    let mut vectors = Vec::new();
    let mut checked = 0;
    for age in [0u32, 16, 17, 127, 128, 32767, 32768, 65534] {
        for (key_value, key) in [secret(1), secret(7), [0x11; 32]].into_iter().enumerate() {
            // A valid age-adjusted target; enough winners and misses for VM proofs.
            let old = (BigUint::from(1u32) << 254usize) * 144u32 / (age + 143);
            let new = &old * (age + 143) / 144u32;
            let mut target = [0; 32];
            let bytes = new.to_bytes_le();
            target[..bytes.len()].copy_from_slice(&bytes);
            let raw = template(age, &target, &key);
            let layout = PhotonLayout::for_age(age).unwrap();
            let message = tx::photon_message_sha256(NONCE, &hex::encode(target)).unwrap();
            engine.set_job(&raw, &target, &key).unwrap();
            incremental.set_message(&engine, &target, NONCE).unwrap();
            for (base, count) in [(0, 1), (1, 65), (65520, 257), (u32::MAX - 256, 257)] {
                let result = incremental.batch(&mut engine, base, count).unwrap();
                let mut expected = Vec::new();
                for index in 0..count {
                    let candidate = base + index;
                    let sig = signature(&key, &message, u64::from(candidate) + 1);
                    let mut completed = raw.clone();
                    completed[layout.signature_offset()..layout.signature_offset() + 64]
                        .copy_from_slice(&sig);
                    let digest = search::hash256(&completed);
                    let meets = search::meets_target_le(&digest, &target);
                    if base == 0 || (base == 1 && index < 8) {
                        vectors.push(serde_json::json!({ "age": age, "raw": hex::encode(&completed), "old_target": old.to_str_radix(16), "meets": meets }));
                    }
                    if meets {
                        expected.push(PhotonCudaWinner {
                            nonce: candidate,
                            digest,
                            schnorr_k: None,
                        });
                    }
                    checked += 1;
                }
                let mut actual = result.winners;
                actual.sort_by_key(|winner| winner.nonce);
                assert_eq!(
                    actual, expected,
                    "age={age} key={key_value} base={base} count={count}"
                );
                assert_eq!(result.total_winners as usize, expected.len());
            }
            assert!(incremental.batch(&mut engine, u32::MAX, 2).is_err());
            assert!(incremental.batch(&mut engine, 0, 258).is_err());
            assert_eq!(incremental.batch(&mut engine, 0, 0).unwrap().candidates, 0);
        }
    }
    // Zero target rejects all; all-pass target exercises bounded readback.
    for (target, expected) in [
        ([0; 32], 0),
        (
            {
                let mut t = [255; 32];
                t[31] = 127;
                t
            },
            65,
        ),
    ] {
        engine
            .set_job(&template(17, &target, &secret(7)), &target, &secret(7))
            .unwrap();
        incremental.set_message(&engine, &target, NONCE).unwrap();
        engine.winner_cap = 4;
        let result = incremental.batch(&mut engine, 0, 65).unwrap();
        assert_eq!(result.total_winners, expected);
        assert_eq!(result.winners.len(), expected.min(4) as usize);
        assert_eq!(result.truncated(), expected > 4);
    }
    // Exercise the benchmark's full 32-step lane geometry, including its tail.
    drop(incremental);
    drop(engine);
    let mut engine = CudaPhotonEngine::new(0, BATCH, 8).unwrap();
    let mut incremental = Incremental::new(&engine, 32).unwrap();
    incremental.c1_per_thread = 8;
    let mut target = [255; 32];
    target[31] = 127;
    engine
        .set_job(&template(128, &target, &secret(7)), &target, &secret(7))
        .unwrap();
    incremental.set_message(&engine, &target, NONCE).unwrap();
    let result = incremental.batch(&mut engine, 65520, BATCH).unwrap();
    assert_eq!(result.total_winners, BATCH);
    assert!(result.truncated());
    let plus = engine.stream.clone_dtoh(&engine.signatures_gpu).unwrap();
    let minus = engine
        .stream
        .clone_dtoh(&engine.negated_nonce_s_gpu)
        .unwrap();
    let message = tx::photon_message_sha256(NONCE, &hex::encode(target)).unwrap();
    for index in [
        0usize,
        1,
        63,
        2047,
        2048,
        8191,
        8192,
        16384,
        BATCH as usize - 1,
    ] {
        let expected = signature(&secret(7), &message, 65521 + index as u64);
        assert_eq!(&plus[index * 64..index * 64 + 32], &expected[..32]);
        assert!(
            plus[index * 64 + 32..index * 64 + 64] == expected[32..]
                || minus[index * 32..index * 32 + 32] == expected[32..]
        );
    }
    std::fs::create_dir_all("artifacts/incremental-k").unwrap();
    std::fs::write(
        "artifacts/incremental-k/vectors.json",
        serde_json::to_vec_pretty(&vectors).unwrap(),
    )
    .unwrap();
    eprintln!("PASS {checked} independently reconstructed candidates, 8 ages, 3 keys, partial batches, u32 boundary, target and readback bounds; full-batch lane samples verified");
}

#[test]
#[ignore = "requires CUDA; run serially after correctness and record other GPU activity"]
fn incremental_k_benchmark() {
    let mut engine = CudaPhotonEngine::new(0, BATCH, 8).unwrap();
    let mut incremental = Incremental::new(&engine, 32).unwrap();
    let target: [u8; 32] =
        hex::decode("ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000")
            .unwrap()
            .try_into()
            .unwrap();
    engine
        .set_job(&template(10, &target, &[0x11; 32]), &target, &[0x11; 32])
        .unwrap();
    incremental.set_message(&engine, &target, NONCE).unwrap();
    let mut records = Vec::new();
    let mut base = 0u32;
    for (round, candidate) in [false, true, true, false, false, true, true, false]
        .into_iter()
        .enumerate()
    {
        let mut run = || {
            let result = if candidate {
                incremental.batch(&mut engine, base, BATCH)
            } else {
                engine.search_batch(base, BATCH)
            }
            .unwrap();
            assert_eq!(result.candidates, BATCH);
            base = base.wrapping_add(BATCH);
        };
        let warm = Instant::now();
        while warm.elapsed() < Duration::from_secs(1) {
            run();
        }
        let start = Instant::now();
        let mut candidates = 0u64;
        while start.elapsed() < Duration::from_secs(4) {
            run();
            candidates += u64::from(BATCH);
        }
        let seconds = start.elapsed().as_secs_f64();
        let rate = candidates as f64 / seconds / 1e6;
        let pipeline = if candidate { "incremental" } else { "baseline" };
        eprintln!("round={round} pipeline={pipeline} candidates={candidates} seconds={seconds:.6} M_candidates_per_s={rate:.6}");
        records.push(serde_json::json!({"round": round, "pipeline": pipeline, "candidates": candidates, "seconds": seconds, "million_candidates_per_second": rate}));
    }
    std::fs::create_dir_all("artifacts/incremental-k").unwrap();
    std::fs::write(
        "artifacts/incremental-k/benchmark.json",
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "CUDA tuning, serial offline synthetic-key comparison"]
fn incremental_k_tuning() {
    let mut engine = CudaPhotonEngine::new(0, BATCH, 8).unwrap();
    let mut target = [0u8; 32];
    target[28] = 1;
    engine
        .set_job(&template(10, &target, &[0x11; 32]), &target, &[0x11; 32])
        .unwrap();
    let mut records = Vec::new();
    {
        for per_lane in [16, 32, 64] {
            for c1 in [4, 8, 16] {
                let mut incremental = Incremental::new(&engine, per_lane).unwrap();
                incremental.c1_per_thread = c1;
                incremental.set_message(&engine, &target, NONCE).unwrap();
                let mut base = 65536u32;
                let mut run = || {
                    incremental.batch(&mut engine, base, BATCH).unwrap();
                    base = base.wrapping_add(BATCH);
                };
                let warm = Instant::now();
                while warm.elapsed() < Duration::from_millis(250) {
                    run();
                }
                let start = Instant::now();
                let mut candidates = 0u64;
                while start.elapsed() < Duration::from_secs(1) {
                    run();
                    candidates += u64::from(BATCH);
                }
                let rate = candidates as f64 / start.elapsed().as_secs_f64() / 1e6;
                eprintln!("lane={per_lane} c1={c1} M_candidates_per_s={rate:.3}");
                records.push(serde_json::json!({"per_lane":per_lane,"c1":c1,"million_candidates_per_second":rate}));
            }
        }
    }
    std::fs::write(
        "artifacts/incremental-k/tuning.json",
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
}
