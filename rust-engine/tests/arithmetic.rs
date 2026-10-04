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
        assert_eq!(
            x.is_square(),
            aa == BigUint::from(0u32) || aa.modpow(&((&p - 1u32) >> 1), &p) == BigUint::from(1u32)
        );
    }
    eprintln!("{} field vectors passed independently", inputs.len());
}
