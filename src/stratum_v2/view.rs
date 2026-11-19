//! The BCH ASIC server shown somewhere other than the terminal.
//!
//! What: a `ServerView` takes the server's terminal dashboard's place. Each
//! tick the server hands it a `ServerFrame`, what the terminal pages show,
//! and applies the `ServerControl`s it sent: stop, the donation's 0.5%
//! steps, Connection info's addresses and the Device panel's keys, through
//! the code the terminal's keys run.
//! Why: the desktop window (`pickaxe gui`) shows and steers the server the
//! setup starts; the server's threads, status files and saving stay as they
//! are.
//! Look here if: the window and the terminal dashboard disagree, or a
//! window control does something its terminal key does not.
// The terminal pages' own headings, columns and cells, for a view to show
// the same.
pub use super::command::{
    connect_heading, device_cells, format_difficulty, worker_cells, ConnectLine, ServeMode,
    DEVICE_COLUMNS, NO_LISTENER, TEMPLATES_HEADING, WORKER_COLUMNS,
};
use super::server::ServerStats;
use super::telemetry::DeviceSnapshot;
use crate::config::MiningNetwork;
use crate::donation::bch::BchDonation;
use crate::p2pool::launch::Light;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::OnceLock;

/// Shows ASIC servers in place of their terminal dashboard.
pub trait ServerView: Send + Sync {
    /// One tick of the server: what its pages show now.
    fn show(&self, frame: ServerFrame);
    /// What the viewer asked of the server since the last tick, oldest
    /// first.
    fn controls(&self) -> Vec<ServerControl>;
}

static SERVER_VIEW: OnceLock<Box<dyn ServerView>> = OnceLock::new();

/// Sends every later ASIC server to `view` instead of the terminal.
pub fn show_servers_in(view: Box<dyn ServerView>) -> Result<(), String> {
    SERVER_VIEW
        .set(view)
        .map_err(|_| "ASIC servers already have a view".to_owned())
}

/// Whether ASIC servers show in a view other than the terminal.
pub fn servers_shown_elsewhere() -> bool {
    SERVER_VIEW.get().is_some()
}

pub(super) fn shown_in() -> Option<&'static dyn ServerView> {
    SERVER_VIEW.get().map(Box::as_ref)
}

/// What a view asks of the server, as the terminal's keys do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerControl {
    /// Stops the server (q).
    Stop,
    /// Moves the donation one 0.5% step (←/→ on Advanced settings); saved
    /// before new jobs use it.
    Donation { up: bool },
    /// Reads this computer's network addresses again, as opening
    /// Connection info (i) does.
    ReadAddresses,
    /// Opens the Device panel of the worker with this label (Enter on its
    /// row), online or offline.
    OpenDevice(String),
    /// A key on the open Device panel.
    PanelKey(PanelKey),
    /// Closes the Device panel from any of its pages.
    ClosePanel,
}

/// The keys the Device panel reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKey {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
}

impl From<PanelKey> for KeyEvent {
    fn from(key: PanelKey) -> Self {
        let (code, modifiers) = match key {
            PanelKey::Char(c) => (KeyCode::Char(c), KeyModifiers::NONE),
            PanelKey::Enter => (KeyCode::Enter, KeyModifiers::NONE),
            PanelKey::Esc => (KeyCode::Esc, KeyModifiers::NONE),
            PanelKey::Tab => (KeyCode::Tab, KeyModifiers::NONE),
            PanelKey::BackTab => (KeyCode::BackTab, KeyModifiers::SHIFT),
            PanelKey::Backspace => (KeyCode::Backspace, KeyModifiers::NONE),
        };
        KeyEvent::new(code, modifiers)
    }
}

/// One tick of the server, as its terminal pages show it. It holds no
/// payout, credential or device address.
#[derive(Debug, Clone)]
pub struct ServerFrame {
    pub network: MiningNetwork,
    /// The overview's title: BCH or ASIC token mining.
    pub title: &'static str,
    pub mode: ServeMode,
    /// Devices mine at a pool that builds the blocks (no Job Declaration).
    pub joining: bool,
    /// The pools in failover order, or your own P2Pool node by name.
    pub pool: Option<String>,
    /// The node's client and version, such as "Bitcoin Cash Node 29.1.0".
    pub node_client: Option<String>,
    /// The server's counts. `device_stats` is left empty, since it holds
    /// device addresses; `devices` has the rows.
    pub stats: ServerStats,
    /// The workers in the table's order: online first, then by label.
    pub devices: Vec<DeviceSnapshot>,
    /// The lights over both dashboard pages: name, color and what it means.
    pub lights: Vec<(&'static str, Light, String)>,
    pub donation: BchDonation,
    /// Why the last donation change was not saved.
    pub setting_error: Option<&'static str>,
    /// The workers page's header, ending with where devices connect.
    pub header: String,
    /// The overview page's status.
    pub overview: String,
    /// What the donation pays and how it changes (Advanced settings).
    pub advanced: String,
    /// What the server started with (Advanced settings).
    pub started: String,
    /// Where to point devices.
    pub devices_hint: String,
    pub connect: ConnectInfo,
    /// The open Device panel's page.
    pub panel: Option<String>,
}

/// Connection info: every address devices and pools connect to, and what
/// to type with them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectInfo {
    /// SV1 lines first, then SV2 (see `connect_heading`).
    pub lines: Vec<ConnectLine>,
    /// Where pools take this node's templates (`TEMPLATES_HEADING`).
    pub templates: Vec<ConnectLine>,
    /// The username, password and reach notes.
    pub notes: String,
    /// What an SRI pool or Job Declaration client puts in its config.
    pub template_config: Option<String>,
}
