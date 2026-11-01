//! #### PR #42
//! Coinbase outputs as Job Declaration carries them: a CompactSize count,
//! then each output's value (8 bytes, little-endian), CompactSize script
//! length and script.

/// The largest script an output may carry (BCH's script size limit).
const MAX_SCRIPT: u64 = 10_000;
/// The most satoshis that can exist.
pub const MAX_MONEY: u64 = 21_000_000 * 100_000_000;

/// The outputs in `bytes`; refused when a count or length is not minimally
/// encoded, a value exceeds the money supply, or bytes are left over.
pub fn parse_outputs(bytes: &[u8]) -> Result<Vec<(u64, Vec<u8>)>, &'static str> {
    let mut at = 0;
    let count = compact_size(bytes, &mut at).ok_or("malformed output count")?;
    // An output takes at least 9 bytes, which bounds the count by the bytes.
    if count > (bytes.len() / 9) as u64 {
        return Err("output count exceeds the outputs' bytes");
    }
    let mut outputs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let value = bytes
            .get(at..at + 8)
            .ok_or("truncated output value")?
            .try_into()
            .map(u64::from_le_bytes)
            .map_err(|_| "truncated output value")?;
        if value > MAX_MONEY {
            return Err("output value exceeds the money supply");
        }
        at += 8;
        let length = compact_size(bytes, &mut at).ok_or("malformed script length")?;
        if length > MAX_SCRIPT {
            return Err("output script too long");
        }
        let script = bytes
            .get(at..at + length as usize)
            .ok_or("truncated output script")?;
        at += script.len();
        outputs.push((value, script.to_vec()));
    }
    if at != bytes.len() {
        return Err("bytes after the outputs");
    }
    Ok(outputs)
}

/// `outputs` in the same encoding.
pub fn serialize_outputs(outputs: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_compact_size(outputs.len() as u64, &mut bytes);
    for (value, script) in outputs {
        bytes.extend_from_slice(&value.to_le_bytes());
        write_compact_size(script.len() as u64, &mut bytes);
        bytes.extend_from_slice(script);
    }
    bytes
}

/// A minimally encoded CompactSize at `*at`, advancing past it.
fn compact_size(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let first = *bytes.get(*at)?;
    let (value, width) = match first {
        0..=0xfc => (u64::from(first), 1),
        0xfd => (
            u64::from(u16::from_le_bytes(
                bytes.get(*at + 1..*at + 3)?.try_into().ok()?,
            )),
            3,
        ),
        0xfe => (
            u64::from(u32::from_le_bytes(
                bytes.get(*at + 1..*at + 5)?.try_into().ok()?,
            )),
            5,
        ),
        0xff => (
            u64::from_le_bytes(bytes.get(*at + 1..*at + 9)?.try_into().ok()?),
            9,
        ),
    };
    let minimal = match width {
        1 => true,
        3 => value >= 0xfd,
        5 => value > 0xffff,
        _ => value > 0xffff_ffff,
    };
    if !minimal {
        return None;
    }
    *at += width;
    Some(value)
}

fn write_compact_size(value: u64, out: &mut Vec<u8>) {
    match value {
        0..=0xfc => out.push(value as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(value as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(value as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
    // What: outputs survive a round trip; a non-minimal count, trailing
    // bytes, a truncated script, a value over the money supply and a count
    // larger than the bytes allow are refused.
    // Look here if: parse_outputs or serialize_outputs changes.
    #[test]
    fn outputs_round_trip_and_reject_trailing_bytes_and_overflow() {
        let outputs = vec![
            (304_734_375, vec![0x76, 0xa9, 0x14]),
            (0, Vec::new()),
            (4_687_500, vec![0x51; 300]),
        ];
        let bytes = serialize_outputs(&outputs);
        assert_eq!(bytes[0], 3);
        assert_eq!(parse_outputs(&bytes).unwrap(), outputs);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(parse_outputs(&trailing).is_err());
        assert!(parse_outputs(&bytes[..bytes.len() - 1]).is_err());
        let mut padded = vec![0xfd, 3, 0];
        padded.extend_from_slice(&bytes[1..]);
        assert!(parse_outputs(&padded).is_err(), "a non-minimal count");
        let mut rich = serialize_outputs(&[(MAX_MONEY, vec![0x51])]);
        assert_eq!(parse_outputs(&rich).unwrap()[0].0, MAX_MONEY);
        rich[1] = rich[1].wrapping_add(1);
        assert!(parse_outputs(&rich).is_err(), "over the money supply");
        assert!(parse_outputs(&[0xfe, 0xff, 0xff, 0xff, 0x7f]).is_err());
        assert_eq!(parse_outputs(&[0]).unwrap(), Vec::new());
    }
}
