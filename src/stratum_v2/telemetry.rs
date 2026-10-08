//! #### PR #38
//! Per-connection estimates from validated work, never advertised hardware speed.
//! Socket addresses only join the in-process SV1 adapter to its SV2 connection;
//! public snapshots contain generated labels, not worker identities or payouts.

use super::{device_api::DeviceReport, template::Hash};
use num_traits::ToPrimitive;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

const WINDOW: u64 = 300;
/// The longer window, kept in one-minute buckets.
const HOUR: u64 = 3600;
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
    /// The same estimate over up to an hour.
    pub hashrate_hour: Option<f64>,
    /// Difficulty of the last accepted share's target.
    pub difficulty: Option<f64>,
    pub estimate_seconds: f64,
    pub last_share_seconds: Option<u64>,
    pub connection_error: Option<&'static str>,
    pub adapter_error: Option<&'static str>,
    /// What the device itself reports, when it answers a read-only query.
    pub reported_hashrate: Option<f64>,
    pub temperature_c: Option<f64>,
    pub fan: Option<String>,
    /// #### PR #40
    /// Make and model, firmware and power draw, from asic-rs's report.
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub power_w: Option<f64>,
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
    // At most one aggregate per minute, for the hour estimate.
    hour: VecDeque<(u64, f64)>,
    difficulty: Option<f64>,
    connection_error: Option<&'static str>,
    adapter_error: Option<&'static str>,
    report: Option<DeviceReport>,
}

#[derive(Clone)]
pub struct Devices {
    prefix: u32,
    next_id: u64,
    sockets: HashMap<SocketAddr, u64>,
    rows: BTreeMap<u64, Device>,
    // Device LAN addresses, used only to query the device itself; never shown
    // or written to JSON.
    addresses: HashMap<u64, IpAddr>,
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
            addresses: HashMap::new(),
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
                hour: VecDeque::new(),
                difficulty: None,
                connection_error: None,
                adapter_error: None,
                report: None,
            },
        );
        self.prune();
        id
    }

    /// Records where a device can be asked for its own report. Only local
    /// network addresses are kept: loopback is the SV1 adapter's link, and a
    /// public address belongs to a router, not the device.
    pub fn set_address(&mut self, id: u64, ip: IpAddr) {
        if super::device_api::queryable(ip) && self.rows.contains_key(&id) {
            self.addresses.insert(id, ip);
        }
    }

    /// The local network address of a connected worker, for actions the user
    /// confirms on the workers page. Never shown or written to JSON.
    pub fn address_for_label(&self, label: &str) -> Option<IpAddr> {
        self.rows
            .iter()
            .find(|(_, row)| row.label == label && row.ended.is_none())
            .and_then(|(id, _)| self.addresses.get(id).copied())
    }

    /// Connected devices with a known address.
    pub fn addresses(&self) -> Vec<(u64, IpAddr)> {
        self.addresses
            .iter()
            .filter(|(id, _)| self.rows.get(id).is_some_and(|row| row.ended.is_none()))
            .map(|(id, ip)| (*id, *ip))
            .collect()
    }

    /// Stores what a device reported about itself.
    pub fn set_report(&mut self, id: u64, report: Option<DeviceReport>) {
        if let Some(row) = self.rows.get_mut(&id) {
            row.report = report;
        }
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
                let minute = second / 60;
                while row
                    .hour
                    .front()
                    .is_some_and(|(m, _)| (m + 1) * 60 + HOUR <= second)
                {
                    row.hour.pop_front();
                }
                if let Some((_, total)) = row.hour.back_mut().filter(|(m, _)| *m == minute) {
                    *total += hashes;
                } else {
                    row.hour.push_back((minute, hashes));
                }
                row.difficulty = Some(hashes / 2f64.powi(32));
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

    /// #### PR #40: a problem the adapter reports while the device mines,
    /// such as a pool refusing the donation channel.
    pub fn note_adapter(&mut self, id: u64, issue: &'static str) {
        if let Some(row) = self.rows.get_mut(&id) {
            row.adapter_error = Some(issue);
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
        self.addresses.remove(&id);
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
                let hour_seconds = elapsed.as_secs_f64().min(HOUR as f64);
                let hour_work: f64 = row
                    .hour
                    .iter()
                    .filter(|(m, _)| (m + 1) * 60 + HOUR > second)
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
                    hashrate_hour: if row.ended.is_some() {
                        Some(0.0)
                    } else if elapsed >= Duration::from_secs(WARMUP) {
                        Some(hour_work / hour_seconds)
                    } else {
                        None
                    },
                    difficulty: row.difficulty,
                    estimate_seconds: seconds,
                    last_share_seconds: row
                        .last_share
                        .map(|t| now.saturating_duration_since(t).as_secs()),
                    connection_error: row.connection_error,
                    adapter_error: row.adapter_error,
                    reported_hashrate: row.report.as_ref().and_then(|r| r.hashrate),
                    temperature_c: row.report.as_ref().and_then(|r| r.temperature_c),
                    fan: row.report.as_ref().and_then(|r| r.fan.clone()),
                    model: row.report.as_ref().and_then(|r| r.model.clone()),
                    firmware: row.report.as_ref().and_then(|r| r.firmware.clone()),
                    power_w: row.report.as_ref().and_then(|r| r.power_w),
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
        // #### PR #40: a remote pool's answers, in pool mode.
        "SV2 server unavailable" => "upstream unavailable",
        "SV2 setup rejected" | "SV2 setup incompatible" => "upstream refused the connection",
        "SV2 channel rejected" => "upstream refused the channel; check the pool identity",
        "unexpected SV2 extranonce allocation" => "upstream extranonce size does not fit SV1",
        "SV2 upstream asked to reconnect" | "SV2 upstream changed the extranonce" => {
            "upstream asked to reconnect"
        }
        "SV2 upstream closed the channel" => "upstream closed the channel",
        "invalid SV2 authority key" => "upstream key is not an SV2 authority key",
        "SV2 certificate version unsupported" => {
            "upstream certificate version is not SV2's; the pool must fix it"
        }
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
    fn hour_estimate_spans_an_hour_and_difficulty_follows_the_last_share() {
        let start = Instant::now();
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:1234".parse().unwrap(), false, start);
        // One share every minute for an hour.
        let mut target = [0xff; 32];
        target[28..32].fill(0);
        for minute in 0..60 {
            devices.share(
                id,
                ShareEvent::Accepted(target),
                false,
                start + Duration::from_secs(minute * 60 + 30),
            );
        }
        let row = devices
            .snapshots(start + Duration::from_secs(3600))
            .remove(0);
        let per_share = expected_hashes(&target);
        let hour = row.hashrate_hour.unwrap();
        assert!((hour - 60.0 * per_share / 3600.0).abs() < 1e-6 * hour);
        // The five-minute window holds only the last five shares.
        let five = row.hashrate_estimate.unwrap();
        assert!((five - 5.0 * per_share / 300.0).abs() < 1e-6 * five);
        assert!((row.difficulty.unwrap() - per_share / 2f64.powi(32)).abs() < 1e-9);
        // Buckets older than an hour leave the hour estimate.
        let later = devices
            .snapshots(start + Duration::from_secs(3600 + 1800))
            .remove(0);
        assert!(later.hashrate_hour.unwrap() < hour);
        assert!(devices.rows[&id].hour.len() <= 61);
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

    #[test]
    fn device_reports_appear_but_device_addresses_never_do() {
        let now = Instant::now();
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:5000".parse().unwrap(), true, now);
        // The adapter's loopback link is not the device.
        devices.set_address(id, "127.0.0.1".parse().unwrap());
        // A public address is the router's, never asked.
        devices.set_address(id, "203.0.113.5".parse().unwrap());
        assert!(devices.addresses().is_empty());
        devices.set_address(id, "10.9.8.7".parse().unwrap());
        let label = devices.snapshots(now)[0].label.clone();
        assert_eq!(
            devices.address_for_label(&label),
            Some("10.9.8.7".parse().unwrap())
        );
        assert_eq!(devices.address_for_label("Device unknown"), None);
        assert_eq!(devices.addresses(), vec![(id, "10.9.8.7".parse().unwrap())]);
        devices.set_report(
            id,
            Some(DeviceReport {
                hashrate: Some(4.0e12),
                temperature_c: Some(61.0),
                fan: Some("40%".into()),
                model: Some("Avalonminer AvalonNano3s".into()),
                power_w: Some(140.0),
                ..DeviceReport::default()
            }),
        );
        let row = devices.snapshots(now).remove(0);
        assert_eq!(row.reported_hashrate, Some(4.0e12));
        assert_eq!(row.temperature_c, Some(61.0));
        assert_eq!(row.fan.as_deref(), Some("40%"));
        assert_eq!(row.model.as_deref(), Some("Avalonminer AvalonNano3s"));
        assert_eq!(row.power_w, Some(140.0));
        let json = serde_json::to_string(&devices.snapshots(now)).unwrap();
        assert!(!json.contains("10.9.8.7"));
        devices.close(id, true, None, now);
        assert!(devices.addresses().is_empty());
    }
}
