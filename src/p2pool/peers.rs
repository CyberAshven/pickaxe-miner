//! Finding other P2Pool servers (docs/p2pool.md, "Finding other P2Pool
//! servers").
//!
//! P2Pool dials only the `dial_peers` of its configuration: Bitcoin Cash has
//! no P2Pool DNS seeds yet, and a Bitcoin Cash node never asks any. Once one
//! peer answers, P2Pool finds the others itself (libp2p identify and
//! Kademlia), so one reachable first contact is enough. Pickaxe gives it
//! that contact from three lists: the seeds compiled in below, the peers
//! P2Pool was connected to on earlier runs (kept per network in
//! `p2pool/peers-<network>.json`) and your own (`pickaxe p2pool set
//! --peer`). Before each start it checks which of them answer, as Gupaxx
//! checks its node list, and dials the fastest. Nothing here runs on a
//! timer: the check is made at a start and on `pickaxe p2pool peers --check`.
use crate::config::MiningNetwork;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::VecDeque;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// P2Pool servers on mainnet that every Pickaxe may dial first. None yet.
///
/// To add one, put its P2P address in this list as P2Pool's `dial_peers`
/// takes it: `"/ip4/203.0.113.7/tcp/6884"`, `"/ip6/2001:db8::7/tcp/6884"`,
/// or a DNS name such as `"/dns4/p2pool.example.com/tcp/6884"` (`/dns6/`
/// and `/dns/` too), which is looked up at each start, so the server can
/// move without a new Pickaxe. No `/p2p/<peer id>`: P2Pool makes a new
/// identity at each start. A seed runs P2Pool on this network, with its P2P
/// port open, on the share chain Pickaxe's miners use (mini by default).
/// The next Pickaxe release carries the list; each start checks every seed
/// and dials the fastest that answer.
pub const SEEDS_MAINNET: &[&str] = &[];
/// Chipnet's seeds, added as [`SEEDS_MAINNET`] says. None yet.
pub const SEEDS_CHIPNET: &[&str] = &[];

/// Entries the known-peers file keeps.
pub const FILE_CAP: usize = 64;
/// A learned peer that has not answered for this long is dropped.
const MAX_SILENCE: u64 = 7 * 24 * 60 * 60;
/// Each check is one TCP connect, given this long.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(2);
/// Checks made at once.
const PARALLEL: usize = 16;
/// All the checks of one start take at most this many check timeouts; the
/// ones not started by then are not made.
const CRAWL_ROUNDS: u32 = 5;
/// Peers P2Pool dials at a start besides yours, which are always dialed.
pub const MAX_DIAL: usize = 8;
/// While P2Pool runs, the peers it is connected to are saved when one is
/// new, and again this often to keep their times current.
const RESAVE: Duration = Duration::from_secs(60 * 60);

/// The seeds of `network`.
pub fn seeds(network: MiningNetwork) -> &'static [&'static str] {
    match network {
        MiningNetwork::Mainnet => SEEDS_MAINNET,
        MiningNetwork::Chipnet => SEEDS_CHIPNET,
    }
}

/// The known-peers file of `network` in P2Pool's home.
pub fn file(network: MiningNetwork, home: &Path) -> PathBuf {
    home.join(format!("peers-{}.json", network.as_str()))
}

/// One address in the known-peers file: a peer P2Pool was connected to
/// (learned), or the last check of a seed or of one of your peers.
/// Addresses and times only; nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub address: String,
    /// P2Pool was connected to it, so the next start dials it again.
    #[serde(default)]
    pub learned: bool,
    /// When it was first kept, in Unix seconds.
    pub added: u64,
    /// When it was last checked.
    pub checked: Option<u64>,
    /// How long the last check's TCP connect took; none when it did not
    /// answer.
    pub millis: Option<u64>,
    /// When a check last answered or P2Pool was last connected to it.
    pub reached: Option<u64>,
}

impl Known {
    fn new(address: &str, learned: bool, now: u64) -> Self {
        Self {
            address: address.to_owned(),
            learned,
            added: now,
            checked: None,
            millis: None,
            reached: None,
        }
    }

    fn alive(&self) -> u64 {
        self.reached.unwrap_or(self.added)
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    peers: Vec<Known>,
}

/// The kept peers: none when the file is missing or unreadable, since it
/// is only a memory of earlier runs.
pub fn load(file: &Path) -> Vec<Known> {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
        .map(|file| file.peers)
        .unwrap_or_default()
}

fn save(file: &Path, peers: &[Known]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&serde_json::json!({ "peers": peers }))
        .map_err(|error| error.to_string())?;
    super::write_atomically(file, text.as_bytes())
}

/// Keeps the file to its rules: a check record only while it is a seed or
/// one of your peers, a learned peer until it has been silent for a week,
/// one entry per address, and at most [`FILE_CAP`]: the configured ones
/// first, then the most recently reached.
fn tidy(peers: &mut Vec<Known>, configured: &[String], now: u64) {
    let mut seen = Vec::<String>::new();
    peers.retain_mut(|peer| {
        if seen.contains(&peer.address) {
            return false;
        }
        seen.push(peer.address.clone());
        if configured.contains(&peer.address) {
            peer.learned = false;
            return true;
        }
        peer.learned && now.saturating_sub(peer.alive()) <= MAX_SILENCE
    });
    peers.sort_by_key(|peer| (peer.learned, Reverse(peer.alive())));
    peers.truncate(FILE_CAP);
}

/// The entry of `address`, added as `learned` when there is none.
fn entry<'a>(peers: &'a mut Vec<Known>, address: &str, learned: bool, now: u64) -> &'a mut Known {
    let index = match peers.iter().position(|peer| peer.address == address) {
        Some(index) => index,
        None => {
            peers.push(Known::new(address, learned, now));
            peers.len() - 1
        }
    };
    &mut peers[index]
}

/// Where a P2P address points.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
    family: Family,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    V4,
    V6,
    Any,
}

impl Family {
    fn takes(self, address: &SocketAddr) -> bool {
        match self {
            Self::V4 => address.is_ipv4(),
            Self::V6 => address.is_ipv6(),
            Self::Any => true,
        }
    }
}

/// The host and TCP port of a P2P address as P2Pool dials it:
/// `/ip4/<address>/tcp/<port>`, `/ip6/...`, `/dns4/<name>/...`, `/dns6/`
/// or `/dns/`, with an optional `/p2p/<peer id>` after.
fn target(address: &str) -> Option<Target> {
    let mut parts = address.trim().strip_prefix('/')?.split('/');
    let (kind, host, protocol, port) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    match (parts.next(), parts.next(), parts.next()) {
        (None, _, _) => {}
        (Some("p2p"), Some(id), None) if !id.is_empty() => {}
        _ => return None,
    }
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    if protocol != "tcp" || host.is_empty() {
        return None;
    }
    let family = match kind {
        "ip4" => host.parse::<Ipv4Addr>().ok().map(|_| Family::V4)?,
        "ip6" => host.parse::<Ipv6Addr>().ok().map(|_| Family::V6)?,
        "dns4" => Family::V4,
        "dns6" => Family::V6,
        "dns" => Family::Any,
        _ => return None,
    };
    Some(Target {
        host: host.to_owned(),
        port,
        family,
    })
}

/// Whether P2Pool can dial `address` (`pickaxe p2pool set --peer` checks
/// it).
pub fn is_dial_address(address: &str) -> bool {
    target(address).is_some()
}

/// `address` as a peer to dial again: trimmed and without a trailing
/// `/p2p/<peer id>`, since P2Pool makes a new identity at each start and a
/// dial naming the old one would fail. None when P2Pool cannot dial it.
fn dial_address(address: &str) -> Option<String> {
    let address = address.trim();
    let address = address.find("/p2p/").map_or(address, |at| &address[..at]);
    target(address).map(|_| address.to_owned())
}

/// The addresses P2Pool dialed out to, from its API's `GET /peers`
/// (`[{"peer_id", "ip", "address", "direction", "connected_secs"}]`). An
/// inbound peer's address is the port it connected from, not one to dial,
/// so only outbound peers count.
pub(crate) fn outbound(peers: &serde_json::Value) -> Vec<String> {
    peers
        .as_array()
        .into_iter()
        .flatten()
        .filter(|peer| peer["direction"] == "Outbound")
        .filter_map(|peer| peer["address"].as_str())
        .filter_map(dial_address)
        .collect()
}

/// `addresses` trimmed, without empty ones and repeats, in order.
fn distinct<'a>(addresses: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for address in addresses {
        let address = address.trim();
        if !address.is_empty() && !out.iter().any(|kept| kept == address) {
            out.push(address.to_owned());
        }
    }
    out
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// What one check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Its TCP connect took this long.
    Took(Duration),
    Silent,
}

/// One TCP connect to `address`, within `timeout`. A DNS name is looked up
/// first, so the time is the connect's alone; its addresses are tried
/// IPv4 first (P2Pool listens on IPv4 as Pickaxe configures it), at most
/// four, sharing the timeout.
fn check(address: &str, timeout: Duration) -> Answer {
    let Some(target) = target(address) else {
        return Answer::Silent;
    };
    let Ok(resolved) = (target.host.as_str(), target.port).to_socket_addrs() else {
        return Answer::Silent;
    };
    let mut sockets: Vec<SocketAddr> = resolved
        .filter(|socket| target.family.takes(socket))
        .collect();
    sockets.sort_by_key(SocketAddr::is_ipv6);
    sockets.truncate(4);
    let started = Instant::now();
    for (index, socket) in sockets.iter().enumerate() {
        let left = timeout.saturating_sub(started.elapsed());
        let share = left / u32::try_from(sockets.len() - index).unwrap_or(1);
        if share.is_zero() {
            break;
        }
        let attempt = Instant::now();
        if TcpStream::connect_timeout(socket, share).is_ok() {
            return Answer::Took(attempt.elapsed());
        }
    }
    Answer::Silent
}

/// Checks `addresses`, at most [`PARALLEL`] at once and for at most
/// [`CRAWL_ROUNDS`] timeouts in all: each answer, or none for a check not
/// made in time.
fn crawl(addresses: &[String], timeout: Duration) -> Vec<Option<Answer>> {
    let mut answers = vec![None; addresses.len()];
    let queue = Arc::new(Mutex::new(
        addresses
            .iter()
            .cloned()
            .enumerate()
            .collect::<VecDeque<_>>(),
    ));
    let (send, receive) = mpsc::channel();
    for _ in 0..addresses.len().min(PARALLEL) {
        let (queue, send) = (Arc::clone(&queue), send.clone());
        let spawned = std::thread::Builder::new()
            .name("pickaxe-p2pool-peers".into())
            .spawn(move || loop {
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                let Some((index, address)) = next else {
                    return;
                };
                if send.send((index, check(&address, timeout))).is_err() {
                    return;
                }
            });
        if spawned.is_err() {
            break;
        }
    }
    drop(send);
    let deadline = Instant::now() + timeout * CRAWL_ROUNDS;
    while let Ok((index, answer)) =
        receive.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        answers[index] = Some(answer);
    }
    // Out of time: the checks not started yet are not made.
    queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
    answers
}

/// The peers P2Pool dials at this start: none when `standalone`. Otherwise
/// all of `user_peers` first, then the seeds and known peers that answer a
/// check now, fastest first, up to [`MAX_DIAL`] peers in all; when nothing
/// answers, every one of them anyway (up to that many), so P2Pool keeps
/// trying them. Each check is saved in the known-peers file.
pub fn dial_list(
    network: MiningNetwork,
    user_peers: &[String],
    standalone: bool,
    home: &Path,
) -> Vec<String> {
    if standalone {
        return Vec::new();
    }
    dial_from(
        seeds(network),
        user_peers,
        &file(network, home),
        CHECK_TIMEOUT,
    )
}

fn dial_from(seeds: &[&str], user_peers: &[String], file: &Path, timeout: Duration) -> Vec<String> {
    let yours = distinct(user_peers.iter().map(String::as_str));
    let configured = distinct(
        yours
            .iter()
            .map(String::as_str)
            .chain(seeds.iter().copied()),
    );
    let now = unix_now();
    let before = load(file);
    let mut kept = before.clone();
    tidy(&mut kept, &configured, now);
    let candidates = distinct(
        configured
            .iter()
            .chain(
                kept.iter()
                    .filter(|peer| peer.learned)
                    .map(|peer| &peer.address),
            )
            .map(String::as_str),
    );
    let answers = crawl(&candidates, timeout);
    for (address, answer) in candidates.iter().zip(&answers) {
        let Some(answer) = answer else {
            continue;
        };
        let peer = entry(&mut kept, address, false, now);
        peer.checked = Some(now);
        peer.millis = match answer {
            Answer::Took(time) => {
                peer.reached = Some(now);
                Some(u64::try_from(time.as_millis()).unwrap_or(u64::MAX))
            }
            Answer::Silent => None,
        };
    }
    tidy(&mut kept, &configured, now);
    if kept != before {
        // A memory only: a failed write forgets this check, nothing more.
        let _ = save(file, &kept);
    }
    let others = candidates
        .iter()
        .zip(&answers)
        .filter(|(address, _)| !yours.contains(address));
    let mut answered: Vec<(Duration, &String)> = others
        .clone()
        .filter_map(|(address, answer)| match answer {
            Some(Answer::Took(time)) => Some((*time, address)),
            _ => None,
        })
        .collect();
    answered.sort_by_key(|(time, _)| *time);
    let nothing_answered = !answers
        .iter()
        .any(|answer| matches!(answer, Some(Answer::Took(_))));
    let rest: Vec<&String> = if nothing_answered {
        others.map(|(address, _)| address).collect()
    } else {
        answered.into_iter().map(|(_, address)| address).collect()
    };
    let mut dial = yours;
    let room = MAX_DIAL.saturating_sub(dial.len());
    dial.extend(rest.into_iter().take(room).cloned());
    dial
}

/// Remembers, while P2Pool runs, the peers it dialed out to (from its
/// API's `GET /peers`), so the next start dials them again.
#[derive(Debug, Clone)]
pub struct Learner {
    file: PathBuf,
    configured: Vec<String>,
}

/// What one run of P2Pool has saved so far.
#[derive(Debug, Default)]
pub struct Learned {
    saved: Vec<String>,
    at: Option<Instant>,
}

impl Learner {
    pub fn new(network: MiningNetwork, user_peers: &[String], home: &Path) -> Self {
        Self {
            file: file(network, home),
            configured: distinct(
                user_peers
                    .iter()
                    .map(String::as_str)
                    .chain(seeds(network).iter().copied()),
            ),
        }
    }

    /// Saves `outbound` (P2Pool's outbound peers now, seeds and yours left
    /// out) when one of them is new to this run or [`RESAVE`] has passed.
    pub fn learn(&self, outbound: &[String], learned: &mut Learned) {
        let fresh: Vec<String> = distinct(outbound.iter().map(String::as_str))
            .into_iter()
            .filter_map(|address| dial_address(&address))
            .filter(|address| !self.configured.contains(address))
            .collect();
        let due = learned.at.is_none_or(|at| at.elapsed() >= RESAVE);
        if fresh.is_empty() || (!due && fresh.iter().all(|address| learned.saved.contains(address)))
        {
            return;
        }
        let now = unix_now();
        let mut peers = load(&self.file);
        for address in &fresh {
            let peer = entry(&mut peers, address, true, now);
            peer.learned = true;
            peer.reached = Some(now);
        }
        tidy(&mut peers, &self.configured, now);
        if save(&self.file, &peers).is_ok() {
            learned.at = Some(Instant::now());
            for address in fresh {
                if !learned.saved.contains(&address) {
                    learned.saved.push(address);
                }
            }
        }
    }
}

/// The three lists with each entry's last check, for `pickaxe p2pool
/// peers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub seeds: Vec<Known>,
    pub yours: Vec<Known>,
    pub known: Vec<Known>,
}

pub fn listing(network: MiningNetwork, user_peers: &[String], home: &Path) -> Listing {
    let yours = distinct(user_peers.iter().map(String::as_str));
    let seeds = distinct(seeds(network).iter().copied());
    let configured = distinct(yours.iter().chain(&seeds).map(String::as_str));
    let mut kept = load(&file(network, home));
    tidy(&mut kept, &configured, unix_now());
    let row = |address: &String| {
        kept.iter()
            .find(|peer| peer.address == *address)
            .cloned()
            .unwrap_or_else(|| Known::new(address, false, 0))
    };
    Listing {
        seeds: seeds.iter().map(row).collect(),
        yours: yours.iter().map(row).collect(),
        known: kept.iter().filter(|peer| peer.learned).cloned().collect(),
    }
}

/// How long ago `then` was, in words.
fn ago(now: u64, then: u64) -> String {
    match now.saturating_sub(then) {
        0..=59 => "just now".into(),
        secs @ 60..=3_599 => format!("{} min ago", secs / 60),
        secs @ 3_600..=86_399 => format!("{} h ago", secs / 3_600),
        secs => format!("{} d ago", secs / 86_400),
    }
}

/// An entry's last check in words, and when it last answered if that was
/// not then.
pub fn describe(peer: &Known, now: u64) -> String {
    let mut text = match (peer.checked, peer.millis) {
        (None, _) => "not checked yet".to_owned(),
        (Some(at), Some(millis)) => format!("answered in {millis} ms, {}", ago(now, at)),
        (Some(at), None) => format!("no answer, {}", ago(now, at)),
    };
    if let Some(reached) = peer.reached {
        if peer.millis.is_none() || peer.checked.is_none_or(|at| reached > at) {
            text.push_str(&format!(" (last reached {})", ago(now, reached)));
        }
    }
    text
}

/// One line of counts for `pickaxe p2pool status`.
pub fn summary(network: MiningNetwork, user_peers: &[String], home: &Path) -> String {
    let listing = listing(network, user_peers, home);
    let mut parts = vec![
        match listing.seeds.len() {
            0 => "no seeds yet".to_owned(),
            1 => "1 seed".to_owned(),
            count => format!("{count} seeds"),
        },
        format!("{} known", listing.known.len()),
        format!("{} of yours", listing.yours.len()),
    ];
    let all: Vec<&Known> = listing
        .seeds
        .iter()
        .chain(&listing.yours)
        .chain(&listing.known)
        .collect();
    if let Some(last) = all.iter().filter_map(|peer| peer.checked).max() {
        let checked = all.iter().filter(|peer| peer.checked == Some(last)).count();
        let answered = all
            .iter()
            .filter(|peer| peer.checked == Some(last) && peer.millis.is_some())
            .count();
        parts.push(format!(
            "last check {}: {answered} of {checked} answered",
            ago(unix_now(), last)
        ));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::super::test_dir;
    use super::*;
    use std::net::TcpListener;

    /// Short enough for tests; a closed port on Windows takes about 2 s to
    /// refuse, so it times out instead.
    const TIMEOUT: Duration = Duration::from_millis(1_000);

    fn open() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, format!("/ip4/127.0.0.1/tcp/{port}"))
    }

    fn closed() -> String {
        open().1
    }

    fn learned(address: &str, reached: u64) -> Known {
        Known {
            reached: Some(reached),
            ..Known::new(address, true, reached)
        }
    }

    #[test]
    fn standalone_dials_nobody_and_checks_nothing() {
        let home = test_dir("peers-standalone");
        let (_listener, address) = open();
        let yours = std::slice::from_ref(&address);
        assert!(dial_list(MiningNetwork::Chipnet, yours, true, &home).is_empty());
        assert!(!file(MiningNetwork::Chipnet, &home).exists());
        // Linked, the same peer is dialed and its check kept.
        assert_eq!(
            dial_list(MiningNetwork::Chipnet, yours, false, &home),
            yours
        );
        let kept = load(&file(MiningNetwork::Chipnet, &home));
        assert_eq!(kept.len(), 1);
        assert!(!kept[0].learned && kept[0].millis.is_some());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn your_peers_come_first_then_seeds_and_known_peers_that_answer() {
        let home = test_dir("peers-order");
        let path = home.join("peers.json");
        let now = unix_now();
        let (_seed, seed) = open();
        let (_known, known) = open();
        let (silent_seed, silent_known, yours) = (closed(), closed(), closed());
        save(
            &path,
            &[learned(&silent_known, now), learned(&known, now - 10)],
        )
        .unwrap();
        let dial = dial_from(
            &[silent_seed.as_str(), seed.as_str()],
            std::slice::from_ref(&yours),
            &path,
            TIMEOUT,
        );
        // Yours first even though it is silent; then only what answered.
        assert_eq!(dial[0], yours);
        assert_eq!(dial.len(), 3, "{dial:?}");
        assert!(dial.contains(&seed) && dial.contains(&known));
        let kept = load(&path);
        let of = |address: &str| kept.iter().find(|peer| peer.address == address).unwrap();
        assert!(of(&known).learned && of(&known).millis.is_some());
        assert!(of(&silent_known).checked.is_some() && of(&silent_known).millis.is_none());
        assert!(!of(&seed).learned && of(&seed).millis.is_some());
        assert!(of(&yours).checked.is_some() && of(&yours).millis.is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_dial_list_stops_at_its_cap_but_keeps_all_of_yours() {
        let home = test_dir("peers-cap");
        let path = home.join("peers.json");
        let now = unix_now();
        let listeners: Vec<_> = (0..MAX_DIAL + 3).map(|_| open()).collect();
        let known: Vec<Known> = listeners
            .iter()
            .map(|(_, address)| learned(address, now))
            .collect();
        save(&path, &known).unwrap();
        let yours = closed();
        let dial = dial_from(&[], std::slice::from_ref(&yours), &path, TIMEOUT);
        assert_eq!(dial.len(), MAX_DIAL);
        assert_eq!(dial[0], yours);
        // Ten of yours: all of them, and nothing else.
        let many: Vec<String> = (0..MAX_DIAL + 2).map(|_| closed()).collect();
        let dial = dial_from(&[], &many, &path, TIMEOUT);
        assert_eq!(dial, many);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn when_nothing_answers_every_configured_peer_is_dialed_anyway() {
        let home = test_dir("peers-silent");
        let path = home.join("peers.json");
        let (yours, seed, known) = (closed(), closed(), closed());
        save(&path, &[learned(&known, unix_now())]).unwrap();
        assert_eq!(
            dial_from(
                &[seed.as_str()],
                std::slice::from_ref(&yours),
                &path,
                TIMEOUT
            ),
            vec![yours, seed, known]
        );
        // Nothing at all: nothing to dial.
        assert!(dial_from(&[], &[], &home.join("none.json"), TIMEOUT).is_empty());
        assert!(!home.join("none.json").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn dns_names_are_looked_up_to_check_and_dialed_as_written() {
        let home = test_dir("peers-dns");
        let path = home.join("peers.json");
        let (listener, _) = open();
        let port = listener.local_addr().unwrap().port();
        let (other, _) = open();
        let other_port = other.local_addr().unwrap().port();
        let yours = format!("/dns4/localhost/tcp/{port}");
        let known = format!("/dns/localhost/tcp/{other_port}");
        save(&path, &[learned(&known, unix_now())]).unwrap();
        assert_eq!(
            dial_from(&[], std::slice::from_ref(&yours), &path, TIMEOUT),
            vec![yours.clone(), known.clone()]
        );
        assert!(load(&path).iter().all(|peer| peer.millis.is_some()));
        // A name that does not resolve is silent and still dialed as written.
        let missing = "/dns4/pickaxe-test.invalid/tcp/6884".to_owned();
        drop((listener, other));
        assert_eq!(check(&missing, TIMEOUT), Answer::Silent);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn addresses_are_p2p_tcp_multiaddrs() {
        for good in [
            "/ip4/203.0.113.7/tcp/6884",
            "/ip6/2001:db8::7/tcp/6884",
            "/dns4/pool.example.com/tcp/6884",
            "/dns6/pool.example.com/tcp/6884",
            "/dns/pool.example.com/tcp/6884",
            "/ip4/203.0.113.7/tcp/6884/p2p/12D3KooWExample",
        ] {
            assert!(is_dial_address(good), "{good}");
        }
        for bad in [
            "203.0.113.7:6884",
            "/ip4/203.0.113.7/udp/6884",
            "/ip4/203.0.113.7/tcp/0",
            "/ip4/pool.example.com/tcp/6884",
            "/ip4/203.0.113.7/tcp/6884/ws",
            "/dns4//tcp/6884",
            "",
        ] {
            assert!(!is_dial_address(bad), "{bad}");
        }
        assert_eq!(
            dial_address(" /ip4/203.0.113.7/tcp/6884/p2p/12D3KooWExample ").as_deref(),
            Some("/ip4/203.0.113.7/tcp/6884")
        );
    }

    #[test]
    fn the_file_round_trips_ages_out_silent_peers_and_keeps_64() {
        let home = test_dir("peers-file");
        let path = home.join("peers.json");
        let now = unix_now();
        let day = 24 * 60 * 60;
        let peers = vec![
            Known {
                checked: Some(now),
                millis: Some(12),
                ..learned("/ip4/203.0.113.1/tcp/6884", now)
            },
            Known::new("/dns4/pool.example.com/tcp/6884", false, now),
        ];
        save(&path, &peers).unwrap();
        assert_eq!(load(&path), peers);
        // Only addresses and times are written.
        let text = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys: Vec<&str> = json["peers"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["added", "address", "checked", "learned", "millis", "reached"]
        );
        // A week of silence drops a learned peer; a configured one stays,
        // a record of one no longer configured goes.
        let mut aging = vec![
            learned("/ip4/203.0.113.2/tcp/6884", now - 8 * day),
            learned("/ip4/203.0.113.3/tcp/6884", now - 6 * day),
            Known::new("/ip4/203.0.113.4/tcp/6884", false, now - 30 * day),
            Known::new("/ip4/203.0.113.5/tcp/6884", false, now),
            learned("/ip4/203.0.113.3/tcp/6884", now),
        ];
        tidy(&mut aging, &["/ip4/203.0.113.4/tcp/6884".to_owned()], now);
        let addresses: Vec<&str> = aging.iter().map(|peer| peer.address.as_str()).collect();
        assert_eq!(
            addresses,
            ["/ip4/203.0.113.4/tcp/6884", "/ip4/203.0.113.3/tcp/6884"]
        );
        // The cap keeps the most recently reached.
        let mut many: Vec<Known> = (0..FILE_CAP as u64 + 6)
            .map(|index| learned(&format!("/ip4/198.51.100.{index}/tcp/6884"), now - index))
            .collect();
        tidy(&mut many, &[], now);
        assert_eq!(many.len(), FILE_CAP);
        assert_eq!(many[0].address, "/ip4/198.51.100.0/tcp/6884");
        assert_eq!(
            many[FILE_CAP - 1].address,
            format!("/ip4/198.51.100.{}/tcp/6884", FILE_CAP - 1)
        );
        // A broken file is an empty memory.
        std::fs::write(&path, "{").unwrap();
        assert!(load(&path).is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn outbound_peers_from_p2pools_api_are_learned_without_their_ids() {
        let home = test_dir("peers-learn");
        let answer = serde_json::json!([
            {"peer_id": "12D3KooWA", "ip": "203.0.113.7", "address": "/ip4/203.0.113.7/tcp/6884/p2p/12D3KooWA", "direction": "Outbound", "connected_secs": 5},
            {"peer_id": "12D3KooWB", "ip": "198.51.100.2", "address": "/ip4/198.51.100.2/tcp/53122", "direction": "Inbound", "connected_secs": 9},
            {"peer_id": "12D3KooWC", "ip": null, "address": "/dns4/pool.example.com/tcp/6884", "direction": "Outbound", "connected_secs": 1},
            {"peer_id": "12D3KooWD", "ip": "203.0.113.8", "address": "/ip4/203.0.113.8/tcp/6884", "direction": "Outbound", "connected_secs": 1}
        ]);
        let outbound = outbound(&answer);
        assert_eq!(
            outbound,
            [
                "/ip4/203.0.113.7/tcp/6884",
                "/dns4/pool.example.com/tcp/6884",
                "/ip4/203.0.113.8/tcp/6884"
            ]
        );
        // Yours (the last) is not learned: it is dialed as yours already.
        let learner = Learner::new(
            MiningNetwork::Chipnet,
            &["/ip4/203.0.113.8/tcp/6884".into()],
            &home,
        );
        let mut learned = Learned::default();
        learner.learn(&outbound, &mut learned);
        let kept = load(&file(MiningNetwork::Chipnet, &home));
        let addresses: Vec<&str> = kept.iter().map(|peer| peer.address.as_str()).collect();
        assert_eq!(
            addresses,
            [
                "/ip4/203.0.113.7/tcp/6884",
                "/dns4/pool.example.com/tcp/6884"
            ]
        );
        assert!(kept
            .iter()
            .all(|peer| peer.learned && peer.reached.is_some()));
        // The same peers again within the hour write nothing.
        std::fs::remove_file(file(MiningNetwork::Chipnet, &home)).unwrap();
        learner.learn(&outbound, &mut learned);
        assert!(!file(MiningNetwork::Chipnet, &home).exists());
        let listing = listing(
            MiningNetwork::Chipnet,
            &["/ip4/203.0.113.8/tcp/6884".into()],
            &home,
        );
        assert!(listing.seeds.is_empty() && listing.known.is_empty());
        assert_eq!(listing.yours.len(), 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn checks_read_as_words() {
        let now = 1_000_000;
        let mut peer = Known::new("/ip4/203.0.113.7/tcp/6884", true, now - 100);
        assert_eq!(describe(&peer, now), "not checked yet");
        peer.reached = Some(now - 7_200);
        assert_eq!(
            describe(&peer, now),
            "not checked yet (last reached 2 h ago)"
        );
        peer.checked = Some(now - 120);
        peer.millis = Some(42);
        peer.reached = Some(now - 120);
        assert_eq!(describe(&peer, now), "answered in 42 ms, 2 min ago");
        peer.millis = None;
        peer.reached = Some(now - 3 * 86_400);
        assert_eq!(
            describe(&peer, now),
            "no answer, 2 min ago (last reached 3 d ago)"
        );
        assert!(SEEDS_MAINNET
            .iter()
            .chain(SEEDS_CHIPNET)
            .all(|seed| is_dial_address(seed)));
    }
}
