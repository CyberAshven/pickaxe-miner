//! #### PR #51
//! What: ELECTRIC (`PoWMineStateful`), a CashToken whose `mine` claim needs a
//! SHA-256d proof over 32 bytes, searched on CUDA or portable GPUs. The relay fee comes from
//! a pot covenant with keyless claims and owner-authorized refunds. Pickaxe
//! stores a separate recovery key; it never imports the payout wallet's key.
//! Why: the operator asked (2026-10-15) to mine the published contract as it
//! is, document its replay weakness, and be ready for a fixed contract. A
//! fixed contract is a new `Contract` variant (see docs/electric-token.md).
//! Check: `tools/reward-policy-vm/electric.mjs` runs the claims and refunds
//! built here through the BCH 2026 VM; the tests below check the proof
//! encoding against two claims accepted on mainnet.

pub mod gpu;
pub mod miner;
mod pending;
mod recovery;
pub(crate) mod ui;

use crate::config::{
    MiningNetwork, CHIPNET_DONATION_ADDRESS, DONATION_ADDRESS, SHREC_DONATION_ADDRESS,
};
use crate::donation::{Policy, Recipient, Schedule, Scheme, TokenDonation};
use crate::proof::hash256;
use crate::reward::{compact_uint, parse_txid_display, push_data, transaction_id};
use sha2::block_api::compress256;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// `PoWMineStateful` as compiled by cashc 0.13.3 (fingerprint
/// 80d20113…0bd4), without its constructor arguments.
const POW_MINE_STATEFUL_V1_BODY_HEX: &str = concat!(
    "547a82011488c5b175c0009d00ce5279517e8800cf768277609d76517f758178",
    "517f77547f75817b557f77547f758178548054797e597a7eaa53797f75005479",
    "8088c5765379a269537978547aa0635779776753798b77765979a06358797768",
    "6851807c54807e7c8b54807e07000000000000007e0376a914537a7e0288ac7e",
    "00cd8800d153798800d354799d00cc02e8039d51cd00c78851d1537a517e8851",
    "d28851d300d0537a949d51cc02e803a269c4529cc4539c9b69c4539c6352d100",
    "88686d7551",
);

/// The published mainnet category, in the order explorers print it.
pub const MAINNET_CATEGORY_HEX: &str =
    "821f86ed21c38043500d7364fa73108cce8990baf95c4a0925fc44abfc27f8c7";
/// The mainnet covenant's locking bytecode (P2SH32), as found holding the
/// baton on 2026-10-15.
pub const MAINNET_COVENANT_LOCK_HEX: &str =
    "aa20f3b3cc8e800544dacb055235a1d45d7b7d0804a894935d301ef4796a2b08e70587";
/// Pickaxe's Chipnet deployment of the same contract (genesis
/// `6ba9682c…3ee1`, 2026-10-18), with the mainnet constructor values.
pub const CHIPNET_CATEGORY_HEX: &str =
    "39629630f3f5ba91c6e656e7b79c2c8de2fc6f776bfe085e1b7eaf70aa8a5e76";
/// The published deployment's constructor values (`electric-webgpu-miner/src/config.ts`).
pub const MAINNET_REWARD: u64 = 60_000_000;
pub const MAINNET_MIN_DIFFICULTY: u8 = 3;
pub const MAINNET_MAX_DIFFICULTY: u8 = 4;

/// The contract requires exactly this on the reward output; the sats come
/// back with the tokens, they are not a fee.
pub const TOKEN_OUTPUT_SATS: u64 = 1_000;
/// The most one claim or refund may take from the pot as a fee.
pub const POT_MAX_FEE_SATS: u64 = 3_000;
/// The least a claim leaves in the pot (above the P2SH32 dust limit).
pub const POT_MIN_CHANGE_SATS: u64 = 1_000;
/// A claim needs a non-final input for the contract's locktime guard.
const CLAIM_SEQUENCE: u32 = 0xffff_fffe;

/// The least anyone donates to mine ELECTRIC: PHOTON's 4% work share.
pub const DONATION_MINIMUM: TokenDonation = TokenDonation::from_bps(400);

/// A contract a deployment runs. The published one hashes only the baton's
/// height, the recipient and the nonce, so a proof can be spent again while
/// the height stays put (docs/electric-token.md, "Replay").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contract {
    PoWMineStatefulV1,
}

/// One deployment of the contract: its category and constructor values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub network: MiningNetwork,
    pub contract: Contract,
    /// Display order.
    pub category: [u8; 32],
    pub reward: u64,
    pub min_difficulty: u8,
    pub max_difficulty: u8,
}

impl Deployment {
    /// The published mainnet deployment.
    pub fn mainnet() -> Self {
        Self {
            network: MiningNetwork::Mainnet,
            contract: Contract::PoWMineStatefulV1,
            category: parse_txid_display(MAINNET_CATEGORY_HEX).expect("constant category"),
            reward: MAINNET_REWARD,
            min_difficulty: MAINNET_MIN_DIFFICULTY,
            max_difficulty: MAINNET_MAX_DIFFICULTY,
        }
    }

    /// Pickaxe's Chipnet deployment.
    pub fn chipnet() -> Self {
        Self {
            network: MiningNetwork::Chipnet,
            contract: Contract::PoWMineStatefulV1,
            category: parse_txid_display(CHIPNET_CATEGORY_HEX).expect("constant category"),
            reward: MAINNET_REWARD,
            min_difficulty: MAINNET_MIN_DIFFICULTY,
            max_difficulty: MAINNET_MAX_DIFFICULTY,
        }
    }

    /// The deployment to mine: the published one on mainnet, Pickaxe's on
    /// Chipnet, or one the miner names (such as their own deploy).
    pub fn select(
        network: MiningNetwork,
        category: Option<&str>,
        reward: Option<u64>,
        min_difficulty: Option<u8>,
        max_difficulty: Option<u8>,
    ) -> Result<Self, String> {
        let custom = category.is_some()
            || reward.is_some()
            || min_difficulty.is_some()
            || max_difficulty.is_some();
        if !custom {
            return Ok(match network {
                MiningNetwork::Mainnet => Self::mainnet(),
                MiningNetwork::Chipnet => Self::chipnet(),
            });
        }
        let category =
            category.ok_or("--electric-category is needed with other contract values")?;
        let deployment = Self {
            network,
            contract: Contract::PoWMineStatefulV1,
            category: parse_txid_display(category)
                .map_err(|_| "--electric-category must be 64 hexadecimal characters")?,
            reward: reward.unwrap_or(MAINNET_REWARD),
            min_difficulty: min_difficulty.unwrap_or(MAINNET_MIN_DIFFICULTY),
            max_difficulty: max_difficulty.unwrap_or(MAINNET_MAX_DIFFICULTY),
        };
        if network == MiningNetwork::Mainnet
            && deployment.category == Self::mainnet().category
            && deployment != Self::mainnet()
        {
            return Err("the published mainnet category has other constructor values".into());
        }
        if deployment.min_difficulty == 0
            || deployment.min_difficulty > deployment.max_difficulty
            || deployment.max_difficulty > 32
            || deployment.reward == 0
        {
            return Err("ELECTRIC difficulty must be 1 to 32 bytes, minimum first".into());
        }
        Ok(deployment)
    }

    pub fn category_hex(&self) -> String {
        hex::encode(self.category)
    }

    /// The category as it sits in transactions and the VM.
    pub fn category_le(&self) -> [u8; 32] {
        let mut out = self.category;
        out.reverse();
        out
    }

    /// Constructor arguments in reverse, as cashc places them, then the body.
    pub fn redeem_script(&self) -> Vec<u8> {
        let body = match self.contract {
            Contract::PoWMineStatefulV1 => {
                hex::decode(POW_MINE_STATEFUL_V1_BODY_HEX).expect("constant contract body")
            }
        };
        let mut out = Vec::with_capacity(body.len() + 45);
        out.extend(push_number(i64::from(self.max_difficulty)));
        out.extend(push_number(i64::from(self.min_difficulty)));
        out.extend(push_number(self.reward as i64));
        out.extend(push_bytes(&self.category_le()));
        out.extend(body);
        out
    }

    /// The covenant's P2SH32 locking bytecode.
    pub fn locking_bytecode(&self) -> Vec<u8> {
        p2sh32(&self.redeem_script())
    }

    /// The donation's work share at the miner's setting: PHOTON's policy,
    /// with anything above the minimum going to the project address.
    pub fn donation_policy(&self, donation: Option<TokenDonation>) -> Policy {
        let extra = donation
            .unwrap_or(DONATION_MINIMUM)
            .at_least(DONATION_MINIMUM)
            .bps()
            - DONATION_MINIMUM.bps();
        match self.network {
            MiningNetwork::Mainnet => Policy {
                scheme: Scheme::Work([200 + extra, 200]),
                addresses: [DONATION_ADDRESS, SHREC_DONATION_ADDRESS],
            },
            MiningNetwork::Chipnet => Policy {
                scheme: Scheme::Work([400 + extra, 0]),
                addresses: [CHIPNET_DONATION_ADDRESS, CHIPNET_DONATION_ADDRESS],
            },
        }
    }
}

/// The baton's 16-byte NFT commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub difficulty: u8,
    pub last_height: u32,
    pub mine_count: u32,
}

impl State {
    pub fn decode(commitment: &[u8]) -> Result<Self, String> {
        if commitment.len() != 16 {
            return Err("an ELECTRIC baton commitment is 16 bytes".into());
        }
        Ok(Self {
            difficulty: commitment[0],
            last_height: u32::from_le_bytes(commitment[1..5].try_into().expect("4 bytes")),
            mine_count: u32::from_le_bytes(commitment[5..9].try_into().expect("4 bytes")),
        })
    }

    pub fn encode(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0] = self.difficulty;
        out[1..5].copy_from_slice(&self.last_height.to_le_bytes());
        out[5..9].copy_from_slice(&self.mine_count.to_le_bytes());
        out
    }

    /// The state a claim with this locktime leaves (the contract's steps 3
    /// and 4): the minimum difficulty at a new height, else one byte more up
    /// to the maximum.
    pub fn next(self, locktime: u32, deployment: &Deployment) -> Result<Self, String> {
        if locktime < self.last_height {
            return Err("a claim's locktime cannot be below the baton's height".into());
        }
        let difficulty = if locktime > self.last_height {
            deployment.min_difficulty
        } else {
            self.difficulty
                .saturating_add(1)
                .min(deployment.max_difficulty)
        };
        Ok(Self {
            difficulty,
            last_height: locktime,
            mine_count: self.mine_count.wrapping_add(1),
        })
    }
}

/// The locktime of the claim the pot pays for at this tip, if any: one that
/// moves the baton to a new height, or the claim right after it at the
/// minimum difficulty. Anything else would let a spent proof be replayed
/// through the pot, so the pot refuses it and Pickaxe does not search for it.
pub fn claim_locktime(state: State, tip: u32, deployment: &Deployment) -> Option<u32> {
    if tip > state.last_height
        || (tip == state.last_height && state.difficulty == deployment.min_difficulty)
    {
        Some(tip)
    } else {
        None
    }
}

/// The 32 bytes the contract hashes: height (4, little-endian), recipient,
/// nonce (8 bytes here; the contract takes any length).
pub fn preimage(last_height: u32, pkh: &[u8; 20], nonce: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..4].copy_from_slice(&last_height.to_le_bytes());
    out[4..24].copy_from_slice(pkh);
    out[24..].copy_from_slice(&nonce.to_le_bytes());
    out
}

/// Whether the first `difficulty` bytes of `hash` are zero.
pub fn meets(hash: &[u8; 32], difficulty: u8) -> bool {
    usize::from(difficulty) <= hash.len() && hash[..usize::from(difficulty)].iter().all(|b| *b == 0)
}

const SHA256_IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// The first nonce in `start..start + count` whose proof meets `difficulty`.
/// Each try is two SHA-256 compressions (hardware SHA where the CPU has it).
pub fn search(
    last_height: u32,
    pkh: &[u8; 20],
    difficulty: u8,
    start: u64,
    count: u64,
) -> Option<u64> {
    if difficulty > 32 {
        return None;
    }
    // One 64-byte block each: the 32-byte message, 0x80, and its bit length.
    let mut first = [0u8; 64];
    first[..32].copy_from_slice(&preimage(last_height, pkh, 0));
    first[32] = 0x80;
    first[62] = 0x01;
    let mut second = [0u8; 64];
    second[32] = 0x80;
    second[62] = 0x01;
    let (words, bits) = (usize::from(difficulty / 4), u32::from(difficulty % 4) * 8);
    for offset in 0..count {
        let nonce = start.wrapping_add(offset);
        first[24..32].copy_from_slice(&nonce.to_le_bytes());
        let mut state = SHA256_IV;
        compress256(&mut state, &[first]);
        for (chunk, word) in second.as_chunks_mut::<4>().0.iter_mut().zip(state) {
            *chunk = word.to_be_bytes();
        }
        let mut digest = SHA256_IV;
        compress256(&mut digest, &[second]);
        if digest[..words].iter().all(|word| *word == 0)
            && (bits == 0 || digest[words] >> (32 - bits) == 0)
        {
            return Some(nonce);
        }
    }
    None
}

/// What one search is for: the baton's height and difficulty, and the key
/// hash of each recipient (miner, project, collaborator).
#[derive(Clone, Copy, Debug)]
pub struct SearchJob {
    pub last_height: u32,
    pub difficulty: u8,
    pub pkhs: [[u8; 20]; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Win {
    pub recipient: Recipient,
    pub pkh: [u8; 20],
    pub nonce: u64,
}

/// Hashes per batch a thread takes; about 20 ms on one core.
const SEARCH_BATCH: u32 = 1 << 18;

/// Searches on `threads` CPU threads until a proof is found or `stop` is
/// set. Each batch is for one recipient: the donation's share of the work is
/// taken from `schedule` before the batch is hashed, so a donation win pays
/// that address the whole reward and nothing is split afterwards.
pub fn parallel_search(
    job: SearchJob,
    schedule: &Mutex<Schedule>,
    threads: usize,
    next_nonce: &AtomicU64,
    stop: &AtomicBool,
    hashes: &AtomicU64,
) -> Option<Win> {
    let found = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let (recipient, count) = {
                        let mut schedule = schedule.lock().unwrap_or_else(|e| e.into_inner());
                        let recipient = schedule.recipient();
                        let count = schedule.limit_batch(SEARCH_BATCH);
                        if schedule.record(count).is_err() {
                            return;
                        }
                        (recipient, count)
                    };
                    let start = next_nonce.fetch_add(u64::from(count), Ordering::Relaxed);
                    let pkh = job.pkhs[recipient as usize];
                    let hit = search(
                        job.last_height,
                        &pkh,
                        job.difficulty,
                        start,
                        u64::from(count),
                    );
                    hashes.fetch_add(u64::from(count), Ordering::Relaxed);
                    if let Some(nonce) = hit {
                        stop.store(true, Ordering::Relaxed);
                        *found.lock().unwrap_or_else(|e| e.into_inner()) = Some(Win {
                            recipient,
                            pkh,
                            nonce,
                        });
                        return;
                    }
                }
            });
        }
    });
    found.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// An unspent baton as Fulcrum lists it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Baton {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
    pub token_amount: u64,
    pub state: State,
}

/// An unspent pot output.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Coin {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
}

/// A transaction ready to broadcast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    pub raw: Vec<u8>,
    pub txid: String,
    pub fee: u64,
}

/// The fee pot for one payout. It pays a claim's relay fee and the
/// 1,000 sats on the reward output, and only for a claim to the payout or a
/// donation address; anything else it returns to the payout address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pot {
    pub redeem: Vec<u8>,
    pub network: MiningNetwork,
    recovery_public_key: Option<secp256k1::PublicKey>,
}

impl Pot {
    /// `recipients` are P2PKH locking bytecodes (miner, project, collaborator);
    /// the miner's is also the refund address.
    pub fn new(deployment: &Deployment, recipients: &[Vec<u8>; 3]) -> Result<Self, String> {
        Self::with_recovery(deployment, recipients, None)
    }

    /// None preserves the original keyless pot for already-funded addresses.
    pub fn with_recovery(
        deployment: &Deployment,
        recipients: &[Vec<u8>; 3],
        recovery_public_key: Option<secp256k1::PublicKey>,
    ) -> Result<Self, String> {
        for lock in recipients {
            if lock.len() != 25 || lock[..3] != [0x76, 0xa9, 0x14] || lock[23..] != [0x88, 0xac] {
                return Err("ELECTRIC pays P2PKH addresses only".into());
            }
        }
        Ok(Self {
            redeem: pot_redeem(
                &deployment.locking_bytecode(),
                deployment.min_difficulty,
                recipients,
                &recipients[0],
                recovery_public_key.as_ref(),
            ),
            network: deployment.network,
            recovery_public_key,
        })
    }

    pub fn locking_bytecode(&self) -> Vec<u8> {
        p2sh32(&self.redeem)
    }

    /// Where the miner sends BCH to fund claims (P2SH32, no tokens).
    pub fn address(&self) -> String {
        crate::tx::cashaddr_with_version(0x0b, &hash256(&self.redeem), self.network)
    }
}

/// Electrum's scripthash of a locking bytecode.
pub fn script_hash_hex(locking: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash: [u8; 32] = Sha256::digest(locking).into();
    hash.reverse();
    hex::encode(hash)
}

/// The pot covenant. A claim (pot as input 1 of 2): input 0 is the ELECTRIC
/// covenant, output 0 pays one of `recipients`, output 1 keeps the baton's
/// sats, output 2 returns the rest to the pot with no token and at most
/// `POT_MAX_FEE_SATS` taken as fee, and the claim either moves the baton to a
/// new height or is the minimum-difficulty claim after it. A refund (the pot
/// alone): one output to `refund`, at most `POT_MAX_FEE_SATS` taken.
fn pot_redeem(
    electric_lock: &[u8],
    min_difficulty: u8,
    recipients: &[Vec<u8>; 3],
    refund: &[u8],
    recovery_public_key: Option<&secp256k1::PublicKey>,
) -> Vec<u8> {
    const OP_0: u8 = 0x00;
    const OP_1: u8 = 0x51;
    const OP_2: u8 = 0x52;
    const OP_3: u8 = 0x53;
    const OP_4: u8 = 0x54;
    const OP_IF: u8 = 0x63;
    const OP_ELSE: u8 = 0x67;
    const OP_ENDIF: u8 = 0x68;
    const OP_VERIFY: u8 = 0x69;
    const OP_DROP: u8 = 0x75;
    const OP_DUP: u8 = 0x76;
    const OP_OVER: u8 = 0x78;
    const OP_SWAP: u8 = 0x7c;
    const OP_SPLIT: u8 = 0x7f;
    const OP_BIN2NUM: u8 = 0x81;
    const OP_EQUAL: u8 = 0x87;
    const OP_EQUALVERIFY: u8 = 0x88;
    const OP_SUB: u8 = 0x94;
    const OP_BOOLOR: u8 = 0x9b;
    const OP_NUMEQUAL: u8 = 0x9c;
    const OP_NUMEQUALVERIFY: u8 = 0x9d;
    const OP_LESSTHAN: u8 = 0x9f;
    const OP_GREATERTHANOREQUAL: u8 = 0xa2;
    const OP_INPUTINDEX: u8 = 0xc0;
    const OP_TXINPUTCOUNT: u8 = 0xc3;
    const OP_TXOUTPUTCOUNT: u8 = 0xc4;
    const OP_TXLOCKTIME: u8 = 0xc5;
    const OP_UTXOVALUE: u8 = 0xc6;
    const OP_UTXOBYTECODE: u8 = 0xc7;
    const OP_OUTPUTVALUE: u8 = 0xcc;
    const OP_OUTPUTBYTECODE: u8 = 0xcd;
    const OP_UTXOTOKENCOMMITMENT: u8 = 0xcf;
    const OP_OUTPUTTOKENCATEGORY: u8 = 0xd1;

    let mut s = vec![OP_INPUTINDEX, OP_1, OP_NUMEQUAL, OP_IF];
    // Claim.
    s.extend([OP_TXINPUTCOUNT, OP_2, OP_NUMEQUALVERIFY]);
    s.extend([OP_0, OP_UTXOBYTECODE]);
    s.extend(push_bytes(electric_lock));
    s.push(OP_EQUALVERIFY);
    s.extend([OP_TXOUTPUTCOUNT, OP_3, OP_NUMEQUALVERIFY]);
    s.extend([OP_0, OP_OUTPUTBYTECODE, OP_DUP]);
    s.extend(push_bytes(&recipients[0]));
    s.extend([OP_EQUAL, OP_OVER]);
    s.extend(push_bytes(&recipients[1]));
    s.extend([OP_EQUAL, OP_BOOLOR, OP_SWAP]);
    s.extend(push_bytes(&recipients[2]));
    s.extend([OP_EQUAL, OP_BOOLOR, OP_VERIFY]);
    s.extend([OP_1, OP_OUTPUTVALUE, OP_0, OP_UTXOVALUE, OP_NUMEQUALVERIFY]);
    s.extend([
        OP_2,
        OP_OUTPUTBYTECODE,
        OP_1,
        OP_UTXOBYTECODE,
        OP_EQUALVERIFY,
    ]);
    s.extend([OP_2, OP_OUTPUTTOKENCATEGORY, OP_0, OP_EQUALVERIFY]);
    s.extend([OP_2, OP_OUTPUTVALUE, OP_1, OP_UTXOVALUE]);
    s.extend(push_number((TOKEN_OUTPUT_SATS + POT_MAX_FEE_SATS) as i64));
    s.extend([OP_SUB, OP_GREATERTHANOREQUAL, OP_VERIFY]);
    // Baton height below the locktime, or the minimum difficulty.
    s.extend([OP_0, OP_UTXOTOKENCOMMITMENT, OP_1, OP_SPLIT, OP_4, OP_SPLIT]);
    s.extend([
        OP_DROP,
        OP_BIN2NUM,
        OP_TXLOCKTIME,
        OP_LESSTHAN,
        OP_SWAP,
        OP_BIN2NUM,
    ]);
    s.extend(push_number(i64::from(min_difficulty)));
    s.extend([OP_NUMEQUAL, OP_BOOLOR, OP_ELSE]);
    // Refund.
    if let Some(key) = recovery_public_key {
        s.extend(push_bytes(&key.serialize()));
        s.push(0xad); // CHECKSIGVERIFY binds recovery to the committed owner key.
    }
    s.extend([OP_TXINPUTCOUNT, OP_1, OP_NUMEQUALVERIFY]);
    s.extend([OP_TXOUTPUTCOUNT, OP_1, OP_NUMEQUALVERIFY]);
    s.extend([OP_0, OP_OUTPUTBYTECODE]);
    s.extend(push_bytes(refund));
    s.push(OP_EQUALVERIFY);
    s.extend([OP_0, OP_OUTPUTTOKENCATEGORY, OP_0, OP_EQUALVERIFY]);
    s.extend([OP_0, OP_OUTPUTVALUE, OP_0, OP_UTXOVALUE]);
    s.extend(push_number(POT_MAX_FEE_SATS as i64));
    s.extend([OP_SUB, OP_GREATERTHANOREQUAL, OP_ENDIF]);
    s
}

/// Builds the `mine` claim: baton and pot in, reward, baton and pot change
/// out, at the relay fee for its exact size.
#[allow(clippy::too_many_arguments)]
pub fn build_claim(
    deployment: &Deployment,
    pot: &Pot,
    baton: &Baton,
    coin: &Coin,
    win: &Win,
    locktime: u32,
    relay_fee_sats_per_kb: u64,
) -> Result<Built, String> {
    if baton.token_amount < deployment.reward {
        return Err("the ELECTRIC baton holds less than one reward".into());
    }
    let next = baton.state.next(locktime, deployment)?;
    let mut baton_unlock = push_bytes(&win.nonce.to_le_bytes());
    baton_unlock.extend(push_bytes(&win.pkh));
    baton_unlock.extend(push_data(&deployment.redeem_script())?);
    let pot_unlock = push_data(&pot.redeem)?;
    let category = deployment.category_le();
    let reward_lock = p2pkh(&win.pkh);
    let mut reward_token = vec![0xef];
    reward_token.extend(category);
    reward_token.push(0x10);
    reward_token.extend(compact_uint(deployment.reward));
    let mut baton_token = vec![0xef];
    baton_token.extend(category);
    baton_token.push(0x71);
    baton_token.extend(compact_uint(16));
    baton_token.extend(next.encode());
    baton_token.extend(compact_uint(baton.token_amount - deployment.reward));
    let pot_lock = pot.locking_bytecode();

    let assemble = |change: u64| -> Result<Vec<u8>, String> {
        let mut tx = 2u32.to_le_bytes().to_vec();
        tx.extend(compact_uint(2));
        tx.extend(input(
            &baton.txid,
            baton.vout,
            &baton_unlock,
            CLAIM_SEQUENCE,
        )?);
        tx.extend(input(&coin.txid, coin.vout, &pot_unlock, CLAIM_SEQUENCE)?);
        tx.extend(compact_uint(3));
        tx.extend(output(TOKEN_OUTPUT_SATS, &reward_token, &reward_lock));
        tx.extend(output(
            baton.value,
            &baton_token,
            &deployment.locking_bytecode(),
        ));
        tx.extend(output(change, &[], &pot_lock));
        tx.extend(locktime.to_le_bytes());
        Ok(tx)
    };
    let fee = relay_fee(assemble(0)?.len(), relay_fee_sats_per_kb);
    if fee > POT_MAX_FEE_SATS {
        return Err(format!(
            "the relay fee ({fee} sats) is above what the pot pays ({POT_MAX_FEE_SATS})"
        ));
    }
    let change = coin
        .value
        .checked_sub(TOKEN_OUTPUT_SATS + fee)
        .filter(|change| *change >= POT_MIN_CHANGE_SATS)
        .ok_or("the pot cannot cover this claim; send it more BCH")?;
    let raw = assemble(change)?;
    Ok(Built {
        txid: transaction_id(&raw),
        raw,
        fee,
    })
}

/// Returns one pot coin to the payout address, signed by its recovery key.
/// Legacy pots need no signature. Both versions retain the payout and fee cap.
pub fn build_refund(
    pot: &Pot,
    coin: &Coin,
    refund_lock: &[u8],
    relay_fee_sats_per_kb: u64,
    recovery_secret: Option<&secp256k1::SecretKey>,
) -> Result<Built, String> {
    match (pot.recovery_public_key, recovery_secret) {
        (Some(public), Some(secret)) if secp256k1::PublicKey::from_secret_key(secret) == public => {
        }
        (None, None) => {}
        _ => return Err("the pot recovery key is missing or does not match".into()),
    }
    let assemble = |value: u64, signature: Option<&[u8]>| -> Result<Vec<u8>, String> {
        let mut unlock = signature.map(push_data).transpose()?.unwrap_or_default();
        unlock.extend(push_data(&pot.redeem)?);
        let mut tx = 2u32.to_le_bytes().to_vec();
        tx.extend(compact_uint(1));
        tx.extend(input(&coin.txid, coin.vout, &unlock, 0xffff_ffff)?);
        tx.extend(compact_uint(1));
        tx.extend(output(value, &[], refund_lock));
        tx.extend(0u32.to_le_bytes());
        Ok(tx)
    };
    let placeholder = recovery_secret.map(|_| [0u8; 65]);
    let fee = relay_fee(
        assemble(0, placeholder.as_ref().map(|s| &s[..]))?.len(),
        relay_fee_sats_per_kb,
    );
    if fee > POT_MAX_FEE_SATS {
        return Err("the relay fee is above the pot refund cap".into());
    }
    let value = coin
        .value
        .checked_sub(fee)
        .filter(|value| *value >= 546)
        .ok_or("this pot coin is too small to refund")?;
    let signature = recovery_secret
        .map(|secret| {
            let mut outpoint = parse_txid_display(&coin.txid)?.to_vec();
            outpoint.reverse();
            outpoint.extend(coin.vout.to_le_bytes());
            let mut preimage = 2u32.to_le_bytes().to_vec();
            preimage.extend(hash256(&outpoint));
            preimage.extend(hash256(&u32::MAX.to_le_bytes()));
            preimage.extend(outpoint);
            preimage.extend(compact_uint(pot.redeem.len() as u64));
            preimage.extend(&pot.redeem);
            preimage.extend(coin.value.to_le_bytes());
            preimage.extend(u32::MAX.to_le_bytes());
            preimage.extend(hash256(&output(value, &[], refund_lock)));
            preimage.extend(0u32.to_le_bytes());
            preimage.extend(0x41u32.to_le_bytes()); // SIGHASH_ALL | FORKID.
            let mut sig =
                crate::crypto::bch_schnorr_sign(&secret.to_secret_bytes(), &hash256(&preimage))?
                    .to_vec();
            sig.push(0x41);
            Ok::<_, String>(sig)
        })
        .transpose()?;
    let raw = assemble(value, signature.as_deref())?;
    Ok(Built {
        txid: transaction_id(&raw),
        raw,
        fee,
    })
}

/// The most a P2PKH signature and public key add to an unsigned input.
const P2PKH_UNLOCK_BYTES: usize = 107;

/// Builds the genesis of a new deployment of the contract (for Chipnet,
/// where none is published): the baton with the whole `supply` at the
/// covenant, at the minimum difficulty and `height`, and the change. The
/// funding coin is output 0 of its transaction, so its txid becomes the
/// category. The input is left unsigned for the coin's wallet to sign, so
/// the returned txid changes once it is signed.
pub fn build_genesis(
    deployment: &Deployment,
    funding: &Coin,
    change_lock: &[u8],
    supply: u64,
    height: u32,
    relay_fee_sats_per_kb: u64,
) -> Result<Built, String> {
    if funding.vout != 0 || parse_txid_display(&funding.txid)? != deployment.category {
        return Err(
            "the funding coin must be output 0 of the transaction named by the category".into(),
        );
    }
    if supply < deployment.reward || supply > i64::MAX as u64 {
        return Err("the supply must hold at least one reward".into());
    }
    let state = State {
        difficulty: deployment.min_difficulty,
        last_height: height,
        mine_count: 0,
    };
    let mut baton_token = vec![0xef];
    baton_token.extend(deployment.category_le());
    baton_token.push(0x71);
    baton_token.extend(compact_uint(16));
    baton_token.extend(state.encode());
    baton_token.extend(compact_uint(supply));
    let assemble = |change: u64| -> Result<Vec<u8>, String> {
        let mut tx = 2u32.to_le_bytes().to_vec();
        tx.extend(compact_uint(1));
        tx.extend(input(&funding.txid, 0, &[], 0xffff_ffff)?);
        tx.extend(compact_uint(2));
        tx.extend(output(
            TOKEN_OUTPUT_SATS,
            &baton_token,
            &deployment.locking_bytecode(),
        ));
        tx.extend(output(change, &[], change_lock));
        tx.extend(0u32.to_le_bytes());
        Ok(tx)
    };
    let fee = relay_fee(
        assemble(0)?.len() + P2PKH_UNLOCK_BYTES,
        relay_fee_sats_per_kb,
    );
    let change = funding
        .value
        .checked_sub(TOKEN_OUTPUT_SATS + fee)
        .filter(|change| *change >= 546)
        .ok_or("the funding coin cannot pay the baton's 1,000 sats and the fee")?;
    let raw = assemble(change)?;
    Ok(Built {
        txid: transaction_id(&raw),
        raw,
        fee,
    })
}

/// The relay fee for `size` bytes, rounded up.
fn relay_fee(size: usize, sats_per_kb: u64) -> u64 {
    (size as u64)
        .saturating_mul(sats_per_kb.max(1_000))
        .div_ceil(1_000)
}

fn input(txid: &str, vout: u32, unlocking: &[u8], sequence: u32) -> Result<Vec<u8>, String> {
    let mut out = parse_txid_display(txid)?.to_vec();
    out.reverse();
    out.extend(vout.to_le_bytes());
    out.extend(compact_uint(unlocking.len() as u64));
    out.extend_from_slice(unlocking);
    out.extend(sequence.to_le_bytes());
    Ok(out)
}

fn output(value: u64, token: &[u8], locking: &[u8]) -> Vec<u8> {
    let mut out = value.to_le_bytes().to_vec();
    out.extend(compact_uint((token.len() + locking.len()) as u64));
    out.extend_from_slice(token);
    out.extend_from_slice(locking);
    out
}

pub fn p2pkh(pkh: &[u8; 20]) -> Vec<u8> {
    let mut out = vec![0x76, 0xa9, 0x14];
    out.extend_from_slice(pkh);
    out.extend([0x88, 0xac]);
    out
}

fn p2sh32(redeem: &[u8]) -> Vec<u8> {
    let mut out = vec![0xaa, 0x20];
    out.extend(hash256(redeem));
    out.push(0x87);
    out
}

/// A minimal push, as the VM's standard rules want: OP_0, OP_1..OP_16 and
/// OP_1NEGATE for the one-byte values they stand for.
fn push_bytes(data: &[u8]) -> Vec<u8> {
    match data {
        [] => vec![0x00],
        [n @ 1..=16] => vec![0x50 + n],
        [0x81] => vec![0x4f],
        _ => push_data(data).expect("script data under 64 KiB"),
    }
}

/// A minimally encoded script number, pushed.
fn push_number(value: i64) -> Vec<u8> {
    let mut magnitude = value.unsigned_abs();
    let mut bytes = Vec::new();
    while magnitude > 0 {
        bytes.push((magnitude & 0xff) as u8);
        magnitude >>= 8;
    }
    if let Some(last) = bytes.last_mut() {
        if *last & 0x80 != 0 {
            bytes.push(if value < 0 { 0x80 } else { 0 });
        } else if value < 0 {
            *last |= 0x80;
        }
    }
    push_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINER: [u8; 20] = [0x11; 20];

    fn recipients(network: MiningNetwork) -> [Vec<u8>; 3] {
        let policy = Deployment::mainnet().donation_policy(None);
        let miner = crate::tx::p2pkh_hash_to_cashaddr_for_network(&MINER, network).unwrap();
        policy
            .payouts(network, &miner)
            .unwrap()
            .map(|address| crate::tx::cashaddr_to_p2pkh_locking(&address).unwrap())
    }

    fn pkh(lock: &[u8]) -> [u8; 20] {
        lock[3..23].try_into().unwrap()
    }

    #[test]
    fn the_published_deployment_is_the_covenant_holding_the_baton() {
        let mainnet = Deployment::mainnet();
        assert_eq!(
            hex::encode(mainnet.locking_bytecode()),
            MAINNET_COVENANT_LOCK_HEX
        );
        assert_eq!(mainnet.redeem_script().len(), 237);
        assert_eq!(
            script_hash_hex(&mainnet.locking_bytecode()),
            "77f4978548da75ea5aa1efff64082f66c62ae0416483b4198bcef515bd1dc4b6"
        );
        assert_eq!(
            Deployment::select(MiningNetwork::Mainnet, None, None, None, None).unwrap(),
            mainnet
        );
        assert_eq!(
            Deployment::select(MiningNetwork::Chipnet, None, None, None, None).unwrap(),
            Deployment::chipnet()
        );
        assert_eq!(
            hex::encode(Deployment::chipnet().locking_bytecode()),
            "aa207e5c85e853901c250eba8a0e4786af7ef2d9e022159832296f9572ad9bb1017187"
        );
        assert!(Deployment::select(
            MiningNetwork::Mainnet,
            Some(MAINNET_CATEGORY_HEX),
            Some(1),
            None,
            None
        )
        .is_err());
        let chipnet = Deployment::select(
            MiningNetwork::Chipnet,
            Some(&"ab".repeat(32)),
            None,
            Some(2),
            None,
        )
        .unwrap();
        assert_eq!((chipnet.min_difficulty, chipnet.max_difficulty), (2, 4));
        assert_ne!(chipnet.locking_bytecode(), mainnet.locking_bytecode());
        assert!(Deployment::select(MiningNetwork::Chipnet, Some("ab"), None, None, None).is_err());
        assert!(Deployment::select(
            MiningNetwork::Chipnet,
            Some(&"ab".repeat(32)),
            None,
            Some(5),
            Some(4)
        )
        .is_err());
    }

    /// Two claims accepted on mainnet at height 972909: the one that moved
    /// the baton to height 972908 (4 zero bytes), then the cheap claim after
    /// it (3 zero bytes).
    #[test]
    fn proofs_and_states_match_claims_accepted_on_mainnet() {
        let deployment = Deployment::mainnet();
        let pkh: [u8; 20] = hex::decode("69f20c110fc6878d95ae284291e8071343551d6b")
            .unwrap()
            .try_into()
            .unwrap();
        for (commitment, nonce, locktime, next, zeros, hash) in [
            (
                "042cd80e008205000000000000000000",
                "3e7c660700000000",
                972_908,
                "036cd80e008305000000000000000000",
                4,
                "00000000efc5cd4062dd50491276523f08db8d131e621f5ea3eab33bfd9adb67",
            ),
            (
                "036cd80e008305000000000000000000",
                "cdafa30100000000",
                972_908,
                "046cd80e008405000000000000000000",
                3,
                "000000688765d5bab1a960d5e56014315d0ddf6461e822a17171493083a31d30",
            ),
        ] {
            let state = State::decode(&hex::decode(commitment).unwrap()).unwrap();
            assert_eq!(hex::encode(state.encode()), commitment);
            let nonce = u64::from_le_bytes(hex::decode(nonce).unwrap().try_into().unwrap());
            let proof = hash256(&preimage(state.last_height, &pkh, nonce));
            assert_eq!(hex::encode(proof), hash);
            assert!(meets(&proof, state.difficulty));
            assert!(meets(&proof, zeros) && !meets(&proof, zeros + 1));
            assert_eq!(
                search(state.last_height, &pkh, zeros, nonce, 1),
                Some(nonce)
            );
            assert_eq!(search(state.last_height, &pkh, zeros + 1, nonce, 1), None);
            let after = state.next(locktime, &deployment).unwrap();
            assert_eq!(hex::encode(after.encode()), next);
            assert_eq!(claim_locktime(state, 972_908, &deployment), Some(972_908));
        }
    }

    #[test]
    fn the_pot_pays_only_claims_that_move_the_height_or_the_cheap_claim_after() {
        let deployment = Deployment::mainnet();
        let at = |difficulty, last_height| State {
            difficulty,
            last_height,
            mine_count: 0,
        };
        assert_eq!(claim_locktime(at(4, 100), 101, &deployment), Some(101));
        assert_eq!(claim_locktime(at(3, 100), 100, &deployment), Some(100));
        assert_eq!(claim_locktime(at(4, 100), 100, &deployment), None);
        assert_eq!(claim_locktime(at(3, 100), 99, &deployment), None);
        assert!(at(3, 100).next(99, &deployment).is_err());
        assert_eq!(at(4, 100).next(100, &deployment).unwrap().difficulty, 4);
    }

    #[test]
    fn search_finds_the_same_proofs_as_hashing_each_nonce() {
        for difficulty in 0..=2u8 {
            let mut start = 0x1234_5678_9abc_def0;
            for _ in 0..4 {
                let nonce = search(972_908, &MINER, difficulty, start, 1 << 20).unwrap();
                for earlier in start..nonce {
                    assert!(!meets(
                        &hash256(&preimage(972_908, &MINER, earlier)),
                        difficulty
                    ));
                }
                assert!(meets(
                    &hash256(&preimage(972_908, &MINER, nonce)),
                    difficulty
                ));
                start = nonce.wrapping_add(1);
            }
        }
        assert_eq!(search(1, &MINER, 33, 0, 1_000), None);
    }

    #[test]
    fn parallel_search_follows_the_donation_schedule() {
        let policy = Deployment::mainnet().donation_policy(None);
        let schedule =
            Mutex::new(Schedule::new(policy.scheme, u64::from(SEARCH_BATCH), 0).unwrap());
        let pkhs = [[1u8; 20], [2u8; 20], [3u8; 20]];
        let job = SearchJob {
            last_height: 7,
            difficulty: 2,
            pkhs,
        };
        let (next, stop, hashes) = (AtomicU64::new(0), AtomicBool::new(false), AtomicU64::new(0));
        let win = parallel_search(job, &schedule, 3, &next, &stop, &hashes).unwrap();
        assert_eq!(win.pkh, pkhs[win.recipient as usize]);
        assert!(meets(&hash256(&preimage(7, &win.pkh, win.nonce)), 2));
        assert!(hashes.load(Ordering::Relaxed) > 0);
        // A stopped search returns at once.
        let stopped = AtomicBool::new(true);
        assert_eq!(
            parallel_search(job, &schedule, 2, &next, &stopped, &hashes),
            None
        );
    }

    #[test]
    fn donation_is_four_percent_of_the_work_with_extra_to_the_project() {
        let mainnet = Deployment::mainnet();
        assert_eq!(
            mainnet.donation_policy(None).scheme,
            Scheme::Work([200, 200])
        );
        assert_eq!(
            mainnet
                .donation_policy(Some(TokenDonation::from_bps(150)))
                .scheme,
            Scheme::Work([200, 200])
        );
        assert_eq!(
            mainnet
                .donation_policy(Some(TokenDonation::from_bps(550)))
                .scheme,
            Scheme::Work([350, 200])
        );
        let chipnet = Deployment::select(
            MiningNetwork::Chipnet,
            Some(&"cd".repeat(32)),
            None,
            None,
            None,
        )
        .unwrap();
        let policy = chipnet.donation_policy(None);
        assert_eq!(policy.scheme, Scheme::Work([400, 0]));
        assert_eq!(policy.addresses, [CHIPNET_DONATION_ADDRESS; 2]);
    }

    #[test]
    fn genesis_puts_the_whole_supply_on_a_minimum_difficulty_baton() {
        let funding_txid = "39629630f3f5ba91c6e656e7b79c2c8de2fc6f776bfe085e1b7eaf70aa8a5e76";
        let deployment =
            Deployment::select(MiningNetwork::Chipnet, Some(funding_txid), None, None, None)
                .unwrap();
        let coin = |vout, value| Coin {
            txid: funding_txid.into(),
            vout,
            value,
        };
        let change = p2pkh(&MINER);
        let supply = deployment.reward * 1_000_000;
        let genesis = build_genesis(
            &deployment,
            &coin(0, 10_000),
            &change,
            supply,
            328_450,
            1_000,
        )
        .unwrap();
        let raw = &genesis.raw;
        // One unsigned input spending output 0 of the category's transaction.
        assert_eq!(raw[4], 1);
        assert_eq!(&raw[5..37], &deployment.category_le());
        assert_eq!(&raw[37..41], &[0; 4]);
        assert_eq!(raw[41], 0);
        // The baton: 1,000 sats, a mutable NFT with the starting state and
        // the whole supply, at the covenant.
        let mut token = vec![0xef];
        token.extend(deployment.category_le());
        token.push(0x71);
        token.push(16);
        token.extend(
            State {
                difficulty: deployment.min_difficulty,
                last_height: 328_450,
                mine_count: 0,
            }
            .encode(),
        );
        token.extend(compact_uint(supply));
        let baton = output(TOKEN_OUTPUT_SATS, &token, &deployment.locking_bytecode());
        assert_eq!(&raw[47..47 + baton.len()], &baton[..]);
        // The change pays the fee for the signed size.
        let rest = &raw[47 + baton.len()..];
        let value = u64::from_le_bytes(rest[..8].try_into().unwrap());
        assert_eq!(value, 10_000 - TOKEN_OUTPUT_SATS - genesis.fee);
        assert_eq!(
            genesis.fee,
            relay_fee(raw.len() + P2PKH_UNLOCK_BYTES, 1_000)
        );
        assert_eq!(&rest[9..34], &change[..]);
        assert_eq!(&rest[34..], &[0; 4]);
        // Output 0 only, enough for the baton and fee, at least one reward.
        assert!(build_genesis(&deployment, &coin(1, 10_000), &change, supply, 1, 1_000).is_err());
        assert!(build_genesis(&deployment, &coin(0, 1_500), &change, supply, 1, 1_000).is_err());
        assert!(build_genesis(&deployment, &coin(0, 10_000), &change, 1, 1, 1_000).is_err());
        let other = Deployment::select(
            MiningNetwork::Chipnet,
            Some(&"ab".repeat(32)),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(build_genesis(&other, &coin(0, 10_000), &change, supply, 1, 1_000).is_err());
    }

    #[test]
    fn claims_and_refunds_have_the_contract_shape_and_exact_fees() {
        let deployment = Deployment::mainnet();
        let locks = recipients(MiningNetwork::Mainnet);
        let pot = Pot::new(&deployment, &locks).unwrap();
        assert!(pot.address().starts_with("bitcoincash:p"));
        assert_eq!(
            crate::tx::cashaddr_to_coinbase_locking(&pot.address()).unwrap(),
            pot.locking_bytecode()
        );
        let baton = Baton {
            txid: "835dfb0c5b08de7a81c7345e7e1440605ff0b53d49475a4cba48e1412f23ee18".into(),
            vout: 1,
            value: 1_800,
            token_amount: 599_915_280_000_000,
            state: State::decode(&hex::decode("046cd80e008405000000000000000000").unwrap())
                .unwrap(),
        };
        let coin = Coin {
            txid: "ab".repeat(32),
            vout: 0,
            value: 50_000,
        };
        let win = Win {
            recipient: Recipient::Miner,
            pkh: pkh(&locks[0]),
            nonce: 9,
        };
        let claim = build_claim(&deployment, &pot, &baton, &coin, &win, 972_986, 1_000).unwrap();
        assert_eq!(claim.fee, claim.raw.len() as u64);
        assert!(claim.raw.len() < 1_000, "{}", claim.raw.len());
        let change = u64::from_le_bytes(
            claim.raw[claim.raw.len() - 4 - 44..claim.raw.len() - 4 - 36]
                .try_into()
                .unwrap(),
        );
        assert_eq!(change, 50_000 - TOKEN_OUTPUT_SATS - claim.fee);
        assert_eq!(&claim.raw[claim.raw.len() - 4..], &972_986u32.to_le_bytes());
        // An empty pot or a fee above the pot's cap is refused before broadcast.
        let small = Coin {
            value: 2_000,
            ..coin.clone()
        };
        assert!(build_claim(&deployment, &pot, &baton, &small, &win, 972_986, 1_000).is_err());
        assert!(build_claim(&deployment, &pot, &baton, &coin, &win, 972_986, 10_000).is_err());
        assert!(build_claim(&deployment, &pot, &baton, &coin, &win, 972_000, 1_000).is_err());
        let refund = build_refund(&pot, &coin, &locks[0], 1_000, None).unwrap();
        assert_eq!(refund.fee, refund.raw.len() as u64);
        assert!(build_refund(&pot, &Coin { value: 600, ..coin }, &locks[0], 1_000, None).is_err());
        assert!(Pot::new(
            &deployment,
            &[vec![0; 25], locks[1].clone(), locks[2].clone()]
        )
        .is_err());
    }

    #[test]
    fn recovery_requires_the_matching_key_and_keeps_exact_fees() {
        let deployment = Deployment::mainnet();
        let locks = recipients(MiningNetwork::Mainnet);
        let owner = secp256k1::SecretKey::from_secret_bytes([0x31; 32]).unwrap();
        let other = secp256k1::SecretKey::from_secret_bytes([0x32; 32]).unwrap();
        let pot = Pot::with_recovery(
            &deployment,
            &locks,
            Some(secp256k1::PublicKey::from_secret_key(&owner)),
        )
        .unwrap();
        let legacy = Pot::new(&deployment, &locks).unwrap();
        assert_ne!(pot.address(), legacy.address());
        let coin = Coin {
            txid: "ab".repeat(32),
            vout: 3,
            value: 50_000,
        };
        assert!(build_refund(&pot, &coin, &locks[0], 1_000, None).is_err());
        assert!(build_refund(&pot, &coin, &locks[0], 1_000, Some(&other)).is_err());
        for rate in [1_000, 1_100, 2_500] {
            let refund = build_refund(&pot, &coin, &locks[0], rate, Some(&owner)).unwrap();
            assert_eq!(refund.fee, (refund.raw.len() as u64 * rate).div_ceil(1_000));
            assert_eq!(refund.txid, transaction_id(&refund.raw));
        }
        assert!(build_refund(&pot, &coin, &locks[0], 100_000, Some(&owner)).is_err());
        assert!(build_refund(&legacy, &coin, &locks[0], u64::MAX, None).is_err());
    }

    #[test]
    fn script_numbers_are_minimal() {
        assert_eq!(push_number(0), vec![0x00]);
        assert_eq!(push_number(4), vec![0x54]);
        assert_eq!(push_number(-1), vec![0x4f]);
        assert_eq!(push_number(128), vec![0x02, 0x80, 0x00]);
        assert_eq!(push_number(4_000), vec![0x02, 0xa0, 0x0f]);
        assert_eq!(push_number(60_000_000), vec![0x04, 0x00, 0x87, 0x93, 0x03]);
        assert_eq!(push_number(-129), vec![0x02, 0x81, 0x80]);
    }

    /// #### PR #51: transactions for `tools/reward-policy-vm/electric.mjs`,
    /// written when PICKAXE_ELECTRIC_PROOF names a file. A test deployment at
    /// 1-2 bytes keeps the search short; the VM runs the real contract body.
    #[test]
    fn vm_proof_vectors() {
        let Some(path) = std::env::var_os("PICKAXE_ELECTRIC_PROOF") else {
            return;
        };
        write_vm_vectors(&std::path::PathBuf::from(path), search);
    }

    pub(super) fn write_vm_vectors(
        path: &std::path::Path,
        mut search: impl FnMut(u32, &[u8; 20], u8, u64, u64) -> Option<u64>,
    ) {
        let deployment = Deployment::select(
            MiningNetwork::Mainnet,
            Some(&"5a".repeat(32)),
            None,
            Some(1),
            Some(2),
        )
        .unwrap();
        let locks = recipients(MiningNetwork::Mainnet);
        let coin = Coin {
            txid: "cd".repeat(32),
            vout: 2,
            value: 20_000,
        };
        let mut samples = Vec::new();
        for owner in [
            None,
            Some(secp256k1::SecretKey::from_secret_bytes([0x31; 32]).unwrap()),
        ] {
            let pot = Pot::with_recovery(
                &deployment,
                &locks,
                owner.as_ref().map(secp256k1::PublicKey::from_secret_key),
            )
            .unwrap();
            let mut sample = |name: &str, expect: &str, baton: &Baton, built: &Built| {
                samples.push(serde_json::json!({
                    "name": format!("{}: {name}", if owner.is_some() { "owner" } else { "legacy" }),
                    "pot_lock": hex::encode(pot.locking_bytecode()),
                    "owner": owner.is_some(),
                    "expect": expect,
                    "raw": hex::encode(&built.raw),
                    "fee": built.fee,
                    "baton": {
                        "value": baton.value,
                        "amount": baton.token_amount.to_string(),
                        "commitment": hex::encode(baton.state.encode()),
                    },
                    "pot_value": coin.value,
                }));
            };
            let mut find = |state: State, pkh: [u8; 20], recipient: usize| {
                let nonce = search(state.last_height, &pkh, state.difficulty, 0, 1 << 24).unwrap();
                Win {
                    recipient: Recipient::ALL[recipient],
                    pkh,
                    nonce,
                }
            };
            let baton_at = |difficulty, last_height| Baton {
                txid: "ef".repeat(32),
                vout: 1,
                value: 1_800,
                token_amount: 600_000_000_000_000,
                state: State {
                    difficulty,
                    last_height,
                    mine_count: 41,
                },
            };
            for (recipient, lock) in locks.iter().enumerate() {
                let baton = baton_at(2, 1_000);
                let win = find(baton.state, pkh(lock), recipient);
                let built =
                    build_claim(&deployment, &pot, &baton, &coin, &win, 1_001, 1_000).unwrap();
                sample(
                    &format!("new height to recipient {recipient}"),
                    "valid",
                    &baton,
                    &built,
                );
            }
            let cheap = baton_at(1, 1_000);
            let win = find(cheap.state, pkh(&locks[0]), 0);
            let built = build_claim(&deployment, &pot, &cheap, &coin, &win, 1_000, 1_000).unwrap();
            sample(
                "minimum difficulty at the same height",
                "valid",
                &cheap,
                &built,
            );
            // The contract accepts these; the pot does not pay for them.
            let replay = baton_at(2, 1_000);
            let win = find(replay.state, pkh(&locks[0]), 0);
            let built = build_claim(&deployment, &pot, &replay, &coin, &win, 1_000, 1_000).unwrap();
            sample("same height above the minimum", "pot", &replay, &built);
            let stranger = find(
                State {
                    difficulty: 2,
                    last_height: 1_000,
                    mine_count: 41,
                },
                [0x77; 20],
                0,
            );
            let baton = baton_at(2, 1_000);
            let built =
                build_claim(&deployment, &pot, &baton, &coin, &stranger, 1_001, 1_000).unwrap();
            sample("another recipient", "pot", &baton, &built);
            let win = find(baton.state, pkh(&locks[0]), 0);
            let valid = build_claim(&deployment, &pot, &baton, &coin, &win, 1_001, 1_000).unwrap();
            let mut greedy = valid.clone();
            let change_at = greedy.raw.len() - 4 - 44;
            let change =
                u64::from_le_bytes(greedy.raw[change_at..change_at + 8].try_into().unwrap());
            greedy.raw[change_at..change_at + 8]
                .copy_from_slice(&(change - POT_MAX_FEE_SATS).to_le_bytes());
            sample("fee above the cap", "pot", &baton, &greedy);
            let refund = build_refund(&pot, &coin, &locks[0], 1_000, owner.as_ref()).unwrap();
            sample("refund", "valid", &baton, &refund);
            let elsewhere = build_refund(&pot, &coin, &locks[1], 1_000, owner.as_ref()).unwrap();
            sample("refund to a donation address", "pot", &baton, &elsewhere);
            if owner.is_some() {
                let mut unsigned = pot.clone();
                unsigned.recovery_public_key = None;
                let built = build_refund(&unsigned, &coin, &locks[0], 1_000, None).unwrap();
                sample("refund without a signature", "pot", &baton, &built);
                let attacker = secp256k1::SecretKey::from_secret_bytes([0x32; 32]).unwrap();
                let mut forged = pot.clone();
                forged.recovery_public_key = Some(secp256k1::PublicKey::from_secret_key(&attacker));
                let built =
                    build_refund(&forged, &coin, &locks[0], 1_000, Some(&attacker)).unwrap();
                sample("refund signed by another key", "pot", &baton, &built);
            }
        }
        let proof = serde_json::json!({
            "electric_lock": hex::encode(deployment.locking_bytecode()),
            "electric_redeem": hex::encode(deployment.redeem_script()),
            "category": deployment.category_hex(),
            "reward": deployment.reward,
            "min_difficulty": deployment.min_difficulty,
            "max_difficulty": deployment.max_difficulty,
            "pot_max_fee": POT_MAX_FEE_SATS,
            "mainnet_lock": hex::encode(Deployment::mainnet().locking_bytecode()),
            "samples": samples,
        });
        std::fs::write(path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
    }
}
