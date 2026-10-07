//! #### PR #38
//! Per-connection estimates from validated work, never advertised hardware speed.
//! Socket addresses only join the in-process SV1 adapter to its SV2 connection;
//! public snapshots contain generated labels, not worker identities or payouts.

use super::template::Hash;
use num_traits::ToPrimitive;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::SocketAddr,
    time::{Duration, Instant},
};

const WINDOW: u64 = 300;
const WARMUP: u64 = 30;
const RECENT_CLOSED: usize = 64;

#[derive(Clone, Copy)]
pub enum ShareEvent {
    Accepted(Hash),
    Rejected(&'static str),
}

pub fn expected_hashes(target: &Hash) -> f64 {
    let denominator = num_bigint::BigUint::from_bytes_le(target) + 1u8;
    2f64.powi(256) / denominator.to_f64().expect("256-bit target fits f64")
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceSnapshot {
    pub label: String,
    pub protocol: &'static str,
    pub connected: bool,
    pub channels: usize,
    pub accepted: u64,
    pub rejected: u64,
    pub adapter_rejected: u64,
    pub last_rejection: Option<&'static str>,
    pub hashrate_estimate: Option<f64>,
    pub estimate_seconds: f64,
    pub last_share_seconds: Option<u64>,
    pub connection_error: Option<&'static str>,
    pub adapter_error: Option<&'static str>,
}

#[derive(Clone, Debug)]
struct Device {
    label: String,
    protocol: &'static str,
    started: Instant,
    ended: Option<Instant>,
    channels: usize,
    accepted: u64,
    rejected: u64,
    adapter_rejected: u64,
    last_rejection: Option<&'static str>,
    last_share: Option<Instant>,
    // At most one aggregate per second, independent of incoming share rate.
    work: VecDeque<(u64, f64)>,
    connection_error: Option<&'static str>,
    adapter_error: Option<&'static str>,
}

#[derive(Clone)]
pub struct Devices {
    prefix: u32,
    next_id: u64,
    sockets: HashMap<SocketAddr, u64>,
    rows: BTreeMap<u64, Device>,
}

impl std::fmt::Debug for Devices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Devices")
            .field("rows", &self.rows.len())
            .finish()
    }
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            prefix: rand::random(),
            next_id: 0,
            sockets: HashMap::new(),
            rows: BTreeMap::new(),
        }
    }
}

impl Devices {
    pub fn connect(&mut self, socket: SocketAddr, adapter: bool, now: Instant) -> u64 {
        if let Some(id) = self.sockets.get(&socket) {
            if adapter {
                self.rows.get_mut(id).unwrap().protocol = "SV1";
            }
            return *id;
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("device session identifiers exhausted");
        let id = self.next_id;
        self.sockets.insert(socket, id);
        self.rows.insert(
            id,
            Device {
                label: format!("Device {:08x}-{id}", self.prefix),
                protocol: if adapter { "SV1" } else { "SV2" },
                started: now,
                ended: None,
                channels: 0,
                accepted: 0,
                rejected: 0,
                adapter_rejected: 0,
                last_rejection: None,
                last_share: None,
                work: VecDeque::new(),
                connection_error: None,
                adapter_error: None,
            },
        );
        self.prune();
        id
    }

    pub fn channels(&mut self, id: u64, count: usize) {
        if let Some(row) = self.rows.get_mut(&id) {
            row.channels = count;
        }
    }

    pub fn share(&mut self, id: u64, event: ShareEvent, adapter: bool, now: Instant) {
        let Some(row) = self.rows.get_mut(&id) else {
            return;
        };
        match event {
            ShareEvent::Accepted(target) => {
                row.accepted = row.accepted.saturating_add(1);
                row.last_share = Some(now);
                let second = now.saturating_duration_since(row.started).as_secs();
                while row
                    .work
                    .front()
                    .is_some_and(|(t, _)| t.saturating_add(WINDOW) <= second)
                {
                    row.work.pop_front();
                }
                let hashes = expected_hashes(&target);
                if let Some((_, total)) = row.work.back_mut().filter(|(t, _)| *t == second) {
                    *total += hashes;
                } else {
                    row.work.push_back((second, hashes));
                }
            }
            ShareEvent::Rejected(reason) => {
                row.rejected = row.rejected.saturating_add(1);
                row.last_rejection = Some(reason);
                if adapter {
                    row.adapter_rejected = row.adapter_rejected.saturating_add(1);
                }
            }
        }
    }

    pub fn close(&mut self, id: u64, adapter: bool, error: Option<&str>, now: Instant) {
        let Some(row) = self.rows.get_mut(&id) else {
            return;
        };
        row.ended.get_or_insert(now);
        if adapter {
            row.adapter_error = error.map(connection_reason);
        } else {
            row.connection_error = error.map(connection_reason);
        }
        self.sockets.retain(|_, existing| *existing != id);
        self.prune();
    }

    fn prune(&mut self) {
        let closed: Vec<_> = self
            .rows
            .iter()
            .filter(|(_, r)| r.ended.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in closed
            .iter()
            .take(closed.len().saturating_sub(RECENT_CLOSED))
        {
            self.rows.remove(id);
        }
    }

    pub fn snapshots(&self, now: Instant) -> Vec<DeviceSnapshot> {
        let mut rows: Vec<_> = self
            .rows
            .values()
            .map(|row| {
                let elapsed = now.saturating_duration_since(row.started);
                let seconds = elapsed.as_secs_f64().min(WINDOW as f64);
                let second = elapsed.as_secs();
                let work: f64 = row
                    .work
                    .iter()
                    .filter(|(t, _)| t.saturating_add(WINDOW) > second)
                    .map(|(_, w)| w)
                    .sum();
                DeviceSnapshot {
                    label: row.label.clone(),
                    protocol: row.protocol,
                    connected: row.ended.is_none(),
                    channels: row.channels,
                    accepted: row.accepted,
                    rejected: row.rejected,
                    adapter_rejected: row.adapter_rejected,
                    last_rejection: row.last_rejection,
                    hashrate_estimate: if row.ended.is_some() {
                        Some(0.0)
                    } else if elapsed >= Duration::from_secs(WARMUP) {
                        Some(work / seconds)
                    } else {
                        None
                    },
                    estimate_seconds: seconds,
                    last_share_seconds: row
                        .last_share
                        .map(|t| now.saturating_duration_since(t).as_secs()),
                    connection_error: row.connection_error,
                    adapter_error: row.adapter_error,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.label.cmp(&b.label))
        });
        rows
    }
}

// Only static categories leave this module; unknown upstream text may contain
// an address, credentials or untrusted control characters.
pub fn connection_reason(error: &str) -> &'static str {
    match error {
        "SV1 disconnected" => "device disconnected",
        "SV2 peer disconnected"
        | "SV2 peer disconnected during frame"
        | "SV2 peer disconnected during write"
        | "SV2 connection is closed" => "peer disconnected",
        "SV1 setup timed out" | "SV2 setup timed out" | "SV2 channel timed out" => {
            "setup timed out"
        }
        "SV2 share response timed out" => "share response timed out",
        "SV1 line timed out"
        | "SV2 frame deadline exceeded"
        | "SV2 frame read failed or timed out" => "message timed out or read failed",
        "SV1 read failed"
        | "SV1 write failed"
        | "SV2 receive failed"
        | "SV2 frame write failed or timed out" => "connection I/O failed",
        "template source unavailable; reconnect when healthy" => "template unavailable",
        "job time precedes active parent" => "job time precedes active parent",
        "cannot persist solved block; mining stopped" => "block storage failed",
        "SV2 authentication or framing failed"
        | "SV2 handshake failed"
        | "SV2 authority authentication failed" => "authentication or framing failed",
        "device worker panicked" => "device worker stopped unexpectedly",
        _ => "protocol or connection failure",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_weight_validated_targets_and_decay_when_work_stops() {
        let start = Instant::now();
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:1234".parse().unwrap(), false, start);
        let mut twice = [255; 32];
        twice[31] = 127;
        assert_eq!(expected_hashes(&[255; 32]), 1.0);
        assert_eq!(expected_hashes(&twice), 2.0);
        for second in 1..=30 {
            devices.share(
                id,
                ShareEvent::Accepted(if second <= 15 { [255; 32] } else { twice }),
                false,
                start + Duration::from_secs(second),
            );
        }
        devices.share(
            id,
            ShareEvent::Rejected("difficulty-too-low"),
            false,
            start + Duration::from_secs(30),
        );
        assert!(devices.snapshots(start + Duration::from_secs(29))[0]
            .hashrate_estimate
            .is_none());
        let row = devices.snapshots(start + Duration::from_secs(30)).remove(0);
        assert_eq!(row.hashrate_estimate, Some(1.5));
        assert_eq!((row.accepted, row.rejected), (30, 1));
        assert_eq!(row.last_share_seconds, Some(0));
        assert_eq!(
            devices.snapshots(start + Duration::from_secs(331))[0].hashrate_estimate,
            Some(0.0)
        );
        // Hundreds of shares in a second still occupy one bounded bucket.
        for _ in 0..1000 {
            devices.share(
                id,
                ShareEvent::Accepted(twice),
                false,
                start + Duration::from_secs(332),
            );
        }
        assert_eq!(devices.rows[&id].work.len(), 1);
        assert_eq!(
            devices.snapshots(start + Duration::from_secs(332))[0].hashrate_estimate,
            Some(2000.0 / 300.0)
        );
    }

    #[test]
    fn adapter_and_native_socket_share_one_row_but_reconnect_gets_a_new_label() {
        for adapter_first in [true, false] {
            let now = Instant::now();
            let peer = "127.0.0.1:4321".parse().unwrap();
            let mut devices = Devices::default();
            let id = devices.connect(peer, adapter_first, now);
            assert_eq!(devices.connect(peer, !adapter_first, now), id);
            devices.share(id, ShareEvent::Rejected("stale job"), true, now);
            let row = devices.snapshots(now).remove(0);
            assert_eq!(row.protocol, "SV1");
            assert_eq!(
                (row.accepted, row.rejected, row.adapter_rejected),
                (0, 1, 1)
            );
            devices.close(id, true, Some("job time precedes active parent"), now);
            devices.close(id, false, Some("SV2 peer disconnected"), now);
            let closed = devices.snapshots(now).remove(0);
            assert_eq!(
                closed.adapter_error,
                Some("job time precedes active parent")
            );
            assert_eq!(closed.connection_error, Some("peer disconnected"));
            assert!(!closed.connected);
            let next = devices.connect(peer, false, now);
            assert_ne!(next, id);
            assert_ne!(devices.snapshots(now)[0].label, row.label);
            assert_eq!(devices.snapshots(now).len(), 2);
        }
    }

    #[test]
    fn closed_history_is_bounded_and_public_output_excludes_private_connection_text() {
        let now = Instant::now();
        let peer = "192.0.2.7:4321".parse().unwrap();
        let mut devices = Devices::default();
        for _ in 0..1000 {
            let id = devices.connect(peer, false, now);
            devices.close(
                id,
                false,
                Some("private credentials must never appear"),
                now,
            );
        }
        assert_eq!(devices.rows.len(), RECENT_CLOSED);
        assert!(devices.sockets.is_empty());
        let json = serde_json::to_string(&devices.snapshots(now)).unwrap();
        assert!(!json.contains("private credentials"));
        assert!(!json.contains("192.0.2.7"));
        assert!(!format!("{devices:?}").contains("192.0.2.7"));
    }
}
