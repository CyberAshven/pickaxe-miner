//! Pickaxe's own updates (docs/updates.md), as Gupaxx and Claude Code do
//! theirs: a quiet check at start and every 6 hours, a one-line notice, a
//! download in the background, "restart to apply".
//!
//! A release installs by itself only when its checksum file is signed with
//! the key compiled into Pickaxe (`trust`); otherwise Pickaxe only shows it
//! and installs it when asked, after saying it is unsigned and why. An
//! installed update waits in `versions/` and replaces the program at the
//! next start (`install::apply`), never under a mining session and never
//! while a claim is pending; Pickaxe never restarts a session by itself.
pub mod command;
pub mod install;
pub mod release;
pub mod trust;

use release::{Release, Source, Version};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use trust::Trust;

/// How often a running Pickaxe asks GitHub again.
pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// A start this soon after the last check reads that check instead of
/// asking again (a miner restarted in a loop asks once).
const FRESH_FOR: u64 = 15 * 60;
/// The program's ancestors that are older copies of it, waiting for it
/// (`relaunch`): they hold their process marks, but run no session.
const ANCESTORS_ENV: &str = "PICKAXE_UPDATE_ANCESTORS";

/// Where Pickaxe keeps its updates: `updates/` beside the configuration,
/// with `settings.json`, `check.json`, `installed.json` and `versions/`.
pub fn home(config_path: &Path) -> PathBuf {
    config_path.with_file_name("updates")
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

pub(crate) fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

/// Writes `bytes` beside `path` first, then renames it over `path`.
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes).map_err(|error| format!("{}: {error}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|error| format!("{}: {error}", path.display()))
}

/// The running version's release tag.
pub fn running_tag() -> String {
    Version::running().tag()
}

/// What Pickaxe keeps about its updates in `updates/settings.json`. Unknown
/// fields are ignored, so an older Pickaxe (after a rollback) reads a newer
/// one's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Install signed releases by themselves. Unset, it is on when this
    /// build has a signing key, otherwise Pickaxe only tells.
    pub auto_update: Option<bool>,
    /// Off: Pickaxe never asks GitHub for its releases.
    pub check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_update: None,
            check: true,
        }
    }
}

/// The three ways the settings row and `pickaxe update set` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Signed releases install by themselves.
    On,
    /// Releases are shown; U or `pickaxe update` installs them.
    Notify,
    /// No checks at all.
    Off,
}

impl Settings {
    fn path(home: &Path) -> PathBuf {
        home.join("settings.json")
    }

    pub fn load(home: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(Self::path(home)) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", Self::path(home).display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", Self::path(home).display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        write_file(&Self::path(home), text.as_bytes())
    }

    /// Whether signed releases install by themselves.
    pub fn auto_update(&self) -> bool {
        self.check && self.auto_update.unwrap_or_else(trust::has_key)
    }

    pub fn mode(&self) -> Mode {
        match (self.check, self.auto_update()) {
            (false, _) => Mode::Off,
            (true, true) => Mode::On,
            (true, false) => Mode::Notify,
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.check = mode != Mode::Off;
        if mode != Mode::Off {
            self.auto_update = Some(mode == Mode::On);
        }
    }

    /// The settings row's value: what the mode does in this build.
    pub fn describe(&self) -> &'static str {
        match self.mode() {
            Mode::On if trust::has_key() => {
                "on: signed releases install by themselves and apply at the next start"
            }
            Mode::On => {
                "on for signed releases; this build has no signing key, so Pickaxe only tells you (U updates)"
            }
            Mode::Notify => "notify: Pickaxe tells you about a release; U installs it",
            Mode::Off => "off: Pickaxe never checks for releases",
        }
    }
}

impl Mode {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "on" => Ok(Self::On),
            "notify" => Ok(Self::Notify),
            "off" => Ok(Self::Off),
            _ => Err(format!("{text}: on, notify or off")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Notify => "notify",
            Self::Off => "off",
        }
    }

    /// The next mode the settings row steps to.
    pub fn next(self, forward: bool) -> Self {
        let order = [Self::On, Self::Notify, Self::Off];
        let index = order.iter().position(|mode| *mode == self).unwrap_or(0);
        order[(index + if forward { 1 } else { 2 }) % 3]
    }
}

/// A newer release, whether it may install by itself, and the archive's
/// SHA-256 its checksum file gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub release: Release,
    pub trust: Trust,
    pub expected: [u8; 32],
}

impl Offer {
    pub fn version(&self) -> Version {
        self.release.version
    }
}

/// Who asks for an install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// The background check, with auto-update on or off.
    Auto { enabled: bool },
    /// The person (U, the Update button, `pickaxe update`); `confirmed` once
    /// they said yes to an unsigned release.
    Person { confirmed: bool },
}

/// What an install request leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Install,
    /// Unsigned: the person must say yes first, having been told why.
    Confirm,
    /// Not by itself: unsigned, or auto-update off.
    Skip,
}

/// The trust rule: only a release signed with the compiled-in key installs
/// by itself; an unsigned one only when a person asked and confirmed.
pub fn decide(trust: &Trust, ask: Ask) -> Decision {
    match (trust.is_signed(), ask) {
        (true, Ask::Auto { enabled: true }) | (true, Ask::Person { .. }) => Decision::Install,
        (false, Ask::Person { confirmed: true }) => Decision::Install,
        (false, Ask::Person { confirmed: false }) => Decision::Confirm,
        (_, Ask::Auto { .. }) => Decision::Skip,
    }
}

/// What the updater knows and does, as the screens show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Checks are off, or none has finished yet.
    Idle,
    Checking,
    UpToDate,
    Available(Offer),
    /// An unsigned release a person asked for: waiting for their yes.
    Confirm(Offer),
    Downloading {
        offer: Offer,
        percent: u8,
    },
    /// Installed; it replaces the program at the next start. `waiting`
    /// says why a start left it waiting (another Pickaxe, a pending claim).
    Ready {
        version: Version,
        signed: bool,
        waiting: Option<String>,
    },
    /// The check (no offer) or the install (with its offer) failed.
    Failed {
        error: String,
        offer: Option<Offer>,
    },
}

impl State {
    /// The one-line notice for the terminal's screens, if there is news.
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::Idle | Self::Checking | Self::UpToDate | Self::Failed { offer: None, .. } => None,
            Self::Available(offer) if offer.trust.is_signed() => Some(format!(
                "Pickaxe {} available · U to update",
                offer.version()
            )),
            Self::Available(offer) => Some(format!(
                "Pickaxe {} available · Unsigned release: update manually (U)",
                offer.version()
            )),
            Self::Confirm(offer) => Some(format!(
                "Unsigned release {} · U again installs it, X keeps {}",
                offer.version(),
                Version::running()
            )),
            Self::Downloading { percent, .. } => Some(format!("Updating… {percent}%")),
            Self::Ready { version, .. } => Some(format!("Updated to {version} · restart to apply")),
            Self::Failed {
                offer: Some(offer), ..
            } => Some(format!(
                "Update to {} failed · U to try again",
                offer.version()
            )),
        }
    }

    /// What the notice is about, so a dismissed one stays dismissed while
    /// only its progress changes.
    fn topic(&self) -> Option<(u8, Version)> {
        match self {
            Self::Available(offer) | Self::Confirm(offer) => Some((1, offer.version())),
            Self::Downloading { offer, .. } => Some((2, offer.version())),
            Self::Ready { version, .. } => Some((3, *version)),
            Self::Failed {
                offer: Some(offer), ..
            } => Some((4, offer.version())),
            _ => None,
        }
    }

    /// The release on offer, if any.
    pub fn offer(&self) -> Option<&Offer> {
        match self {
            Self::Available(offer)
            | Self::Confirm(offer)
            | Self::Downloading { offer, .. }
            | Self::Failed {
                offer: Some(offer), ..
            } => Some(offer),
            _ => None,
        }
    }
}

/// What pressing U (or the Update button) did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// A line for the status bar.
    Said(String),
    /// Restart into the installed version: only between sessions.
    Restart,
}

enum Request {
    Install { confirmed: bool },
    Check,
}

/// The updater of this process: the background check and install, and
/// what the screens read and press.
pub struct Updater {
    home: PathBuf,
    source: Source,
    config_path: PathBuf,
    state: Mutex<State>,
    dismissed: Mutex<Option<(u8, Version)>>,
    requests: Mutex<Option<mpsc::Sender<Request>>>,
    /// Told whenever the state changes (the window repaints).
    waker: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Headless: state changes are printed to stderr.
    log: bool,
    /// Why an installed update would wait now (`blocked`; a stub in tests,
    /// which never look at this computer's other Pickaxe processes).
    blocked: fn(&Path) -> Option<String>,
}

static UPDATER: OnceLock<Updater> = OnceLock::new();
static RESTART: AtomicBool = AtomicBool::new(false);
/// `updates/` for this run, for the settings row when no updater runs.
static HOME: OnceLock<PathBuf> = OnceLock::new();

/// The updater, when this run started one.
pub fn updater() -> Option<&'static Updater> {
    UPDATER.get()
}

/// The update settings of this run, from the updater or `updates/`.
pub fn settings() -> Settings {
    HOME.get()
        .and_then(|home| Settings::load(home).ok())
        .unwrap_or_default()
}

/// Saves `settings` for this run; a running updater checks right away when
/// checks were turned on.
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let home = HOME.get().ok_or("updates are not set up in this run")?;
    let before = Settings::load(home).unwrap_or_default();
    settings.save(home)?;
    if let Some(updater) = updater() {
        if settings.check && !before.check {
            updater.send(Request::Check);
        }
        if !settings.check {
            updater.set(State::Idle);
        } else if settings.auto_update() {
            // A signed release already on offer installs now.
            if let State::Available(offer) = updater.state() {
                if decide(&offer.trust, Ask::Auto { enabled: true }) == Decision::Install {
                    updater.send(Request::Install { confirmed: false });
                }
            }
        }
    }
    Ok(())
}

/// Asks for a restart into the installed version once this run's screens
/// have closed (`restart_if_asked`).
pub fn request_restart() {
    RESTART.store(true, Ordering::SeqCst);
}

pub fn restart_requested() -> bool {
    RESTART.load(Ordering::SeqCst)
}

/// Sets this run's `updates/` folder (before anything else reads it).
pub fn init(config_path: &Path) {
    let _ = HOME.set(home(config_path));
}

/// Starts the background check for this run, once: at start (unless the
/// last check is fresh) and then every 6 hours. `log` prints its news to
/// stderr for runs without a screen; `waiting` is why the start left an
/// installed update waiting (`at_start`).
pub fn start(config_path: &Path, log: bool, waiting: Option<String>) -> Option<&'static Updater> {
    init(config_path);
    if let Some(updater) = UPDATER.get() {
        return Some(updater);
    }
    let home = home(config_path);
    if !Settings::load(&home).unwrap_or_default().check {
        return None;
    }
    let (send, receive) = mpsc::channel();
    let initial = match install::Installed::load(&home)
        .ok()
        .and_then(|record| record.pending)
    {
        Some(entry) => State::Ready {
            version: entry.version().unwrap_or(Version(0, 0, 0)),
            signed: entry.signed,
            waiting,
        },
        None => {
            if let Some(why) = waiting {
                eprintln!("{why}");
            }
            State::Idle
        }
    };
    let updater = UPDATER.get_or_init(|| Updater {
        home,
        source: Source::github(),
        config_path: config_path.to_path_buf(),
        state: Mutex::new(initial),
        dismissed: Mutex::new(None),
        requests: Mutex::new(Some(send)),
        waker: Mutex::new(None),
        log,
        blocked,
    });
    let spawned = std::thread::Builder::new()
        .name("pickaxe-update".into())
        .spawn(move || updater.run(receive));
    spawned.is_ok().then_some(updater)
}

impl Updater {
    /// An updater for tests: its own folder and source, no thread.
    #[cfg(test)]
    fn for_test(home: &Path, source: Source) -> Self {
        Self {
            home: home.to_path_buf(),
            source,
            config_path: home.join("config.json"),
            state: Mutex::new(State::Idle),
            dismissed: Mutex::new(None),
            requests: Mutex::new(None),
            waker: Mutex::new(None),
            log: false,
            blocked: |_| None,
        }
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The notice the terminal shows, unless it was dismissed.
    pub fn notice(&self) -> Option<String> {
        let state = self.state();
        let dismissed = *self.dismissed.lock().unwrap_or_else(|e| e.into_inner());
        if state.topic().is_some() && state.topic() == dismissed {
            return None;
        }
        state.notice()
    }

    /// Hides the notice until there is other news (X).
    pub fn dismiss(&self) {
        let state = self.state();
        if let State::Confirm(offer) = &state {
            // X answers the confirmation: keep this version.
            self.set(State::Available(offer.clone()));
            *self.dismissed.lock().unwrap_or_else(|e| e.into_inner()) =
                State::Available(offer.clone()).topic();
            return;
        }
        *self.dismissed.lock().unwrap_or_else(|e| e.into_inner()) = state.topic();
    }

    /// Calls `waker` whenever the state changes.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send>) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(waker);
    }

    fn set(&self, state: State) {
        let changed = {
            let mut current = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let changed = current.topic() != state.topic();
            *current = state.clone();
            changed
        };
        if self.log && changed {
            if let Some(notice) = state.notice() {
                eprintln!("update: {notice}");
            }
        }
        if let Some(waker) = &*self.waker.lock().unwrap_or_else(|e| e.into_inner()) {
            waker();
        }
    }

    fn send(&self, request: Request) {
        if let Some(send) = &*self.requests.lock().unwrap_or_else(|e| e.into_inner()) {
            let _ = send.send(request);
        }
    }

    /// U, or the Update button: installs, asks for the confirmation an
    /// unsigned release needs, or (`between_sessions`) restarts into an
    /// installed one. Never restarts a session.
    pub fn press(&self, between_sessions: bool) -> Press {
        let state = self.state();
        *self.dismissed.lock().unwrap_or_else(|e| e.into_inner()) = None;
        match state {
            State::Available(offer)
            | State::Failed {
                offer: Some(offer), ..
            } => match decide(&offer.trust, Ask::Person { confirmed: false }) {
                Decision::Confirm => {
                    let why = match &offer.trust {
                        Trust::Unsigned(why) => why.why(),
                        Trust::Signed { .. } => "",
                    };
                    let text = format!(
                        "Unsigned release {}: {why}. Press U again to install it anyway \
                             (release notes: {}), or X to keep {}.",
                        offer.version(),
                        offer.release.notes_url,
                        Version::running()
                    );
                    self.set(State::Confirm(offer));
                    Press::Said(text)
                }
                _ => {
                    self.send(Request::Install { confirmed: false });
                    Press::Said(format!("Downloading Pickaxe {}…", offer.version()))
                }
            },
            State::Confirm(offer) => {
                self.send(Request::Install { confirmed: true });
                Press::Said(format!(
                    "Downloading unsigned Pickaxe {}, as you confirmed…",
                    offer.version()
                ))
            }
            State::Downloading { percent, .. } => Press::Said(format!("Updating… {percent}%")),
            State::Ready { .. } if between_sessions => Press::Restart,
            State::Ready { version, .. } => Press::Said(format!(
                "Pickaxe {version} is installed. It never restarts a session: stop mining \
                 (B for profiles) and press U there to restart into it."
            )),
            State::UpToDate => {
                Press::Said(format!("Pickaxe {} is up to date.", Version::running()))
            }
            State::Idle | State::Checking | State::Failed { offer: None, .. } => {
                self.send(Request::Check);
                Press::Said("Checking for a newer Pickaxe…".into())
            }
        }
    }

    fn run(&self, receive: mpsc::Receiver<Request>) {
        let cache = release::Cache::load(&self.home);
        let age = now().saturating_sub(cache.checked_at);
        let mut next = if cache.checked_at > 0 && age < FRESH_FOR {
            // Checked moments ago: show what that check found.
            if let Some(release) = cache.release {
                self.consider(Ok(release));
            }
            Instant::now() + CHECK_EVERY.saturating_sub(Duration::from_secs(age))
        } else {
            Instant::now()
        };
        loop {
            let wait = next.saturating_duration_since(Instant::now());
            match receive.recv_timeout(wait) {
                Ok(Request::Install { confirmed }) => self.install(Ask::Person { confirmed }),
                Ok(Request::Check) => {
                    self.check();
                    next = Instant::now() + CHECK_EVERY;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.check();
                    next = Instant::now() + CHECK_EVERY;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// One check; a signed release installs when auto-update is on.
    fn check(&self) {
        if !Settings::load(&self.home).unwrap_or_default().check {
            self.set(State::Idle);
            return;
        }
        if matches!(
            self.state(),
            State::Downloading { .. } | State::Ready { .. }
        ) {
            // One update at a time; the next start checks again.
            return;
        }
        if !matches!(self.state(), State::Available(_) | State::Confirm(_)) {
            self.set(State::Checking);
        }
        let checked = release::check(&self.source, &self.home, now());
        self.consider(checked);
    }

    /// Shows what a check found; a signed release installs by itself when
    /// auto-update is on.
    fn consider(&self, checked: Result<Release, String>) {
        if matches!(
            self.state(),
            State::Downloading { .. } | State::Ready { .. }
        ) {
            return;
        }
        self.offer_or_up_to_date(checked);
        let settings = Settings::load(&self.home).unwrap_or_default();
        self.install(Ask::Auto {
            enabled: settings.auto_update(),
        });
    }

    fn offer_or_up_to_date(&self, checked: Result<Release, String>) {
        match checked
            .and_then(|release| offer(&self.source, &self.home, release, trust::TRUSTED_KEYS))
        {
            Ok(Some(offer)) => {
                // The confirmation a person is being asked stays.
                if !matches!(self.state(), State::Confirm(ref asked) if *asked == offer) {
                    self.set(State::Available(offer));
                }
            }
            Ok(None) => self.set(State::UpToDate),
            Err(error) => self.set(State::Failed { error, offer: None }),
        }
    }

    /// Installs the release on offer if `ask` may (one download at a time).
    fn install(&self, ask: Ask) {
        let offer = match self.state() {
            State::Available(offer)
            | State::Confirm(offer)
            | State::Failed {
                offer: Some(offer), ..
            } => offer,
            _ => return,
        };
        match decide(&offer.trust, ask) {
            Decision::Skip => return,
            Decision::Confirm => {
                self.set(State::Confirm(offer));
                return;
            }
            Decision::Install => {}
        }
        self.set(State::Downloading {
            offer: offer.clone(),
            percent: 0,
        });
        let progress = |percent: u8| {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let State::Downloading { percent: shown, .. } = &mut *state {
                *shown = percent;
            }
            drop(state);
            if let Some(waker) = &*self.waker.lock().unwrap_or_else(|e| e.into_inner()) {
                waker();
            }
        };
        let installed =
            install::download(&self.source, &offer.release, &progress).and_then(|archive| {
                install::install_archive(
                    &self.home,
                    &offer.release.tag,
                    &offer.release.asset,
                    &archive,
                    offer.expected,
                    offer.trust.is_signed(),
                )
            });
        match installed {
            Ok(entry) => self.set(State::Ready {
                version: offer.version(),
                signed: entry.signed,
                waiting: (self.blocked)(&self.config_path),
            }),
            Err(error) => self.set(State::Failed {
                error,
                offer: Some(offer),
            }),
        }
    }
}

/// The offer for `release`, when it is newer than what runs and newer than
/// what is installed and waiting: its checksum file and signature checked
/// against `keys`, the archive's SHA-256 taken from the checksum file and
/// compared with GitHub's own record of it.
pub fn offer(
    source: &Source,
    home: &Path,
    release: Release,
    keys: &[&str],
) -> Result<Option<Offer>, String> {
    if release.version <= Version::running() {
        return Ok(None);
    }
    if let Some(pending) = install::Installed::load(home)?.pending {
        if pending
            .version()
            .is_some_and(|version| version >= release.version)
        {
            return Ok(None);
        }
    }
    let (sums, signature) = release::signature_files(source, home, &release)?;
    let trust = trust::verify(sums.as_bytes(), signature.as_deref(), keys);
    let expected = trust::sums_entry(&sums, &release.asset)?;
    if let Some(digest) = &release.digest {
        if *digest != hex::encode(expected) {
            return Err(format!(
                "{}: the checksum file and GitHub's own record of the archive disagree",
                release.asset
            ));
        }
    }
    Ok(Some(Offer {
        release,
        trust,
        expected,
    }))
}

/// Why an update must not replace the program now, if it must not: another
/// Pickaxe runs (its session may hold a claim), or a claim of an earlier
/// session is still pending in its journal.
pub fn blocked(config_path: &Path) -> Option<String> {
    let ancestors = ancestors();
    if crate::mining_lock::other_pickaxe_running(&ancestors) {
        return Some("another Pickaxe is running; the update applies when it has stopped".into());
    }
    let mut folders = vec![config_path
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf)];
    if let Ok(owner_pots) = crate::mining_lock::electric_journal_dir() {
        folders.push(owner_pots);
    }
    pending_claim(&folders)
        .map(|file| format!("a claim is pending ({file}); the update applies once it settles"))
}

/// A claim journal in `folders` that still holds a claim: an ELECTRIC
/// pot's `*.pending.json` with a pending claim, or a coordinator's
/// `*.rig-winners.json` with winners not yet claimed.
fn pending_claim(folders: &[PathBuf]) -> Option<String> {
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pot = name.ends_with(".pending.json");
            let rigs = name.ends_with(".rig-winners.json");
            if !pot && !rigs {
                continue;
            }
            let held = std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_none_or(|json| {
                    if pot {
                        !json["pending"].is_null()
                    } else {
                        json.as_array().is_none_or(|winners| !winners.is_empty())
                    }
                });
            if held {
                return Some(name);
            }
        }
    }
    None
}

fn ancestors() -> Vec<u32> {
    std::env::var(ANCESTORS_ENV)
        .unwrap_or_default()
        .split(',')
        .filter_map(|pid| pid.trim().parse().ok())
        .collect()
}

/// At a start: removes what earlier swaps set aside, and puts a pending
/// update in place and runs it instead of this program (never returning)
/// when nothing blocks it. Otherwise returns why it waits, if it does.
/// `window`: the new program is started on its own (the desktop window
/// needs no console to wait in).
pub fn at_start(config_path: &Path, window: bool) -> Option<String> {
    init(config_path);
    let program = std::env::current_exe().ok()?;
    install::clean_aside(&program);
    apply_now(config_path, &program, window)
}

/// Puts a pending update in place now and runs it; why not, otherwise.
fn apply_now(config_path: &Path, program: &Path, window: bool) -> Option<String> {
    let home = home(config_path);
    match install::apply(&home, program, &running_tag(), || blocked(config_path)) {
        Ok(install::Applied::Nothing) => None,
        Ok(install::Applied::Waiting { tag, why }) => Some(format!(
            "Pickaxe {} is installed and waits: {why}",
            Version::parse(&tag).map_or(tag, |version| version.to_string())
        )),
        Ok(install::Applied::Swapped { program, .. }) => relaunch(&program, window),
        Err(error) => Some(format!("update: {error}")),
    }
}

/// After this run's screens closed: when a restart was asked for, puts the
/// installed version in place and runs it. Returns why not, if it did not.
pub fn restart_if_asked(config_path: &Path, window: bool) -> Option<String> {
    if !RESTART.swap(false, Ordering::SeqCst) {
        return None;
    }
    let program = std::env::current_exe().ok()?;
    Some(
        apply_now(config_path, &program, window)
            .unwrap_or_else(|| "No installed update waits; Pickaxe was not restarted.".into()),
    )
}

/// Runs `program` (the new version, now in this program's place) with this
/// run's arguments in place of this process.
fn relaunch(program: &Path, window: bool) -> ! {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = window;
        let error = std::process::Command::new(program).args(&args).exec();
        eprintln!("error: start {}: {error}", program.display());
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let mut ancestors = ancestors();
        ancestors.push(std::process::id());
        let ancestors = ancestors
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut command = crate::child_process::command(program);
        command.args(&args).env(ANCESTORS_ENV, ancestors);
        if window {
            // The window runs on its own; this process just ends.
            match command.spawn() {
                Ok(_) => std::process::exit(0),
                Err(error) => {
                    eprintln!("error: start {}: {error}", program.display());
                    std::process::exit(1);
                }
            }
        }
        // The terminal waits for this process, so it waits for the new one
        // and ends with it; Ctrl+C reaches the new one, not this one.
        let ignore = std::sync::Arc::new(AtomicBool::new(false));
        let _ = crate::session_control::install_stop(&ignore);
        match command.status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(error) => {
                eprintln!("error: start {}: {error}", program.display());
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pickaxe-update-{name}-{:016x}",
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests;
