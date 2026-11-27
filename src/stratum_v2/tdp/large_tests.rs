//! The provider's pushes and the client's rebuild, without sockets, as
//! Knuth's session tests check them (src/node/test/sv2_template_provider.cpp
//! at CyberAshven/kth 9ddf0d0b66, cited per test). Every push is read back
//! by `Received`, which checks each list against the template's merkle path.

use super::super::super::{
    extensions::{
        ExtensionMessage, RequestTransactionDataKept, RequestTransactionDataTransactions,
        FRAME_MAX, LARGE_TEMPLATES, REQUEST_TRANSACTION_DATA_KEPT,
        REQUEST_TRANSACTION_DATA_TRANSACTIONS,
    },
    template::{compact_target, Provided},
};
use super::*;
use stratum_core::{
    binary_sv2,
    template_distribution_sv2::{
        RequestTransactionDataError, RequestTransactionDataSuccess,
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS,
    },
};

/// A transaction BCH's checks take: one input spending output `n` of a
/// fixed parent, one output whose script is OP_RETURN and `pad` zeros.
pub(in crate::stratum_v2::tdp) fn tx(n: u32, pad: usize) -> Vec<u8> {
    let mut bytes = 2u32.to_le_bytes().to_vec();
    bytes.push(1);
    bytes.extend([1; 32]);
    bytes.extend(n.to_le_bytes());
    bytes.push(0);
    bytes.extend(u32::MAX.to_le_bytes());
    bytes.push(1);
    bytes.extend(1000u64.to_le_bytes());
    let script = pad + 1;
    if script < 0xfd {
        bytes.push(script as u8);
    } else {
        bytes.push(0xfd);
        bytes.extend((script as u16).to_le_bytes());
    }
    bytes.push(0x6a);
    bytes.extend(std::iter::repeat_n(0, pad));
    bytes.extend(0u32.to_le_bytes());
    bytes
}

/// A provider's template on parent `[parent; 32]` holding the transactions
/// numbered `numbers`, in CTOR order (Knuth's `numbered_template`).
pub(in crate::stratum_v2::tdp) fn template(
    numbers: impl IntoIterator<Item = u32>,
    pad: usize,
    parent: u8,
) -> Arc<BchTemplate> {
    let mut txs: Vec<(Hash, Arc<[u8]>)> = numbers
        .into_iter()
        .map(|n| {
            let bytes = tx(n, pad);
            (double_sha256(&bytes), bytes.into())
        })
        .collect();
    txs.sort_unstable_by(|left, right| ctor(&left.0, &right.0));
    let (ids, transactions): (Vec<Hash>, Vec<Arc<[u8]>>) = txs.into_iter().unzip();
    let path = coinbase_path(&ids);
    Arc::new(
        BchTemplate::from_provided(Provided {
            previous_hash: [parent; 32],
            version: 0x2000_0000,
            bits: 0x207f_ffff,
            target: compact_target(0x207f_ffff).unwrap(),
            ntime: 1_700_000_010,
            prefix: &[3, 0x15, 0xf9, 0x04],
            value: 312_500_000,
            transactions,
            merkle_path: &path,
            reserve: 122,
        })
        .unwrap(),
    )
}

/// The template's transactions as bytes.
fn raw(template: &BchTemplate) -> Vec<Vec<u8>> {
    template
        .transactions()
        .iter()
        .map(|tx| tx.to_vec())
        .collect()
}

/// A frame's extension (channel bit clear), message type and payload.
fn parts(frame: &SerializedFrame) -> (u16, u8, &[u8]) {
    let bytes = frame.as_bytes();
    let extension = u16::from_le_bytes([bytes[0], bytes[1]]);
    assert_eq!(extension & 0x8000, 0, "no channel bit");
    let length = u32::from_le_bytes([bytes[3], bytes[4], bytes[5], 0]) as usize;
    assert_eq!(length, bytes.len() - 6);
    (extension, bytes[2], &bytes[6..])
}

/// One connection's retained templates and its pusher, as the server's
/// session holds them.
#[derive(Default)]
struct Session {
    pusher: Pusher,
    /// Id, template, and whether its parent was replaced.
    retained: Vec<(u64, Arc<BchTemplate>, bool)>,
    last: u64,
}

impl Session {
    /// Announces `template` (its NewTemplate and SetNewPrevHash went out)
    /// and starts its push.
    fn offer(&mut self, template: &Arc<BchTemplate>) -> u64 {
        self.last += 1;
        self.retained.push((self.last, template.clone(), false));
        self.pusher.push(self.last, template);
        self.last
    }

    fn next(&mut self) -> Option<SerializedFrame> {
        let retained = &self.retained;
        self.pusher
            .next_frame(|id| {
                retained
                    .iter()
                    .find(|(retained, ..)| *retained == id)
                    .map(|(_, template, stale)| (&**template, *stale))
            })
            .unwrap()
    }

    /// Every frame of the data under way, taken one at a time as the server
    /// does while the socket drains (Knuth's `drain`).
    fn drain(&mut self) -> Vec<SerializedFrame> {
        let mut frames = Vec::new();
        for count in 0.. {
            if !self.pusher.sending() {
                break;
            }
            assert!(count < 100_000, "the data never ends");
            frames.extend(self.next());
        }
        frames
    }

    fn forget(&mut self, id: u64) {
        self.retained.retain(|(retained, ..)| *retained != id);
    }

    fn replace_parent(&mut self) {
        for (_, _, stale) in &mut self.retained {
            *stale = true;
        }
    }
}

/// One template's data as a client reads it (Knuth's `collect`): Kept first
/// for a delta, then pages, then the core Success, every frame naming the
/// template and none over a page.
#[derive(Debug)]
struct Data {
    kept: Option<RequestTransactionDataKept>,
    pages: usize,
    transactions: Vec<Vec<u8>>,
}

fn collect(frames: &[SerializedFrame], id: u64) -> Data {
    assert!(!frames.is_empty());
    let mut data = Data {
        kept: None,
        pages: 0,
        transactions: Vec::new(),
    };
    for (index, frame) in frames.iter().enumerate() {
        let (extension, kind, payload) = parts(frame);
        assert!(payload.len() <= PAGE_SIZE, "{}", payload.len());
        if index + 1 == frames.len() {
            assert_eq!(
                (extension, kind),
                (0, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS)
            );
            let mut bytes = payload.to_vec();
            let success: RequestTransactionDataSuccess =
                binary_sv2::from_bytes(&mut bytes).unwrap();
            assert_eq!(success.template_id, id);
            assert!(success.excess_data.as_ref().is_empty());
            data.transactions.extend(
                success
                    .transaction_list
                    .iter()
                    .map(|tx| tx.as_ref().to_vec()),
            );
        } else if index == 0 && kind == REQUEST_TRANSACTION_DATA_KEPT {
            assert_eq!(extension, LARGE_TEMPLATES);
            let kept = RequestTransactionDataKept::decode(payload).unwrap();
            assert_eq!(kept.template_id, id);
            data.kept = Some(kept);
        } else {
            assert_eq!(
                (extension, kind),
                (LARGE_TEMPLATES, REQUEST_TRANSACTION_DATA_TRANSACTIONS)
            );
            let page = RequestTransactionDataTransactions::decode(payload).unwrap();
            assert_eq!(page.template_id, id);
            assert!(!page.transactions.is_empty());
            data.pages += 1;
            data.transactions.extend(page.transactions);
        }
    }
    data
}

/// Feeds a push's frames to the client's side, which rebuilds the list and
/// checks it against `template`'s merkle path.
fn receive(
    received: &mut Received,
    frames: &[SerializedFrame],
    id: u64,
    template: &BchTemplate,
) -> Result<(), &'static str> {
    for frame in frames {
        match parts(frame) {
            (LARGE_TEMPLATES, REQUEST_TRANSACTION_DATA_KEPT, payload) => received.kept(payload)?,
            (LARGE_TEMPLATES, REQUEST_TRANSACTION_DATA_TRANSACTIONS, payload) => {
                received.page(payload)?
            }
            (0, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS, payload) => {
                let mut bytes = payload.to_vec();
                let success: RequestTransactionDataSuccess =
                    binary_sv2::from_bytes(&mut bytes).unwrap();
                let last = success
                    .transaction_list
                    .iter()
                    .map(|tx| Arc::from(tx.as_ref()))
                    .collect();
                received.finish(success.template_id, last, template.merkle_path())?
            }
            other => panic!("unexpected frame {:?}", (other.0, other.1)),
        }
    }
    let pushed = received.pushed(id).ok_or("the push did not finish")?;
    assert_eq!(&pushed.transactions[..], template.transactions());
    Ok(())
}

fn error_code(frame: &SerializedFrame) -> String {
    let (extension, kind, payload) = parts(frame);
    assert_eq!(
        (extension, kind),
        (0, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR)
    );
    let mut bytes = payload.to_vec();
    let error: RequestTransactionDataError = binary_sv2::from_bytes(&mut bytes).unwrap();
    String::from_utf8_lossy(error.error_code.as_ref()).into_owned()
}

// What: a template the core Success cannot carry (65,537 transactions, or
// 17 MB of them) is pushed in pages of at most 65,535 transactions and
// 1 MiB, then the Success; the client's list is the template's, and its
// merkle path checks. Nothing more follows.
// Why: Knuth's "pushes a template too large for one Success in pages a
// client rebuilds" (sv2_template_provider.cpp:1058-1085).
// Look here if: Pusher's pages or Received's full rebuild change.
#[test]
fn a_template_too_large_for_one_success_is_pushed_in_pages_a_client_rebuilds() {
    for (count, pad) in [(65_537, 0), (16_400, 1_000)] {
        let large = template(1..=count, pad, 0xab);
        let bytes: usize = large.transactions().iter().map(|tx| 3 + tx.len()).sum();
        assert!(count as usize > SEQ_MAX || bytes > FRAME_MAX, "{count}");
        assert_eq!(
            convert::transaction_data(1, &large).err(),
            Some("template-too-large")
        );
        let mut session = Session::default();
        let id = session.offer(&large);
        assert!(session.pusher.sending() && !session.pusher.answering());
        let frames = session.drain();
        let data = collect(&frames, id);
        assert!(data.kept.is_none());
        assert!(data.pages > 0);
        assert_eq!(data.transactions, raw(&large));
        let mut received = Received::default();
        received.announced(id);
        receive(&mut received, &frames, id, &large).unwrap();
        assert!(session.next().is_none());
    }
}

// What: a delta's bitmap is exactly ceil(base count / 8) bytes, every
// base bit set when all stay, the unused high bits clear, for bases of 0,
// 1, 7, 8, 9 and 17 transactions; one added transaction follows it.
// Why: Knuth's "sizes the Kept bitmap to its base" (:1087-1115).
// Look here if: kept_frame changes.
#[test]
fn the_kept_bitmap_is_sized_to_its_base() {
    for count in [0u32, 1, 7, 8, 9, 17] {
        let mut session = Session::default();
        let mut received = Received::default();
        let base = template(1..=count, 0, 0xab);
        let first = session.offer(&base);
        received.announced(first);
        let frames = session.drain();
        assert_eq!(collect(&frames, first).transactions, raw(&base));
        receive(&mut received, &frames, first, &base).unwrap();
        let next = template((1..=count).chain([1000]), 0, 0xab);
        let id = session.offer(&next);
        received.announced(id);
        let frames = session.drain();
        let delta = collect(&frames, id);
        let kept = delta.kept.unwrap();
        assert_eq!(kept.base_template_id, first);
        assert_eq!(kept.kept.len(), count.div_ceil(8) as usize, "{count}");
        for (index, byte) in kept.kept.iter().enumerate() {
            let bits = (count as usize - index * 8).min(8);
            assert_eq!(*byte, ((1u16 << bits) - 1) as u8, "{count}");
        }
        assert_eq!(delta.transactions.len(), 1);
        receive(&mut received, &frames, id, &next).unwrap();
    }
}

// What: a refresh on the same parent that loses two of 21 transactions and
// gains three pushes Kept (3 bytes, bit i set exactly when base
// transaction i stays, the high bits clear) and only the three new ones;
// the client rebuilds the template.
// Why: Knuth's "pushes only what a refresh on the same tip adds"
// (:1117-1154).
// Look here if: kept_frame or skip_kept changes.
#[test]
fn a_refresh_on_the_same_parent_pushes_only_what_it_adds() {
    let mut session = Session::default();
    let mut received = Received::default();
    let first = template(1..=21, 0, 0xab);
    let first_id = session.offer(&first);
    received.announced(first_id);
    let frames = session.drain();
    assert!(collect(&frames, first_id).kept.is_none());
    receive(&mut received, &frames, first_id, &first).unwrap();
    let numbers = (1..=21)
        .filter(|n| ![5, 17].contains(n))
        .chain([100, 101, 102]);
    let second = template(numbers, 0, 0xab);
    let id = session.offer(&second);
    received.announced(id);
    let frames = session.drain();
    let delta = collect(&frames, id);
    let kept = delta.kept.unwrap();
    assert_eq!((kept.base_template_id, kept.kept.len()), (first_id, 3));
    let base = raw(&first);
    let expected = raw(&second);
    for (index, tx) in base.iter().enumerate() {
        assert_eq!(
            kept_bit(&kept.kept, index),
            expected.contains(tx),
            "{index}"
        );
    }
    assert_eq!(kept.kept[2] >> (21 % 8), 0);
    assert_eq!(delta.transactions.len(), 3);
    assert!(delta.transactions.iter().all(|tx| !base.contains(tx)));
    receive(&mut received, &frames, id, &second).unwrap();
}

// What: after a new parent the push is a delta against the last template
// pushed (7 to 10 stay, 11 and 12 join); once that template is no longer
// retained, the next push is still a delta against it, from the ids the
// session kept.
// Why: Knuth's "pushes a delta after a new tip, against a base it keeps
// itself" (:1156-1193).
// Look here if: the base's lifetime changes.
#[test]
fn a_new_parent_pushes_a_delta_against_the_base() {
    let mut session = Session::default();
    let mut received = Received::default();
    let first = template(1..=10, 0, 1);
    let first_id = session.offer(&first);
    received.announced(first_id);
    receive(&mut received, &session.drain(), first_id, &first).unwrap();
    session.replace_parent();
    let second = template(7..=12, 0, 2);
    let second_id = session.offer(&second);
    received.announced(second_id);
    let frames = session.drain();
    let after_parent = collect(&frames, second_id);
    assert_eq!(after_parent.kept.unwrap().base_template_id, first_id);
    assert_eq!(after_parent.transactions.len(), 2);
    receive(&mut received, &frames, second_id, &second).unwrap();
    session.forget(first_id);
    session.forget(second_id);
    let third = template(11..=13, 0, 4);
    let third_id = session.offer(&third);
    received.announced(third_id);
    let frames = session.drain();
    assert_eq!(
        collect(&frames, third_id).kept.unwrap().base_template_id,
        second_id
    );
    receive(&mut received, &frames, third_id, &third).unwrap();
}

// What: a push the next template overtakes (after Kept and a page, or
// before its first frame) or whose template is no longer retained sends
// nothing more, and the next push is full although a base was pushed
// before; the client, which saw the push unfinished, rebuilds it; the push
// after that is a delta again.
// Why: Knuth's "abandons an unfinished push and sends the next one full"
// (:1195-1247).
// Look here if: Pusher::push or the abandoning changes.
#[test]
fn an_unfinished_push_is_abandoned_and_the_next_is_full() {
    for case in ["after a page", "before the first frame", "template gone"] {
        let mut session = Session::default();
        let mut received = Received::default();
        let base = template(1..=10, 0, 0xab);
        let base_id = session.offer(&base);
        received.announced(base_id);
        receive(&mut received, &session.drain(), base_id, &base).unwrap();
        // About 2.6 MB: Kept, two pages, then the Success.
        let first = template(1..=2500, 1024, 0xab);
        let first_id = session.offer(&first);
        received.announced(first_id);
        let mut taken = Vec::new();
        if case != "before the first frame" {
            taken.push(session.next().unwrap());
            taken.push(session.next().unwrap());
            assert_eq!(parts(&taken[0]).1, REQUEST_TRANSACTION_DATA_KEPT);
            assert_eq!(parts(&taken[1]).1, REQUEST_TRANSACTION_DATA_TRANSACTIONS);
            received.kept(parts(&taken[0]).2).unwrap();
            received.page(parts(&taken[1]).2).unwrap();
        }
        if case == "template gone" {
            session.forget(first_id);
            assert!(session.next().is_none());
            assert!(!session.pusher.sending(), "{case}");
        }
        assert!(received.pushed(first_id).is_none());
        let next = template(1..=2501, 1024, 0xab);
        let next_id = session.offer(&next);
        received.announced(next_id);
        let frames = session.drain();
        let data = collect(&frames, next_id);
        assert!(data.kept.is_none(), "{case}");
        assert!(data.pages > 0);
        assert_eq!(data.transactions, raw(&next));
        receive(&mut received, &frames, next_id, &next).unwrap();
        let after = template(1..=2502, 1024, 0xab);
        let after_id = session.offer(&after);
        received.announced(after_id);
        let frames = session.drain();
        let delta = collect(&frames, after_id);
        assert_eq!(delta.kept.unwrap().base_template_id, next_id);
        assert_eq!(delta.transactions.len(), 1);
        receive(&mut received, &frames, after_id, &after).unwrap();
    }
}

// What: a RequestTransactionData during a push is answered after it, the
// two never interleaving, with the whole list in pages and the Success; it
// leaves the base alone; with nothing under way it starts at once; an
// unknown id gets template-id-not-found (also behind a push) and a
// replaced parent's stale-template-id; a second request before the answer
// closes the connection.
// Why: Knuth's "answers RequestTransactionData in pages after any push and
// keeps the base" (:1249-1324).
// Look here if: Pusher::request or answering changes.
#[test]
fn a_request_is_answered_in_pages_after_any_push_and_keeps_the_base() {
    let mut session = Session::default();
    let first = template(1..=2000, 1024, 0xab);
    let first_id = session.offer(&first);
    session.drain();
    let second = template(1..=2001, 1024, 0xab);
    let second_id = session.offer(&second);
    let mut frames = vec![session.next().unwrap()];
    session.pusher.request(first_id).unwrap();
    assert!(session.pusher.answering());
    let rest = session.drain();
    assert!(!session.pusher.answering());
    let end = rest.iter().position(|frame| parts(frame).0 == 0).unwrap();
    frames.extend_from_slice(&rest[..=end]);
    let pushed = collect(&frames, second_id);
    assert_eq!(pushed.kept.unwrap().base_template_id, first_id);
    let answer = collect(&rest[end + 1..], first_id);
    assert!(answer.kept.is_none());
    assert!(answer.pages > 0);
    assert_eq!(answer.transactions, raw(&first));
    // The answer left the base: the next push is a delta against the second.
    let third = template(1..=2002, 1024, 0xab);
    let third_id = session.offer(&third);
    let delta = collect(&session.drain(), third_id);
    assert_eq!(delta.kept.unwrap().base_template_id, second_id);
    // With nothing under way the answer starts at once, in pages.
    session.pusher.request(third_id).unwrap();
    let started = session.drain();
    assert_eq!(parts(&started[0]).1, REQUEST_TRANSACTION_DATA_TRANSACTIONS);
    assert_eq!(collect(&started, third_id).transactions, raw(&third));
    // Unknown ids get the usual error, also behind a push.
    session.pusher.request(999).unwrap();
    assert_eq!(
        error_code(&session.next().unwrap()),
        "template-id-not-found"
    );
    let fourth = template(1..=2003, 1024, 0xab);
    let fourth_id = session.offer(&fourth);
    session.next().unwrap();
    session.pusher.request(999).unwrap();
    let behind = session.drain();
    assert_eq!(error_code(behind.last().unwrap()), "template-id-not-found");
    let (extension, kind, payload) = parts(&behind[behind.len() - 2]);
    assert_eq!(
        (extension, kind),
        (0, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS)
    );
    let mut bytes = payload.to_vec();
    let success: RequestTransactionDataSuccess = binary_sv2::from_bytes(&mut bytes).unwrap();
    assert_eq!(success.template_id, fourth_id);
    // Templates of a replaced parent are stale.
    session.replace_parent();
    session.pusher.request(fourth_id).unwrap();
    assert_eq!(error_code(&session.next().unwrap()), "stale-template-id");
    // One answer waits at a time.
    session.pusher.request(fourth_id).unwrap();
    assert!(session.pusher.request(fourth_id).is_err());
}

// What: the client refuses a push that breaks the extension: a Kept naming
// another base, a bitmap of the wrong length or with a high bit set, a
// Kept after a page, a page or Success for another template, a list whose
// merkle path differs or that repeats its last transaction (which an odd
// merkle level hides), and a Kept when no push finished.
// Why: a provider's list is checked before it is mined; Knuth's rules
// (docs/sv2.md, "Template flow").
// Look here if: Received's checks change.
#[test]
fn the_client_refuses_a_push_that_breaks_the_rules() {
    let first = template(1..=9, 0, 0xab);
    let second = template(1..=10, 0, 0xab);
    // A base with 9 transactions, and the push of the second announced.
    let based = || {
        let mut received = Received::default();
        received.announced(1);
        let mut session = Session::default();
        session.offer(&first);
        receive(&mut received, &session.drain(), 1, &first).unwrap();
        received.announced(2);
        received
    };
    let kept = |base: u64, bitmap: Vec<u8>| {
        RequestTransactionDataKept {
            template_id: 2,
            base_template_id: base,
            kept: bitmap,
        }
        .encode()
        .unwrap()
    };
    assert!(based().kept(&kept(1, vec![0xff, 0x01])).is_ok());
    for (base, bitmap) in [
        (7, vec![0xff, 0x01]),
        (1, vec![0xff]),
        (1, vec![0xff, 0x01, 0x00]),
        (1, vec![0xff, 0x03]),
    ] {
        assert!(based().kept(&kept(base, bitmap)).is_err());
    }
    let page = |id: u64| {
        RequestTransactionDataTransactions {
            template_id: id,
            transactions: vec![tx(10, 0)],
        }
        .encode()
        .unwrap()
    };
    let mut late = based();
    late.page(&page(2)).unwrap();
    assert!(late.kept(&kept(1, vec![0xff, 0x01])).is_err());
    assert!(based().page(&page(3)).is_err());
    let whole = |received: &mut Received, path: &[Hash], last: Vec<Vec<u8>>| {
        received.finish(2, last.into_iter().map(Arc::from).collect(), path)
    };
    let mut good = based();
    assert!(whole(&mut good, second.merkle_path(), raw(&second)).is_ok());
    assert!(whole(&mut based(), first.merkle_path(), raw(&second)).is_err());
    let mut repeated = raw(&second);
    let last = repeated.last().cloned().unwrap();
    repeated.push(last);
    let path = coinbase_path(
        &repeated
            .iter()
            .map(|tx| double_sha256(tx))
            .collect::<Vec<_>>(),
    );
    assert!(whole(&mut based(), &path, repeated).is_err());
    let mut wrong = based();
    assert!(wrong.finish(3, Vec::new(), &[]).is_err());
    let mut fresh = Received::default();
    fresh.announced(2);
    assert!(fresh.kept(&kept(1, vec![0xff, 0x01])).is_err());
}
