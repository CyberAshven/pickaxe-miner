//! #### PR #42
//! A Full-Template declaration's coinbase, split as Job Declaration carries
//! it, and the job a pool keeps once it has checked one: the block its node
//! validates, and the block a PushSolution names.

use super::{codec::parse_outputs, Refusal};
use crate::stratum_v2::template::{double_sha256, fold, meets_target, BchTemplate, Coinbase, Hash};
use std::sync::Arc;

/// A declared coinbase: `prefix` runs through the script's head (up to the
/// whole extranonce), `suffix` from the input's sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseShape {
    pub tx_version: u32,
    /// The script's head: the height push and whatever follows it before
    /// the extranonce.
    pub head: Vec<u8>,
    /// The whole script's length: the head and the whole extranonce.
    pub script_len: usize,
    pub sequence: u32,
    pub outputs: Vec<(u64, Vec<u8>)>,
    /// The outputs as declared (count and outputs).
    pub outputs_bytes: Vec<u8>,
    pub locktime: u32,
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
}

impl CoinbaseShape {
    /// Parses a declaration's prefix and suffix.
    pub fn parse(prefix: &[u8], suffix: &[u8]) -> Result<Self, Refusal> {
        let invalid = |details: &str| Refusal::new("invalid-coinbase-tx", details);
        let word = |bytes: Option<&[u8]>| {
            bytes
                .and_then(|bytes| bytes.try_into().ok())
                .map(u32::from_le_bytes)
        };
        let tx_version =
            word(prefix.get(..4)).ok_or_else(|| invalid("the coinbase prefix is too short"))?;
        // BIP141's marker and flag: a segwit coinbase, which a BCH block
        // cannot carry.
        if prefix.get(4..6) == Some(&[0, 1][..]) {
            return Err(invalid("segwit coinbase"));
        }
        if !matches!(tx_version, 1 | 2) {
            return Err(invalid("the coinbase version must be 1 or 2"));
        }
        if prefix.get(4) != Some(&1)
            || prefix.get(5..37) != Some(&[0; 32][..])
            || prefix.get(37..41) != Some(&[0xff; 4][..])
        {
            return Err(Refusal::new(
                "invalid-coinbase-tx-input",
                "a coinbase has one input, which spends the null outpoint",
            ));
        }
        let script_len = usize::from(
            *prefix
                .get(41)
                .ok_or_else(|| invalid("the coinbase prefix is too short"))?,
        );
        if script_len > 100 {
            return Err(invalid("the coinbase script exceeds 100 bytes"));
        }
        let head = prefix[42..].to_vec();
        if !script_len
            .checked_sub(head.len())
            .is_some_and(|extranonce| (1..=32).contains(&extranonce))
        {
            return Err(invalid("the extranonce must be 1 to 32 bytes"));
        }
        let sequence =
            word(suffix.get(..4)).ok_or_else(|| invalid("the coinbase suffix is too short"))?;
        let outputs_end = suffix
            .len()
            .checked_sub(4)
            .filter(|end| *end >= 4)
            .ok_or_else(|| invalid("the coinbase suffix is too short"))?;
        let outputs_bytes = suffix[4..outputs_end].to_vec();
        let outputs =
            parse_outputs(&outputs_bytes).map_err(|_| invalid("malformed coinbase outputs"))?;
        let locktime = word(Some(&suffix[outputs_end..]))
            .ok_or_else(|| invalid("the coinbase suffix is too short"))?;
        if prefix.len() + (script_len - head.len()) + suffix.len() < 65 {
            return Err(invalid("the coinbase is under 65 bytes"));
        }
        Ok(Self {
            tx_version,
            head,
            script_len,
            sequence,
            outputs,
            outputs_bytes,
            locktime,
            prefix: prefix.to_vec(),
            suffix: suffix.to_vec(),
        })
    }

    /// The whole extranonce's length.
    pub fn extranonce_len(&self) -> usize {
        self.script_len - self.head.len()
    }

    /// The coinbase with `extranonce` (its whole length).
    pub fn coinbase(&self, extranonce: &[u8]) -> Option<Vec<u8>> {
        if extranonce.len() != self.extranonce_len() {
            return None;
        }
        let mut bytes = self.prefix.clone();
        bytes.extend_from_slice(extranonce);
        bytes.extend_from_slice(&self.suffix);
        Some(bytes)
    }
}

/// Transaction ids in BCH's canonical order: strictly ascending as BCHN
/// compares them, most significant byte first (the display order).
pub fn ctor_ordered(txids: &[Hash]) -> bool {
    txids
        .windows(2)
        .all(|pair| pair[0].iter().rev().cmp(pair[1].iter().rev()).is_lt())
}

/// A PushSolution's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solution {
    /// The whole extranonce: the pool channel's prefix and the rolled bytes.
    pub extranonce: Vec<u8>,
    pub prev_hash: Hash,
    pub nonce: u32,
    pub ntime: u32,
    pub nbits: u32,
    pub version: u32,
}

/// A declaration the pool checked: its coinbase shape and its template (the
/// pool's parent, bits and limits with the declared version and
/// transactions), which builds whole blocks.
pub struct DeclaredJob {
    pub shape: CoinbaseShape,
    pub template: Arc<BchTemplate>,
}

impl DeclaredJob {
    fn coinbase(&self, extranonce: &[u8]) -> Option<Coinbase> {
        let bytes = self.shape.coinbase(extranonce)?;
        let merkle_root = fold(double_sha256(&bytes), self.template.merkle_path());
        Some(Coinbase { bytes, merkle_root })
    }

    /// Whether the coinbase fits the pool's block size beside the
    /// transactions.
    pub fn fits(&self) -> bool {
        (self.shape.prefix.len() + self.shape.extranonce_len() + self.shape.suffix.len()) as u64
            <= self.template.coinbase_budget()
    }

    /// The block the pool's node validates: a zero extranonce, the
    /// template's time and nonce 0 (the node does not check proof of work);
    /// refused when it exceeds the pool's block size.
    pub fn candidate(&self) -> Result<Vec<u8>, Refusal> {
        let coinbase = self
            .coinbase(&vec![0; self.shape.extranonce_len()])
            .ok_or_else(|| Refusal::new("invalid-coinbase-tx", "malformed coinbase"))?;
        let header = self
            .template
            .header(
                &coinbase,
                self.template.version,
                self.template.current_time,
                0,
            )
            .map_err(|_| Refusal::new("invalid-job", "invalid header time"))?;
        self.template
            .block(&coinbase, header)
            .map_err(|_| Refusal::new("invalid-job", "too large for this pool's block size"))
    }

    /// The block `solution` names when it is one of this job's: its coinbase
    /// rebuilt with the pushed extranonce, on this job's parent and bits,
    /// with a hash that meets the bits' target. Returns the block's hash too.
    pub fn solved(&self, solution: &Solution) -> Option<(Hash, Vec<u8>)> {
        if solution.prev_hash != self.template.previous_hash || solution.nbits != self.template.bits
        {
            return None;
        }
        let coinbase = self.coinbase(&solution.extranonce)?;
        let header = self
            .template
            .header(&coinbase, solution.version, solution.ntime, solution.nonce)
            .ok()?;
        let hash = double_sha256(&header);
        if !meets_target(&hash, &self.template.target) {
            return None;
        }
        Some((hash, self.template.block(&coinbase, header).ok()?))
    }
}

#[cfg(test)]
pub(in crate::stratum_v2) mod tests {
    use super::*;
    use crate::stratum_v2::template_tests::{rpc_template, transaction};
    use serde_json::{json, Value};

    /// A declaration's prefix and suffix with a `head` and an extranonce of
    /// `extranonce` bytes, paying 312,500,000 to OP_1.
    pub(in crate::stratum_v2) fn shape_bytes(head: &[u8], extranonce: usize) -> (Vec<u8>, Vec<u8>) {
        let mut prefix = 2u32.to_le_bytes().to_vec();
        prefix.push(1);
        prefix.extend([0; 32]);
        prefix.extend([0xff; 4]);
        prefix.push((head.len() + extranonce) as u8);
        prefix.extend(head);
        let mut suffix = u32::MAX.to_le_bytes().to_vec();
        suffix.extend(super::super::codec::serialize_outputs(&[(
            312_500_000,
            vec![0x51],
        )]));
        suffix.extend(0u32.to_le_bytes());
        (prefix, suffix)
    }

    /// A node template with `count` transactions in CTOR order.
    pub(in crate::stratum_v2) fn context(count: u32) -> BchTemplate {
        let mut raw = rpc_template();
        let mut txs: Vec<Value> = (1..=count).map(transaction).collect();
        txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
        raw["transactions"] = json!(txs);
        BchTemplate::from_rpc(&raw).unwrap()
    }

    // #### PR #42
    // What: a declared coinbase parses into its head, extranonce length and
    // outputs, and rebuilds with an extranonce of that length; a segwit
    // coinbase ("segwit coinbase"), a version other than 1 or 2, a non-null
    // prevout, a script over 100 bytes, no extranonce, trailing output bytes
    // and a coinbase under 65 bytes are refused with the spec's codes.
    // Look here if: CoinbaseShape changes.
    #[test]
    fn wrong_versions_null_prevouts_long_scripts_and_bip141_are_refused() {
        let head = [3, 0x15, 0xf9, 0x04, b'j', b'd'];
        let (prefix, suffix) = shape_bytes(&head, 32);
        let shape = CoinbaseShape::parse(&prefix, &suffix).unwrap();
        assert_eq!(shape.head, head);
        assert_eq!(shape.extranonce_len(), 32);
        assert_eq!(shape.outputs, vec![(312_500_000, vec![0x51])]);
        let coinbase = shape.coinbase(&[7; 32]).unwrap();
        assert_eq!(coinbase.len(), prefix.len() + 32 + suffix.len());
        assert!(shape.coinbase(&[7; 31]).is_none());
        let refused =
            |prefix: &[u8], suffix: &[u8]| CoinbaseShape::parse(prefix, suffix).unwrap_err();
        let mut segwit = prefix.clone();
        segwit.splice(4..4, [0, 1]);
        assert_eq!(
            refused(&segwit, &suffix),
            Refusal::new("invalid-coinbase-tx", "segwit coinbase")
        );
        let mut version = prefix.clone();
        version[0] = 3;
        assert_eq!(refused(&version, &suffix).code, "invalid-coinbase-tx");
        let mut spends = prefix.clone();
        spends[5] = 1;
        assert_eq!(refused(&spends, &suffix).code, "invalid-coinbase-tx-input");
        let (long, _) = shape_bytes(&head, 95);
        assert_eq!(
            refused(&long, &suffix).details,
            "the coinbase script exceeds 100 bytes"
        );
        let (none, _) = shape_bytes(&head, 0);
        assert_eq!(
            refused(&none, &suffix).details,
            "the extranonce must be 1 to 32 bytes"
        );
        let mut trailing = suffix.clone();
        trailing.insert(suffix.len() - 4, 0);
        assert_eq!(
            refused(&prefix, &trailing).details,
            "malformed coinbase outputs"
        );
        let mut tiny = 2u32.to_le_bytes().to_vec();
        tiny.push(1);
        tiny.extend([0; 32]);
        tiny.extend([0xff; 4]);
        tiny.extend([2, 0x51]);
        let mut empty = u32::MAX.to_le_bytes().to_vec();
        empty.push(0);
        empty.extend(0u32.to_le_bytes());
        assert_eq!(
            refused(&tiny, &empty).details,
            "the coinbase is under 65 bytes"
        );
    }

    // #### PR #42
    // What: canonical order is ascending display order, as BCHN and
    // `from_rpc` compare; equal or descending neighbours are refused.
    // Look here if: ctor_ordered changes.
    #[test]
    fn ctor_order_matches_bchn_and_from_rpc() {
        let template = context(9);
        let ids = template.transaction_ids().to_vec();
        assert!(ctor_ordered(&ids), "from_rpc's order");
        assert!(ctor_ordered(&[]));
        let mut twice = ids.clone();
        twice.push(*ids.last().unwrap());
        assert!(!ctor_ordered(&twice));
        let mut reversed = ids;
        reversed.reverse();
        assert!(!ctor_ordered(&reversed));
    }

    // #### PR #42
    // What: for 0 to 17 transactions, a Job Declaration template's declared
    // prefix and suffix around any extranonce give the local channels'
    // coinbase, the declared template (the same transactions by id) has the
    // template's merkle path and builds the same block, and the block a
    // solution names is found by proof of work and only on its parent.
    // Look here if: declared_parts, BchTemplate::declared or DeclaredJob
    // change.
    #[test]
    fn the_declared_path_equals_the_template_parts_path() {
        use crate::stratum_v2::{jd::plan::JdPlan, jd::token::PoolRates, template::meets_target};
        for count in 0..=17 {
            let mut template = context(count);
            template.declare(Arc::new(JdPlan {
                serial: 1,
                upstream_prefix: vec![9; 16],
                pad: Vec::new(),
                scripts: vec![vec![0x51], vec![0x52]],
                rates: PoolRates {
                    donation_bps: 150,
                    fee_bps: 0,
                    donation_output: Some(1),
                    fee_output: None,
                },
                pool_target: [0xff; 32],
            }));
            let parts = template.declared_parts().unwrap();
            let shape = CoinbaseShape::parse(&parts.prefix, &parts.suffix).unwrap();
            assert_eq!(shape.extranonce_len(), 32);
            let mut extranonce = vec![9; 16];
            extranonce.extend([5; 16]);
            let local = template
                .coinbase_with_payout(
                    crate::config::MiningNetwork::Chipnet,
                    "",
                    None,
                    &extranonce,
                    Default::default(),
                )
                .unwrap();
            assert_eq!(shape.coinbase(&extranonce).unwrap(), local.bytes);
            let declared = BchTemplate::declared(
                &template,
                template.version,
                template.transactions().to_vec(),
                template.transaction_ids().to_vec(),
            );
            assert_eq!(declared.merkle_path(), template.merkle_path(), "{count}");
            assert!(declared.is_declared());
            let job = DeclaredJob {
                shape,
                template: Arc::new(declared),
            };
            let candidate = job.candidate().unwrap();
            let header: [u8; 80] = candidate[..80].try_into().unwrap();
            let zero = template
                .coinbase_with_payout(
                    crate::config::MiningNetwork::Chipnet,
                    "",
                    None,
                    &[0; 32],
                    Default::default(),
                )
                .unwrap();
            assert_eq!(candidate, template.block(&zero, header).unwrap());
            let mut solution = Solution {
                extranonce,
                prev_hash: template.previous_hash,
                nonce: 0,
                ntime: template.current_time,
                nbits: template.bits,
                version: template.version,
            };
            let coinbase = job.coinbase(&solution.extranonce).unwrap();
            solution.nonce = (0..1_000)
                .find(|nonce| {
                    let header = template
                        .header(&coinbase, solution.version, solution.ntime, *nonce)
                        .unwrap();
                    meets_target(&double_sha256(&header), &template.target)
                })
                .unwrap();
            let (_, block) = job.solved(&solution).unwrap();
            assert_eq!(
                block,
                template
                    .block(&local, block[..80].try_into().unwrap())
                    .unwrap()
            );
            let elsewhere = Solution {
                prev_hash: [1; 32],
                ..solution.clone()
            };
            assert!(job.solved(&elsewhere).is_none());
            let short = Solution {
                extranonce: vec![0; 31],
                ..solution
            };
            assert!(job.solved(&short).is_none());
        }
    }
}
