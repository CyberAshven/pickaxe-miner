//! #### PR #38
//! Per-connection estimates from validated work, never advertised hardware speed.
//! Socket addresses only join the in-process SV1 adapter to its SV2 connection;
//! public snapshots contain generated labels, not worker identities or payouts.

use super::{device_api::DeviceReport, template::Hash};
use num_traits::ToPrimitive;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
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

/// #### PR #42
/// What: why a worker's address cannot be used for its Device panel.
/// Why: commands must reach the device itself, so a worker with no local
/// address, or one whose address several devices share, is not controlled.
/// Look here if: the panel refuses a device it should control, or controls a
/// router or VPN gateway instead of the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressIssue {
    /// No local network address is known: the worker connected from this
    /// computer (the SV1 adapter's link) or from a public address (a
    /// router's).
    Unknown,
    /// Several devices use this address: a Tailscale subnet router, a VPN
    /// gateway or CGNAT is in between, and commands would reach it instead of
    /// the device (see `Devices::sharing`).
    Shared,
    /// #### PR #42: two or more connections from this address are all under
    /// a minute old, so whether they are one device or several is not known
    /// yet (for example right after the server starts).
    Settling,
    /// #### PR #42: the watch view cannot read the server's owner-only list
    /// of device addresses, so it has none (it does not run as the server's
    /// user).
    Unlisted,
}

#[derive(Clone)]
pub struct Devices {
    prefix: u32,
    next_id: u64,
    sockets: HashMap<SocketAddr, u64>,
    // #### PR #42
    // What: device addresses live only in their rows (`Device::ip`); the
    // separate address map is gone. They are still used only to ask or
    // control the device itself, and never shown or written to JSON.
    // Why: the map duplicated the connected rows' addresses, and the Device
    // panel needs offline rows' addresses too, which only the rows keep.
    // Look here if: the poller asks an offline device, or a device's report
    // stops after a reconnect.
    rows: BTreeMap<u64, Device>,
    // #### PR #42
    // What: addresses where two devices were seen mining side by side, kept
    // until no row on the address remains.
    // Why: the rows that showed it can leave (a reconnect merges the rows it
    // took over when it closes; old offline rows are pruned), and the address
    // is still a gateway's.
    // Look here if: a device behind a gateway can be controlled after its
    // neighbours' rows are gone, or an address stays refused with no row.
    shared_addresses: HashSet<IpAddr>,
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
            shared_addresses: HashSet::new(),
        }
    }
}

/// #### PR #40
/// The worker name in a username, never an address: printable, at most 24
/// characters. (PR #42: visible to the other server modules, so the Device
/// panel's pool view can name a pool's worker this way, never by an address.)
pub(super) fn worker_name(username: &str) -> Option<String> {
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

    /// #### PR #42: removes a connection that turned out to be a Job
    /// Declaration client's, not a device's: it leaves no row, and the
    /// offline rows it took over on its address come back.
    pub fn forget(&mut self, id: u64) {
        if let Some(row) = self.rows.remove(&id) {
            self.rows.extend(row.replaced);
        }
        self.sockets.retain(|_, existing| *existing != id);
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
    /// number of the latest of them to close, and its worker name until the
    /// device gives its own, and keeps them aside, out of the table, until
    /// it closes.
    /// Why: the label then follows the device across reconnects, and a short
    /// extra connection that took over another device's row gives it back
    /// when it closes. The name is kept too because the device names itself
    /// only after its address is known: "rig1 #12" briefly read "Device
    /// …-12", and the highlight on the workers page lost its device.
    /// Look here if: a reconnect renumbers a label, two rows share one, or a
    /// reconnected device shows a generated label for a moment.
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
            .map(|(_, row)| (row.number, row.worker.clone()));
        if let (Some((number, worker)), Some(row)) = (latest, self.rows.get_mut(&id)) {
            row.number = number;
            if row.worker.is_none() {
                row.worker = worker;
            }
            row.label = device_label(self.prefix, row.worker.as_deref(), number);
            row.replaced.extend(removed);
        }
    }

    /// The row with this label, the connected one first.
    fn row_labelled(&self, label: &str) -> Option<&Device> {
        self.rows
            .values()
            .filter(|row| row.label == label)
            .max_by_key(|row| row.ended.is_none())
    }

    /// #### PR #42
    /// What: the local network address of the worker with this label, online
    /// or offline, for its Device panel; refused when several devices may use
    /// it (see `sharing`). Never shown or written to JSON.
    /// Why: the panel opens on any row, offline rows included (a device that
    /// stopped mining is the one most likely to need a restart). Behind a
    /// Tailscale subnet router, a VPN gateway or CGNAT the address is the
    /// gateway's, so commands would go there instead of the device.
    /// Look here if: an offline row cannot be controlled, or a device behind a
    /// shared address can.
    pub fn address_of(&self, label: &str, now: Instant) -> Result<IpAddr, AddressIssue> {
        let ip = self
            .row_labelled(label)
            .and_then(|row| row.ip)
            .ok_or(AddressIssue::Unknown)?;
        match self.sharing(ip, now) {
            Some(issue) => Err(issue),
            None => Ok(ip),
        }
    }

    /// #### PR #42
    /// What: how long ago the connection of the worker with this label
    /// closed; none while it is connected.
    /// Why: the Device panel shows it, and asks to confirm the device behind
    /// an offline row that left more than a few minutes ago, since its
    /// address may have passed to another device.
    /// Look here if: the panel shows a wrong offline time.
    pub fn offline_for(&self, label: &str, now: Instant) -> Option<Duration> {
        let ended = self.row_labelled(label)?.ended?;
        Some(now.saturating_duration_since(ended))
    }

    /// #### PR #42
    /// What: (label, address, refused) for every row with a local network
    /// address, offline rows included; refused when the address is shared
    /// or not known yet to be one device's (see `sharing`).
    /// Why: `stratum-v2 watch` will control devices too, from an owner-only
    /// file the server writes from this list; the status JSON stays free of
    /// addresses, as it is world-readable and printed to service logs. Only
    /// the server modules may read it.
    /// Look here if: an address reaches the status JSON, or the watch cannot
    /// find a device's address.
    #[cfg_attr(not(test), allow(dead_code))] // Read by the watch view's devices file, next.
    pub(super) fn private_addresses(&self, now: Instant) -> Vec<(String, IpAddr, bool)> {
        self.rows
            .values()
            .filter_map(|row| {
                let ip = row.ip?;
                Some((row.label.clone(), ip, self.sharing(ip, now).is_some()))
            })
            .collect()
    }

    /// #### PR #42
    /// What: why this address must not be controlled now, if it must not.
    ///
    /// - `Shared` when two connections there are each open for a minute or
    ///   more, or when two devices were seen mining side by side there (see
    ///   `mined_side_by_side`). The second stays until no row on the address
    ///   remains, offline rows included.
    /// - `Settling` when two or more connections there are all under a
    ///   minute old.
    ///
    /// A minute-old device beside a younger connection is not refused: that
    /// is how a firmware's short extra connection looks.
    ///
    /// Why: behind a Tailscale subnet router, a VPN gateway or CGNAT every
    /// device has the gateway's address. Counting only connected rows let a
    /// command through to the gateway as soon as one device there went
    /// offline, for a minute after a reconnect, and for the first minute
    /// after the server started.
    /// Look here if: a device behind a gateway can be controlled, or a single
    /// device is refused as shared.
    fn sharing(&self, ip: IpAddr, now: Instant) -> Option<AddressIssue> {
        if self.shared_addresses.contains(&ip) || self.mined_side_by_side(ip) {
            return Some(AddressIssue::Shared);
        }
        let ages: Vec<Duration> = self
            .rows
            .values()
            .filter(|row| row.ended.is_none() && row.ip == Some(ip))
            .map(|row| now.saturating_duration_since(row.started))
            .collect();
        let long_lived = ages.iter().filter(|age| **age >= SHORT_LIVED).count();
        match (long_lived, ages.len()) {
            (2.., _) => Some(AddressIssue::Shared),
            (0, 2..) => Some(AddressIssue::Settling),
            _ => None,
        }
    }

    /// #### PR #42
    /// What: whether two rows on this address (online or offline, and the
    /// rows a reconnect took over) each accepted a share a minute or more
    /// after the other connected: two devices mining side by side.
    /// Why: that holds after one of them goes offline, and it never holds for
    /// one device: its dead connection stopped mining before its new one
    /// opened, a probe does not mine, and a share still in flight when it
    /// reconnects comes within seconds.
    /// Look here if: two devices behind one gateway are not refused, or one
    /// device is refused as shared.
    fn mined_side_by_side(&self, ip: IpAddr) -> bool {
        let rows: Vec<&Device> = self
            .rows
            .values()
            .filter(|row| row.ip == Some(ip))
            .flat_map(|row| std::iter::once(row).chain(row.replaced.iter().map(|(_, old)| old)))
            .collect();
        let after = |row: &Device, other: &Device| {
            row.last_share
                .is_some_and(|share| share.saturating_duration_since(other.started) >= SHORT_LIVED)
        };
        rows.iter().enumerate().any(|(index, first)| {
            rows[index + 1..]
                .iter()
                .any(|second| after(first, second) && after(second, first))
        })
    }

    /// Connected devices with a known address.
    pub fn addresses(&self) -> Vec<(u64, IpAddr)> {
        // #### PR #42: read from the rows, which keep each device's address.
        self.rows
            .iter()
            .filter(|(_, row)| row.ended.is_none())
            .filter_map(|(id, row)| Some((*id, row.ip?)))
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
        self.sockets.retain(|_, existing| *existing != id);
        if let (true, Some(ip)) = (first, ip) {
            // #### PR #42: the shared mark is taken before settling, which
            // can merge away the rows this connection took over.
            if self.mined_side_by_side(ip) {
                self.shared_addresses.insert(ip);
            }
            let replaced = self
                .rows
                .get_mut(&id)
                .map(|row| std::mem::take(&mut row.replaced))
                .unwrap_or_default();
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
            // #### PR #42: an address stays shared when the rows that showed
            // it are pruned, and is forgotten once no row on it remains.
            if let Some(ip) = self.rows.get(id).and_then(|row| row.ip) {
                if self.mined_side_by_side(ip) {
                    self.shared_addresses.insert(ip);
                }
            }
            self.rows.remove(id);
        }
        let rows = &self.rows;
        self.shared_addresses
            .retain(|ip| rows.values().any(|row| row.ip == Some(*ip)));
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
        // #### PR #42: both devices keep mining side by side.
        for device in [first, sibling] {
            devices.share(
                device,
                ShareEvent::Accepted([255; 32]),
                true,
                start + Duration::from_secs(100),
            );
        }
        let labels = [first, sibling].map(|device| devices.label(device).unwrap());
        // A device that mined for two minutes stays, offline.
        devices.close(first, true, Some("SV1 disconnected"), later);
        let rows = devices.snapshots(later);
        assert_eq!(rows.len(), 2, "the device shows offline, not gone");
        assert_eq!(rows.iter().filter(|row| row.connected).count(), 1);
        assert_eq!(rows.iter().filter(|row| !row.connected).count(), 1);
        // The adapter's native socket closing the same row changes nothing.
        devices.close(first, false, Some("SV2 peer disconnected"), later);
        assert_eq!(devices.snapshots(later).len(), 2);
        // #### PR #42: with one of them offline, neither is controlled: the
        // address is still the gateway's.
        for label in &labels {
            assert_eq!(
                devices.address_of(label, later),
                Err(AddressIssue::Shared),
                "{label}"
            );
        }
        assert!(devices
            .private_addresses(later)
            .iter()
            .all(|(_, ip, refused)| *ip == gateway && *refused));
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
        // #### PR #42: with the number, the name (the probe gives none).
        assert_eq!(devices.label(probe), Some(label.clone()));
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
    // What: a device that reconnects keeps the number in its label, and its
    // name until it gives one, so "rig1 #N" survives close, connect,
    // set_address and set_worker; a new name is taken.
    // Why: a selection that follows the label would jump on a reconnect.
    // Look here if: connect, set_address, replace or set_worker label with
    // the row id, or drop the name.
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
        // #### PR #42: the name too, before the device gives it again, so
        // the label never reads "Device …" in between.
        assert_eq!(devices.label(second), Some(label.clone()));
        devices.set_worker(second, worker);
        assert_eq!(devices.label(second), Some(label.clone()));
        // A name given before the address is kept when the number changes.
        devices.close(second, true, Some("SV1 disconnected"), now);
        let third = devices.connect("127.0.0.1:7004".parse().unwrap(), true, now);
        devices.set_worker(third, worker);
        assert_eq!(devices.label(third), Some(format!("rig1 #{third}")));
        devices.set_address(third, nano);
        assert_eq!(devices.label(third), Some(label.clone()));
        let labels: Vec<_> = devices
            .snapshots(now)
            .into_iter()
            .map(|row| row.label)
            .collect();
        assert_eq!(labels.len(), 2, "the device and its neighbour");
        assert_ne!(labels[0], labels[1]);
        // #### PR #42: a device that comes back under another name takes it.
        devices.close(third, true, Some("SV1 disconnected"), now);
        let fourth = devices.connect("127.0.0.1:7005".parse().unwrap(), true, now);
        devices.set_address(fourth, nano);
        assert_eq!(devices.label(fourth), Some(label));
        devices.set_worker(fourth, "account.rig9");
        assert_eq!(devices.label(fourth), Some(format!("rig9 #{first}")));
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
        // #### PR #42: `address_of` replaces `address_for_label`.
        assert_eq!(
            devices.address_of(&label, now),
            Ok("10.9.8.7".parse().unwrap())
        );
        assert_eq!(
            devices.address_of("Device unknown", now),
            Err(AddressIssue::Unknown)
        );
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

    // #### PR #42
    // What: a worker that went offline keeps its local address for the
    // Device panel, while the poller no longer asks it; a worker with no
    // local address has none.
    // Why: the panel opens on offline rows too.
    // Look here if: address_of() or addresses() changes.
    #[test]
    fn an_offline_worker_keeps_its_address_for_the_panel() {
        let start = Instant::now();
        let later = start + Duration::from_secs(120);
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:8001".parse().unwrap(), true, start);
        devices.set_address(id, nano);
        devices.set_worker(id, "account.rig1");
        let label = devices.label(id).unwrap();
        assert_eq!(devices.addresses(), vec![(id, nano)]);
        assert_eq!(devices.offline_for(&label, later), None, "online");
        devices.close(id, true, Some("SV1 disconnected"), later);
        let rows = devices.snapshots(later);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].connected);
        assert_eq!(devices.address_of(&label, later), Ok(nano));
        assert!(devices.addresses().is_empty(), "the poller skips it");
        // #### PR #42: how long ago it left, for the panel.
        assert_eq!(
            devices.offline_for(&label, later + Duration::from_secs(90)),
            Some(Duration::from_secs(90))
        );
        assert_eq!(devices.offline_for("Device unknown", later), None);
        // A worker seen only from this computer has no address to use.
        let local = devices.connect("127.0.0.1:8002".parse().unwrap(), true, start);
        let local_label = devices.label(local).unwrap();
        assert_eq!(
            devices.address_of(&local_label, later),
            Err(AddressIssue::Unknown)
        );
    }

    // #### PR #42
    // What: two workers connected for a minute or more on one address are
    // not controlled, online or offline; two younger ones are not known yet
    // to be one device; a short probe beside one device does not count.
    // Why: that address belongs to a router or VPN gateway, not a device.
    // Look here if: address_of(), sharing() or SHORT_LIVED changes.
    #[test]
    fn workers_sharing_one_address_are_not_controlled() {
        let start = Instant::now();
        let gateway: IpAddr = "100.64.0.1".parse().unwrap();
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:8101".parse().unwrap(), true, start);
        devices.set_address(first, gateway);
        let first_label = devices.label(first).unwrap();
        // A 5 s probe beside one device leaves its address usable.
        let opened = start + Duration::from_secs(115);
        let probe = devices.connect("127.0.0.1:8102".parse().unwrap(), true, opened);
        devices.set_address(probe, gateway);
        let now = opened + Duration::from_secs(5);
        assert_eq!(devices.address_of(&first_label, now), Ok(gateway));
        devices.close(probe, true, Some("SV1 disconnected"), now);
        // A second device there: both are refused once each is a minute
        // old; before that, whether they are one device is not known yet.
        let second = devices.connect("127.0.0.1:8103".parse().unwrap(), true, start);
        devices.set_address(second, gateway);
        let second_label = devices.label(second).unwrap();
        let young = start + Duration::from_secs(30);
        assert_eq!(
            devices.address_of(&first_label, young),
            Err(AddressIssue::Settling)
        );
        assert_eq!(
            devices.address_of(&first_label, now),
            Err(AddressIssue::Shared)
        );
        assert_eq!(
            devices.address_of(&second_label, now),
            Err(AddressIssue::Shared)
        );
        // A third device there that went offline is refused too.
        let third = devices.connect("127.0.0.1:8104".parse().unwrap(), true, start);
        devices.set_address(third, gateway);
        let third_label = devices.label(third).unwrap();
        devices.close(third, true, Some("SV1 disconnected"), now);
        assert!(devices
            .snapshots(now)
            .iter()
            .any(|row| row.label == third_label && !row.connected));
        assert_eq!(
            devices.address_of(&third_label, now),
            Err(AddressIssue::Shared)
        );
    }

    // #### PR #42
    // What: two devices that mine side by side behind one address are
    // refused from their first shares a minute apart, after one goes
    // offline, after it reconnects, and after the rows that showed it are
    // merged away; a single device whose dead connection was still open
    // when it reconnected is not refused once that connection closes.
    // Why: counting only connected rows let a command through to the
    // gateway as soon as one device there went offline.
    // Look here if: sharing(), mined_side_by_side(), close() or prune()
    // changes.
    #[test]
    fn an_address_two_devices_mined_from_stays_shared() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let gateway: IpAddr = "100.64.0.1".parse().unwrap();
        let mut devices = Devices::default();
        let mine = |devices: &mut Devices, id, seconds| {
            devices.share(id, ShareEvent::Accepted([255; 32]), true, at(seconds));
        };
        let [a, b] = [("a", 8301), ("b", 8302)].map(|(name, port)| {
            let id = devices.connect(format!("127.0.0.1:{port}").parse().unwrap(), true, start);
            devices.set_address(id, gateway);
            devices.set_worker(id, &format!("account.{name}"));
            id
        });
        let labels = [a, b].map(|id| devices.label(id).unwrap());
        let refused = |devices: &Devices, seconds| {
            labels
                .iter()
                .map(|label| devices.address_of(label, at(seconds)))
                .collect::<Vec<_>>()
        };
        mine(&mut devices, a, 5);
        mine(&mut devices, b, 6);
        assert_eq!(refused(&devices, 7), [Err(AddressIssue::Settling); 2]);
        mine(&mut devices, a, 61);
        mine(&mut devices, b, 62);
        assert!(devices.mined_side_by_side(gateway));
        // b goes offline after two minutes: both stay refused.
        mine(&mut devices, a, 110);
        mine(&mut devices, b, 111);
        devices.close(b, true, Some("SV1 disconnected"), at(120));
        assert_eq!(refused(&devices, 121), [Err(AddressIssue::Shared); 2]);
        // b comes back: its young connection takes over its row and label.
        let b2 = devices.connect("127.0.0.1:8303".parse().unwrap(), true, at(130));
        devices.set_address(b2, gateway);
        assert_eq!(devices.label(b2).as_ref(), Some(&labels[1]));
        assert_eq!(refused(&devices, 131), [Err(AddressIssue::Shared); 2]);
        // Both leave, one comes back and takes over every offline row
        // there, then leaves again: the evidence is merged away, the mark
        // stays while a row on the address remains.
        mine(&mut devices, b2, 200);
        mine(&mut devices, a, 201);
        devices.close(b2, true, Some("SV1 disconnected"), at(250));
        devices.close(a, true, Some("SV1 disconnected"), at(251));
        let a2 = devices.connect("127.0.0.1:8304".parse().unwrap(), true, at(260));
        devices.set_address(a2, gateway);
        devices.close(a2, true, Some("SV1 disconnected"), at(400));
        assert_eq!(devices.snapshots(at(401)).len(), 1);
        assert!(!devices.mined_side_by_side(gateway));
        assert_eq!(
            devices.address_of(&labels[0], at(401)),
            Err(AddressIssue::Shared)
        );
        // Once no row on the address remains, it is forgotten.
        for port in 0..RECENT_CLOSED as u16 {
            let id = devices.connect(
                format!("127.0.0.1:{}", 9000 + port).parse().unwrap(),
                true,
                at(500),
            );
            devices.close(id, true, Some("SV1 disconnected"), at(500));
        }
        assert!(devices.shared_addresses.is_empty());
        // One device: its dead connection stopped mining before the new one
        // opened, so once it is seen closed the address is the device's.
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let old = devices.connect("127.0.0.1:8305".parse().unwrap(), true, at(1000));
        devices.set_address(old, nano);
        mine(&mut devices, old, 1030);
        let new = devices.connect("127.0.0.1:8306".parse().unwrap(), true, at(1060));
        devices.set_address(new, nano);
        for second in [1065, 1130, 1190] {
            mine(&mut devices, new, second);
        }
        // While the dead connection still looks open, two connections a
        // minute old are refused, for now.
        let label = devices.label(new).unwrap();
        assert_eq!(
            devices.address_of(&label, at(1190)),
            Err(AddressIssue::Shared)
        );
        devices.close(old, true, Some("SV1 write failed"), at(1200));
        let label = devices.label(new).unwrap();
        assert_eq!(devices.address_of(&label, at(1201)), Ok(nano));
        assert!(devices.shared_addresses.is_empty());
    }

    // #### PR #42
    // What: the private address list holds every row with a local address,
    // offline rows included, with whether the address is shared, and none of
    // it reaches the JSON snapshots or Debug output.
    // Why: the owner-only devices file is the only place addresses leave
    // this module; the status JSON is world-readable and logged.
    // Look here if: private_addresses() or the snapshots change.
    #[test]
    fn private_addresses_lists_local_ips_only_and_never_reaches_json() {
        let start = Instant::now();
        let now = start + Duration::from_secs(120);
        let lan: IpAddr = "192.168.0.127".parse().unwrap();
        let tailscale: IpAddr = "100.101.102.103".parse().unwrap();
        let gateway: IpAddr = "100.64.0.1".parse().unwrap();
        let mut devices = Devices::default();
        // Loopback (the adapter's link) and a public address are not kept.
        let adapter = devices.connect("127.0.0.1:8201".parse().unwrap(), true, start);
        devices.set_address(adapter, "127.0.0.1".parse().unwrap());
        let public = devices.connect("127.0.0.1:8202".parse().unwrap(), true, start);
        devices.set_address(public, "203.0.113.5".parse().unwrap());
        let online = devices.connect("127.0.0.1:8203".parse().unwrap(), true, start);
        devices.set_address(online, lan);
        let offline = devices.connect("127.0.0.1:8204".parse().unwrap(), true, start);
        devices.set_address(offline, tailscale);
        devices.close(offline, true, Some("SV1 disconnected"), now);
        let behind = [8205, 8206].map(|port| {
            let id = devices.connect(format!("127.0.0.1:{port}").parse().unwrap(), true, start);
            devices.set_address(id, gateway);
            id
        });
        let mut listed = devices.private_addresses(now);
        listed.sort();
        let mut expected = vec![
            (devices.label(online).unwrap(), lan, false),
            (devices.label(offline).unwrap(), tailscale, false),
            (devices.label(behind[0]).unwrap(), gateway, true),
            (devices.label(behind[1]).unwrap(), gateway, true),
        ];
        expected.sort();
        assert_eq!(listed, expected);
        let json = serde_json::to_string(&devices.snapshots(now)).unwrap();
        let debug = format!("{devices:?}");
        for address in [
            "192.168.0.127",
            "100.101.102.103",
            "100.64.0.1",
            "203.0.113.5",
        ] {
            assert!(!json.contains(address), "{address}");
            assert!(!debug.contains(address), "{address}");
        }
    }
}
