//! #### PR #42
//! Header-shaped token jobs (SAFA layout v1, draft): the payout script a
//! grinder's commitment pays, which is also the "coinbase" devices hash, and
//! the record of a win.
//!
//! A SAFA commitment pays `HASH256(L)` for the grinder's locking bytecode
//! `L`. Devices compute their header's merkle root as `SHA256d(coinbase)`
//! when the merkle branch is empty, so a job whose coinbase is `L` itself
//! lets any SV1 or SV2 device mine SAFA unchanged. `L` is a keyless
//! forwarder (P2S): a push of salt bytes (a tag, the job id and the
//! extranonce), `OP_DROP`, then a rule that the output at the same index pays
//! a fixed destination the same tokens. Anyone may sweep it, and only to
//! that destination.

use super::OutPoint;

/// Bare P2S locking bytecode may take at most this many bytes.
pub const MAX_P2S: usize = 201;

// #### PR #42: the BIP141-safe tag
// What: every forwarder's salt starts with the 4-byte tag "PXH1" right
// after its push opcode, so byte 4 of a job's coinbase prefix is never 0x00.
// Why: SRI's translator reads bytes 4 and 5 of a coinbase prefix as a
// BIP141 marker and flag; a prefix of only the push and the job id is too
// short for that check, which fails, and a zero byte there would look like a
// segwit marker. Either way an SV1 device would get no job.
// Look here if: an SV1 device gets no SAFA job and the adapter logs a
// translation error.
/// The tag after the push opcode.
pub const TAG: [u8; 4] = *b"PXH1";
/// `OP_DROP`, which ends the salt.
const OP_DROP: u8 = 0x75;

/// The forwarder's rule for `destination` (a locking bytecode of at most 75
/// bytes): the output at this input's index pays `destination`, with this
/// input's token category and amount.
pub fn forwarder_body(destination: &[u8]) -> Result<Vec<u8>, &'static str> {
    if destination.is_empty() || destination.len() > 75 {
        return Err("a forwarder destination takes 1 to 75 bytes");
    }
    let mut body = vec![0xc0, 0xcd, destination.len() as u8];
    body.extend_from_slice(destination);
    body.extend_from_slice(&[
        0x88, // OP_EQUALVERIFY
        0xc0, 0xd1, 0xc0, 0xce, 0x88, // same category
        0xc0, 0xd3, 0xc0, 0xd0, 0x9c, // same amount (OP_NUMEQUAL)
    ]);
    Ok(body)
}

/// A job's coinbase around the extranonce on an extended channel (and SV1):
/// the prefix (`0x20`, the tag, the job id) and the suffix (`OP_DROP`, the
/// rule). The 32 pushed bytes are the tag, the job id, the channel's 16-byte
/// prefix and the device's 8.
pub fn extended_parts(job_id: u32, destination: &[u8]) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let mut prefix = vec![0x20];
    prefix.extend_from_slice(&TAG);
    prefix.extend_from_slice(&job_id.to_le_bytes());
    let mut suffix = vec![OP_DROP];
    suffix.extend(forwarder_body(destination)?);
    if prefix.len() + 16 + 8 + suffix.len() > MAX_P2S {
        return Err("the forwarder exceeds 201 bytes");
    }
    Ok((prefix, suffix))
}

/// A standard channel's whole script: `0x18`, the tag, the job id, the
/// channel's 16-byte prefix, `OP_DROP` and the rule.
pub fn standard_script(
    job_id: u32,
    channel_prefix: &[u8; 16],
    destination: &[u8],
) -> Result<Vec<u8>, &'static str> {
    let mut script = vec![0x18];
    script.extend_from_slice(&TAG);
    script.extend_from_slice(&job_id.to_le_bytes());
    script.extend_from_slice(channel_prefix);
    script.push(OP_DROP);
    script.extend(forwarder_body(destination)?);
    if script.len() > MAX_P2S {
        return Err("the forwarder exceeds 201 bytes");
    }
    Ok(script)
}

/// `HeaderWin` v1: a win on a header-shaped token, as the journal keeps it.
/// ```text
/// 0 4 "CTMH" | 4 1 01 | 5 1 layout ('S' = SAFA v1) | 6 80 header
/// 86 2 age u16le | 88 36 thread outpoint (txid, vout u32le)
/// 124 1 n | 125 n payout locking bytecode (at most 201)
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderWin {
    pub layout: u8,
    pub header: [u8; 80],
    pub age: u16,
    pub thread: OutPoint,
    pub script: Vec<u8>,
}

const WIN_MAGIC: [u8; 4] = *b"CTMH";
/// The SAFA v1 layout.
pub const SAFA_V1: u8 = b'S';

impl HeaderWin {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = WIN_MAGIC.to_vec();
        bytes.push(1);
        bytes.push(self.layout);
        bytes.extend_from_slice(&self.header);
        bytes.extend_from_slice(&self.age.to_le_bytes());
        bytes.extend_from_slice(&self.thread.txid);
        bytes.extend_from_slice(&self.thread.vout.to_le_bytes());
        bytes.push(self.script.len() as u8);
        bytes.extend_from_slice(&self.script);
        bytes
    }

    /// Decodes exactly one win, refusing any other version, a script over
    /// 201 bytes and trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 125 || bytes[..4] != WIN_MAGIC || bytes[4] != 1 {
            return Err("not a HeaderWin v1");
        }
        let length = usize::from(bytes[124]);
        if length > MAX_P2S || bytes.len() != 125 + length {
            return Err("a HeaderWin's script has the wrong length");
        }
        Ok(Self {
            layout: bytes[5],
            header: bytes[6..86].try_into().unwrap(),
            age: u16::from_le_bytes([bytes[86], bytes[87]]),
            thread: OutPoint {
                txid: bytes[88..120].try_into().unwrap(),
                vout: u32::from_le_bytes(bytes[120..124].try_into().unwrap()),
            },
            script: bytes[125..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stratum_v2::template::{double_sha256, fold};

    fn p2pkh() -> Vec<u8> {
        let mut destination = vec![0x76, 0xa9, 0x14];
        destination.extend([0x11; 20]);
        destination.extend([0x88, 0xac]);
        destination
    }

    // #### PR #42
    // What: the forwarder for a P2PKH destination is 73 bytes on extended
    // channels and SV1 (salt 0x22×28 hashes to 10243fae…) and 65 on standard
    // channels (salt 0x22×20 hashes to ea83f927…), a P2SH32 destination
    // also fits P2S's 201 bytes, and the parts split at the 24 extranonce
    // bytes.
    // Look here if: the forwarder layout changes.
    #[test]
    fn forwarder_fits_p2s_and_splits_at_the_extranonce() {
        let (prefix, suffix) = extended_parts(0x2222_2222, &p2pkh()).unwrap();
        let mut script = prefix.clone();
        script.extend([0x22; 24]);
        script.extend(&suffix);
        assert_eq!(script.len(), 73);
        assert_eq!(
            prefix,
            [0x20, b'P', b'X', b'H', b'1', 0x22, 0x22, 0x22, 0x22]
        );
        assert_eq!(
            hex::encode(double_sha256(&script)),
            "10243fae278cba699c3ca67039a035f1c92fc75890a2b777ba0c01b17786d8cb"
        );
        let standard = standard_script(0x2222_2222, &[0x22; 16], &p2pkh()).unwrap();
        assert_eq!(standard.len(), 65);
        assert_eq!(
            hex::encode(double_sha256(&standard)),
            "ea83f92729d0dd2bd8fe78b99d8af753681f3430ea2497aa0f55e012a8e354a4"
        );
        let mut p2sh32 = vec![0xaa, 0x20];
        p2sh32.extend([0x33; 32]);
        p2sh32.push(0x87);
        let (prefix, suffix) = extended_parts(7, &p2sh32).unwrap();
        assert!(prefix.len() + 24 + suffix.len() <= MAX_P2S);
        assert!(forwarder_body(&[0x51; 76]).is_err());
        assert!(forwarder_body(&[]).is_err());
    }

    // #### PR #42
    // What: byte 4 of the prefix is the tag's, never 0x00, so SRI's BIP141
    // check finds no marker for any job id (it fails on a prefix too short
    // to hold one).
    // Look here if: the tag or the prefix changes.
    #[test]
    fn forwarder_prefix_never_trips_bip141_detection() {
        use stratum_core::channels_sv2::bip141::try_strip_bip141;
        for job_id in [0, 0xff, 0x00ff_0000, 0x8000_0000, u32::MAX] {
            let (prefix, suffix) = extended_parts(job_id, &p2pkh()).unwrap();
            assert!(
                matches!(try_strip_bip141(&prefix, &suffix), Ok(None)),
                "{job_id}"
            );
        }
        let mut untagged = vec![0x1c];
        untagged.extend(0u32.to_le_bytes());
        assert!(
            try_strip_bip141(&untagged, &[0x75]).is_err(),
            "without the tag the prefix is too short"
        );
    }

    // #### PR #42
    // What: a device's merkle root over the parts with an empty branch is
    // HASH256 of the whole script, which is the payout field SAFA checks.
    // Look here if: the parts or the job's merkle path change.
    #[test]
    fn device_merkle_with_empty_branch_equals_the_payout_field() {
        let (prefix, suffix) = extended_parts(9, &p2pkh()).unwrap();
        let mut coinbase = prefix;
        coinbase.extend([0x44; 16]);
        coinbase.extend([0x55; 8]);
        coinbase.extend(&suffix);
        assert_eq!(
            fold(double_sha256(&coinbase), &[]),
            double_sha256(&coinbase)
        );
    }

    // #### PR #42
    // What: a win round-trips through its 125 + n bytes; another magic or
    // version, a script over 201 bytes and trailing bytes are refused.
    // Look here if: HeaderWin changes.
    #[test]
    fn win_round_trips_and_rejects_trailing_bytes() {
        let win = HeaderWin {
            layout: SAFA_V1,
            header: [7; 80],
            age: 71,
            thread: OutPoint {
                txid: [8; 32],
                vout: 1,
            },
            script: standard_script(1, &[2; 16], &p2pkh()).unwrap(),
        };
        let bytes = win.encode();
        assert_eq!(bytes.len(), 125 + 65);
        assert_eq!(HeaderWin::decode(&bytes), Ok(win.clone()));
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(HeaderWin::decode(&trailing).is_err());
        let mut version = bytes.clone();
        version[4] = 2;
        assert!(HeaderWin::decode(&version).is_err());
        let mut long = bytes[..124].to_vec();
        long.push(202);
        long.extend([0; 202]);
        assert!(HeaderWin::decode(&long).is_err());
    }
}
