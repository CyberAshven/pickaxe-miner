//! The Knuth node's extension (k-nuth/kth PR #879; its docs/sv2.md,
//! "Template flow"). Once a client negotiates it, the provider pushes each
//! template's transactions after its NewTemplate and SetNewPrevHash, unasked.
//! The first push, and the one after an abandoned push, is full: pages of
//! RequestTransactionData.Transactions (0x03), then the core
//! RequestTransactionData.Success with the rest. A later push starts with
//! RequestTransactionData.Kept (0x04), a bitmap over the base (the template
//! whose push finished last) naming the transactions that stay, and carries
//! only the ones the base lacks; the client rebuilds the list in CTOR order
//! and checks it against the template's merkle path.
//!
//! `Pusher` is a provider connection's side and `Received` a client's;
//! neither does I/O. The pages, the bitmap and the abandoning follow Knuth's
//! template_provider.cpp (CyberAshven/kth 9ddf0d0b66) line for line.

use super::super::{
    extensions::{
        kept_bit, kept_bitmap_fits, ExtensionMessage, RequestTransactionDataKept,
        RequestTransactionDataTransactions, MAX_KEPT, SEQ_MAX,
    },
    template::{coinbase_path, double_sha256, BchTemplate, Hash},
};
use super::convert;
use std::{cmp::Ordering, sync::Arc};
use stratum_core::codec_sv2::SerializedFrame;

/// The most payload of one page, counted as the core Success counts it (its
/// excess_data adds two bytes): Knuth's `data_page_size`. A BCH transaction
/// is at most 1,000,000 bytes, so every page holds at least one.
pub const PAGE_SIZE: usize = 1024 * 1024;

/// CTOR, BCH's block order after the coinbase: ascending transaction id,
/// compared from its last byte (Knuth's `ctor_less`).
pub fn ctor(left: &Hash, right: &Hash) -> Ordering {
    left.iter().rev().cmp(right.iter().rev())
}

/// One template's transactions on their way: a push, or the answer to a
/// request, which leaves the base alone.
struct Transfer {
    id: u64,
    /// A push's transaction ids in block order, the next base once sent;
    /// none for an answer.
    push: Option<Arc<[Hash]>>,
    /// A delta sends Kept, then only what the base lacks.
    delta: bool,
    kept_sent: bool,
    /// The template's next transaction.
    next: usize,
    /// The base's next id, for a delta.
    base_next: usize,
}

impl Transfer {
    fn new(id: u64, push: Option<Arc<[Hash]>>, delta: bool) -> Self {
        Self {
            id,
            push,
            delta,
            kept_sent: false,
            next: 0,
            base_next: 0,
        }
    }
}

/// A provider connection's transaction data once the client negotiated
/// 0x4C54 (Knuth's template_provider_session with `large_templates_` on).
#[derive(Default)]
pub struct Pusher {
    transfer: Option<Transfer>,
    /// At most one answer waits: nothing more is read until it is sent.
    requested: Option<u64>,
    /// The last template whose push finished, which the next push names as
    /// its base, and its transaction ids in block order. The client keeps
    /// it too.
    base: Option<(u64, Arc<[Hash]>)>,
}

impl Pusher {
    /// Starts the push of template `id`, just announced. A push still under
    /// way is abandoned; the client then drops what it got, so this push is
    /// full.
    pub fn push(&mut self, id: u64, template: &BchTemplate) {
        if self.transfer.is_some() {
            self.abandon();
        }
        let delta = self
            .base
            .as_ref()
            .is_some_and(|(_, ids)| ids.len().div_ceil(8) <= MAX_KEPT);
        self.transfer = Some(Transfer::new(
            id,
            Some(template.shared_transaction_ids()),
            delta,
        ));
    }

    fn abandon(&mut self) {
        self.transfer = None;
        self.base = None;
    }

    /// A core RequestTransactionData: answered with the whole list in pages
    /// after any push under way. The session reads nothing more until the
    /// answer is sent, so a second request cannot be waiting.
    pub fn request(&mut self, id: u64) -> Result<(), String> {
        if self.requested.is_some() {
            return Err("template client asked for transaction data before its answer".into());
        }
        self.requested = Some(id);
        Ok(())
    }

    /// Transaction data is still to be sent.
    pub fn sending(&self) -> bool {
        self.transfer.is_some() || self.requested.is_some()
    }

    /// An answer the client asked for is still to be sent; any push ahead
    /// of it is finished first.
    pub fn answering(&self) -> bool {
        self.requested.is_some() || self.transfer.as_ref().is_some_and(|t| t.push.is_none())
    }

    /// The next frame of the data under way. `find` gives a template the
    /// client can still name, and whether its parent was replaced. None
    /// when nothing is under way, or when the template went away before its
    /// push finished: the push is then abandoned and the next one is full.
    pub fn next_frame<'t>(
        &mut self,
        find: impl Fn(u64) -> Option<(&'t BchTemplate, bool)>,
    ) -> Result<Option<SerializedFrame>, String> {
        if self.transfer.is_none() {
            let Some(id) = self.requested.take() else {
                return Ok(None);
            };
            match find(id) {
                None => {
                    return convert::transaction_data_error(id, "template-id-not-found").map(Some)
                }
                Some((_, true)) => {
                    return convert::transaction_data_error(id, "stale-template-id").map(Some)
                }
                Some((_, false)) => self.transfer = Some(Transfer::new(id, None, false)),
            }
        }
        let Some(id) = self.transfer.as_ref().map(|transfer| transfer.id) else {
            return Ok(None);
        };
        let Some((template, _)) = find(id) else {
            self.abandon();
            return Ok(None);
        };
        let (Some(transfer), base) = (self.transfer.as_mut(), self.base.as_ref()) else {
            return Ok(None);
        };
        if transfer.delta && !transfer.kept_sent {
            transfer.kept_sent = true;
            return kept_frame(transfer, base).map(Some);
        }
        let (frame, done) = page_frame(transfer, base.map(|(_, ids)| &ids[..]), template)?;
        if done {
            // The core Success ends the data. A finished push is the next
            // base.
            if let Some(Transfer {
                id,
                push: Some(ids),
                ..
            }) = self.transfer.take()
            {
                self.base = Some((id, ids));
            }
        }
        Ok(Some(frame))
    }
}

/// The Kept bitmap of `transfer` over `base`. Both lists are in CTOR order,
/// so one merge finds the kept transactions.
fn kept_frame(
    transfer: &Transfer,
    base: Option<&(u64, Arc<[Hash]>)>,
) -> Result<SerializedFrame, String> {
    let (Some((base_id, base)), Some(ids)) = (base, transfer.push.as_ref()) else {
        return Err("a delta push without a base".into());
    };
    let mut kept = vec![0; base.len().div_ceil(8)];
    let mut next = 0;
    for (index, id) in base.iter().enumerate() {
        while ids.get(next).is_some_and(|other| ctor(other, id).is_lt()) {
            next += 1;
        }
        if ids.get(next) == Some(id) {
            kept[index / 8] |= 1 << (index % 8);
        }
    }
    RequestTransactionDataKept {
        template_id: transfer.id,
        base_template_id: *base_id,
        kept,
    }
    .frame()
}

/// Moves past the template's transactions its base already has, so a
/// delta sends only the added ones.
fn skip_kept(transfer: &mut Transfer, base: Option<&[Hash]>) {
    let Transfer {
        push,
        delta,
        next,
        base_next,
        ..
    } = transfer;
    let (true, Some(ids), Some(base)) = (*delta, push.as_deref(), base) else {
        return;
    };
    while let Some(id) = ids.get(*next) {
        while base
            .get(*base_next)
            .is_some_and(|other| ctor(other, id).is_lt())
        {
            *base_next += 1;
        }
        if base.get(*base_next) != Some(id) {
            return;
        }
        *base_next += 1;
        *next += 1;
    }
}

/// The next page of `template`, at most 65,535 transactions in at most
/// `PAGE_SIZE` bytes, or the core Success with the rest (true once sent).
fn page_frame(
    transfer: &mut Transfer,
    base: Option<&[Hash]>,
    template: &BchTemplate,
) -> Result<(SerializedFrame, bool), String> {
    let txs = template.transactions();
    let mut list: Vec<&[u8]> = Vec::new();
    // Counted as the core Success, whose excess_data adds two bytes.
    let mut size = 8 + 2 + 2;
    skip_kept(transfer, base);
    while let Some(tx) = txs.get(transfer.next).filter(|_| list.len() < SEQ_MAX) {
        let added = 3 + tx.len();
        if !list.is_empty() && size + added > PAGE_SIZE {
            break;
        }
        list.push(tx);
        size += added;
        transfer.next += 1;
        skip_kept(transfer, base);
    }
    if transfer.next < txs.len() {
        let page = RequestTransactionDataTransactions {
            template_id: transfer.id,
            transactions: list.iter().map(|tx| tx.to_vec()).collect(),
        };
        return Ok((page.frame()?, false));
    }
    Ok((convert::transaction_data_success(transfer.id, &list)?, true))
}

/// A template's transactions once its push finished: the base of the
/// next push.
pub struct Pushed {
    pub id: u64,
    txids: Vec<Hash>,
    pub transactions: Vec<Arc<[u8]>>,
}

/// What of the push due has arrived.
#[derive(Default)]
struct Incoming {
    kept: Option<Vec<u8>>,
    added: Vec<Arc<[u8]>>,
}

/// A client connection's pushes: the base, the push due, and what of it
/// arrived. The provider keeps the same base.
#[derive(Default)]
pub struct Received {
    base: Option<Pushed>,
    /// The template whose push is due: the one announced last, until its
    /// Success.
    due: Option<u64>,
    incoming: Option<Incoming>,
    /// Push frames read, so a wait for a push can tell it is still coming.
    pub progress: u64,
}

impl Received {
    /// A NewTemplate: its push is due. A push still due was abandoned, and
    /// the provider dropped its base with it, so the next push is full.
    pub fn announced(&mut self, id: u64) {
        if self.due.is_some() {
            self.base = None;
        }
        self.due = Some(id);
        self.incoming = None;
    }

    /// RequestTransactionData.Kept: the push due starts as a delta.
    pub fn kept(&mut self, payload: &[u8]) -> Result<(), &'static str> {
        let message = RequestTransactionDataKept::decode(payload)
            .ok_or("malformed RequestTransactionData.Kept")?;
        if self.due != Some(message.template_id) || self.incoming.is_some() {
            return Err("RequestTransactionData.Kept out of order");
        }
        let base = self
            .base
            .as_ref()
            .filter(|base| base.id == message.base_template_id)
            .ok_or("RequestTransactionData.Kept names another base")?;
        if !kept_bitmap_fits(&message.kept, base.txids.len()) {
            return Err("RequestTransactionData.Kept does not fit its base");
        }
        self.incoming = Some(Incoming {
            kept: Some(message.kept),
            added: Vec::new(),
        });
        self.progress += 1;
        Ok(())
    }

    /// RequestTransactionData.Transactions: a page of the push due.
    pub fn page(&mut self, payload: &[u8]) -> Result<(), &'static str> {
        let message = RequestTransactionDataTransactions::decode(payload)
            .ok_or("malformed RequestTransactionData.Transactions")?;
        if self.due != Some(message.template_id) {
            return Err("RequestTransactionData.Transactions for another template");
        }
        self.incoming
            .get_or_insert_default()
            .added
            .extend(message.transactions.into_iter().map(Arc::from));
        self.progress += 1;
        Ok(())
    }

    // What: the core Success ends the push of `id`. A full push's list is
    // every page in order, then the Success's; a delta's is the base's kept
    // transactions and the added ones sorted by id (CTOR). The list must be
    // in strict CTOR order and give the template's merkle path; it is then
    // the base of the next push.
    // Why: Knuth's provider sends a delta's added transactions only, and its
    // test client rebuilds and checks the list so (sv2_template_provider.cpp
    // :639-673). Strict order also refuses a repeated last transaction, which
    // an odd merkle level would hide.
    // Look here if: a provider's pushes break the protocol, or a pushed
    // template's block has the wrong merkle root.
    /// The Success ending the push of `id`, checked against the template's
    /// `merkle_path`.
    pub fn finish(
        &mut self,
        id: u64,
        last: Vec<Arc<[u8]>>,
        merkle_path: &[Hash],
    ) -> Result<(), &'static str> {
        if self.due != Some(id) {
            return Err("RequestTransactionData.Success for another template");
        }
        let Incoming { kept, mut added } = self.incoming.take().unwrap_or_default();
        added.extend(last);
        let added = added.into_iter().map(|tx| (double_sha256(&tx), tx));
        let list: Vec<(Hash, Arc<[u8]>)> = match kept {
            Some(kept) => {
                let base = self.base.as_ref().ok_or("a delta push without a base")?;
                let mut list: Vec<_> = base
                    .txids
                    .iter()
                    .zip(&base.transactions)
                    .enumerate()
                    .filter(|(index, _)| kept_bit(&kept, *index))
                    .map(|(_, (txid, tx))| (*txid, tx.clone()))
                    .chain(added)
                    .collect();
                list.sort_unstable_by(|left, right| ctor(&left.0, &right.0));
                list
            }
            None => added.collect(),
        };
        if list
            .windows(2)
            .any(|pair| ctor(&pair[0].0, &pair[1].0).is_ge())
        {
            return Err("pushed transactions are duplicated or not in CTOR order");
        }
        let (txids, transactions): (Vec<Hash>, Vec<Arc<[u8]>>) = list.into_iter().unzip();
        if coinbase_path(&txids) != merkle_path {
            return Err("pushed transactions disagree with the template's merkle path");
        }
        self.base = Some(Pushed {
            id,
            txids,
            transactions,
        });
        self.due = None;
        self.progress += 1;
        Ok(())
    }

    /// Template `id`'s transactions, once its push finished.
    pub fn pushed(&self, id: u64) -> Option<&Pushed> {
        self.base.as_ref().filter(|base| base.id == id)
    }
}

#[cfg(test)]
#[path = "large_tests.rs"]
pub(super) mod tests;
