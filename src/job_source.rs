//! #### PR #42
//! Where GPU mining takes its PHOTON jobs, and the way back to the miner's
//! own node after a fall back to the Fulcrum servers: a watch that asks the
//! node again on its own thread, waiting longer after each failure, and
//! hands a ready node to the supervisor only when no claim is in flight.

use crate::config::MiningNetwork;
use crate::electrum::{ElectrumSession, LiveJob};
use crate::node::NodeInfo;
use crate::protocol::PhotonDeployment;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// The first try comes this long after a fall back; each failure doubles
/// the wait, up to `MAX_WAIT`.
pub const FIRST_WAIT: Duration = Duration::from_secs(15);
pub const MAX_WAIT: Duration = Duration::from_secs(300);
/// A syncing node is asked again after this long.
const SYNCING_WAIT: Duration = Duration::from_secs(120);
/// A node with a setting to fix (its login, its network, its PHOTON state)
/// is asked again after this long.
const SETTINGS_WAIT: Duration = Duration::from_secs(30 * 60);
/// The longest reason kept for screens.
const REASON_LEN: usize = 160;

/// Why the miner's node is not the job source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeTrouble {
    Down,
    Syncing,
    WrongNetwork,
    Login,
    NoPhoton,
}

impl NodeTrouble {
    pub fn label(self) -> &'static str {
        match self {
            Self::Down => "not answering",
            Self::Syncing => "syncing",
            Self::WrongNetwork => "on another network",
            Self::Login => "refusing the RPC login",
            Self::NoPhoton => "without the PHOTON state",
        }
    }

    /// The wait before the next try after this trouble, `backoff` being
    /// the doubling wait so far.
    fn wait(self, backoff: Duration) -> Duration {
        match self {
            Self::Down => backoff,
            Self::Syncing => SYNCING_WAIT,
            Self::WrongNetwork | Self::Login | Self::NoPhoton => SETTINGS_WAIT,
        }
    }
}

/// Sorts a node error into what to tell the miner and how long to wait.
pub fn classify(error: &str) -> NodeTrouble {
    let text = error.to_ascii_lowercase();
    let any = |words: &[&str]| {
        words
            .iter()
            .any(|word| text.contains(&word.to_ascii_lowercase()))
    };
    if any(&[crate::node::NODE_RPC_LOGIN_REFUSED, " 401", "unauthorized"]) {
        NodeTrouble::Login
    } else if any(&[
        "syncing",
        "initial block download",
        "\"code\":-28",
        "loading block",
    ]) {
        NodeTrouble::Syncing
    } else if any(&[
        "is not on",
        "another network",
        "wrong network",
        "not bitcoin cash",
        "not chipnet",
    ]) {
        NodeTrouble::WrongNetwork
    } else if any(&[
        "connect",
        "refused",
        "timed out",
        "timeout",
        "reset",
        "unreachable",
        "closed",
    ]) {
        NodeTrouble::Down
    } else if any(&["-32601", "method not found", "baton", "photon"]) {
        NodeTrouble::NoPhoton
    } else {
        NodeTrouble::Down
    }
}

/// Where jobs come from now, for the dashboard and the status file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum JobSourceStatus {
    /// The Fulcrum servers, with no node configured.
    #[default]
    Fulcrum,
    /// The miner's own node.
    Node,
    /// The Fulcrum servers while the node is down: for how long, when the
    /// next try comes, and why.
    FulcrumNodeDown {
        down_secs: u64,
        next_try_secs: u64,
        trouble: NodeTrouble,
        reason: String,
    },
}

impl JobSourceStatus {
    /// The status file's `kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Fulcrum => "fulcrum",
            Self::Node => "node",
            Self::FulcrumNodeDown { .. } => "fulcrum-node-down",
        }
    }

    /// One line for screens, such as "Fulcrum (node down 4m, next try in
    /// 25s)".
    pub fn label(&self) -> String {
        match self {
            Self::Fulcrum => "Fulcrum".into(),
            Self::Node => "your node".into(),
            Self::FulcrumNodeDown {
                down_secs,
                next_try_secs,
                trouble,
                ..
            } => format!(
                "Fulcrum (node {} {}, next try in {})",
                match trouble {
                    NodeTrouble::Down => "down",
                    other => other.label(),
                },
                span(*down_secs),
                span(*next_try_secs)
            ),
        }
    }
}

/// "45s", "4m", "2h".
fn span(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3_600),
    }
}

/// What a watch's turn gives the supervisor.
pub enum Poll<T> {
    Nothing,
    /// A node that answered, to take over as the job source now.
    Ready(T),
}

/// The way back to the node: down since a fall back, the next try, and a
/// try running on its own thread or a ready node waiting for a safe moment.
pub struct NodeWatch<T> {
    down_since: Option<Instant>,
    next_try: Instant,
    backoff: Duration,
    trouble: NodeTrouble,
    reason: String,
    probe: Option<Receiver<Result<T, String>>>,
    ready: Option<T>,
}

impl<T> Default for NodeWatch<T> {
    fn default() -> Self {
        Self {
            down_since: None,
            next_try: Instant::now(),
            backoff: FIRST_WAIT,
            trouble: NodeTrouble::Down,
            reason: String::new(),
            probe: None,
            ready: None,
        }
    }
}

impl<T> NodeWatch<T> {
    pub fn is_down(&self) -> bool {
        self.down_since.is_some()
    }

    /// The node is the job source (again), or none is configured.
    pub fn up(&mut self) {
        self.down_since = None;
        self.probe = None;
        self.ready = None;
        self.backoff = FIRST_WAIT;
    }

    /// The job source fell back to Fulcrum at `now` because of `reason`;
    /// returns whether the node was up until now.
    pub fn down(&mut self, now: Instant, reason: &str) -> bool {
        let newly = self.down_since.is_none();
        if newly {
            self.down_since = Some(now);
            self.backoff = FIRST_WAIT;
        }
        self.note(reason);
        self.next_try = now + self.trouble.wait(self.backoff);
        newly
    }

    /// Ask the node again at once (the miner's Reconnect).
    pub fn try_now(&mut self, now: Instant) {
        if self.is_down() {
            self.backoff = FIRST_WAIT;
            self.next_try = now;
        }
    }

    /// One supervisor turn: starts a try when one is due (`start` runs it
    /// on its own thread and returns where its result arrives), takes a
    /// try's result, and hands a ready node over only when `safe` (no
    /// winner, claim or winner refresh in flight).
    pub fn poll(
        &mut self,
        now: Instant,
        safe: bool,
        start: impl FnOnce() -> Receiver<Result<T, String>>,
    ) -> Poll<T> {
        if !self.is_down() {
            return Poll::Nothing;
        }
        if let Some(probe) = &self.probe {
            match probe.try_recv() {
                Ok(Ok(node)) => {
                    self.probe = None;
                    self.ready = Some(node);
                }
                Ok(Err(reason)) => {
                    self.probe = None;
                    self.failed(now, &reason);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.probe = None;
                    self.failed(now, "the node check stopped");
                }
            }
        }
        if self.ready.is_some() {
            if !safe {
                return Poll::Nothing;
            }
            let node = self.ready.take();
            self.up();
            return node.map_or(Poll::Nothing, Poll::Ready);
        }
        if self.probe.is_none() && now >= self.next_try {
            self.probe = Some(start());
        }
        Poll::Nothing
    }

    fn failed(&mut self, now: Instant, reason: &str) {
        self.note(reason);
        if self.trouble == NodeTrouble::Down {
            self.backoff = self.backoff.saturating_mul(2).min(MAX_WAIT);
        }
        self.next_try = now + self.trouble.wait(self.backoff);
    }

    fn note(&mut self, reason: &str) {
        self.trouble = classify(reason);
        self.reason = reason
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|ch| !ch.is_control())
            .take(REASON_LEN)
            .collect();
    }

    /// The job source for screens; `node` is whether the node is the
    /// session's source now.
    pub fn status(&self, now: Instant, node: bool) -> JobSourceStatus {
        if node {
            return JobSourceStatus::Node;
        }
        match self.down_since {
            None => JobSourceStatus::Fulcrum,
            Some(since) => JobSourceStatus::FulcrumNodeDown {
                down_secs: now.saturating_duration_since(since).as_secs(),
                next_try_secs: if self.probe.is_some() || self.ready.is_some() {
                    0
                } else {
                    self.next_try.saturating_duration_since(now).as_secs()
                },
                trouble: self.trouble,
                reason: self.reason.clone(),
            },
        }
    }
}

/// One try at the miner's own nodes, on its own thread: a node must be on
/// `network`, synced and at most a block behind the mined job, and then it
/// follows the mined baton (`live`), with no scan while that baton is
/// current.
pub fn try_node_return(
    nodes: &[String],
    deployment: &'static PhotonDeployment,
    network: MiningNetwork,
    live: &LiveJob,
) -> Result<(ElectrumSession, LiveJob), String> {
    first_ready_node(
        nodes,
        network,
        live.height,
        crate::node::node_info,
        |node| {
            // #### PR #42: BCH, not BTC; Chipnet, not testnet4.
            crate::node::verify_chain(node, network)?;
            ElectrumSession::connect_node_failover(&[node.to_owned()], deployment, Some(live))
                .and_then(|mut session| session.fetch_live_job().map(|job| (session, job)))
        },
    )
}

/// The first of `nodes` whose `info` passes `node_ready` and that `connect`
/// takes, or every node's reason.
fn first_ready_node<T>(
    nodes: &[String],
    network: MiningNetwork,
    height: u32,
    info: impl Fn(&str) -> Result<NodeInfo, String>,
    connect: impl Fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    let mut failures = Vec::new();
    for node in nodes {
        match info(node)
            .and_then(|info| node_ready(&info, network, height))
            .and_then(|()| connect(node))
        {
            Ok(ready) => return Ok(ready),
            Err(error) => failures.push(error),
        }
    }
    Err(if failures.is_empty() {
        "no node is configured".into()
    } else {
        failures.join("; ")
    })
}

/// Whether a node may take over: on `network`, synced, and at most one
/// block behind the job mined at `height`.
fn node_ready(info: &NodeInfo, network: MiningNetwork, height: u32) -> Result<(), String> {
    if info.network() != Some(network) {
        return Err(format!("the node is not on {}", network.as_str()));
    }
    if info.syncing || info.blocks < info.headers {
        return Err(format!("the node is syncing ({})", info.summary()));
    }
    if info.blocks + 1 < u64::from(height) {
        return Err(format!(
            "the node is syncing: height {} while the mined job is at {height}",
            info.blocks
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A try that answers `result` at once.
    fn answer(result: Result<u32, &str>) -> Receiver<Result<u32, String>> {
        let (send, receive) = mpsc::sync_channel(1);
        send.send(result.map_err(str::to_owned)).unwrap();
        receive
    }

    // #### PR #42
    // What: after a fall back the first try comes at 15 s, and each failure
    // doubles the wait (30, 60, ...) up to 300 s; a ready node waits while a
    // claim is in flight (`safe` false) and is handed over once it is safe,
    // which ends the watch; Reconnect asks at once; a node that is up
    // starts nothing.
    // Look here if: NodeWatch's timing or hand-over changes.
    #[test]
    fn waits_backs_off_and_offers_a_node_when_safe() {
        let start = Instant::now();
        let mut watch = NodeWatch::<u32>::default();
        assert!(matches!(
            watch.poll(start, true, || unreachable!("up")),
            Poll::Nothing
        ));
        assert!(watch.down(start, "connect: connection refused"));
        assert!(!watch.down(start, "connect: connection refused"));
        let mut started = 0;
        let mut at = start + Duration::from_secs(14);
        assert!(matches!(
            watch.poll(at, true, || unreachable!("not due")),
            Poll::Nothing
        ));
        let mut waits = Vec::new();
        for _ in 0..6 {
            let due = watch.next_try;
            waits.push(due.saturating_duration_since(at).as_secs());
            at = due;
            watch.poll(at, true, || {
                started += 1;
                answer(Err("connect: connection refused"))
            });
            watch.poll(at, true, || unreachable!("one try at a time"));
        }
        assert_eq!(started, 6);
        assert_eq!(waits, [1, 30, 60, 120, 240, 300]);
        assert_eq!(watch.status(at, false).kind(), "fulcrum-node-down");
        // Reconnect asks at once, and the wait starts over.
        watch.try_now(at);
        watch.poll(at, true, || answer(Ok(7)));
        // A ready node waits for the claim, then takes over.
        assert!(matches!(
            watch.poll(at, false, || unreachable!()),
            Poll::Nothing
        ));
        assert!(watch.is_down());
        assert!(matches!(
            watch.poll(at, true, || unreachable!()),
            Poll::Ready(7)
        ));
        assert!(!watch.is_down());
        assert_eq!(watch.status(at, true), JobSourceStatus::Node);
        assert_eq!(watch.status(at, false), JobSourceStatus::Fulcrum);
    }

    // #### PR #42
    // What: a node found ready while a winner or claim is in flight is held
    // (no second try starts) and handed over only once the claim is done.
    // Look here if: the hand-over's safety changes.
    #[test]
    fn a_node_ready_during_a_claim_waits_for_the_claim() {
        let start = Instant::now();
        let mut watch = NodeWatch::<u32>::default();
        watch.down(start, "timed out");
        let due = start + FIRST_WAIT;
        watch.poll(due, false, || answer(Ok(1)));
        for second in 0..60 {
            let at = due + Duration::from_secs(second);
            assert!(matches!(
                watch.poll(at, false, || unreachable!("held, not asked again")),
                Poll::Nothing
            ));
        }
        assert_eq!(watch.status(due, false).kind(), "fulcrum-node-down");
        assert!(matches!(
            watch.poll(due, true, || unreachable!()),
            Poll::Ready(1)
        ));
    }

    // #### PR #42
    // What: the status names how long the node has been down, when the next
    // try comes and why; a syncing node waits 2 minutes and a refused login
    // 30, without doubling.
    // Look here if: JobSourceStatus's text or the trouble waits change.
    #[test]
    fn status_names_down_time_next_try_and_trouble() {
        let start = Instant::now();
        let mut watch = NodeWatch::<u32>::default();
        watch.down(start, "connect: connection refused\nsecond line");
        let status = watch.status(start + Duration::from_secs(250), false);
        assert_eq!(
            status.label(),
            "Fulcrum (node down 4m, next try in 0s)",
            "{status:?}"
        );
        let status = watch.status(start + Duration::from_secs(10), false);
        assert_eq!(status.label(), "Fulcrum (node down 10s, next try in 5s)");
        let JobSourceStatus::FulcrumNodeDown { reason, .. } = status else {
            panic!("{status:?}")
        };
        assert_eq!(reason, "connect: connection refused");
        watch.down(
            start,
            "the node is syncing (BCHN · syncing, height 1 of 9 (0%))",
        );
        assert_eq!(watch.next_try, start + SYNCING_WAIT);
        assert!(watch
            .status(start, false)
            .label()
            .starts_with("Fulcrum (node syncing"));
        watch.down(start, crate::node::NODE_RPC_LOGIN_REFUSED);
        assert_eq!(watch.next_try, start + SETTINGS_WAIT);
        assert_eq!(JobSourceStatus::Node.label(), "your node");
        assert_eq!(JobSourceStatus::Fulcrum.kind(), "fulcrum");
    }

    // #### PR #42
    #[test]
    fn classify_maps_node_errors() {
        for (error, trouble) in [
            (crate::node::NODE_RPC_LOGIN_REFUSED, NodeTrouble::Login),
            ("HTTP/1.1 401 Unauthorized", NodeTrouble::Login),
            (
                "rpc error: {\"code\":-28,\"message\":\"Loading block index...\"}",
                NodeTrouble::Syncing,
            ),
            ("the node is syncing: height 3", NodeTrouble::Syncing),
            ("the node is not on chipnet", NodeTrouble::WrongNetwork),
            (
                "the node is on Bitcoin (BTC), not Bitcoin Cash",
                NodeTrouble::WrongNetwork,
            ),
            ("the node is on testnet4, not Chipnet", NodeTrouble::WrongNetwork),
            (
                "rpc error: {\"code\":-32601,\"message\":\"Method not found\"}",
                NodeTrouble::NoPhoton,
            ),
            ("native PHOTON baton is gone", NodeTrouble::NoPhoton),
            (
                "All native node PHOTON-state RPCs failed:\nhttp://***@127.0.0.1:8332: connect: refused",
                NodeTrouble::Down,
            ),
            ("something else", NodeTrouble::Down),
        ] {
            assert_eq!(classify(error), trouble, "{error}");
        }
    }

    fn info(chain: &str, blocks: u64, headers: u64, syncing: bool) -> NodeInfo {
        NodeInfo {
            client: "Bitcoin Cash Node 29.1.0".into(),
            chain: chain.into(),
            blocks,
            headers,
            syncing,
            progress: 1.0,
        }
    }

    // #### PR #42
    // What: a node on another network, a syncing node and a node more than
    // one block behind the mined job are refused, each with its reason,
    // before any PHOTON call; the first node that passes is connected.
    // Look here if: node_ready or first_ready_node changes.
    #[test]
    fn try_node_return_refuses_syncing_foreign_and_lagging_nodes() {
        let nodes: Vec<String> = ["a", "b", "c", "d"].map(str::to_owned).to_vec();
        let infos = |node: &str| {
            Ok(match node {
                "a" => info("main", 1_000, 1_000, false),
                "b" => info("chip", 1_000, 1_200, false),
                "c" => info("chip", 997, 997, false),
                _ => info("chip", 999, 999, false),
            })
        };
        let connected = std::cell::RefCell::new(Vec::new());
        let ready = first_ready_node(&nodes, MiningNetwork::Chipnet, 1_000, infos, |node| {
            connected.borrow_mut().push(node.to_owned());
            Ok(node.to_owned())
        })
        .unwrap();
        assert_eq!(ready, "d");
        assert_eq!(*connected.borrow(), ["d"]);
        let error = first_ready_node(&nodes[..3], MiningNetwork::Chipnet, 1_000, infos, |_| {
            Ok::<_, String>(())
        })
        .unwrap_err();
        assert!(error.contains("not on chipnet"), "{error}");
        assert!(error.contains("syncing (Bitcoin Cash Node"), "{error}");
        assert!(
            error.contains("height 997 while the mined job is at 1000"),
            "{error}"
        );
        assert_eq!(classify(&error), NodeTrouble::Syncing);
        // A node that passes but cannot follow the baton gives its reason.
        let error = first_ready_node(&nodes[3..], MiningNetwork::Chipnet, 1_000, infos, |_| {
            Err::<(), _>("native PHOTON baton is gone".into())
        })
        .unwrap_err();
        assert_eq!(classify(&error), NodeTrouble::NoPhoton);
        assert!(
            first_ready_node(&[], MiningNetwork::Chipnet, 1, infos, |_| Ok(()))
                .unwrap_err()
                .contains("no node")
        );
    }
}
