//! #### PR #38
//! Bounded native SV2 server. One worker owns the node/template and submissions;
//! device connections only see immutable jobs and cannot select chain payouts.

use super::{
    channel::TokenWin,
    jd::{
        client::{JdClientSummary, UplinkEvent, UplinkHandle},
        server::{Context, Declarator, DeclaratorSession},
    },
    journal::{Journal, PendingBlock},
    merge::hub::TokenHub,
    provider::{NodeRpc, SourceKind, SubmissionOutcome, TemplateProvider, TemplateSource},
    tdp,
    telemetry::Devices,
    template::{BchTemplate, Hash},
    transport::{Limits, Receiver, Sender, Session},
    wire::MiningSession,
    work_allocation::WorkAllocation,
};
use crate::config::{validate_payout_address, MiningNetwork};
use crate::donation::bch::{BchDonation, BchPayout};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stratum_core::bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey};

const MAX_CONNECTIONS: usize = 64;
const TEMPLATE_RECOVERY_GRACE: Duration = Duration::from_secs(3);

// No Debug: contains a local authority secret and payout configuration.
pub struct ServerConfig {
    pub network: MiningNetwork,
    pub payout: String,
    pub authority_secret: [u8; 32],
    pub share_target: Hash,
    pub journal_path: PathBuf,
    /// #### PR #40: the configured nodes' identities, so a journal written
    /// when journals were bound to one node still opens (and is rebound).
    pub legacy_sources: Vec<[u8; 32]>,
    pub donation: Arc<RwLock<BchDonation>>,
    /// #### PR #40: a public pool, where each miner's blocks pay them.
    pub public: Option<super::payout::PublicPool>,
    /// #### PR #40: the pool's name, written into every block's coinbase
    /// (empty for none).
    pub pool_tag: Vec<u8>,
    /// #### PR #42: the merge-mined tokens jobs carry, and where their wins
    /// are proven; none unless a token is merge-mined.
    pub tokens: Option<Arc<TokenHub>>,
    /// #### PR #42: where blocks found on this server's templates by
    /// Template Distribution clients are saved until the node answers.
    pub relay_journal_path: Option<PathBuf>,
    /// #### PR #42: a public pool's Job Declaration rules, when it accepts
    /// miners' own templates.
    pub declarator: Option<Arc<Declarator>>,
    /// #### PR #42: where a pool that accepts Full-Template Job Declaration
    /// saves the blocks found on miners' declared templates until its node
    /// answers.
    pub declared_journal_path: Option<PathBuf>,
    /// #### PR #42: a Job Declaration client's uplink, when this server is
    /// the local server that mines its own templates at a pool.
    pub uplink: Option<UplinkHandle>,
    #[cfg(test)]
    pub allocation_phase: Option<u64>,
}

#[derive(Default, Clone, Debug)]
pub struct ServerStats {
    pub connections: usize,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub blocks_accepted: u64,
    pub blocks_pending: usize,
    pub blocks_rejected: u64,
    pub block_retries: u64,
    pub last_block_result: Option<&'static str>,
    pub connection_errors: u64,
    pub sv1_connection_errors: u64,
    pub sv1_local_rejected: u64,
    pub sessions_started: u64,
    pub device_stats: Devices,
    pub template_ready: bool,
    pub template_failures: u64,
    pub last_template_error: Option<&'static str>,
    pub height: Option<u32>,
    /// #### PR #40: the node templates come from (0 is the first in failover
    /// order), how many nodes there are, and how often the server moved on.
    pub active_node: usize,
    pub nodes: usize,
    pub node_switches: u64,
    /// #### PR #40: the highest share difficulty since start, and its worker.
    pub best_share: Option<(f64, String)>,
    /// #### PR #40: the latest blocks found, newest last.
    pub recent_blocks: VecDeque<FoundBlock>,
    /// #### PR #42: merge-mined token wins handed to the claim worker, those
    /// dropped because its queue was full, the latest proven (newest last),
    /// and why token claims are off, if they are.
    pub token_wins: u64,
    pub token_wins_dropped: u64,
    pub recent_token_wins: VecDeque<FoundTokenWin>,
    pub tokens_off: Option<String>,
    /// #### PR #42: the Template Distribution server's counts, when it
    /// serves templates or still holds relayed blocks.
    pub template_server: Option<TemplateServerStats>,
    /// #### PR #42: the Job Declaration server's counts, when the pool
    /// accepts miners' own templates.
    pub jd_server: Option<JdServerStats>,
    /// #### PR #42: the Job Declaration client's state and counts.
    pub jd_client: Option<JdClientSummary>,
    /// #### PR #42: what kind of source the active one is (`active_node` is
    /// its place in failover order).
    pub template_source: Option<SourceKind>,
}

/// #### PR #42: what the Job Declaration server did. No count names a
/// client, an identity, a token or an address.
#[derive(Default, Clone, Debug)]
pub struct JdServerStats {
    /// Job Declaration sessions open now.
    pub clients: usize,
    pub tokens: u64,
    pub custom_jobs: u64,
    pub refused: u64,
    pub last_refusal: Option<&'static str>,
    /// Blocks found on custom jobs: a Coinbase-only client's node submits
    /// them; a Full-Template client's the pool's node gets too.
    pub blocks: u64,
    /// Full-Template: declarations accepted, rounds of missing transactions
    /// asked for, node checks made and what makes them, solutions pushed and
    /// those no declared job matched, and the JD journal's counts.
    pub declared: u64,
    pub missing_rounds: u64,
    pub validations: u64,
    pub validator: Option<&'static str>,
    pub pushed: u64,
    pub push_unmatched: u64,
    pub declared_pending: usize,
    pub declared_accepted: u64,
    pub declared_rejected: u64,
}

/// #### PR #42: what the Template Distribution server did. No count names a
/// client or an address.
#[derive(Default, Clone, Debug)]
pub struct TemplateServerStats {
    /// Clients connected now.
    pub clients: usize,
    pub sent: u64,
    /// Templates a client's coinbase reserve did not fit.
    pub withheld: u64,
    pub solutions: u64,
    /// Solutions with no block to send: an unknown template or a malformed
    /// coinbase.
    pub invalid: u64,
    /// Solutions Pickaxe's own checks refused; one per 10 s still goes to
    /// the node.
    pub refused_locally: u64,
    /// Solutions the relay journal could not save; each still goes to the
    /// node once.
    pub unsaved: u64,
    pub relay_pending: usize,
    pub relay_accepted: u64,
    pub relay_rejected: u64,
    /// Blocks sent to the node once, outside the journal, and those it
    /// accepted.
    pub once_sent: u64,
    pub once_accepted: u64,
}

/// #### PR #42: a proven merge-mined token win.
#[derive(Clone, Debug)]
pub struct FoundTokenWin {
    pub token: &'static str,
    /// 'A' (the share met the token's target) or 'B' (a found block).
    pub mode: char,
    pub height: u32,
    pub worker: String,
    pub found: Instant,
}

/// #### PR #40
/// A block a device found: when, by which worker, and the node's answer once
/// it has one.
#[derive(Clone, Debug)]
pub struct FoundBlock {
    pub height: u32,
    pub hash: String,
    pub worker: String,
    pub found: Instant,
    pub result: Option<&'static str>,
}

/// How many found blocks the dashboard keeps.
const RECENT_BLOCKS: usize = 10;

#[derive(Clone)]
pub(super) struct PublishedJob {
    pub(super) generation: u64,
    /// #### PR #42: the token set's serial; a change re-issues jobs on the
    /// same parent.
    serial: u64,
    pub(super) template: Arc<BchTemplate>,
    valid_until: Instant,
}

pub(super) struct Shared {
    pub(super) job: RwLock<Option<PublishedJob>>,
    stats: Arc<Mutex<ServerStats>>,
    pub(super) wake: SyncSender<()>,
    journal: Mutex<Journal>,
    fatal: Mutex<Option<&'static str>>,
    pub(super) stop: Arc<AtomicBool>,
    solved: Mutex<SolvedParents>,
    /// #### PR #42: the claim worker's queue, bounded; device threads never
    /// wait on it.
    claims: Option<SyncSender<(TokenWin, String)>>,
    /// #### PR #42: what Template Distribution sessions share with the node
    /// worker, when templates are served or relayed blocks wait.
    pub(super) relay: Option<Relay>,
    /// #### PR #42: the lanes a Job Declaration client's local channels take.
    lanes: Arc<AtomicU32>,
    /// #### PR #42: blocks found on Full-Template declared jobs.
    declared: Option<DeclaredBlocks>,
    /// #### PR #42: the latest published templates with different
    /// transactions, newest first, whose transactions a declaration may name.
    recent: Option<Mutex<VecDeque<Arc<BchTemplate>>>>,
}

/// #### PR #42: the JD journal and one block per parent.
pub(super) struct DeclaredBlocks {
    journal: Mutex<Journal>,
    solved: Mutex<SolvedParents>,
}

/// Published templates a declaration's transactions are looked up in.
const RECENT_TEMPLATES: usize = 3;

impl Shared {
    /// #### PR #42: changes the Job Declaration counts.
    fn jd_stats(&self, change: impl FnOnce(&mut JdServerStats)) {
        if let Ok(mut stats) = self.stats.lock() {
            change(stats.jd_server.get_or_insert_with(Default::default));
        }
    }

    /// #### PR #42: changes the Template Distribution counts.
    pub(super) fn template_stats(&self, change: impl FnOnce(&mut TemplateServerStats)) {
        if let Ok(mut stats) = self.stats.lock() {
            change(stats.template_server.get_or_insert_with(Default::default));
        }
    }
}

/// #### PR #42: the relay side of the Template Distribution server.
pub(super) struct Relay {
    pub(super) journal: Mutex<Journal>,
    /// One relayed block per parent, as for this server's own blocks.
    pub(super) solved: Mutex<SolvedParents>,
    /// Blocks the node worker submits once, outside the journal: those the
    /// journal could not save, and at most one a check refused per 10 s.
    once: Mutex<VecDeque<PendingBlock>>,
    pub(super) unchecked_at: Mutex<Option<Instant>>,
    last_id: AtomicU64,
}

/// Blocks waiting for their one submission outside the journal.
const ONCE_QUEUE: usize = 8;

impl Relay {
    fn new(journal: Journal) -> Self {
        Self {
            journal: Mutex::new(journal),
            solved: Mutex::new(SolvedParents::default()),
            once: Mutex::new(VecDeque::new()),
            unchecked_at: Mutex::new(None),
            last_id: AtomicU64::new(0),
        }
    }

    /// A new template id: `max(last + 1, unix milliseconds)`, so ids rise
    /// within a session and across restarts, as the spec allows.
    pub(super) fn next_id(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        let mut last = self.last_id.load(Ordering::Relaxed);
        loop {
            let next = last.saturating_add(1).max(now);
            match self.last_id.compare_exchange_weak(
                last,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => last = actual,
            }
        }
    }

    /// Hands `block` (its hash and bytes) to the node worker for one
    /// submission outside the journal; the oldest waiting one gives way.
    pub(super) fn submit_once(&self, hash: String, block: &[u8]) {
        if let Ok(mut once) = self.once.lock() {
            if once.len() >= ONCE_QUEUE {
                once.pop_front();
            }
            once.push_back(PendingBlock {
                hash,
                block: hex::encode(block),
                payout: None,
                miner: None,
                operator: None,
            });
        }
    }
}

/// How many token wins wait for the claim worker before more are dropped.
const CLAIM_QUEUE: usize = 64;
/// How many proven token wins the dashboard keeps.
const RECENT_TOKEN_WINS: usize = 10;

/// #### PR #42: the template with the pool's name and the current token
/// set, and that set's serial (0 without tokens).
fn with_tokens(
    mut template: BchTemplate,
    tag: &[u8],
    tokens: Option<&TokenHub>,
) -> (BchTemplate, u64) {
    template.tag(tag);
    match tokens.and_then(TokenHub::current) {
        Some(set) => {
            let serial = set.serial();
            template.commit(set);
            (template, serial)
        }
        None => (template, 0),
    }
}

// #### PR #38
// Only the first solved block on a parent can win; later solutions on the same
// parent would only compete with it. Chipnet allows difficulty-1 blocks after
// a 20-minute gap, and then every share is a solution: an Avalon Nano sends
// about 930 a second. Saving each one filled the 64-block journal within a
// fraction of a second, and a refused save stops the server. Keep one per
// parent, for the most recent parents only.
#[derive(Default)]
pub(super) struct SolvedParents(VecDeque<Hash>);

impl SolvedParents {
    const RECENT: usize = 64;

    /// Records `parent`; false when a block on it was already saved.
    pub(super) fn first(&mut self, parent: &Hash) -> bool {
        if self.0.contains(parent) {
            return false;
        }
        if self.0.len() >= Self::RECENT {
            self.0.pop_front();
        }
        self.0.push_back(*parent);
        true
    }
}

/// #### PR #42: the listeners a server serves: devices, and Template
/// Distribution clients when asked (`--tp-listen`).
pub struct Listeners {
    pub devices: TcpListener,
    pub templates: Option<TcpListener>,
}

/// Bind the listener before calling this function. The caller owns the stop
/// flag and display, so native TUI and CPU tests use the same server lifecycle.
/// `nodes` are the BCH nodes in failover order, the one to start with first.
pub fn run<R: NodeRpc + Send + 'static>(
    listener: TcpListener,
    nodes: Vec<R>,
    config: ServerConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    let sources = rpc_sources(nodes, config.network);
    run_with(
        Listeners {
            devices: listener,
            templates: None,
        },
        sources,
        config,
        stop,
        stats,
    )
}

/// #### PR #42: nodes as template sources, in failover order.
pub fn rpc_sources<R: NodeRpc + Send + 'static>(
    nodes: Vec<R>,
    network: MiningNetwork,
) -> Vec<Box<dyn TemplateSource>> {
    nodes
        .into_iter()
        .map(|rpc| Box::new(TemplateProvider::new(rpc, network)) as Box<dyn TemplateSource>)
        .collect()
}

/// `run` with any template sources (nodes and template providers, in
/// failover order), also serving templates when `listeners` has a template
/// listener.
pub fn run_with(
    listeners: Listeners,
    sources: Vec<Box<dyn TemplateSource>>,
    config: ServerConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    let Listeners {
        devices: listener,
        templates,
    } = listeners;
    let total = sources.len();
    if total == 0 {
        return Err("no BCH node configured".into());
    }
    // #### PR #42: each source keeps its place in failover order.
    let mut sources: VecDeque<(usize, Box<dyn TemplateSource>)> =
        sources.into_iter().enumerate().collect();
    validate_payout_address(config.network, &config.payout)
        .map_err(|_| "invalid payout for selected network")?;
    if config.share_target == [0; 32] {
        return Err("share target cannot be zero".into());
    }
    let public = authority_public(&config.authority_secret)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot configure mining listener")?;
    // #### PR #42: a Job Declaration client's blocks pay the pool's outputs.
    let journal = if config.uplink.is_some() {
        Journal::open_declared(&config.journal_path, config.network)?
    } else {
        Journal::open(
            &config.journal_path,
            config.network,
            &config.payout,
            &config.legacy_sources,
        )?
    };
    // #### PR #42: the relay journal opens with the template listener, and
    // also without it while its file exists, so blocks a pool found before a
    // restart without --tp-listen still reach the node.
    let relay = match (&templates, config.relay_journal_path.as_deref()) {
        (Some(_), None) => return Err("the template server needs a relay journal".into()),
        (Some(_), Some(path)) => Some(Relay::new(Journal::open_relay(path, config.network)?)),
        (None, Some(path)) if path.exists() => {
            Some(Relay::new(Journal::open_relay(path, config.network)?))
        }
        _ => None,
    };
    if let Some(templates) = &templates {
        templates
            .set_nonblocking(true)
            .map_err(|_| "cannot configure template listener")?;
    }
    // #### PR #42: the JD journal opens when the pool accepts Full-Template
    // Job Declaration, and also without it while its file exists, so blocks
    // found before a restart still reach the node.
    let full_template = config
        .declarator
        .as_ref()
        .is_some_and(|declarator| declarator.accept.allows(true));
    let declared = match (
        full_template,
        config
            .declared_journal_path
            .as_deref()
            .filter(|_| config.uplink.is_none()),
    ) {
        (true, None) => return Err("Full-Template Job Declaration needs a JD journal".into()),
        (true, Some(path)) => Some(path),
        (false, Some(path)) if path.exists() => Some(path),
        _ => None,
    }
    .map(|path| -> Result<DeclaredBlocks, String> {
        Ok(DeclaredBlocks {
            journal: Mutex::new(Journal::open_declared(path, config.network)?),
            solved: Mutex::new(SolvedParents::default()),
        })
    })
    .transpose()?;
    if let Ok(mut stats) = stats.lock() {
        stats.nodes = total;
        stats.active_node = 0;
        stats.template_source = Some(sources[0].1.kind());
        if relay.is_some() {
            stats.template_server = Some(TemplateServerStats::default());
        }
        if let Some(declarator) = &config.declarator {
            stats.jd_server = Some(JdServerStats {
                validator: full_template.then(|| declarator.validator_kind()),
                ..JdServerStats::default()
            });
        }
    }
    let (wake, receive_blocks) = mpsc::sync_channel::<()>(1);
    // #### PR #42: the claim worker
    // What: proves each token win the device threads hand on (through a
    // bounded queue they never wait on), saves the proofs and lists them on
    // the dashboard.
    // Why: proofs and their journal take disk time; a share's
    // acknowledgement and BCH mining must never wait for them.
    // Look here if: token wins are dropped while the queue is not full, or
    // proofs stop appearing.
    let (claims, claim_worker) = match config.tokens.clone() {
        Some(hub) => {
            let (send, receive) = mpsc::sync_channel::<(TokenWin, String)>(CLAIM_QUEUE);
            let stats = stats.clone();
            let stop = stop.clone();
            let worker = thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match receive.recv_timeout(Duration::from_millis(250)) {
                        Ok((win, worker)) => {
                            let proven = hub.record(&win);
                            if let Ok(mut stats) = stats.lock() {
                                for found in proven {
                                    stats.recent_token_wins.push_back(FoundTokenWin {
                                        token: found.token,
                                        mode: if found.mode == super::merge::leaf::Mode::ShareTarget
                                        {
                                            'A'
                                        } else {
                                            'B'
                                        },
                                        height: found.height,
                                        worker: worker.clone(),
                                        found: Instant::now(),
                                    });
                                    while stats.recent_token_wins.len() > RECENT_TOKEN_WINS {
                                        stats.recent_token_wins.pop_front();
                                    }
                                }
                                stats.tokens_off = hub.off();
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
            (Some(send), Some(worker))
        }
        None => (None, None),
    };
    let shared = Arc::new(Shared {
        job: RwLock::new(None),
        stats: stats.clone(),
        wake,
        journal: Mutex::new(journal),
        fatal: Mutex::new(None),
        stop: stop.clone(),
        solved: Mutex::new(SolvedParents::default()),
        claims,
        relay,
        lanes: Arc::new(AtomicU32::new(0)),
        declared,
        recent: full_template.then(|| Mutex::new(VecDeque::new())),
    });
    update_journal_stats(&shared)?;
    update_relay_stats(&shared)?;
    update_declared_stats(&shared)?;
    let node_shared = shared.clone();
    let tag = config.pool_tag.clone();
    let node_tokens = config.tokens.clone();
    let node_uplink = config.uplink.clone();
    let node = thread::spawn(move || -> Result<(), String> {
        // #### PR #42: the plan serial jobs were last published with.
        let mut published_plan: Option<u64> = None;
        // #### PR #42: server-owned generations
        // What: the server counts its own template generations: a new one
        // whenever the active source or its generation changes, the same one
        // when a template is published again (a token set or a plan change).
        // Why: each source counts its own generations, and after a failover
        // an equal number would look like the same template, so devices
        // would get no new jobs.
        // Look here if: devices keep old jobs after the server moves to its
        // next source.
        let mut generations = Generations::default();
        let mut refreshed = Instant::now() - Duration::from_secs(60);
        let mut retries = RetrySchedule::default();
        let mut relay_retries = RetrySchedule::default();
        let mut declared_retries = RetrySchedule::default();
        while !node_shared.stop.load(Ordering::Relaxed) {
            match receive_blocks.recv_timeout(Duration::from_millis(250)) {
                Ok(()) => (),
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // #### PR #38
            // The journal is authoritative; a coalesced wake cannot lose work.
            // Release its lock before RPC so another device can persist a block
            // while a reply is delayed. Retry old blocks fairly with backoff.
            if let Some((pending, outcome, retry)) =
                submit_next(&node_shared.journal, &mut retries, &mut sources)?
            {
                if let Ok(mut stats) = node_shared.stats.lock() {
                    if retry {
                        stats.block_retries = stats.block_retries.saturating_add(1);
                    }
                    // #### PR #40: the node's answer, on the dashboard's list.
                    let result = match outcome {
                        SubmissionOutcome::Accepted => Some("accepted"),
                        SubmissionOutcome::Rejected(_) => Some("rejected"),
                        SubmissionOutcome::Pending(_) => None,
                    };
                    if let Some(found) = stats
                        .recent_blocks
                        .iter_mut()
                        .find(|found| found.hash == pending.hash)
                        .filter(|_| result.is_some())
                    {
                        found.result = result;
                    }
                }
                update_journal_stats(&node_shared)?;
                if let Ok(mut stats) = node_shared.stats.lock() {
                    stats.last_block_result = Some(match outcome {
                        SubmissionOutcome::Accepted => "accepted",
                        SubmissionOutcome::Rejected(reason)
                        | SubmissionOutcome::Pending(reason) => reason,
                    });
                }
                // Refresh even when RPC acceptance is unknown. Saved bytes
                // survive tip changes; no response is treated as acceptance.
                refreshed = Instant::now() - Duration::from_secs(60);
            }
            // #### PR #42: relayed blocks
            // What: blocks Template Distribution clients found go to the node
            // like this server's own: from the relay journal, retried with
            // backoff until the node answers exactly. A block the journal
            // could not save, or one Pickaxe's checks refused (one per 10 s),
            // is sent once.
            // Why: an SRI pool sends its blocks only to its template
            // provider; this is their only way to the chain.
            // Look here if: a pool's block is missing on chain, or the relay
            // counts on the dashboard do not move.
            if let Some(relay) = node_shared.relay.as_ref() {
                let once = relay.once.lock().ok().and_then(|mut once| once.pop_front());
                if let Some(block) = once {
                    let accepted =
                        submit_saved(&mut sources, &block) == SubmissionOutcome::Accepted;
                    node_shared.template_stats(|stats| {
                        stats.once_sent = stats.once_sent.saturating_add(1);
                        stats.once_accepted =
                            stats.once_accepted.saturating_add(u64::from(accepted));
                    });
                    refreshed = Instant::now() - Duration::from_secs(60);
                }
                if submit_next(&relay.journal, &mut relay_retries, &mut sources)?.is_some() {
                    update_relay_stats(&node_shared)?;
                    refreshed = Instant::now() - Duration::from_secs(60);
                }
            }
            // #### PR #42: a declared job's block is propagated by the pool
            // What: blocks found on Full-Template declared jobs (a client's
            // share on its custom job, or its PushSolution) go to the node
            // from the JD journal, retried with backoff until the node answers
            // exactly; the dashboard's list shows the answer.
            // Why: Full-Template lets the pool propagate its miners' blocks as
            // well as their own nodes; the PR #38 rule applies: saved first,
            // then submitted.
            // Look here if: a declared job's block is missing on chain, or its
            // result on the dashboard never comes.
            if let Some(declared) = node_shared.declared.as_ref() {
                if let Some((pending, outcome, _)) =
                    submit_next(&declared.journal, &mut declared_retries, &mut sources)?
                {
                    if let Ok(mut stats) = node_shared.stats.lock() {
                        let result = match outcome {
                            SubmissionOutcome::Accepted => Some("accepted"),
                            SubmissionOutcome::Rejected(_) => Some("rejected"),
                            SubmissionOutcome::Pending(_) => None,
                        };
                        if let Some(found) = stats
                            .recent_blocks
                            .iter_mut()
                            .find(|found| found.hash == pending.hash)
                            .filter(|_| result.is_some())
                        {
                            found.result = result;
                        }
                    }
                    update_declared_stats(&node_shared)?;
                    refreshed = Instant::now() - Duration::from_secs(60);
                }
            }
            // #### end PR #42 ####
            // #### PR #42
            // What: when a merge-mined token's state changes, the current
            // template is published again with the new token set, without
            // asking the node; devices get new jobs on the same parent.
            // Why: a win moves the token's baton, and work on the old state
            // can no longer win it. Asking the node again could fail and
            // revoke all work.
            // Look here if: jobs do not change after a token win, or a token
            // win revokes work.
            // #### PR #42: a Job Declaration plan that came or went re-offers
            // the current template (see `offer`).
            let plan_changed = node_uplink.as_ref().is_some_and(|uplink| {
                if let Ok(mut stats) = node_shared.stats.lock() {
                    stats.jd_client = uplink.status.summary.lock().ok().map(|s| s.clone());
                }
                current_plan(uplink).map(|plan| plan.serial) != published_plan
            });
            if node_tokens.as_ref().is_some_and(|hub| hub.take_changed()) || plan_changed {
                let slot = sources[0].0;
                if let Some((generation, template)) = sources[0].1.current() {
                    let generation = generations.of(slot, generation);
                    let (template, serial) =
                        with_tokens(template.clone(), &tag, node_tokens.as_deref());
                    offer(
                        &node_shared,
                        node_uplink.as_ref(),
                        &mut published_plan,
                        generation,
                        serial,
                        template,
                    );
                }
            }
            let current = sources[0].1.tip_is_current().unwrap_or(false);
            if !current || refreshed.elapsed() >= Duration::from_secs(15) {
                let slot = sources[0].0;
                match sources[0].1.refresh() {
                    Ok((generation, template)) => {
                        let generation = generations.of(slot, generation);
                        let (template, serial) =
                            with_tokens(template.clone(), &tag, node_tokens.as_deref());
                        offer(
                            &node_shared,
                            node_uplink.as_ref(),
                            &mut published_plan,
                            generation,
                            serial,
                            template,
                        );
                        refreshed = Instant::now();
                    }
                    Err(error) => {
                        if let Ok(mut stats) = node_shared.stats.lock() {
                            stats.template_failures = stats.template_failures.saturating_add(1);
                            stats.last_template_error = Some(template_reason(&error));
                        }
                        publish(&node_shared, None);
                        // #### PR #40
                        // What: with more than one node, move to the next in
                        // failover order when this one gives no template, and
                        // try it at once; the failed node waits at the back.
                        // Why: one node down stopped the whole server while
                        // its other nodes were fine. Saved blocks are whole
                        // blocks, so they go to the next node too.
                        // Look here if: the node number on the dashboard keeps
                        // changing, which means every node is failing.
                        // #### PR #42: across nodes and template providers
                        // alike (see `TemplateSource`).
                        if total > 1 {
                            if let Some((slot, mut failed)) = sources.pop_front() {
                                failed.reset();
                                sources.push_back((slot, failed));
                            }
                            if let Ok(mut stats) = node_shared.stats.lock() {
                                stats.active_node = sources[0].0;
                                stats.template_source = Some(sources[0].1.kind());
                                stats.node_switches = stats.node_switches.saturating_add(1);
                            }
                            refreshed = Instant::now() - Duration::from_secs(60);
                        }
                    }
                }
            }
        }
        publish(&node_shared, None);
        Ok(())
    });
    let config = Arc::new(config);
    let mut devices: Vec<(u64, thread::JoinHandle<()>)> = Vec::new();
    let mut template_clients: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut listener_error = None;
    while !stop.load(Ordering::Relaxed) {
        if node.is_finished() {
            listener_error = Some("template worker stopped unexpectedly".to_owned());
            break;
        }
        let mut remaining = Vec::new();
        for (id, device) in devices.drain(..) {
            if device.is_finished() {
                if device.join().is_err() {
                    device_ended(&shared, id, Some("device worker panicked"));
                }
            } else {
                remaining.push((id, device));
            }
        }
        devices = remaining;
        let mut waiting = Vec::new();
        for client in template_clients.drain(..) {
            if !client.is_finished() {
                waiting.push(client);
            } else if client.join().is_err() {
                shared.template_stats(|stats| stats.clients = stats.clients.saturating_sub(1));
            }
        }
        template_clients = waiting;
        let mut idle = true;
        match listener.accept() {
            Ok((stream, peer)) => {
                idle = false;
                // #### PR #40
                // What: accepted sockets block again before the handshake.
                // Why: on Windows an accepted socket inherits the listener's
                // non-blocking mode, so the Noise handshake's first read
                // failed at once whenever the client's first bytes came a
                // moment after the connection (under load, or from a device
                // on the network), and the server dropped it. Linux does not
                // inherit the mode, which is why the live ASIC never showed it.
                // Check: SV2 devices and the SV1 adapter connect on a Windows
                // host under load (the release-mode server tests).
                if devices.len() < MAX_CONNECTIONS && stream.set_nonblocking(false).is_ok() {
                    let shared = shared.clone();
                    let config = config.clone();
                    let id = if let Ok(mut stats) = shared.stats.lock() {
                        stats.connections += 1;
                        stats.sessions_started = stats.sessions_started.saturating_add(1);
                        let id = stats.device_stats.connect(peer, false, Instant::now());
                        stats.device_stats.set_address(id, peer.ip());
                        id
                    } else {
                        0
                    };
                    devices.push((
                        id,
                        thread::spawn(move || {
                            let mut served = Served::Device;
                            let result =
                                serve_device(stream, public, &config, &shared, id, &mut served);
                            match served {
                                Served::Device => device_ended(
                                    &shared,
                                    id,
                                    result.as_ref().err().map(String::as_str),
                                ),
                                Served::JobDeclaration => shared.jd_stats(|stats| {
                                    stats.clients = stats.clients.saturating_sub(1)
                                }),
                            }
                        }),
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
            Err(_) => {
                listener_error = Some("mining listener failed".to_owned());
                break;
            }
        }
        // #### PR #42: the template listener
        // What: Template Distribution clients (SV2 pools, Job Declaration
        // clients, P2Pool, another Pickaxe) connect here, at most 8 at once,
        // each on its own thread with the mining listener's key. The
        // accepted socket blocks again before the handshake (the PR #40
        // rule above).
        // Why: SRI's pool takes its templates from a template provider; this
        // node's templates reach it without the node's RPC login.
        // Look here if: a template client is dropped at connect, or a ninth
        // is accepted.
        if let Some(templates) = templates.as_ref() {
            match templates.accept() {
                Ok((stream, _)) => {
                    idle = false;
                    if template_clients.len() < tdp::MAX_TP_CLIENTS
                        && stream.set_nonblocking(false).is_ok()
                    {
                        let shared = shared.clone();
                        let config = config.clone();
                        shared.template_stats(|stats| stats.clients += 1);
                        template_clients.push(thread::spawn(move || {
                            let _ = tdp::server::serve(
                                stream,
                                public,
                                &config.authority_secret,
                                &shared,
                            );
                            shared.template_stats(|stats| {
                                stats.clients = stats.clients.saturating_sub(1)
                            });
                        }));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
                Err(_) => {
                    listener_error = Some("template listener failed".to_owned());
                    break;
                }
            }
        }
        if idle {
            thread::sleep(Duration::from_millis(20));
        }
    }
    stop.store(true, Ordering::Relaxed);
    for client in template_clients {
        let _ = client.join();
    }
    for (id, device) in devices {
        if device.join().is_err() {
            device_ended(&shared, id, Some("device worker panicked"));
        }
    }
    node.join()
        .map_err(|_| "template worker stopped unexpectedly")??;
    if let Some(worker) = claim_worker {
        let _ = worker.join();
    }
    if let Some(reason) = *shared
        .fatal
        .lock()
        .map_err(|_| "server failure state unavailable")?
    {
        return Err(reason.into());
    }
    if let Some(error) = listener_error {
        Err(error)
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct RetrySchedule(HashMap<String, (u32, Instant)>);

impl RetrySchedule {
    fn next(&mut self, pending: &[String], now: Instant) -> Option<String> {
        self.0.retain(|hash, _| pending.contains(hash));
        pending
            .iter()
            .find(|hash| self.0.get(*hash).is_none_or(|(_, due)| now >= *due))
            .cloned()
    }
    fn is_retry(&self, hash: &str) -> bool {
        self.0.contains_key(hash)
    }
    fn defer(&mut self, hash: &str, now: Instant) {
        let attempts = self.0.get(hash).map_or(0, |(n, _)| *n).saturating_add(1);
        let delay = (1u64 << attempts.saturating_sub(1).min(5)).min(30);
        self.0.insert(
            hash.to_owned(),
            (attempts, now + Duration::from_secs(delay)),
        );
    }
    fn remove(&mut self, hash: &str) {
        self.0.remove(hash);
    }
}

/// #### PR #42: the server's own generation numbers (see the node thread).
#[derive(Default)]
struct Generations {
    count: u64,
    last: Option<(usize, u64)>,
}

impl Generations {
    /// The server generation of `source`'s generation `generation`.
    fn of(&mut self, source: usize, generation: u64) -> u64 {
        if self.last != Some((source, generation)) {
            self.count += 1;
            self.last = Some((source, generation));
        }
        self.count
    }
}

// #### PR #42: the whole-block fallback
// What: a saved block goes to the active source; when that is a template
// provider and its answer is not final, the first node among the other
// sources gets the whole block too.
// Why: a template provider's template is full, so the journal holds a whole
// block any node of the network takes (PR #40's rule), and a provider may
// relay a block slowly or not at all.
// Look here if: a block found on a provider's template is missing on chain
// while a node is configured.
fn submit_saved(
    sources: &mut VecDeque<(usize, Box<dyn TemplateSource>)>,
    pending: &PendingBlock,
) -> SubmissionOutcome {
    let Some((_, active)) = sources.front_mut() else {
        return SubmissionOutcome::Pending("no template source");
    };
    let outcome = active.submit_saved(pending);
    if active.kind() == SourceKind::TemplateProvider
        && matches!(outcome, SubmissionOutcome::Pending(_))
    {
        if let Some((_, node)) = sources
            .iter_mut()
            .skip(1)
            .find(|(_, source)| source.kind() == SourceKind::NodeRpc)
        {
            let fallback = node.submit_saved(pending);
            if !matches!(fallback, SubmissionOutcome::Pending(_)) {
                return fallback;
            }
        }
    }
    outcome
}

/// #### PR #38: one saved block from `journal` to the node: the oldest one
/// due, retried with backoff until the node answers exactly. The journal's
/// lock is released during the RPC. Returns the block, the node's answer and
/// whether it was a retry.
fn submit_next(
    journal: &Mutex<Journal>,
    retries: &mut RetrySchedule,
    sources: &mut VecDeque<(usize, Box<dyn TemplateSource>)>,
) -> Result<Option<(PendingBlock, SubmissionOutcome, bool)>, String> {
    let pending = {
        let journal = journal.lock().map_err(|_| "block journal unavailable")?;
        retries
            .next(&journal.pending_hashes(), Instant::now())
            .and_then(|hash| journal.pending(&hash))
    };
    let Some(pending) = pending else {
        return Ok(None);
    };
    let retry = retries.is_retry(&pending.hash);
    let outcome = submit_saved(sources, &pending);
    match outcome {
        SubmissionOutcome::Accepted | SubmissionOutcome::Rejected(_) => {
            journal
                .lock()
                .map_err(|_| "block journal unavailable")?
                .finish(&pending.hash, outcome == SubmissionOutcome::Accepted)?;
            retries.remove(&pending.hash);
        }
        SubmissionOutcome::Pending(_) => retries.defer(&pending.hash, Instant::now()),
    }
    Ok(Some((pending, outcome, retry)))
}

/// #### PR #42: the relay journal's counts, for the dashboard.
pub(super) fn update_relay_stats(shared: &Shared) -> Result<(), String> {
    let Some(relay) = shared.relay.as_ref() else {
        return Ok(());
    };
    let (pending, accepted, rejected) = relay
        .journal
        .lock()
        .map_err(|_| "relay journal unavailable")?
        .counts();
    shared.template_stats(|stats| {
        stats.relay_pending = pending;
        stats.relay_accepted = accepted;
        stats.relay_rejected = rejected;
    });
    Ok(())
}

/// #### PR #42: the JD journal's counts, for the dashboard.
fn update_declared_stats(shared: &Shared) -> Result<(), String> {
    let Some(declared) = shared.declared.as_ref() else {
        return Ok(());
    };
    let (pending, accepted, rejected) = declared
        .journal
        .lock()
        .map_err(|_| "JD journal unavailable")?
        .counts();
    shared.jd_stats(|stats| {
        stats.declared_pending = pending;
        stats.declared_accepted = accepted;
        stats.declared_rejected = rejected;
    });
    Ok(())
}

/// #### PR #42: saves a block found on a declared job, the first on its
/// parent, in the JD journal and lists it, then wakes the node worker. A
/// journal that cannot save it stops the server, as for its own blocks.
fn save_declared(
    shared: &Shared,
    parent: &Hash,
    hash: Hash,
    block: &[u8],
    height: u32,
    worker: String,
) -> Result<(), String> {
    let Some(declared) = shared.declared.as_ref() else {
        return Ok(());
    };
    let first = declared
        .solved
        .lock()
        .map_err(|_| "block state unavailable")?
        .first(parent);
    if !first {
        return Ok(());
    }
    let saved = declared
        .journal
        .lock()
        .map_err(|_| "JD journal unavailable")?
        .enqueue_declared(block);
    let Ok(saved) = saved else {
        if let Ok(mut fatal) = shared.fatal.lock() {
            *fatal = Some("cannot persist solved block; mining stopped");
        }
        shared.stop.store(true, Ordering::Relaxed);
        return Err("cannot persist solved block; mining stopped".into());
    };
    if saved {
        if let Ok(mut stats) = shared.stats.lock() {
            let mut display = hash;
            display.reverse();
            stats.recent_blocks.push_back(FoundBlock {
                height,
                hash: hex::encode(display),
                worker,
                found: Instant::now(),
                result: None,
            });
            while stats.recent_blocks.len() > RECENT_BLOCKS {
                stats.recent_blocks.pop_front();
            }
            if let Some(jd) = stats.jd_server.as_mut() {
                jd.blocks += 1;
            }
        }
    }
    update_declared_stats(shared)?;
    let _ = shared.wake.try_send(());
    Ok(())
}

fn update_journal_stats(shared: &Shared) -> Result<(), String> {
    let journal = shared
        .journal
        .lock()
        .map_err(|_| "block journal unavailable")?;
    let (pending, accepted, rejected) = journal.counts();
    let mut stats = shared
        .stats
        .lock()
        .map_err(|_| "mining statistics unavailable")?;
    stats.blocks_pending = pending;
    stats.blocks_accepted = accepted;
    stats.blocks_rejected = rejected;
    Ok(())
}

pub fn authority_public(secret: &[u8; 32]) -> Result<[u8; 32], String> {
    let key = SecretKey::from_slice(secret).map_err(|_| "invalid SV2 authority secret")?;
    Ok(Keypair::from_secret_key(&Secp256k1::new(), &key)
        .x_only_public_key()
        .0
        .serialize())
}

fn device_ended(shared: &Shared, id: u64, error: Option<&str>) {
    if let Ok(mut stats) = shared.stats.lock() {
        stats.connections = stats.connections.saturating_sub(1);
        // Closing sockets during the operator's shutdown is expected.
        let error = error.filter(|_| !shared.stop.load(Ordering::Relaxed));
        if error.is_some() {
            stats.connection_errors = stats.connection_errors.saturating_add(1);
        }
        stats.device_stats.close(id, false, error, Instant::now());
    }
}

/// #### PR #42: the Job Declaration plan the uplink holds now.
fn current_plan(uplink: &UplinkHandle) -> Option<Arc<super::jd::plan::JdPlan>> {
    uplink.status.plan.read().ok().and_then(|plan| plan.clone())
}

// #### PR #42: the JD plan gates local jobs
// What: a Job Declaration client's local server publishes a template only
// while the uplink holds a plan: the template carries the plan (the pool's
// outputs, the nested extranonce) and goes to the uplink to declare. Without
// a plan nothing is published, so devices fall back to the pool's own jobs
// through the SV1 adapter. A plan change gives new jobs on the same
// template (its serial joins the token set's).
// Why: a custom job the pool has not accepted earns the miner nothing.
// Look here if: devices get no work while Job Declaration is active, or keep
// mining local jobs after a fallback.
/// Publishes a refreshed template, or under Job Declaration offers it.
fn offer(
    shared: &Shared,
    uplink: Option<&UplinkHandle>,
    published_plan: &mut Option<u64>,
    generation: u64,
    serial: u64,
    mut template: BchTemplate,
) {
    let valid_until = Instant::now() + Duration::from_secs(30);
    let Some(uplink) = uplink else {
        publish(
            shared,
            Some(PublishedJob {
                generation,
                serial,
                template: Arc::new(template),
                valid_until,
            }),
        );
        return;
    };
    let plan = current_plan(uplink);
    *published_plan = plan.as_ref().map(|plan| plan.serial);
    let Some(plan) = plan else {
        publish(shared, None);
        return;
    };
    let plan_serial = plan.serial;
    template.declare(plan);
    let template = Arc::new(template);
    let _ = uplink.events.try_send(UplinkEvent::Published {
        serial: generation,
        template: template.clone(),
    });
    publish(
        shared,
        Some(PublishedJob {
            generation,
            serial: (plan_serial << 32) | (serial & 0xffff_ffff),
            template,
            valid_until,
        }),
    );
}
// #### end PR #42 ####

fn publish(shared: &Shared, job: Option<PublishedJob>) {
    // #### PR #42: the latest templates with other transactions, for
    // Full-Template declarations.
    if let (Some(job), Some(recent)) = (job.as_ref(), shared.recent.as_ref()) {
        if let Ok(mut recent) = recent.lock() {
            if recent
                .front()
                .is_none_or(|last| last.transaction_ids() != job.template.transaction_ids())
            {
                recent.push_front(job.template.clone());
                recent.truncate(RECENT_TEMPLATES);
            }
        }
    }
    let ready = job.is_some();
    let height = job.as_ref().map(|job| job.template.height);
    if let Ok(mut current) = shared.job.write() {
        *current = job;
    }
    if let Ok(mut stats) = shared.stats.lock() {
        stats.template_ready = ready;
        stats.height = height;
    }
}

// #### PR #38
// A brief template failure is not a failed device transport. Revoke all work
// immediately, reject submissions while unavailable, then cleanly activate a
// fresh job on the same channel. The grace is bounded; never extend a job's
// original freshness lease just to keep a connection alive.
struct JobAvailability {
    generation: Option<u64>,
    /// #### PR #42: the token set's serial of the job issued last.
    serial: Option<u64>,
    unavailable_since: Option<Instant>,
    payout: Option<BchPayout>,
    next_id: u32,
    allocation: WorkAllocation,
}

impl JobAvailability {
    fn new(phase: u64) -> Self {
        Self {
            generation: None,
            serial: None,
            unavailable_since: None,
            payout: None,
            next_id: 0,
            allocation: WorkAllocation::new(phase),
        }
    }

    fn update(
        &mut self,
        mining: &mut MiningSession,
        current: Option<PublishedJob>,
        now: Instant,
        donation: BchDonation,
        fee: Option<crate::donation::bch::PoolFee>,
    ) -> Result<Vec<stratum_core::codec_sv2::SerializedFrame>, String> {
        if let Some(job) = current.filter(|job| now < job.valid_until) {
            self.allocation.update(now, !mining.channels.is_empty());
            let donation_work = self.allocation.donation_work(donation);
            let payout = BchPayout {
                donation,
                donation_work,
                // #### PR #40: a public pool's fee, after the donation.
                fee,
                fee_work: !donation_work
                    && fee.is_some_and(|fee| self.allocation.fee_work(donation, fee)),
            };
            self.unavailable_since = None;
            if self.generation == Some(job.generation)
                && self.serial == Some(job.serial)
                && self.payout == Some(payout)
            {
                return Ok(Vec::new());
            }
            // Policy rotations need unique search space even with the same
            // template generation. The provider still receives its original
            // generation, while the wire ID commits a distinct coinbase.
            self.next_id = self
                .next_id
                .checked_add(1)
                .ok_or("job identifiers exhausted")?;
            let frames =
                mining.set_job_with_payout(self.next_id, job.generation, job.template, payout)?;
            self.generation = Some(job.generation);
            self.serial = Some(job.serial);
            self.payout = Some(payout);
            return Ok(frames);
        }
        self.allocation.update(now, false);
        if self.generation.take().is_some() {
            self.serial = None;
            mining.revoke_job();
        }
        let since = *self.unavailable_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= TEMPLATE_RECOVERY_GRACE {
            return Err("template source unavailable; reconnect when healthy".into());
        }
        Ok(Vec::new())
    }
}

impl JobAvailability {
    /// #### PR #38
    /// Vardiff runs only while a job is live. A new target reuses this
    /// session's job counter, so a re-issued job never repeats an ID.
    fn retarget(
        &mut self,
        mining: &mut MiningSession,
    ) -> Result<Vec<stratum_core::codec_sv2::SerializedFrame>, String> {
        if self.generation.is_none() {
            return Ok(Vec::new());
        }
        mining.retarget(&mut self.next_id)
    }
}

pub(super) fn template_reason(error: &str) -> &'static str {
    match error {
        crate::node::NODE_RPC_LOGIN_REFUSED => "node refused the RPC login",
        "node tip changed while fetching the template" => "tip changed during refresh",
        "node is on the wrong network" => "wrong network",
        "node is not fully synchronized" => "node not synchronized",
        "node omitted height" | "node omitted tip" | "node returned an invalid tip hash" => {
            "invalid node tip"
        }
        "invalid block template" => "invalid block template",
        "template generation exhausted" => "template generation exhausted",
        // #### PR #42: template providers' reasons, as they are.
        "template provider closed the connection" => "template provider closed the connection",
        "template provider broke the protocol" => "template provider broke the protocol",
        "template provider sent an invalid template" => {
            "template provider sent an invalid template"
        }
        "template provider's network is not confirmed" => {
            "template provider's network is not confirmed"
        }
        "template provider sent no transaction data in time" => {
            "template provider sent no transaction data in time"
        }
        "template too large for SV2" => "template too large for SV2",
        "template provider sent no template" => "template provider sent no template",
        "cannot reach the template provider" => "cannot reach the template provider",
        "template provider requires coinbase outputs (a Bitcoin template?)" => {
            "template provider requires coinbase outputs (a Bitcoin template?)"
        }
        _ => "node RPC unavailable",
    }
}

/// #### PR #42: what a connection on the SV2 listener turned out to be.
enum Served {
    Device,
    JobDeclaration,
}

/// #### PR #42: whether `frame` sets up a Job Declaration session.
fn job_declaration_setup(frame: &mut stratum_core::codec_sv2::SerializedFrame) -> bool {
    let header = frame.header();
    header.msg_type() == stratum_core::common_messages_sv2::MESSAGE_TYPE_SETUP_CONNECTION
        && !header.channel_msg()
        && frame.payload().first()
            == Some(&(stratum_core::common_messages_sv2::Protocol::JobDeclarationProtocol as u8))
}

// #### PR #42: JD sessions on the SV2 listener
// What: a connection whose first SetupConnection names Job Declaration is a
// client's JD session when this pool accepts miners' own templates: it
// leaves the device table (its row goes, and the rows it took over on its
// address come back) and is counted as a JD client. Tokens it allocated go
// when it closes. Without Job Declaration the mining session refuses it with
// unsupported-protocol, as before.
// Why: Job Declaration shares the pool's SV2 port and key (D17); the client
// pins one key for both its sessions.
// Look here if: a JD client shows up as a device, or its tokens outlive it.
fn serve_declarator(
    sender: &mut Sender,
    receiver: &mut Receiver,
    first: stratum_core::codec_sv2::SerializedFrame,
    declarator: &Declarator,
    shared: &Shared,
) -> Result<(), String> {
    let mut session = DeclaratorSession::new(declarator);
    let result = (|| {
        let mut next = Some(first);
        let mut raised = false;
        while !shared.stop.load(Ordering::Relaxed) {
            let frame = match next.take() {
                Some(frame) => Some(frame),
                None => receiver.receive(Duration::from_millis(100))?,
            };
            let now = Instant::now();
            let mut responses = match frame {
                Some(mut frame) => {
                    // #### PR #42: a declaration is checked against the
                    // pool's current template and its latest few.
                    let current = shared
                        .job
                        .read()
                        .map_err(|_| "template state unavailable")?
                        .as_ref()
                        .map(|job| job.template.clone());
                    let recent: Vec<Arc<BchTemplate>> = shared
                        .recent
                        .as_ref()
                        .and_then(|recent| recent.lock().ok().map(|r| r.iter().cloned().collect()))
                        .unwrap_or_default();
                    let context = Context {
                        current: current.as_ref(),
                        recent: &recent,
                    };
                    session.receive(&mut frame, declarator, &context, now)?
                }
                None => Default::default(),
            };
            let expired = session.expire(now)?;
            responses.frames.extend(expired.frames);
            responses.refused = expired.refused.or(responses.refused);
            // #### PR #42: a Full-Template session's frames may take 16 MiB
            // (`Limits::JD`); a Coinbase-only one keeps a device's limits.
            if session.full_template() && !raised {
                sender.set_limits(Limits::JD);
                receiver.set_limits(Limits::JD);
                raised = true;
            }
            if let Some(at) = responses.not_before {
                while Instant::now() < at && !shared.stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(100));
                }
            }
            for frame in responses.frames {
                sender.send(frame)?;
            }
            shared.jd_stats(|stats| {
                stats.tokens += responses.allocated;
                stats.declared += responses.declared;
                stats.missing_rounds += responses.missing_rounds;
                stats.validations += responses.validations;
                stats.pushed += responses.blocks.len() as u64 + responses.unmatched;
                stats.push_unmatched += responses.unmatched;
                if let Some(code) = responses.refused {
                    stats.refused += 1;
                    stats.last_refusal = Some(code);
                }
                stats.validator = stats.validator.map(|_| declarator.validator_kind());
            });
            for pushed in &responses.blocks {
                save_declared(
                    shared,
                    &pushed.parent,
                    pushed.hash,
                    &pushed.block,
                    pushed.height,
                    "Job Declaration".to_owned(),
                )?;
            }
            if responses.close {
                break;
            }
        }
        Ok(())
    })();
    session.close(declarator);
    result
}

fn serve_device(
    stream: TcpStream,
    public: [u8; 32],
    config: &ServerConfig,
    shared: &Shared,
    device: u64,
    served: &mut Served,
) -> Result<(), String> {
    let session = Session::accept(stream, &public, &config.authority_secret)?;
    let (mut sender, mut receiver) = session.split();
    let result = (|| {
        let mut mining = MiningSession::new(
            config.network,
            config.payout.clone(),
            rand::random(),
            config.share_target,
        )?;
        mining.set_public(config.public.clone());
        mining.set_declarator(config.declarator.clone());
        mining.set_lanes(config.uplink.as_ref().map(|_| shared.lanes.clone()));
        let fee = config.public.as_ref().and_then(|public| public.fee);
        let phase = rand::random();
        #[cfg(test)]
        let phase = config.allocation_phase.unwrap_or(phase);
        let mut availability = JobAvailability::new(phase);
        let mut accepted = 0u64;
        let mut rejected = 0u64;
        while !shared.stop.load(Ordering::Relaxed) {
            let current_job = || {
                shared
                    .job
                    .read()
                    .map(|job| job.clone())
                    .map_err(|_| "template state unavailable")
            };
            // #### PR #42: under Job Declaration the custom job's coinbase
            // pays the donation, so no local job is donation work.
            let donation = || {
                if config.uplink.is_some() {
                    return BchDonation::try_from(0);
                }
                config
                    .donation
                    .read()
                    .map(|value| *value)
                    .map_err(|_| "donation setting unavailable".to_owned())
            };
            for frame in availability.update(
                &mut mining,
                current_job()?,
                Instant::now(),
                donation()?,
                fee,
            )? {
                sender.send(frame)?;
            }
            for frame in availability.retarget(&mut mining)? {
                sender.send(frame)?;
            }
            if let Some(mut frame) = receiver.receive(Duration::from_millis(100))? {
                // #### PR #42: JD sessions on the SV2 listener (see
                // `serve_declarator`).
                if let Some(declarator) = config
                    .declarator
                    .as_deref()
                    .filter(|_| !mining.is_set_up() && job_declaration_setup(&mut frame))
                {
                    *served = Served::JobDeclaration;
                    if let Ok(mut stats) = shared.stats.lock() {
                        stats.connections = stats.connections.saturating_sub(1);
                        stats.device_stats.forget(device);
                        stats.jd_server.get_or_insert_with(Default::default).clients += 1;
                    }
                    return serve_declarator(&mut sender, &mut receiver, frame, declarator, shared);
                }
                // A node failure or tip change may have occurred while waiting
                // for a device frame; resample before validating that frame.
                for update in availability.update(
                    &mut mining,
                    current_job()?,
                    Instant::now(),
                    donation()?,
                    fee,
                )? {
                    sender.send(update)?;
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "invalid system clock")?
                    .as_secs();
                let now = u32::try_from(now).map_err(|_| "system time exceeds header range")?;
                let responses = mining.receive(frame, now)?;
                // #### PR #42: shares for the pool, handed over without
                // waiting (see `jd::client::forward`).
                if let Some(uplink) = config.uplink.as_ref() {
                    for share in &responses.forward {
                        let _ = uplink.events.try_send(UplinkEvent::Share(share.clone()));
                    }
                }
                // #### PR #42: a custom job's outcome on the dashboard.
                if let Some(outcome) = responses.custom_job {
                    shared.jd_stats(|stats| match outcome {
                        Ok(()) => stats.custom_jobs += 1,
                        Err(code) => {
                            stats.refused += 1;
                            stats.last_refusal = Some(code);
                        }
                    });
                }
                // Persist the complete solved block before acknowledging it.
                // Storage failure stops the service rather than acknowledging
                // work which would disappear on a restart.
                for block in responses.blocks {
                    // #### PR #42: a Coinbase-only custom job's block
                    // What: it is listed on the dashboard as submitted by
                    // the miner's node, never journaled, and does not mark
                    // its parent solved.
                    // Why: the pool never sees a custom job's transactions,
                    // so only the client's node can build and submit the
                    // block; the pool's own block on the same parent must
                    // still be saved in case the client's fails.
                    // Look here if: the server stops with "cannot persist
                    // solved block" while JD clients mine.
                    // #### PR #42: a declared job's block (Full-Template) goes
                    // to the JD journal, then the pool's node.
                    if block.template.is_declared() {
                        let bytes = block.template.block(&block.coinbase, block.header)?;
                        let worker = shared
                            .stats
                            .lock()
                            .ok()
                            .and_then(|stats| stats.device_stats.label(device))
                            .unwrap_or_default();
                        save_declared(
                            shared,
                            &block.template.previous_hash,
                            block.hash,
                            &bytes,
                            block.template.height,
                            worker,
                        )?;
                        continue;
                    }
                    if block.template.is_header_only() {
                        if let Ok(mut stats) = shared.stats.lock() {
                            let mut hash = super::template::double_sha256(&block.header);
                            hash.reverse();
                            let worker = stats.device_stats.label(device).unwrap_or_default();
                            stats.recent_blocks.push_back(FoundBlock {
                                height: block.template.height,
                                hash: hex::encode(hash),
                                worker,
                                found: Instant::now(),
                                result: Some("submitted by the miner's node"),
                            });
                            while stats.recent_blocks.len() > RECENT_BLOCKS {
                                stats.recent_blocks.pop_front();
                            }
                            if let Some(jd) = stats.jd_server.as_mut() {
                                jd.blocks += 1;
                            }
                        }
                        continue;
                    }
                    // The share is still acknowledged and counted; only a
                    // repeat solution on an already solved parent is not saved.
                    let first = shared
                        .solved
                        .lock()
                        .map_err(|_| "block state unavailable")?
                        .first(&block.template.previous_hash);
                    if !first {
                        continue;
                    }
                    let saved = shared
                        .journal
                        .lock()
                        .map_err(|_| "block journal unavailable")?
                        .enqueue(&block);
                    if saved.is_err() {
                        if let Ok(mut fatal) = shared.fatal.lock() {
                            *fatal = Some("cannot persist solved block; mining stopped");
                        }
                        shared.stop.store(true, Ordering::Relaxed);
                        return Err("cannot persist solved block; mining stopped".into());
                    }
                    // #### PR #40: the dashboard lists the blocks found.
                    if saved == Ok(true) {
                        if let Ok(mut stats) = shared.stats.lock() {
                            let mut hash = super::template::double_sha256(&block.header);
                            hash.reverse();
                            let worker = stats.device_stats.label(device).unwrap_or_default();
                            stats.recent_blocks.push_back(FoundBlock {
                                height: block.template.height,
                                hash: hex::encode(hash),
                                worker,
                                found: Instant::now(),
                                result: None,
                            });
                            while stats.recent_blocks.len() > RECENT_BLOCKS {
                                stats.recent_blocks.pop_front();
                            }
                        }
                    }
                    update_journal_stats(shared)?;
                    // A full wake slot already guarantees the worker wakes;
                    // it also scans pending disk work on its bounded timeout.
                    let _ = shared.wake.try_send(());
                }
                // #### PR #42
                // What: a share's token wins go to the claim worker, each
                // token state once; a full queue drops the win (counted)
                // instead of waiting.
                // Why: the acknowledgement below must never wait on disk, and
                // a failing token journal must never stop BCH mining.
                // Look here if: shares are acknowledged late while tokens are
                // merge-mined, or the dropped count grows.
                if let (Some(hub), Some(claims)) = (config.tokens.as_ref(), shared.claims.as_ref())
                {
                    for win in responses.token_wins {
                        if !hub.first(&win) {
                            continue;
                        }
                        let worker = shared
                            .stats
                            .lock()
                            .ok()
                            .and_then(|stats| stats.device_stats.label(device))
                            .unwrap_or_default();
                        let sent = claims.try_send((win, worker)).is_ok();
                        if let Ok(mut stats) = shared.stats.lock() {
                            if sent {
                                stats.token_wins = stats.token_wins.saturating_add(1);
                            } else {
                                stats.token_wins_dropped =
                                    stats.token_wins_dropped.saturating_add(1);
                            }
                        }
                    }
                }
                let next_accepted = mining.accepted;
                let next_rejected = mining.rejected;
                if let Ok(mut stats) = shared.stats.lock() {
                    stats.shares_accepted = stats
                        .shares_accepted
                        .saturating_add(next_accepted.saturating_sub(accepted));
                    stats.shares_rejected = stats
                        .shares_rejected
                        .saturating_add(next_rejected.saturating_sub(rejected));
                    stats.device_stats.channels(device, mining.channels.len());
                    // #### PR #40: best shares, the device's and the pool's.
                    if let Some(difficulty) = mining.share_difficulty.take() {
                        if let Some(worker) = stats.device_stats.best_share(device, difficulty) {
                            if stats
                                .best_share
                                .as_ref()
                                .is_none_or(|(best, _)| difficulty > *best)
                            {
                                stats.best_share = Some((difficulty, worker));
                            }
                        }
                    }
                    // #### PR #40: named by its channel's user identity.
                    if let Some(identity) = mining.identity.take() {
                        stats.device_stats.set_worker(device, &identity);
                    }
                    if let Some(event) = responses.share_event {
                        stats
                            .device_stats
                            .share(device, event, false, Instant::now());
                    }
                }
                accepted = next_accepted;
                rejected = next_rejected;
                // #### PR #38
                // Publish validation counters before the peer can observe its
                // acknowledgement. The node worker may already have accepted
                // the block; delayed socket scheduling must not leave the
                // acknowledged share missing from the dashboard snapshot.
                for frame in responses.frames {
                    sender.send(frame)?;
                }
            }
        }
        Ok(())
    })();
    sender.close();
    receiver.close();
    result
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn only_the_first_solution_on_a_parent_is_saved() {
        let mut solved = SolvedParents::default();
        assert!(solved.first(&[1; 32]));
        assert!(!solved.first(&[1; 32]));
        assert!(solved.first(&[2; 32]));
        for parent in 3..=70u8 {
            assert!(solved.first(&[parent; 32]));
        }
        // Old parents age out, so the record stays small.
        assert_eq!(solved.0.len(), SolvedParents::RECENT);
        assert!(solved.first(&[1; 32]));
    }

    #[test]
    fn payout_rotation_issues_unique_jobs_without_changing_template_generation() {
        use super::super::channel::{Channel, ChannelKind};
        let start = Instant::now();
        let payout = super::super::template_tests::payout();
        let mut mining =
            MiningSession::new(MiningNetwork::Chipnet, payout.clone(), [17; 12], [255; 32])
                .unwrap();
        mining.insert_channel(
            Channel::new(
                1,
                ChannelKind::Standard,
                [255; 32],
                [17; 12],
                MiningNetwork::Chipnet,
                &payout,
            )
            .unwrap(),
            [255; 32],
        );
        let job = PublishedJob {
            generation: 99,
            serial: 0,
            template: Arc::new(
                BchTemplate::from_rpc(&super::super::template_tests::rpc_template()).unwrap(),
            ),
            valid_until: start + Duration::from_secs(30),
        };
        let mut availability = JobAvailability::new(0);
        for (seconds, rate, expected_work, expected_id) in [
            (0, "1.5", true, 1),
            (3, "1.5", false, 2),
            (5, "2", false, 3),
        ] {
            availability
                .update(
                    &mut mining,
                    Some(job.clone()),
                    start + Duration::from_secs(seconds),
                    rate.parse().unwrap(),
                    None,
                )
                .unwrap();
            let current = mining.channels[&1].job().unwrap();
            assert_eq!(current.id, expected_id);
            assert_eq!(current.generation, 99);
            assert_eq!(current.payout.donation_work, expected_work);
            assert_eq!(current.payout.donation, rate.parse().unwrap());
        }
        assert!(availability
            .update(
                &mut mining,
                Some(job),
                start + Duration::from_secs(6),
                "2".parse().unwrap(),
                None,
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn expired_template_is_revoked_and_repeated_failures_do_not_extend_grace() {
        let start = Instant::now();
        let mut mining = MiningSession::new(
            MiningNetwork::Chipnet,
            super::super::template_tests::payout(),
            [17; 12],
            [255; 32],
        )
        .unwrap();
        let job = PublishedJob {
            generation: 1,
            serial: 0,
            template: Arc::new(
                BchTemplate::from_rpc(&super::super::template_tests::rpc_template()).unwrap(),
            ),
            valid_until: start + Duration::from_secs(30),
        };
        let mut availability = JobAvailability::new(300_000_000_000);
        availability
            .update(
                &mut mining,
                Some(job.clone()),
                start,
                BchDonation::default(),
                None,
            )
            .unwrap();
        assert_eq!(availability.generation, Some(1));
        availability
            .update(
                &mut mining,
                Some(job.clone()),
                job.valid_until,
                BchDonation::default(),
                None,
            )
            .unwrap();
        assert_eq!(availability.generation, None);
        availability
            .update(
                &mut mining,
                None,
                job.valid_until + Duration::from_secs(2),
                BchDonation::default(),
                None,
            )
            .unwrap();
        assert!(availability
            .update(
                &mut mining,
                Some(job.clone()),
                job.valid_until + TEMPLATE_RECOVERY_GRACE,
                BchDonation::default(),
                None,
            )
            .is_err());
        assert_eq!(
            template_reason("private://user:secret@node/?key=secret"),
            "node RPC unavailable"
        );
        assert_eq!(
            template_reason("node tip changed while fetching the template"),
            "tip changed during refresh"
        );
    }

    #[test]
    fn pending_retry_backoff_is_bounded_and_does_not_block_new_work() {
        let mut schedule = RetrySchedule::default();
        let mut now = Instant::now();
        let hashes = vec!["old".to_owned(), "new".to_owned()];
        assert_eq!(schedule.next(&hashes, now).as_deref(), Some("old"));
        for expected in [1, 2, 4, 8, 16, 30, 30] {
            schedule.defer("old", now);
            assert_eq!(schedule.next(&hashes, now).as_deref(), Some("new"));
            assert_eq!(
                schedule.0["old"].1.duration_since(now),
                Duration::from_secs(expected)
            );
            now += Duration::from_secs(expected);
            assert_eq!(schedule.next(&hashes, now).as_deref(), Some("old"));
        }
        schedule.next(&["new".to_owned()], now);
        assert!(!schedule.is_retry("old"));
    }
}
