use num_bigint::BigUint;
use pickaxe_rust_engine::field::{Field, P};
use pickaxe_rust_engine::{
    point::Point,
    scalar::{Scalar, N},
};
use sha2::{Digest, Sha256};

#[test]
fn gpu_sha256_matches_rustcrypto() {
    use pickaxe_rust_engine::sha256;
    for index in 0..256u32 {
        let r: [u8; 32] = Sha256::digest(index.to_le_bytes()).into();
        let message: [u8; 32] = Sha256::digest(index.to_be_bytes()).into();
        let mut public = [0; 33];
        public[0] = 2;
        public[1..].copy_from_slice(&r);
        let expected: [u8; 32] = Sha256::digest([r.as_slice(), &public, &message].concat()).into();
        assert_eq!(sha256::challenge(r, public, message), expected);
        let mut state =
            core::array::from_fn(|i| u32::from_be_bytes(r[i * 4..i * 4 + 4].try_into().unwrap()));
        let mut block = [0; 64];
        block[..32].copy_from_slice(&r);
        block[32..].copy_from_slice(&message);
        let mut expected = state;
        sha2::block_api::compress256(&mut expected, &[block]);
        let words: [u32; 16] = core::array::from_fn(|i| {
            u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let mut resumed = state;
        sha256::compress_from::<10>(&mut resumed, words, sha256::head10(state, words));
        assert_eq!(resumed, expected);
        let mut schedule = [0u32; 64];
        schedule[..16].copy_from_slice(&words);
        for i in 16..64 {
            let x = schedule[i - 15];
            let y = schedule[i - 2];
            schedule[i] = schedule[i - 16]
                .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
                .wrapping_add(schedule[i - 7])
                .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10));
        }
        let mut scheduled = state;
        sha256::compress_scheduled(&mut scheduled, &schedule);
        assert_eq!(scheduled, expected);
        sha256::compress_bytes(&mut state, block);
        assert_eq!(state, expected);
        let expected: [u8; 32] = Sha256::digest(sha256::bytes(state)).into();
        assert_eq!(sha256::hash_state(state), expected);
        let top = expected[31] & 0x7f;
        for limit in [0, 7, 127, 255, top, top.saturating_sub(1)] {
            assert_eq!(
                sha256::hash_state_filtered(state, limit),
                (top <= limit).then_some(expected),
            );
        }
    }
}

fn integer(words: [u32; 8]) -> BigUint {
    BigUint::new(words.to_vec())
}
fn words(value: BigUint) -> [u32; 8] {
    let digits = value.to_u32_digits();
    core::array::from_fn(|i| digits.get(i).copied().unwrap_or(0))
}

#[test]
fn nonce_matches_authoritative_photon_vector() {
    let decode = |text: &[u8]| Field(words(BigUint::parse_bytes(text, 16).unwrap())).to_be_bytes();
    let mut secret = [0; 32];
    secret[31] = 1;
    let message = decode(b"098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f");
    let expected = decode(b"615da2b700fbb1ae10a72a391ce49b17cd4d0f614311ac7d216281217fd7797a");
    assert_eq!(
        pickaxe_rust_engine::nonce::bch_rfc6979(&secret, &message),
        expected
    );
}

#[test]
fn scalars_and_points_match_independent_oracles() {
    let n = integer(N);
    let mut inputs = vec![
        [0; 32],
        [255; 32],
        Field(N).to_be_bytes(),
        Field(words(&n - 1u32)).to_be_bytes(),
    ];
    inputs.extend((0..256u32).map(|i| <[u8; 32]>::from(Sha256::digest(i.to_be_bytes()))));
    for (index, bytes) in inputs.iter().copied().enumerate() {
        let a = Scalar::from_be_bytes(bytes);
        let b = Scalar::from_be_bytes(inputs[(index * 37 + 11) % inputs.len()]);
        let aa = BigUint::from_bytes_be(&bytes) % &n;
        let bb = integer(b.0);
        assert_eq!(a.0, words(aa.clone()));
        let be_words = core::array::from_fn(|i| {
            u32::from_be_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        assert_eq!(pickaxe_rust_engine::sign::scalar_from_be_words(be_words), a);
        assert_eq!(a.add_mod(b).0, words((&aa + &bb) % &n));
        assert_eq!(a.negate().0, words((&n - &aa) % &n));
        let mont = Scalar(words((&bb << 256) % &n));
        assert_eq!(a.montgomery_mul(mont).0, words((&aa * &bb) % &n));
        if let Ok(secret) = secp256k1::SecretKey::from_secret_bytes(a.to_be_bytes()) {
            let expected = secp256k1::PublicKey::from_secret_key(&secret).serialize_uncompressed();
            let (x, y) = Point::generator_mul(a.to_be_bytes()).affine().unwrap();
            assert_eq!(x.to_be_bytes(), expected[1..33]);
            assert_eq!(y.to_be_bytes(), expected[33..65]);
        } else {
            assert!(Point::generator_mul(a.to_be_bytes()).affine().is_none());
        }
    }
    let g = Point::GENERATOR;
    assert!(g
        .add_affine(g.x, Field::ZERO.sub_mod(g.y))
        .affine()
        .is_none());
    assert_eq!(g.add_affine(g.x, g.y).affine(), g.double().affine());
    assert!(Point::generator_mul(Field(N).to_be_bytes())
        .affine()
        .is_none());
    eprintln!(
        "{} scalar and point vectors passed independently",
        inputs.len()
    );
}

#[test]
fn field_matches_independent_integer_oracle() {
    let p = integer(P);
    let mut inputs = vec![[0; 8], Field::ONE.0, P, words(&p - 1u32), [u32::MAX; 8]];
    for bits in (1..256).step_by(7) {
        inputs.push(words((BigUint::from(1u32) << bits) - 1u32));
        inputs.push(words(BigUint::from(1u32) << bits));
    }
    for i in 0..512u32 {
        inputs.push(words(BigUint::from_bytes_be(&Sha256::digest(
            i.to_le_bytes(),
        ))));
    }
    for (index, a) in inputs.iter().copied().enumerate() {
        let b = inputs[(index * 37 + 17) % inputs.len()];
        let aa = integer(a) % &p;
        let bb = integer(b) % &p;
        let x = Field::reduced(a);
        let y = Field::reduced(b);
        assert_eq!(x.0, words(aa.clone()));
        assert_eq!(x.add_mod(y).0, words((&aa + &bb) % &p));
        assert_eq!(x.sub_mod(y).0, words((&aa + &p - &bb) % &p));
        assert_eq!(Field(a).mul_mod(Field(b)).0, words((&aa * &bb) % &p));
        assert_eq!(x.square().0, words((&aa * &aa) % &p));
        assert_eq!(x.inverse().0, words(aa.modpow(&(&p - 2u32), &p)));
        let residue = aa.modpow(&((&p - 1u32) >> 1), &p) == BigUint::from(1u32);
        assert_eq!(x.is_square(), aa == BigUint::from(0u32) || residue);
        // Portable signer forms.
        assert_eq!(pickaxe_rust_engine::sign::inverse_binary(x), x.inverse());
        assert_eq!(pickaxe_rust_engine::sign::is_square_binary(x), residue);
        assert_eq!(
            pickaxe_rust_engine::sign::reversed(x.0)
                .map(u32::to_be_bytes)
                .concat(),
            x.to_be_bytes()
        );
    }
    eprintln!("{} field vectors passed independently", inputs.len());
}

// #### PR #22: the portable signer's word forms against independent oracles.
fn be_words(bytes: &[u8]) -> [u32; 8] {
    core::array::from_fn(|i| u32::from_be_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
}

fn word_bytes(words: [u32; 8]) -> [u8; 32] {
    let mut out = [0; 32];
    for (chunk, word) in out.chunks_mut(4).zip(words) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[test]
fn portable_signer_words_match_byte_forms() {
    use pickaxe_rust_engine::{nonce, sha256, sign};
    for index in 0..256u32 {
        let r: [u8; 32] = Sha256::digest(index.to_le_bytes()).into();
        let message: [u8; 32] = Sha256::digest(index.to_be_bytes()).into();
        let mut public = [0; 33];
        public[0] = 2 + (index & 1) as u8;
        public[1..].copy_from_slice(&Sha256::digest(r));
        let mut packed = [0u32; 9];
        packed[..8].copy_from_slice(&be_words(&public[..32]));
        packed[8] = u32::from(public[32]) << 24;
        assert_eq!(
            word_bytes(sign::challenge_words(
                be_words(&r),
                packed,
                be_words(&message)
            )),
            sha256::challenge(r, public, message)
        );
        let mut secret: [u8; 32] = Sha256::digest([b"key".as_slice(), &r].concat()).into();
        if index == 0 {
            secret = [0; 32];
            secret[31] = 1;
        }
        let words = be_words(&secret);
        assert_eq!(
            word_bytes(sign::rfc6979_words(
                sign::rfc6979_midstates(words),
                words,
                be_words(&message)
            )),
            nonce::bch_rfc6979(&secret, &message)
        );
    }
}

#[test]
fn portable_signatures_verify_independently() {
    use pickaxe_rust_engine::{nonce, sign};
    use secp256k1::{PublicKey, Scalar as Tweak, SecretKey};
    let p = integer(P);
    let n = integer(N);
    for index in 0..48u32 {
        let secret: [u8; 32] =
            Sha256::digest([b"signer".as_slice(), &index.to_le_bytes()].concat()).into();
        let Ok(key) = SecretKey::from_secret_bytes(secret) else {
            continue;
        };
        let public = PublicKey::from_secret_key(&key).serialize();
        let mut packed = [0u32; 9];
        packed[..8].copy_from_slice(&be_words(&public[..32]));
        packed[8] = u32::from(public[32]) << 24;
        let target: [u8; 32] = Sha256::digest(index.to_be_bytes()).into();
        let photon_nonce = index.wrapping_mul(0x9e37_79b9);

        let mut data = photon_nonce.to_le_bytes().to_vec();
        data.extend_from_slice(&target);
        let expected_message: [u8; 32] = Sha256::digest(&data).into();
        let secret_words = be_words(&secret);
        let (message, k) = sign::nonce_words(
            photon_nonce,
            be_words(&target),
            secret_words,
            sign::rfc6979_midstates(secret_words),
        );
        assert_eq!(word_bytes(message), expected_message);
        assert_eq!(
            k.to_be_bytes(),
            nonce::bch_rfc6979(&secret, &expected_message)
        );

        // Sum the sixteen window points as the GPU table would supply them.
        let mut point = Point::INFINITY;
        for window in 0..16 {
            let digit = sign::window_digit(k, window);
            let expected = (integer(k.0) >> (16 * window)) & BigUint::from(0xffffu32);
            assert_eq!(BigUint::from(digit), expected);
            if digit == 0 {
                continue;
            }
            let entry = Scalar(words(BigUint::from(digit) << (16 * window)));
            let (x, y) = Point::generator_mul(entry.to_be_bytes()).affine().unwrap();
            point = point.add_affine(x, y);
        }
        let d = Scalar::from_be_bytes(secret);
        let (r, s) = sign::signature_words(point, message, k, packed, d);
        let (r, s) = (word_bytes(r), word_bytes(s));

        // Independent BCH Schnorr verification: R = sG - eP, x(R) = r, y(R) square.
        let mut challenge = r.to_vec();
        challenge.extend_from_slice(&public);
        challenge.extend_from_slice(&expected_message);
        let e = BigUint::from_bytes_be(&Sha256::digest(&challenge)) % &n;
        let s_key = SecretKey::from_secret_bytes(s).unwrap();
        let negative_e = Tweak::from_be_bytes(Field(words((&n - &e) % &n)).to_be_bytes()).unwrap();
        let ep = PublicKey::from_slice(&public)
            .unwrap()
            .mul_tweak(&negative_e)
            .unwrap();
        let recovered = PublicKey::from_secret_key(&s_key)
            .combine(&ep)
            .unwrap()
            .serialize_uncompressed();
        assert_eq!(recovered[1..33], r);
        let y = BigUint::from_bytes_be(&recovered[33..65]);
        assert_eq!(y.modpow(&((&p - 1u32) >> 1), &p), BigUint::from(1u32));
    }
}

#[test]
fn portable_wide_matches_u64() {
    use pickaxe_rust_engine::wide::portable::{self, Wide};
    let value = |w: Wide| (u64::from(w.hi) << 32) | u64::from(w.lo);
    let split = |x: u64| Wide {
        lo: x as u32,
        hi: (x >> 32) as u32,
    };
    let mut words = vec![
        0,
        1,
        2,
        977,
        0xffff,
        0x1_0000,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
    ];
    for i in 0..64u32 {
        let digest = Sha256::digest(i.to_le_bytes());
        words.push(u32::from_le_bytes(digest[..4].try_into().unwrap()));
    }
    for &a in &words {
        for &b in &words {
            let wide = (u64::from(a) << 32) | u64::from(b);
            assert_eq!(value(portable::mul(a, b)), u64::from(a) * u64::from(b));
            assert_eq!(value(portable::extend(a)), u64::from(a));
            assert_eq!(portable::low(split(wide)), wide as u32);
            assert_eq!(value(portable::high(split(wide))), wide >> 32);
            assert_eq!(value(portable::sign(split(wide))), wide >> 63);
            assert_eq!(portable::nonzero(split(wide)), wide != 0);
            let other = u64::from(b).rotate_left(17) ^ u64::from(a);
            assert_eq!(value(split(wide) + split(other)), wide.wrapping_add(other));
            assert_eq!(
                value(portable::sub(split(wide), split(other))),
                wide.wrapping_sub(other)
            );
            let small = wide >> 34;
            assert_eq!(value(portable::scale(split(small), 977)), small * 977);
            let mut sum = split(wide);
            sum += split(other);
            assert_eq!(value(sum), wide.wrapping_add(other));
        }
    }
}

/// Independent BCH RFC6979 model with RustCrypto HMAC, taking `forced` retries.
fn rfc6979_model(secret: &[u8; 32], message: &[u8; 32], mut forced: u32) -> [u8; 32] {
    use hmac::{Hmac, KeyInit, Mac};
    let hmac = |key: &[u8; 32], parts: &[&[u8]]| -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        for part in parts {
            mac.update(part);
        }
        mac.finalize().into_bytes().into()
    };
    let h1 = Scalar::from_be_bytes(*message).to_be_bytes();
    let tag = b"Schnorr+SHA256  ";
    let mut v = [1u8; 32];
    let mut k = [0u8; 32];
    k = hmac(&k, &[&v, &[0], secret, &h1, tag]);
    v = hmac(&k, &[&v]);
    k = hmac(&k, &[&v, &[1], secret, &h1, tag]);
    v = hmac(&k, &[&v]);
    loop {
        v = hmac(&k, &[&v]);
        let scalar = Scalar::from_be_bytes(v);
        if forced == 0 && scalar != Scalar::ZERO && scalar.to_be_bytes() == v {
            return v;
        }
        forced = forced.saturating_sub(1);
        k = hmac(&k, &[&v, &[0]]);
        v = hmac(&k, &[&v]);
    }
}

#[test]
fn portable_rfc6979_retry_steps_match_model() {
    use pickaxe_rust_engine::{nonce, sign};
    for index in 0..32u32 {
        let secret: [u8; 32] =
            Sha256::digest([b"retry".as_slice(), &index.to_le_bytes()].concat()).into();
        let message: [u8; 32] = Sha256::digest(index.to_be_bytes()).into();
        let words = be_words(&secret);
        assert_eq!(
            rfc6979_model(&secret, &message, 0),
            nonce::bch_rfc6979(&secret, &message)
        );
        for forced in 0..3 {
            assert_eq!(
                word_bytes(sign::rfc6979_words_retrying(
                    sign::rfc6979_midstates(words),
                    words,
                    be_words(&message),
                    forced,
                )),
                rfc6979_model(&secret, &message, forced),
                "case {index}, forced retries {forced}"
            );
        }
    }
}

#[test]
fn portable_transaction_words_match_serialized_bytes() {
    use pickaxe_rust_engine::window;
    for shift in 0..=16usize {
        let length = 615 + shift;
        let seed = |tag: &str| -> [u8; 32] { Sha256::digest(format!("{tag}{shift}")).into() };
        let mut template: Vec<u8> = (0..length).map(|i| (i * 7 + shift) as u8).collect();
        template[394 + shift..426 + shift].copy_from_slice(&seed("target"));
        let nonce = 0x0102_0304u32 + shift as u32;
        let (r, s) = (seed("r"), seed("s"));
        let mut completed = template.clone();
        completed[390 + shift..394 + shift].copy_from_slice(&nonce.to_le_bytes());
        completed[426 + shift..458 + shift].copy_from_slice(&r);
        completed[458 + shift..490 + shift].copy_from_slice(&s);
        let mut padded = completed.clone();
        padded.push(0x80);
        padded.resize(632, 0);
        padded.extend_from_slice(&(length as u64 * 8).to_be_bytes());
        assert_eq!(padded.len(), 640);
        let mut packed = template.clone();
        packed.resize(632, 0);
        let packed: Vec<u32> = packed
            .chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect();
        for w in 0..64 {
            let expected = u32::from_be_bytes(padded[384 + w * 4..388 + w * 4].try_into().unwrap());
            assert_eq!(
                window::tx_word(&packed, shift, length, nonce, be_words(&r), be_words(&s), w),
                expected,
                "shift {shift}, word {w}"
            );
        }
        // The same words and schedule SHA-256 produces over the last block.
        let block: [u32; 16] = core::array::from_fn(|i| {
            u32::from_be_bytes(padded[576 + i * 4..580 + i * 4].try_into().unwrap())
        });
        let schedule = window::schedule(block);
        let mut expected = [0u32; 64];
        expected[..16].copy_from_slice(&block);
        for i in 16..64 {
            let (x, y) = (expected[i - 15], expected[i - 2]);
            expected[i] = expected[i - 16]
                .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
                .wrapping_add(expected[i - 7])
                .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10));
        }
        assert_eq!(schedule, expected);
    }
}

#[test]
fn portable_target_rule_matches_little_endian_comparison() {
    use pickaxe_rust_engine::window;
    let mut cases: Vec<([u8; 32], [u8; 32])> = Vec::new();
    for i in 0..512u32 {
        let digest: [u8; 32] = Sha256::digest(i.to_le_bytes()).into();
        let mut target: [u8; 32] = Sha256::digest(i.to_be_bytes()).into();
        if i % 4 == 0 {
            // Equal high bytes force the comparison deeper.
            target[16..].copy_from_slice(&digest[16..]);
        }
        if i % 8 == 1 {
            target = digest;
        }
        cases.push((digest, target));
    }
    cases.push(([0; 32], [0xff; 32]));
    for (digest, target) in cases {
        let mut masked = digest;
        masked[31] &= 0x7f;
        let below = masked.iter().rev().cmp(target.iter().rev()) == core::cmp::Ordering::Less;
        let positive_ok = digest[31] & 0x80 == 0 && digest.iter().any(|b| *b != 0);
        let mut template = vec![0u8; 400 + 32];
        template[400..].copy_from_slice(&target);
        let packed: Vec<u32> = template
            .chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(
            window::below_target(be_words(&digest), &packed, 400, false),
            below
        );
        assert_eq!(
            window::below_target(be_words(&digest), &packed, 400, true),
            below && positive_ok
        );
    }
}
