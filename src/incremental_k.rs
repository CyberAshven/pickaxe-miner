//! Offline-only incremental-k experiment. Never compiled into the miner CLI.
//! Known k leaks the signing key: synthetic, unfunded identities only.
//! ponytail: k is limited to 1..=2^32; production needs explicit scalar/key rotation.
use super::*;
use crate::{crypto, search, tx};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const NONCE: u32 = 0x1234_5678;
const BATCH: u32 = 262_144;

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

struct Incremental {
    walk: CudaFunction,
    filters: [CudaFunction; 4],
    message: CudaSlice<u8>,
    step: CudaSlice<u32>,
    stride: u32,
    per_lane: u32,
}

impl Incremental {
    fn new(engine: &CudaPhotonEngine, per_lane: u32) -> Self {
        assert!((1..=128).contains(&per_lane));
        Self {
            walk: load_function(
                &engine._ctx,
                "photon_incremental_k.ptx",
                "pickaxe_photon_incremental_k",
            )
            .unwrap(),
            filters: STAGE_C3_FUNCTIONS.map(|name| {
                load_function(&engine._ctx, "photon_incremental_c3.ptx", name).unwrap()
            }),
            message: engine.stream.alloc_zeros(32).unwrap(),
            step: engine.stream.alloc_zeros(16).unwrap(),
            stride: 0,
            per_lane,
        }
    }

    fn set_message(&mut self, engine: &CudaPhotonEngine, target: &[u8; 32]) {
        let hash = tx::photon_message_sha256(NONCE, &hex::encode(target)).unwrap();
        engine.stream.memcpy_htod(&hash, &mut self.message).unwrap();
    }

    fn batch(
        &mut self,
        engine: &mut CudaPhotonEngine,
        base: u32,
        count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if !engine.job_ready
            || count > engine.max_candidates
            || u64::from(base) + u64::from(count) > 1u64 << 32
        {
            return Err("incremental batch is unconfigured, oversized, or exhausts the 32-bit experiment range".into());
        }
        if count == 0 {
            return Ok(PhotonCudaBatchResult {
                candidates: 0,
                total_winners: 0,
                winners: vec![],
            });
        }
        let blocks = count.div_ceil(self.per_lane).div_ceil(64);
        let stride = blocks * 64;
        if self.stride != stride {
            let point = PublicKey::from_secret_key(
                &SecretKey::from_secret_bytes(secret(u64::from(stride))).unwrap(),
            )
            .serialize_uncompressed();
            let words: Vec<u32> = point[1..]
                .chunks_exact(32)
                .flat_map(|coordinate| {
                    coordinate
                        .chunks_exact(4)
                        .rev()
                        .map(|word| u32::from_be_bytes(word.try_into().unwrap()))
                })
                .collect();
            engine.stream.memcpy_htod(&words, &mut self.step).unwrap();
            self.stride = stride;
        }
        engine
            .stream
            .memset_zeros(&mut engine.winner_count_gpu)
            .unwrap();
        unsafe {
            engine
                .stream
                .launch_builder(&self.walk)
                .arg(&base)
                .arg(&count)
                .arg(&engine.table_gpu)
                .arg(&self.step)
                .arg(&self.message)
                .arg(&mut engine.message_hashes_gpu)
                .arg(&mut engine.rfc6979_gpu)
                .arg(&mut engine.points_gpu)
                .launch(LaunchConfig {
                    grid_dim: (blocks, 1, 1),
                    block_dim: (64, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| format!("incremental point walk: {error}"))?;
        }
        std::mem::swap(&mut self.filters, &mut engine.stage_c3);
        let result = engine.finish_batch(base, count);
        std::mem::swap(&mut self.filters, &mut engine.stage_c3);
        result
    }
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
    out
}

#[test]
#[ignore = "requires built experimental PTX and a CUDA GPU; offline synthetic keys only"]
fn incremental_k_correctness() {
    let mut engine = CudaPhotonEngine::new(0, 257, 257).unwrap();
    let mut incremental = Incremental::new(&engine, 32);
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
            incremental.set_message(&engine, &target);
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
        incremental.set_message(&engine, &target);
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
    let mut incremental = Incremental::new(&engine, 32);
    let mut target = [255; 32];
    target[31] = 127;
    engine
        .set_job(&template(128, &target, &secret(7)), &target, &secret(7))
        .unwrap();
    incremental.set_message(&engine, &target);
    let result = incremental.batch(&mut engine, 65520, BATCH).unwrap();
    assert_eq!(result.total_winners, BATCH);
    assert!(result.truncated());
    let plus = engine.stream.clone_dtoh(&engine.signatures_gpu).unwrap();
    let minus = engine
        .stream
        .clone_dtoh(&engine.negated_nonce_s_gpu)
        .unwrap();
    let message = tx::photon_message_sha256(NONCE, &hex::encode(target)).unwrap();
    for index in [0usize, 1, 63, 8191, 8192, 16384, BATCH as usize - 1] {
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
    let mut incremental = Incremental::new(&engine, 32);
    let target: [u8; 32] =
        hex::decode("ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000")
            .unwrap()
            .try_into()
            .unwrap();
    engine
        .set_job(&template(10, &target, &[0x11; 32]), &target, &[0x11; 32])
        .unwrap();
    incremental.set_message(&engine, &target);
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
