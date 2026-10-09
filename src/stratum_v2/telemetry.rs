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
/// #### PR #42
/// What: a connection that ends within a minute of opening is a short one.
/// Why: firmwares open short extra connections beside their mining one; only
/// those may vanish on close. Several devices behind one shared address (a
/// Tailscale subnet router, a VPN gateway, CGNAT) each live much longer.
/// Look here if: a device's row vanishes instead of showing offline, or a
/// firmware's extra connections leave offline rows behind.
const SHORT_LIVED: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub enum ShareEvent {
    Accepted(Hash),
    Rejected(&'static str),
}

/// #### PR #40
/// The difficulty a share's hash reached (pool difficulty: 1 is one in 2^32
/// hashes), for best-share records; the target's difficulty is the same
/// measure of what was asked.
pub fn share_difficulty(hash: &Hash) -> f64 {
    expected_hashes(hash) / 2f64.powi(32)
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
    /// #### PR #40: the highest difficulty one of its shares reached.
    pub best_share: Option<f64>,
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
    /// #### PR #42
    /// What: the number in the label. A reconnecting device's new row takes
    /// over the number of the offline row it replaces, so "rig1 #12" stays
    /// "rig1 #12" instead of becoming "rig1 #15".
    /// Why: the label was built from the row id, which every connection gets
    /// anew; a selection that follows the label would jump on a reconnect.
    /// Look here if: two rows show the same label, or a reconnect renumbers.
    number: u64,
    /// #### PR #42: the worker name its owner gave it, kept to rebuild the
    /// label when the number changes.
    worker: Option<String>,
    /// #### PR #42
    /// What: the offline rows this connection took over, kept aside until it
    /// closes.
    /// Why: behind a shared address a short extra connection can take over
    /// another device's offline row; when it closes within a minute, that row
    /// comes back instead of vanishing.
    /// Look here if: an offline row vanishes after a short connection on the
    /// same address, or a row comes back twice.
    replaced: Vec<(u64, Device)>,
    protocol: &'static str,
    /// #### PR #42: the device's local network address, kept after it
    /// disconnects so a reconnect replaces its row.
    ip: Option<IpAddr>,
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
    best_share: Option<f64>,
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

/// #### PR #40
/// The worker name in a username, never an address: printable, at most 24
/// characters.
fn worker_name(username: &str) -> Option<String> {
    let username = username.trim();
    let name = match username.rsplit_once('.') {
        Some((_, name)) => name,
        None => username,
    };
    let name: String = name.chars().filter(char::is_ascii_graphic).collect();
    if name.is_empty() || looks_like_address(&name) {
        return None;
    }
    Some(name.chars().take(24).collect())
}

/// #### PR #42
/// What: a row's label, from its owner's worker name or the generated one,
/// and the device's number (which keeps labels unique).
/// Why: the number can change after the name is set, when a reconnect takes
/// over the device's old row, so the label is rebuilt from both.
/// Look here if: a label shows the connection's id instead of the device's
/// number, or two rows show the same label.
fn device_label(prefix: u32, worker: Option<&str>, number: u64) -> String {
    match worker {
        Some(name) => format!("{name} #{number}"),
        None => format!("Device {prefix:08x}-{number}"),
    }
}

/// A CashAddr, with or without its prefix, or anything with a prefix.
fn looks_like_address(text: &str) -> bool {
    const CHARSET: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let lower = text.to_ascii_lowercase();
    lower.contains(':')
        || (lower.len() >= 40
            && (lower.starts_with('q') || lower.starts_with('p'))
            && lower.chars().all(|c| CHARSET.contains(c)))
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
                label: device_label(self.prefix, None, id),
                number: id,
                worker: None,
                replaced: Vec::new(),
                ip: None,
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
                best_share: None,
            },
        );
        self.prune();
        id
    }

    /// #### PR #40: a device's label, for records kept outside the table.
    pub fn label(&self, id: u64) -> Option<String> {
        self.rows.get(&id).map(|row| row.label.clone())
    }

    /// #### PR #40
    /// Records a share's difficulty as the device's best when it is; returns
    /// the device's label, for the pool's own best share.
    pub fn best_share(&mut self, id: u64, difficulty: f64) -> Option<String> {
        let row = self.rows.get_mut(&id)?;
        if row.best_share.is_none_or(|best| difficulty > best) {
            row.best_share = Some(difficulty);
        }
        Some(row.label.clone())
    }

    /// #### PR #40
    /// Names a worker by the name its owner gave it: the part after the payout
    /// address (`bitcoincash:q....rig1` gives `rig1`), or the whole username
    /// when it is not an address. An address alone keeps the generated label:
    /// payout addresses are never shown. The number keeps labels unique.
    pub fn set_worker(&mut self, id: u64, username: &str) {
        if let (Some(name), Some(row)) = (worker_name(username), self.rows.get_mut(&id)) {
            // #### PR #42
            // What: the label carries the device's number, not the
            // connection's id, and the name is kept.
            // Why: a reconnect takes over the number of the device's old row
            // (before or after the name arrives), and the label is rebuilt
            // with the name then.
            // Look here if: a named worker's number changes on a reconnect.
            row.label = device_label(self.prefix, Some(&name), row.number);
            row.worker = Some(name);
        }
    }

    /// Records where a device can be asked for its own report. Only local
    /// network addresses are kept: loopback is the SV1 adapter's link, and a
    /// public address belongs to a router, not the device.
    pub fn set_address(&mut self, id: u64, ip: IpAddr) {
        if super::device_api::queryable(ip) && self.rows.contains_key(&id) {
            self.addresses.insert(id, ip);
            // #### PR #42
            // What: one row per device on the local network. A device that
            // connects again takes over its offline rows on that address and
            // the number in their label (the latest to close, when there are
            // several), so "rig1 #12" stays "rig1 #12".
            // Why: every connection was a row of its own, so a reconnect left
            // an offline twin beside the device and renumbered its label, and
            // a firmware's short extra connections made rows come and go.
            // Look here if: two devices behind one shared address merge their
            // offline rows when one reconnects. Only local addresses are
            // matched, but a Tailscale subnet router, a VPN gateway or CGNAT
            // (100.64/10) gives many devices one address. A short connection
            // gives the rows back when it closes (see `settle_closed`).
            if let Some(row) = self.rows.get_mut(&id) {
                row.ip = Some(ip);
            }
            let offline: Vec<u64> = self
                .rows
                .iter()
                .filter(|(other, row)| **other != id && row.ended.is_some() && row.ip == Some(ip))
                .map(|(other, _)| *other)
                .collect();
            self.replace(id, &offline);
            // #### end PR #42 ####
        }
    }

    /// #### PR #42
    /// What: connection `id` takes over the rows in `old`: it takes the
    /// number of the latest of them to close, and keeps them aside, out of
    /// the table, until it closes.
    /// Why: the label then follows the device across reconnects, and a short
    /// extra connection that took over another device's row gives it back
    /// when it closes.
    /// Look here if: a reconnect renumbers a label, or two rows share one.
    fn replace(&mut self, id: u64, old: &[u64]) {
        if !self.rows.contains_key(&id) {
            return;
        }
        let removed: Vec<(u64, Device)> = old
            .iter()
            .filter_map(|other| self.rows.remove_entry(other))
            .collect();
        let latest = removed
            .iter()
            .max_by_key(|(other, row)| (row.ended, *other))
            .map(|(_, row)| row.number);
        if let (Some(number), Some(row)) = (latest, self.rows.get_mut(&id)) {
            row.number = number;
            row.label = device_label(self.prefix, row.worker.as_deref(), number);
            row.replaced.extend(removed);
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
        // #### PR #42
        // What: only the first close of a row decides what becomes of it,
        // from how long the connection lived.
        // Why: the adapter and the native socket each close the same row; a
        // second decision could drop a row already kept, with the rows it
        // took over.
        // Look here if: the second close of a row changes the table.
        let first = row.ended.is_none();
        let ended = *row.ended.get_or_insert(now);
        if adapter {
            row.adapter_error = error.map(connection_reason);
        } else {
            row.connection_error = error.map(connection_reason);
        }
        let (ip, lived, quiet) = (
            row.ip,
            ended.saturating_duration_since(row.started),
            row.last_share,
        );
        let replaced = std::mem::take(&mut row.replaced);
        self.sockets.retain(|_, existing| *existing != id);
        self.addresses.remove(&id);
        if let (true, Some(ip)) = (first, ip) {
            self.settle_closed(id, ip, lived, quiet, replaced);
        }
        // #### end PR #42 ####
        self.prune();
    }

    /// #### PR #42
    /// What: what becomes of a row on a local address when its connection
    /// closes, in this order:
    ///
    /// 1. The device's new connection takes it over: the only connection on
    ///    the address that opened after this one's last accepted share and
    ///    has not taken over a row takes this row's number and replaces it.
    /// 2. A short connection (under a minute) beside another online one
    ///    leaves no row, and gives back the offline rows it took over.
    /// 3. Otherwise it stays as an offline row, and the rows it took over are
    ///    merged into it for good.
    ///
    /// Why: the server sees a dead connection closed only when a write to it
    /// fails, often after the device (power cut, reboot, pulled cable) has
    /// already reconnected; the device then kept an offline twin and its new
    /// connection a new number. Behind a shared address (a Tailscale subnet
    /// router, a VPN gateway, CGNAT) every device has the same IP, so a row
    /// must not vanish just because another device there is online, and a
    /// device that kept mining after a new connection opened is not that
    /// connection's device.
    /// Look here if: a reconnect leaves an offline twin or renumbers a label,
    /// numbers move between devices behind one address, or a device's row
    /// vanishes instead of showing offline.
    fn settle_closed(
        &mut self,
        id: u64,
        ip: IpAddr,
        lived: Duration,
        quiet: Option<Instant>,
        replaced: Vec<(u64, Device)>,
    ) {
        let online: Vec<(u64, Instant, bool)> = self
            .rows
            .iter()
            .filter(|(other, row)| **other != id && row.ended.is_none() && row.ip == Some(ip))
            .map(|(other, row)| (*other, row.started, row.replaced.is_empty()))
            .collect();
        let successors: Vec<u64> = online
            .iter()
            .filter(|(_, opened, fresh)| *fresh && quiet.is_some_and(|share| *opened > share))
            .map(|(other, _, _)| *other)
            .collect();
        if let [next] = successors[..] {
            self.replace(next, &[id]);
        } else if lived < SHORT_LIVED && !online.is_empty() {
            self.rows.remove(&id);
            self.rows.extend(replaced);
        }
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
                    best_share: row.best_share,
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

    // #### PR #42
    // What: a device's reconnect replaces its own offline row, not another
    // device's, and its short extra connection leaves no row.
    // Why: every connection was a row of its own.
    // Look here if: set_address() or settle_closed() changes.
    #[test]
    fn a_device_keeps_one_row_through_reconnects_and_extra_connections() {
        let now = Instant::now();
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let other: IpAddr = "192.168.0.128".parse().unwrap();
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:5001".parse().unwrap(), true, now);
        devices.set_address(first, nano);
        let neighbour = devices.connect("127.0.0.1:5002".parse().unwrap(), true, now);
        devices.set_address(neighbour, other);
        devices.close(neighbour, true, Some("SV1 disconnected"), now);
        devices.close(first, true, Some("SV1 disconnected"), now);
        assert_eq!(
            devices.snapshots(now).len(),
            2,
            "offline rows stay until the device returns"
        );
        // The device reconnects: its offline row goes, another device's stays.
        let second = devices.connect("127.0.0.1:5003".parse().unwrap(), true, now);
        devices.set_address(second, nano);
        let rows = devices.snapshots(now);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.connected));
        assert_eq!(rows.iter().filter(|row| !row.connected).count(), 1);
        // A short extra connection from the same device leaves no row.
        let probe = devices.connect("127.0.0.1:5004".parse().unwrap(), true, now);
        devices.set_address(probe, nano);
        assert_eq!(devices.snapshots(now).len(), 3);
        devices.close(probe, true, Some("SV1 disconnected"), now);
        assert_eq!(devices.snapshots(now).len(), 2);
        assert!(devices.snapshots(now).iter().any(|row| row.connected));
    }

    // #### PR #42
    // What: behind one shared address (CGNAT, a subnet router), a device that
    // mined for two minutes stays as an offline row when it disconnects
    // while its sibling is online; a short probe there still vanishes.
    // Why: every device there has the same IP, so a real device must show
    // offline instead of vanishing, and a sibling that was online while it
    // kept mining is not its reconnect.
    // Look here if: SHORT_LIVED, close() or settle_closed() changes.
    #[test]
    fn a_long_lived_device_behind_a_shared_address_stays_as_an_offline_row() {
        let start = Instant::now();
        let later = start + Duration::from_secs(120);
        let gateway: IpAddr = "100.64.0.1".parse().unwrap();
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:6001".parse().unwrap(), true, start);
        devices.set_address(first, gateway);
        let sibling = devices.connect("127.0.0.1:6002".parse().unwrap(), true, start);
        devices.set_address(sibling, gateway);
        // The first device keeps mining after its sibling connected.
        devices.share(
            first,
            ShareEvent::Accepted([255; 32]),
            true,
            start + Duration::from_secs(30),
        );
        // A probe that ends within a minute leaves no row.
        let opened = start + Duration::from_secs(10);
        let probe = devices.connect("127.0.0.1:6003".parse().unwrap(), true, opened);
        devices.set_address(probe, gateway);
        assert_eq!(devices.snapshots(opened).len(), 3);
        devices.close(
            probe,
            true,
            Some("SV1 disconnected"),
            opened + Duration::from_secs(59),
        );
        assert_eq!(devices.snapshots(opened).len(), 2);
        // A device that mined for two minutes stays, offline.
        devices.close(first, true, Some("SV1 disconnected"), later);
        let rows = devices.snapshots(later);
        assert_eq!(rows.len(), 2, "the device shows offline, not gone");
        assert_eq!(rows.iter().filter(|row| row.connected).count(), 1);
        assert_eq!(rows.iter().filter(|row| !row.connected).count(), 1);
        // The adapter's native socket closing the same row changes nothing.
        devices.close(first, false, Some("SV2 peer disconnected"), later);
        assert_eq!(devices.snapshots(later).len(), 2);
    }

    // #### PR #42
    // What: behind one shared address, a short probe that opens after a
    // device went offline, while a sibling is online, takes over the
    // offline row and gives it back when it closes within a minute.
    // Why: a probe's takeover had deleted the offline row, and the probe was
    // then dropped as short-lived, so the device vanished.
    // Look here if: set_address(), replace() or settle_closed() changes.
    #[test]
    fn a_short_probe_behind_a_shared_address_gives_back_the_row_it_took_over() {
        let start = Instant::now();
        let gateway: IpAddr = "100.64.0.1".parse().unwrap();
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:6101".parse().unwrap(), true, start);
        devices.set_address(first, gateway);
        devices.set_worker(first, "account.rig1");
        let sibling = devices.connect("127.0.0.1:6102".parse().unwrap(), true, start);
        devices.set_address(sibling, gateway);
        devices.share(
            first,
            ShareEvent::Accepted([255; 32]),
            true,
            start + Duration::from_secs(30),
        );
        let label = devices.label(first).unwrap();
        devices.close(
            first,
            true,
            Some("SV1 disconnected"),
            start + Duration::from_secs(120),
        );
        let opened = start + Duration::from_secs(130);
        let probe = devices.connect("127.0.0.1:6103".parse().unwrap(), true, opened);
        devices.set_address(probe, gateway);
        assert_eq!(devices.label(first), None, "taken over for now");
        assert_eq!(
            devices.label(probe),
            Some(format!("Device {:08x}-{first}", devices.prefix))
        );
        let closed = opened + Duration::from_secs(5);
        devices.close(probe, true, Some("SV1 disconnected"), closed);
        let rows = devices.snapshots(closed);
        assert_eq!(rows.len(), 2, "the sibling and the offline device");
        assert!(rows.iter().any(|row| row.connected && row.label != label));
        assert!(rows.iter().any(|row| !row.connected && row.label == label));
        assert_eq!(devices.label(first), Some(label));
        assert_eq!(devices.label(probe), None);
    }

    // #### PR #42
    // What: a device whose new connection arrives before its old one is seen
    // closed (power cut, reboot, pulled cable) ends with one row, and the new
    // connection takes the old one's number when the old one closes. With
    // two new connections there, the old row stays offline.
    // Why: the server sees a dead connection closed only when a write to it
    // fails, often after the device has reconnected; the device kept an
    // offline twin and a new number.
    // Look here if: settle_closed() or replace() changes.
    #[test]
    fn a_device_back_before_its_old_connection_closes_keeps_one_row_and_its_number() {
        let start = Instant::now();
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let mut devices = Devices::default();
        let old = devices.connect("127.0.0.1:7101".parse().unwrap(), true, start);
        devices.set_address(old, nano);
        devices.set_worker(old, "account.rig1");
        devices.share(
            old,
            ShareEvent::Accepted([255; 32]),
            true,
            start + Duration::from_secs(30),
        );
        let label = devices.label(old).unwrap();
        // The device is back while its dead connection still looks online.
        let back = start + Duration::from_secs(60);
        let new = devices.connect("127.0.0.1:7102".parse().unwrap(), true, back);
        devices.set_address(new, nano);
        devices.set_worker(new, "account.rig1");
        assert_eq!(devices.label(new), Some(format!("rig1 #{new}")));
        assert_eq!(devices.snapshots(back).len(), 2);
        let closed = start + Duration::from_secs(120);
        devices.close(old, true, Some("SV1 write failed"), closed);
        let rows = devices.snapshots(closed);
        assert_eq!(rows.len(), 1, "no offline twin");
        assert!(rows[0].connected);
        assert_eq!(rows[0].label, label, "the old number");
        assert_eq!(devices.label(old), None);
        devices.close(old, false, Some("SV2 peer disconnected"), closed);
        assert_eq!(devices.snapshots(closed).len(), 1);
        // Two new connections before the old one closes: no guess.
        let lamp: IpAddr = "192.168.0.129".parse().unwrap();
        let gone = devices.connect("127.0.0.1:7103".parse().unwrap(), true, start);
        devices.set_address(gone, lamp);
        devices.share(
            gone,
            ShareEvent::Accepted([255; 32]),
            true,
            start + Duration::from_secs(30),
        );
        let one = devices.connect("127.0.0.1:7104".parse().unwrap(), true, back);
        devices.set_address(one, lamp);
        let two = devices.connect("127.0.0.1:7105".parse().unwrap(), true, back);
        devices.set_address(two, lamp);
        devices.close(gone, true, Some("SV1 write failed"), closed);
        let offline: Vec<_> = devices
            .snapshots(closed)
            .into_iter()
            .filter(|row| !row.connected)
            .map(|row| row.label)
            .collect();
        assert_eq!(offline, [format!("Device {:08x}-{gone}", devices.prefix)]);
        assert_eq!(devices.snapshots(closed).len(), 4);
    }

    // #### PR #42
    // What: a reconnect over several offline rows on one address takes the
    // number of the latest to close, not of the newest connection, and
    // replaces them all.
    // Why: the latest row to close is the device's most recent connection;
    // the choice decides which number a device keeps when it has an offline
    // twin, or when several devices share one address.
    // Look here if: replace() picks its number differently.
    #[test]
    fn a_reconnect_takes_the_number_of_the_latest_row_to_close() {
        let start = Instant::now();
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let mut devices = Devices::default();
        let older = devices.connect("127.0.0.1:7201".parse().unwrap(), true, start);
        devices.set_address(older, nano);
        let newer = devices.connect("127.0.0.1:7202".parse().unwrap(), true, start);
        devices.set_address(newer, nano);
        devices.close(
            newer,
            true,
            Some("SV1 disconnected"),
            start + Duration::from_secs(100),
        );
        devices.close(
            older,
            true,
            Some("SV1 disconnected"),
            start + Duration::from_secs(200),
        );
        assert_eq!(devices.snapshots(start).len(), 2);
        let now = start + Duration::from_secs(300);
        let back = devices.connect("127.0.0.1:7203".parse().unwrap(), true, now);
        devices.set_address(back, nano);
        let rows = devices.snapshots(now);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].label,
            format!("Device {:08x}-{older}", devices.prefix)
        );
    }

    // #### PR #42
    // What: a device that reconnects keeps the number in its label, so
    // "rig1 #N" survives close, connect, set_address and set_worker.
    // Why: a selection that follows the label would jump on a reconnect.
    // Look here if: connect, set_address or set_worker label with the row id.
    #[test]
    fn a_reconnected_device_keeps_its_number_and_name() {
        let now = Instant::now();
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let worker = "account.rig1";
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:7001".parse().unwrap(), true, now);
        devices.set_address(first, nano);
        devices.set_worker(first, worker);
        let label = devices.label(first).unwrap();
        assert_eq!(label, format!("rig1 #{first}"));
        // Another device connects in between, so ids move on.
        let neighbour = devices.connect("127.0.0.1:7002".parse().unwrap(), true, now);
        devices.set_address(neighbour, "192.168.0.128".parse().unwrap());
        devices.close(first, true, Some("SV1 disconnected"), now);
        assert_eq!(devices.label(first).as_deref(), Some(label.as_str()));
        let second = devices.connect("127.0.0.1:7003".parse().unwrap(), true, now);
        assert_ne!(second, first);
        assert_eq!(
            devices.label(second),
            Some(format!("Device {:08x}-{second}", devices.prefix))
        );
        devices.set_address(second, nano);
        assert_eq!(devices.label(first), None, "the offline row is replaced");
        assert_eq!(
            devices.label(second),
            Some(format!("Device {:08x}-{first}", devices.prefix))
        );
        devices.set_worker(second, worker);
        assert_eq!(devices.label(second), Some(label.clone()));
        // A name given before the address is kept when the number changes.
        devices.close(second, true, Some("SV1 disconnected"), now);
        let third = devices.connect("127.0.0.1:7004".parse().unwrap(), true, now);
        devices.set_worker(third, worker);
        assert_eq!(devices.label(third), Some(format!("rig1 #{third}")));
        devices.set_address(third, nano);
        assert_eq!(devices.label(third), Some(label));
        let labels: Vec<_> = devices
            .snapshots(now)
            .into_iter()
            .map(|row| row.label)
            .collect();
        assert_eq!(labels.len(), 2, "the device and its neighbour");
        assert_ne!(labels[0], labels[1]);
    }

    // #### PR #40
    #[test]
    fn workers_are_named_by_their_owners_but_never_by_an_address() {
        let address = crate::tx::p2pkh_hash_to_cashaddr_for_network(
            &[0x11; 20],
            crate::config::MiningNetwork::Mainnet,
        )
        .unwrap();
        let address = address.as_str();
        let bare = address.strip_prefix("bitcoincash:").unwrap();
        assert_eq!(
            worker_name(&format!("{address}.rig1")).as_deref(),
            Some("rig1")
        );
        assert_eq!(worker_name(&format!("{bare}.r2")).as_deref(), Some("r2"));
        assert_eq!(worker_name("rack-7").as_deref(), Some("rack-7"));
        assert_eq!(worker_name("account.worker1").as_deref(), Some("worker1"));
        assert_eq!(worker_name(address), None);
        assert_eq!(worker_name(bare), None);
        assert_eq!(worker_name(" \u{1b} "), None);
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:4000".parse().unwrap(), true, Instant::now());
        devices.set_worker(id, address);
        assert!(devices.snapshots(Instant::now())[0]
            .label
            .starts_with("Device "));
        devices.set_worker(id, &format!("{address}.rig1"));
        assert_eq!(
            devices.snapshots(Instant::now())[0].label,
            format!("rig1 #{id}")
        );
    }

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
