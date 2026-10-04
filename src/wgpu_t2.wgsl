// T2 keeps one RFC6979 BCH Schnorr signature for each 65,536 conserved
// #### PR #22: portable T2 uses the native amount coordinate and proof rules.
// Validate against complete CPU transactions across job/key and window boundaries.
// token-amount pairs. Only the transaction HASH256 varies inside a window.
// The shared Rust transaction code validates the complete window before use.
@group(0) @binding(17) var<storage, read_write> t2Windows: array<u32>;
override T2_GROUP_SIZE: u32 = 64u;
override T2_REUSE: bool = true;
var<workgroup> t2Middle: array<u32, 64>;
struct T2Control { base: u32, count: u32, windows: u32, unused: u32 };
@group(0) @binding(18) var<uniform> t2Control: T2Control;

// Each record contains a prefix state through byte 447 and three padded
// transaction blocks (448..639). Signatures never leave the GPU.
@compute @workgroup_size(64)
fn pickaxe_t2_prepare(@builtin(global_invocation_id) gid: vec3<u32>) {
    let index = gid.x + gid.y * 1048576u;
    if (index >= t2Control.windows) { return; }
    let nonce = input.byteLength + index;
    let r = m6724_load_sig_bytes(index, 0u);
    let s = m6724_load_sig_bytes(index, 1u);
    var block: array<u32, 16>;
    for (var w = 0u; w < 64u; w++) {
        var word = 0u;
        for (var b = 0u; b < 4u; b++) {
            let offset = 384u + w * 4u + b;
            var byte = 0u;
            if (offset < input.txLength) {
                byte = m9_completed_tx_byte(offset, nonce, r, s);
            } else if (offset == input.txLength) {
                byte = 128u;
            } else if (offset >= 636u) {
                byte = ((input.txLength * 8u) >> ((639u - offset) * 8u)) & 255u;
            }
            word = (word << 8u) | byte;
        }
        if (w < 16u) { block[w] = word; }
        else { t2Windows[index * 128u + 8u + w - 16u] = word; }
    }
    var prefix: array<u32, 8>;
    for (var i = 0u; i < 8u; i++) { prefix[i] = m30TxPrefix.words[i]; }
    prefix = compressBlock(prefix, block);
    for (var i = 0u; i < 8u; i++) { t2Windows[index * 128u + i] = prefix[i]; }
    for (var i = 0u; i < 16u; i++) { block[i] = t2Windows[index * 128u + 8u + i]; }
    let head = t2_head10(prefix, block);
    for (var i = 0u; i < 8u; i++) { t2Windows[index * 128u + 56u + i] = head[i]; }
    // Eligibility proves this entire block stays fixed across every window.
    if (index == 0u) {
        var schedule: array<u32, 64>;
        for (var i = 0u; i < 16u; i++) { schedule[i] = t2Windows[24u + i]; }
        for (var i = 16u; i < 64u; i++) {
            schedule[i] = schedule[i - 16u] + smallSigma0(schedule[i - 15u]) + schedule[i - 7u] + smallSigma1(schedule[i - 2u]);
        }
        for (var i = 0u; i < 64u; i++) { t2Windows[64u + i] = schedule[i]; }
    }

}

fn t2_input_u32_le(offset: u32) -> u32 {
    return m9_input_byte(offset) | (m9_input_byte(offset + 1u) << 8u)
        | (m9_input_byte(offset + 2u) << 16u) | (m9_input_byte(offset + 3u) << 24u);
}

@compute @workgroup_size(T2_GROUP_SIZE)
fn pickaxe_t2_filter(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    if (T2_REUSE) {
        if (lane < 64u) { t2Middle[lane] = t2Windows[64u + lane]; }
        workgroupBarrier();
    }
    let index = gid.x + gid.y * 16384u * T2_GROUP_SIZE;
    if (index >= t2Control.count) { return; }
    let candidate = t2Control.base + index;
    let window = candidate >> 16u;
    let j = candidate & 65535u;
    let batonOffset = 491u + T2_SHIFT;
    let rewardOffset = 578u + T2_SHIFT;
    let batonLo = t2_input_u32_le(batonOffset);
    let newBatonLo = batonLo + j;
    let newBatonHi = t2_input_u32_le(batonOffset + 4u) + select(0u, 1u, newBatonLo < batonLo);
    let rewardLo = t2_input_u32_le(rewardOffset);
    let newRewardLo = rewardLo - j;
    let newRewardHi = t2_input_u32_le(rewardOffset + 4u) - select(0u, 1u, rewardLo < j);
    var state: array<u32, 8>;
    for (var i = 0u; i < 8u; i++) { state[i] = t2Windows[window * 128u + i]; }
    let first = t2_block0(window, newBatonLo, newBatonHi, newRewardLo, newRewardHi);
    if (T2_REUSE) {
        var head: array<u32, 8>;
        for (var i = 0u; i < 8u; i++) { head[i] = t2Windows[window * 128u + 56u + i]; }
        state = t2_compress_after10(state, first, head);
        state = t2_compress_middle(state);
    } else {
        state = t2_compress(state, first);
        state = t2_compress(state, t2_block1(window, newBatonLo, newBatonHi, newRewardLo, newRewardHi));
    }
    state = t2_compress(state, t2_block2(window, newBatonLo, newBatonHi, newRewardLo, newRewardHi));
    var last: array<u32, 16>;
    for (var i = 0u; i < 8u; i++) { last[i] = state[i]; }
    last[8] = 0x80000000u;
    last[15] = 256u;
    let digest = t2_compress(initializeState(), last);
    if ((index % T2_GROUP_SIZE) == 0u) {
        atomicAdd(&benchmarkOutput.completed, min(T2_GROUP_SIZE, t2Control.count - index));
    }
    if (m10_hash_is_below_target(digest)) {
        let slot = atomicAdd(&benchmarkOutput.winners, 1u);
        if (slot < arrayLength(&pickaxeWinnerRecords) / 9u) {
            // Return the flattened candidate; the host decodes nonce and j.
            pickaxeWinnerRecords[slot * 9u] = input.byteLength * 65536u + candidate;
            for (var i = 0u; i < 8u; i++) { pickaxeWinnerRecords[slot * 9u + 1u + i] = digest[i]; }
        }
    }
}
