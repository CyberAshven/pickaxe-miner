//! #### PR #65
//! P2Pool-specific coinbase preparation stays in the P2Pool process. The
//! miner's own BCH template and transaction list never leave this adapter
//! until it sends the standard Full-Template Job Declaration, if selected.
use crate::stratum_v2::jd::declared::CoinbaseShape;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

#[derive(Debug)]
pub struct Plan {
    pub api: String,
    pub payout: String,
    pub share_owner: String,
    pub tokens: Mutex<VecDeque<Vec<u8>>>,
    pub consumed: AtomicUsize,
}

impl PartialEq for Plan {
    fn eq(&self, other: &Self) -> bool {
        (
            self.api.as_str(),
            self.payout.as_str(),
            self.share_owner.as_str(),
        ) == (
            other.api.as_str(),
            other.payout.as_str(),
            other.share_owner.as_str(),
        )
    }
}
impl Eq for Plan {}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub token: Vec<u8>,
    pub coinbase: CoinbaseShape,
}

#[derive(Serialize)]
struct Auxiliary {
    merkle_root: [u8; 32],
    tree_size: u32,
    nonce: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u8,
    token_hash: String,
    anchor: String,
    height: u32,
    coinbase_value: u64,
    coinbase_prefix: String,
    coinbase_suffix: String,
    extranonce_size: u8,
}

impl Plan {
    /// Uses an already allocated token. Returned bytes are checked before any
    /// device can hash them; the SV2 pool independently checks PPLNS membership.
    pub fn prepare(
        &self,
        height: u32,
        bits: u32,
        previous_hash: &[u8; 32],
        value: u64,
        prefix: &[u8],
        auxiliary: Option<([u8; 32], u32, u32)>,
    ) -> Result<Prepared, String> {
        let token = self
            .tokens
            .lock()
            .map_err(|_| "P2Pool token queue unavailable")?
            .pop_front()
            .ok_or("waiting for a P2Pool job token")?;
        // Replenish even if the HTTP helper fails or its anchor is stale.
        // Otherwise two preparation failures would permanently empty the queue.
        self.consumed.fetch_add(1, Ordering::Relaxed);
        if token.first() != Some(&1) || !(65..=255).contains(&token.len()) {
            return Err("unsupported BCH P2Pool job token".into());
        }
        let aux = auxiliary.map_or(
            Auxiliary {
                merkle_root: [0; 32],
                tree_size: 0,
                nonce: 0,
            },
            |(merkle_root, tree_size, nonce)| Auxiliary {
                merkle_root,
                tree_size,
                nonce,
            },
        );
        let body = serde_json::to_vec(&serde_json::json!({
            "version":1,"token":hex::encode(&token),"payout":self.payout,
            "share_owner":self.share_owner,"height":height,"nbits":bits,"coinbase_value":value,
            "previous_hash":hex::encode(previous_hash.iter().rev().copied().collect::<Vec<_>>()),
            "script_prefix":hex::encode(prefix),"auxiliary":aux,
        }))
        .map_err(|_| "cannot encode P2Pool preparation")?;
        let response = super::http::post_json(&self.api, &body, 160_000, Duration::from_secs(3))?
            .ok("P2Pool preparation")?;
        let reply: Reply =
            serde_json::from_slice(&response).map_err(|_| "invalid P2Pool preparation response")?;
        validate(reply, &token, height, value, prefix, &aux)
    }
}

fn validate(
    reply: Reply,
    token: &[u8],
    height: u32,
    value: u64,
    prefix: &[u8],
    aux: &Auxiliary,
) -> Result<Prepared, String> {
    use sha2::{Digest, Sha256};
    if reply.version != 1
        || reply.extranonce_size != 12
        || reply.height != height
        || reply.coinbase_value != value
        || reply.token_hash != hex::encode(Sha256::digest(token))
        || reply.anchor != hex::encode(token[33..65].iter().rev().copied().collect::<Vec<_>>())
        || reply.coinbase_prefix.len() > 131070
        || reply.coinbase_suffix.len() > 131070
    {
        return Err("P2Pool preparation does not match this token and template".into());
    }
    let head = hex::decode(reply.coinbase_prefix).map_err(|_| "invalid P2Pool coinbase")?;
    let tail = hex::decode(reply.coinbase_suffix).map_err(|_| "invalid P2Pool coinbase")?;
    let coinbase =
        CoinbaseShape::parse(&head, &tail).map_err(|_| "invalid P2Pool coinbase shape")?;
    if coinbase.prefix != head
        || coinbase.suffix != tail
        || coinbase.tx_version != 2
        || coinbase.sequence != 0xffff_fffe
        || coinbase.locktime != height.checked_sub(1).ok_or("invalid BCH height")?
        || coinbase.extranonce_len() != 12
        || coinbase.head != prefix
        || coinbase
            .outputs
            .iter()
            .try_fold(0u64, |sum, (amount, _)| sum.checked_add(*amount))
            != Some(value)
    {
        return Err("P2Pool preparation changed template revenue or coinbase layout".into());
    }
    let index = coinbase
        .outputs
        .len()
        .checked_sub(3)
        .ok_or("P2Pool commitments missing")?;
    let (amount, script) = &coinbase.outputs[index];
    let mut expected = vec![0x6a, 44, 0xfa, 0xbe, 0x6d, 0x6d];
    expected.extend(aux.merkle_root);
    expected.extend(aux.tree_size.to_le_bytes());
    expected.extend(aux.nonce.to_le_bytes());
    if *amount != 0 || *script != expected {
        return Err("P2Pool preparation changed the aux commitment".into());
    }
    Ok(Prepared {
        token: token.to_vec(),
        coinbase,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stratum_v2::jd::codec::serialize_outputs;
    use sha2::{Digest, Sha256};

    fn fixture() -> (Reply, Vec<u8>, Auxiliary) {
        let mut token = vec![0; 76];
        token[0] = 1;
        token[33..65].fill(0x37);
        let mut head = 2u32.to_le_bytes().to_vec();
        head.push(1);
        head.extend([0; 32]);
        head.extend(u32::MAX.to_le_bytes());
        head.extend([14, 1, 100]);
        let mut aux_script = vec![0x6a, 44, 0xfa, 0xbe, 0x6d, 0x6d];
        aux_script.extend([0; 40]);
        let mut tail = 0xffff_fffeu32.to_le_bytes().to_vec();
        tail.extend(serialize_outputs(&[
            (10000, vec![0x51]),
            (0, aux_script),
            (0, vec![0x6a, 32]),
            (0, vec![0x6a]),
        ]));
        tail.extend(99u32.to_le_bytes());
        (
            Reply {
                version: 1,
                token_hash: hex::encode(Sha256::digest(&token)),
                anchor: hex::encode([0x37; 32]),
                height: 100,
                coinbase_value: 10000,
                coinbase_prefix: hex::encode(head),
                coinbase_suffix: hex::encode(tail),
                extranonce_size: 12,
            },
            token,
            Auxiliary {
                merkle_root: [0; 32],
                tree_size: 0,
                nonce: 0,
            },
        )
    }

    #[test]
    fn prepared_coinbase_preserves_revenue_and_all_pplns_bytes() {
        let (reply, token, aux) = fixture();
        let raw_head = hex::decode(&reply.coinbase_prefix).unwrap();
        let raw_tail = hex::decode(&reply.coinbase_suffix).unwrap();
        let prepared = validate(reply, &token, 100, 10000, &[1, 100], &aux).unwrap();
        let raw = prepared.coinbase.coinbase(&[5; 12]).unwrap();
        assert_eq!(&raw[..raw_head.len()], raw_head);
        assert_eq!(&raw[raw.len() - raw_tail.len()..], raw_tail);
        assert_eq!(prepared.token, token);
        assert!(prepared.coinbase.coinbase(&[5; 16]).is_none());
    }

    #[test]
    fn preparation_refuses_changed_token_tip_revenue_and_aux_root() {
        let (mut reply, token, aux) = fixture();
        reply.token_hash = hex::encode([0; 32]);
        assert!(validate(reply, &token, 100, 10000, &[1, 100], &aux).is_err());
        let (mut reply, token, aux) = fixture();
        reply.anchor = hex::encode([0; 32]);
        assert!(validate(reply, &token, 100, 10000, &[1, 100], &aux).is_err());
        let (reply, token, aux) = fixture();
        assert!(validate(reply, &token, 100, 10001, &[1, 100], &aux).is_err());
        let (reply, token, mut aux) = fixture();
        aux.merkle_root[0] = 1;
        assert!(validate(reply, &token, 100, 10000, &[1, 100], &aux).is_err());
    }

    #[test]
    fn failed_preparation_replenishes_the_token_without_reusing_it() {
        let port = super::super::http::tests::serve(vec![
            b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ]);
        let plan = Plan {
            api: format!("http://127.0.0.1:{port}/jd_prepare"),
            payout: "test".into(),
            share_owner: "owner".into(),
            tokens: Mutex::new(VecDeque::from([vec![1; 76]])),
            consumed: AtomicUsize::new(0),
        };
        assert!(plan
            .prepare(100, 0x1d00ffff, &[2; 32], 10000, &[1, 100], None)
            .is_err());
        assert_eq!(plan.consumed.load(Ordering::Relaxed), 1);
        assert!(plan.tokens.lock().unwrap().is_empty());
        assert!(plan
            .prepare(100, 0x1d00ffff, &[2; 32], 10000, &[1, 100], None)
            .unwrap_err()
            .contains("waiting"));
        assert_eq!(plan.consumed.load(Ordering::Relaxed), 1);
    }
}
