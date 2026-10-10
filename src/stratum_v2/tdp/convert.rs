//! #### PR #42
//! Template Distribution messages from and to a `BchTemplate`, with no I/O:
//! what a template client receives, and the block its solution makes.

use super::super::{
    channel::VERSION_ROLLING_MASK,
    template::{double_sha256, fold, meets_target, BchTemplate},
    wire::encoded,
};
use stratum_core::{
    binary_sv2::{Seq064K, B016M, U256},
    bitcoin::{consensus, Transaction},
    codec_sv2::SerializedFrame,
    template_distribution_sv2::{
        NewTemplate, RequestTransactionDataError, RequestTransactionDataSuccess, SetNewPrevHash,
        SubmitSolution, MESSAGE_TYPE_NEW_TEMPLATE, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR,
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS, MESSAGE_TYPE_SET_NEW_PREV_HASH,
    },
};

/// SV2's limits on one `RequestTransactionData.Success` frame.
const MAX_PAYLOAD: u64 = 16_777_215;
const MAX_TRANSACTIONS: usize = 65_535;

/// `NewTemplate`: the coinbase prefix is the BIP34 height push alone, and a
/// BCH template asks for no coinbase outputs (no segwit commitment), so the
/// client's outputs take the whole value.
pub fn new_template(
    id: u64,
    future: bool,
    template: &BchTemplate,
) -> Result<SerializedFrame, String> {
    let prefix = template.height_push();
    let path: Vec<U256> = template.merkle_path().iter().map(Into::into).collect();
    encoded(
        NewTemplate {
            template_id: id,
            future_template: future,
            version: template.version,
            coinbase_tx_version: 2,
            coinbase_prefix: prefix
                .as_slice()
                .try_into()
                .map_err(|_| "coinbase prefix too long")?,
            coinbase_tx_input_sequence: u32::MAX,
            coinbase_tx_value_remaining: template.coinbase_value,
            coinbase_tx_outputs_count: 0,
            coinbase_tx_outputs: (&[][..])
                .try_into()
                .map_err(|_| "coinbase outputs too long")?,
            coinbase_tx_locktime: 0,
            merkle_path: path.try_into().map_err(|_| "merkle path too long")?,
        },
        MESSAGE_TYPE_NEW_TEMPLATE,
        false,
    )
}

/// `SetNewPrevHash`: the parent in header byte order, the node's current
/// time to start from, and the block target, `compact_target(nBits)`.
pub fn set_new_prev_hash(id: u64, template: &BchTemplate) -> Result<SerializedFrame, String> {
    encoded(
        SetNewPrevHash {
            template_id: id,
            prev_hash: (&template.previous_hash).into(),
            header_timestamp: template.current_time,
            n_bits: template.bits,
            target: (&template.target).into(),
        },
        MESSAGE_TYPE_SET_NEW_PREV_HASH,
        false,
    )
}

/// `RequestTransactionData.Success` with the template's transactions in
/// block order, or the error code to send instead: `template-too-large`
/// (a Pickaxe code) when they do not fit SV2's one frame of 16,777,215
/// bytes and 65,535 transactions, which a BCH block under ABLA can exceed.
pub fn transaction_data(id: u64, template: &BchTemplate) -> Result<SerializedFrame, &'static str> {
    let transactions = template.transactions();
    let payload = transactions
        .iter()
        .fold(12u64, |sum, tx| sum.saturating_add(3 + tx.len() as u64));
    if transactions.len() > MAX_TRANSACTIONS || payload > MAX_PAYLOAD {
        return Err("template-too-large");
    }
    let list = transactions
        .iter()
        .map(|tx| B016M::try_from(&tx[..]))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "template-too-large")?;
    let list: Seq064K<B016M> = list.try_into().map_err(|_| "template-too-large")?;
    encoded(
        RequestTransactionDataSuccess {
            template_id: id,
            excess_data: (&[][..]).try_into().map_err(|_| "template-too-large")?,
            transaction_list: list,
        },
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS,
        false,
    )
    .map_err(|_| "template-too-large")
}

/// `RequestTransactionData.Error` with `code`.
pub fn transaction_data_error(id: u64, code: &'static str) -> Result<SerializedFrame, String> {
    encoded(
        RequestTransactionDataError {
            template_id: id,
            error_code: code.try_into().map_err(|_| "error code too long")?,
        },
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR,
        false,
    )
}

/// A solution's block, and why Pickaxe's own check refused it, if it did.
pub struct Assembled {
    pub block: Vec<u8>,
    pub header: [u8; 80],
    pub refused: Option<&'static str>,
}

impl Assembled {
    /// The block's hash as nodes show it.
    pub fn hash(&self) -> String {
        let mut hash = double_sha256(&self.header);
        hash.reverse();
        hex::encode(hash)
    }
}

/// The block a `SubmitSolution` makes on `template`, assembled before its
/// checks: an error means there is no block to send (a malformed coinbase
/// or a witness other than BIP141's one item).
///
/// #### PR #42: the checks (D24)
/// What: the coinbase script must begin with the height push sent, the
/// version may differ only in the version-rolling bits, the header must
/// meet the block target, and the block must fit the size limit. A
/// solution failing one is still assembled, and the caller sends it to the
/// node at a bounded rate instead of saving it.
/// Why: the checks keep spam out of the relay journal, but a bug in them
/// must not drop a pool's only block submission; the node decides.
/// Look here if: a pool's block is counted "refused locally", or the node
/// gets blocks Pickaxe's checks refused.
pub fn assemble(template: &BchTemplate, solution: &SubmitSolution) -> Result<Assembled, String> {
    let coinbase = strip_bip141(solution.coinbase_tx.as_ref())?;
    let tx: Transaction =
        consensus::deserialize(&coinbase).map_err(|_| "malformed solution coinbase")?;
    if !tx.is_coinbase() || tx.input.len() != 1 {
        return Err("solution coinbase is not a coinbase".into());
    }
    let root = fold(double_sha256(&coinbase), template.merkle_path());
    let mut header = [0; 80];
    header[..4].copy_from_slice(&solution.version.to_le_bytes());
    header[4..36].copy_from_slice(&template.previous_hash);
    header[36..68].copy_from_slice(&root);
    header[68..72].copy_from_slice(&solution.header_timestamp.to_le_bytes());
    header[72..76].copy_from_slice(&template.bits.to_le_bytes());
    header[76..].copy_from_slice(&solution.header_nonce.to_le_bytes());
    let refused = if !tx.input[0]
        .script_sig
        .as_bytes()
        .starts_with(&template.height_push())
    {
        Some("coinbase-prefix")
    } else if (solution.version ^ template.version) & !VERSION_ROLLING_MASK != 0 {
        Some("version")
    } else if !meets_target(&double_sha256(&header), &template.target) {
        Some("high-hash")
    } else if coinbase.len() as u64 > template.coinbase_budget() {
        Some("block-size")
    } else {
        None
    };
    Ok(Assembled {
        block: template.assemble(&coinbase, &header),
        header,
        refused,
    })
}

/// #### PR #42: the BIP141 strip (D20)
/// What: a coinbase whose bytes 4 and 5 are 00 01 (BIP141's marker and
/// flag) loses them and its one 32-byte witness item; any other witness is
/// refused. A coinbase without them is taken as it is.
/// Why: SRI's job factory always gives the coinbase a 32-byte witness, and
/// its pool submits it. BCH has no witnesses and its txid commits to every
/// byte, so the block carries the coinbase without them; Pickaxe never
/// writes BIP141 bytes.
/// Look here if: an SRI pool's block is refused as malformed, or its
/// coinbase's txid differs from the pool's.
pub fn strip_bip141(coinbase: &[u8]) -> Result<Vec<u8>, String> {
    if coinbase.get(4..6) != Some(&[0, 1][..]) {
        return Ok(coinbase.to_vec());
    }
    let mut tx: Transaction =
        consensus::deserialize(coinbase).map_err(|_| "malformed solution coinbase")?;
    let [input] = tx.input.as_mut_slice() else {
        return Err("solution coinbase is not a coinbase".into());
    };
    if input.witness.iter().map(<[u8]>::len).ne([32]) {
        return Err("solution coinbase has a witness other than BIP141's".into());
    }
    input.witness.clear();
    let stripped = consensus::serialize(&tx);
    // Marker and flag (2), item count (1), item length (1), item (32).
    if stripped.len() + 36 != coinbase.len() {
        return Err("solution coinbase is not canonical".into());
    }
    Ok(stripped)
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::super::{
        template::Coinbase,
        template_tests::{rpc_template, transaction},
    };
    use super::*;
    use serde_json::json;
    use stratum_core::{
        binary_sv2,
        bitcoin::{hashes::Hash as _, Block},
    };

    /// A coinbase as SRI's job factory builds one: version 2, a script of
    /// `prefix` and `extranonce`, one P2PKH output taking `value`, and with
    /// `witness` BIP141's marker, flag and one 32-byte zero item.
    pub(in crate::stratum_v2::tdp) fn sri_coinbase(
        prefix: &[u8],
        extranonce: &[u8],
        value: u64,
        witness: bool,
    ) -> Vec<u8> {
        let mut script = prefix.to_vec();
        script.extend_from_slice(extranonce);
        let mut bytes = 2u32.to_le_bytes().to_vec();
        if witness {
            bytes.extend([0, 1]);
        }
        bytes.push(1);
        bytes.extend([0; 32]);
        bytes.extend(u32::MAX.to_le_bytes());
        bytes.push(script.len() as u8);
        bytes.extend(script);
        bytes.extend(u32::MAX.to_le_bytes());
        bytes.push(1);
        bytes.extend(value.to_le_bytes());
        bytes.extend([25, 0x76, 0xa9, 0x14]);
        bytes.extend([0x56; 20]);
        bytes.extend([0x88, 0xac]);
        if witness {
            bytes.extend([1, 32]);
            bytes.extend([0; 32]);
        }
        bytes.extend(0u32.to_le_bytes());
        bytes
    }

    pub(in crate::stratum_v2::tdp) fn solution(
        id: u64,
        coinbase: &[u8],
        version: u32,
        time: u32,
        nonce: u32,
    ) -> SubmitSolution<'_> {
        SubmitSolution {
            template_id: id,
            version,
            header_timestamp: time,
            header_nonce: nonce,
            coinbase_tx: coinbase.try_into().unwrap(),
        }
    }

    fn template() -> BchTemplate {
        BchTemplate::from_rpc(&rpc_template()).unwrap()
    }

    // #### PR #42
    // What: NewTemplate and SetNewPrevHash for the test template are byte
    // for byte the design's layouts: 45 and 80 bytes, the prefix the height
    // push alone, no outputs, the target compact(nBits).
    // Look here if: new_template or set_new_prev_hash changes.
    #[test]
    fn new_template_and_set_new_prev_hash_golden_bytes() {
        let mut frame = new_template(1, true, &template()).unwrap();
        assert_eq!(frame.header().msg_type(), 0x71);
        assert_eq!(
            hex::encode(frame.payload()),
            concat!(
                "0100000000000000",
                "01",
                "00000020",
                "02000000",
                "040315f904",
                "ffffffff",
                "205fa01200000000",
                "00000000",
                "0000",
                "00000000",
                "00"
            )
        );
        let mut frame = set_new_prev_hash(1, &template()).unwrap();
        assert_eq!(frame.header().msg_type(), 0x72);
        assert_eq!(
            hex::encode(frame.payload()),
            format!(
                "0100000000000000{}0af15365ffff7f20{}ffff7f",
                "ab".repeat(32),
                "00".repeat(29)
            )
        );
    }

    // #### PR #42
    // What: a solution's block is the template's block with the client's
    // coinbase, stripped of BIP141's marker, flag and one 32-byte item; a
    // wrong prefix, a version outside the rolling bits or a header above the
    // target is still assembled but marked refused; any other witness, or a
    // coinbase that is not one, has no block.
    // Look here if: assemble or strip_bip141 changes.
    #[test]
    fn assemble_rebuilds_the_block_and_refuses_bad_prefix_version_or_pow() {
        let mut raw = rpc_template();
        let mut txs: Vec<_> = (1..=3).map(transaction).collect();
        txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
        raw["transactions"] = json!(txs);
        let template = BchTemplate::from_rpc(&raw).unwrap();
        let prefix = template.height_push();
        let value = template.coinbase_value;
        let plain = sri_coinbase(&prefix, &[7; 16], value, false);
        let witness = sri_coinbase(&prefix, &[7; 16], value, true);
        assert_eq!(strip_bip141(&witness).unwrap(), plain);
        assert_eq!(strip_bip141(&plain).unwrap(), plain);
        let time = template.current_time;
        let rolled = template.version ^ 0x2000;
        let solved = (0..10_000)
            .find(|nonce| {
                assemble(&template, &solution(1, &witness, rolled, time, *nonce))
                    .unwrap()
                    .refused
                    .is_none()
            })
            .unwrap();
        let assembled = assemble(&template, &solution(1, &witness, rolled, time, solved)).unwrap();
        let block: Block = consensus::deserialize(&assembled.block).unwrap();
        assert!(block.check_merkle_root());
        assert!(block.header.validate_pow(block.header.target()).is_ok());
        assert_eq!(block.txdata.len(), 4);
        assert_eq!(consensus::serialize(&block.txdata[0]), plain);
        assert!(block.txdata[0].input[0].witness.is_empty());
        assert_eq!(assembled.hash(), block.block_hash().to_string());
        let coinbase = Coinbase {
            bytes: plain.clone(),
            merkle_root: block.header.merkle_root.to_byte_array(),
        };
        assert_eq!(
            template.block(&coinbase, assembled.header).unwrap(),
            assembled.block
        );
        // Refused, yet assembled: the node may still decide.
        let other = sri_coinbase(&[3, 1, 2, 3], &[7; 16], value, false);
        let refused = |coinbase: &[u8], version, nonce| {
            assemble(&template, &solution(1, coinbase, version, time, nonce))
                .unwrap()
                .refused
        };
        assert_eq!(refused(&other, rolled, 0), Some("coinbase-prefix"));
        assert_eq!(
            refused(&plain, template.version ^ 0x8000_0000, solved),
            Some("version")
        );
        assert!((0..10_000).any(|nonce| refused(&plain, rolled, nonce) == Some("high-hash")));
        // No block: another witness, or not a coinbase.
        let at = witness.len() - 4 - 34;
        let mut two_items = witness.clone();
        two_items[at] = 2;
        two_items.splice(at + 34..at + 34, [1, 9]);
        let mut short_item = witness.clone();
        short_item[at + 1] = 31;
        short_item.remove(at + 2);
        let mut spends = plain.clone();
        spends[5] = 1;
        for bad in [two_items, short_item, spends, vec![2, 0, 0, 0, 0, 1]] {
            assert!(
                assemble(&template, &solution(1, &bad, rolled, time, 0)).is_err(),
                "{}",
                hex::encode(&bad)
            );
        }
    }

    // #### PR #42
    // What: transaction data is one SV2 frame: up to 16,777,215 payload
    // bytes and 65,535 transactions; beyond either it is template-too-large.
    // Look here if: transaction_data changes.
    #[test]
    fn transaction_data_fits_one_frame_or_is_too_large() {
        let small = template().with_transactions(vec![vec![1, 2, 3], vec![4]]);
        let mut frame = transaction_data(9, &small).unwrap();
        assert_eq!(frame.header().msg_type(), 0x74);
        let data: RequestTransactionDataSuccess = binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(data.template_id, 9);
        assert!(data.excess_data.as_ref().is_empty());
        let list: Vec<&[u8]> = data.transaction_list.iter().map(|tx| tx.as_ref()).collect();
        assert_eq!(list, [&[1u8, 2, 3][..], &[4][..]]);
        let at_limit = template().with_transactions(vec![vec![0; 16_777_200]]);
        let frame = transaction_data(9, &at_limit).unwrap();
        assert_eq!(frame.header().payload_length(), 16_777_215);
        for too_large in [vec![vec![0; 16_777_201]], vec![vec![0; 1]; 65_536]] {
            assert_eq!(
                transaction_data(9, &template().with_transactions(too_large)).err(),
                Some("template-too-large")
            );
        }
    }
}
