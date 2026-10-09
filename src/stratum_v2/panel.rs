//! #### PR #42
//! The Device panel: one worker's controls, opened from the highlighted row
//! of the workers page, online or offline. It names how the device is reached
//! ("local network", "Tailscale/CGNAT", "IPv6 local"), never its address.
//! The device is identified and every confirmed action is sent on a
//! background thread, so the page stays live while a device answers, or does
//! not.

use super::{
    device_api::DeviceAction,
    fleet::{DeviceControls, Fleet},
    telemetry::{AddressIssue, DeviceSnapshot, Devices},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    widgets::{Block, Paragraph, TableState, Wrap},
    Frame,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

/// Rows moved by PgUp and PgDn.
const PAGE: usize = 10;

/// #### PR #42
/// What: for this long after an offline row's connection closed, its last
/// address is trusted without confirming the device; after that, the panel
/// offers actions only when the device there identifies as the make and
/// model the row reported while online.
/// Why: once a device is gone, its address can pass to another device (a
/// new DHCP lease) that does not mine here, and an action would reach it.
/// Look here if: an offline row offers nothing although its device answers,
/// or offers actions for another device.
const TRUSTED_OFFLINE: Duration = Duration::from_secs(600);

/// The longest device reply or error shown, in characters.
const REPLY_CHARS: usize = 240;

/// What an address in a device's reply is shown as.
const HIDDEN: &str = "[address hidden]";

/// #### PR #42
/// What: the highlighted row of the workers page, kept by its label.
/// Why: rows re-sort every frame (online first, then by label), so a device
/// going offline would move an index-based highlight onto another device,
/// and Enter must open the device the user highlighted.
/// Look here if: the highlight jumps when a device connects or disconnects,
/// or Enter opens another device than the highlighted one.
#[derive(Debug, Default)]
pub(super) struct Selection {
    label: Option<String>,
    pub(super) table: TableState,
}

impl Selection {
    /// Finds the selected device among this frame's rows, or the top row
    /// when it is gone, and highlights it.
    pub(super) fn follow(&mut self, labels: &[&str]) -> Option<usize> {
        let index = (!labels.is_empty()).then(|| {
            self.label
                .as_deref()
                .and_then(|label| labels.iter().position(|row| *row == label))
                .unwrap_or(0)
        });
        self.table.select(index);
        self.label = index.map(|index| labels[index].to_owned());
        index
    }

    /// Moves the highlight for an arrow key, PgUp, PgDn, Home or End.
    pub(super) fn step(&mut self, code: KeyCode, labels: &[&str]) {
        let Some(last) = labels.len().checked_sub(1) else {
            return;
        };
        let index = self.table.selected().unwrap_or(0).min(last);
        let next = match code {
            KeyCode::Up => index.saturating_sub(1),
            KeyCode::Down => index.saturating_add(1).min(last),
            KeyCode::PageUp => index.saturating_sub(PAGE),
            KeyCode::PageDown => index.saturating_add(PAGE).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => return,
        };
        self.table.select(Some(next));
        self.label = Some(labels[next].to_owned());
    }

    /// The highlighted row, which Enter opens.
    pub(super) fn index(&self) -> Option<usize> {
        self.table.selected()
    }
}

/// #### PR #42
/// What: whether a key on the workers page opens the Device panel: Enter,
/// or `c` without Ctrl (Ctrl+C stops the server).
/// Why: one rule, used by the workers page and by its test.
/// Look here if: Enter or `c` does not open the panel, or Ctrl+C does.
pub(super) fn opens_panel(key: &KeyEvent, workers_page: bool) -> bool {
    workers_page
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char('C')
        )
}

/// #### PR #42
/// What: one worker's Device panel: its make and power, how it is reached,
/// the actions it offers, and the device's reply to the last one.
/// Why: the panel opens on any row, offline rows included, with the address
/// the device last connected from. It refuses an address several devices
/// may share, and an offline row's address once it may belong to another
/// device. The address itself is never shown.
/// Look here if: the panel controls a different device than its row, shows
/// an address, or offers actions it cannot send.
pub(super) struct DevicePanel {
    label: String,
    online: bool,
    /// How long ago an offline row's connection closed.
    offline_for: Option<Duration>,
    /// Model, firmware and power, as the device reports them.
    details: String,
    /// The make and model asic-rs read while the device was online.
    model: Option<String>,
    target: Result<IpAddr, AddressIssue>,
    /// How the device is reached; never its address.
    network: &'static str,
    /// What the device offers, once identified in the background.
    controls: Arc<Mutex<Option<DeviceControls>>>,
    page: Page,
    reply: Arc<Mutex<Option<String>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Menu,
    Confirm(DeviceAction),
}

/// What the panel offers at the moment.
enum Offer {
    /// The address cannot be used.
    Refused(AddressIssue),
    /// The device is being identified.
    Identifying,
    /// Identified, but an offline row's device is not confirmed.
    Doubted(DeviceControls, Doubt),
    Ready(DeviceControls),
}

/// Why the device answering at an offline row's address is not confirmed
/// as the row's device.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Doubt {
    /// asic-rs identifies another make or model there.
    Another(String),
    /// The row left too long ago, and its make and model cannot be compared.
    Unconfirmed,
}

impl DevicePanel {
    /// What Enter does on the workers page: the panel for the highlighted
    /// row of this frame's `rows`, in the table's order.
    pub(super) fn for_selection(
        devices: &Devices,
        rows: &[DeviceSnapshot],
        selection: &Selection,
        now: Instant,
    ) -> Option<Self> {
        rows.get(selection.index()?)
            .map(|row| Self::for_row(devices, row, now))
    }

    /// The panel for a row of the workers page, online or offline, with the
    /// address its device last connected from. Nothing is sent to the device
    /// until `identify` and a confirmed action.
    pub(super) fn for_row(devices: &Devices, row: &DeviceSnapshot, now: Instant) -> Self {
        let mut panel = Self::new(
            row.label.clone(),
            row.connected,
            details(row.model.as_deref(), row.firmware.as_deref(), row.power_w),
            devices.address_of(&row.label, now),
        );
        panel.model = row.model.clone();
        panel.offline_for = devices
            .offline_for(&row.label, now)
            .filter(|_| !row.connected);
        panel
    }

    fn new(
        label: String,
        online: bool,
        details: String,
        target: Result<IpAddr, AddressIssue>,
    ) -> Self {
        Self {
            label,
            online,
            offline_for: None,
            details,
            model: None,
            network: target.map_or("", network_class),
            target,
            controls: Arc::new(Mutex::new(None)),
            page: Page::Menu,
            reply: Arc::new(Mutex::new(None)),
        }
    }

    /// Asks the device what it is, on a background thread (at most ten
    /// seconds); the page says "Identifying the device" until it answers.
    /// An offline row's address is looked at afresh, since another device
    /// may answer there now.
    pub(super) fn identify(&self, fleet: &Arc<Fleet>) {
        let Ok(ip) = self.target else {
            return;
        };
        let fleet = Arc::clone(fleet);
        let controls = Arc::clone(&self.controls);
        let fresh = !self.online;
        thread::spawn(move || {
            let found = fleet.identify_now(ip, fresh);
            if let Ok(mut controls) = controls.lock() {
                *controls = Some(found);
            }
        });
    }

    /// Handles one key; true closes the panel. The panel takes every key, so
    /// `q` here never stops the server. A number chooses an action, `y`
    /// confirms it, any other key cancels, and Esc goes back.
    pub(super) fn handle_key(&mut self, key: KeyEvent, fleet: &Arc<Fleet>) -> bool {
        match (self.page, key.code) {
            (Page::Menu, KeyCode::Esc) => return true,
            (Page::Menu, KeyCode::Char(choice @ '1'..='9')) => {
                if let Some(action) = self
                    .actions()
                    .get(usize::from(choice as u8 - b'1'))
                    .copied()
                {
                    self.page = Page::Confirm(action);
                }
            }
            (Page::Confirm(action), KeyCode::Char('y') | KeyCode::Char('Y')) => {
                self.page = Page::Menu;
                self.send(action, fleet);
            }
            (Page::Confirm(_), _) => self.page = Page::Menu,
            (Page::Menu, _) => {}
        }
        false
    }

    /// What the panel can offer now.
    fn offer(&self) -> Offer {
        if let Err(issue) = self.target {
            return Offer::Refused(issue);
        }
        let Some(found) = self
            .controls
            .lock()
            .ok()
            .and_then(|controls| controls.clone())
        else {
            return Offer::Identifying;
        };
        match self.doubt(&found) {
            Some(doubt) => Offer::Doubted(found, doubt),
            None => Offer::Ready(found),
        }
    }

    /// #### PR #42
    /// What: whether the device that answers at an offline row's last
    /// address is not confirmed as the row's device. It is confirmed when it
    /// identifies as the make and model the row reported while online, or,
    /// when that cannot be compared, while the row left at most
    /// `TRUSTED_OFFLINE` ago. Online rows need no confirmation.
    /// Why: see `TRUSTED_OFFLINE`.
    /// Look here if: an offline row offers nothing although its device
    /// answers, or offers actions for another device.
    fn doubt(&self, found: &DeviceControls) -> Option<Doubt> {
        if self.online {
            return None;
        }
        match (found.model.as_deref(), self.model.as_deref()) {
            (Some(now), Some(then)) if now == then => None,
            (Some(now), Some(_)) => Some(Doubt::Another(now.to_owned())),
            _ if self.offline_for.is_some_and(|age| age <= TRUSTED_OFFLINE) => None,
            _ => Some(Doubt::Unconfirmed),
        }
    }

    /// The actions offered: none without a usable address, until the device
    /// is identified, or for an offline row's device that is not confirmed.
    fn actions(&self) -> Vec<DeviceAction> {
        match self.offer() {
            Offer::Ready(found) => found.actions,
            _ => Vec::new(),
        }
    }

    /// Sends a confirmed action on a background thread; its reply replaces
    /// "Sending".
    fn send(&self, action: DeviceAction, fleet: &Arc<Fleet>) {
        let Ok(ip) = self.target else {
            return;
        };
        self.set_reply(format!("Sending: {}…", action.label()));
        let reply = Arc::clone(&self.reply);
        let fleet = Arc::clone(fleet);
        thread::spawn(move || {
            let text = outcome(action, fleet.control(ip, action));
            if let Ok(mut reply) = reply.lock() {
                *reply = Some(text);
            }
        });
    }

    fn set_reply(&self, text: String) {
        if let Ok(mut reply) = self.reply.lock() {
            *reply = Some(text);
        }
    }

    /// The panel's text.
    fn text(&self) -> String {
        let status = match (self.online, self.offline_for) {
            (true, _) => "Online".to_owned(),
            (false, Some(age)) => format!("Offline for {}", span(age)),
            (false, None) => "Offline".to_owned(),
        };
        let mut text = format!("Worker  {} · {status}\n", self.label);
        if !self.details.is_empty() {
            text.push_str(&format!("{}\n", self.details));
        }
        match self.offer() {
            Offer::Refused(issue) => text.push_str(match issue {
                AddressIssue::Unknown => {
                    "\nThis worker's network address is not known. It connected from this \
                     computer or from a public address, so it cannot be controlled from here.\n"
                }
                AddressIssue::Shared => {
                    "\nSeveral workers share this address, so a router or VPN gateway is in \
                     between. Control them from their own network.\n"
                }
                AddressIssue::Settling => {
                    "\nSeveral connections from this address are under a minute old. Pickaxe \
                     cannot tell yet whether they are one device. Open this panel again in a \
                     minute.\n"
                }
            }),
            Offer::Identifying => text.push_str(&format!(
                "Reached on: {}\n\nIdentifying the device…\n",
                self.network
            )),
            Offer::Doubted(found, doubt) => {
                text.push_str(&self.reached(&found));
                text.push_str(&match doubt {
                    Doubt::Another(model) => format!(
                        "\nA different device answers at this worker's last address. It \
                         identifies as {model}. This worker reported {}. Nothing is offered.\n",
                        self.model.as_deref().unwrap_or("another model")
                    ),
                    Doubt::Unconfirmed => format!(
                        "\nThis worker went offline over {} ago. Pickaxe cannot confirm that \
                         the device at its last address is the same one, so nothing is offered. \
                         Use the device's own page or app.\n",
                        span(TRUSTED_OFFLINE)
                    ),
                });
            }
            Offer::Ready(found) => {
                text.push_str(&self.reached(&found));
                text.push('\n');
                match self.page {
                    Page::Confirm(action) => text.push_str(&format!(
                        "{} {}?\n\ny  Yes · any other key  No\n",
                        action.label(),
                        self.label
                    )),
                    Page::Menu if found.actions.is_empty() => {
                        text.push_str("This device offers no actions.\n")
                    }
                    Page::Menu => {
                        for (index, action) in found.actions.iter().take(9).enumerate() {
                            text.push_str(&format!("{}  {}\n", index + 1, action.label()));
                        }
                    }
                }
            }
        }
        if self.page == Page::Menu {
            text.push_str("\nEsc  Back to workers\n");
        }
        if let Some(reply) = self.reply.lock().ok().and_then(|reply| reply.clone()) {
            text.push_str(&format!("\n{reply}\n"));
        }
        text.push_str(
            "\nActions go straight to the device, on your local network or Tailscale. Each one \
             needs your confirmation. The device's address is never shown. The list is what \
             asic-rs supports for this make and firmware, plus Avalon work levels. Restart on \
             an Avalon is Canaan's own reboot. A device asic-rs does not identify offers \
             Restart and work levels.",
        );
        text
    }

    /// How the device is reached, and what asic-rs identified.
    fn reached(&self, found: &DeviceControls) -> String {
        let identified = match &found.firmware {
            Some(firmware) => format!("Identified by asic-rs: {firmware}"),
            None => "Not identified by asic-rs, so Pickaxe's own commands".to_owned(),
        };
        format!("Reached on: {}\n{identified}\n", self.network)
    }
}

/// Draws the Device panel over the whole screen.
pub(super) fn render(frame: &mut Frame<'_>, panel: &DevicePanel) {
    frame.render_widget(
        Paragraph::new(panel.text())
            .block(Block::bordered().title("Pickaxe · Device panel"))
            .wrap(Wrap { trim: false }),
        frame.area(),
    );
}

/// Model, firmware and power, as the device reports them:
/// "Avalonminer AvalonNano3s · firmware 25103101 · 140 W".
fn details(model: Option<&str>, firmware: Option<&str>, power_w: Option<f64>) -> String {
    [
        model.map(str::to_owned),
        firmware.map(|firmware| format!("firmware {firmware}")),
        power_w
            .filter(|watts| watts.is_finite() && *watts > 0.0)
            .map(|watts| format!("{watts:.0} W")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// How a device is reached, named without its address. Only local network
/// addresses reach the panel (`device_api::queryable`).
fn network_class(ip: IpAddr) -> &'static str {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            if first == 100 && second & 0xc0 == 0x40 {
                "Tailscale/CGNAT"
            } else {
                "local network"
            }
        }
        IpAddr::V6(_) => "IPv6 local",
    }
}

/// A duration as "45 s", "12 min", "3 h" or "2 days".
fn span(duration: Duration) -> String {
    match duration.as_secs() {
        seconds @ 0..60 => format!("{seconds} s"),
        seconds @ 60..3600 => format!("{} min", seconds / 60),
        seconds @ 3600..172_800 => format!("{} h", seconds / 3600),
        seconds => format!("{} days", seconds / 86_400),
    }
}

/// #### PR #42
/// What: the line shown for a sent action: the device's reply, or why it was
/// not done, with every address taken out and at most `REPLY_CHARS`
/// characters of it.
/// Why: asic-rs passes on its HTTP library's errors, which name the request's
/// URL and so the device's address ("error sending request for url
/// (http://192.168.…/api/system/pause)"), and a device's own message could
/// hold one. The panel never shows an address.
/// Look here if: an address shows on the Device panel, or a reply is cut
/// short or reads oddly.
fn outcome(action: DeviceAction, result: Result<String, String>) -> String {
    match result {
        Ok(message) => format!("{}: {}", action.label(), plain(&message)),
        Err(error) => format!(
            "{}: not done. {}",
            action.label(),
            capitalized(&plain(&error))
        ),
    }
}

/// Device text made safe to show: no URL or address, at most `REPLY_CHARS`
/// characters. The scan for addresses tries every place in the text, so it
/// reads only the start of a long text; what it skips is past the part shown.
fn plain(text: &str) -> String {
    let head: String = without_urls(text).chars().take(REPLY_CHARS * 4).collect();
    let shown = without_addresses(&head);
    if shown.chars().count() > REPLY_CHARS {
        format!("{}…", shown.chars().take(REPLY_CHARS).collect::<String>())
    } else {
        shown
    }
}

/// The text without the " for url (…)" parts reqwest's errors end with.
fn without_urls(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find(" for url (") {
        let end = text[start..]
            .find(')')
            .map_or(text.len(), |close| start + close + 1);
        text.replace_range(start..end, "");
    }
    text
}

/// The text with each IP address in it, with or without brackets or a
/// port, shown as `HIDDEN`.
fn without_addresses(text: &str) -> String {
    let is_part = |c: char| c.is_ascii_hexdigit() || matches!(c, '.' | ':' | '[' | ']');
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_part) {
        shown.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c: char| !is_part(c)).unwrap_or(rest.len());
        shown.push_str(&hide_in_run(&rest[..end]));
        rest = &rest[end..];
    }
    shown.push_str(rest);
    shown
}

/// One run of the characters addresses are written with, such as
/// "de:192.168.0.5:80", with the longest address starting at each place
/// hidden. A run is ASCII, so its byte offsets are character offsets.
fn hide_in_run(run: &str) -> String {
    let mut shown = String::with_capacity(run.len());
    let mut start = 0;
    while start < run.len() {
        let longest = (start + 1..=run.len().min(start + 64))
            .rev()
            .find(|&end| is_address(&run[start..end]));
        match longest {
            Some(end) => {
                shown.push_str(HIDDEN);
                start = end;
            }
            None => {
                shown.push_str(&run[start..=start]);
                start += 1;
            }
        }
    }
    shown
}

/// Whether the text is an IP address, with or without brackets or a port.
/// It must hold a digit, so words such as "bad" or "::" stay.
fn is_address(text: &str) -> bool {
    let bare = text
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(text);
    text.contains(|c: char| c.is_ascii_digit())
        && (bare.parse::<IpAddr>().is_ok() || text.parse::<SocketAddr>().is_ok())
}

/// The text with its first letter in upper case.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::{device_api::DeviceReport, telemetry::ShareEvent};
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn screen(panel: &DevicePanel) -> String {
        let mut terminal = Terminal::new(TestBackend::new(200, 30)).unwrap();
        terminal.draw(|frame| render(frame, panel)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Three named devices on the local network, all online.
    fn three_devices(start: Instant) -> (Devices, [u64; 3]) {
        let mut devices = Devices::default();
        let ids = [("alpha", 1), ("bravo", 2), ("charlie", 3)].map(|(name, host)| {
            let id = devices.connect(
                format!("127.0.0.1:{}", 9000 + host).parse().unwrap(),
                true,
                start,
            );
            devices.set_address(id, format!("192.168.0.{host}").parse().unwrap());
            devices.set_worker(id, &format!("account.{name}"));
            id
        });
        (devices, ids)
    }

    fn labels_of(devices: &Devices, now: Instant) -> Vec<String> {
        devices
            .snapshots(now)
            .into_iter()
            .map(|row| row.label)
            .collect()
    }

    fn own() -> DeviceControls {
        DeviceControls {
            actions: DeviceAction::OWN.to_vec(),
            ..DeviceControls::default()
        }
    }

    // #### PR #42
    // What: Enter (or c) on the workers page opens the panel for the
    // highlighted row, with that device's address, not for the top row;
    // Ctrl+C and the other pages do not open it.
    // Why: `c` used to open the scroll position's row.
    // Look here if: Selection, opens_panel or DevicePanel::for_selection
    // changes.
    #[test]
    fn enter_opens_the_selected_row_not_the_top_row() {
        let start = Instant::now();
        let (mut devices, ids) = three_devices(start);
        devices.set_report(
            ids[1],
            Some(DeviceReport {
                model: Some("Avalonminer AvalonNano3s".into()),
                power_w: Some(140.0),
                ..DeviceReport::default()
            }),
        );
        let rows = devices.snapshots(start);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        let mut selection = Selection::default();
        assert_eq!(selection.follow(&labels), Some(0), "the top row at first");
        selection.step(KeyCode::Down, &labels);
        assert_eq!(selection.index(), Some(1));
        for code in [KeyCode::Enter, KeyCode::Char('c'), KeyCode::Char('C')] {
            assert!(opens_panel(&key(code), true), "{code:?}");
            assert!(!opens_panel(&key(code), false), "{code:?} elsewhere");
        }
        let stop = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!opens_panel(&stop, true), "Ctrl+C stops the server");
        assert!(!opens_panel(&key(KeyCode::Char('q')), true));
        let panel = DevicePanel::for_selection(&devices, &rows, &selection, start).unwrap();
        assert_eq!(panel.label, devices.label(ids[1]).unwrap());
        assert_eq!(panel.target, Ok("192.168.0.2".parse().unwrap()));
        let text = screen(&panel);
        assert!(
            text.contains(&format!("Worker  {} · Online", labels[1])),
            "{text}"
        );
        assert!(!text.contains(labels[0]), "{text}");
        assert!(text.contains("Avalonminer AvalonNano3s · 140 W"), "{text}");
        assert!(text.contains("Reached on: local network"), "{text}");
        assert!(text.contains("Identifying the device"), "{text}");
        assert!(!text.contains("192.168"), "{text}");
        // Home, End and the page keys stay within the rows.
        selection.step(KeyCode::End, &labels);
        assert_eq!(selection.index(), Some(2));
        selection.step(KeyCode::PageDown, &labels);
        assert_eq!(selection.index(), Some(2));
        selection.step(KeyCode::PageUp, &labels);
        assert_eq!(selection.index(), Some(0));
        selection.step(KeyCode::Up, &labels);
        assert_eq!(selection.index(), Some(0));
        selection.step(KeyCode::Home, &labels);
        assert_eq!(selection.index(), Some(0));
        assert!(DevicePanel::for_selection(&devices, &[], &selection, start).is_none());
    }

    // #### PR #42
    // What: the highlight stays on its device when that device goes offline
    // and the rows re-sort, and when it comes back; it goes to the top row
    // only when its device's row is gone.
    // Why: an index-based highlight moved onto another device, and a
    // reconnecting device's label lost its name until the device gave it.
    // Look here if: Selection::follow or Devices::replace changes.
    #[test]
    fn selection_follows_the_device_when_rows_resort() {
        let start = Instant::now();
        let (mut devices, ids) = three_devices(start);
        let labels = labels_of(&devices, start);
        let bravo = labels[1].clone();
        let mut selection = Selection::default();
        selection.follow(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        selection.step(
            KeyCode::Down,
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        // Bravo goes offline: online rows come first, so it moves to the end.
        let later = start + Duration::from_secs(120);
        devices.close(ids[1], true, Some("SV1 disconnected"), later);
        let labels = labels_of(&devices, later);
        assert_eq!(labels[2], bravo);
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        assert_eq!(selection.follow(&labels), Some(2));
        assert_eq!(selection.label.as_deref(), Some(bravo.as_str()));
        // Bravo comes back: its label is bravo's before it names itself.
        let back = devices.connect("127.0.0.1:9104".parse().unwrap(), true, later);
        devices.set_address(back, "192.168.0.2".parse().unwrap());
        let labels = labels_of(&devices, later);
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let index = selection.follow(&labels).unwrap();
        assert_eq!(labels[index], bravo);
        // A device that is gone leaves the highlight on the top row.
        let others: Vec<&str> = labels.iter().copied().filter(|l| *l != bravo).collect();
        assert_eq!(selection.follow(&others), Some(0));
        assert_eq!(selection.label.as_deref(), Some(others[0]));
        assert_eq!(selection.follow(&[]), None);
        assert_eq!(selection.index(), None);
    }

    // #### PR #42
    // What: an offline row opens with the address its device last connected
    // from, shows how long it has been offline, and never shows the address;
    // a row without a local address, or on a shared or unsettled one,
    // explains why it cannot be controlled and offers nothing.
    // Why: the panel opens on offline rows too.
    // Look here if: DevicePanel::for_row or its text changes.
    #[test]
    fn an_offline_row_opens_with_its_last_address() {
        let start = Instant::now();
        let later = start + Duration::from_secs(120);
        let tailscale: IpAddr = "100.101.102.103".parse().unwrap();
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:9101".parse().unwrap(), true, start);
        devices.set_address(id, tailscale);
        devices.set_worker(id, "account.rig1");
        devices.share(
            id,
            ShareEvent::Accepted([255; 32]),
            true,
            start + Duration::from_secs(30),
        );
        devices.close(id, true, Some("SV1 disconnected"), later);
        let opened = later + Duration::from_secs(150);
        let row = devices.snapshots(opened).remove(0);
        assert!(!row.connected);
        let panel = DevicePanel::for_row(&devices, &row, opened);
        assert_eq!(panel.target, Ok(tailscale));
        let text = screen(&panel);
        assert!(
            text.contains(&format!("Worker  {} · Offline for 2 min", row.label)),
            "{text}"
        );
        assert!(text.contains("Reached on: Tailscale/CGNAT"), "{text}");
        assert!(!text.contains("100.101"), "{text}");
        // Seen only from this computer: no address, no actions.
        let local = devices.connect("127.0.0.1:9102".parse().unwrap(), true, start);
        let label = devices.label(local).unwrap();
        let row = devices
            .snapshots(later)
            .into_iter()
            .find(|row| row.label == label)
            .unwrap();
        let mut panel = DevicePanel::for_row(&devices, &row, later);
        assert_eq!(panel.target, Err(AddressIssue::Unknown));
        *panel.controls.lock().unwrap() = Some(own());
        let fleet = Arc::new(Fleet::new());
        assert!(!panel.handle_key(key(KeyCode::Char('1')), &fleet));
        assert_eq!(panel.page, Page::Menu, "nothing to choose");
        let text = screen(&panel);
        assert!(text.contains("cannot be controlled from here"), "{text}");
        assert!(!text.contains("1  Restart"), "{text}");
        // Behind a shared address, or one not settled yet: refused, with the
        // reason, and nothing to choose.
        for (issue, reason) in [
            (AddressIssue::Shared, "Several workers share this address"),
            (AddressIssue::Settling, "Open this panel again in a minute"),
        ] {
            let mut refused = DevicePanel::new(label.clone(), true, String::new(), Err(issue));
            *refused.controls.lock().unwrap() = Some(own());
            assert!(!refused.handle_key(key(KeyCode::Char('1')), &fleet));
            assert_eq!(refused.page, Page::Menu, "{issue:?}");
            let text = screen(&refused);
            assert!(text.contains(reason), "{text}");
            assert!(!text.contains("1  Restart"), "{text}");
        }
    }

    // #### PR #42
    // What: an offline row offers actions when the device answering at its
    // last address identifies as the make and model the row reported, at
    // any age; never when it identifies as another; and, when they cannot
    // be compared, only within ten minutes of the row going offline. Online
    // rows are not checked.
    // Why: the address can pass to another device after the row's device
    // left, and Restart would reboot that device.
    // Look here if: DevicePanel::doubt, TRUSTED_OFFLINE or the identified
    // model's text changes.
    #[test]
    fn an_offline_rows_device_is_confirmed_by_make_and_model() {
        let start = Instant::now();
        let left = start + Duration::from_secs(120);
        let nano: IpAddr = "192.168.0.127".parse().unwrap();
        let mut devices = Devices::default();
        let id = devices.connect("127.0.0.1:9201".parse().unwrap(), true, start);
        devices.set_address(id, nano);
        devices.set_worker(id, "account.rig1");
        devices.set_report(
            id,
            Some(DeviceReport {
                model: Some("Avalonminer AvalonNano3s".into()),
                ..DeviceReport::default()
            }),
        );
        devices.close(id, true, Some("SV1 disconnected"), left);
        let identified = |model: &str| DeviceControls {
            firmware: Some("AvalonMiner Stock".into()),
            model: Some(model.into()),
            actions: vec![DeviceAction::Restart, DeviceAction::Pause],
        };
        let open = |seconds: u64, found: DeviceControls| {
            let now = left + Duration::from_secs(seconds);
            let row = devices.snapshots(now).remove(0);
            let panel = DevicePanel::for_row(&devices, &row, now);
            *panel.controls.lock().unwrap() = Some(found);
            panel
        };
        // The same make and model answers: offered, however long ago.
        for seconds in [30, 3 * 3600] {
            let panel = open(seconds, identified("Avalonminer AvalonNano3s"));
            assert_eq!(
                panel.actions(),
                [DeviceAction::Restart, DeviceAction::Pause]
            );
            assert!(screen(&panel).contains("1  Restart"));
        }
        let text = screen(&open(3 * 3600, identified("Avalonminer AvalonNano3s")));
        assert!(text.contains("Offline for 3 h"), "{text}");
        // Another make or model: refused, saying what answers.
        let mut panel = open(30, identified("Bitaxe Gamma"));
        assert!(panel.actions().is_empty());
        let fleet = Arc::new(Fleet::new());
        assert!(!panel.handle_key(key(KeyCode::Char('1')), &fleet));
        assert_eq!(panel.page, Page::Menu);
        let text = screen(&panel);
        assert!(text.contains("A different device answers"), "{text}");
        assert!(text.contains("It identifies as Bitaxe Gamma."), "{text}");
        assert!(!text.contains("1  Restart"), "{text}");
        // Not identified: Pickaxe's own actions only within ten minutes.
        assert_eq!(open(9 * 60, own()).actions(), DeviceAction::OWN);
        let panel = open(11 * 60, own());
        assert!(panel.actions().is_empty());
        let text = screen(&panel);
        assert!(text.contains("went offline over 10 min ago"), "{text}");
        assert!(text.contains("cannot confirm"), "{text}");
        // An online row is the device that connected: not checked.
        let mut online = DevicePanel::new("rig2 #2".into(), true, String::new(), Ok(nano));
        online.model = Some("Avalonminer AvalonNano3s".into());
        *online.controls.lock().unwrap() = Some(identified("Bitaxe Gamma"));
        assert_eq!(online.actions().len(), 2);
    }

    // #### PR #42 (moved from command.rs, where it tested the controls page)
    // What: an action needs a choice and a confirmation; the reply of a sent
    // action shows on the panel.
    // Look here if: DevicePanel::handle_key or send changes.
    #[test]
    fn controls_need_a_choice_and_a_confirmation() {
        let fleet = Arc::new(Fleet::new());
        // A public address: Fleet refuses it before any connection, so the
        // confirmed action below sends nothing.
        let mut panel = DevicePanel::new(
            "rig1 #1".into(),
            true,
            details(Some("Avalonminer AvalonNano3s"), None, Some(140.0)),
            Ok("203.0.113.9".parse().unwrap()),
        );
        // Not identified yet: nothing to choose.
        assert!(!panel.handle_key(key(KeyCode::Char('1')), &fleet));
        assert_eq!(panel.page, Page::Menu);
        *panel.controls.lock().unwrap() = Some(own());
        // Choosing only asks for confirmation; a number past the list
        // chooses nothing.
        assert!(!panel.handle_key(key(KeyCode::Char('9')), &fleet));
        assert_eq!(panel.page, Page::Menu);
        assert!(!panel.handle_key(key(KeyCode::Char('2')), &fleet));
        assert_eq!(panel.page, Page::Confirm(DeviceAction::LowerPower));
        assert!(screen(&panel).contains("Lower power (one work level down) rig1 #1?"));
        // Any key other than y cancels, Esc included, and q never closes.
        panel.handle_key(key(KeyCode::Char('n')), &fleet);
        assert_eq!(panel.page, Page::Menu);
        panel.handle_key(key(KeyCode::Char('3')), &fleet);
        assert!(!panel.handle_key(key(KeyCode::Esc), &fleet));
        assert_eq!(panel.page, Page::Menu);
        assert!(!panel.handle_key(key(KeyCode::Char('q')), &fleet));
        assert!(panel.reply.lock().unwrap().is_none());
        // Confirmed: sent in the background, and the reply shows.
        panel.handle_key(key(KeyCode::Char('1')), &fleet);
        panel.handle_key(key(KeyCode::Char('y')), &fleet);
        let deadline = Instant::now() + Duration::from_secs(5);
        while panel
            .reply
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|reply| reply.starts_with("Sending"))
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        let text = screen(&panel);
        assert!(
            text.contains("Restart: not done. Only devices on the local network"),
            "{text}"
        );
        assert!(
            text.contains("1  Restart") && text.contains("3  Raise power"),
            "{text}"
        );
        assert!(text.contains("Not identified by asic-rs"), "{text}");
        assert!(panel.handle_key(key(KeyCode::Esc), &fleet));
    }

    // #### PR #42
    // What: a device's reply or error never shows an address: reqwest's
    // " for url (…)" part is dropped, and IPv4 and IPv6 addresses, with or
    // without brackets or a port, are hidden; ordinary replies stay as they
    // are, and a long one is cut.
    // Why: asic-rs errors carry the request's URL, which holds the device's
    // address, and the panel promises never to show it.
    // Look here if: outcome, plain, without_urls or without_addresses
    // changes.
    #[test]
    fn replies_never_show_an_address() {
        for (raw, shown) in [
            (
                "Network error: error sending request for url (http://192.168.0.5:80/api/system/pause)",
                "Network error: error sending request",
            ),
            (
                "HTTP status client error (401 Unauthorized) for url (http://[fd12::5]/cgi-bin/get_miner_conf.cgi)",
                "HTTP status client error (401 Unauthorized)",
            ),
            (
                "could not reach 100.101.102.103:4028.",
                "could not reach [address hidden].",
            ),
            ("no answer from fe80::1:80/api", "no answer from [address hidden]/api"),
            (
                "no answer from [fe80::1]:4028 or ::ffff:192.168.0.5",
                "no answer from [address hidden] or [address hidden]",
            ),
            ("code:10.0.0.2", "code:[address hidden]"),
            ("ASC 0 set info: reboot", "ASC 0 set info: reboot"),
            (
                "ASC 0 set OK (work level 1 to 2)",
                "ASC 0 set OK (work level 1 to 2)",
            ),
            (
                "asic_rs::backends failed at 12:30:45",
                "asic_rs::backends failed at 12:30:45",
            ),
        ] {
            assert_eq!(plain(raw), shown, "{raw}");
        }
        let panel = DevicePanel::new(
            "rig1 #1".into(),
            false,
            String::new(),
            Ok("192.168.0.5".parse().unwrap()),
        );
        panel.set_reply(outcome(
            DeviceAction::Pause,
            Err("Network error: error sending request for url (http://192.168.0.5/api/system/pause)"
                .into()),
        ));
        let text = screen(&panel);
        assert!(
            text.contains("Pause mining: not done. Network error: error sending request"),
            "{text}"
        );
        assert!(!text.contains("192.168"), "{text}");
        assert!(!text.contains("http"), "{text}");
        let long = outcome(DeviceAction::Restart, Ok("x".repeat(1000)));
        assert_eq!(long.chars().count(), "Restart: ".len() + REPLY_CHARS + 1);
        assert!(long.ends_with('…'));
    }

    // #### PR #42
    // What: the panel names the network class, never the address, and
    // offline times read plainly.
    // Look here if: network_class or span changes.
    #[test]
    fn the_panel_names_the_network_not_the_address() {
        for (address, class) in [
            ("192.168.1.5", "local network"),
            ("10.0.0.2", "local network"),
            ("169.254.3.3", "local network"),
            ("100.64.0.7", "Tailscale/CGNAT"),
            ("100.127.255.1", "Tailscale/CGNAT"),
            ("::ffff:100.101.102.103", "Tailscale/CGNAT"),
            ("fd12::5", "IPv6 local"),
            ("fe80::1", "IPv6 local"),
        ] {
            assert_eq!(network_class(address.parse().unwrap()), class, "{address}");
        }
        assert_eq!(
            details(
                Some("Avalonminer AvalonNano3"),
                Some("25103101"),
                Some(f64::NAN)
            ),
            "Avalonminer AvalonNano3 · firmware 25103101"
        );
        for (seconds, text) in [
            (45, "45 s"),
            (600, "10 min"),
            (3 * 3600, "3 h"),
            (3 * 86_400, "3 days"),
        ] {
            assert_eq!(span(Duration::from_secs(seconds)), text);
        }
    }
}
