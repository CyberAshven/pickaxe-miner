//! SV2 extensions Pickaxe shares with the Knuth node (k-nuth/kth, docs/sv2.md):
//! Extensions Negotiation (0x0001), Worker-Specific Hashrate Tracking
//! (0x0002), Requested Payout Outputs (0x5052) and BCH large templates for
//! Job Declaration and Template Distribution (0x4C54). 0x5052 and 0x4C54 are
//! experimental identifiers, not in the SV2 extension registry; Knuth and
//! Pickaxe use the same ones.
//!
//! Every integer is little-endian (sv2-spec 3.1), as Knuth and the reference
//! implementation write them. Each decoder takes exactly one message: a
//! short buffer or a trailing byte is refused.
//!
//! ```text
//! 0x0001 RequestExtensions        (0x00) request_id U16 | SEQ0_64K[U16] requested
//! 0x0001 RequestExtensions.Success(0x01) request_id U16 | SEQ0_64K[U16] supported
//! 0x0001 RequestExtensions.Error  (0x02) request_id U16 | SEQ0_64K[U16] unsupported
//!                                         | SEQ0_64K[U16] required
//! 0x0002 TLV after SubmitSharesExtended:  U16 0x0002 | U8 0x01 | U16 length | UTF-8
//! 0x5052 RequestPayoutOutputs     (0x00) request_id U32 | B0_255 token
//!                                         | U64 available_payout_value | U32 height
//!                                         | U256 prev_hash | U32 nbits | B0_255 coinbase_prefix
//! 0x5052 RequestPayoutOutputs.Success (0x01) request_id U32 | U32 flags | U32 version
//!                                         | U32 sequence | U32 locktime | B0_64K outputs
//! 0x5052 RequestPayoutOutputs.Error   (0x02) request_id U32 | STR0_255 error_code
//! 0x5052 PayoutOutputsStale       (0x03) (empty)
//! 0x4C54 DeclareMiningJob.Transactions     (0x01) request_id U32 | SEQ0_64K[U256] txids
//! 0x4C54 ProvideMissingTransactions.Positions (0x02) request_id U32 | SEQ0_64K[U32] positions
//! 0x4C54 RequestTransactionData.Transactions  (0x03) template_id U64 | SEQ0_64K[B0_16M] transactions
//! 0x4C54 RequestTransactionData.Kept          (0x04) template_id U64 | U64 base_template_id
//!                                         | B0_16M kept
//! ```
//!
//! 0x4C54's 0x01 and 0x02 are Job Declaration's, 0x03 and 0x04 Template
//! Distribution's (Knuth's jd_messages.hpp, k-nuth/kth PR #879).

use super::{template::Hash, wire::extension_frame};
use stratum_core::codec_sv2::SerializedFrame;

/// Extensions Negotiation and its messages.
pub const NEGOTIATION: u16 = 0x0001;
pub const REQUEST_EXTENSIONS: u8 = 0x00;
pub const REQUEST_EXTENSIONS_SUCCESS: u8 = 0x01;
pub const REQUEST_EXTENSIONS_ERROR: u8 = 0x02;

/// Worker-Specific Hashrate Tracking: one TLV field after
/// SubmitSharesExtended, whose frame keeps extension type 0.
pub const WORKER_HASHRATE: u16 = 0x0002;
pub const USER_IDENTITY_FIELD: u8 = 0x01;
pub const MAX_USER_IDENTITY: usize = 32;
/// sv2-spec 3.4.3: a field longer than its extension allows refuses the
/// message.
pub const USER_IDENTITY_TOO_LONG: &str = "user-identity-too-long";

/// Requested Payout Outputs (experimental, "PR") and its messages.
pub const PAYOUT_OUTPUTS: u16 = 0x5052;
pub const REQUEST_PAYOUT_OUTPUTS: u8 = 0x00;
pub const REQUEST_PAYOUT_OUTPUTS_SUCCESS: u8 = 0x01;
pub const REQUEST_PAYOUT_OUTPUTS_ERROR: u8 = 0x02;
pub const PAYOUT_OUTPUTS_STALE: u8 = 0x03;
/// Bit 0 of RequestPayoutOutputs.Success's flags; every other bit is clear.
pub const EXACT_OUTPUTS: u32 = 1;
pub const STALE_PAYOUT_OUTPUTS: &str = "stale-payout-outputs";

/// BCH large templates for Job Declaration (experimental, "LT") and its
/// messages.
pub const LARGE_TEMPLATES: u16 = 0x4C54;
pub const DECLARE_MINING_JOB_TRANSACTIONS: u8 = 0x01;
pub const PROVIDE_MISSING_TRANSACTIONS_POSITIONS: u8 = 0x02;
/// The most items one SEQ0_64K list holds: a page of transaction ids, and
/// DeclareMiningJob's own list.
pub const SEQ_MAX: usize = 65_535;
/// BCH large templates on Template Distribution: a page of a template's
/// transactions, and the bitmap that starts a push of only what its base
/// lacks.
pub const REQUEST_TRANSACTION_DATA_TRANSACTIONS: u8 = 0x03;
pub const REQUEST_TRANSACTION_DATA_KEPT: u8 = 0x04;
/// The most payload one SV2 frame carries: its length is a U24 (sv2-spec
/// 3.2), Knuth's `jd_frame_max_payload`. Also the most bytes one B0_16M
/// holds.
pub const FRAME_MAX: usize = 16_777_215;

/// Reads one message's fields in order.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_le_bytes)
    }

    fn b0_255(&mut self) -> Option<&'a [u8]> {
        let length = self.u8()?;
        self.take(usize::from(length))
    }

    fn b0_64k(&mut self) -> Option<&'a [u8]> {
        let length = self.u16()?;
        self.take(usize::from(length))
    }

    fn b0_16m(&mut self) -> Option<&'a [u8]> {
        let [low, middle, high] = self.array()?;
        self.take(u32::from_le_bytes([low, middle, high, 0]) as usize)
    }

    /// SEQ0_64K of `N`-byte items, its count bounded by what is left before
    /// anything is allocated.
    fn seq<const N: usize>(&mut self) -> Option<Vec<[u8; N]>> {
        let count = usize::from(self.u16()?);
        let items = self.take(count.checked_mul(N)?)?;
        Some(items.as_chunks::<N>().0.to_vec())
    }

    fn seq_u16(&mut self) -> Option<Vec<u16>> {
        Some(
            self.seq::<2>()?
                .into_iter()
                .map(u16::from_le_bytes)
                .collect(),
        )
    }

    fn seq_u32(&mut self) -> Option<Vec<u32>> {
        Some(
            self.seq::<4>()?
                .into_iter()
                .map(u32::from_le_bytes)
                .collect(),
        )
    }

    /// `value` when every byte was read.
    fn end<T>(self, value: T) -> Option<T> {
        self.0.is_empty().then_some(value)
    }
}

/// Writes one message's fields in order.
#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.extend_from_slice(bytes);
        self
    }

    fn u16(&mut self, value: u16) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn b0_16m(&mut self, bytes: &[u8]) -> Result<&mut Self, &'static str> {
        if bytes.len() > FRAME_MAX {
            return Err("B0_16M field over 16,777,215 bytes");
        }
        self.bytes(&(bytes.len() as u32).to_le_bytes()[..3]);
        Ok(self.bytes(bytes))
    }

    fn b0_255(&mut self, bytes: &[u8]) -> Result<&mut Self, &'static str> {
        let length = u8::try_from(bytes.len()).map_err(|_| "B0_255 field over 255 bytes")?;
        self.0.push(length);
        Ok(self.bytes(bytes))
    }

    fn b0_64k(&mut self, bytes: &[u8]) -> Result<&mut Self, &'static str> {
        let length = u16::try_from(bytes.len()).map_err(|_| "B0_64K field over 65,535 bytes")?;
        self.u16(length);
        Ok(self.bytes(bytes))
    }

    fn count(&mut self, count: usize) -> Result<&mut Self, &'static str> {
        let count = u16::try_from(count).map_err(|_| "SEQ0_64K list over 65,535 items")?;
        Ok(self.u16(count))
    }

    fn seq_u16(&mut self, items: &[u16]) -> Result<&mut Self, &'static str> {
        self.count(items.len())?;
        items.iter().for_each(|item| {
            self.u16(*item);
        });
        Ok(self)
    }

    fn done(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

/// A message of one of these extensions: its type, its message type, and
/// its exact bytes.
pub trait ExtensionMessage: Sized {
    const EXTENSION: u16;
    const MESSAGE_TYPE: u8;
    fn encode(&self) -> Result<Vec<u8>, &'static str>;
    fn decode(payload: &[u8]) -> Option<Self>;

    /// The whole frame, with the channel bit clear (none of these messages
    /// is a channel message).
    fn frame(&self) -> Result<SerializedFrame, String> {
        extension_frame(Self::EXTENSION, Self::MESSAGE_TYPE, false, &self.encode()?)
    }
}

/// RequestExtensions: the extensions a client asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestExtensions {
    pub request_id: u16,
    pub requested: Vec<u16>,
}

impl ExtensionMessage for RequestExtensions {
    const EXTENSION: u16 = NEGOTIATION;
    const MESSAGE_TYPE: u8 = REQUEST_EXTENSIONS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        Ok(Writer::default()
            .u16(self.request_id)
            .seq_u16(&self.requested)?
            .done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u16()?;
        let requested = reader.seq_u16()?;
        reader.end(Self {
            request_id,
            requested,
        })
    }
}

/// RequestExtensions.Success: the requested extensions the server grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestExtensionsSuccess {
    pub request_id: u16,
    pub supported: Vec<u16>,
}

impl ExtensionMessage for RequestExtensionsSuccess {
    const EXTENSION: u16 = NEGOTIATION;
    const MESSAGE_TYPE: u8 = REQUEST_EXTENSIONS_SUCCESS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        Ok(Writer::default()
            .u16(self.request_id)
            .seq_u16(&self.supported)?
            .done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u16()?;
        let supported = reader.seq_u16()?;
        reader.end(Self {
            request_id,
            supported,
        })
    }
}

/// RequestExtensions.Error: none of the requested extensions is supported,
/// or the server requires some the client did not ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestExtensionsError {
    pub request_id: u16,
    pub unsupported: Vec<u16>,
    pub required: Vec<u16>,
}

impl ExtensionMessage for RequestExtensionsError {
    const EXTENSION: u16 = NEGOTIATION;
    const MESSAGE_TYPE: u8 = REQUEST_EXTENSIONS_ERROR;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        Ok(Writer::default()
            .u16(self.request_id)
            .seq_u16(&self.unsupported)?
            .seq_u16(&self.required)?
            .done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u16()?;
        let unsupported = reader.seq_u16()?;
        let required = reader.seq_u16()?;
        reader.end(Self {
            request_id,
            unsupported,
            required,
        })
    }
}

// What: the server's answer to RequestExtensions: Success listing the
// requested extensions it supports, in the order asked; Error naming every
// requested one as unsupported (and requiring none) when it supports none of
// them, as extension 0x0001 section 4 says.
// Why: Knuth answers exactly so; a client that asked for nothing Pickaxe has
// learns that from an Error, as from Knuth.
// Look here if: a negotiation answer lists an extension that was not asked
// for, or Success comes back empty.
/// The answer to `request` from a server that supports `granted` (in the
/// order the client asked).
pub fn negotiation_answer(
    request: &RequestExtensions,
    supported: impl Fn(u16) -> bool,
) -> Result<SerializedFrame, String> {
    let mut granted: Vec<u16> = Vec::new();
    for extension in &request.requested {
        if supported(*extension) && !granted.contains(extension) {
            granted.push(*extension);
        }
    }
    if granted.is_empty() {
        RequestExtensionsError {
            request_id: request.request_id,
            unsupported: request.requested.clone(),
            required: Vec::new(),
        }
        .frame()
    } else {
        RequestExtensionsSuccess {
            request_id: request.request_id,
            supported: granted,
        }
        .frame()
    }
}

/// One TLV field after a message's own fields (sv2-spec 3.4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tlv {
    pub extension: u16,
    pub field: u8,
    pub value: Vec<u8>,
}

impl Tlv {
    /// The Worker-Specific Hashrate Tracking field naming `worker`, cut to
    /// at most 32 bytes between characters, as Knuth cuts it.
    pub fn user_identity(worker: &str) -> Self {
        let end = worker.floor_char_boundary(MAX_USER_IDENTITY);
        Self {
            extension: WORKER_HASHRATE,
            field: USER_IDENTITY_FIELD,
            value: worker.as_bytes()[..end].to_vec(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        Ok(Writer::default()
            .u16(self.extension)
            .bytes(&[self.field])
            .b0_64k(&self.value)?
            .done())
    }

    /// Every field in `bytes`, in order; `None` when they are not whole
    /// fields (a header cut short, a length past the end).
    pub fn read_all(bytes: &[u8]) -> Option<Vec<Self>> {
        let mut reader = Reader(bytes);
        let mut fields = Vec::new();
        while !reader.0.is_empty() {
            let extension = reader.u16()?;
            let field = reader.u8()?;
            let value = reader.b0_64k()?.to_vec();
            fields.push(Self {
                extension,
                field,
                value,
            });
        }
        Some(fields)
    }
}

// What: the worker a share names (extension 0x0002): the first user_identity
// field among `bytes`' TLV fields, other fields ignored. Bytes that are not
// whole fields are malformed whether or not the extension was granted; a
// field is read only when it was (`granted`), and one over 32 bytes refuses
// the share (`user-identity-too-long`).
// Why: extension 0x0002 section 1.3 has servers ignore fields they did not
// negotiate; Knuth closes on bytes that are not whole fields and refuses a
// name over 32 bytes.
// Look here if: a share with a worker name is refused or closes the
// connection.
/// The worker `bytes` (what follows a SubmitSharesExtended) names: `Ok(None)`
/// without one or without the grant, `Err(Some(code))` for a refused share,
/// `Err(None)` for malformed bytes.
pub fn share_worker(bytes: &[u8], granted: bool) -> Result<Option<String>, Option<&'static str>> {
    let fields = Tlv::read_all(bytes).ok_or(None)?;
    if !granted {
        return Ok(None);
    }
    let mut worker = None;
    for field in fields
        .iter()
        .filter(|field| field.extension == WORKER_HASHRATE && field.field == USER_IDENTITY_FIELD)
    {
        if field.value.len() > MAX_USER_IDENTITY {
            return Err(Some(USER_IDENTITY_TOO_LONG));
        }
        if worker.is_none() {
            worker = Some(String::from_utf8_lossy(&field.value).into_owned());
        }
    }
    Ok(worker)
}

/// RequestPayoutOutputs: the outputs a token's job must pay, asked for a
/// template's revenue, height, parent and bits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestPayoutOutputs {
    pub request_id: u32,
    pub token: Vec<u8>,
    pub available_payout_value: u64,
    pub height: u32,
    pub prev_hash: Hash,
    pub nbits: u32,
    /// The coinbase script before the extranonce.
    pub coinbase_prefix: Vec<u8>,
}

impl ExtensionMessage for RequestPayoutOutputs {
    const EXTENSION: u16 = PAYOUT_OUTPUTS;
    const MESSAGE_TYPE: u8 = REQUEST_PAYOUT_OUTPUTS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let mut writer = Writer::default();
        writer.u32(self.request_id).b0_255(&self.token)?;
        writer
            .bytes(&self.available_payout_value.to_le_bytes())
            .u32(self.height)
            .bytes(&self.prev_hash)
            .u32(self.nbits)
            .b0_255(&self.coinbase_prefix)?;
        Ok(writer.done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u32()?;
        let token = reader.b0_255()?.to_vec();
        let available_payout_value = reader.u64()?;
        let height = reader.u32()?;
        let prev_hash = reader.array()?;
        let nbits = reader.u32()?;
        let coinbase_prefix = reader.b0_255()?.to_vec();
        reader.end(Self {
            request_id,
            token,
            available_payout_value,
            height,
            prev_hash,
            nbits,
            coinbase_prefix,
        })
    }
}

/// RequestPayoutOutputs.Success: the outputs (a count and the serialized
/// outputs, as AllocateMiningJobToken.Success carries them) and, with
/// EXACT_OUTPUTS, the coinbase's version, sequence and lock time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestPayoutOutputsSuccess {
    pub request_id: u32,
    pub flags: u32,
    pub version: u32,
    pub sequence: u32,
    pub locktime: u32,
    pub outputs: Vec<u8>,
}

impl ExtensionMessage for RequestPayoutOutputsSuccess {
    const EXTENSION: u16 = PAYOUT_OUTPUTS;
    const MESSAGE_TYPE: u8 = REQUEST_PAYOUT_OUTPUTS_SUCCESS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        Ok(Writer::default()
            .u32(self.request_id)
            .u32(self.flags)
            .u32(self.version)
            .u32(self.sequence)
            .u32(self.locktime)
            .b0_64k(&self.outputs)?
            .done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u32()?;
        let flags = reader.u32()?;
        let version = reader.u32()?;
        let sequence = reader.u32()?;
        let locktime = reader.u32()?;
        let outputs = reader.b0_64k()?.to_vec();
        reader.end(Self {
            request_id,
            flags,
            version,
            sequence,
            locktime,
            outputs,
        })
    }
}

/// RequestPayoutOutputs.Error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestPayoutOutputsError {
    pub request_id: u32,
    pub code: String,
}

impl ExtensionMessage for RequestPayoutOutputsError {
    const EXTENSION: u16 = PAYOUT_OUTPUTS;
    const MESSAGE_TYPE: u8 = REQUEST_PAYOUT_OUTPUTS_ERROR;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let mut writer = Writer::default();
        writer.u32(self.request_id).b0_255(self.code.as_bytes())?;
        Ok(writer.done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u32()?;
        let code = String::from_utf8(reader.b0_255()?.to_vec()).ok()?;
        reader.end(Self { request_id, code })
    }
}

/// PayoutOutputsStale: the server's payouts changed; tokens allocated
/// before it are stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PayoutOutputsStale;

impl ExtensionMessage for PayoutOutputsStale {
    const EXTENSION: u16 = PAYOUT_OUTPUTS;
    const MESSAGE_TYPE: u8 = PAYOUT_OUTPUTS_STALE;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        Ok(Vec::new())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        payload.is_empty().then_some(Self)
    }
}

/// DeclareMiningJob.Transactions: one page of a declaration's transaction
/// ids, sent before the DeclareMiningJob with the same request id. The
/// declared list is every page in order, then DeclareMiningJob's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclareMiningJobTransactions {
    pub request_id: u32,
    pub txids: Vec<Hash>,
}

impl ExtensionMessage for DeclareMiningJobTransactions {
    const EXTENSION: u16 = LARGE_TEMPLATES;
    const MESSAGE_TYPE: u8 = DECLARE_MINING_JOB_TRANSACTIONS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let mut writer = Writer::default();
        writer.u32(self.request_id).count(self.txids.len())?;
        self.txids.iter().for_each(|txid| {
            writer.bytes(txid);
        });
        Ok(writer.done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u32()?;
        let txids = reader.seq::<32>()?;
        reader.end(Self { request_id, txids })
    }
}

/// ProvideMissingTransactions.Positions: the core request with U32
/// positions in the whole declared list (pages first, then
/// DeclareMiningJob's own), answered with the core
/// ProvideMissingTransactions.Success.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProvideMissingTransactionsPositions {
    pub request_id: u32,
    pub positions: Vec<u32>,
}

impl ExtensionMessage for ProvideMissingTransactionsPositions {
    const EXTENSION: u16 = LARGE_TEMPLATES;
    const MESSAGE_TYPE: u8 = PROVIDE_MISSING_TRANSACTIONS_POSITIONS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let mut writer = Writer::default();
        writer.u32(self.request_id).count(self.positions.len())?;
        self.positions.iter().for_each(|position| {
            writer.u32(*position);
        });
        Ok(writer.done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let request_id = reader.u32()?;
        let positions = reader.seq_u32()?;
        reader.end(Self {
            request_id,
            positions,
        })
    }
}

// What: a declaration's transaction ids as 0x4C54 pages them: full pages of
// 65,535 ids, then the last 1 to 65,535 for DeclareMiningJob's own list; a
// list that fits one SEQ0_64K is not paged.
// Why: Knuth's client pages exactly so (jd_declaration.cpp, declare_work),
// and its JDS reading must match.
// Look here if: a large declaration's last page or DeclareMiningJob carries
// the wrong count.
/// The ids `txids` pages ahead of DeclareMiningJob, and the ids it carries.
pub fn pages(txids: &[Hash]) -> (Vec<&[Hash]>, &[Hash]) {
    let tail = if txids.is_empty() {
        0
    } else {
        (txids.len() - 1) % SEQ_MAX + 1
    };
    let (paged, last) = txids.split_at(txids.len() - tail);
    (paged.chunks(SEQ_MAX).collect(), last)
}

/// RequestTransactionData.Transactions: one page of a template's
/// transactions in order, at most 65,535 in one frame. The list is every
/// page in order, then the core RequestTransactionData.Success's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestTransactionDataTransactions {
    pub template_id: u64,
    pub transactions: Vec<Vec<u8>>,
}

impl RequestTransactionDataTransactions {
    /// The payload's size: the id, the count and each B0_16M.
    pub fn size(&self) -> usize {
        self.transactions
            .iter()
            .fold(8 + 2, |size, tx| size.saturating_add(3 + tx.len()))
    }
}

impl ExtensionMessage for RequestTransactionDataTransactions {
    const EXTENSION: u16 = LARGE_TEMPLATES;
    const MESSAGE_TYPE: u8 = REQUEST_TRANSACTION_DATA_TRANSACTIONS;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        // A page fills at most one frame.
        if self.size() > FRAME_MAX {
            return Err("transaction page over one frame");
        }
        let mut writer = Writer::default();
        writer
            .u64(self.template_id)
            .count(self.transactions.len())?;
        for tx in &self.transactions {
            writer.b0_16m(tx)?;
        }
        Ok(writer.done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() > FRAME_MAX {
            return None;
        }
        let mut reader = Reader(payload);
        let template_id = reader.u64()?;
        let count = usize::from(reader.u16()?);
        // Each item takes at least its 3-byte length.
        if count > reader.0.len() / 3 {
            return None;
        }
        let transactions = (0..count)
            .map(|_| reader.b0_16m().map(<[u8]>::to_vec))
            .collect::<Option<_>>()?;
        reader.end(Self {
            template_id,
            transactions,
        })
    }
}

/// RequestTransactionData.Kept: starts a push of only what the base (the
/// template whose push finished last) lacks. Bit i, least significant
/// first, is set when the base's i-th transaction (block order, without
/// the coinbase) is also in this template; see `kept_bitmap_fits`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestTransactionDataKept {
    pub template_id: u64,
    pub base_template_id: u64,
    pub kept: Vec<u8>,
}

impl ExtensionMessage for RequestTransactionDataKept {
    const EXTENSION: u16 = LARGE_TEMPLATES;
    const MESSAGE_TYPE: u8 = REQUEST_TRANSACTION_DATA_KEPT;

    fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if 8 + 8 + 3 + self.kept.len() > FRAME_MAX {
            return Err("kept bitmap over one frame");
        }
        let mut writer = Writer::default();
        writer
            .u64(self.template_id)
            .u64(self.base_template_id)
            .b0_16m(&self.kept)?;
        Ok(writer.done())
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() > FRAME_MAX {
            return None;
        }
        let mut reader = Reader(payload);
        let template_id = reader.u64()?;
        let base_template_id = reader.u64()?;
        let kept = reader.b0_16m()?.to_vec();
        reader.end(Self {
            template_id,
            base_template_id,
            kept,
        })
    }
}

/// The largest Kept bitmap one frame carries, after the two ids and the
/// U24 length: a base of more transactions is followed by a full push.
pub const MAX_KEPT: usize = FRAME_MAX - (8 + 8 + 3);

// What: a Kept bitmap over a base of `count` transactions is exactly
// ceil(count / 8) bytes and its unused high bits are clear.
// Why: Knuth's docs/sv2.md ("Template flow") and its test client's rebuild
// (sv2_template_provider.cpp:642-643) hold the bitmap to that length.
// Look here if: a provider's Kept is refused, or a delta keeps a
// transaction past the base's end.
/// Whether `kept` is a Kept bitmap over `count` transactions.
pub fn kept_bitmap_fits(kept: &[u8], count: usize) -> bool {
    kept.len() == count.div_ceil(8)
        && kept
            .last()
            .is_none_or(|last| count.is_multiple_of(8) || last >> (count % 8) == 0)
}

/// Whether bit `index` of a Kept bitmap is set.
pub fn kept_bit(kept: &[u8], index: usize) -> bool {
    kept.get(index / 8)
        .is_some_and(|byte| byte >> (index % 8) & 1 == 1)
}

#[cfg(test)]
#[path = "extensions_tests.rs"]
mod tests;
