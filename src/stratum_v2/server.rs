//! #### PR #38
//! Bounded native SV2 server. One worker owns the node/template and submissions;
//! device connections only see immutable jobs and cannot select chain payouts.

use super::{
    channel::ValidatedShare,
    provider::{NodeRpc, TemplateProvider},
    template::{BchTemplate, Hash},
    transport::Session,
    wire::MiningSession,
};
use crate::config::{validate_payout_address, MiningNetwork};
use std::{
    io,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stratum_core::bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey};

const MAX_CONNECTIONS: usize = 64;

// No Debug: contains a local authority secret and payout configuration.
pub struct ServerConfig {
    pub network: MiningNetwork,
    pub payout: String,
    pub authority_secret: [u8; 32],
    pub share_target: Hash,
}

#[derive(Default, Clone, Debug)]
pub struct ServerStats {
    pub connections: usize,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub blocks_accepted: u64,
    pub blocks_unconfirmed: u64,
    pub connection_errors: u64,
    pub template_ready: bool,
    pub height: Option<u32>,
}

#[derive(Clone)]
struct PublishedJob {
    generation: u64,
    template: Arc<BchTemplate>,
    valid_until: Instant,
}

struct Shared {
    job: RwLock<Option<PublishedJob>>,
    stats: Arc<Mutex<ServerStats>>,
    blocks: SyncSender<ValidatedShare>,
    stop: Arc<AtomicBool>,
}

/// Bind the listener before calling this function. The caller owns the stop
/// flag and display, so native TUI and CPU tests use the same server lifecycle.
pub fn run<R: NodeRpc + Send + 'static>(
    listener: TcpListener,
    rpc: R,
    config: ServerConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    validate_payout_address(config.network, &config.payout)
        .map_err(|_| "invalid payout for selected network")?;
    if config.share_target == [0; 32] {
        return Err("share target cannot be zero".into());
    }
    let public = authority_public(&config.authority_secret)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot configure mining listener")?;
    let (blocks, receive_blocks) = mpsc::sync_channel::<ValidatedShare>(MAX_CONNECTIONS);
    let shared = Arc::new(Shared {
        job: RwLock::new(None),
        stats: stats.clone(),
        blocks,
        stop: stop.clone(),
    });
    let node_shared = shared.clone();
    let network = config.network;
    let node = thread::spawn(move || {
        let mut provider = TemplateProvider::new(rpc, network);
        let mut refreshed = Instant::now() - Duration::from_secs(60);
        while !node_shared.stop.load(Ordering::Relaxed) {
            match receive_blocks.recv_timeout(Duration::from_millis(250)) {
                Ok(share) => {
                    let accepted = provider
                        .submit(share.generation, &share.coinbase, share.header)
                        .is_ok();
                    if let Ok(mut stats) = node_shared.stats.lock() {
                        if accepted {
                            stats.blocks_accepted = stats.blocks_accepted.saturating_add(1);
                        } else {
                            stats.blocks_unconfirmed = stats.blocks_unconfirmed.saturating_add(1);
                        }
                    }
                    // Refresh even if RPC acceptance is unknown. Do not count a
                    // lost RPC response as an accepted block or keep old work.
                    refreshed = Instant::now() - Duration::from_secs(60);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let current = provider.tip_is_current().unwrap_or(false);
            if !current || refreshed.elapsed() >= Duration::from_secs(15) {
                match provider.refresh() {
                    Ok((generation, template)) => {
                        publish(
                            &node_shared,
                            Some(PublishedJob {
                                generation,
                                template: Arc::new(template.clone()),
                                valid_until: Instant::now() + Duration::from_secs(30),
                            }),
                        );
                        refreshed = Instant::now();
                    }
                    Err(_) => publish(&node_shared, None),
                }
            }
        }
        publish(&node_shared, None);
    });
    let config = Arc::new(config);
    let mut devices: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut listener_error = None;
    while !stop.load(Ordering::Relaxed) {
        if node.is_finished() {
            listener_error = Some("template worker stopped unexpectedly".to_owned());
            break;
        }
        let mut remaining = Vec::new();
        for device in devices.drain(..) {
            if device.is_finished() {
                if device.join().is_err() {
                    connection_error(&shared);
                }
            } else {
                remaining.push(device);
            }
        }
        devices = remaining;
        match listener.accept() {
            Ok((stream, _)) => {
                if devices.len() >= MAX_CONNECTIONS {
                    drop(stream);
                    continue;
                }
                let shared = shared.clone();
                let config = config.clone();
                devices.push(thread::spawn(move || {
                    if let Ok(mut stats) = shared.stats.lock() {
                        stats.connections += 1;
                    }
                    if serve_device(stream, public, &config, &shared).is_err() {
                        connection_error(&shared);
                    }
                    if let Ok(mut stats) = shared.stats.lock() {
                        stats.connections = stats.connections.saturating_sub(1);
                    }
                }));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(_) => {
                listener_error = Some("mining listener failed".to_owned());
                break;
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    for device in devices {
        if device.join().is_err() {
            connection_error(&shared);
        }
    }
    node.join()
        .map_err(|_| "template worker stopped unexpectedly")?;
    if let Some(error) = listener_error {
        Err(error)
    } else {
        Ok(())
    }
}

pub fn authority_public(secret: &[u8; 32]) -> Result<[u8; 32], String> {
    let key = SecretKey::from_slice(secret).map_err(|_| "invalid SV2 authority secret")?;
    Ok(Keypair::from_secret_key(&Secp256k1::new(), &key)
        .x_only_public_key()
        .0
        .serialize())
}

fn connection_error(shared: &Shared) {
    if let Ok(mut stats) = shared.stats.lock() {
        stats.connection_errors = stats.connection_errors.saturating_add(1);
    }
}

fn publish(shared: &Shared, job: Option<PublishedJob>) {
    if let Ok(mut stats) = shared.stats.lock() {
        stats.template_ready = job.is_some();
        stats.height = job.as_ref().map(|job| job.template.height);
    }
    if let Ok(mut current) = shared.job.write() {
        *current = job;
    }
}

fn serve_device(
    stream: TcpStream,
    public: [u8; 32],
    config: &ServerConfig,
    shared: &Shared,
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
        let mut generation = None;
        let mut accepted = 0u64;
        let mut rejected = 0u64;
        while !shared.stop.load(Ordering::Relaxed) {
            let current = shared
                .job
                .read()
                .map_err(|_| "template state unavailable")?
                .clone();
            match current {
                Some(job) if Instant::now() < job.valid_until => {
                    if generation != Some(job.generation) {
                        let id = u32::try_from(job.generation)
                            .map_err(|_| "job identifiers exhausted")?;
                        for frame in mining.set_job(id, job.generation, job.template)? {
                            sender.send(frame)?;
                        }
                        generation = Some(job.generation);
                    }
                }
                _ if generation.is_some() => {
                    return Err("template source unavailable; reconnect when healthy".into())
                }
                _ => (),
            }
            if let Some(frame) = receiver.receive(Duration::from_millis(100))? {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "invalid system clock")?
                    .as_secs();
                let now = u32::try_from(now).map_err(|_| "system time exceeds header range")?;
                let responses = mining.receive(frame, now)?;
                // Submit candidate blocks before sending acknowledgements. A
                // full queue is an error, never a silently dropped block.
                for block in responses.blocks {
                    shared
                        .blocks
                        .try_send(block)
                        .map_err(|_| "block submission queue unavailable")?;
                }
                for frame in responses.frames {
                    sender.send(frame)?;
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
                }
                accepted = next_accepted;
                rejected = next_rejected;
            }
        }
        Ok(())
    })();
    sender.close();
    receiver.close();
    result
}
