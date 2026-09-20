// PHOTON WebGPU Milestone 2 — fixed shared binding layout
//
// Both compute entry points use exactly the same storage-buffer bindings:
//
//   @binding(0): SharedInput
//   @binding(1): SharedOutput
//
// This avoids duplicate binding declarations in the same WGSL module.
//
// Entry points:
//   photon_message_sha256
//       SHA256(nonceLE || target), exactly 36 bytes.
//
//   transaction_hash256
//       SHA256(SHA256(transaction)) for the deterministic 615-byte PHOTON
//       transaction.
//
// Input words are stored in SHA-256 big-endian word order.

struct SharedInput {
    words: array<u32, 154>,
    byteLength: u32,
};

struct SharedOutput {
    // Milestone 1:
    //   words[0..7] = SHA256(message)
    //
    // Milestone 2:
    //   words[0..7]  = SHA256(transaction)
    //   words[8..15] = HASH256(transaction)
    words: array<u32, 16>,
};


@group(0) @binding(0)
var<storage, read> input: SharedInput;

@group(0) @binding(1)
var<storage, read_write> output: SharedOutput;

@group(0)
@binding(3)
var<storage, read>
generatorTable:
    array<u32>;

struct PrivateKeyInput {
    words:
        array<u32, 8>,
};

@group(0)
@binding(4)
var<storage, read>
privateKeyInput:
    PrivateKeyInput;


struct M27PrecomputedHmacState {
    words:
        array<u32, 16>,
};

@group(0)
@binding(5)
var<storage, read>
m27Precomputed:
    M27PrecomputedHmacState;


struct M30TransactionPrefixState {
    words:
        array<u32, 8>,
};

@group(0)
@binding(6)
var<storage, read>
m30TxPrefix:
    M30TransactionPrefixState;


struct M31ProfileControl {
    stage:
        u32,
};

@group(0)
@binding(7)
var<storage, read>
m31Control:
    M31ProfileControl;


struct M33ProfileControl {
    stage:
        u32,
};

@group(0)
@binding(8)
var<storage, read>
m33Control:
    M33ProfileControl;


@group(0)
@binding(9)
var<storage, read>
m35SignedTable:
    array<u32>;


@group(0)
@binding(10)
var<storage, read_write>
m37Intermediate:
    array<u32>;


@group(0)
@binding(11)
var<storage, read>
m39FixedDTable:
    array<u32>;


struct M42DispatchParams {
    groupsX:
        u32,

    candidateCount:
        u32,

    _pad0:
        u32,

    _pad1:
        u32,
};

@group(0)
@binding(12)
var<uniform>
m42DispatchParams:
    M42DispatchParams;




const K: array<u32, 64> = array<u32, 64>(
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u,
    0x3956c25bu, 0x59f111f1u, 0x923f82a4u, 0xab1c5ed5u,
    0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u,
    0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u, 0xc19bf174u,
    0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu,
    0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau,
    0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u,
    0xc6e00bf3u, 0xd5a79147u, 0x06ca6351u, 0x14292967u,
    0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu, 0x53380d13u,
    0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u,
    0xa2bfe8a1u, 0xa81a664bu, 0xc24b8b70u, 0xc76c51a3u,
    0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u,
    0x19a4c116u, 0x1e376c08u, 0x2748774cu, 0x34b0bcb5u,
    0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu, 0x682e6ff3u,
    0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u,
    0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u
);


fn rotr(
    x: u32,
    amount: u32
) -> u32 {
    return (
        x >> amount
    ) | (
        x << (
            32u - amount
        )
    );
}


fn choose(
    x: u32,
    y: u32,
    z: u32
) -> u32 {
    return (
        x & y
    ) ^ (
        (~x) & z
    );
}


fn majority(
    x: u32,
    y: u32,
    z: u32
) -> u32 {
    return (
        x & y
    ) ^ (
        x & z
    ) ^ (
        y & z
    );
}


fn bigSigma0(x: u32) -> u32 {
    return rotr(x, 2u) ^
        rotr(x, 13u) ^
        rotr(x, 22u);
}


fn bigSigma1(x: u32) -> u32 {
    return rotr(x, 6u) ^
        rotr(x, 11u) ^
        rotr(x, 25u);
}


fn smallSigma0(x: u32) -> u32 {
    return rotr(x, 7u) ^
        rotr(x, 18u) ^
        (x >> 3u);
}


fn smallSigma1(x: u32) -> u32 {
    return rotr(x, 17u) ^
        rotr(x, 19u) ^
        (x >> 10u);
}


fn initializeState() -> array<u32, 8> {
    var state: array<u32, 8>;

    state[0] = 0x6a09e667u;
    state[1] = 0xbb67ae85u;
    state[2] = 0x3c6ef372u;
    state[3] = 0xa54ff53au;
    state[4] = 0x510e527fu;
    state[5] = 0x9b05688cu;
    state[6] = 0x1f83d9abu;
    state[7] = 0x5be0cd19u;

    return state;
}


fn compressBlock(
    initialState: array<u32, 8>,
    first16: array<u32, 16>
) -> array<u32, 8> {
    var w: array<u32, 64>;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        w[i] =
            first16[i];
    }

    for (
        var i: u32 = 16u;
        i < 64u;
        i = i + 1u
    ) {
        w[i] =
            smallSigma1(
                w[i - 2u]
            ) +
            w[i - 7u] +
            smallSigma0(
                w[i - 15u]
            ) +
            w[i - 16u];
    }

    var a = initialState[0];
    var b = initialState[1];
    var c = initialState[2];
    var d = initialState[3];
    var e = initialState[4];
    var f = initialState[5];
    var g = initialState[6];
    var h = initialState[7];

    for (
        var i: u32 = 0u;
        i < 64u;
        i = i + 1u
    ) {
        let t1 =
            h +
            bigSigma1(e) +
            choose(e, f, g) +
            K[i] +
            w[i];

        let t2 =
            bigSigma0(a) +
            majority(a, b, c);

        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }

    var result: array<u32, 8>;

    result[0] = initialState[0] + a;
    result[1] = initialState[1] + b;
    result[2] = initialState[2] + c;
    result[3] = initialState[3] + d;
    result[4] = initialState[4] + e;
    result[5] = initialState[5] + f;
    result[6] = initialState[6] + g;
    result[7] = initialState[7] + h;

    return result;
}


@compute
@workgroup_size(1)
fn photon_message_sha256() {
    var block: array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 9u;
        i = i + 1u
    ) {
        block[i] =
            input.words[i];
    }

    block[9] = 0x80000000u;

    for (
        var i: u32 = 10u;
        i < 15u;
        i = i + 1u
    ) {
        block[i] = 0u;
    }

    block[15] = 288u;

    let digest =
        compressBlock(
            initializeState(),
            block
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            digest[i];
    }
}


fn paddedTransactionWord(
    wordIndex: u32,
    byteLength: u32,
    totalBlocks: u32
) -> u32 {
    let totalWords =
        totalBlocks * 16u;

    if (
        wordIndex ==
        totalWords - 2u
    ) {
        // For our current test/vector lengths (< 2^29 bytes), the high
        // 32 bits of the SHA-256 bit-length are zero.
        return 0u;
    }

    if (
        wordIndex ==
        totalWords - 1u
    ) {
        return byteLength << 3u;
    }

    let wordStartByte =
        wordIndex * 4u;

    if (
        wordStartByte <
        byteLength
    ) {
        let existingWord =
            input.words[wordIndex];

        if (
            wordStartByte + 4u <=
            byteLength
        ) {
            return existingWord;
        }

        let bytesInFinalWord =
            byteLength -
            wordStartByte;

        if (
            bytesInFinalWord == 1u
        ) {
            return existingWord |
                0x00800000u;
        }

        if (
            bytesInFinalWord == 2u
        ) {
            return existingWord |
                0x00008000u;
        }

        if (
            bytesInFinalWord == 3u
        ) {
            return existingWord |
                0x00000080u;
        }

        return existingWord;
    }

    if (
        wordStartByte ==
        byteLength
    ) {
        return 0x80000000u;
    }

    return 0u;
}


@compute
@workgroup_size(1)
fn transaction_hash256() {
    let byteLength =
        input.byteLength;

    let totalBlocks =
        (
            byteLength +
            9u +
            63u
        ) / 64u;

    var state =
        initializeState();

    for (
        var blockIndex: u32 = 0u;
        blockIndex < totalBlocks;
        blockIndex = blockIndex + 1u
    ) {
        var block: array<u32, 16>;

        for (
            var wordInBlock: u32 = 0u;
            wordInBlock < 16u;
            wordInBlock = wordInBlock + 1u
        ) {
            let globalWordIndex =
                blockIndex * 16u +
                wordInBlock;

            block[wordInBlock] =
                paddedTransactionWord(
                    globalWordIndex,
                    byteLength,
                    totalBlocks
                );
        }

        state =
            compressBlock(
                state,
                block
            );
    }

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            state[i];
    }

    var secondBlock: array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        secondBlock[i] =
            state[i];
    }

    secondBlock[8] =
        0x80000000u;

    for (
        var i: u32 = 9u;
        i < 15u;
        i = i + 1u
    ) {
        secondBlock[i] =
            0u;
    }

    secondBlock[15] =
        256u;

    let finalDigest =
        compressBlock(
            initializeState(),
            secondBlock
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[8u + i] =
            finalDigest[i];
    }
}


// ============================================================================
// Milestone 3 — secp256k1 field arithmetic and 2·G
//
// Portable correctness implementation using 8 little-endian u32 limbs.
// Multiplication uses bit-serial modular multiplication. This is intentionally
// correctness-first; later mining kernels will replace it with a faster limb
// multiplication/reduction strategy.
// ============================================================================

alias U256 = array<u32, 8>;


fn u256_zero() -> U256 {
    var r: U256;
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        r[i] = 0u;
    }
    return r;
}


fn u256_one() -> U256 {
    var r = u256_zero();
    r[0] = 1u;
    return r;
}


fn field_p() -> U256 {
    // secp256k1 p =
    // FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
    // little-endian u32 limbs
    var p: U256;
    p[0] = 0xfffffc2fu;
    p[1] = 0xfffffffeu;
    p[2] = 0xffffffffu;
    p[3] = 0xffffffffu;
    p[4] = 0xffffffffu;
    p[5] = 0xffffffffu;
    p[6] = 0xffffffffu;
    p[7] = 0xffffffffu;
    return p;
}


fn reduction_c() -> U256 {
    // 2^256 == 2^32 + 977 (mod p)
    var c = u256_zero();
    c[0] = 977u;
    c[1] = 1u;
    return c;
}


fn u256_ge(
    a: U256,
    b: U256
) -> bool {
    var i: i32 = 7;

    loop {
        if (a[u32(i)] > b[u32(i)]) {
            return true;
        }

        if (a[u32(i)] < b[u32(i)]) {
            return false;
        }

        if (i == 0) {
            break;
        }

        i = i - 1;
    }

    return true;
}


fn u256_sub_raw(
    a: U256,
    b: U256
) -> U256 {
    var r: U256;
    var borrow: u32 = 0u;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        let bi =
            b[i] + borrow;

        let carryFromB =
            select(
                0u,
                1u,
                bi < b[i]
            );

        let ri =
            a[i] - bi;

        let borrowFromA =
            select(
                0u,
                1u,
                a[i] < bi
            );

        r[i] =
            ri;

        borrow =
            carryFromB |
            borrowFromA;
    }

    return r;
}


fn add_with_carry(
    a: U256,
    b: U256
) -> array<U256, 2> {
    // result[0] is the low 256 bits.
    // result[1][0] is the carry bit.
    var packed: array<U256, 2>;
    var r: U256;
    var carry: u32 = 0u;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        let s1 =
            a[i] + b[i];

        let c1 =
            select(
                0u,
                1u,
                s1 < a[i]
            );

        let s2 =
            s1 + carry;

        let c2 =
            select(
                0u,
                1u,
                s2 < s1
            );

        r[i] =
            s2;

        carry =
            c1 | c2;
    }

    packed[0] =
        r;

    var carryBox =
        u256_zero();

    carryBox[0] =
        carry;

    packed[1] =
        carryBox;

    return packed;
}


fn normalize_field(
    value: U256
) -> U256 {
    var r =
        value;

    let p =
        field_p();

    // Inputs to this helper are already close to the field range.
    for (
        var j: u32 = 0u;
        j < 3u;
        j = j + 1u
    ) {
        if (u256_ge(r, p)) {
            r =
                u256_sub_raw(
                    r,
                    p
                );
        }
    }

    return r;
}


fn field_add(
    a: U256,
    b: U256
) -> U256 {
    let first =
        add_with_carry(
            a,
            b
        );

    var r =
        first[0];

    var carry =
        first[1][0];

    // Fold any 2^256 carry with:
    // 2^256 = 2^32 + 977 mod p.
    for (
        var fold: u32 = 0u;
        fold < 3u;
        fold = fold + 1u
    ) {
        if (carry == 0u) {
            break;
        }

        let folded =
            add_with_carry(
                r,
                reduction_c()
            );

        r =
            folded[0];

        carry =
            folded[1][0];
    }

    return normalize_field(r);
}


fn field_sub(
    a: U256,
    b: U256
) -> U256 {
    if (u256_ge(a, b)) {
        return u256_sub_raw(
            a,
            b
        );
    }

    let p =
        field_p();

    let difference =
        u256_sub_raw(
            b,
            a
        );

    return u256_sub_raw(
        p,
        difference
    );
}


fn u256_bit(
    a: U256,
    bitIndex: u32
) -> u32 {
    let wordIndex =
        bitIndex >> 5u;

    let bitInWord =
        bitIndex & 31u;

    return (
        a[wordIndex] >>
        bitInWord
    ) & 1u;
}


fn field_mul(
    a: U256,
    b: U256
) -> U256 {
    // Milestone 11 fast field multiplication.
    //
    // Radix B = 2^16. Each U256 becomes 16 base-B digits. 16x16 products
    // fit safely in WGSL u32 because:
    //
    //   0xffff * 0xffff + 0xffff + 0xffff = 0xffffffff
    //
    // secp256k1:
    //
    //   p = 2^256 - 2^32 - 977
    //
    // therefore:
    //
    //   B^16 = 2^256 = B^2 + 977 (mod p)
    //
    // High radix digits are folded with this pseudo-Mersenne identity.

    var a16:
        array<u32, 16>;

    var b16:
        array<u32, 16>;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        a16[limb * 2u] =
            a[limb] &
            0xffffu;

        a16[limb * 2u + 1u] =
            a[limb] >>
            16u;

        b16[limb * 2u] =
            b[limb] &
            0xffffu;

        b16[limb * 2u + 1u] =
            b[limb] >>
            16u;
    }

    var product:
        array<u32, 34>;

    for (
        var i: u32 = 0u;
        i < 34u;
        i = i + 1u
    ) {
        product[i] =
            0u;
    }

    // Exact 512-bit schoolbook product, maintained in normalized radix-B
    // digits after each row.
    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        var carry:
            u32 =
            0u;

        for (
            var j: u32 = 0u;
            j < 16u;
            j = j + 1u
        ) {
            let index =
                i + j;

            let uv =
                product[index] +
                a16[i] *
                b16[j] +
                carry;

            product[index] =
                uv &
                0xffffu;

            carry =
                uv >>
                16u;
        }

        var propagate =
            carry;

        var q =
            i +
            16u;

        loop {
            if (
                propagate == 0u ||
                q >= 34u
            ) {
                break;
            }

            let sum =
                product[q] +
                propagate;

            product[q] =
                sum &
                0xffffu;

            propagate =
                sum >>
                16u;

            q =
                q +
                1u;
        }
    }

    // Fold all digits B^16 and above. Three fixed passes are more than enough
    // for the carry generated by the pseudo-Mersenne fold and keep control flow
    // deterministic across vendors.
    for (
        var reducePass: u32 = 0u;
        reducePass < 3u;
        reducePass = reducePass + 1u
    ) {
        var k: i32 =
            33;

        loop {
            if (k < 16) {
                break;
            }

            let ku =
                u32(k);

            let high =
                product[ku];

            product[ku] =
                0u;

            product[ku - 16u] =
                product[ku - 16u] +
                high *
                977u;

            product[ku - 14u] =
                product[ku - 14u] +
                high;

            k =
                k -
                1;
        }

        var carry:
            u32 =
            0u;

        for (
            var i: u32 = 0u;
            i < 34u;
            i = i + 1u
        ) {
            let value =
                product[i] +
                carry;

            product[i] =
                value &
                0xffffu;

            carry =
                value >>
                16u;
        }
    }

    var result:
        U256;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        result[limb] =
            product[limb * 2u] |
            (
                product[limb * 2u + 1u] <<
                16u
            );
    }

    return normalize_field(
        result
    );
}


fn field_inv(
    a: U256
) -> U256 {
    // Fermat: a^(p-2) mod p
    // p-2 =
    // FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2D
    var exponent: U256;
    exponent[0] = 0xfffffc2du;
    exponent[1] = 0xfffffffeu;
    exponent[2] = 0xffffffffu;
    exponent[3] = 0xffffffffu;
    exponent[4] = 0xffffffffu;
    exponent[5] = 0xffffffffu;
    exponent[6] = 0xffffffffu;
    exponent[7] = 0xffffffffu;

    var result =
        u256_one();

    var i: i32 =
        255;

    loop {
        result =
            field_mul(
                result,
                result
            );

        if (
            u256_bit(
                exponent,
                u32(i)
            ) == 1u
        ) {
            result =
                field_mul(
                    result,
                    a
                );
        }

        if (i == 0) {
            break;
        }

        i =
            i - 1;
    }

    return result;
}


fn secp_gx() -> U256 {
    // 79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
    var x: U256;
    x[0] = 0x16f81798u;
    x[1] = 0x59f2815bu;
    x[2] = 0x2dce28d9u;
    x[3] = 0x029bfcdbu;
    x[4] = 0xce870b07u;
    x[5] = 0x55a06295u;
    x[6] = 0xf9dcbbacu;
    x[7] = 0x79be667eu;
    return x;
}


fn secp_gy() -> U256 {
    // 483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8
    var y: U256;
    y[0] = 0xfb10d4b8u;
    y[1] = 0x9c47d08fu;
    y[2] = 0xa6855419u;
    y[3] = 0xfd17b448u;
    y[4] = 0x0e1108a8u;
    y[5] = 0x5da4fbfcu;
    y[6] = 0x26a3c465u;
    y[7] = 0x483ada77u;
    return y;
}


fn point_double_g() -> array<U256, 2> {
    let x1 =
        secp_gx();

    let y1 =
        secp_gy();

    let xSquared =
        field_mul(
            x1,
            x1
        );

    let twoXSquared =
        field_add(
            xSquared,
            xSquared
        );

    let threeXSquared =
        field_add(
            twoXSquared,
            xSquared
        );

    let twoY =
        field_add(
            y1,
            y1
        );

    let inverseTwoY =
        field_inv(
            twoY
        );

    let slope =
        field_mul(
            threeXSquared,
            inverseTwoY
        );

    let slopeSquared =
        field_mul(
            slope,
            slope
        );

    let twoX =
        field_add(
            x1,
            x1
        );

    let x2 =
        field_sub(
            slopeSquared,
            twoX
        );

    let x1MinusX2 =
        field_sub(
            x1,
            x2
        );

    let y2 =
        field_sub(
            field_mul(
                slope,
                x1MinusX2
            ),
            y1
        );

    var point: array<U256, 2>;
    point[0] = x2;
    point[1] = y2;

    return point;
}


@compute
@workgroup_size(1)
fn secp256k1_two_g() {
    let point =
        point_double_g();

    let x =
        point[0];

    let y =
        point[1];

    // Convert little-endian limb arrays into normal big-endian 256-bit
    // word order for JavaScript display/comparison.
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            x[7u - i];

        output.words[8u + i] =
            y[7u - i];
    }
}


// ============================================================================
// Milestone 3 diagnostic v2 — Jacobian doubling of G, no field inversion.
//
// For a = 0 and Z1 = 1:
//   A  = X1^2
//   B  = Y1^2
//   C  = B^2
//   D  = 2*((X1+B)^2 - A - C)
//   E  = 3*A
//   F  = E^2
//   X3 = F - 2*D
//   Y3 = E*(D-X3) - 8*C
//   Z3 = 2*Y1
//
// We output X3 and Y3. Z3 is known and independently provided in the JS
// vector for reference.
// ============================================================================

fn field_double(
    a: U256
) -> U256 {
    return field_add(
        a,
        a
    );
}


fn field_triple(
    a: U256
) -> U256 {
    return field_add(
        field_double(a),
        a
    );
}


fn field_times8(
    a: U256
) -> U256 {
    return field_double(
        field_double(
            field_double(a)
        )
    );
}


@compute
@workgroup_size(1)
fn secp256k1_two_g_jacobian() {
    let x1 =
        secp_gx();

    let y1 =
        secp_gy();

    let A =
        field_mul(
            x1,
            x1
        );

    let B =
        field_mul(
            y1,
            y1
        );

    let C =
        field_mul(
            B,
            B
        );

    let xPlusB =
        field_add(
            x1,
            B
        );

    let xPlusBSquared =
        field_mul(
            xPlusB,
            xPlusB
        );

    let innerD =
        field_sub(
            field_sub(
                xPlusBSquared,
                A
            ),
            C
        );

    let D =
        field_double(
            innerD
        );

    let E =
        field_triple(
            A
        );

    let F =
        field_mul(
            E,
            E
        );

    let X3 =
        field_sub(
            F,
            field_double(D)
        );

    let DMinusX3 =
        field_sub(
            D,
            X3
        );

    let Y3 =
        field_sub(
            field_mul(
                E,
                DMinusX3
            ),
            field_times8(C)
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            X3[7u - i];

        output.words[8u + i] =
            Y3[7u - i];
    }
}


// ============================================================================
// Milestone 3 field diagnostic ladder.
//
// input.words[0]:
//   0 = echo generator X
//   1 = 2 + 3 mod p
//   2 = 2 * 3 mod p
//   3 = Gx * 1 mod p
//   4 = Gx * Gx mod p
// ============================================================================

fn u256_small(
    value: u32
) -> U256 {
    var r =
        u256_zero();

    r[0] =
        value;

    return r;
}


fn diagnostic_write_u256(
    value: U256
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            value[7u - i];
    }

    for (
        var i: u32 = 8u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn secp256k1_field_diagnostic() {
    let selector =
        input.words[0];

    if (selector == 0u) {
        diagnostic_write_u256(
            secp_gx()
        );
        return;
    }

    if (selector == 1u) {
        diagnostic_write_u256(
            field_add(
                u256_small(2u),
                u256_small(3u)
            )
        );
        return;
    }

    if (selector == 2u) {
        diagnostic_write_u256(
            field_mul(
                u256_small(2u),
                u256_small(3u)
            )
        );
        return;
    }

    if (selector == 3u) {
        diagnostic_write_u256(
            field_mul(
                secp_gx(),
                u256_one()
            )
        );
        return;
    }

    if (selector == 4u) {
        let gxValue =
            secp_gx();

        diagnostic_write_u256(
            field_mul(
                gxValue,
                gxValue
            )
        );
        return;
    }

    diagnostic_write_u256(
        u256_zero()
    );
}


// ============================================================================
// Milestone 3 Jacobian doubling diagnostic.
//
// Selector in input.words[0]:
//   0 A  = X1^2
//   1 B  = Y1^2
//   2 C  = B^2
//   3 D  = 2*((X1+B)^2 - A - C)
//   4 E  = 3*A
//   5 F  = E^2
//   6 X3 = F - 2*D
//   7 Y3 = E*(D-X3) - 8*C
//   8 Z3 = 2*Y1
// ============================================================================

fn jacobian_diag_write(
    value: U256
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            value[7u - i];
    }

    for (
        var i: u32 = 8u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn secp256k1_jacobian_diagnostic() {
    let selector =
        input.words[0];

    let X1 =
        secp_gx();

    let Y1 =
        secp_gy();

    let A =
        field_mul(
            X1,
            X1
        );

    let B =
        field_mul(
            Y1,
            Y1
        );

    let C =
        field_mul(
            B,
            B
        );

    let X1PlusB =
        field_add(
            X1,
            B
        );

    let X1PlusBSquared =
        field_mul(
            X1PlusB,
            X1PlusB
        );

    let innerD =
        field_sub(
            field_sub(
                X1PlusBSquared,
                A
            ),
            C
        );

    let D =
        field_add(
            innerD,
            innerD
        );

    let twoA =
        field_add(
            A,
            A
        );

    let E =
        field_add(
            twoA,
            A
        );

    let F =
        field_mul(
            E,
            E
        );

    let twoD =
        field_add(
            D,
            D
        );

    let X3 =
        field_sub(
            F,
            twoD
        );

    let DMinusX3 =
        field_sub(
            D,
            X3
        );

    let EDifference =
        field_mul(
            E,
            DMinusX3
        );

    let twoC =
        field_add(
            C,
            C
        );

    let fourC =
        field_add(
            twoC,
            twoC
        );

    let eightC =
        field_add(
            fourC,
            fourC
        );

    let Y3 =
        field_sub(
            EDifference,
            eightC
        );

    let Z3 =
        field_add(
            Y1,
            Y1
        );

    if (selector == 0u) {
        jacobian_diag_write(A);
        return;
    }

    if (selector == 1u) {
        jacobian_diag_write(B);
        return;
    }

    if (selector == 2u) {
        jacobian_diag_write(C);
        return;
    }

    if (selector == 3u) {
        jacobian_diag_write(D);
        return;
    }

    if (selector == 4u) {
        jacobian_diag_write(E);
        return;
    }

    if (selector == 5u) {
        jacobian_diag_write(F);
        return;
    }

    if (selector == 6u) {
        jacobian_diag_write(X3);
        return;
    }

    if (selector == 7u) {
        jacobian_diag_write(Y3);
        return;
    }

    if (selector == 8u) {
        jacobian_diag_write(Z3);
        return;
    }

    jacobian_diag_write(
        u256_zero()
    );
}


// ============================================================================
// Milestone 4 — secp256k1 Jacobian scalar multiplication.
//
// Small scalar is provided in input.words[0].
// Coordinate selector is input.words[1]: 0=X, 1=Y, 2=Z.
//
// Uses left-to-right binary double-and-add with mixed Jacobian + affine G.
// ============================================================================

struct JacobianPoint {
    x: U256,
    y: U256,
    z: U256,
};


fn jacobian_infinity() -> JacobianPoint {
    var p: JacobianPoint;
    p.x = u256_zero();
    p.y = u256_one();
    p.z = u256_zero();
    return p;
}


fn u256_is_zero(
    a: U256
) -> bool {
    var accum: u32 = 0u;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        accum =
            accum | a[i];
    }

    return accum == 0u;
}


fn u256_equal(
    a: U256,
    b: U256
) -> bool {
    var diff: u32 = 0u;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        diff =
            diff | (
                a[i] ^ b[i]
            );
    }

    return diff == 0u;
}


fn jacobian_double(
    p1: JacobianPoint
) -> JacobianPoint {
    if (
        u256_is_zero(p1.z) ||
        u256_is_zero(p1.y)
    ) {
        return jacobian_infinity();
    }

    // General Jacobian doubling for secp256k1 (a = 0):
    // YY = Y1^2
    // S  = 4*X1*YY
    // M  = 3*X1^2
    // X3 = M^2 - 2*S
    // Y3 = M*(S-X3) - 8*YY^2
    // Z3 = 2*Y1*Z1

    let YY =
        field_mul(
            p1.y,
            p1.y
        );

    let XYY =
        field_mul(
            p1.x,
            YY
        );

    let twoXYY =
        field_add(
            XYY,
            XYY
        );

    let S =
        field_add(
            twoXYY,
            twoXYY
        );

    let XX =
        field_mul(
            p1.x,
            p1.x
        );

    let twoXX =
        field_add(
            XX,
            XX
        );

    let M =
        field_add(
            twoXX,
            XX
        );

    let M2 =
        field_mul(
            M,
            M
        );

    let twoS =
        field_add(
            S,
            S
        );

    let X3 =
        field_sub(
            M2,
            twoS
        );

    let YY2 =
        field_mul(
            YY,
            YY
        );

    let twoYY2 =
        field_add(
            YY2,
            YY2
        );

    let fourYY2 =
        field_add(
            twoYY2,
            twoYY2
        );

    let eightYY2 =
        field_add(
            fourYY2,
            fourYY2
        );

    let SMinusX3 =
        field_sub(
            S,
            X3
        );

    let MSMinusX3 =
        field_mul(
            M,
            SMinusX3
        );

    let Y3 =
        field_sub(
            MSMinusX3,
            eightYY2
        );

    let YZ =
        field_mul(
            p1.y,
            p1.z
        );

    let Z3 =
        field_add(
            YZ,
            YZ
        );

    var result: JacobianPoint;
    result.x = X3;
    result.y = Y3;
    result.z = Z3;

    return result;
}


fn jacobian_add_affine(
    p1: JacobianPoint,
    qx: U256,
    qy: U256
) -> JacobianPoint {
    // Mixed addition: Jacobian P1 + affine Q.
    if (u256_is_zero(p1.z)) {
        var q: JacobianPoint;
        q.x = qx;
        q.y = qy;
        q.z = u256_one();
        return q;
    }

    let Z1Z1 =
        field_mul(
            p1.z,
            p1.z
        );

    let U2 =
        field_mul(
            qx,
            Z1Z1
        );

    let Z1Cubed =
        field_mul(
            p1.z,
            Z1Z1
        );

    let S2 =
        field_mul(
            qy,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            p1.x
        );

    let R =
        field_sub(
            S2,
            p1.y
        );

    if (u256_is_zero(H)) {
        if (u256_is_zero(R)) {
            return jacobian_double(p1);
        }

        return jacobian_infinity();
    }

    let HH =
        field_mul(
            H,
            H
        );

    let HHH =
        field_mul(
            H,
            HH
        );

    let V =
        field_mul(
            p1.x,
            HH
        );

    let R2 =
        field_mul(
            R,
            R
        );

    let twoV =
        field_add(
            V,
            V
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            twoV
        );

    let VMinusX3 =
        field_sub(
            V,
            X3
        );

    let RVMinusX3 =
        field_mul(
            R,
            VMinusX3
        );

    let Y1HHH =
        field_mul(
            p1.y,
            HHH
        );

    let Y3 =
        field_sub(
            RVMinusX3,
            Y1HHH
        );

    let Z3 =
        field_mul(
            p1.z,
            H
        );

    var result: JacobianPoint;
    result.x = X3;
    result.y = Y3;
    result.z = Z3;

    return result;
}


fn jacobian_add_generator(
    p1: JacobianPoint
) -> JacobianPoint {
    return jacobian_add_affine(
        p1,
        secp_gx(),
        secp_gy()
    );
}


fn scalar_mul_small_g(
    scalar: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    // Small-scalar milestone: process all 32 bits, left to right.
    var bit: i32 =
        31;

    loop {
        result =
            jacobian_double(
                result
            );

        if (
            (
                (
                    scalar >>
                    u32(bit)
                ) & 1u
            ) == 1u
        ) {
            result =
                jacobian_add_generator(
                    result
                );
        }

        if (bit == 0) {
            break;
        }

        bit =
            bit - 1;
    }

    return result;
}


fn write_scalar_coordinate(
    value: U256
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            value[7u - i];
    }

    for (
        var i: u32 = 8u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn secp256k1_scalar_mul_g() {
    let scalar =
        input.words[0];

    let coordinate =
        input.words[1];

    let point =
        scalar_mul_small_g(
            scalar
        );

    if (coordinate == 0u) {
        write_scalar_coordinate(
            point.x
        );
        return;
    }

    if (coordinate == 1u) {
        write_scalar_coordinate(
            point.y
        );
        return;
    }

    if (coordinate == 2u) {
        write_scalar_coordinate(
            point.z
        );
        return;
    }

    write_scalar_coordinate(
        u256_zero()
    );
}


// ============================================================================
// Milestone 5 — full 256-bit scalar multiplication.
//
// input.words[0..7] = scalar k, little-endian u32 limbs
// input.words[8]    = coordinate selector: 0=X, 1=Y, 2=Z
// ============================================================================

fn scalar_from_input() -> U256 {
    var scalar: U256;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        scalar[i] =
            input.words[i];
    }

    return scalar;
}


fn generator_table_point(
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    // Each table entry is 16 u32 words:
    // X[0..7] then Y[0..7], both little-endian limb order.
    let base =
        (
            windowIndex *
            16u +
            digit
        ) *
        16u;

    var p: JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] =
            generatorTable[
                base + i
            ];

        p.y[i] =
            generatorTable[
                base + 8u + i
            ];
    }

    p.z =
        u256_one();

    return p;
}


fn scalar_nibble(
    scalar: U256,
    windowIndex: u32
) -> u32 {
    let limb =
        windowIndex /
        8u;

    let shift =
        (
            windowIndex %
            8u
        ) *
        4u;

    return (
        scalar[limb] >>
        shift
    ) & 0x0fu;
}


fn scalar_mul_full_g(
    scalar: U256
) -> JacobianPoint {
    // Fixed-base nibble decomposition:
    //
    // k = sum(d_w * 16^w), w=0..63
    //
    // generatorTable[w][d] already stores d * 16^w * G, so runtime scalar
    // multiplication requires no scalar-loop point doublings — only one mixed
    // addition for each non-zero nibble.
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < 64u;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            scalar_nibble(
                scalar,
                windowIndex
            );

        if (digit != 0u) {
            let q =
                generator_table_point(
                    windowIndex,
                    digit
                );

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


@compute
@workgroup_size(1)
fn secp256k1_full_scalar_mul_g() {
    let scalar =
        scalar_from_input();

    let coordinate =
        input.words[8];

    let point =
        scalar_mul_full_g(
            scalar
        );

    if (coordinate == 0u) {
        write_scalar_coordinate(
            point.x
        );
        return;
    }

    if (coordinate == 1u) {
        write_scalar_coordinate(
            point.y
        );
        return;
    }

    if (coordinate == 2u) {
        write_scalar_coordinate(
            point.z
        );
        return;
    }

    write_scalar_coordinate(
        u256_zero()
    );
}


// ============================================================================
// Milestone 6 — BCH RFC6979 / HMAC-SHA256.
//
// Exact deterministic vector:
// private key = 1
// msg hash = 098d398f...0457f
// algorithm tag = ASCII "Schnorr+SHA256  "
//
// This diagnostic uses fixed maximum byte arrays represented as u32 values
// 0..255. It is correctness-first, not performance-oriented.
// ============================================================================

alias Bytes32 = array<u32, 32>;
alias Bytes128 = array<u32, 128>;
alias Bytes256 = array<u32, 256>;


fn bytes32_fill(
    value: u32
) -> Bytes32 {
    var out: Bytes32;

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        out[i] =
            value & 0xffu;
    }

    return out;
}


fn sha256_bytes256(
    data: Bytes256,
    byteLength: u32
) -> array<u32, 8> {
    var state =
        initializeState();

    let totalBlocks =
        (
            byteLength +
            9u +
            63u
        ) / 64u;

    let bitLengthLow =
        byteLength * 8u;

    // All messages in this milestone are far below 2^29 bytes,
    // so the high 32 bits of bit length are zero.
    let bitLengthHigh =
        0u;

    for (
        var blockIndex: u32 = 0u;
        blockIndex < totalBlocks;
        blockIndex = blockIndex + 1u
    ) {
        var blockWords:
            array<u32, 16>;

        for (
            var wordInBlock: u32 = 0u;
            wordInBlock < 16u;
            wordInBlock = wordInBlock + 1u
        ) {
            let globalWord =
                blockIndex * 16u +
                wordInBlock;

            let byteStart =
                globalWord * 4u;

            var word =
                0u;

            for (
                var j: u32 = 0u;
                j < 4u;
                j = j + 1u
            ) {
                let byteIndex =
                    byteStart + j;

                var b =
                    0u;

                if (byteIndex < byteLength) {
                    b =
                        data[byteIndex] &
                        0xffu;

                } else if (byteIndex == byteLength) {
                    b =
                        0x80u;

                } else {
                    let totalBytes =
                        totalBlocks * 64u;

                    if (
                        byteIndex >= totalBytes - 8u
                    ) {
                        let lengthOffset =
                            byteIndex -
                            (totalBytes - 8u);

                        if (lengthOffset < 4u) {
                            b =
                                (
                                    bitLengthHigh >>
                                    (
                                        (3u - lengthOffset) *
                                        8u
                                    )
                                ) & 0xffu;

                        } else {
                            let lowOffset =
                                lengthOffset - 4u;

                            b =
                                (
                                    bitLengthLow >>
                                    (
                                        (3u - lowOffset) *
                                        8u
                                    )
                                ) & 0xffu;
                        }
                    }
                }

                word =
                    word |
                    (
                        b <<
                        (
                            (3u - j) *
                            8u
                        )
                    );
            }

            blockWords[wordInBlock] =
                word;
        }

        state =
            compressBlock(
                state,
                blockWords
            );
    }

    return state;
}


fn digest_words_to_bytes(
    digest: array<u32, 8>
) -> Bytes32 {
    var out: Bytes32;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        let word =
            digest[i];

        out[i * 4u + 0u] =
            (word >> 24u) &
            0xffu;

        out[i * 4u + 1u] =
            (word >> 16u) &
            0xffu;

        out[i * 4u + 2u] =
            (word >> 8u) &
            0xffu;

        out[i * 4u + 3u] =
            word &
            0xffu;
    }

    return out;
}


fn hmac_sha256_fixed(
    key: Bytes32,
    message: Bytes128,
    messageLength: u32
) -> Bytes32 {
    var inner:
        Bytes256;

    var outer:
        Bytes256;

    for (
        var i: u32 = 0u;
        i < 256u;
        i = i + 1u
    ) {
        inner[i] =
            0u;

        outer[i] =
            0u;
    }

    // HMAC block size is 64 bytes. Key is exactly 32 bytes.
    for (
        var i: u32 = 0u;
        i < 64u;
        i = i + 1u
    ) {
        let keyByte =
            select(
                0u,
                key[i] & 0xffu,
                i < 32u
            );

        inner[i] =
            keyByte ^
            0x36u;

        outer[i] =
            keyByte ^
            0x5cu;
    }

    for (
        var i: u32 = 0u;
        i < messageLength;
        i = i + 1u
    ) {
        inner[64u + i] =
            message[i] &
            0xffu;
    }

    let innerDigestWords =
        sha256_bytes256(
            inner,
            64u + messageLength
        );

    let innerDigest =
        digest_words_to_bytes(
            innerDigestWords
        );

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        outer[64u + i] =
            innerDigest[i];
    }

    let outerDigestWords =
        sha256_bytes256(
            outer,
            96u
        );

    return digest_words_to_bytes(
        outerDigestWords
    );
}


fn rfc_private_key_one() -> Bytes32 {
    var x =
        bytes32_fill(
            0u
        );

    x[31] =
        1u;

    return x;
}


fn rfc_message_hash() -> Bytes32 {
    // 098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f
    var h: Bytes32;

    h[0]=0x09u; h[1]=0x8du; h[2]=0x39u; h[3]=0x8fu;
    h[4]=0xfeu; h[5]=0xb4u; h[6]=0x39u; h[7]=0x10u;
    h[8]=0x01u; h[9]=0x2du; h[10]=0xb4u; h[11]=0x26u;
    h[12]=0xebu; h[13]=0x01u; h[14]=0x27u; h[15]=0x95u;
    h[16]=0x63u; h[17]=0xbeu; h[18]=0xafu; h[19]=0x5eu;
    h[20]=0x07u; h[21]=0x0au; h[22]=0xbau; h[23]=0xe7u;
    h[24]=0x7au; h[25]=0xfau; h[26]=0xcfu; h[27]=0x31u;
    h[28]=0x20u; h[29]=0x30u; h[30]=0x45u; h[31]=0x7fu;

    return h;
}


fn write_bytes32_as_words(
    value: Bytes32
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            (
                (value[i * 4u + 0u] << 24u) |
                (value[i * 4u + 1u] << 16u) |
                (value[i * 4u + 2u] << 8u) |
                value[i * 4u + 3u]
            );
    }

    for (
        var i: u32 = 8u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn bch_rfc6979_diagnostic() {
    let selector =
        input.words[0];

    let x =
        rfc_private_key_one();

    // For this vector, message hash < curve order n, so scalar reduction
    // leaves it unchanged. This matches scalar_from_bytes + scalar_to_bytes.
    let msgmod =
        rfc_message_hash();

    var V =
        bytes32_fill(
            0x01u
        );

    var K =
        bytes32_fill(
            0x00u
        );

    // Step D:
    // K = HMAC(K, V || 0x00 || x || msgmod || algo16)
    var message113:
        Bytes128;

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];
    }

    message113[32] =
        0x00u;

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[33u + i] =
            x[i];

        message113[65u + i] =
            msgmod[i];
    }

    // ASCII: Schnorr+SHA256[space][space]
    let algo:
        array<u32, 16> =
        array<u32, 16>(
            0x53u,0x63u,0x68u,0x6eu,
            0x6fu,0x72u,0x72u,0x2bu,
            0x53u,0x48u,0x41u,0x32u,
            0x35u,0x36u,0x20u,0x20u
        );

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[97u + i] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    let K_after_D =
        K;

    // V = HMAC(K, V)
    var message32:
        Bytes128;

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    let V_after_D =
        V;

    // Step F:
    // K = HMAC(K, V || 0x01 || x || msgmod || algo16)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];
    }

    message113[32] =
        0x01u;

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[33u + i] =
            x[i];

        message113[65u + i] =
            msgmod[i];
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[97u + i] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    let K_after_F =
        K;

    // V = HMAC(K, V)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    let V_after_F =
        V;

    // First candidate:
    // V = HMAC(K, V)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    let candidate =
        V;

    if (selector == 0u) {
        write_bytes32_as_words(
            K_after_D
        );
        return;
    }

    if (selector == 1u) {
        write_bytes32_as_words(
            V_after_D
        );
        return;
    }

    if (selector == 2u) {
        write_bytes32_as_words(
            K_after_F
        );
        return;
    }

    if (selector == 3u) {
        write_bytes32_as_words(
            V_after_F
        );
        return;
    }

    if (selector == 4u) {
        write_bytes32_as_words(
            candidate
        );
        return;
    }

    write_bytes32_as_words(
        bytes32_fill(
            0u
        )
    );
}


// ============================================================================
// Milestone 7 — actual RFC6979 nonce point, Jacobian -> affine.
//
// Uses binary extended inversion rather than Fermat exponentiation.
// input.words[0..7] = k, little-endian u32 limbs
// input.words[8] = 0 for affine X, 1 for affine Y
// ============================================================================

fn u256_is_one(
    a: U256
) -> bool {
    if (a[0] != 1u) {
        return false;
    }

    for (
        var i: u32 = 1u;
        i < 8u;
        i = i + 1u
    ) {
        if (a[i] != 0u) {
            return false;
        }
    }

    return true;
}


fn u256_is_even(
    a: U256
) -> bool {
    return (
        a[0] &
        1u
    ) == 0u;
}


fn u256_shift_right_one(
    a: U256
) -> U256 {
    var r: U256;

    for (
        var i: u32 = 0u;
        i < 7u;
        i = i + 1u
    ) {
        r[i] =
            (a[i] >> 1u) |
            ((a[i + 1u] & 1u) << 31u);
    }

    r[7] =
        a[7] >> 1u;

    return r;
}


fn u256_shift_right_one_with_high(
    a: U256,
    highBit: u32
) -> U256 {
    var r =
        u256_shift_right_one(
            a
        );

    if (highBit != 0u) {
        r[7] =
            r[7] |
            0x80000000u;
    }

    return r;
}


fn field_half(
    a: U256
) -> U256 {
    if (u256_is_even(a)) {
        return u256_shift_right_one(
            a
        );
    }

    let sum =
        add_with_carry(
            a,
            field_p()
        );

    return u256_shift_right_one_with_high(
        sum[0],
        sum[1][0]
    );
}


fn field_inv_binary(
    a: U256
) -> U256 {
    var u =
        a;

    var v =
        field_p();

    var x1 =
        u256_one();

    var x2 =
        u256_zero();

    // Binary extended GCD. Inputs are non-zero field elements.
    loop {
        if (u256_is_one(u)) {
            return x1;
        }

        if (u256_is_one(v)) {
            return x2;
        }

        loop {
            if (!u256_is_even(u)) {
                break;
            }

            u =
                u256_shift_right_one(
                    u
                );

            x1 =
                field_half(
                    x1
                );
        }

        loop {
            if (!u256_is_even(v)) {
                break;
            }

            v =
                u256_shift_right_one(
                    v
                );

            x2 =
                field_half(
                    x2
                );
        }

        if (u256_ge(u, v)) {
            u =
                u256_sub_raw(
                    u,
                    v
                );

            x1 =
                field_sub(
                    x1,
                    x2
                );

        } else {
            v =
                u256_sub_raw(
                    v,
                    u
                );

            x2 =
                field_sub(
                    x2,
                    x1
                );
        }
    }
}


@compute
@workgroup_size(1)
fn bch_schnorr_nonce_affine() {
    var scalar: U256;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        scalar[i] =
            input.words[i];
    }

    let selector =
        input.words[8];

    let point =
        scalar_mul_full_g(
            scalar
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let zInv3 =
        field_mul(
            zInv2,
            zInv
        );

    let affineX =
        field_mul(
            point.x,
            zInv2
        );

    let affineY =
        field_mul(
            point.y,
            zInv3
        );

    if (selector == 0u) {
        write_scalar_coordinate(
            affineX
        );
        return;
    }

    if (selector == 1u) {
        write_scalar_coordinate(
            affineY
        );
        return;
    }

    write_scalar_coordinate(
        u256_zero()
    );
}


// ============================================================================
// Milestone 8 — complete deterministic BCH Schnorr signature.
//
// Fixed deterministic PHOTON vector:
//   private key d = 1
//   original RFC6979 k known from M6
//   affine R known from M7
//
// Selectors:
//   0 = QR flag for R.y (1=residue, 0=non-residue)
//   1 = adjusted k (n-k because this vector is non-residue)
//   2 = challenge SHA256(R.x || compressed pubkey || msg)
//   3 = s = adjusted_k + e*d mod n
// ============================================================================

fn scalar_order_n() -> U256 {
    // n =
    // FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFE
    // BAAEDCE6AF48A03BBFD25E8CD0364141
    return U256(
        0xd0364141u,
        0xbfd25e8cu,
        0xaf48a03bu,
        0xbaaedce6u,
        0xfffffffeu,
        0xffffffffu,
        0xffffffffu,
        0xffffffffu
    );
}


fn u256_from_be_bytes32(
    bytes: Bytes32
) -> U256 {
    var out: U256;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        let byteBase =
            32u -
            (limb + 1u) * 4u;

        out[limb] =
            (
                (bytes[byteBase + 0u] << 24u) |
                (bytes[byteBase + 1u] << 16u) |
                (bytes[byteBase + 2u] << 8u) |
                bytes[byteBase + 3u]
            );
    }

    return out;
}


fn u256_to_be_bytes32(
    value: U256
) -> Bytes32 {
    var out: Bytes32;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        let word =
            value[7u - limb];

        out[limb * 4u + 0u] =
            (word >> 24u) & 0xffu;

        out[limb * 4u + 1u] =
            (word >> 16u) & 0xffu;

        out[limb * 4u + 2u] =
            (word >> 8u) & 0xffu;

        out[limb * 4u + 3u] =
            word & 0xffu;
    }

    return out;
}


fn scalar_sub_n(
    a: U256,
    b: U256
) -> U256 {
    // Inputs satisfy a >= b.
    return u256_sub_raw(
        a,
        b
    );
}


fn scalar_add_mod_n(
    a: U256,
    b: U256
) -> U256 {
    let n =
        scalar_order_n();

    // Avoid 256-bit overflow:
    // if a >= n-b, return a-(n-b), else a+b.
    let nMinusB =
        u256_sub_raw(
            n,
            b
        );

    if (u256_ge(a, nMinusB)) {
        return u256_sub_raw(
            a,
            nMinusB
        );
    }

    let sum =
        add_with_carry(
            a,
            b
        );

    return sum[0];
}


fn scalar_mul_mod_n(
    a: U256,
    b: U256
) -> U256 {
    var result =
        u256_zero();

    var addend =
        a;

    for (
        var bit: u32 = 0u;
        bit < 256u;
        bit = bit + 1u
    ) {
        if (
            u256_bit(
                b,
                bit
            ) == 1u
        ) {
            result =
                scalar_add_mod_n(
                    result,
                    addend
                );
        }

        addend =
            scalar_add_mod_n(
                addend,
                addend
            );
    }

    return result;
}


fn fixed_r_bytes() -> Bytes32 {
    var r: Bytes32;

    let words =
        array<u32, 8>(
            0x5b73543bu,
            0x21b74bd4u,
            0x7b0dfc45u,
            0x65780e4eu,
            0xd2f0e5c4u,
            0xbb85f2c6u,
            0xdd354672u,
            0x7f84604fu
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        r[i * 4u + 0u] =
            (words[i] >> 24u) & 0xffu;

        r[i * 4u + 1u] =
            (words[i] >> 16u) & 0xffu;

        r[i * 4u + 2u] =
            (words[i] >> 8u) & 0xffu;

        r[i * 4u + 3u] =
            words[i] & 0xffu;
    }

    return r;
}


fn fixed_ry() -> U256 {
    // 4b574c015e8a3149605cb27b69b73a939e1251b10524eed4631acc27c6aa9347
    return U256(
        0xc6aa9347u,
        0x631acc27u,
        0x0524eed4u,
        0x9e1251b1u,
        0x69b73a93u,
        0x605cb27bu,
        0x5e8a3149u,
        0x4b574c01u
    );
}


fn fixed_original_k() -> U256 {
    return U256(
        0x7fd7797au,
        0x21628121u,
        0x4311ac7du,
        0xcd4d0f61u,
        0x1ce49b17u,
        0x10a72a39u,
        0x00fbb1aeu,
        0x615da2b7u
    );
}


fn field_pow(
    baseValue: U256,
    exponent: U256
) -> U256 {
    var result =
        u256_one();

    var base =
        baseValue;

    for (
        var bit: u32 = 0u;
        bit < 256u;
        bit = bit + 1u
    ) {
        if (
            u256_bit(
                exponent,
                bit
            ) == 1u
        ) {
            result =
                field_mul(
                    result,
                    base
                );
        }

        base =
            field_mul(
                base,
                base
            );
    }

    return result;
}



fn field_legendre_binary(
    inputValue: U256
) -> i32 {
    var a =
        normalize_field(
            inputValue
        );

    if (u256_is_zero(a)) {
        return 0;
    }

    var n =
        field_p();

    var sign:
        i32 =
        1;

    for (
        var round: u32 = 0u;
        round < 2048u;
        round = round + 1u
    ) {
        if (u256_is_zero(a)) {
            if (u256_is_one(n)) {
                return sign;
            }

            return 0;
        }

        for (
            var shiftRound: u32 = 0u;
            shiftRound < 256u;
            shiftRound = shiftRound + 1u
        ) {
            if (!u256_is_even(a)) {
                break;
            }

            a =
                u256_shift_right_one(
                    a
                );

            let nMod8 =
                n[0] &
                7u;

            if (
                nMod8 == 3u ||
                nMod8 == 5u
            ) {
                sign =
                    -sign;
            }

            if (u256_is_zero(a)) {
                break;
            }
        }

        if (u256_is_zero(a)) {
            if (u256_is_one(n)) {
                return sign;
            }

            return 0;
        }

        if (!u256_ge(a, n)) {
            let oldA =
                a;

            a =
                n;

            n =
                oldA;

            if (
                (a[0] & 3u) == 3u &&
                (n[0] & 3u) == 3u
            ) {
                sign =
                    -sign;
            }
        }

        a =
            u256_sub_raw(
                a,
                n
            );
    }

    return 0;
}


fn bch_jacobian_y_is_qr(
    point: JacobianPoint
) -> bool {
    let ySymbol =
        field_legendre_binary(
            point.y
        );

    let zSymbol =
        field_legendre_binary(
            point.z
        );

    return (
        ySymbol != 0 &&
        zSymbol != 0 &&
        ySymbol == zSymbol
    );
}


fn bch_y_is_qr(
    y: U256
) -> bool {
    // CUDA checks whether field_sqrt(y)^2 == y.
    // For secp256k1 p ≡ 3 mod 4, sqrt candidate is y^((p+1)/4).
    let sqrtExponent =
        U256(
            0xbfffff0cu,
            0xffffffffu,
            0xffffffffu,
            0xffffffffu,
            0xffffffffu,
            0xffffffffu,
            0xffffffffu,
            0x3fffffffu
        );

    let root =
        field_pow(
            y,
            sqrtExponent
        );

    let check =
        field_mul(
            root,
            root
        );

    return u256_equal(
        normalize_field(check),
        normalize_field(y)
    );
}


fn bch_challenge_hash() -> Bytes32 {
    var data:
        Bytes256;

    for (
        var i: u32 = 0u;
        i < 256u;
        i = i + 1u
    ) {
        data[i] =
            0u;
    }

    let r =
        fixed_r_bytes();

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        data[i] =
            r[i];
    }

    // Compressed public key for d=1:
    // 02 || G.x
    data[32] =
        0x02u;

    let gxBytes =
        u256_to_be_bytes32(
            secp_gx()
        );

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        data[33u + i] =
            gxBytes[i];
    }

    let msg =
        rfc_message_hash();

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        data[65u + i] =
            msg[i];
    }

    let digest =
        sha256_bytes256(
            data,
            97u
        );

    return digest_words_to_bytes(
        digest
    );
}


fn write_u256_be(
    value: U256
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            value[7u - i];
    }

    for (
        var i: u32 = 8u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn bch_full_schnorr_diagnostic() {
    let selector =
        input.words[0];

    let qr =
        bch_y_is_qr(
            fixed_ry()
        );

    let originalK =
        fixed_original_k();

    let n =
        scalar_order_n();

    var adjustedK =
        originalK;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                n,
                originalK
            );
    }

    let challenge =
        bch_challenge_hash();

    let e =
        u256_from_be_bytes32(
            challenge
        );

    // d = 1, but deliberately exercise scalar_mul_mod_n.
    let d =
        u256_one();

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    if (selector == 0u) {
        var flag =
            u256_zero();

        if (qr) {
            flag[0] =
                1u;
        }

        write_u256_be(
            flag
        );
        return;
    }

    if (selector == 1u) {
        write_u256_be(
            adjustedK
        );
        return;
    }

    if (selector == 2u) {
        write_bytes32_as_words(
            challenge
        );
        return;
    }

    if (selector == 3u) {
        write_u256_be(
            s
        );
        return;
    }

    write_u256_be(
        u256_zero()
    );
}


// ============================================================================
// Milestone 9 — complete deterministic PHOTON candidate.
//
// input.words[0..153] = unsigned 615-byte transaction template, packed BE.
// input.byteLength     = candidate nonce (u32) for this entry point.
// low byte of input.words[153] is outside the 615-byte transaction and is used
// as selector: 0=signature, 1=transaction SHA256/HASH256.
//
// Deterministic fixture locations:
//   baton start = 390
//   nonce       = bytes 390..393
//   target      = bytes 394..425
//   signature   = bytes 426..489
// ============================================================================

fn m9_input_byte(
    byteIndex: u32
) -> u32 {
    let word =
        input.words[
            byteIndex / 4u
        ];

    let within =
        byteIndex % 4u;

    return (
        word >>
        (
            (3u - within) *
            8u
        )
    ) & 0xffu;
}


fn m9_message_hash(
    nonce: u32
) -> Bytes32 {
    var data:
        Bytes256;

    for (
        var i: u32 = 0u;
        i < 256u;
        i = i + 1u
    ) {
        data[i] =
            0u;
    }

    // nonce uint32 little-endian
    data[0] =
        nonce & 0xffu;

    data[1] =
        (nonce >> 8u) & 0xffu;

    data[2] =
        (nonce >> 16u) & 0xffu;

    data[3] =
        (nonce >> 24u) & 0xffu;

    // target bytes are copied directly from the unsigned transaction template.
    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        data[4u + i] =
            m9_input_byte(
                394u + i
            );
    }

    let digest =
        sha256_bytes256(
            data,
            36u
        );

    return digest_words_to_bytes(
        digest
    );
}


fn m9_rfc6979_nonce(
    msgmod: Bytes32
) -> Bytes32 {
    let x =
        rfc_private_key_one();

    var V =
        bytes32_fill(
            0x01u
        );

    var K =
        bytes32_fill(
            0x00u
        );

    var message113:
        Bytes128;

    var message32:
        Bytes128;

    let algo:
        array<u32, 16> =
        array<u32, 16>(
            0x53u,0x63u,0x68u,0x6eu,
            0x6fu,0x72u,0x72u,0x2bu,
            0x53u,0x48u,0x41u,0x32u,
            0x35u,0x36u,0x20u,0x20u
        );

    // D: K = HMAC(K, V || 00 || x || h1 || algo)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[33u + i] =
            x[i];

        message113[65u + i] =
            msgmod[i];
    }

    message113[32] =
        0u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[97u + i] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    // E: V = HMAC(K,V)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    // F: K = HMAC(K, V || 01 || x || h1 || algo)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[33u + i] =
            x[i];

        message113[65u + i] =
            msgmod[i];
    }

    message113[32] =
        1u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[97u + i] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    // G: V = HMAC(K,V)
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    // H: first candidate. This deterministic fixture's first candidate is
    // nonzero and strictly less than n (already proven in Milestone 6).
    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    return V;
}


fn m9_scalar_reduce_n(
    value: U256
) -> U256 {
    let n =
        scalar_order_n();

    // Any 256-bit value is < 2^256, and secp256k1 n is close enough to
    // 2^256 that at most one subtraction is required.
    if (u256_ge(value, n)) {
        return u256_sub_raw(
            value,
            n
        );
    }

    return value;
}


fn m9_challenge_hash(
    rBytes: Bytes32,
    messageHash: Bytes32
) -> Bytes32 {
    var data:
        Bytes256;

    for (
        var i: u32 = 0u;
        i < 256u;
        i = i + 1u
    ) {
        data[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        data[i] =
            rBytes[i];
    }

    // Pull the exact compressed public key from transaction bytes 45..77.
    for (
        var i: u32 = 0u;
        i < 33u;
        i = i + 1u
    ) {
        data[32u + i] =
            m9_input_byte(
                45u + i
            );
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        data[65u + i] =
            messageHash[i];
    }

    let digest =
        sha256_bytes256(
            data,
            97u
        );

    return digest_words_to_bytes(
        digest
    );
}


fn m9_completed_tx_byte(
    byteIndex: u32,
    nonce: u32,
    rBytes: Bytes32,
    sBytes: Bytes32
) -> u32 {
    // Nonce at bytes 390..393, little-endian.
    if (
        byteIndex >= 390u &&
        byteIndex < 394u
    ) {
        let shift =
            (
                byteIndex -
                390u
            ) * 8u;

        return (
            nonce >>
            shift
        ) & 0xffu;
    }

    // Signature r at 426..457.
    if (
        byteIndex >= 426u &&
        byteIndex < 458u
    ) {
        return rBytes[
            byteIndex -
            426u
        ];
    }

    // Signature s at 458..489.
    if (
        byteIndex >= 458u &&
        byteIndex < 490u
    ) {
        return sBytes[
            byteIndex -
            458u
        ];
    }

    return m9_input_byte(
        byteIndex
    );
}


fn m9_transaction_hashes(
    nonce: u32,
    rBytes: Bytes32,
    sBytes: Bytes32
) -> array<array<u32, 8>, 2> {
    let byteLength =
        615u;

    let totalBlocks =
        10u;

    var state =
        initializeState();

    for (
        var blockIndex: u32 = 0u;
        blockIndex < totalBlocks;
        blockIndex = blockIndex + 1u
    ) {
        var block:
            array<u32, 16>;

        for (
            var wordInBlock: u32 = 0u;
            wordInBlock < 16u;
            wordInBlock = wordInBlock + 1u
        ) {
            let globalWord =
                blockIndex * 16u +
                wordInBlock;

            let byteStart =
                globalWord * 4u;

            var word =
                0u;

            for (
                var j: u32 = 0u;
                j < 4u;
                j = j + 1u
            ) {
                let byteIndex =
                    byteStart +
                    j;

                var b =
                    0u;

                if (byteIndex < byteLength) {
                    b =
                        m9_completed_tx_byte(
                            byteIndex,
                            nonce,
                            rBytes,
                            sBytes
                        );

                } else if (byteIndex == byteLength) {
                    b =
                        0x80u;

                } else if (
                    byteIndex >=
                    totalBlocks * 64u -
                    8u
                ) {
                    let lengthOffset =
                        byteIndex -
                        (
                            totalBlocks *
                            64u -
                            8u
                        );

                    if (lengthOffset >= 4u) {
                        let lowOffset =
                            lengthOffset -
                            4u;

                        let bitLength =
                            byteLength *
                            8u;

                        b =
                            (
                                bitLength >>
                                (
                                    (3u - lowOffset) *
                                    8u
                                )
                            ) & 0xffu;
                    }
                }

                word =
                    word |
                    (
                        b <<
                        (
                            (3u - j) *
                            8u
                        )
                    );
            }

            block[wordInBlock] =
                word;
        }

        state =
            compressBlock(
                state,
                block
            );
    }

    var secondBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        secondBlock[i] =
            state[i];
    }

    secondBlock[8] =
        0x80000000u;

    for (
        var i: u32 = 9u;
        i < 15u;
        i = i + 1u
    ) {
        secondBlock[i] =
            0u;
    }

    secondBlock[15] =
        256u;

    let finalDigest =
        compressBlock(
            initializeState(),
            secondBlock
        );

    return array<array<u32, 8>, 2>(
        state,
        finalDigest
    );
}


@compute
@workgroup_size(1)
fn photon_integrated_candidate() {
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    // 1) SHA256(nonceLE || target)
    let messageHash =
        m9_message_hash(
            nonce
        );

    // 2) BCH RFC6979 nonce
    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    // 3) R = kG, then Jacobian -> affine
    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    // 4) BCH QR rule directly from Jacobian Y,Z.
    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    // 5) e = SHA256(r || compressed_pubkey || message_hash) mod n
    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    // Fixture private key d=1.
    let ed =
        scalar_mul_mod_n(
            e,
            u256_one()
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        // Signature: r in words 0..7, s in words 8..15.
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        // Build completed transaction and return both hashes.
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


// ============================================================================
// Milestone 10 — parallel full-PHOTON benchmark.
//
// Uses the complete M9 candidate calculation for every invocation.
// binding 0 = SharedInput (unsigned deterministic transaction template)
// binding 2 = atomic benchmark counters
//
// Workgroup size 32. JS dispatches enough workgroups for the requested batch.
// ============================================================================

struct BenchmarkOutput {
    checksum: atomic<u32>,
    completed: atomic<u32>,
    winners: atomic<u32>,
    firstWinnerNoncePlusOne: atomic<u32>,
};

// M50.4/M53 portability fix:
// atomicCompareExchangeWeak may fail spuriously. Winner reporting needs any
// winner, not specifically the first, so use atomicExchange instead.

@group(0)
@binding(2)
var<storage, read_write>
benchmarkOutput:
    BenchmarkOutput;


fn m10_hash_is_below_target(
    hashWords: array<u32, 8>
) -> bool {
    // hashWords are SHA-256 digest words in byte order. PHOTON interprets the
    // 32 hash bytes and target bytes as little-endian integers, so compare from
    // byte 31 toward byte 0.
    for (
        var byteRev: i32 = 31;
        byteRev >= 0;
        byteRev = byteRev - 1
    ) {
        let byteIndex =
            u32(byteRev);

        let hashWord =
            hashWords[
                byteIndex /
                4u
            ];

        let hashWithin =
            byteIndex %
            4u;

        let hashByte =
            (
                hashWord >>
                (
                    (3u - hashWithin) *
                    8u
                )
            ) & 0xffu;

        let targetByte =
            m9_input_byte(
                394u +
                byteIndex
            );

        if (
            hashByte <
            targetByte
        ) {
            return true;
        }

        if (
            hashByte >
            targetByte
        ) {
            return false;
        }
    }

    return false;
}


@compute
@workgroup_size(32)
fn photon_parallel_benchmark(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let candidateIndex =
        gid.x;

    // JS always dispatches exact multiples of 32 in M10.
    let nonce =
        input.byteLength +
        candidateIndex;

    // 1) SHA256(nonceLE || target)
    let messageHash =
        m9_message_hash(
            nonce
        );

    // 2) RFC6979
    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    // 3) R = kG and affine conversion
    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    // 4) BCH QR rule directly from Jacobian Y,Z.
    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    // 5) Challenge + s
    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            scalar_mul_mod_n(
                e,
                u256_one()
            )
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    // 6) Complete transaction HASH256
    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    // Atomics ensure every invocation has an observable side-effect, so the
    // full candidate path cannot be optimized away.
    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    if (
        m10_hash_is_below_target(
            finalHash
        )
    ) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


// ============================================================================
// Milestone 14 — cumulative PHOTON stage profiler.
//
// All profiler kernels use 32-thread workgroups and write an atomic checksum so
// their computed values remain observable.
// ============================================================================

fn m14_nonce(
    gid: vec3<u32>
) -> u32 {
    return input.byteLength + gid.x;
}


fn m14_signature_components(
    nonce: u32
) -> array<U256, 3> {
    // Returns [rx, adjustedK, challengeE].
    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    return array<U256, 3>(
        rx,
        adjustedK,
        e
    );
}


@compute
@workgroup_size(32)
fn photon_profile_a_hash_rfc6979(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        m14_nonce(
            gid
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        k[0] ^
        k[3] ^
        k[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(32)
fn photon_profile_b_scalar_mul(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        m14_nonce(
            gid
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(32)
fn photon_profile_c_affine_rx(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        m14_nonce(
            gid
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        rx[0] ^
        rx[3] ^
        rx[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(32)
fn photon_profile_d_signature_scalar(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        m14_nonce(
            gid
        );

    let parts =
        m14_signature_components(
            nonce
        );

    let s =
        scalar_add_mod_n(
            parts[1],
            scalar_mul_mod_n(
                parts[2],
                u256_one()
            )
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        s[0] ^
        s[3] ^
        s[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(32)
fn photon_profile_e_tx_hash(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        m14_nonce(
            gid
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            scalar_mul_mod_n(
                e,
                u256_one()
            )
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}



// ============================================================================
// Milestone 15 — single-pipeline cumulative PHOTON profiler.
//
// Runtime selector lives in byte 615: low byte of input.words[153].
// All selectors share this one entry point and therefore the same pipeline.
// ============================================================================

@compute
@workgroup_size(32)
fn photon_single_pipeline_profiler(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    // ------------------------------------------------------------------------
    // A: signing-message SHA256 + RFC6979
    // ------------------------------------------------------------------------
    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stageSelector == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // ------------------------------------------------------------------------
    // B: fixed-base kG
    // ------------------------------------------------------------------------
    let point =
        scalar_mul_full_g(
            k
        );

    if (stageSelector == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // ------------------------------------------------------------------------
    // C: affine Rx including field inversion
    // ------------------------------------------------------------------------
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stageSelector == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // ------------------------------------------------------------------------
    // D: BCH QR + challenge + scalar s
    // ------------------------------------------------------------------------
    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            scalar_mul_mod_n(
                e,
                u256_one()
            )
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stageSelector == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // ------------------------------------------------------------------------
    // E: completed transaction HASH256
    // ------------------------------------------------------------------------
    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    if (stageSelector == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            finalHash[0] ^
            finalHash[3] ^
            finalHash[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // ------------------------------------------------------------------------
    // F: full candidate including target comparison / winner bookkeeping.
    // ------------------------------------------------------------------------
    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    if (
        m10_hash_is_below_target(
            finalHash
        )
    ) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 16 — sustained full-PHOTON nonce search.
//
// input.byteLength = base nonce.
// low byte of input.words[153] bit 0 = easy winner validation mode.
//
// Easy mode does not alter the transaction bytes or candidate hash. It only
// replaces the final target predicate with "hash is nonzero" so the atomic
// winner-return/readback path can be validated deterministically.
// ============================================================================

fn m16_hash_is_nonzero(
    hashWords: array<u32, 8>
) -> bool {
    var accumulator:
        u32 =
        0u;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        accumulator =
            accumulator |
            hashWords[i];
    }

    return accumulator != 0u;
}


@compute
@workgroup_size(32)
fn photon_sustained_search(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    // 1) SHA256(nonceLE || target)
    let messageHash =
        m9_message_hash(
            nonce
        );

    // 2) BCH RFC6979
    let kBytes =
        m9_rfc6979_nonce(
            messageHash
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    // 3) Fixed-base R = kG
    let point =
        scalar_mul_full_g(
            k
        );

    // 4) affine R.x only
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    // 5) BCH QR + challenge + scalar s
    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            scalar_mul_mod_n(
                e,
                u256_one()
            )
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    // 6) completed transaction HASH256
    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 17 — arbitrary private-key PHOTON candidate.
// ============================================================================

fn m17_private_key_bytes() -> Bytes32 {
    var result:
        Bytes32;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        let word =
            privateKeyInput.words[i];

        result[i * 4u] =
            (word >> 24u) &
            0xffu;

        result[i * 4u + 1u] =
            (word >> 16u) &
            0xffu;

        result[i * 4u + 2u] =
            (word >> 8u) &
            0xffu;

        result[i * 4u + 3u] =
            word &
            0xffu;
    }

    return result;
}


fn m17_rfc6979_nonce(
    msgmod: Bytes32,
    x: Bytes32
) -> Bytes32 {
    var V =
        bytes32_fill(
            0x01u
        );

    var K =
        bytes32_fill(
            0x00u
        );

    var message113:
        Bytes128;

    var message32:
        Bytes128;

    let algo:
        array<u32, 16> =
        array<u32, 16>(
            0x53u,0x63u,0x68u,0x6eu,
            0x6fu,0x72u,0x72u,0x2bu,
            0x53u,0x48u,0x41u,0x32u,
            0x35u,0x36u,0x20u,0x20u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[33u + i] =
            x[i];

        message113[65u + i] =
            msgmod[i];
    }

    message113[32] =
        0u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[97u + i] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[33u + i] =
            x[i];

        message113[65u + i] =
            msgmod[i];
    }

    message113[32] =
        1u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[97u + i] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    return V;
}


fn m17_derive_public_point(
    d: U256
) -> array<U256, 2> {
    let p =
        scalar_mul_full_g(
            d
        );

    let zInv =
        field_inv_binary(
            p.z
        );

    let z2 =
        field_mul(
            zInv,
            zInv
        );

    let z3 =
        field_mul(
            z2,
            zInv
        );

    let ax =
        field_mul(
            p.x,
            z2
        );

    let ay =
        field_mul(
            p.y,
            z3
        );

    return array<U256, 2>(
        ax,
        ay
    );
}


@compute
@workgroup_size(1)
fn photon_arbitrary_key_candidate() {
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}



// ============================================================================
// Milestone 18 — sustained search using arbitrary private key from binding 4.
// ============================================================================

@compute
@workgroup_size(32)
fn photon_arbitrary_key_sustained_search(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 23 — arbitrary-key cumulative performance profiler.
//
// Selector:
//   1 = message SHA256 + arbitrary-key RFC6979
//   2 = + fixed-base kG
//   3 = + affine R.x / field inversion
//   4 = + QR + challenge + e*d + scalar s
//   5 = + completed transaction HASH256
//
// All stages intentionally use the same arbitrary private-key path as the live
// miner. benchmarkOutput keeps the work observable.
// ============================================================================

@compute
@workgroup_size(32)
fn photon_m23_arbitrary_key_profiler(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    // A — signing-message SHA256 + arbitrary-key RFC6979
    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stageSelector == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // B — fixed-base kG
    let point =
        scalar_mul_full_g(
            k
        );

    if (stageSelector == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // C — affine R.x, including field inversion
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stageSelector == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // D — QR + challenge + arbitrary scalar e*d + s
    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stageSelector == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    // E — completed 615-byte transaction HASH256
    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


// ============================================================================
// Milestone 23 — full arbitrary-key search workgroup-size sweep.
// These intentionally duplicate the live compute path, changing only
// @workgroup_size.
// ============================================================================


@compute
@workgroup_size(32)
fn photon_m23_full_search_wg32(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(64)
fn photon_m23_full_search_wg64(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m23_full_search_wg128(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m23_full_search_wg256(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        scalar_mul_full_g(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 24 — fixed-base secp256k1 multiplication bake-off.
//
// Packed generator table byte offsets:
//   4-bit unsigned : 0
//   5-bit unsigned : 65536
//   6-bit unsigned : 172032
//   8-bit unsigned : 348160
//   width-6 wNAF   : 872448
//
// All variants are correctness-checked in affine coordinates before timing.
// ============================================================================

const M24_G4_BASE_WORDS: u32 = 0u;
const M24_G5_BASE_WORDS: u32 = 16384u;
const M24_G6_BASE_WORDS: u32 = 43008u;
const M24_G8_BASE_WORDS: u32 = 87040u;
const M24_WNAF6_BASE_WORDS: u32 = 218112u;


fn m24_table_point(
    baseWords: u32,
    entriesPerWindow: u32,
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    let base =
        baseWords +
        (
            windowIndex *
            entriesPerWindow +
            digit
        ) *
        16u;

    var p: JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] =
            generatorTable[
                base + i
            ];

        p.y[i] =
            generatorTable[
                base + 8u + i
            ];
    }

    p.z =
        u256_one();

    return p;
}


fn m24_scalar_window_bits(
    scalar: U256,
    bitOffset: u32,
    width: u32
) -> u32 {
    let limb =
        bitOffset /
        32u;

    let shift =
        bitOffset %
        32u;

    if (limb >= 8u) {
        return 0u;
    }

    var value =
        scalar[limb] >>
        shift;

    if (
        shift != 0u &&
        shift + width > 32u &&
        limb + 1u < 8u
    ) {
        value =
            value |
            (
                scalar[
                    limb + 1u
                ] <<
                (
                    32u -
                    shift
                )
            );
    }

    let mask =
        (
            1u <<
            width
        ) -
        1u;

    return value &
        mask;
}


fn m24_scalar_mul_unsigned_window(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32,
    baseWords: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < windows;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m24_scalar_window_bits(
                scalar,
                windowIndex *
                    width,
                width
            );

        if (digit != 0u) {
            let q =
                m24_table_point(
                    baseWords,
                    entries,
                    windowIndex,
                    digit
                );

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m24_scalar_mul_g4(
    scalar: U256
) -> JacobianPoint {
    return m24_scalar_mul_unsigned_window(
        scalar,
        4u,
        64u,
        16u,
        M24_G4_BASE_WORDS
    );
}


fn m24_scalar_mul_g5(
    scalar: U256
) -> JacobianPoint {
    return m24_scalar_mul_unsigned_window(
        scalar,
        5u,
        52u,
        32u,
        M24_G5_BASE_WORDS
    );
}


fn m24_scalar_mul_g6(
    scalar: U256
) -> JacobianPoint {
    return m24_scalar_mul_unsigned_window(
        scalar,
        6u,
        43u,
        64u,
        M24_G6_BASE_WORDS
    );
}


fn m24_scalar_mul_g8(
    scalar: U256
) -> JacobianPoint {
    return m24_scalar_mul_unsigned_window(
        scalar,
        8u,
        32u,
        256u,
        M24_G8_BASE_WORDS
    );
}


alias M24U288 =
    array<u32, 9>;


fn m24_u288_from_u256(
    scalar: U256
) -> M24U288 {
    var value: M24U288;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        value[i] =
            scalar[i];
    }

    value[8] =
        0u;

    return value;
}


fn m24_u288_is_zero(
    value: M24U288
) -> bool {
    var accum: u32 =
        0u;

    for (
        var i: u32 = 0u;
        i < 9u;
        i = i + 1u
    ) {
        accum =
            accum |
            value[i];
    }

    return accum ==
        0u;
}


fn m24_u288_add_small(
    value: M24U288,
    small: u32
) -> M24U288 {
    var result =
        value;

    var carry =
        small;

    for (
        var i: u32 = 0u;
        i < 9u;
        i = i + 1u
    ) {
        if (carry == 0u) {
            break;
        }

        let old =
            result[i];

        let sum =
            old +
            carry;

        result[i] =
            sum;

        carry =
            select(
                0u,
                1u,
                sum < old
            );
    }

    return result;
}


fn m24_u288_sub_small(
    value: M24U288,
    small: u32
) -> M24U288 {
    var result =
        value;

    var borrow =
        small;

    for (
        var i: u32 = 0u;
        i < 9u;
        i = i + 1u
    ) {
        if (borrow == 0u) {
            break;
        }

        let old =
            result[i];

        result[i] =
            old -
            borrow;

        borrow =
            select(
                0u,
                1u,
                old < borrow
            );
    }

    return result;
}


fn m24_u288_shift_right_one(
    value: M24U288
) -> M24U288 {
    var result =
        value;

    var carry: u32 =
        0u;

    var i: u32 =
        9u;

    loop {
        if (i == 0u) {
            break;
        }

        i =
            i -
            1u;

        let nextCarry =
            result[i] &
            1u;

        result[i] =
            (
                result[i] >>
                1u
            ) |
            (
                carry <<
                31u
            );

        carry =
            nextCarry;
    }

    return result;
}


fn m24_wnaf6_table_point(
    bitIndex: u32,
    magnitudeIndex: u32
) -> JacobianPoint {
    // magnitudeIndex 0..15 maps to odd digit 1,3,...,31.
    return m24_table_point(
        M24_WNAF6_BASE_WORDS,
        16u,
        bitIndex,
        magnitudeIndex
    );
}


fn m24_scalar_mul_wnaf6(
    scalar: U256
) -> JacobianPoint {
    var k =
        m24_u288_from_u256(
            scalar
        );

    var result =
        jacobian_infinity();

    for (
        var bitIndex: u32 = 0u;
        bitIndex < 257u;
        bitIndex = bitIndex + 1u
    ) {
        if (m24_u288_is_zero(k)) {
            break;
        }

        if (
            (
                k[0] &
                1u
            ) !=
            0u
        ) {
            let low =
                k[0] &
                63u;

            var magnitude: u32 =
                low;

            var negative =
                false;

            if (low > 32u) {
                magnitude =
                    64u -
                    low;

                negative =
                    true;

                k =
                    m24_u288_add_small(
                        k,
                        magnitude
                    );
            } else {
                k =
                    m24_u288_sub_small(
                        k,
                        magnitude
                    );
            }

            // Odd magnitudes are 1..31.
            let magnitudeIndex =
                (
                    magnitude -
                    1u
                ) /
                2u;

            var q =
                m24_wnaf6_table_point(
                    bitIndex,
                    magnitudeIndex
                );

            if (negative) {
                q.y =
                    field_sub(
                        u256_zero(),
                        q.y
                    );
            }

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }

        k =
            m24_u288_shift_right_one(
                k
            );
    }

    return result;
}


fn m24_affine_write(
    point: JacobianPoint
) {
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let zInv3 =
        field_mul(
            zInv2,
            zInv
        );

    let x =
        field_mul(
            point.x,
            zInv2
        );

    let y =
        field_mul(
            point.y,
            zInv3
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            x[i];

        output.words[
            8u + i
        ] =
            y[i];
    }
}


@compute
@workgroup_size(1)
fn photon_m24_point_check() {
    let scalar =
        scalar_from_input();

    let variant =
        input.words[8];

    var point =
        jacobian_infinity();

    switch variant {
        case 0u: {
            point =
                m24_scalar_mul_g4(
                    scalar
                );
        }

        case 1u: {
            point =
                m24_scalar_mul_g5(
                    scalar
                );
        }

        case 2u: {
            point =
                m24_scalar_mul_g6(
                    scalar
                );
        }

        case 3u: {
            point =
                m24_scalar_mul_g8(
                    scalar
                );
        }

        default: {
            point =
                m24_scalar_mul_wnaf6(
                    scalar
                );
        }
    }

    m24_affine_write(
        point
    );
}


// Stage benchmark includes the same SHA256 + arbitrary-key RFC6979 used by
// live mining, then the chosen fixed-base multiplication.

@compute
@workgroup_size(256)
fn photon_m24_kg_g4(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g4(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m24_kg_g5(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g5(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m24_kg_g6(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g6(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m24_kg_g8(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g8(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m24_kg_wnaf6(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_wnaf6(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


// Complete arbitrary-key mining kernels, all at workgroup_size(256).

@compute
@workgroup_size(256)
fn photon_m24_full_g4(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g4(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m24_full_g5(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g5(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m24_full_g6(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g6(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m24_full_g8(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_g8(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m24_full_wnaf6(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m24_scalar_mul_wnaf6(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 25 — wide unsigned fixed-window sweep (8..12 bits)
// ============================================================================

const M25_G8_BASE_WORDS: u32 = 0u;
const M25_G9_BASE_WORDS: u32 = 131072u;
const M25_G10_BASE_WORDS: u32 = 368640u;
const M25_G11_BASE_WORDS: u32 = 794624u;
const M25_G12_BASE_WORDS: u32 = 1581056u;


fn m25_table_point(
    baseWords: u32,
    entriesPerWindow: u32,
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    let base =
        baseWords +
        (
            windowIndex *
            entriesPerWindow +
            digit
        ) *
        16u;

    var p:
        JacobianPoint;

    for (
        var i: u32 =
            0u;
        i <
            8u;
        i =
            i +
            1u
    ) {
        p.x[i] =
            generatorTable[
                base +
                i
            ];

        p.y[i] =
            generatorTable[
                base +
                8u +
                i
            ];
    }

    p.z =
        u256_one();

    return p;
}


fn m25_scalar_window_bits(
    scalar: U256,
    bitOffset: u32,
    width: u32
) -> u32 {
    let limb =
        bitOffset /
        32u;

    let shift =
        bitOffset %
        32u;

    if (
        limb >=
        8u
    ) {
        return 0u;
    }

    var value =
        scalar[
            limb
        ] >>
        shift;

    if (
        shift !=
            0u &&
        shift +
            width >
            32u &&
        limb +
            1u <
            8u
    ) {
        value =
            value |
            (
                scalar[
                    limb +
                    1u
                ] <<
                (
                    32u -
                    shift
                )
            );
    }

    let mask =
        (
            1u <<
            width
        ) -
        1u;

    return value &
        mask;
}


fn m25_scalar_mul_unsigned_window(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32,
    baseWords: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 =
            0u;
        windowIndex <
            windows;
        windowIndex =
            windowIndex +
            1u
    ) {
        let digit =
            m25_scalar_window_bits(
                scalar,
                windowIndex *
                    width,
                width
            );

        if (
            digit !=
            0u
        ) {
            let q =
                m25_table_point(
                    baseWords,
                    entries,
                    windowIndex,
                    digit
                );

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}



fn m25_scalar_mul_g8(
    scalar: U256
) -> JacobianPoint {
    return m25_scalar_mul_unsigned_window(
        scalar,
        8u,
        32u,
        256u,
        M25_G8_BASE_WORDS
    );
}


fn m25_scalar_mul_g9(
    scalar: U256
) -> JacobianPoint {
    return m25_scalar_mul_unsigned_window(
        scalar,
        9u,
        29u,
        512u,
        M25_G9_BASE_WORDS
    );
}


fn m25_scalar_mul_g10(
    scalar: U256
) -> JacobianPoint {
    return m25_scalar_mul_unsigned_window(
        scalar,
        10u,
        26u,
        1024u,
        M25_G10_BASE_WORDS
    );
}


fn m25_scalar_mul_g11(
    scalar: U256
) -> JacobianPoint {
    return m25_scalar_mul_unsigned_window(
        scalar,
        11u,
        24u,
        2048u,
        M25_G11_BASE_WORDS
    );
}


fn m25_scalar_mul_g12(
    scalar: U256
) -> JacobianPoint {
    return m25_scalar_mul_unsigned_window(
        scalar,
        12u,
        22u,
        4096u,
        M25_G12_BASE_WORDS
    );
}


fn m25_affine_write(
    point:
        JacobianPoint
) {
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let zInv3 =
        field_mul(
            zInv2,
            zInv
        );

    let x =
        field_mul(
            point.x,
            zInv2
        );

    let y =
        field_mul(
            point.y,
            zInv3
        );

    for (
        var i: u32 =
            0u;
        i <
            8u;
        i =
            i +
            1u
    ) {
        output.words[i] =
            x[i];

        output.words[
            8u +
            i
        ] =
            y[i];
    }
}


@compute
@workgroup_size(1)
fn photon_m25_point_check() {
    let scalar =
        scalar_from_input();

    let variant =
        input.words[8];

    var point =
        jacobian_infinity();

    switch variant {
        case 0u: {
            point =
                m25_scalar_mul_g8(
                    scalar
                );
        }

        case 1u: {
            point =
                m25_scalar_mul_g9(
                    scalar
                );
        }

        case 2u: {
            point =
                m25_scalar_mul_g10(
                    scalar
                );
        }

        case 3u: {
            point =
                m25_scalar_mul_g11(
                    scalar
                );
        }

        default: {
            point =
                m25_scalar_mul_g12(
                    scalar
                );
        }
    }

    m25_affine_write(
        point
    );
}



@compute
@workgroup_size(256)
fn photon_m25_kg_g8(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g8(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_full_g8(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g8(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m25_kg_g9(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g9(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_full_g9(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g9(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m25_kg_g10(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g10(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_full_g10(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g10(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m25_kg_g11(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g11(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_full_g11(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g11(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m25_kg_g12(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g12(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_full_g12(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m25_scalar_mul_g12(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m25_profiler_g8(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (
        stageSelector ==
        1u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let point =
        m25_scalar_mul_g8(
            k
        );

    if (
        stageSelector ==
        2u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (
        stageSelector ==
        3u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (
        stageSelector ==
        4u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_profiler_g9(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (
        stageSelector ==
        1u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let point =
        m25_scalar_mul_g9(
            k
        );

    if (
        stageSelector ==
        2u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (
        stageSelector ==
        3u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (
        stageSelector ==
        4u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_profiler_g10(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (
        stageSelector ==
        1u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let point =
        m25_scalar_mul_g10(
            k
        );

    if (
        stageSelector ==
        2u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (
        stageSelector ==
        3u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (
        stageSelector ==
        4u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_profiler_g11(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (
        stageSelector ==
        1u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let point =
        m25_scalar_mul_g11(
            k
        );

    if (
        stageSelector ==
        2u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (
        stageSelector ==
        3u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (
        stageSelector ==
        4u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m25_profiler_g12(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let stageSelector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (
        stageSelector ==
        1u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^
            k[3] ^
            k[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let point =
        m25_scalar_mul_g12(
            k
        );

    if (
        stageSelector ==
        2u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (
        stageSelector ==
        3u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^
            rx[3] ^
            rx[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (
        stageSelector ==
        4u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^
            s[3] ^
            s[7] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}



// ============================================================================
// Milestone 26 — extreme fixed-window sweep + RFC6979 sub-profiler
// ============================================================================

const M26_G12_BASE_WORDS: u32 = 0u;
const M26_G13_BASE_WORDS: u32 = 1441792u;
const M26_G14_BASE_WORDS: u32 = 4063232u;
const M26_G15_BASE_WORDS: u32 = 9043968u;


fn m26_table_point(
    baseWords: u32,
    entriesPerWindow: u32,
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    let base =
        baseWords +
        (
            windowIndex *
            entriesPerWindow +
            digit
        ) *
        16u;

    var p:
        JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] =
            generatorTable[
                base + i
            ];

        p.y[i] =
            generatorTable[
                base + 8u + i
            ];
    }

    p.z =
        u256_one();

    return p;
}


fn m26_scalar_window_bits(
    scalar: U256,
    bitOffset: u32,
    width: u32
) -> u32 {
    let limb =
        bitOffset /
        32u;

    let shift =
        bitOffset %
        32u;

    if (
        limb >=
        8u
    ) {
        return 0u;
    }

    var value =
        scalar[limb] >>
        shift;

    if (
        shift != 0u &&
        shift + width > 32u &&
        limb + 1u < 8u
    ) {
        value =
            value |
            (
                scalar[
                    limb + 1u
                ] <<
                (
                    32u - shift
                )
            );
    }

    let mask =
        (
            1u << width
        ) - 1u;

    return value &
        mask;
}


fn m26_scalar_mul_unsigned_window(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32,
    baseWords: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < windows;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m26_scalar_window_bits(
                scalar,
                windowIndex * width,
                width
            );

        if (
            digit != 0u
        ) {
            let q =
                m26_table_point(
                    baseWords,
                    entries,
                    windowIndex,
                    digit
                );

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}



fn m26_scalar_mul_g12(
    scalar: U256
) -> JacobianPoint {
    return m26_scalar_mul_unsigned_window(
        scalar,
        12u,
        22u,
        4096u,
        M26_G12_BASE_WORDS
    );
}


fn m26_scalar_mul_g13(
    scalar: U256
) -> JacobianPoint {
    return m26_scalar_mul_unsigned_window(
        scalar,
        13u,
        20u,
        8192u,
        M26_G13_BASE_WORDS
    );
}


fn m26_scalar_mul_g14(
    scalar: U256
) -> JacobianPoint {
    return m26_scalar_mul_unsigned_window(
        scalar,
        14u,
        19u,
        16384u,
        M26_G14_BASE_WORDS
    );
}


fn m26_scalar_mul_g15(
    scalar: U256
) -> JacobianPoint {
    return m26_scalar_mul_unsigned_window(
        scalar,
        15u,
        18u,
        32768u,
        M26_G15_BASE_WORDS
    );
}


fn m26_affine_write(
    point:
        JacobianPoint
) {
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let zInv3 =
        field_mul(
            zInv2,
            zInv
        );

    let x =
        field_mul(
            point.x,
            zInv2
        );

    let y =
        field_mul(
            point.y,
            zInv3
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            x[i];

        output.words[
            8u + i
        ] =
            y[i];
    }
}


@compute
@workgroup_size(1)
fn photon_m26_point_check() {
    let scalar =
        scalar_from_input();

    let variant =
        input.words[8];

    var point =
        jacobian_infinity();

    switch variant {
        case 0u: {
            point =
                m26_scalar_mul_g12(
                    scalar
                );
        }

        case 1u: {
            point =
                m26_scalar_mul_g13(
                    scalar
                );
        }

        case 2u: {
            point =
                m26_scalar_mul_g14(
                    scalar
                );
        }

        default: {
            point =
                m26_scalar_mul_g15(
                    scalar
                );
        }
    }

    m26_affine_write(
        point
    );
}



@compute
@workgroup_size(256)
fn photon_m26_kg_g12(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g12(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m26_full_g12(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g12(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m26_kg_g13(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g13(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m26_full_g13(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g13(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m26_kg_g14(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g14(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m26_full_g14(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g14(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m26_kg_g15(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g15(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m26_full_g15(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m26_scalar_mul_g15(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



@compute
@workgroup_size(256)
fn photon_m26_rfc6979_profiler(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let selector =
        input.words[153] &
        0xffu;

    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let msgmod =
        m9_message_hash(
            nonce
        );

    if (
        selector == 1u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            msgmod[0] ^
            msgmod[7] ^
            msgmod[15] ^
            msgmod[31] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    var V =
        bytes32_fill(
            0x01u
        );

    var K =
        bytes32_fill(
            0x00u
        );

    var message113:
        Bytes128;

    var message32:
        Bytes128;

    let algo:
        array<u32, 16> =
        array<u32, 16>(
            0x53u,0x63u,0x68u,0x6eu,
            0x6fu,0x72u,0x72u,0x2bu,
            0x53u,0x48u,0x41u,0x32u,
            0x35u,0x36u,0x20u,0x20u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[
            33u + i
        ] =
            x[i];

        message113[
            65u + i
        ] =
            msgmod[i];
    }

    message113[32] =
        0u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[
            97u + i
        ] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    if (
        selector == 2u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            K[0] ^
            K[7] ^
            K[15] ^
            K[31] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    if (
        selector == 3u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            V[0] ^
            V[7] ^
            V[15] ^
            V[31] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[
            33u + i
        ] =
            x[i];

        message113[
            65u + i
        ] =
            msgmod[i];
    }

    message113[32] =
        1u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[
            97u + i
        ] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    if (
        selector == 4u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            K[0] ^
            K[7] ^
            K[15] ^
            K[31] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    if (
        selector == 5u
    ) {
        atomicAdd(
            &benchmarkOutput.checksum,
            V[0] ^
            V[7] ^
            V[15] ^
            V[31] ^
            nonce
        );

        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );

        return;
    }

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        V[0] ^
        V[7] ^
        V[15] ^
        V[31] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}



// ============================================================================
// Milestone 27 — 16-bit fixed-base + RFC6979 first-HMAC SHA precomputation.
// ============================================================================

const M27_G4_BASE_WORDS: u32 = 0u;
const M27_G16_BASE_WORDS: u32 = 16384u;


fn m27_table_point(
    baseWords: u32,
    entriesPerWindow: u32,
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    let base =
        baseWords +
        (
            windowIndex *
            entriesPerWindow +
            digit
        ) *
        16u;

    var p:
        JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] =
            generatorTable[
                base + i
            ];

        p.y[i] =
            generatorTable[
                base + 8u + i
            ];
    }

    p.z =
        u256_one();

    return p;
}


fn m27_scalar_window_bits(
    scalar: U256,
    bitOffset: u32,
    width: u32
) -> u32 {
    let limb =
        bitOffset /
        32u;

    let shift =
        bitOffset %
        32u;

    if (
        limb >=
        8u
    ) {
        return 0u;
    }

    var value =
        scalar[
            limb
        ] >>
        shift;

    if (
        shift != 0u &&
        shift + width > 32u &&
        limb + 1u < 8u
    ) {
        value =
            value |
            (
                scalar[
                    limb + 1u
                ] <<
                (
                    32u -
                    shift
                )
            );
    }

    let mask =
        select(
            (1u << width) - 1u,
            0xffffffffu,
            width == 32u
        );

    return value &
        mask;
}


fn m27_scalar_mul_window(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32,
    baseWords: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < windows;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m27_scalar_window_bits(
                scalar,
                windowIndex * width,
                width
            );

        if (
            digit != 0u
        ) {
            let q =
                m27_table_point(
                    baseWords,
                    entries,
                    windowIndex,
                    digit
                );

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m27_scalar_mul_g4(
    scalar: U256
) -> JacobianPoint {
    return m27_scalar_mul_window(
        scalar,
        4u,
        64u,
        16u,
        M27_G4_BASE_WORDS
    );
}


fn m27_scalar_mul_g16(
    scalar: U256
) -> JacobianPoint {
    return m27_scalar_mul_window(
        scalar,
        16u,
        16u,
        65536u,
        M27_G16_BASE_WORDS
    );
}


fn m27_affine_write(
    point:
        JacobianPoint
) {
    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let zInv3 =
        field_mul(
            zInv2,
            zInv
        );

    let x =
        field_mul(
            point.x,
            zInv2
        );

    let y =
        field_mul(
            point.y,
            zInv3
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            x[i];

        output.words[
            8u + i
        ] =
            y[i];
    }
}


@compute
@workgroup_size(1)
fn photon_m27_point_check() {
    let scalar =
        scalar_from_input();

    let variant =
        input.words[8];

    var point =
        m27_scalar_mul_g4(
            scalar
        );

    if (
        variant == 1u
    ) {
        point =
            m27_scalar_mul_g16(
                scalar
            );
    }

    m27_affine_write(
        point
    );
}


// Precompute:
// words[0..7]  = SHA state after HMAC ipad block + first 64 message bytes
// words[8..15] = SHA state after HMAC opad block
@compute
@workgroup_size(1)
fn photon_m27_precompute_hmac_states() {
    let x =
        m17_private_key_bytes();

    var innerState =
        initializeState();

    var ipadBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        ipadBlock[i] =
            0x36363636u;
    }

    innerState =
        compressBlock(
            innerState,
            ipadBlock
        );

    var messageBlock:
        array<u32, 16>;

    for (
        var word: u32 = 0u;
        word < 16u;
        word = word + 1u
    ) {
        let byteStart =
            word * 4u;

        var packedWord =
            0u;

        for (
            var j: u32 = 0u;
            j < 4u;
            j = j + 1u
        ) {
            let idx =
                byteStart + j;

            var b =
                0u;

            if (
                idx < 32u
            ) {
                b =
                    0x01u;
            } else if (
                idx == 32u
            ) {
                b =
                    0u;
            } else {
                // message bytes 33..63 are private-key bytes 0..30
                b =
                    x[
                        idx - 33u
                    ];
            }

            packedWord =
                packedWord |
                (
                    b <<
                    (
                        (3u - j) *
                        8u
                    )
                );
        }

        messageBlock[word] =
            packedWord;
    }

    innerState =
        compressBlock(
            innerState,
            messageBlock
        );

    var outerState =
        initializeState();

    var opadBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        opadBlock[i] =
            0x5c5c5c5cu;
    }

    outerState =
        compressBlock(
            outerState,
            opadBlock
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            innerState[i];

        output.words[
            8u + i
        ] =
            outerState[i];
    }
}


fn m27_first_k_hmac_precomputed(
    msgmod:
        Bytes32,
    x:
        Bytes32
) -> Bytes32 {
    var innerState:
        array<u32, 8>;

    var outerState:
        array<u32, 8>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        innerState[i] =
            m27Precomputed.words[i];

        outerState[i] =
            m27Precomputed.words[
                8u + i
            ];
    }

    // Remaining inner bytes after the two precomputed blocks:
    // x[31] || msgmod[32] || "Schnorr+SHA256  "[16] = 49 bytes.
    var finalInner:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        finalInner[i] =
            0u;
    }

    let algo:
        array<u32, 16> =
        array<u32, 16>(
            0x53u,0x63u,0x68u,0x6eu,
            0x6fu,0x72u,0x72u,0x2bu,
            0x53u,0x48u,0x41u,0x32u,
            0x35u,0x36u,0x20u,0x20u
        );

    for (
        var byteIndex: u32 = 0u;
        byteIndex < 49u;
        byteIndex = byteIndex + 1u
    ) {
        var b =
            0u;

        if (
            byteIndex == 0u
        ) {
            b =
                x[31u];
        } else if (
            byteIndex <= 32u
        ) {
            b =
                msgmod[
                    byteIndex - 1u
                ];
        } else {
            b =
                algo[
                    byteIndex - 33u
                ];
        }

        let wordIndex =
            byteIndex / 4u;

        let shift =
            (
                3u -
                (
                    byteIndex %
                    4u
                )
            ) *
            8u;

        finalInner[wordIndex] =
            finalInner[wordIndex] |
            (
                b <<
                shift
            );
    }

    // SHA padding after 49 remaining bytes.
    let padByte =
        49u;

    finalInner[
        padByte / 4u
    ] =
        finalInner[
            padByte / 4u
        ] |
        (
            0x80u <<
            (
                (
                    3u -
                    (
                        padByte %
                        4u
                    )
                ) *
                8u
            )
        );

    // Full inner SHA length = 64-byte ipad + 113-byte message = 177 bytes.
    finalInner[15] =
        177u *
        8u;

    innerState =
        compressBlock(
            innerState,
            finalInner
        );

    let innerDigest =
        digest_words_to_bytes(
            innerState
        );

    // Outer HMAC continuation: opad state already processed 64 bytes.
    // Append 32-byte inner digest, SHA padding, total length 96 bytes.
    var finalOuter:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        finalOuter[i] =
            0u;
    }

    for (
        var byteIndex: u32 = 0u;
        byteIndex < 32u;
        byteIndex = byteIndex + 1u
    ) {
        let wordIndex =
            byteIndex / 4u;

        let shift =
            (
                3u -
                (
                    byteIndex %
                    4u
                )
            ) *
            8u;

        finalOuter[wordIndex] =
            finalOuter[wordIndex] |
            (
                innerDigest[
                    byteIndex
                ] <<
                shift
            );
    }

    finalOuter[8] =
        0x80000000u;

    finalOuter[15] =
        96u *
        8u;

    outerState =
        compressBlock(
            outerState,
            finalOuter
        );

    return digest_words_to_bytes(
        outerState
    );
}


fn m27_rfc6979_nonce_optimized(
    msgmod:
        Bytes32,
    x:
        Bytes32
) -> Bytes32 {
    var V =
        bytes32_fill(
            0x01u
        );

    var K =
        m27_first_k_hmac_precomputed(
            msgmod,
            x
        );

    var message113:
        Bytes128;

    var message32:
        Bytes128;

    let algo:
        array<u32, 16> =
        array<u32, 16>(
            0x53u,0x63u,0x68u,0x6eu,
            0x6fu,0x72u,0x72u,0x2bu,
            0x53u,0x48u,0x41u,0x32u,
            0x35u,0x36u,0x20u,0x20u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message113[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message113[i] =
            V[i];

        message113[
            33u + i
        ] =
            x[i];

        message113[
            65u + i
        ] =
            msgmod[i];
    }

    message113[32] =
        1u;

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        message113[
            97u + i
        ] =
            algo[i];
    }

    K =
        hmac_sha256_fixed(
            K,
            message113,
            113u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    for (
        var i: u32 = 0u;
        i < 128u;
        i = i + 1u
    ) {
        message32[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 32u;
        i = i + 1u
    ) {
        message32[i] =
            V[i];
    }

    V =
        hmac_sha256_fixed(
            K,
            message32,
            32u
        );

    return V;
}


@compute
@workgroup_size(1)
fn photon_m27_rfc_correctness() {
    let nonce =
        input.byteLength;

    let x =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let production =
        m17_rfc6979_nonce(
            messageHash,
            x
        );

    let optimized =
        m27_rfc6979_nonce_optimized(
            messageHash,
            x
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            (
                production[i * 4u] << 24u
            ) |
            (
                production[
                    i * 4u + 1u
                ] << 16u
            ) |
            (
                production[
                    i * 4u + 2u
                ] << 8u
            ) |
            production[
                i * 4u + 3u
            ];

        output.words[
            8u + i
        ] =
            (
                optimized[i * 4u] << 24u
            ) |
            (
                optimized[
                    i * 4u + 1u
                ] << 16u
            ) |
            (
                optimized[
                    i * 4u + 2u
                ] << 8u
            ) |
            optimized[
                i * 4u + 3u
            ];
    }
}


@compute
@workgroup_size(256)
fn photon_m27_rfc_baseline_bench(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let h =
        m9_message_hash(
            nonce
        );

    let k =
        m17_rfc6979_nonce(
            h,
            x
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        k[0] ^
        k[7] ^
        k[15] ^
        k[31] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m27_rfc_optimized_bench(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let h =
        m9_message_hash(
            nonce
        );

    let k =
        m27_rfc6979_nonce_optimized(
            h,
            x
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        k[0] ^
        k[7] ^
        k[15] ^
        k[31] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m27_kg16_production(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let h =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            h,
            x
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m27_scalar_mul_g16(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m27_kg16_optimized(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let h =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            h,
            x
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m27_scalar_mul_g16(
            k
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        point.x[0] ^
        point.y[3] ^
        point.z[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m27_full16_production(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m27_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m27_full16_optimized(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m27_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 28 — final 14/15/16-bit optimized bake-off and end-to-end gate.
// ============================================================================

const M28_G14_BASE_WORDS: u32 = 0u;
const M28_G15_BASE_WORDS: u32 = 4980736u;
const M28_G16_BASE_WORDS: u32 = 14417920u;

fn m28_table_point(
    baseWords: u32,
    entriesPerWindow: u32,
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    let base =
        baseWords +
        (
            windowIndex *
            entriesPerWindow +
            digit
        ) *
        16u;

    var p: JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] = generatorTable[base + i];
        p.y[i] = generatorTable[base + 8u + i];
    }

    p.z = u256_one();
    return p;
}

fn m28_scalar_window_bits(
    scalar: U256,
    bitOffset: u32,
    width: u32
) -> u32 {
    let limb = bitOffset / 32u;
    let shift = bitOffset % 32u;

    if (limb >= 8u) {
        return 0u;
    }

    var value = scalar[limb] >> shift;

    if (
        shift != 0u &&
        shift + width > 32u &&
        limb + 1u < 8u
    ) {
        value = value |
            (
                scalar[limb + 1u] <<
                (32u - shift)
            );
    }

    let mask = (1u << width) - 1u;
    return value & mask;
}

fn m28_scalar_mul_window(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32,
    baseWords: u32
) -> JacobianPoint {
    var result = jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < windows;
        windowIndex = windowIndex + 1u
    ) {
        let digit = m28_scalar_window_bits(
            scalar,
            windowIndex * width,
            width
        );

        if (digit != 0u) {
            let q = m28_table_point(
                baseWords,
                entries,
                windowIndex,
                digit
            );

            result = jacobian_add_affine(
                result,
                q.x,
                q.y
            );
        }
    }

    return result;
}

fn m28_scalar_mul_g14(scalar: U256) -> JacobianPoint {
    return m28_scalar_mul_window(
        scalar, 14u, 19u, 16384u, M28_G14_BASE_WORDS
    );
}

fn m28_scalar_mul_g15(scalar: U256) -> JacobianPoint {
    return m28_scalar_mul_window(
        scalar, 15u, 18u, 32768u, M28_G15_BASE_WORDS
    );
}

fn m28_scalar_mul_g16(scalar: U256) -> JacobianPoint {
    return m28_scalar_mul_window(
        scalar, 16u, 16u, 65536u, M28_G16_BASE_WORDS
    );
}


@compute
@workgroup_size(1)
fn photon_m28_candidate_14_production()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g14(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m28_candidate_14_optimized()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g14(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m28_candidate_15_production()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g15(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m28_candidate_15_optimized()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g15(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m28_candidate_16_production()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m28_candidate_16_optimized()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m28_full_14_production(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g14(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m28_full_14_optimized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g14(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m28_full_15_production(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g15(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m28_full_15_optimized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g15(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m28_full_16_production(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m17_rfc6979_nonce(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m28_full_16_optimized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m28_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 29 — isolated generator-buffer final bake-off.
// Each pipeline binds only one width-specific table beginning at word offset 0.
// ============================================================================

fn m29_table_point(
    entriesPerWindow: u32,
    windowIndex: u32,
    digit: u32
) -> JacobianPoint {
    let base =
        (
            windowIndex *
            entriesPerWindow +
            digit
        ) *
        16u;

    var p:
        JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] =
            generatorTable[
                base + i
            ];

        p.y[i] =
            generatorTable[
                base + 8u + i
            ];
    }

    p.z =
        u256_one();

    return p;
}


fn m29_scalar_window_bits(
    scalar: U256,
    bitOffset: u32,
    width: u32
) -> u32 {
    let limb =
        bitOffset /
        32u;

    let shift =
        bitOffset %
        32u;

    if (
        limb >=
        8u
    ) {
        return 0u;
    }

    var value =
        scalar[
            limb
        ] >>
        shift;

    if (
        shift != 0u &&
        shift + width > 32u &&
        limb + 1u < 8u
    ) {
        value =
            value |
            (
                scalar[
                    limb + 1u
                ] <<
                (
                    32u -
                    shift
                )
            );
    }

    let mask =
        (
            1u <<
            width
        ) -
        1u;

    return value &
        mask;
}


fn m29_scalar_mul_window(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < windows;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex *
                    width,
                width
            );

        if (
            digit != 0u
        ) {
            let q =
                m29_table_point(
                    entries,
                    windowIndex,
                    digit
                );

            result =
                jacobian_add_affine(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m29_scalar_mul_g14(
    scalar: U256
) -> JacobianPoint {
    return m29_scalar_mul_window(
        scalar,
        14u,
        19u,
        16384u
    );
}


fn m29_scalar_mul_g15(
    scalar: U256
) -> JacobianPoint {
    return m29_scalar_mul_window(
        scalar,
        15u,
        18u,
        32768u
    );
}


fn m29_scalar_mul_g16(
    scalar: U256
) -> JacobianPoint {
    return m29_scalar_mul_window(
        scalar,
        16u,
        16u,
        65536u
    );
}



@compute
@workgroup_size(256)
fn photon_m29_full_14_optimized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g14(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m29_full_15_optimized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g15(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m29_full_16_optimized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 30 — PHOTON-specific SHA specialization.
//
// 1) Signing-message SHA256 is always exactly 36 bytes:
//      nonceLE[4] || target[32]
//    Build the single padded SHA block directly instead of constructing and
//    zeroing a generic 256-byte byte array.
//
// 2) The 615-byte candidate transaction's bytes 0..383 are constant for one
//    mining template. Precompute SHA256 state after those first 6 blocks once.
//    Per candidate, process only first-SHA blocks 6..9, then the normal second
//    SHA256 block.
// ============================================================================

fn m30_pack_input_word(
    byteStart: u32
) -> u32 {
    return
        (m9_input_byte(byteStart) << 24u) |
        (m9_input_byte(byteStart + 1u) << 16u) |
        (m9_input_byte(byteStart + 2u) << 8u) |
        m9_input_byte(byteStart + 3u);
}


fn m30_nonce_le_as_be_word(
    nonce: u32
) -> u32 {
    return
        ((nonce & 0x000000ffu) << 24u) |
        ((nonce & 0x0000ff00u) << 8u) |
        ((nonce & 0x00ff0000u) >> 8u) |
        ((nonce & 0xff000000u) >> 24u);
}


fn m30_message_hash_specialized(
    nonce: u32
) -> Bytes32 {
    var block:
        array<u32, 16>;

    // nonceLE as the first four message bytes.
    block[0] =
        m30_nonce_le_as_be_word(
            nonce
        );

    // target occupies template bytes 394..425, exactly 32 bytes.
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        block[1u + i] =
            m30_pack_input_word(
                394u +
                i * 4u
            );
    }

    // SHA256 padding for exactly 36 bytes.
    block[9] =
        0x80000000u;

    for (
        var i: u32 = 10u;
        i < 15u;
        i = i + 1u
    ) {
        block[i] =
            0u;
    }

    block[15] =
        288u;

    let digest =
        compressBlock(
            initializeState(),
            block
        );

    return digest_words_to_bytes(
        digest
    );
}


@compute
@workgroup_size(1)
fn photon_m30_precompute_tx_prefix() {
    var state =
        initializeState();

    // Exactly six constant 64-byte blocks = bytes 0..383.
    for (
        var blockIndex: u32 = 0u;
        blockIndex < 6u;
        blockIndex = blockIndex + 1u
    ) {
        var block:
            array<u32, 16>;

        for (
            var wordInBlock: u32 = 0u;
            wordInBlock < 16u;
            wordInBlock = wordInBlock + 1u
        ) {
            block[wordInBlock] =
                input.words[
                    blockIndex * 16u +
                    wordInBlock
                ];
        }

        state =
            compressBlock(
                state,
                block
            );
    }

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            state[i];
    }
}


fn m30_transaction_hashes_prefixed(
    nonce: u32,
    rBytes: Bytes32,
    sBytes: Bytes32
) -> array<array<u32, 8>, 2> {
    let byteLength =
        615u;

    let totalBlocks =
        10u;

    var state:
        array<u32, 8>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        state[i] =
            m30TxPrefix.words[i];
    }

    // Continue only with blocks 6..9 (bytes 384..639 including padding).
    for (
        var blockIndex: u32 = 6u;
        blockIndex < totalBlocks;
        blockIndex = blockIndex + 1u
    ) {
        var block:
            array<u32, 16>;

        for (
            var wordInBlock: u32 = 0u;
            wordInBlock < 16u;
            wordInBlock = wordInBlock + 1u
        ) {
            let globalWord =
                blockIndex * 16u +
                wordInBlock;

            let byteStart =
                globalWord * 4u;

            var word =
                0u;

            for (
                var j: u32 = 0u;
                j < 4u;
                j = j + 1u
            ) {
                let byteIndex =
                    byteStart +
                    j;

                var b =
                    0u;

                if (
                    byteIndex <
                    byteLength
                ) {
                    b =
                        m9_completed_tx_byte(
                            byteIndex,
                            nonce,
                            rBytes,
                            sBytes
                        );

                } else if (
                    byteIndex ==
                    byteLength
                ) {
                    b =
                        0x80u;

                } else if (
                    byteIndex >=
                    totalBlocks * 64u - 8u
                ) {
                    let lengthOffset =
                        byteIndex -
                        (
                            totalBlocks * 64u - 8u
                        );

                    if (
                        lengthOffset >=
                        4u
                    ) {
                        let lowOffset =
                            lengthOffset -
                            4u;

                        let bitLength =
                            byteLength *
                            8u;

                        b =
                            (
                                bitLength >>
                                (
                                    (3u - lowOffset) *
                                    8u
                                )
                            ) &
                            0xffu;
                    }
                }

                word =
                    word |
                    (
                        b <<
                        (
                            (3u - j) *
                            8u
                        )
                    );
            }

            block[wordInBlock] =
                word;
        }

        state =
            compressBlock(
                state,
                block
            );
    }

    var secondBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        secondBlock[i] =
            state[i];
    }

    secondBlock[8] =
        0x80000000u;

    for (
        var i: u32 = 9u;
        i < 15u;
        i = i + 1u
    ) {
        secondBlock[i] =
            0u;
    }

    secondBlock[15] =
        256u;

    let finalDigest =
        compressBlock(
            initializeState(),
            secondBlock
        );

    return array<array<u32, 8>, 2>(
        state,
        finalDigest
    );
}


@compute
@workgroup_size(1)
fn photon_m30_message_hash_check() {
    let nonce =
        input.byteLength;

    let baseline =
        m9_message_hash(
            nonce
        );

    let specialized =
        m30_message_hash_specialized(
            nonce
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            (baseline[i * 4u] << 24u) |
            (baseline[i * 4u + 1u] << 16u) |
            (baseline[i * 4u + 2u] << 8u) |
            baseline[i * 4u + 3u];

        output.words[8u + i] =
            (specialized[i * 4u] << 24u) |
            (specialized[i * 4u + 1u] << 16u) |
            (specialized[i * 4u + 2u] << 8u) |
            specialized[i * 4u + 3u];
    }
}


@compute
@workgroup_size(256)
fn photon_m30_message_baseline_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let h =
        m9_message_hash(
            nonce
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        h[0] ^ h[7] ^ h[15] ^ h[31] ^ nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m30_message_specialized_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let h =
        m30_message_hash_specialized(
            nonce
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        h[0] ^ h[7] ^ h[15] ^ h[31] ^ nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(1)
fn photon_m30_candidate_baseline()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m9_transaction_hashes(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m30_candidate_specialized()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m30_message_hash_specialized(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m30_full_baseline(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m30_full_specialized(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m30_message_hash_specialized(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 31 — transaction-prefix final selection + fresh bottleneck profile.
// Keeps the proven 16-bit kG, optimized RFC6979, and WG256 path fixed.
// ============================================================================


@compute
@workgroup_size(1)
fn photon_m31_candidate_prefix_only()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m31_full_prefix_only(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m31_profile_baseline(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m31Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^
            messageHash[7] ^
            messageHash[15] ^
            messageHash[31] ^
            nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let hashes =
        m9_transaction_hashes(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );
    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m31_profile_prefix_only(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m31Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^
            messageHash[7] ^
            messageHash[15] ^
            messageHash[31] ^
            nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );
    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m31_profile_combined(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m31Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m30_message_hash_specialized(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^
            messageHash[7] ^
            messageHash[15] ^
            messageHash[31] ^
            nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let kBytes =
        m27_rfc6979_nonce_optimized(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^
            point.y[3] ^
            point.z[7] ^
            nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(
            &benchmarkOutput.completed,
            1u
        );
        return;
    }

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );
    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}



// ============================================================================
// Milestone 32 — fixed-shape RFC6979/HMAC-SHA256.
//
// M31 showed RFC6979 at ~48.9% of the winning kernel.
//
// Improvements:
//   * Fixed HMAC-32 and HMAC-113 SHA block layouts.
//   * No 128/256-byte scratch-message clearing for these calls.
//   * Reuse SHA states after K^ipad and K^opad whenever adjacent RFC6979
//     operations use the same K.
//
// The first K HMAC remains the M27 proven precomputed implementation.
// ============================================================================

struct M32HmacKeyStates {
    inner:
        array<u32, 8>,
    outer:
        array<u32, 8>,
};


fn m32_bytes32_word(
    value:
        Bytes32,
    byteIndex:
        u32
) -> u32 {
    return
        (value[byteIndex] << 24u) |
        (value[byteIndex + 1u] << 16u) |
        (value[byteIndex + 2u] << 8u) |
        value[byteIndex + 3u];
}


fn m32_hmac_key_states(
    key:
        Bytes32
) -> M32HmacKeyStates {
    var innerBlock:
        array<u32, 16>;

    var outerBlock:
        array<u32, 16>;

    // The HMAC key is exactly 32 bytes. The remaining 32 bytes of the
    // 64-byte HMAC key block are zero before XOR with ipad/opad.
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        let keyWord =
            m32_bytes32_word(
                key,
                i * 4u
            );

        innerBlock[i] =
            keyWord ^
            0x36363636u;

        outerBlock[i] =
            keyWord ^
            0x5c5c5c5cu;
    }

    for (
        var i: u32 = 8u;
        i < 16u;
        i = i + 1u
    ) {
        innerBlock[i] =
            0x36363636u;

        outerBlock[i] =
            0x5c5c5c5cu;
    }

    var result:
        M32HmacKeyStates;

    result.inner =
        compressBlock(
            initializeState(),
            innerBlock
        );

    result.outer =
        compressBlock(
            initializeState(),
            outerBlock
        );

    return result;
}


fn m32_hmac32_from_states(
    states:
        M32HmacKeyStates,
    message:
        Bytes32
) -> Bytes32 {
    // Inner SHA continuation after 64-byte ipad block.
    // Total SHA input = 64 + 32 = 96 bytes.
    var innerBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        innerBlock[i] =
            m32_bytes32_word(
                message,
                i * 4u
            );
    }

    innerBlock[8] =
        0x80000000u;

    for (
        var i: u32 = 9u;
        i < 15u;
        i = i + 1u
    ) {
        innerBlock[i] =
            0u;
    }

    innerBlock[15] =
        96u *
        8u;

    let innerDigest =
        compressBlock(
            states.inner,
            innerBlock
        );

    // Outer SHA continuation after 64-byte opad block.
    // Again total SHA input = 64 + 32 = 96 bytes.
    var outerBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        outerBlock[i] =
            innerDigest[i];
    }

    outerBlock[8] =
        0x80000000u;

    for (
        var i: u32 = 9u;
        i < 15u;
        i = i + 1u
    ) {
        outerBlock[i] =
            0u;
    }

    outerBlock[15] =
        96u *
        8u;

    return digest_words_to_bytes(
        compressBlock(
            states.outer,
            outerBlock
        )
    );
}


fn m32_hmac113_from_states(
    states:
        M32HmacKeyStates,
    V:
        Bytes32,
    x:
        Bytes32,
    msgmod:
        Bytes32
) -> Bytes32 {
    // RFC6979 message is exactly:
    // V[32] || 0x01 || x[32] || msgmod[32] || "Schnorr+SHA256  "[16]
    //
    // 113 bytes. Following the 64-byte ipad block, this occupies one full
    // 64-byte block plus 49 bytes in the final padded block.

    var block1:
        array<u32, 16>;

    // message bytes 0..31 = V
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        block1[i] =
            m32_bytes32_word(
                V,
                i * 4u
            );
    }

    // message byte 32 = 0x01, then x bytes 0..30.
    block1[8] =
        0x01000000u |
        (x[0] << 16u) |
        (x[1] << 8u) |
        x[2];

    block1[9] =
        (x[3] << 24u) |
        (x[4] << 16u) |
        (x[5] << 8u) |
        x[6];

    block1[10] =
        (x[7] << 24u) |
        (x[8] << 16u) |
        (x[9] << 8u) |
        x[10];

    block1[11] =
        (x[11] << 24u) |
        (x[12] << 16u) |
        (x[13] << 8u) |
        x[14];

    block1[12] =
        (x[15] << 24u) |
        (x[16] << 16u) |
        (x[17] << 8u) |
        x[18];

    block1[13] =
        (x[19] << 24u) |
        (x[20] << 16u) |
        (x[21] << 8u) |
        x[22];

    block1[14] =
        (x[23] << 24u) |
        (x[24] << 16u) |
        (x[25] << 8u) |
        x[26];

    block1[15] =
        (x[27] << 24u) |
        (x[28] << 16u) |
        (x[29] << 8u) |
        x[30];

    var innerState =
        compressBlock(
            states.inner,
            block1
        );

    var block2:
        array<u32, 16>;

    // message byte 64 = x[31], then msgmod bytes 0..31.
    block2[0] =
        (x[31] << 24u) |
        (msgmod[0] << 16u) |
        (msgmod[1] << 8u) |
        msgmod[2];

    block2[1] =
        (msgmod[3] << 24u) |
        (msgmod[4] << 16u) |
        (msgmod[5] << 8u) |
        msgmod[6];

    block2[2] =
        (msgmod[7] << 24u) |
        (msgmod[8] << 16u) |
        (msgmod[9] << 8u) |
        msgmod[10];

    block2[3] =
        (msgmod[11] << 24u) |
        (msgmod[12] << 16u) |
        (msgmod[13] << 8u) |
        msgmod[14];

    block2[4] =
        (msgmod[15] << 24u) |
        (msgmod[16] << 16u) |
        (msgmod[17] << 8u) |
        msgmod[18];

    block2[5] =
        (msgmod[19] << 24u) |
        (msgmod[20] << 16u) |
        (msgmod[21] << 8u) |
        msgmod[22];

    block2[6] =
        (msgmod[23] << 24u) |
        (msgmod[24] << 16u) |
        (msgmod[25] << 8u) |
        msgmod[26];

    block2[7] =
        (msgmod[27] << 24u) |
        (msgmod[28] << 16u) |
        (msgmod[29] << 8u) |
        msgmod[30];

    // msgmod[31] followed by the 16-byte algorithm tag.
    // "Schnorr+SHA256  "
    block2[8] =
        (msgmod[31] << 24u) |
        0x00536368u;

    block2[9] =
        0x6e6f7272u;

    block2[10] =
        0x2b534841u;

    block2[11] =
        0x32353620u;

    // Last algorithm-tag byte is space (0x20), followed by SHA padding.
    block2[12] =
        0x20800000u;

    block2[13] =
        0u;

    block2[14] =
        0u;

    // Total inner SHA input length = 64-byte ipad + 113-byte RFC message.
    block2[15] =
        177u *
        8u;

    innerState =
        compressBlock(
            innerState,
            block2
        );

    // Standard 32-byte inner digest appended after the 64-byte opad state.
    var outerBlock:
        array<u32, 16>;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        outerBlock[i] =
            innerState[i];
    }

    outerBlock[8] =
        0x80000000u;

    for (
        var i: u32 = 9u;
        i < 15u;
        i = i + 1u
    ) {
        outerBlock[i] =
            0u;
    }

    outerBlock[15] =
        96u *
        8u;

    return digest_words_to_bytes(
        compressBlock(
            states.outer,
            outerBlock
        )
    );
}


fn m32_rfc6979_nonce_fixed(
    msgmod:
        Bytes32,
    x:
        Bytes32
) -> Bytes32 {
    // Preserve the already-proven M27 first-K precomputation exactly.
    var K =
        m27_first_k_hmac_precomputed(
            msgmod,
            x
        );

    var V =
        bytes32_fill(
            0x01u
        );

    // K1 is used for both:
    //   V = HMAC(K1, V)
    //   K2 = HMAC(K1, V || 0x01 || x || msgmod || algo)
    // Compute K1 ipad/opad states only once.
    let k1States =
        m32_hmac_key_states(
            K
        );

    V =
        m32_hmac32_from_states(
            k1States,
            V
        );

    K =
        m32_hmac113_from_states(
            k1States,
            V,
            x,
            msgmod
        );

    // K2 is then reused for the two consecutive V updates.
    let k2States =
        m32_hmac_key_states(
            K
        );

    V =
        m32_hmac32_from_states(
            k2States,
            V
        );

    V =
        m32_hmac32_from_states(
            k2States,
            V
        );

    return V;
}


@compute
@workgroup_size(1)
fn photon_m32_rfc_correctness() {
    let nonce =
        input.byteLength;

    let x =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let baseline =
        m27_rfc6979_nonce_optimized(
            messageHash,
            x
        );

    let fixed =
        m32_rfc6979_nonce_fixed(
            messageHash,
            x
        );

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        output.words[i] =
            m32_bytes32_word(
                baseline,
                i * 4u
            );

        output.words[8u + i] =
            m32_bytes32_word(
                fixed,
                i * 4u
            );
    }
}


@compute
@workgroup_size(256)
fn photon_m32_rfc_baseline_bench(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let h =
        m9_message_hash(
            nonce
        );

    let k =
        m27_rfc6979_nonce_optimized(
            h,
            x
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        k[0] ^
        k[7] ^
        k[15] ^
        k[31] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m32_rfc_fixed_bench(
    @builtin(global_invocation_id)
    gid:
        vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let x =
        m17_private_key_bytes();

    let h =
        m9_message_hash(
            nonce
        );

    let k =
        m32_rfc6979_nonce_fixed(
            h,
            x
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        k[0] ^
        k[7] ^
        k[15] ^
        k[31] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}



@compute
@workgroup_size(1)
fn photon_m32_candidate_fixed()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m32_full_fixed(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 33 — post-RFC bottleneck profiler + combined Legendre QR test.
//
// For a Jacobian point (X:Y:Z), affine y = Y/Z^3. For nonzero field values,
// the Legendre symbol is multiplicative and chi(Z)^-3 == chi(Z), so:
//
//   chi(y_affine) = chi(Y) * chi(Z) = chi(Y * Z)
//
// This replaces two binary Legendre computations with one field multiplication
// and one binary Legendre computation.
// ============================================================================

fn bch_jacobian_y_is_qr_combined(
    point:
        JacobianPoint
) -> bool {
    if (
        u256_is_zero(
            point.z
        )
    ) {
        return false;
    }

    let yz =
        field_mul(
            point.y,
            point.z
        );

    return field_legendre_binary(
        yz
    ) == 1;
}


@compute
@workgroup_size(1)
fn photon_m33_candidate_qr_combined()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(64)
fn photon_m33_full_qr_combined_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m33_full_qr_combined_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m33_full_qr_combined_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m33_profile_m32_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m33Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^ messageHash[7] ^
            messageHash[15] ^ messageHash[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^ point.y[3] ^ point.z[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            select(0x13579bdfu, 0x2468ace0u, qr) ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    if (stage == 6u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            challenge[0] ^ challenge[7] ^
            challenge[15] ^ challenge[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 7u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^ finalHash[3] ^ finalHash[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(64)
fn photon_m33_profile_qr_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m33Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^ messageHash[7] ^
            messageHash[15] ^ messageHash[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^ point.y[3] ^ point.z[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            select(0x13579bdfu, 0x2468ace0u, qr) ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    if (stage == 6u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            challenge[0] ^ challenge[7] ^
            challenge[15] ^ challenge[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 7u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^ finalHash[3] ^ finalHash[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(128)
fn photon_m33_profile_qr_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m33Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^ messageHash[7] ^
            messageHash[15] ^ messageHash[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^ point.y[3] ^ point.z[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            select(0x13579bdfu, 0x2468ace0u, qr) ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    if (stage == 6u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            challenge[0] ^ challenge[7] ^
            challenge[15] ^ challenge[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 7u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^ finalHash[3] ^ finalHash[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m33_profile_qr_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let stage =
        m33Control.stage;

    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    if (stage == 1u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            messageHash[0] ^ messageHash[7] ^
            messageHash[15] ^ messageHash[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    if (stage == 2u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            k[0] ^ k[3] ^ k[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let point =
        m29_scalar_mul_g16(
            k
        );

    if (stage == 3u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            point.x[0] ^ point.y[3] ^ point.z[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    if (stage == 4u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            rx[0] ^ rx[3] ^ rx[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let qr =
        bch_jacobian_y_is_qr_combined(
            point
        );

    if (stage == 5u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            select(0x13579bdfu, 0x2468ace0u, qr) ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    if (stage == 6u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            challenge[0] ^ challenge[7] ^
            challenge[15] ^ challenge[31] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (stage == 7u) {
        atomicAdd(
            &benchmarkOutput.checksum,
            s[0] ^ s[3] ^ s[7] ^ nonce
        );
        atomicAdd(&benchmarkOutput.completed, 1u);
        return;
    }

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^ finalHash[3] ^ finalHash[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}



// ============================================================================
// Milestone 34 — XYZZ fixed-base accumulation.
//
// Store the running point as (X, Y, ZZ=Z^2, ZZZ=Z^3). For mixed addition
// with an affine table point Q=(qx,qy):
//
//   U2   = qx * ZZ
//   S2   = qy * ZZZ
//   H    = U2 - X
//   R    = S2 - Y
//   HH   = H^2
//   HHH  = H*HH
//   V    = X*HH
//   X3   = R^2 - HHH - 2V
//   Y3   = R*(V-X3) - Y*HHH
//   ZZ3  = ZZ*HH
//   ZZZ3 = ZZZ*HHH
//
// Compared with the existing Jacobian mixed add, this removes one general
// field square per real addition. The final affine x recovery also becomes:
//
//   x = X / ZZ
//
// instead of X / Z^2, removing another field multiplication.
// ============================================================================

struct M34XYZZPoint {
    x: U256,
    y: U256,
    zz: U256,
    zzz: U256,
};


fn m34_xyzz_infinity() -> M34XYZZPoint {
    var p: M34XYZZPoint;
    p.x = u256_zero();
    p.y = u256_zero();
    p.zz = u256_zero();
    p.zzz = u256_zero();
    return p;
}


fn m34_xyzz_from_affine(
    qx: U256,
    qy: U256
) -> M34XYZZPoint {
    var p: M34XYZZPoint;
    p.x = qx;
    p.y = qy;
    p.zz = u256_one();
    p.zzz = u256_one();
    return p;
}


fn m34_xyzz_double(
    p1: M34XYZZPoint
) -> M34XYZZPoint {
    if (
        u256_is_zero(p1.zz) ||
        u256_is_zero(p1.y)
    ) {
        return m34_xyzz_infinity();
    }

    let YY =
        field_mul(
            p1.y,
            p1.y
        );

    let XYY =
        field_mul(
            p1.x,
            YY
        );

    let twoXYY = field_add(XYY, XYY);
    let S = field_add(twoXYY, twoXYY);

    let XX =
        field_mul(
            p1.x,
            p1.x
        );

    let twoXX = field_add(XX, XX);
    let M = field_add(twoXX, XX);

    let M2 =
        field_mul(
            M,
            M
        );

    let X3 =
        field_sub(
            M2,
            field_add(S, S)
        );

    let YY2 =
        field_mul(
            YY,
            YY
        );

    let twoYY2 = field_add(YY2, YY2);
    let fourYY2 = field_add(twoYY2, twoYY2);
    let eightYY2 = field_add(fourYY2, fourYY2);

    let Y3 =
        field_sub(
            field_mul(
                M,
                field_sub(S, X3)
            ),
            eightYY2
        );

    // ZZ3 = (2*Y*Z)^2 = 4*Y^2*ZZ
    let YYZZ =
        field_mul(
            YY,
            p1.zz
        );
    let twoYYZZ = field_add(YYZZ, YYZZ);
    let ZZ3 = field_add(twoYYZZ, twoYYZZ);

    // ZZZ3 = (2*Y*Z)^3 = 8*Y^3*ZZZ
    let YYY =
        field_mul(
            p1.y,
            YY
        );
    let YYYZZZ =
        field_mul(
            YYY,
            p1.zzz
        );
    let twoYYYZZZ = field_add(YYYZZZ, YYYZZZ);
    let fourYYYZZZ = field_add(twoYYYZZZ, twoYYYZZZ);
    let ZZZ3 = field_add(fourYYYZZZ, fourYYYZZZ);

    var result: M34XYZZPoint;
    result.x = X3;
    result.y = Y3;
    result.zz = ZZ3;
    result.zzz = ZZZ3;
    return result;
}


fn m34_xyzz_add_affine_noninfinity(
    p1: M34XYZZPoint,
    qx: U256,
    qy: U256
) -> M34XYZZPoint {
    let U2 =
        field_mul(
            qx,
            p1.zz
        );

    let S2 =
        field_mul(
            qy,
            p1.zzz
        );

    let H = field_sub(U2, p1.x);
    let R = field_sub(S2, p1.y);

    if (u256_is_zero(H)) {
        if (u256_is_zero(R)) {
            return m34_xyzz_double(p1);
        }
        return m34_xyzz_infinity();
    }

    let HH =
        field_mul(
            H,
            H
        );

    let HHH =
        field_mul(
            H,
            HH
        );

    let V =
        field_mul(
            p1.x,
            HH
        );

    let R2 =
        field_mul(
            R,
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(V, V)
        );

    let Y3 =
        field_sub(
            field_mul(
                R,
                field_sub(V, X3)
            ),
            field_mul(
                p1.y,
                HHH
            )
        );

    let ZZ3 =
        field_mul(
            p1.zz,
            HH
        );

    let ZZZ3 =
        field_mul(
            p1.zzz,
            HHH
        );

    var result: M34XYZZPoint;
    result.x = X3;
    result.y = Y3;
    result.zz = ZZ3;
    result.zzz = ZZZ3;
    return result;
}


fn m34_xyzz_y_is_qr(
    point: M34XYZZPoint
) -> bool {
    let ySymbol =
        field_legendre_binary(
            point.y
        );

    let zzzSymbol =
        field_legendre_binary(
            point.zzz
        );

    return (
        ySymbol != 0 &&
        zzzSymbol != 0 &&
        ySymbol == zzzSymbol
    );
}


fn m34_scalar_mul_g16_xyzz(
    scalar: U256
) -> M34XYZZPoint {
    var result =
        m34_xyzz_infinity();

    var hasPoint =
        false;

    // Width is exactly 16 bits, aligned to the U256's eight 32-bit limbs.
    // Avoid the generic divide/mod/cross-limb window extractor.
    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        let lowDigit =
            scalar[limb] &
            0xffffu;

        if (lowDigit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    limb * 2u,
                    lowDigit
                );

            if (!hasPoint) {
                result =
                    m34_xyzz_from_affine(
                        q.x,
                        q.y
                    );
                hasPoint = true;
            } else {
                result =
                    m34_xyzz_add_affine_noninfinity(
                        result,
                        q.x,
                        q.y
                    );
            }
        }

        let highDigit =
            scalar[limb] >>
            16u;

        if (highDigit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    limb * 2u + 1u,
                    highDigit
                );

            if (!hasPoint) {
                result =
                    m34_xyzz_from_affine(
                        q.x,
                        q.y
                    );
                hasPoint = true;
            } else {
                result =
                    m34_xyzz_add_affine_noninfinity(
                        result,
                        q.x,
                        q.y
                    );
            }
        }
    }

    return result;
}


fn m34_bench_scalar(
    nonce: u32
) -> U256 {
    // Cheap deterministic expansion for kG-only timing. This is not crypto;
    // it merely gives all 16 table windows nontrivial, changing digits.
    var k: U256;
    k[0] = nonce * 0x9e3779b9u + 0x243f6a89u;
    k[1] = (nonce ^ 0xa5a5a5a5u) * 0x85ebca6bu + 0x13198a2eu;
    k[2] = (nonce + 0x7f4a7c15u) * 0xc2b2ae35u + 0x03707344u;
    k[3] = (nonce ^ 0x3c6ef372u) * 0x27d4eb2du + 0xa4093822u;
    k[4] = (nonce + 0xbb67ae85u) * 0x165667b1u + 0x299f31d0u;
    k[5] = (nonce ^ 0x510e527fu) * 0xd3a2646cu + 0x082efa98u;
    k[6] = (nonce + 0x9b05688cu) * 0xfd7046c5u + 0xec4e6c89u;
    k[7] = (nonce ^ 0x1f83d9abu) * 0xb55a4f09u + 0x452821e6u;
    return k;
}


@compute
@workgroup_size(256)
fn photon_m34_kg_baseline(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce = input.byteLength + gid.x;
    let k = m34_bench_scalar(nonce);
    let p = m29_scalar_mul_g16(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m34_kg_xyzz(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce = input.byteLength + gid.x;
    let k = m34_bench_scalar(nonce);
    let p = m34_scalar_mul_g16_xyzz(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.zz[5] ^ p.zzz[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(1)
fn photon_m34_candidate_xyzz()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m34_scalar_mul_g16_xyzz(
            k
        );

    let zzInv =
        field_inv_binary(
            point.zz
        );

    let rx =
        field_mul(
            point.x,
            zzInv
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        m34_xyzz_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m34_full_xyzz(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m34_scalar_mul_g16_xyzz(
            k
        );

    let zzInv =
        field_inv_binary(
            point.zz
        );

    let rx =
        field_mul(
            point.x,
            zzInv
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        m34_xyzz_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 35 — direct unsigned-16 Jacobian and balanced signed-16 half-table.
//
// Balanced base-2^16 recoding uses digits in [-32767, 32768] (plus a final
// carry point at 2^256 G), so each window needs only magnitudes 0..32768.
// This cuts the generator table from 64 MiB to ~32 MiB without increasing the
// ordinary number of mixed Jacobian-affine additions.
// ============================================================================

fn m35_signed_table_point(
    windowIndex: u32,
    magnitude: u32
) -> JacobianPoint {
    let base =
        (
            windowIndex *
            32769u +
            magnitude
        ) *
        16u;

    var p: JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        p.x[i] =
            m35SignedTable[
                base + i
            ];

        p.y[i] =
            m35SignedTable[
                base + 8u + i
            ];
    }

    p.z = u256_one();
    return p;
}


fn m35_top_carry_x() -> U256 {
    return array<u32, 8>(
        0xeb9a9787u,
        0x92f76cc4u,
        0x59599680u,
        0x89bdde81u,
        0xbbd3788du,
        0x74669716u,
        0xef5ba060u,
        0xdd3625fau
    );
}


fn m35_top_carry_y() -> U256 {
    return array<u32, 8>(
        0xc644a573u,
        0x37f68d00u,
        0x28833959u,
        0x94146198u,
        0x045731cau,
        0x61da2501u,
        0x520e30d4u,
        0x7a188fa3u
    );
}


fn m35_negate_field_y(
    y: U256
) -> U256 {
    if (u256_is_zero(y)) {
        return y;
    }

    return u256_sub_raw(
        field_p(),
        y
    );
}


fn m35_scalar_mul_g16_direct(
    scalar: U256
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    var hasPoint =
        false;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        let lowDigit =
            scalar[limb] &
            0xffffu;

        if (lowDigit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    limb * 2u,
                    lowDigit
                );

            if (!hasPoint) {
                result = q;
                hasPoint = true;
            } else {
                result =
                    jacobian_add_affine(
                        result,
                        q.x,
                        q.y
                    );
            }
        }

        let highDigit =
            scalar[limb] >>
            16u;

        if (highDigit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    limb * 2u + 1u,
                    highDigit
                );

            if (!hasPoint) {
                result = q;
                hasPoint = true;
            } else {
                result =
                    jacobian_add_affine(
                        result,
                        q.x,
                        q.y
                    );
            }
        }
    }

    return result;
}


fn m35_scalar_mul_g16_signed(
    scalar: U256
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    var hasPoint =
        false;

    var carry =
        0u;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        var rawLow =
            (scalar[limb] & 0xffffu) +
            carry;

        var lowMagnitude =
            rawLow;

        var lowNegative =
            false;

        if (rawLow > 32768u) {
            lowMagnitude =
                65536u -
                rawLow;
            lowNegative =
                true;
            carry =
                1u;
        } else {
            carry =
                0u;
        }

        if (lowMagnitude != 0u) {
            let q =
                m35_signed_table_point(
                    limb * 2u,
                    lowMagnitude
                );

            var qy =
                q.y;

            if (lowNegative) {
                qy =
                    m35_negate_field_y(
                        qy
                    );
            }

            if (!hasPoint) {
                result.x = q.x;
                result.y = qy;
                result.z = u256_one();
                hasPoint = true;
            } else {
                result =
                    jacobian_add_affine(
                        result,
                        q.x,
                        qy
                    );
            }
        }

        var rawHigh =
            (scalar[limb] >> 16u) +
            carry;

        var highMagnitude =
            rawHigh;

        var highNegative =
            false;

        if (rawHigh > 32768u) {
            highMagnitude =
                65536u -
                rawHigh;
            highNegative =
                true;
            carry =
                1u;
        } else {
            carry =
                0u;
        }

        if (highMagnitude != 0u) {
            let q =
                m35_signed_table_point(
                    limb * 2u + 1u,
                    highMagnitude
                );

            var qy =
                q.y;

            if (highNegative) {
                qy =
                    m35_negate_field_y(
                        qy
                    );
            }

            if (!hasPoint) {
                result.x = q.x;
                result.y = qy;
                result.z = u256_one();
                hasPoint = true;
            } else {
                result =
                    jacobian_add_affine(
                        result,
                        q.x,
                        qy
                    );
            }
        }
    }

    // Balanced recoding can leave a carry of +1 at radix position 16,
    // representing exactly 2^256 * G.
    if (carry != 0u) {
        let qx =
            m35_top_carry_x();

        let qy =
            m35_top_carry_y();

        if (!hasPoint) {
            result.x = qx;
            result.y = qy;
            result.z = u256_one();
        } else {
            result =
                jacobian_add_affine(
                    result,
                    qx,
                    qy
                );
        }
    }

    return result;
}


fn m35_bench_scalar(
    nonce: u32
) -> U256 {
    var k: U256;
    k[0] = nonce * 0x9e3779b9u + 0x243f6a89u;
    k[1] = (nonce ^ 0xa5a5a5a5u) * 0x85ebca6bu + 0x13198a2eu;
    k[2] = (nonce + 0x7f4a7c15u) * 0xc2b2ae35u + 0x03707344u;
    k[3] = (nonce ^ 0x3c6ef372u) * 0x27d4eb2du + 0xa4093822u;
    k[4] = (nonce + 0xbb67ae85u) * 0x165667b1u + 0x299f31d0u;
    k[5] = (nonce ^ 0x510e527fu) * 0xd3a2646cu + 0x082efa98u;
    k[6] = (nonce + 0x9b05688cu) * 0xfd7046c5u + 0xec4e6c89u;
    k[7] = (nonce ^ 0x1f83d9abu) * 0xb55a4f09u + 0x452821e6u;
    return k;
}


@compute
@workgroup_size(256)
fn photon_m35_kg_baseline(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce = input.byteLength + gid.x;
    let k = m35_bench_scalar(nonce);
    let p = m29_scalar_mul_g16(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m35_kg_direct(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce = input.byteLength + gid.x;
    let k = m35_bench_scalar(nonce);
    let p = m35_scalar_mul_g16_direct(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m35_kg_signed(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce = input.byteLength + gid.x;
    let k = m35_bench_scalar(nonce);
    let p = m35_scalar_mul_g16_signed(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}



@compute
@workgroup_size(1)
fn photon_m35_candidate_direct()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m35_scalar_mul_g16_direct(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m35_candidate_signed()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m35_scalar_mul_g16_signed(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m35_full_direct(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m35_scalar_mul_g16_direct(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m35_full_signed(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m35_scalar_mul_g16_signed(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 36 — field arithmetic bake-off.
//
// M33 showed fixed-base kG at ~73% of runtime. M34/M35 showed that larger
// per-thread state or rewritten scalar-control flow can badly hurt occupancy,
// so M36 keeps M32's exact scalar/window/table structure and changes only
// field arithmetic.
//
// Variant 1: same radix-2^16 field_mul with 2 pseudo-Mersenne reduction passes
//            instead of 3.
// Variant 2: same two-pass multiplier plus symmetric field_square:
//            16 diagonal products + 120 off-diagonal products = 136 products,
//            versus 256 products for general 16x16 multiplication.
// ============================================================================

fn m36_field_mul_2pass(
    a: U256,
    b: U256
) -> U256 {
    // Milestone 11 fast field multiplication.
    //
    // Radix B = 2^16. Each U256 becomes 16 base-B digits. 16x16 products
    // fit safely in WGSL u32 because:
    //
    //   0xffff * 0xffff + 0xffff + 0xffff = 0xffffffff
    //
    // secp256k1:
    //
    //   p = 2^256 - 2^32 - 977
    //
    // therefore:
    //
    //   B^16 = 2^256 = B^2 + 977 (mod p)
    //
    // High radix digits are folded with this pseudo-Mersenne identity.

    var a16:
        array<u32, 16>;

    var b16:
        array<u32, 16>;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        a16[limb * 2u] =
            a[limb] &
            0xffffu;

        a16[limb * 2u + 1u] =
            a[limb] >>
            16u;

        b16[limb * 2u] =
            b[limb] &
            0xffffu;

        b16[limb * 2u + 1u] =
            b[limb] >>
            16u;
    }

    var product:
        array<u32, 34>;

    for (
        var i: u32 = 0u;
        i < 34u;
        i = i + 1u
    ) {
        product[i] =
            0u;
    }

    // Exact 512-bit schoolbook product, maintained in normalized radix-B
    // digits after each row.
    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        var carry:
            u32 =
            0u;

        for (
            var j: u32 = 0u;
            j < 16u;
            j = j + 1u
        ) {
            let index =
                i + j;

            let uv =
                product[index] +
                a16[i] *
                b16[j] +
                carry;

            product[index] =
                uv &
                0xffffu;

            carry =
                uv >>
                16u;
        }

        var propagate =
            carry;

        var q =
            i +
            16u;

        loop {
            if (
                propagate == 0u ||
                q >= 34u
            ) {
                break;
            }

            let sum =
                product[q] +
                propagate;

            product[q] =
                sum &
                0xffffu;

            propagate =
                sum >>
                16u;

            q =
                q +
                1u;
        }
    }

    // Fold all digits B^16 and above. Three fixed passes are more than enough
    // for the carry generated by the pseudo-Mersenne fold and keep control flow
    // deterministic across vendors.
    for (
        var reducePass: u32 = 0u;
        reducePass < 2u;
        reducePass = reducePass + 1u
    ) {
        var k: i32 =
            33;

        loop {
            if (k < 16) {
                break;
            }

            let ku =
                u32(k);

            let high =
                product[ku];

            product[ku] =
                0u;

            product[ku - 16u] =
                product[ku - 16u] +
                high *
                977u;

            product[ku - 14u] =
                product[ku - 14u] +
                high;

            k =
                k -
                1;
        }

        var carry:
            u32 =
            0u;

        for (
            var i: u32 = 0u;
            i < 34u;
            i = i + 1u
        ) {
            let value =
                product[i] +
                carry;

            product[i] =
                value &
                0xffffu;

            carry =
                value >>
                16u;
        }
    }

    var result:
        U256;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        result[limb] =
            product[limb * 2u] |
            (
                product[limb * 2u + 1u] <<
                16u
            );
    }

    return normalize_field(
        result
    );
}



fn m36_field_square_2pass(
    a: U256
) -> U256 {
    var a16:
        array<u32, 16>;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        a16[limb * 2u] =
            a[limb] &
            0xffffu;

        a16[limb * 2u + 1u] =
            a[limb] >>
            16u;
    }

    var product:
        array<u32, 34>;

    for (
        var i: u32 = 0u;
        i < 34u;
        i = i + 1u
    ) {
        product[i] =
            0u;
    }

    // Add diagonal and doubled off-diagonal products while keeping each radix
    // digit normalized to 16 bits. This avoids needing u64 arithmetic in WGSL.
    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        let ai =
            a16[i];

        // Diagonal ai^2.
        let diagonal =
            ai *
            ai;

        let diagonalLow =
            diagonal &
            0xffffu;

        let diagonalHigh =
            diagonal >>
            16u;

        var index =
            i *
            2u;

        var sum =
            product[index] +
            diagonalLow;

        product[index] =
            sum &
            0xffffu;

        var carry =
            (sum >> 16u) +
            diagonalHigh;

        var q =
            index +
            1u;

        loop {
            if (
                carry == 0u ||
                q >= 34u
            ) {
                break;
            }

            sum =
                product[q] +
                carry;

            product[q] =
                sum &
                0xffffu;

            carry =
                sum >>
                16u;

            q =
                q +
                1u;
        }

        // Off-diagonal terms occur twice.
        for (
            var j: u32 = i + 1u;
            j < 16u;
            j = j + 1u
        ) {
            let p =
                ai *
                a16[j];

            let pLow =
                p &
                0xffffu;

            let pHigh =
                p >>
                16u;

            let doubledLow =
                pLow <<
                1u;

            let lowDigit =
                doubledLow &
                0xffffu;

            let highAdd =
                (pHigh << 1u) +
                (doubledLow >> 16u);

            index =
                i +
                j;

            sum =
                product[index] +
                lowDigit;

            product[index] =
                sum &
                0xffffu;

            carry =
                (sum >> 16u) +
                highAdd;

            q =
                index +
                1u;

            loop {
                if (
                    carry == 0u ||
                    q >= 34u
                ) {
                    break;
                }

                sum =
                    product[q] +
                    carry;

                product[q] =
                    sum &
                    0xffffu;

                carry =
                    sum >>
                    16u;

                q =
                    q +
                    1u;
            }
        }
    }

    // Same secp256k1 pseudo-Mersenne fold as field_mul, but only two passes.
    for (
        var reducePass: u32 = 0u;
        reducePass < 2u;
        reducePass = reducePass + 1u
    ) {
        var k: i32 =
            33;

        loop {
            if (k < 16) {
                break;
            }

            let ku =
                u32(k);

            let high =
                product[ku];

            product[ku] =
                0u;

            product[ku - 16u] =
                product[ku - 16u] +
                high *
                977u;

            product[ku - 14u] =
                product[ku - 14u] +
                high;

            k =
                k -
                1;
        }

        var carry:
            u32 =
            0u;

        for (
            var i: u32 = 0u;
            i < 34u;
            i = i + 1u
        ) {
            let value =
                product[i] +
                carry;

            product[i] =
                value &
                0xffffu;

            carry =
                value >>
                16u;
        }
    }

    var result:
        U256;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        result[limb] =
            product[limb * 2u] |
            (
                product[limb * 2u + 1u] <<
                16u
            );
    }

    return normalize_field(
        result
    );
}


fn m36_jacobian_add_affine_2pass(
    p1: JacobianPoint,
    qx: U256,
    qy: U256
) -> JacobianPoint {
    if (u256_is_zero(p1.z)) {
        var q: JacobianPoint;
        q.x = qx;
        q.y = qy;
        q.z = u256_one();
        return q;
    }

    let Z1Z1 =
        m36_field_mul_2pass(
            p1.z,
            p1.z
        );

    let U2 =
        m36_field_mul_2pass(
            qx,
            Z1Z1
        );

    let Z1Cubed =
        m36_field_mul_2pass(
            p1.z,
            Z1Z1
        );

    let S2 =
        m36_field_mul_2pass(
            qy,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            p1.x
        );

    let R =
        field_sub(
            S2,
            p1.y
        );

    if (u256_is_zero(H)) {
        if (u256_is_zero(R)) {
            return jacobian_double(p1);
        }

        return jacobian_infinity();
    }

    let HH =
        m36_field_mul_2pass(
            H,
            H
        );

    let HHH =
        m36_field_mul_2pass(
            H,
            HH
        );

    let V =
        m36_field_mul_2pass(
            p1.x,
            HH
        );

    let R2 =
        m36_field_mul_2pass(
            R,
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(V, V)
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                R,
                field_sub(V, X3)
            ),
            m36_field_mul_2pass(
                p1.y,
                HHH
            )
        );

    let Z3 =
        m36_field_mul_2pass(
            p1.z,
            H
        );

    var result: JacobianPoint;
    result.x = X3;
    result.y = Y3;
    result.z = Z3;

    return result;
}


fn m36_jacobian_add_affine_square(
    p1: JacobianPoint,
    qx: U256,
    qy: U256
) -> JacobianPoint {
    if (u256_is_zero(p1.z)) {
        var q: JacobianPoint;
        q.x = qx;
        q.y = qy;
        q.z = u256_one();
        return q;
    }

    let Z1Z1 =
        m36_field_square_2pass(
            p1.z
        );

    let U2 =
        m36_field_mul_2pass(
            qx,
            Z1Z1
        );

    let Z1Cubed =
        m36_field_mul_2pass(
            p1.z,
            Z1Z1
        );

    let S2 =
        m36_field_mul_2pass(
            qy,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            p1.x
        );

    let R =
        field_sub(
            S2,
            p1.y
        );

    if (u256_is_zero(H)) {
        if (u256_is_zero(R)) {
            return jacobian_double(p1);
        }

        return jacobian_infinity();
    }

    let HH =
        m36_field_square_2pass(
            H
        );

    let HHH =
        m36_field_mul_2pass(
            H,
            HH
        );

    let V =
        m36_field_mul_2pass(
            p1.x,
            HH
        );

    let R2 =
        m36_field_square_2pass(
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(V, V)
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                R,
                field_sub(V, X3)
            ),
            m36_field_mul_2pass(
                p1.y,
                HHH
            )
        );

    let Z3 =
        m36_field_mul_2pass(
            p1.z,
            H
        );

    var result: JacobianPoint;
    result.x = X3;
    result.y = Y3;
    result.z = Z3;

    return result;
}


fn m36_scalar_mul_g16_2pass(
    scalar: U256
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < 16u;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex * 16u,
                16u
            );

        if (digit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    windowIndex,
                    digit
                );

            result =
                m36_jacobian_add_affine_2pass(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m36_scalar_mul_g16_square(
    scalar: U256
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < 16u;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex * 16u,
                16u
            );

        if (digit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    windowIndex,
                    digit
                );

            result =
                m36_jacobian_add_affine_square(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m36_fuzz_value(
    seed: u32
) -> U256 {
    return normalize_field(
        m35_bench_scalar(
            seed
        )
    );
}


@compute
@workgroup_size(256)
fn photon_m36_field_fuzz(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed =
        input.byteLength +
        gid.x;

    let a =
        m36_fuzz_value(
            seed
        );

    let b =
        m36_fuzz_value(
            seed ^
            0x6a09e667u
        );

    let baselineMul =
        field_mul(
            a,
            b
        );

    let twoPassMul =
        m36_field_mul_2pass(
            a,
            b
        );

    let baselineSquare =
        field_mul(
            a,
            a
        );

    let specializedSquare =
        m36_field_square_2pass(
            a
        );

    var mismatch =
        0u;

    if (!u256_equal(baselineMul, twoPassMul)) {
        mismatch = mismatch + 1u;
    }

    if (!u256_equal(baselineSquare, specializedSquare)) {
        mismatch = mismatch + 1u;
    }

    if (mismatch != 0u) {
        atomicAdd(
            &benchmarkOutput.winners,
            mismatch
        );
    }

    atomicAdd(
        &benchmarkOutput.checksum,
        baselineMul[0] ^
        twoPassMul[3] ^
        baselineSquare[5] ^
        specializedSquare[7] ^
        seed
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m36_mul_baseline_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let a = m36_fuzz_value(seed);
    let b = m36_fuzz_value(seed ^ 0xbb67ae85u);
    let r = field_mul(a, b);

    atomicAdd(
        &benchmarkOutput.checksum,
        r[0] ^ r[3] ^ r[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m36_mul_2pass_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let a = m36_fuzz_value(seed);
    let b = m36_fuzz_value(seed ^ 0xbb67ae85u);
    let r = m36_field_mul_2pass(a, b);

    atomicAdd(
        &benchmarkOutput.checksum,
        r[0] ^ r[3] ^ r[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m36_square_baseline_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let a = m36_fuzz_value(seed);
    let r = field_mul(a, a);

    atomicAdd(
        &benchmarkOutput.checksum,
        r[0] ^ r[3] ^ r[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m36_square_special_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let a = m36_fuzz_value(seed);
    let r = m36_field_square_2pass(a);

    atomicAdd(
        &benchmarkOutput.checksum,
        r[0] ^ r[3] ^ r[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m36_kg_baseline(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let k = m35_bench_scalar(seed);
    let p = m29_scalar_mul_g16(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m36_kg_2pass(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let k = m35_bench_scalar(seed);
    let p = m36_scalar_mul_g16_2pass(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}


@compute
@workgroup_size(256)
fn photon_m36_kg_square(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed = input.byteLength + gid.x;
    let k = m35_bench_scalar(seed);
    let p = m36_scalar_mul_g16_square(k);

    atomicAdd(
        &benchmarkOutput.checksum,
        p.x[0] ^ p.y[3] ^ p.z[7] ^ seed
    );
    atomicAdd(&benchmarkOutput.completed, 1u);
}



@compute
@workgroup_size(1)
fn photon_m36_candidate_2pass()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(1)
fn photon_m36_candidate_square()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    if (selector == 2u) {
        let publicPoint =
            m17_derive_public_point(
                d
            );

        let px =
            publicPoint[0];

        let py =
            publicPoint[1];

        output.words[0] =
            select(
                2u,
                3u,
                (py[0] & 1u) != 0u
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[1u + i] =
                px[7u - i];
        }

        for (
            var i: u32 = 9u;
            i < 16u;
            i = i + 1u
        ) {
            output.words[i] =
                0u;
        }

        return;
    }

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m36_scalar_mul_g16_square(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }

        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }

        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] =
            0u;
    }
}


@compute
@workgroup_size(256)
fn photon_m36_full_2pass(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m36_full_square(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let point =
        m36_scalar_mul_g16_square(
            k
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 37 — split-kernel PHOTON pipeline.
//
// M36 showed the symmetric square helps isolated kG but catastrophically hurts
// the monolithic full kernel. This strongly suggests register pressure/spills.
// Split the work into:
//
//   Stage A: message SHA + fixed RFC6979 -> packed messageHash + k
//   Stage B: kG only -> Jacobian X/Y/Z
//   Stage C: affine x + QR + Schnorr tail + prefixed HASH256
//
// Intermediate record = 40 u32 = 160 bytes/candidate:
//   0..7   messageHash packed big-endian words
//   8..15  k U256 limbs
//   16..23 X
//   24..31 Y
//   32..39 Z
// ============================================================================

const M37_RECORD_WORDS: u32 = 40u;


fn m37_base(index: u32) -> u32 {
    return index * M37_RECORD_WORDS;
}


fn m37_store_bytes32_packed(
    base: u32,
    value: Bytes32
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        m37Intermediate[base + i] =
            m32_bytes32_word(
                value,
                i * 4u
            );
    }
}


fn m37_load_bytes32_packed(
    base: u32
) -> Bytes32 {
    var value: Bytes32;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        let word =
            m37Intermediate[base + i];

        value[i * 4u] =
            (word >> 24u) & 0xffu;
        value[i * 4u + 1u] =
            (word >> 16u) & 0xffu;
        value[i * 4u + 2u] =
            (word >> 8u) & 0xffu;
        value[i * 4u + 3u] =
            word & 0xffu;
    }

    return value;
}


fn m37_store_u256(
    base: u32,
    value: U256
) {
    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        m37Intermediate[base + i] =
            value[i];
    }
}


fn m37_load_u256(
    base: u32
) -> U256 {
    var value: U256;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        value[i] =
            m37Intermediate[base + i];
    }

    return value;
}


@compute
@workgroup_size(256)
fn photon_m37_stage_a(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}


@compute
@workgroup_size(256)
fn photon_m37_stage_b_baseline(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m29_scalar_mul_g16(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(256)
fn photon_m37_stage_b_2pass(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(256)
fn photon_m37_stage_b_square(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m36_scalar_mul_g16_square(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


fn m37_load_point(index: u32) -> JacobianPoint {
    let base =
        m37_base(index);

    var point: JacobianPoint;
    point.x = m37_load_u256(base + 16u);
    point.y = m37_load_u256(base + 24u);
    point.z = m37_load_u256(base + 32u);
    return point;
}


@compute
@workgroup_size(256)
fn photon_m37_stage_c_full(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(1)
fn photon_m37_stage_c_candidate() {
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let messageHash =
        m37_load_bytes32_packed(
            0u
        );

    let k =
        m37_load_u256(
            8u
        );

    let point =
        m37_load_point(
            0u
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }
        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }
        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] = 0u;
    }
}



// ============================================================================
// Milestone 38 — split-stage tuning.
//
// M37 established that splitting the monolithic miner removes major register
// pressure. M38:
//   * tunes workgroup size independently for A, B, and C,
//   * re-tests 15-bit vs 16-bit fixed-base tables inside isolated Stage B,
//   * tests a two-stage A + (B+C) pipeline to reduce intermediate traffic,
//   * pre-warms every timed pipeline before measurement.
// ============================================================================

fn m38_scalar_mul_window_2pass(
    scalar: U256,
    width: u32,
    windows: u32,
    entries: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < windows;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex * width,
                width
            );

        if (digit != 0u) {
            let q =
                m29_table_point(
                    entries,
                    windowIndex,
                    digit
                );

            result =
                m36_jacobian_add_affine_2pass(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m38_scalar_mul_g15_2pass(
    scalar: U256
) -> JacobianPoint {
    return m38_scalar_mul_window_2pass(
        scalar,
        15u,
        18u,
        32768u
    );
}


fn m38_scalar_mul_g16_2pass(
    scalar: U256
) -> JacobianPoint {
    return m38_scalar_mul_window_2pass(
        scalar,
        16u,
        16u,
        65536u
    );
}



@compute
@workgroup_size(8)
fn photon_m60_stage_a_wg8(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}



@compute
@workgroup_size(16)
fn photon_m60_stage_a_wg16(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}



@compute
@workgroup_size(32)
fn photon_m60_stage_a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}


@compute
@workgroup_size(64)
fn photon_m38_stage_a_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}


@compute
@workgroup_size(128)
fn photon_m38_stage_a_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}


@compute
@workgroup_size(256)
fn photon_m38_stage_a_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}


@compute
@workgroup_size(1)
fn photon_m38_stage_a_candidate(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let privateKeyBytes =
        m17_private_key_bytes();

    let messageHash =
        m9_message_hash(
            nonce
        );

    let kBytes =
        m32_rfc6979_nonce_fixed(
            messageHash,
            privateKeyBytes
        );

    let k =
        u256_from_be_bytes32(
            kBytes
        );

    let base =
        m37_base(gid.x);

    m37_store_bytes32_packed(
        base,
        messageHash
    );

    m37_store_u256(
        base + 8u,
        k
    );
}


@compute
@workgroup_size(64)
fn photon_m38_stage_b15_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(128)
fn photon_m38_stage_b15_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(256)
fn photon_m38_stage_b15_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(1)
fn photon_m38_stage_b15_candidate(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(64)
fn photon_m38_stage_b16_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(128)
fn photon_m38_stage_b16_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(256)
fn photon_m38_stage_b16_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(1)
fn photon_m38_stage_b16_candidate(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(gid.x);

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(base + 16u, point.x);
    m37_store_u256(base + 24u, point.y);
    m37_store_u256(base + 32u, point.z);
}


@compute
@workgroup_size(64)
fn photon_m38_stage_c_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m38_stage_c_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m38_stage_c_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(64)
fn photon_m38_stage_bc15_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m38_stage_bc15_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m38_stage_bc15_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            k
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(64)
fn photon_m38_stage_bc16_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m38_stage_bc16_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(256)
fn photon_m38_stage_bc16_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m38_scalar_mul_g16_2pass(
            k
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}



// ============================================================================
// Milestone 39 — fixed-private-key Stage C.
//
// The Schnorr tail computes e*d mod n for every candidate, while d is fixed
// for the entire mining session. Precompute:
//
//   T[bytePosition][digit] = d * digit * 256^bytePosition mod n
//
// 32 * 256 * 32 bytes = 262,144 bytes. Then e*d is 32 table lookups plus
// modular additions instead of a 256-bit double-and-add multiplication.
// ============================================================================

fn m39_fixed_d_table_value(
    bytePosition: u32,
    digit: u32
) -> U256 {
    let base =
        (
            bytePosition *
            256u +
            digit
        ) *
        8u;

    var value: U256;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        value[i] =
            m39FixedDTable[
                base + i
            ];
    }

    return value;
}


fn m39_scalar_mul_fixed_d(
    e: U256
) -> U256 {
    var result =
        u256_zero();

    for (
        var bytePosition: u32 = 0u;
        bytePosition < 32u;
        bytePosition = bytePosition + 1u
    ) {
        let limb =
            bytePosition >>
            2u;

        let shift =
            (
                bytePosition &
                3u
            ) *
            8u;

        let digit =
            (
                e[limb] >>
                shift
            ) &
            0xffu;

        if (digit != 0u) {
            result =
                scalar_add_mod_n(
                    result,
                    m39_fixed_d_table_value(
                        bytePosition,
                        digit
                    )
                );
        }
    }

    return result;
}


fn m39_test_e(
    seed: u32
) -> U256 {
    return m9_scalar_reduce_n(
        m35_bench_scalar(
            seed
        )
    );
}


@compute
@workgroup_size(256)
fn photon_m39_fixed_d_fuzz(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed =
        input.byteLength +
        gid.x;

    let e =
        m39_test_e(
            seed
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let baseline =
        scalar_mul_mod_n(
            e,
            d
        );

    let fixed =
        m39_scalar_mul_fixed_d(
            e
        );

    if (!u256_equal(baseline, fixed)) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );
    }

    atomicAdd(
        &benchmarkOutput.checksum,
        baseline[0] ^
        baseline[3] ^
        fixed[5] ^
        fixed[7] ^
        seed
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m39_scalar_baseline_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed =
        input.byteLength +
        gid.x;

    let e =
        m39_test_e(seed);

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let value =
        scalar_mul_mod_n(
            e,
            d
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        value[0] ^
        value[3] ^
        value[7] ^
        seed
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(256)
fn photon_m39_scalar_fixed_bench(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed =
        input.byteLength +
        gid.x;

    let e =
        m39_test_e(seed);

    let value =
        m39_scalar_mul_fixed_d(
            e
        );

    atomicAdd(
        &benchmarkOutput.checksum,
        value[0] ^
        value[3] ^
        value[7] ^
        seed
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(32)
fn photon_m39_stage_c_baseline_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(32)
fn photon_m39_stage_c_fixed_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(32)
fn photon_m39_stage_c_fixed_2pass_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        m36_field_mul_2pass(
            zInv,
            zInv
        );

    let rx =
        m36_field_mul_2pass(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(64)
fn photon_m39_stage_c_baseline_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(64)
fn photon_m39_stage_c_fixed_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(64)
fn photon_m39_stage_c_fixed_2pass_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        m36_field_mul_2pass(
            zInv,
            zInv
        );

    let rx =
        m36_field_mul_2pass(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m39_stage_c_baseline_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let privateKeyBytes =
        m17_private_key_bytes();

    let d =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                privateKeyBytes
            )
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        scalar_mul_mod_n(
            e,
            d
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m39_stage_c_fixed_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(128)
fn photon_m39_stage_c_fixed_2pass_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
)
{
    let nonce =
        input.byteLength +
        gid.x;

    let easyTargetMode =
        (
            input.words[153] &
            1u
        ) != 0u;

    let base =
        m37_base(gid.x);

    let messageHash =
        m37_load_bytes32_packed(
            base
        );

    let k =
        m37_load_u256(
            base + 8u
        );

    let point =
        m37_load_point(
            gid.x
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        m36_field_mul_2pass(
            zInv,
            zInv
        );

    let rx =
        m36_field_mul_2pass(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    let hashes =
        m30_transaction_hashes_prefixed(
            nonce,
            rBytes,
            sBytes
        );

    let finalHash =
        hashes[1];

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^
        finalHash[3] ^
        finalHash[7] ^
        nonce
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );

    let isWinner =
        select(
            m10_hash_is_below_target(
                finalHash
            ),
            m16_hash_is_nonzero(
                finalHash
            ),
            easyTargetMode
        );

    if (isWinner) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );

        atomicExchange(
            &benchmarkOutput.firstWinnerNoncePlusOne,
            nonce + 1u
        );
    }
}


@compute
@workgroup_size(1)
fn photon_m39_stage_c_fixed_candidate()
{
    let nonce =
        input.byteLength;

    let selector =
        input.words[153] &
        0xffu;

    let messageHash =
        m37_load_bytes32_packed(
            0u
        );

    let k =
        m37_load_u256(
            8u
        );

    let point =
        m37_load_point(
            0u
        );

    let zInv =
        field_inv_binary(
            point.z
        );

    let zInv2 =
        field_mul(
            zInv,
            zInv
        );

    let rx =
        field_mul(
            point.x,
            zInv2
        );

    let rBytes =
        u256_to_be_bytes32(
            rx
        );

    let qr =
        bch_jacobian_y_is_qr(
            point
        );

    var adjustedK =
        k;

    if (!qr) {
        adjustedK =
            u256_sub_raw(
                scalar_order_n(),
                k
            );
    }

    let challenge =
        m9_challenge_hash(
            rBytes,
            messageHash
        );

    let e =
        m9_scalar_reduce_n(
            u256_from_be_bytes32(
                challenge
            )
        );

    let ed =
        m39_scalar_mul_fixed_d(
            e
        );

    let s =
        scalar_add_mod_n(
            adjustedK,
            ed
        );

    let sBytes =
        u256_to_be_bytes32(
            s
        );

    if (selector == 0u) {
        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                rx[7u - i];

            output.words[8u + i] =
                s[7u - i];
        }
        return;
    }

    if (selector == 1u) {
        let hashes =
            m30_transaction_hashes_prefixed(
                nonce,
                rBytes,
                sBytes
            );

        for (
            var i: u32 = 0u;
            i < 8u;
            i = i + 1u
        ) {
            output.words[i] =
                hashes[0][i];

            output.words[8u + i] =
                hashes[1][i];
        }
        return;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        output.words[i] = 0u;
    }
}



// ============================================================================
// Milestone 40 — Stage-B field-multiply register pressure.
//
// M39 leaves fixed-base kG as the dominant stage. m36_field_mul_2pass expands
// each U256 operand into two 16-element radix-2^16 arrays before allocating
// the 34-element product array. This variant keeps operands in their original
// 8-limb representation and extracts radix-2^16 digits on demand.
//
// Goal: reduce per-invocation live state without changing field arithmetic.
// ============================================================================

fn m40_digit16(
    value: U256,
    index: u32
) -> u32 {
    let word =
        value[
            index >>
            1u
        ];

    let shift =
        (
            index &
            1u
        ) *
        16u;

    return (
        word >>
        shift
    ) &
    0xffffu;
}


fn m40_field_mul_stream(
    a: U256,
    b: U256
) -> U256 {
    var product:
        array<u32, 34>;

    for (
        var i: u32 = 0u;
        i < 34u;
        i = i + 1u
    ) {
        product[i] =
            0u;
    }

    for (
        var i: u32 = 0u;
        i < 16u;
        i = i + 1u
    ) {
        let ai =
            m40_digit16(
                a,
                i
            );

        var carry:
            u32 =
            0u;

        for (
            var j: u32 = 0u;
            j < 16u;
            j = j + 1u
        ) {
            let index =
                i +
                j;

            let bj =
                m40_digit16(
                    b,
                    j
                );

            let uv =
                product[index] +
                ai *
                bj +
                carry;

            product[index] =
                uv &
                0xffffu;

            carry =
                uv >>
                16u;
        }

        var propagate =
            carry;

        var q =
            i +
            16u;

        loop {
            if (
                propagate == 0u ||
                q >= 34u
            ) {
                break;
            }

            let sum =
                product[q] +
                propagate;

            product[q] =
                sum &
                0xffffu;

            propagate =
                sum >>
                16u;

            q =
                q +
                1u;
        }
    }

    for (
        var reducePass: u32 = 0u;
        reducePass < 2u;
        reducePass = reducePass + 1u
    ) {
        var k: i32 =
            33;

        loop {
            if (k < 16) {
                break;
            }

            let ku =
                u32(k);

            let high =
                product[ku];

            product[ku] =
                0u;

            product[
                ku -
                16u
            ] =
                product[
                    ku -
                    16u
                ] +
                high *
                977u;

            product[
                ku -
                14u
            ] =
                product[
                    ku -
                    14u
                ] +
                high;

            k =
                k -
                1;
        }

        var carry:
            u32 =
            0u;

        for (
            var i: u32 = 0u;
            i < 34u;
            i = i + 1u
        ) {
            let value =
                product[i] +
                carry;

            product[i] =
                value &
                0xffffu;

            carry =
                value >>
                16u;
        }
    }

    var result:
        U256;

    for (
        var limb: u32 = 0u;
        limb < 8u;
        limb = limb + 1u
    ) {
        result[limb] =
            product[
                limb *
                2u
            ] |
            (
                product[
                    limb *
                    2u +
                    1u
                ] <<
                16u
            );
    }

    return normalize_field(
        result
    );
}


fn m40_jacobian_add_affine_stream(
    p1: JacobianPoint,
    qx: U256,
    qy: U256
) -> JacobianPoint {
    if (u256_is_zero(p1.z)) {
        var q: JacobianPoint;
        q.x = qx;
        q.y = qy;
        q.z = u256_one();
        return q;
    }

    let Z1Z1 =
        m40_field_mul_stream(
            p1.z,
            p1.z
        );

    let U2 =
        m40_field_mul_stream(
            qx,
            Z1Z1
        );

    let Z1Cubed =
        m40_field_mul_stream(
            p1.z,
            Z1Z1
        );

    let S2 =
        m40_field_mul_stream(
            qy,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            p1.x
        );

    let R =
        field_sub(
            S2,
            p1.y
        );

    if (u256_is_zero(H)) {
        if (u256_is_zero(R)) {
            return jacobian_double(
                p1
            );
        }

        return jacobian_infinity();
    }

    let HH =
        m40_field_mul_stream(
            H,
            H
        );

    let HHH =
        m40_field_mul_stream(
            H,
            HH
        );

    let V =
        m40_field_mul_stream(
            p1.x,
            HH
        );

    let R2 =
        m40_field_mul_stream(
            R,
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(
                V,
                V
            )
        );

    let Y3 =
        field_sub(
            m40_field_mul_stream(
                R,
                field_sub(
                    V,
                    X3
                )
            ),
            m40_field_mul_stream(
                p1.y,
                HHH
            )
        );

    let Z3 =
        m40_field_mul_stream(
            p1.z,
            H
        );

    var result:
        JacobianPoint;

    result.x =
        X3;

    result.y =
        Y3;

    result.z =
        Z3;

    return result;
}


fn m40_scalar_mul_g16_stream(
    scalar: U256
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var windowIndex: u32 = 0u;
        windowIndex < 16u;
        windowIndex = windowIndex + 1u
    ) {
        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex *
                16u,
                16u
            );

        if (digit != 0u) {
            let q =
                m29_table_point(
                    65536u,
                    windowIndex,
                    digit
                );

            result =
                m40_jacobian_add_affine_stream(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


@compute
@workgroup_size(256)
fn photon_m40_field_mul_fuzz(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let seed =
        input.byteLength +
        gid.x;

    let a =
        m35_bench_scalar(
            seed
        );

    let b =
        m35_bench_scalar(
            seed ^
            0x9e3779b9u
        );

    let baseline =
        m36_field_mul_2pass(
            a,
            b
        );

    let streamed =
        m40_field_mul_stream(
            a,
            b
        );

    if (!u256_equal(
        baseline,
        streamed
    )) {
        atomicAdd(
            &benchmarkOutput.winners,
            1u
        );
    }

    atomicAdd(
        &benchmarkOutput.checksum,
        baseline[0] ^
        baseline[3] ^
        streamed[5] ^
        streamed[7] ^
        seed
    );

    atomicAdd(
        &benchmarkOutput.completed,
        1u
    );
}


@compute
@workgroup_size(32)
fn photon_m40_stage_b_baseline_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(32)
fn photon_m40_stage_b_stream_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(64)
fn photon_m40_stage_b_baseline_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(64)
fn photon_m40_stage_b_stream_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(96)
fn photon_m40_stage_b_baseline_wg96(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(96)
fn photon_m40_stage_b_stream_wg96(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(128)
fn photon_m40_stage_b_baseline_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(128)
fn photon_m40_stage_b_stream_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(160)
fn photon_m40_stage_b_baseline_wg160(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(160)
fn photon_m40_stage_b_stream_wg160(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(192)
fn photon_m40_stage_b_baseline_wg192(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(192)
fn photon_m40_stage_b_stream_wg192(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(224)
fn photon_m40_stage_b_baseline_wg224(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(224)
fn photon_m40_stage_b_stream_wg224(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(256)
fn photon_m40_stage_b_baseline_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m36_scalar_mul_g16_2pass(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(256)
fn photon_m40_stage_b_stream_wg256(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


@compute
@workgroup_size(1)
fn photon_m40_stage_b_stream_candidate(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    let base =
        m37_base(
            gid.x
        );

    let k =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m40_scalar_mul_g16_stream(
            k
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}



// ============================================================================
// Milestone 42 — cooperative fixed-base kG.
//
// One candidate currently performs up to 16 mixed Jacobian+affine additions
// serially in one invocation. M42 assigns 16 lanes to one candidate:
//
//   lane 0..15 -> one fixed-base 16-bit window each
//
// The 16 table points are reduced through four workgroup-synchronized levels.
// First level is specialized affine+affine (6 field multiplications); later
// levels use full Jacobian+Jacobian addition. This keeps total arithmetic near
// the serial path while reducing dependency depth and per-lane live state.
//
// Module workgroup storage = 3 * 128 * 8 * 4 = 12,288 bytes.
// ============================================================================

var<workgroup>
m42SharedX:
    array<u32, 1024>;

var<workgroup>
m42SharedY:
    array<u32, 1024>;

var<workgroup>
m42SharedZ:
    array<u32, 1024>;


fn m42_shared_store(
    slot: u32,
    point: JacobianPoint
) {
    let base =
        slot *
        8u;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        m42SharedX[
            base + i
        ] =
            point.x[i];

        m42SharedY[
            base + i
        ] =
            point.y[i];

        m42SharedZ[
            base + i
        ] =
            point.z[i];
    }
}


fn m42_shared_load(
    slot: u32
) -> JacobianPoint {
    let base =
        slot *
        8u;

    var point:
        JacobianPoint;

    for (
        var i: u32 = 0u;
        i < 8u;
        i = i + 1u
    ) {
        point.x[i] =
            m42SharedX[
                base + i
            ];

        point.y[i] =
            m42SharedY[
                base + i
            ];

        point.z[i] =
            m42SharedZ[
                base + i
            ];
    }

    return point;
}


fn m42_jacobian_double_2pass(
    p1: JacobianPoint
) -> JacobianPoint {
    if (
        u256_is_zero(
            p1.z
        ) ||
        u256_is_zero(
            p1.y
        )
    ) {
        return jacobian_infinity();
    }

    let YY =
        m36_field_mul_2pass(
            p1.y,
            p1.y
        );

    let XYY =
        m36_field_mul_2pass(
            p1.x,
            YY
        );

    let twoXYY =
        field_add(
            XYY,
            XYY
        );

    let S =
        field_add(
            twoXYY,
            twoXYY
        );

    let XX =
        m36_field_mul_2pass(
            p1.x,
            p1.x
        );

    let M =
        field_add(
            field_add(
                XX,
                XX
            ),
            XX
        );

    let M2 =
        m36_field_mul_2pass(
            M,
            M
        );

    let X3 =
        field_sub(
            M2,
            field_add(
                S,
                S
            )
        );

    let YY2 =
        m36_field_mul_2pass(
            YY,
            YY
        );

    let eightYY2 =
        field_add(
            field_add(
                field_add(
                    YY2,
                    YY2
                ),
                field_add(
                    YY2,
                    YY2
                )
            ),
            field_add(
                field_add(
                    YY2,
                    YY2
                ),
                field_add(
                    YY2,
                    YY2
                )
            )
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                M,
                field_sub(
                    S,
                    X3
                )
            ),
            eightYY2
        );

    let Z3 =
        field_add(
            m36_field_mul_2pass(
                p1.y,
                p1.z
            ),
            m36_field_mul_2pass(
                p1.y,
                p1.z
            )
        );

    var result:
        JacobianPoint;

    result.x =
        X3;

    result.y =
        Y3;

    result.z =
        Z3;

    return result;
}


// Inputs are either infinity or affine (Z=1). This is the first reduction
// level and avoids all multiplications by Z=1.
fn m42_add_affine_pair_2pass(
    p1: JacobianPoint,
    p2: JacobianPoint
) -> JacobianPoint {
    if (
        u256_is_zero(
            p1.z
        )
    ) {
        return p2;
    }

    if (
        u256_is_zero(
            p2.z
        )
    ) {
        return p1;
    }

    let H =
        field_sub(
            p2.x,
            p1.x
        );

    let R =
        field_sub(
            p2.y,
            p1.y
        );

    if (
        u256_is_zero(
            H
        )
    ) {
        if (
            u256_is_zero(
                R
            )
        ) {
            return m42_jacobian_double_2pass(
                p1
            );
        }

        return jacobian_infinity();
    }

    let HH =
        m36_field_mul_2pass(
            H,
            H
        );

    let HHH =
        m36_field_mul_2pass(
            H,
            HH
        );

    let V =
        m36_field_mul_2pass(
            p1.x,
            HH
        );

    let R2 =
        m36_field_mul_2pass(
            R,
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(
                V,
                V
            )
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                R,
                field_sub(
                    V,
                    X3
                )
            ),
            m36_field_mul_2pass(
                p1.y,
                HHH
            )
        );

    var result:
        JacobianPoint;

    result.x =
        X3;

    result.y =
        Y3;

    result.z =
        H;

    return result;
}


fn m42_jacobian_add_2pass(
    p1: JacobianPoint,
    p2: JacobianPoint
) -> JacobianPoint {
    if (
        u256_is_zero(
            p1.z
        )
    ) {
        return p2;
    }

    if (
        u256_is_zero(
            p2.z
        )
    ) {
        return p1;
    }

    let Z1Z1 =
        m36_field_mul_2pass(
            p1.z,
            p1.z
        );

    let Z2Z2 =
        m36_field_mul_2pass(
            p2.z,
            p2.z
        );

    let U1 =
        m36_field_mul_2pass(
            p1.x,
            Z2Z2
        );

    let U2 =
        m36_field_mul_2pass(
            p2.x,
            Z1Z1
        );

    let Z1Cubed =
        m36_field_mul_2pass(
            p1.z,
            Z1Z1
        );

    let Z2Cubed =
        m36_field_mul_2pass(
            p2.z,
            Z2Z2
        );

    let S1 =
        m36_field_mul_2pass(
            p1.y,
            Z2Cubed
        );

    let S2 =
        m36_field_mul_2pass(
            p2.y,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            U1
        );

    let R =
        field_sub(
            S2,
            S1
        );

    if (
        u256_is_zero(
            H
        )
    ) {
        if (
            u256_is_zero(
                R
            )
        ) {
            return m42_jacobian_double_2pass(
                p1
            );
        }

        return jacobian_infinity();
    }

    let HH =
        m36_field_mul_2pass(
            H,
            H
        );

    let HHH =
        m36_field_mul_2pass(
            H,
            HH
        );

    let V =
        m36_field_mul_2pass(
            U1,
            HH
        );

    let R2 =
        m36_field_mul_2pass(
            R,
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(
                V,
                V
            )
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                R,
                field_sub(
                    V,
                    X3
                )
            ),
            m36_field_mul_2pass(
                S1,
                HHH
            )
        );

    let Z1Z2 =
        m36_field_mul_2pass(
            p1.z,
            p2.z
        );

    let Z3 =
        m36_field_mul_2pass(
            Z1Z2,
            H
        );

    var result:
        JacobianPoint;

    result.x =
        X3;

    result.y =
        Y3;

    result.z =
        Z3;

    return result;
}


fn m42_load_window_point(
    scalar: U256,
    windowIndex: u32
) -> JacobianPoint {
    let digit =
        m29_scalar_window_bits(
            scalar,
            windowIndex *
            16u,
            16u
        );

    if (
        digit ==
        0u
    ) {
        return jacobian_infinity();
    }

    let q =
        m29_table_point(
            65536u,
            windowIndex,
            digit
        );

    var result:
        JacobianPoint;

    result.x =
        q.x;

    result.y =
        q.y;

    result.z =
        u256_one();

    return result;
}


// Shared implementation. `slotsPerWorkgroup` is 32/64/128 and therefore
// candidatesPerWorkgroup is slots/16. Every invocation participates in all
// barriers; out-of-range storage accesses are handled robustly by WebGPU.
fn m42_cooperative_body(
    localIndex: u32,
    workgroupIndex: u32,
    slotsPerWorkgroup: u32
) {
    let lane =
        localIndex &
        15u;

    let candidateLocal =
        localIndex >>
        4u;

    let candidatesPerWorkgroup =
        slotsPerWorkgroup >>
        4u;

    let candidateIndex =
        workgroupIndex *
        candidatesPerWorkgroup +
        candidateLocal;

    let candidateIsActive =
        candidateIndex <
        m42DispatchParams.candidateCount;

    var point =
        jacobian_infinity();

    if (candidateIsActive) {
        let base =
            m37_base(
                candidateIndex
            );

        let k =
            m37_load_u256(
                base +
                8u
            );

        point =
            m42_load_window_point(
                k,
                lane
            );
    }

    m42_shared_store(
        localIndex,
        point
    );

    workgroupBarrier();

    // Level 1: affine/infinity pairs.
    if (
        (
            lane &
            1u
        ) ==
        0u
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                1u
            );

        m42_shared_store(
            localIndex,
            m42_add_affine_pair_2pass(
                left,
                right
            )
        );
    }

    workgroupBarrier();

    // Level 2.
    if (
        (
            lane &
            3u
        ) ==
        0u
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                2u
            );

        m42_shared_store(
            localIndex,
            m42_jacobian_add_2pass(
                left,
                right
            )
        );
    }

    workgroupBarrier();

    // Level 3.
    if (
        (
            lane &
            7u
        ) ==
        0u
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                4u
            );

        m42_shared_store(
            localIndex,
            m42_jacobian_add_2pass(
                left,
                right
            )
        );
    }

    workgroupBarrier();

    // Level 4.
    if (
        lane ==
        0u &&
        candidateIsActive
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                8u
            );

        let result =
            m42_jacobian_add_2pass(
                left,
                right
            );

        let base =
            m37_base(
                candidateIndex
            );

        m37_store_u256(
            base +
            16u,
            result.x
        );

        m37_store_u256(
            base +
            24u,
            result.y
        );

        m37_store_u256(
            base +
            32u,
            result.z
        );
    }
}


@compute
@workgroup_size(32)
fn photon_m42_stage_b_coop_wg32(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m42_cooperative_body(
        lid.x,
        flattenedWorkgroup,
        32u
    );
}


@compute
@workgroup_size(64)
fn photon_m42_stage_b_coop_wg64(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m42_cooperative_body(
        lid.x,
        flattenedWorkgroup,
        64u
    );
}


@compute
@workgroup_size(128)
fn photon_m42_stage_b_coop_wg128(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m42_cooperative_body(
        lid.x,
        flattenedWorkgroup,
        128u
    );
}



// ============================================================================
// Milestone 43 — chunked fixed-base kG.
//
// M42 proved that a full 16-lane reduction tree loses badly because it replaces
// cheap Jacobian+affine mixed additions with many full Jacobian+Jacobian adds.
//
// M43 keeps the cheap mixed-add path inside small serial chunks:
//
//   2-way: two lanes each accumulate 8 affine windows, then 1 full J+J merge.
//   4-way: four lanes each accumulate 4 affine windows, then 3 full J+J merges.
//
// This reduces serial dependency depth while minimizing expensive full merges.
// ============================================================================


fn m43_accumulate_chunk(
    scalar: U256,
    firstWindow: u32,
    windowCount: u32
) -> JacobianPoint {
    var result =
        jacobian_infinity();

    for (
        var offset: u32 = 0u;
        offset < windowCount;
        offset = offset + 1u
    ) {
        let windowIndex =
            firstWindow +
            offset;

        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex *
                16u,
                16u
            );

        if (
            digit !=
            0u
        ) {
            let q =
                m29_table_point(
                    65536u,
                    windowIndex,
                    digit
                );

            result =
                m36_jacobian_add_affine_2pass(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m43_chunk2_body(
    localIndex: u32,
    workgroupIndex: u32,
    slotsPerWorkgroup: u32
) {
    let lane =
        localIndex &
        1u;

    let candidateLocal =
        localIndex >>
        1u;

    let candidatesPerWorkgroup =
        slotsPerWorkgroup >>
        1u;

    let candidateIndex =
        workgroupIndex *
        candidatesPerWorkgroup +
        candidateLocal;

    let candidateIsActive =
        candidateIndex <
        m42DispatchParams.candidateCount;

    var partial =
        jacobian_infinity();

    if (
        candidateIsActive
    ) {
        let base =
            m37_base(
                candidateIndex
            );

        let k =
            m37_load_u256(
                base +
                8u
            );

        partial =
            m43_accumulate_chunk(
                k,
                lane *
                8u,
                8u
            );
    }

    m42_shared_store(
        localIndex,
        partial
    );

    workgroupBarrier();

    if (
        lane ==
        0u &&
        candidateIsActive
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                1u
            );

        let result =
            m42_jacobian_add_2pass(
                left,
                right
            );

        let base =
            m37_base(
                candidateIndex
            );

        m37_store_u256(
            base +
            16u,
            result.x
        );

        m37_store_u256(
            base +
            24u,
            result.y
        );

        m37_store_u256(
            base +
            32u,
            result.z
        );
    }
}


fn m43_chunk4_body(
    localIndex: u32,
    workgroupIndex: u32,
    slotsPerWorkgroup: u32
) {
    let lane =
        localIndex &
        3u;

    let candidateLocal =
        localIndex >>
        2u;

    let candidatesPerWorkgroup =
        slotsPerWorkgroup >>
        2u;

    let candidateIndex =
        workgroupIndex *
        candidatesPerWorkgroup +
        candidateLocal;

    let candidateIsActive =
        candidateIndex <
        m42DispatchParams.candidateCount;

    var partial =
        jacobian_infinity();

    if (
        candidateIsActive
    ) {
        let base =
            m37_base(
                candidateIndex
            );

        let k =
            m37_load_u256(
                base +
                8u
            );

        partial =
            m43_accumulate_chunk(
                k,
                lane *
                4u,
                4u
            );
    }

    m42_shared_store(
        localIndex,
        partial
    );

    workgroupBarrier();

    if (
        (
            lane &
            1u
        ) ==
        0u
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                1u
            );

        m42_shared_store(
            localIndex,
            m42_jacobian_add_2pass(
                left,
                right
            )
        );
    }

    workgroupBarrier();

    if (
        lane ==
        0u &&
        candidateIsActive
    ) {
        let left =
            m42_shared_load(
                localIndex
            );

        let right =
            m42_shared_load(
                localIndex +
                2u
            );

        let result =
            m42_jacobian_add_2pass(
                left,
                right
            );

        let base =
            m37_base(
                candidateIndex
            );

        m37_store_u256(
            base +
            16u,
            result.x
        );

        m37_store_u256(
            base +
            24u,
            result.y
        );

        m37_store_u256(
            base +
            32u,
            result.z
        );
    }
}


@compute
@workgroup_size(32)
fn photon_m43_stage_b_chunk2_wg32(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m43_chunk2_body(
        lid.x,
        flattenedWorkgroup,
        32u
    );
}


@compute
@workgroup_size(64)
fn photon_m43_stage_b_chunk2_wg64(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m43_chunk2_body(
        lid.x,
        flattenedWorkgroup,
        64u
    );
}


@compute
@workgroup_size(128)
fn photon_m43_stage_b_chunk2_wg128(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m43_chunk2_body(
        lid.x,
        flattenedWorkgroup,
        128u
    );
}


@compute
@workgroup_size(32)
fn photon_m43_stage_b_chunk4_wg32(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m43_chunk4_body(
        lid.x,
        flattenedWorkgroup,
        32u
    );
}


@compute
@workgroup_size(64)
fn photon_m43_stage_b_chunk4_wg64(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m43_chunk4_body(
        lid.x,
        flattenedWorkgroup,
        64u
    );
}


@compute
@workgroup_size(128)
fn photon_m43_stage_b_chunk4_wg128(
    @builtin(local_invocation_id)
    lid: vec3<u32>,
    @builtin(workgroup_id)
    wid: vec3<u32>
) {
    let flattenedWorkgroup =
        wid.x +
        wid.y *
        m42DispatchParams.groupsX;

    m43_chunk4_body(
        lid.x,
        flattenedWorkgroup,
        128u
    );
}



// ============================================================================
// Milestone 45 — serial Stage-B kernel splitting.
//
// Keep exactly the proven Jacobian+affine 2-pass arithmetic, but divide the
// 16 fixed-base windows across multiple GPU dispatches. Each later dispatch
// reloads the partial Jacobian point and continues mixed additions.
//
// This tests whether M37's large kernel-splitting benefit also exists *inside*
// the current Stage-B bottleneck, without introducing Jacobian+Jacobian merges.
// ============================================================================


fn m45_accumulate_range(
    initialPoint: JacobianPoint,
    scalar: U256,
    firstWindow: u32,
    windowCount: u32
) -> JacobianPoint {
    var result =
        initialPoint;

    for (
        var offset: u32 = 0u;
        offset < windowCount;
        offset = offset + 1u
    ) {
        let windowIndex =
            firstWindow +
            offset;

        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex *
                16u,
                16u
            );

        if (
            digit !=
            0u
        ) {
            let q =
                m29_table_point(
                    65536u,
                    windowIndex,
                    digit
                );

            result =
                m36_jacobian_add_affine_2pass(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m45_stage_b_part(
    candidateIndex: u32,
    firstWindow: u32,
    windowCount: u32,
    startFromInfinity: bool
) {
    if (
        candidateIndex >=
        m42DispatchParams.candidateCount
    ) {
        return;
    }

    let base =
        m37_base(
            candidateIndex
        );

    let scalar =
        m37_load_u256(
            base +
            8u
        );

    var initialPoint =
        jacobian_infinity();

    if (
        !startFromInfinity
    ) {
        initialPoint.x =
            m37_load_u256(
                base +
                16u
            );

        initialPoint.y =
            m37_load_u256(
                base +
                24u
            );

        initialPoint.z =
            m37_load_u256(
                base +
                32u
            );
    }

    let result =
        m45_accumulate_range(
            initialPoint,
            scalar,
            firstWindow,
            windowCount
        );

    m37_store_u256(
        base +
        16u,
        result.x
    );

    m37_store_u256(
        base +
        24u,
        result.y
    );

    m37_store_u256(
        base +
        32u,
        result.z
    );
}


// ---- 2-split: 8 + 8 windows -----------------------------------------------

@compute
@workgroup_size(32)
fn photon_m45_b2a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        0u,
        8u,
        true
    );
}

@compute
@workgroup_size(32)
fn photon_m45_b2b_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        8u,
        8u,
        false
    );
}

@compute
@workgroup_size(64)
fn photon_m45_b2a_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        0u,
        8u,
        true
    );
}

@compute
@workgroup_size(64)
fn photon_m45_b2b_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        8u,
        8u,
        false
    );
}

@compute
@workgroup_size(128)
fn photon_m45_b2a_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        0u,
        8u,
        true
    );
}

@compute
@workgroup_size(128)
fn photon_m45_b2b_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        8u,
        8u,
        false
    );
}


// ---- 4-split: 4 + 4 + 4 + 4 windows -------------------------------------

@compute
@workgroup_size(32)
fn photon_m45_b4a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        0u,
        4u,
        true
    );
}

@compute
@workgroup_size(32)
fn photon_m45_b4b_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        4u,
        4u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m45_b4c_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        8u,
        4u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m45_b4d_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        12u,
        4u,
        false
    );
}

@compute
@workgroup_size(64)
fn photon_m45_b4a_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        0u,
        4u,
        true
    );
}

@compute
@workgroup_size(64)
fn photon_m45_b4b_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        4u,
        4u,
        false
    );
}

@compute
@workgroup_size(64)
fn photon_m45_b4c_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        8u,
        4u,
        false
    );
}

@compute
@workgroup_size(64)
fn photon_m45_b4d_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        12u,
        4u,
        false
    );
}

@compute
@workgroup_size(128)
fn photon_m45_b4a_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        0u,
        4u,
        true
    );
}

@compute
@workgroup_size(128)
fn photon_m45_b4b_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        4u,
        4u,
        false
    );
}

@compute
@workgroup_size(128)
fn photon_m45_b4c_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        8u,
        4u,
        false
    );
}

@compute
@workgroup_size(128)
fn photon_m45_b4d_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m45_stage_b_part(
        gid.x,
        12u,
        4u,
        false
    );
}



// ============================================================================
// Milestone 46 — full-path 15-bit vs 16-bit fixed-base table width.
//
// 15-bit table: 18 windows, 32768 entries/window, 37,748,736 bytes.
// 16-bit table: 16 windows, 65536 entries/window, 67,108,864 bytes.
//
// M46 retests table width after M45 established that serial kernel splitting
// improves complete throughput. The 15-bit split is 5+5+4+4 windows so each
// dispatch remains close to the M45 16-bit 4+4+4+4 kernel size.
// ============================================================================


fn m46_accumulate_range15(
    initialPoint: JacobianPoint,
    scalar: U256,
    firstWindow: u32,
    windowCount: u32
) -> JacobianPoint {
    var result =
        initialPoint;

    for (
        var offset: u32 = 0u;
        offset < windowCount;
        offset = offset + 1u
    ) {
        let windowIndex =
            firstWindow +
            offset;

        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex *
                15u,
                15u
            );

        if (
            digit !=
            0u
        ) {
            let q =
                m29_table_point(
                    32768u,
                    windowIndex,
                    digit
                );

            result =
                m36_jacobian_add_affine_2pass(
                    result,
                    q.x,
                    q.y
                );
        }
    }

    return result;
}


fn m46_stage_b15_part(
    candidateIndex: u32,
    firstWindow: u32,
    windowCount: u32,
    startFromInfinity: bool
) {
    if (
        candidateIndex >=
        m42DispatchParams.candidateCount
    ) {
        return;
    }

    let base =
        m37_base(
            candidateIndex
        );

    let scalar =
        m37_load_u256(
            base +
            8u
        );

    var initialPoint =
        jacobian_infinity();

    if (
        !startFromInfinity
    ) {
        initialPoint.x =
            m37_load_u256(
                base +
                16u
            );

        initialPoint.y =
            m37_load_u256(
                base +
                24u
            );

        initialPoint.z =
            m37_load_u256(
                base +
                32u
            );
    }

    let result =
        m46_accumulate_range15(
            initialPoint,
            scalar,
            firstWindow,
            windowCount
        );

    m37_store_u256(
        base +
        16u,
        result.x
    );

    m37_store_u256(
        base +
        24u,
        result.y
    );

    m37_store_u256(
        base +
        32u,
        result.z
    );
}


// 15-bit unsplit baseline, added at WG32 for full-path comparison.
@compute
@workgroup_size(32)
fn photon_m46_b15_baseline_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    if (
        gid.x >=
        m42DispatchParams.candidateCount
    ) {
        return;
    }

    let base =
        m37_base(
            gid.x
        );

    let scalar =
        m37_load_u256(
            base +
            8u
        );

    let point =
        m38_scalar_mul_g15_2pass(
            scalar
        );

    m37_store_u256(
        base +
        16u,
        point.x
    );

    m37_store_u256(
        base +
        24u,
        point.y
    );

    m37_store_u256(
        base +
        32u,
        point.z
    );
}


// 15-bit four-split: 5 + 5 + 4 + 4 windows.
// A: windows 0..4
// B: windows 5..9
// C: windows 10..13
// D: windows 14..17

@compute
@workgroup_size(32)
fn photon_m46_b15_4a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        0u,
        5u,
        true
    );
}

@compute
@workgroup_size(32)
fn photon_m46_b15_4b_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        5u,
        5u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m46_b15_4c_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        10u,
        4u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m46_b15_4d_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        14u,
        4u,
        false
    );
}


@compute
@workgroup_size(64)
fn photon_m46_b15_4a_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        0u,
        5u,
        true
    );
}

@compute
@workgroup_size(64)
fn photon_m46_b15_4b_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        5u,
        5u,
        false
    );
}

@compute
@workgroup_size(64)
fn photon_m46_b15_4c_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        10u,
        4u,
        false
    );
}

@compute
@workgroup_size(64)
fn photon_m46_b15_4d_wg64(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        14u,
        4u,
        false
    );
}


@compute
@workgroup_size(128)
fn photon_m46_b15_4a_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        0u,
        5u,
        true
    );
}

@compute
@workgroup_size(128)
fn photon_m46_b15_4b_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        5u,
        5u,
        false
    );
}

@compute
@workgroup_size(128)
fn photon_m46_b15_4c_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        10u,
        4u,
        false
    );
}

@compute
@workgroup_size(128)
fn photon_m46_b15_4d_wg128(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m46_stage_b15_part(
        gid.x,
        14u,
        4u,
        false
    );
}



// ============================================================================
// Milestone 47 — revive specialized field squaring under M45's split Stage B.
//
// M36 showed m36_field_square_2pass could accelerate isolated fixed-base kG,
// but the monolithic full shader regressed badly, consistent with register
// pressure. M45 now limits each Stage-B dispatch to four windows.
//
// Test three selective mixed-add variants:
//   Z-square  : specialized square only for Z1^2
//   HR-square : specialized squares only for H^2 and R^2
//   all-square: specialized squares for Z1^2, H^2, R^2 (M36 formula)
//
// Everything else remains the M46 winner: 16-bit table, 4-split, WG32.
// ============================================================================


fn m47_jacobian_add_affine_zsquare(
    p1: JacobianPoint,
    qx: U256,
    qy: U256
) -> JacobianPoint {
    if (
        u256_is_zero(
            p1.z
        )
    ) {
        var q:
            JacobianPoint;

        q.x =
            qx;

        q.y =
            qy;

        q.z =
            u256_one();

        return q;
    }

    let Z1Z1 =
        m36_field_square_2pass(
            p1.z
        );

    let U2 =
        m36_field_mul_2pass(
            qx,
            Z1Z1
        );

    let Z1Cubed =
        m36_field_mul_2pass(
            p1.z,
            Z1Z1
        );

    let S2 =
        m36_field_mul_2pass(
            qy,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            p1.x
        );

    let R =
        field_sub(
            S2,
            p1.y
        );

    if (
        u256_is_zero(
            H
        )
    ) {
        if (
            u256_is_zero(
                R
            )
        ) {
            return jacobian_double(
                p1
            );
        }

        return jacobian_infinity();
    }

    let HH =
        m36_field_mul_2pass(
            H,
            H
        );

    let HHH =
        m36_field_mul_2pass(
            H,
            HH
        );

    let V =
        m36_field_mul_2pass(
            p1.x,
            HH
        );

    let R2 =
        m36_field_mul_2pass(
            R,
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(
                V,
                V
            )
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                R,
                field_sub(
                    V,
                    X3
                )
            ),
            m36_field_mul_2pass(
                p1.y,
                HHH
            )
        );

    let Z3 =
        m36_field_mul_2pass(
            p1.z,
            H
        );

    var result:
        JacobianPoint;

    result.x =
        X3;

    result.y =
        Y3;

    result.z =
        Z3;

    return result;
}


fn m47_jacobian_add_affine_hrsquare(
    p1: JacobianPoint,
    qx: U256,
    qy: U256
) -> JacobianPoint {
    if (
        u256_is_zero(
            p1.z
        )
    ) {
        var q:
            JacobianPoint;

        q.x =
            qx;

        q.y =
            qy;

        q.z =
            u256_one();

        return q;
    }

    let Z1Z1 =
        m36_field_mul_2pass(
            p1.z,
            p1.z
        );

    let U2 =
        m36_field_mul_2pass(
            qx,
            Z1Z1
        );

    let Z1Cubed =
        m36_field_mul_2pass(
            p1.z,
            Z1Z1
        );

    let S2 =
        m36_field_mul_2pass(
            qy,
            Z1Cubed
        );

    let H =
        field_sub(
            U2,
            p1.x
        );

    let R =
        field_sub(
            S2,
            p1.y
        );

    if (
        u256_is_zero(
            H
        )
    ) {
        if (
            u256_is_zero(
                R
            )
        ) {
            return jacobian_double(
                p1
            );
        }

        return jacobian_infinity();
    }

    let HH =
        m36_field_square_2pass(
            H
        );

    let HHH =
        m36_field_mul_2pass(
            H,
            HH
        );

    let V =
        m36_field_mul_2pass(
            p1.x,
            HH
        );

    let R2 =
        m36_field_square_2pass(
            R
        );

    let X3 =
        field_sub(
            field_sub(
                R2,
                HHH
            ),
            field_add(
                V,
                V
            )
        );

    let Y3 =
        field_sub(
            m36_field_mul_2pass(
                R,
                field_sub(
                    V,
                    X3
                )
            ),
            m36_field_mul_2pass(
                p1.y,
                HHH
            )
        );

    let Z3 =
        m36_field_mul_2pass(
            p1.z,
            H
        );

    var result:
        JacobianPoint;

    result.x =
        X3;

    result.y =
        Y3;

    result.z =
        Z3;

    return result;
}


fn m47_accumulate_variant(
    initialPoint: JacobianPoint,
    scalar: U256,
    firstWindow: u32,
    mode: u32
) -> JacobianPoint {
    var result =
        initialPoint;

    // Exactly four 16-bit windows per M45/M46 winning split.
    for (
        var offset: u32 = 0u;
        offset < 4u;
        offset = offset + 1u
    ) {
        let windowIndex =
            firstWindow +
            offset;

        let digit =
            m29_scalar_window_bits(
                scalar,
                windowIndex *
                16u,
                16u
            );

        if (
            digit !=
            0u
        ) {
            let q =
                m29_table_point(
                    65536u,
                    windowIndex,
                    digit
                );

            if (
                mode ==
                1u
            ) {
                result =
                    m47_jacobian_add_affine_zsquare(
                        result,
                        q.x,
                        q.y
                    );

            } else if (
                mode ==
                2u
            ) {
                result =
                    m47_jacobian_add_affine_hrsquare(
                        result,
                        q.x,
                        q.y
                    );

            } else {
                result =
                    m36_jacobian_add_affine_square(
                        result,
                        q.x,
                        q.y
                    );
            }
        }
    }

    return result;
}


fn m47_stage_b_part(
    candidateIndex: u32,
    firstWindow: u32,
    mode: u32,
    startFromInfinity: bool
) {
    if (
        candidateIndex >=
        m42DispatchParams.candidateCount
    ) {
        return;
    }

    let base =
        m37_base(
            candidateIndex
        );

    let scalar =
        m37_load_u256(
            base +
            8u
        );

    var initialPoint =
        jacobian_infinity();

    if (
        !startFromInfinity
    ) {
        initialPoint.x =
            m37_load_u256(
                base +
                16u
            );

        initialPoint.y =
            m37_load_u256(
                base +
                24u
            );

        initialPoint.z =
            m37_load_u256(
                base +
                32u
            );
    }

    let result =
        m47_accumulate_variant(
            initialPoint,
            scalar,
            firstWindow,
            mode
        );

    m37_store_u256(
        base +
        16u,
        result.x
    );

    m37_store_u256(
        base +
        24u,
        result.y
    );

    m37_store_u256(
        base +
        32u,
        result.z
    );
}


// mode 1 = Z-square only
@compute
@workgroup_size(32)
fn photon_m47_z_4a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        0u,
        1u,
        true
    );
}

@compute
@workgroup_size(32)
fn photon_m47_z_4b_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        4u,
        1u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m47_z_4c_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        8u,
        1u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m47_z_4d_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        12u,
        1u,
        false
    );
}


// mode 2 = H-square + R-square only
@compute
@workgroup_size(32)
fn photon_m47_hr_4a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        0u,
        2u,
        true
    );
}

@compute
@workgroup_size(32)
fn photon_m47_hr_4b_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        4u,
        2u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m47_hr_4c_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        8u,
        2u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m47_hr_4d_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        12u,
        2u,
        false
    );
}


// mode 3 = all three specialized squares
@compute
@workgroup_size(32)
fn photon_m47_all_4a_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        0u,
        3u,
        true
    );
}

@compute
@workgroup_size(32)
fn photon_m47_all_4b_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        4u,
        3u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m47_all_4c_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        8u,
        3u,
        false
    );
}

@compute
@workgroup_size(32)
fn photon_m47_all_4d_wg32(
    @builtin(global_invocation_id)
    gid: vec3<u32>
) {
    m47_stage_b_part(
        gid.x,
        12u,
        3u,
        false
    );
}


// ============================================================================
// Milestone 51 — Stage-B split-depth bake-off.
// Same 16-bit table, WG32, and M36 two-pass mixed-add arithmetic.
// Control = 4x4 windows; variants = 8x2 and 16x1.
// ============================================================================

@compute
@workgroup_size(32)
fn photon_m51_b8_0_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 0u, 2u, true);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_1_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 2u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_2_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 4u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_3_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 6u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_4_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 8u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_5_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 10u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_6_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 12u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b8_7_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 14u, 2u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_0_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 0u, 1u, true);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_1_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 1u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_2_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 2u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_3_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 3u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_4_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 4u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_5_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 5u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_6_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 6u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_7_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 7u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_8_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 8u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_9_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 9u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_10_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 10u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_11_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 11u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_12_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 12u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_13_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 13u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_14_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 14u, 1u, false);
}

@compute
@workgroup_size(32)
fn photon_m51_b16_15_wg32(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    m45_stage_b_part(gid.x, 15u, 1u, false);
}


// ============================================================================
// M53 target-comparison portability diagnostic.
// JS supplies a target with every byte = 0x80.
// output.words[0] -> mask: one lower byte at each position must be below
// output.words[1] -> mask: one greater byte at each position must NOT be below
// output.words[2] -> equal hash must NOT be below
// ============================================================================

fn m53_target_words() -> array<u32, 8> {
    var h:
        array<u32, 8>;

    for (
        var wordIndex: u32 = 0u;
        wordIndex < 8u;
        wordIndex = wordIndex + 1u
    ) {
        var word:
            u32 =
            0u;

        for (
            var within: u32 = 0u;
            within < 4u;
            within = within + 1u
        ) {
            let byteIndex =
                wordIndex * 4u +
                within;

            let b =
                m9_input_byte(
                    394u +
                    byteIndex
                );

            word =
                word |
                (
                    b <<
                    (
                        (3u - within) *
                        8u
                    )
                );
        }

        h[wordIndex] =
            word;
    }

    return h;
}


fn m53_hash_with_byte(
    source: array<u32, 8>,
    byteIndex: u32,
    value: u32
) -> array<u32, 8> {
    var h =
        source;

    let wordIndex =
        byteIndex /
        4u;

    let within =
        byteIndex %
        4u;

    let shift =
        (3u - within) *
        8u;

    let clearMask =
        ~(
            0xffu <<
            shift
        );

    h[wordIndex] =
        (
            h[wordIndex] &
            clearMask
        ) |
        (
            (
                value &
                0xffu
            ) <<
            shift
        );

    return h;
}


@compute
@workgroup_size(1)
fn photon_m53_target_compare_diag()
{
    let targetWords =
        m53_target_words();

    var lessMask:
        u32 =
        0u;

    var greaterMask:
        u32 =
        0u;

    for (
        var byteIndex: u32 = 0u;
        byteIndex < 32u;
        byteIndex = byteIndex + 1u
    ) {
        let less =
            m53_hash_with_byte(
                targetWords,
                byteIndex,
                0x7fu
            );

        let greater =
            m53_hash_with_byte(
                targetWords,
                byteIndex,
                0x81u
            );

        if (
            m10_hash_is_below_target(
                less
            )
        ) {
            lessMask =
                lessMask |
                (
                    1u <<
                    byteIndex
                );
        }

        if (
            m10_hash_is_below_target(
                greater
            )
        ) {
            greaterMask =
                greaterMask |
                (
                    1u <<
                    byteIndex
                );
        }
    }

    output.words[0] =
        lessMask;

    output.words[1] =
        greaterMask;

    output.words[2] =
        select(
            0u,
            1u,
            m10_hash_is_below_target(
                targetWords
            )
        );
}


// ============================================================================
// M67.27 — full Apple/Metal compatibility mining tail.
//
// Production Stage A + M45 split Stage B stay unchanged.  Apple-compatible
// Stage C is deliberately split into two small entry points:
//   C1: Jacobian -> BCH Schnorr R||s, stored per candidate at binding 13.
//   C2: consume stored R||s -> prefixed transaction HASH256 -> winner atomics.
//
// No CPU crypto occurs between C1 and C2.  The CPU-created M27 RFC6979
// precompute state is still supplied to unchanged Stage A through binding 5.
// ============================================================================

@group(0) @binding(13)
var<storage, read_write> m6724Signatures: array<u32>;

@group(0) @binding(14)
var<storage, read_write> m6725Hashes: array<u32>;

fn m6724_signature_base(index: u32) -> u32 {
    return index * 16u;
}

fn m6724_store_signature(index: u32, rx: U256, s: U256) {
    let base = m6724_signature_base(index);
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        m6724Signatures[base + i] = rx[7u - i];
        m6724Signatures[base + 8u + i] = s[7u - i];
    }
}

fn m6724_load_sig_bytes(index: u32, half: u32) -> Bytes32 {
    var value: Bytes32;
    let base = m6724_signature_base(index) + half * 8u;
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        let word = m6724Signatures[base + i];
        value[i * 4u] = (word >> 24u) & 0xffu;
        value[i * 4u + 1u] = (word >> 16u) & 0xffu;
        value[i * 4u + 2u] = (word >> 8u) & 0xffu;
        value[i * 4u + 3u] = word & 0xffu;
    }
    return value;
}

@compute
@workgroup_size(64)
fn photon_m6724_c1_fixed_wg64(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let index = gid.x;
    let base = m37_base(index);
    let messageHash = m37_load_bytes32_packed(base);
    let k = m37_load_u256(base + 8u);
    let point = m37_load_point(index);

    // M67.30 Apple/Metal compatibility barrier: although M45 Z is already
    // numerically canonical, normalize_field() before binary inversion is
    // required for correct Metal code generation at the B -> C handoff.
    let zNorm = normalize_field(point.z);
    let zInv = field_inv_binary(zNorm);
    let zInv2 = field_mul(zInv, zInv);
    let rx = field_mul(point.x, zInv2);
    let rBytes = u256_to_be_bytes32(rx);

    let qr = bch_jacobian_y_is_qr(point);
    var adjustedK = k;
    if (!qr) {
        adjustedK = u256_sub_raw(scalar_order_n(), k);
    }

    let challenge = m9_challenge_hash(rBytes, messageHash);
    let e = m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let ed = m39_scalar_mul_fixed_d(e);
    let s = scalar_add_mod_n(adjustedK, ed);
    m6724_store_signature(index, rx, s);
}

@compute
@workgroup_size(64)
fn photon_m6724_c1_baseline_wg64(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let index = gid.x;
    let base = m37_base(index);
    let messageHash = m37_load_bytes32_packed(base);
    let k = m37_load_u256(base + 8u);
    let point = m37_load_point(index);

    let privateKeyBytes = m17_private_key_bytes();
    let d = m9_scalar_reduce_n(u256_from_be_bytes32(privateKeyBytes));

    // M67.30 Apple/Metal compatibility barrier: although M45 Z is already
    // numerically canonical, normalize_field() before binary inversion is
    // required for correct Metal code generation at the B -> C handoff.
    let zNorm = normalize_field(point.z);
    let zInv = field_inv_binary(zNorm);
    let zInv2 = field_mul(zInv, zInv);
    let rx = field_mul(point.x, zInv2);
    let rBytes = u256_to_be_bytes32(rx);

    let qr = bch_jacobian_y_is_qr(point);
    var adjustedK = k;
    if (!qr) {
        adjustedK = u256_sub_raw(scalar_order_n(), k);
    }

    let challenge = m9_challenge_hash(rBytes, messageHash);
    let e = m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let ed = scalar_mul_mod_n(e, d);
    let s = scalar_add_mod_n(adjustedK, ed);
    m6724_store_signature(index, rx, s);
}

// M67.27 Apple compatibility split: keep transaction HASH256 and winner
// accounting in separate entry points. This deliberately minimizes each
// shader's register/control-flow pressure on Metal.
@compute
@workgroup_size(64)
fn photon_m6725_c2_hash_only_wg64(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let index = gid.x;
    let nonce = input.byteLength + index;
    let rBytes = m6724_load_sig_bytes(index, 0u);
    let sBytes = m6724_load_sig_bytes(index, 1u);
    let hashes = m30_transaction_hashes_prefixed(nonce, rBytes, sBytes);
    let finalHash = hashes[1];
    let base = index * 8u;
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        m6725Hashes[base + i] = finalHash[i];
    }
}

@compute
@workgroup_size(64)
fn photon_m6725_c3_winner_wg64(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let index = gid.x;
    let nonce = input.byteLength + index;
    let base = index * 8u;
    var finalHash: array<u32, 8>;
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        finalHash[i] = m6725Hashes[base + i];
    }

    atomicAdd(
        &benchmarkOutput.checksum,
        finalHash[0] ^ finalHash[3] ^ finalHash[7] ^ nonce
    );
    atomicAdd(&benchmarkOutput.completed, 1u);

    if (m10_hash_is_below_target(finalHash)) {
        atomicAdd(&benchmarkOutput.winners, 1u);
        atomicExchange(&benchmarkOutput.firstWinnerNoncePlusOne, nonce + 1u);
    }
}

@compute
@workgroup_size(1)
fn photon_m6724_c2_candidate_hash() {
    let nonce = input.byteLength;
    let rBytes = m6724_load_sig_bytes(0u, 0u);
    let sBytes = m6724_load_sig_bytes(0u, 1u);
    let hashes = m30_transaction_hashes_prefixed(nonce, rBytes, sBytes);
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        output.words[i] = hashes[0][i];
        output.words[8u + i] = hashes[1][i];
    }
}


// ============================================================================
// M67.28 — M45/C1 workgroup isolation.
// Exact proven WG1 C1 output plus scalable baseline/fixed-d variants.
// Avoid aggregate/struct returns: prior Apple diagnostics showed those can
// themselves perturb Metal code generation.
// ============================================================================

// Intentionally mirrors M67.19's known-good function, consuming candidate 0
// and writing to the original binding-1 SharedOutput.
@compute @workgroup_size(1)
fn photon_m6728_c1_proven_wg1() {
    let messageHash = m37_load_bytes32_packed(0u);
    let k = m37_load_u256(8u);
    let point = m37_load_point(0u);
    let privateKeyBytes = m17_private_key_bytes();
    let d = m9_scalar_reduce_n(u256_from_be_bytes32(privateKeyBytes));
    let zInv = field_inv_binary(point.z);
    let zInv2 = field_mul(zInv, zInv);
    let rx = field_mul(point.x, zInv2);
    let rBytes = u256_to_be_bytes32(rx);
    let qr = bch_jacobian_y_is_qr(point);
    var adjustedK = k;
    if (!qr) { adjustedK = u256_sub_raw(scalar_order_n(), k); }
    let challenge = m9_challenge_hash(rBytes, messageHash);
    let e = m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let ed = scalar_mul_mod_n(e, d);
    let s = scalar_add_mod_n(adjustedK, ed);
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        output.words[i] = rx[7u - i];
        output.words[8u + i] = s[7u - i];
    }
}

fn m6728_baseline_body(index: u32) {
    let base = m37_base(index);
    let messageHash = m37_load_bytes32_packed(base);
    let k = m37_load_u256(base + 8u);
    let point = m37_load_point(index);
    let privateKeyBytes = m17_private_key_bytes();
    let d = m9_scalar_reduce_n(u256_from_be_bytes32(privateKeyBytes));
    let zInv = field_inv_binary(point.z);
    let zInv2 = field_mul(zInv, zInv);
    let rx = field_mul(point.x, zInv2);
    let rBytes = u256_to_be_bytes32(rx);
    let qr = bch_jacobian_y_is_qr(point);
    var adjustedK = k;
    if (!qr) { adjustedK = u256_sub_raw(scalar_order_n(), k); }
    let challenge = m9_challenge_hash(rBytes, messageHash);
    let e = m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let ed = scalar_mul_mod_n(e, d);
    let s = scalar_add_mod_n(adjustedK, ed);
    m6724_store_signature(index, rx, s);
}

fn m6728_fixed_body(index: u32) {
    let base = m37_base(index);
    let messageHash = m37_load_bytes32_packed(base);
    let k = m37_load_u256(base + 8u);
    let point = m37_load_point(index);
    let zInv = field_inv_binary(point.z);
    let zInv2 = field_mul(zInv, zInv);
    let rx = field_mul(point.x, zInv2);
    let rBytes = u256_to_be_bytes32(rx);
    let qr = bch_jacobian_y_is_qr(point);
    var adjustedK = k;
    if (!qr) { adjustedK = u256_sub_raw(scalar_order_n(), k); }
    let challenge = m9_challenge_hash(rBytes, messageHash);
    let e = m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let ed = m39_scalar_mul_fixed_d(e);
    let s = scalar_add_mod_n(adjustedK, ed);
    m6724_store_signature(index, rx, s);
}

@compute @workgroup_size(1) fn photon_m6728_c1_baseline_wg1(@builtin(global_invocation_id) gid:vec3<u32>){m6728_baseline_body(gid.x);}
@compute @workgroup_size(8) fn photon_m6728_c1_baseline_wg8(@builtin(global_invocation_id) gid:vec3<u32>){m6728_baseline_body(gid.x);}
@compute @workgroup_size(16) fn photon_m6728_c1_baseline_wg16(@builtin(global_invocation_id) gid:vec3<u32>){m6728_baseline_body(gid.x);}
@compute @workgroup_size(32) fn photon_m6728_c1_baseline_wg32(@builtin(global_invocation_id) gid:vec3<u32>){m6728_baseline_body(gid.x);}
@compute @workgroup_size(64) fn photon_m6728_c1_baseline_wg64(@builtin(global_invocation_id) gid:vec3<u32>){m6728_baseline_body(gid.x);}
@compute @workgroup_size(1) fn photon_m6728_c1_fixed_wg1(@builtin(global_invocation_id) gid:vec3<u32>){m6728_fixed_body(gid.x);}
@compute @workgroup_size(8) fn photon_m6728_c1_fixed_wg8(@builtin(global_invocation_id) gid:vec3<u32>){m6728_fixed_body(gid.x);}
@compute @workgroup_size(16) fn photon_m6728_c1_fixed_wg16(@builtin(global_invocation_id) gid:vec3<u32>){m6728_fixed_body(gid.x);}
@compute @workgroup_size(32) fn photon_m6728_c1_fixed_wg32(@builtin(global_invocation_id) gid:vec3<u32>){m6728_fixed_body(gid.x);}
@compute @workgroup_size(64) fn photon_m6728_c1_fixed_wg64(@builtin(global_invocation_id) gid:vec3<u32>){m6728_fixed_body(gid.x);}


// ============================================================================
// M67.29 — actual M45 Jacobian normalization isolation.
// Deliberately avoid returning R/s through aggregate values: earlier Apple
// diagnostics showed that aggregate-return helpers can perturb Metal codegen.
// ============================================================================

fn m6729_write_shared_znorm_binary() {
    let messageHash = m37_load_bytes32_packed(0u);
    let k = m37_load_u256(8u);
    let point = m37_load_point(0u);
    let zNorm = normalize_field(point.z);
    let zInv = field_inv_binary(zNorm);
    let zInv2 = field_mul(zInv,zInv);
    let rx = field_mul(point.x,zInv2);
    let rBytes = u256_to_be_bytes32(rx);
    let qr = bch_jacobian_y_is_qr(point);
    var adjustedK=k; if(!qr){adjustedK=u256_sub_raw(scalar_order_n(),k);}
    let challenge=m9_challenge_hash(rBytes,messageHash);
    let e=m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let d=m9_scalar_reduce_n(u256_from_be_bytes32(m17_private_key_bytes()));
    let s=scalar_add_mod_n(adjustedK,scalar_mul_mod_n(e,d));
    for(var i:u32=0u;i<8u;i=i+1u){output.words[i]=rx[7u-i];output.words[8u+i]=s[7u-i];}
}

fn m6729_write_shared_xyznorm_binary() {
    let messageHash = m37_load_bytes32_packed(0u);
    let k = m37_load_u256(8u);
    let rawPoint = m37_load_point(0u);
    var point=rawPoint;
    point.x=normalize_field(rawPoint.x); point.y=normalize_field(rawPoint.y); point.z=normalize_field(rawPoint.z);
    let zInv=field_inv_binary(point.z); let zInv2=field_mul(zInv,zInv); let rx=field_mul(point.x,zInv2);
    let rBytes=u256_to_be_bytes32(rx); let qr=bch_jacobian_y_is_qr(point);
    var adjustedK=k; if(!qr){adjustedK=u256_sub_raw(scalar_order_n(),k);}
    let challenge=m9_challenge_hash(rBytes,messageHash); let e=m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let d=m9_scalar_reduce_n(u256_from_be_bytes32(m17_private_key_bytes()));
    let s=scalar_add_mod_n(adjustedK,scalar_mul_mod_n(e,d));
    for(var i:u32=0u;i<8u;i=i+1u){output.words[i]=rx[7u-i];output.words[8u+i]=s[7u-i];}
}

fn m6729_write_shared_znorm_fermat() {
    let messageHash=m37_load_bytes32_packed(0u); let k=m37_load_u256(8u); let point=m37_load_point(0u);
    let zNorm=normalize_field(point.z); let zInv=field_inv(zNorm); let zInv2=field_mul(zInv,zInv); let rx=field_mul(point.x,zInv2);
    let rBytes=u256_to_be_bytes32(rx); let qr=bch_jacobian_y_is_qr(point);
    var adjustedK=k; if(!qr){adjustedK=u256_sub_raw(scalar_order_n(),k);}
    let challenge=m9_challenge_hash(rBytes,messageHash); let e=m9_scalar_reduce_n(u256_from_be_bytes32(challenge));
    let d=m9_scalar_reduce_n(u256_from_be_bytes32(m17_private_key_bytes()));
    let s=scalar_add_mod_n(adjustedK,scalar_mul_mod_n(e,d));
    for(var i:u32=0u;i<8u;i=i+1u){output.words[i]=rx[7u-i];output.words[8u+i]=s[7u-i];}
}

@compute @workgroup_size(1) fn photon_m6729_c1_znorm_wg1(){m6729_write_shared_znorm_binary();}
@compute @workgroup_size(1) fn photon_m6729_c1_xyznorm_wg1(){m6729_write_shared_xyznorm_binary();}
@compute @workgroup_size(1) fn photon_m6729_c1_znorm_fermat_wg1(){m6729_write_shared_znorm_fermat();}

fn m6729_store_znorm_binary(index:u32){
    let base=m37_base(index); let messageHash=m37_load_bytes32_packed(base); let k=m37_load_u256(base+8u); let point=m37_load_point(index);
    let zNorm=normalize_field(point.z); let zInv=field_inv_binary(zNorm); let zInv2=field_mul(zInv,zInv); let rx=field_mul(point.x,zInv2);
    let rBytes=u256_to_be_bytes32(rx); let qr=bch_jacobian_y_is_qr(point); var adjustedK=k; if(!qr){adjustedK=u256_sub_raw(scalar_order_n(),k);}
    let challenge=m9_challenge_hash(rBytes,messageHash); let e=m9_scalar_reduce_n(u256_from_be_bytes32(challenge)); let d=m9_scalar_reduce_n(u256_from_be_bytes32(m17_private_key_bytes()));
    let s=scalar_add_mod_n(adjustedK,scalar_mul_mod_n(e,d)); m6724_store_signature(index,rx,s);
}
fn m6729_store_xyznorm_binary(index:u32){
    let base=m37_base(index); let messageHash=m37_load_bytes32_packed(base); let k=m37_load_u256(base+8u); let rawPoint=m37_load_point(index); var point=rawPoint;
    point.x=normalize_field(rawPoint.x); point.y=normalize_field(rawPoint.y); point.z=normalize_field(rawPoint.z);
    let zInv=field_inv_binary(point.z); let zInv2=field_mul(zInv,zInv); let rx=field_mul(point.x,zInv2); let rBytes=u256_to_be_bytes32(rx); let qr=bch_jacobian_y_is_qr(point);
    var adjustedK=k; if(!qr){adjustedK=u256_sub_raw(scalar_order_n(),k);} let challenge=m9_challenge_hash(rBytes,messageHash); let e=m9_scalar_reduce_n(u256_from_be_bytes32(challenge)); let d=m9_scalar_reduce_n(u256_from_be_bytes32(m17_private_key_bytes()));
    let s=scalar_add_mod_n(adjustedK,scalar_mul_mod_n(e,d)); m6724_store_signature(index,rx,s);
}
fn m6729_store_znorm_fermat(index:u32){
    let base=m37_base(index); let messageHash=m37_load_bytes32_packed(base); let k=m37_load_u256(base+8u); let point=m37_load_point(index);
    let zNorm=normalize_field(point.z); let zInv=field_inv(zNorm); let zInv2=field_mul(zInv,zInv); let rx=field_mul(point.x,zInv2); let rBytes=u256_to_be_bytes32(rx); let qr=bch_jacobian_y_is_qr(point);
    var adjustedK=k; if(!qr){adjustedK=u256_sub_raw(scalar_order_n(),k);} let challenge=m9_challenge_hash(rBytes,messageHash); let e=m9_scalar_reduce_n(u256_from_be_bytes32(challenge)); let d=m9_scalar_reduce_n(u256_from_be_bytes32(m17_private_key_bytes()));
    let s=scalar_add_mod_n(adjustedK,scalar_mul_mod_n(e,d)); m6724_store_signature(index,rx,s);
}
@compute @workgroup_size(64) fn photon_m6729_c1_znorm_wg64(@builtin(global_invocation_id) gid:vec3<u32>){m6729_store_znorm_binary(gid.x);}
@compute @workgroup_size(64) fn photon_m6729_c1_xyznorm_wg64(@builtin(global_invocation_id) gid:vec3<u32>){m6729_store_xyznorm_binary(gid.x);}
@compute @workgroup_size(64) fn photon_m6729_c1_znorm_fermat_wg64(@builtin(global_invocation_id) gid:vec3<u32>){m6729_store_znorm_fermat(gid.x);}
