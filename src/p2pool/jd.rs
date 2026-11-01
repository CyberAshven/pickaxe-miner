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

/// #### PR #65: the whole of one preparation request: the name lookup, the
/// connection, TLS, the request and every read of the reply (see
/// `http::post_json`).
pub const PREPARE_DEADLINE: Duration = Duration::from_secs(2);

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
        let response = super::http::post_json(&self.api, &body, 160_000, PREPARE_DEADLINE)?
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
    // #### PR #65: whose coinbase this is
    // The reply names no payout or share owner, and the share commitment
    // (the 32-byte OP_RETURN) is a hash whose preimage stays with P2Pool,
    // while the PPLNS outputs pay whoever holds shares in the window, so
    // these bytes cannot show which session (personal or donation) they are
    // for. They are bound to this session's own token (`token_hash` above),
    // which the pool allocated to this session's identity; the pool's
    // acknowledgement of the declared job, which it validates for that
    // identity, is the guard against swapped coinbases. Devices mine a
    // P2Pool job only once its own session acknowledged it.
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

    // #### PR #65
    // What: the fake helper's answers prepare exactly, and one that never
    // answers fails the preparation at its 2 s deadline.
    // Look here if: PREPARE_DEADLINE or the fake helper changes.
    #[test]
    fn a_hung_helper_fails_the_preparation_at_its_deadline() {
        use super::fake::{plan, serve, Answer};
        let (url, requests) = serve(vec![Answer::Valid, Answer::Hang]);
        let plan = plan(&url, "owner", 2);
        let prepared = plan
            .prepare(100, 0x1d00ffff, &[2; 32], 10000, &[1, 100], None)
            .unwrap();
        assert_eq!(prepared.coinbase.head, [1, 100]);
        let start = std::time::Instant::now();
        let error = plan
            .prepare(100, 0x1d00ffff, &[2; 32], 10000, &[1, 100], None)
            .unwrap_err();
        let took = start.elapsed();
        assert!(error.contains("failed"), "{error}");
        assert!(
            took >= PREPARE_DEADLINE - Duration::from_millis(100)
                && took < PREPARE_DEADLINE + Duration::from_secs(1),
            "{took:?}"
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

/// #### PR #65: a /jd_prepare helper on 127.0.0.1 for tests.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use crate::stratum_v2::jd::codec::serialize_outputs;
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Arc,
        time::Instant,
    };

    /// What the helper does with a request.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Answer {
        /// A preparation that passes `validate` for the request's template.
        Valid,
        /// HTTP 503.
        Refuse,
        /// Reads the request and keeps the connection without answering
        /// for 5 s.
        Hang,
    }

    /// When each request came, and its share owner.
    pub(crate) type Requests = Arc<Mutex<Vec<(Instant, String)>>>;

    /// Serves `answers` to requests in turn, the last one for all later
    /// requests, each connection on its own thread; the helper's URL and
    /// the requests it got.
    pub(crate) fn serve(answers: Vec<Answer>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Requests::default();
        let seen = requests.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let answer = answers[n.min(answers.len() - 1)];
                let seen = seen.clone();
                std::thread::spawn(move || answer_one(stream, answer, &seen));
            }
        });
        (format!("http://127.0.0.1:{port}/jd_prepare"), requests)
    }

    fn answer_one(mut stream: std::net::TcpStream, answer: Answer, seen: &Requests) {
        let mut raw = Vec::new();
        let mut buffer = [0u8; 4096];
        let body = loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => raw.extend_from_slice(&buffer[..read]),
            }
            let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= end + 4 + length {
                break raw[end + 4..end + 4 + length].to_vec();
            }
        };
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        if let Ok(mut seen) = seen.lock() {
            seen.push((
                Instant::now(),
                request["share_owner"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ));
        }
        let reply = match answer {
            Answer::Hang => {
                std::thread::sleep(Duration::from_secs(5));
                return;
            }
            Answer::Refuse => b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec(),
            Answer::Valid => {
                let json = serde_json::to_vec(&reply(&request)).unwrap();
                let mut reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    json.len()
                )
                .into_bytes();
                reply.extend(json);
                reply
            }
        };
        let _ = stream.write_all(&reply);
    }

    /// The exact preparation for `request`: its revenue paid to an output
    /// naming the share owner, the empty aux commitment and two more
    /// P2Pool outputs.
    pub(crate) fn reply(request: &serde_json::Value) -> serde_json::Value {
        let token = hex::decode(request["token"].as_str().unwrap_or_default()).unwrap_or_default();
        let height = request["height"].as_u64().unwrap_or_default() as u32;
        let value = request["coinbase_value"].as_u64().unwrap_or_default();
        let prefix =
            hex::decode(request["script_prefix"].as_str().unwrap_or_default()).unwrap_or_default();
        let owner = request["share_owner"]
            .as_str()
            .unwrap_or_default()
            .as_bytes();
        let mut head = 2u32.to_le_bytes().to_vec();
        head.push(1);
        head.extend([0; 32]);
        head.extend(u32::MAX.to_le_bytes());
        head.push((prefix.len() + 12) as u8);
        head.extend(&prefix);
        let mut aux = vec![0x6a, 44, 0xfa, 0xbe, 0x6d, 0x6d];
        aux.extend([0; 40]);
        let mut pays = vec![0x6a, owner.len().min(64) as u8];
        pays.extend(&owner[..owner.len().min(64)]);
        let mut tail = 0xffff_fffeu32.to_le_bytes().to_vec();
        tail.extend(serialize_outputs(&[
            (value, pays),
            (0, aux),
            (0, vec![0x6a, 32]),
            (0, vec![0x6a]),
        ]));
        tail.extend(height.saturating_sub(1).to_le_bytes());
        let anchor: Vec<u8> = token
            .get(33..65)
            .unwrap_or_default()
            .iter()
            .rev()
            .copied()
            .collect();
        serde_json::json!({
            "version": 1,
            "token_hash": hex::encode(Sha256::digest(&token)),
            "anchor": hex::encode(anchor),
            "height": height,
            "coinbase_value": value,
            "coinbase_prefix": hex::encode(head),
            "coinbase_suffix": hex::encode(tail),
            "extranonce_size": 12,
        })
    }

    /// A preparation plan at `url` for `share_owner`, with `tokens` tokens.
    pub(crate) fn plan(url: &str, share_owner: &str, tokens: usize) -> Plan {
        Plan {
            api: url.to_owned(),
            payout: "payout".into(),
            share_owner: share_owner.to_owned(),
            tokens: Mutex::new(vec![vec![1; 76]; tokens].into()),
            consumed: AtomicUsize::new(0),
        }
    }
}
