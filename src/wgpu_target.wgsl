fn m10_hash_is_below_target(hashWords: array<u32, 8>) -> bool {
    var nonzero = 0u;
    for (var i = 0u; i < 8u; i++) { nonzero |= hashWords[i]; }
    if (input.positiveProof != 0u && ((hashWords[7] & 0x80u) != 0u || nonzero == 0u)) {
        return false;
    }
    for (var index = 31i; index >= 0i; index--) {
        let i = u32(index);
        var byte = (hashWords[i / 4u] >> ((3u - i % 4u) * 8u)) & 255u;
        if (i == 31u) { byte &= 127u; }
        let targetByte = m9_input_byte(394u + input.layoutShift + i);
        if (byte < targetByte) { return true; }
        if (byte > targetByte) { return false; }
    }
    return false;
}
