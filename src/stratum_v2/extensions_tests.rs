//! Byte-level checks against Knuth's own vectors. Each `knuth` comment names
//! the test file and line at the extension's head in CyberAshven/kth:
//! worker-hashrate 3311cc2555, jd-payout-outputs 854d7100b8,
//! jd-large-templates 6370ee1e1c.

use super::*;

/// Knuth's `marked_hash(first)`: bytes first, first + 1, ... in order
/// (src/node/test/sv2_jd_messages.cpp:19).
fn marked(first: u8) -> Hash {
    std::array::from_fn(|index| first.wrapping_add(index as u8))
}

/// Encodes, decodes back to the same message and bytes, and refuses every
/// shorter buffer and one trailing byte, as Knuth's `require_codec` does.
fn round_trip<M: ExtensionMessage + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode().unwrap();
    assert_eq!(M::decode(&bytes).as_ref(), Some(message));
    for cut in 0..bytes.len() {
        assert!(
            M::decode(&bytes[..cut]).is_none(),
            "{cut} of {}",
            bytes.len()
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(M::decode(&trailing).is_none());
    bytes
}

/// The frame's header: the type (channel bit clear), the message type and
/// the U24 length, then the payload.
fn framed<M: ExtensionMessage>(message: &M) -> Vec<u8> {
    message.frame().unwrap().as_bytes().to_vec()
}

// What: RequestExtensions and its answers encode as Knuth's vectors (a U16
// request id, SEQ0_64K[U16] lists) and frame with extension 0x0001, message
// types 0, 1 and 2 and the channel bit clear; a list over 65,535 items is
// refused.
// Look here if: the negotiation's codec changes.
#[test]
fn negotiation_messages_match_knuths_bytes() {
    // knuth: src/node/test/sv2_extensions.cpp:56 (worker-hashrate head).
    let ask = RequestExtensions {
        request_id: 1,
        requested: vec![WORKER_HASHRATE],
    };
    assert_eq!(round_trip(&ask), [0x01, 0x00, 0x01, 0x00, 0x02, 0x00]);
    // knuth: src/node/test/sv2_jd_messages.cpp:286 (jd-payout-outputs head).
    let payout = RequestExtensions {
        request_id: 1,
        requested: vec![PAYOUT_OUTPUTS],
    };
    assert_eq!(round_trip(&payout), [0x01, 0x00, 0x01, 0x00, 0x52, 0x50]);
    assert_eq!(
        framed(&payout),
        [0x01, 0x00, 0x00, 0x06, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x52, 0x50]
    );
    round_trip(&RequestExtensions::default());
    let granted = RequestExtensionsSuccess {
        request_id: 7,
        supported: vec![0x0002, 0x0003],
    };
    assert_eq!(
        round_trip(&granted),
        [0x07, 0x00, 0x02, 0x00, 0x02, 0x00, 0x03, 0x00]
    );
    assert_eq!(framed(&granted)[..3], [0x01, 0x00, 0x01]);
    let refused = RequestExtensionsError {
        request_id: 4,
        unsupported: vec![0x0002],
        required: vec![0x00ab],
    };
    assert_eq!(
        round_trip(&refused),
        [0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0xab, 0x00]
    );
    assert_eq!(framed(&refused)[..3], [0x01, 0x00, 0x02]);
    let over = RequestExtensions {
        request_id: 1,
        requested: vec![0; SEQ_MAX + 1],
    };
    assert!(over.encode().is_err());
    let full = RequestExtensions {
        request_id: 1,
        requested: vec![PAYOUT_OUTPUTS; SEQ_MAX],
    };
    assert_eq!(
        RequestExtensions::decode(&full.encode().unwrap()),
        Some(full)
    );
}

// What: the server's answer grants the supported extensions in the order
// asked, once each; asking for none it supports, or for nothing, is answered
// with Error naming them and requiring none (Knuth's mining_session
// negotiate).
// Look here if: `negotiation_answer` changes.
#[test]
fn the_answer_grants_what_is_supported_or_refuses_all() {
    let ask = |requested: Vec<u16>| RequestExtensions {
        request_id: 5,
        requested,
    };
    let supported = |extension: u16| matches!(extension, 0x4D4D | WORKER_HASHRATE);
    let mut frame =
        negotiation_answer(&ask(vec![0x0003, 0x0002, 0x4D4D, 0x0002]), supported).unwrap();
    assert_eq!(frame.header().msg_type(), REQUEST_EXTENSIONS_SUCCESS);
    assert_eq!(
        RequestExtensionsSuccess::decode(frame.payload()),
        Some(RequestExtensionsSuccess {
            request_id: 5,
            supported: vec![0x0002, 0x4D4D],
        })
    );
    for requested in [vec![0x0003], vec![]] {
        let mut frame = negotiation_answer(&ask(requested.clone()), supported).unwrap();
        assert_eq!(frame.header().msg_type(), REQUEST_EXTENSIONS_ERROR);
        assert_eq!(
            RequestExtensionsError::decode(frame.payload()),
            Some(RequestExtensionsError {
                request_id: 5,
                unsupported: requested,
                required: Vec::new(),
            })
        );
    }
}

// What: the worker TLV is U16 type, U8 field and U16 length little-endian
// then the name (Knuth's Worker_001 and rig-7 vectors); fields follow each
// other to the end; a header or value cut short is malformed; a name is cut
// to 32 bytes between characters.
// Look here if: `Tlv` changes.
#[test]
fn worker_fields_match_knuths_bytes() {
    // knuth: src/node/test/sv2_extensions.cpp:79 (worker-hashrate head).
    let worker = Tlv::user_identity("Worker_001");
    let mut expected = vec![0x02, 0x00, 0x01, 0x0a, 0x00];
    expected.extend_from_slice(b"Worker_001");
    assert_eq!(worker.encode().unwrap(), expected);
    // knuth: src/node/test/sv2_extensions.cpp:90.
    assert_eq!(
        Tlv::user_identity("").encode().unwrap(),
        [0x02, 0x00, 0x01, 0x00, 0x00]
    );
    // knuth: src/node/test/sv2_mining_session.cpp:788.
    let rig = [0x02, 0x00, 0x01, 0x05, 0x00, b'r', b'i', b'g', b'-', b'7'];
    assert_eq!(Tlv::user_identity("rig-7").encode().unwrap(), rig);
    let mut two = Tlv {
        extension: 0x0003,
        field: 0x07,
        value: vec![0xaa],
    }
    .encode()
    .unwrap();
    two.extend_from_slice(&expected);
    let fields = Tlv::read_all(&two).unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].value, [0xaa]);
    assert_eq!(fields[1], worker);
    assert_eq!(Tlv::read_all(&[]), Some(Vec::new()));
    for cut in 1..expected.len() {
        assert!(Tlv::read_all(&expected[..cut]).is_none(), "{cut}");
    }
    // knuth: src/node/test/sv2_extensions.cpp:120-127, the 32-byte cut.
    let exact = "w".repeat(32);
    assert_eq!(
        Tlv::user_identity(&format!("{exact}x")).value,
        exact.as_bytes()
    );
    let straddling = format!("{}é", "w".repeat(31));
    assert_eq!(
        Tlv::user_identity(&straddling).value,
        "w".repeat(31).as_bytes()
    );
    let fits = format!("{}é", "w".repeat(30));
    assert_eq!(Tlv::user_identity(&fits).value, fits.as_bytes());
}

// What: what a share's trailing bytes name: without the grant whole fields
// are ignored; with it the first user_identity names the worker, other
// fields (another extension's, or 0x0002's field 2) are ignored, and one
// over 32 bytes is `user-identity-too-long`; bytes that are not whole fields
// are malformed either way (Knuth's stray byte, short header and long
// length).
// Look here if: `share_worker` changes.
#[test]
fn a_share_names_its_worker_only_once_granted() {
    let fields = |list: &[Tlv]| -> Vec<u8> {
        list.iter()
            .flat_map(|field| field.encode().unwrap())
            .collect()
    };
    let named = fields(&[
        Tlv {
            extension: 0x0003,
            field: 0x01,
            value: vec![1, 2],
        },
        Tlv::user_identity("rig-7"),
        Tlv {
            extension: WORKER_HASHRATE,
            field: 0x02,
            value: vec![9],
        },
        Tlv::user_identity("second"),
    ]);
    assert_eq!(share_worker(&named, true), Ok(Some("rig-7".into())));
    assert_eq!(share_worker(&named, false), Ok(None));
    assert_eq!(share_worker(&[], true), Ok(None));
    let long = fields(&[Tlv {
        extension: WORKER_HASHRATE,
        field: USER_IDENTITY_FIELD,
        value: vec![b'w'; 33],
    }]);
    assert_eq!(share_worker(&long, true), Err(Some(USER_IDENTITY_TOO_LONG)));
    assert_eq!(share_worker(&long, false), Ok(None));
    // knuth: src/node/test/sv2_mining_session.cpp:791-792.
    for malformed in [
        &[0x00][..],
        &[0x02, 0x00, 0x01],
        &[0x02, 0x00, 0x01, 0x05, 0x00, b'r', b'i'],
    ] {
        assert_eq!(share_worker(malformed, true), Err(None));
        assert_eq!(share_worker(malformed, false), Err(None));
    }
}

// What: Requested Payout Outputs' four messages encode as Knuth's vectors,
// frame with extension 0x5052 and types 0 to 3, and refuse a token or prefix
// over 255 bytes and outputs over 65,535.
// Look here if: the 0x5052 codec changes.
#[test]
fn payout_output_messages_match_knuths_bytes() {
    // knuth: src/node/test/sv2_jd_messages.cpp:308-327 (jd-payout-outputs head).
    let request = RequestPayoutOutputs {
        request_id: 0x0403_0201,
        token: vec![0xaa, 0xbb],
        available_payout_value: 0x0807_0605_0403_0201,
        height: 101,
        prev_hash: marked(1),
        nbits: 0x207f_ffff,
        coinbase_prefix: vec![0x03, 0x65],
    };
    let mut expected = vec![0x01, 0x02, 0x03, 0x04, 0x02, 0xaa, 0xbb];
    expected.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
    expected.extend_from_slice(&[0x65, 0x00, 0x00, 0x00]);
    expected.extend((1..=0x20).collect::<Vec<u8>>());
    expected.extend_from_slice(&[0xff, 0xff, 0x7f, 0x20, 0x02, 0x03, 0x65]);
    assert_eq!(round_trip(&request), expected);
    assert_eq!(framed(&request)[..6], [0x52, 0x50, 0x00, 58, 0, 0]);
    // knuth: src/node/test/sv2_jd_messages.cpp:338-352.
    let success = RequestPayoutOutputsSuccess {
        request_id: 7,
        flags: EXACT_OUTPUTS,
        version: 2,
        sequence: 0xffff_fffe,
        locktime: 100,
        outputs: vec![0x51],
    };
    assert_eq!(
        round_trip(&success),
        [
            0x07, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0xfe, 0xff,
            0xff, 0xff, 0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x51
        ]
    );
    assert_eq!(framed(&success)[..3], [0x52, 0x50, 0x01]);
    // knuth: src/node/test/sv2_jd_messages.cpp:364-366.
    let stale = RequestPayoutOutputsError {
        request_id: 7,
        code: STALE_PAYOUT_OUTPUTS.into(),
    };
    let mut expected = vec![0x07, 0x00, 0x00, 0x00, 20];
    expected.extend_from_slice(b"stale-payout-outputs");
    assert_eq!(round_trip(&stale), expected);
    assert_eq!(framed(&stale)[..3], [0x52, 0x50, 0x02]);
    // knuth: src/node/test/sv2_jd_messages.cpp:367-368: no payload.
    assert_eq!(
        framed(&PayoutOutputsStale),
        [0x52, 0x50, 0x03, 0x00, 0x00, 0x00]
    );
    assert_eq!(PayoutOutputsStale::decode(&[0]), None);
    // The largest fields pass; one byte more is refused.
    let largest = RequestPayoutOutputs {
        token: vec![0; 255],
        coinbase_prefix: vec![0; 255],
        ..request.clone()
    };
    round_trip(&largest);
    for over in [
        RequestPayoutOutputs {
            token: vec![0; 256],
            ..request.clone()
        },
        RequestPayoutOutputs {
            coinbase_prefix: vec![0; 256],
            ..request
        },
    ] {
        assert!(over.encode().is_err());
    }
    assert!(RequestPayoutOutputsSuccess {
        outputs: vec![0; SEQ_MAX + 1],
        ..success.clone()
    }
    .encode()
    .is_err());
    assert!(RequestPayoutOutputsSuccess {
        outputs: vec![0; SEQ_MAX],
        ..success
    }
    .encode()
    .is_ok());
    assert!(RequestPayoutOutputsError {
        request_id: 1,
        code: "e".repeat(256),
    }
    .encode()
    .is_err());
}

// What: 0x4C54's page and positions encode as Knuth's vectors (a U32 request
// id, SEQ0_64K of 32-byte ids or U32 positions, positions past U16 exact),
// frame with extension 0x4C54 and types 1 and 2, and refuse 65,536 items.
// Look here if: the 0x4C54 codec changes.
#[test]
fn large_template_messages_match_knuths_bytes() {
    // knuth: src/node/test/sv2_jd_messages.cpp:398-403 (jd-large-templates head).
    let page = DeclareMiningJobTransactions {
        request_id: 0x0403_0201,
        txids: vec![marked(1), marked(0x40)],
    };
    let bytes = round_trip(&page);
    assert_eq!(bytes.len(), 4 + 2 + 2 * 32);
    assert_eq!(bytes[..6], [0x01, 0x02, 0x03, 0x04, 0x02, 0x00]);
    assert_eq!(bytes[6..38], marked(1));
    assert_eq!(framed(&page)[..3], [0x54, 0x4c, 0x01]);
    round_trip(&DeclareMiningJobTransactions::default());
    assert!(DeclareMiningJobTransactions {
        request_id: 1,
        txids: vec![[0; 32]; SEQ_MAX + 1],
    }
    .encode()
    .is_err());
    // knuth: src/node/test/sv2_jd_messages.cpp:410-412.
    let ask = ProvideMissingTransactionsPositions {
        request_id: 7,
        positions: vec![0x0001_0000, 3],
    };
    assert_eq!(
        round_trip(&ask),
        [0x07, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00]
    );
    assert_eq!(framed(&ask)[..3], [0x54, 0x4c, 0x02]);
    assert!(ProvideMissingTransactionsPositions {
        request_id: 1,
        positions: vec![0; SEQ_MAX + 1],
    }
    .encode()
    .is_err());
}

// What: paging leaves a list of up to 65,535 ids whole; past that, full
// pages of 65,535 go first and DeclareMiningJob carries the last 1 to
// 65,535, as Knuth's declare_work splits them (jd_declaration.cpp:352-368 at
// the jd-large-templates head).
// Look here if: `pages` changes.
#[test]
fn paging_splits_as_knuth_does() {
    let ids = |count: usize| -> Vec<Hash> {
        (0..count)
            .map(|index| {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&(index as u64).to_le_bytes());
                id
            })
            .collect()
    };
    for (count, paged, last) in [
        (0, 0, 0),
        (1, 0, 1),
        (SEQ_MAX, 0, SEQ_MAX),
        (SEQ_MAX + 1, 1, 1),
        (2 * SEQ_MAX, 1, SEQ_MAX),
        (2 * SEQ_MAX + 1, 2, 1),
    ] {
        let all = ids(count);
        let (pages, tail) = pages(&all);
        assert_eq!((pages.len(), tail.len()), (paged, last), "{count}");
        assert!(pages.iter().all(|page| page.len() == SEQ_MAX));
        let joined: Vec<Hash> = pages.concat().into_iter().chain(tail.to_vec()).collect();
        assert_eq!(joined, all);
    }
}
