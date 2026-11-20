//! The window's Updates panel, opened from every top bar's Updates button
//! (with a dot while there is news), as Gupaxx has one: Pickaxe's own
//! updates (`crate::update`), with the terminal notice's states and the
//! same trust rule, and P2Pool v2's through its own updater
//! (`crate::p2pool::release`), each with its Update button and automatic
//! updates.
use super::style;
use crate::update::{self, release::Version, trust::Trust, Mode, State};
use eframe::egui;
#[cfg(feature = "stratum-v2")]
use std::path::PathBuf;
#[cfg(feature = "stratum-v2")]
use std::sync::{Arc, Mutex};

fn open_id() -> egui::Id {
    egui::Id::unique("pickaxe.updates.open")
}

/// The top bar's Updates button: a dot and the notice on hover while there
/// is news. It opens the panel.
pub fn bar_button(ui: &mut egui::Ui) {
    let notice = update::updater().and_then(update::Updater::notice);
    let text = match &notice {
        Some(_) => egui::RichText::new("● Updates").color(style::accent(ui)),
        None => egui::RichText::new("Updates"),
    };
    let hover = notice
        .as_deref()
        .unwrap_or("Pickaxe's version and updates, and P2Pool v2's");
    if ui.button(text).on_hover_text(hover).clicked() {
        ui.ctx().data_mut(|data| data.insert_temp(open_id(), true));
    }
}

/// What the panel shows of Pickaxe's own updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickaxeView {
    pub running: Version,
    pub state: State,
    pub mode: Mode,
    pub has_key: bool,
    /// An updater runs in this window (not with `--no-update-check`).
    pub checking: bool,
}

impl PickaxeView {
    fn now() -> Self {
        let updater = update::updater();
        Self {
            running: Version::running(),
            state: updater.map_or(State::Idle, update::Updater::state),
            mode: update::settings().mode(),
            has_key: update::trust::has_key(),
            checking: updater.is_some(),
        }
    }
}

/// The newest P2Pool release, as the panel shows it.
#[cfg(feature = "stratum-v2")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum P2poolLatest {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available {
        tag: String,
        prerelease: bool,
        signature: &'static str,
    },
    Failed(String),
}

/// What the panel shows of P2Pool v2.
#[cfg(feature = "stratum-v2")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct P2poolView {
    pub installed: Option<String>,
    pub latest: P2poolLatest,
    /// What runs in the background (installing, rolling back), if anything.
    pub busy: Option<String>,
    /// The last action's answer, and whether it is an error.
    pub answer: Option<(String, bool)>,
    pub auto_update: bool,
    pub can_roll_back: bool,
}

/// What a click in the panel asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Update, as U does: an unsigned release asks first.
    Update,
    /// Yes to installing an unsigned release.
    ConfirmUnsigned,
    /// No to it: keep this version.
    Keep,
    Restart,
    CheckNow,
    SetMode(Mode),
    #[cfg(feature = "stratum-v2")]
    P2poolCheck,
    #[cfg(feature = "stratum-v2")]
    P2poolUpdate,
    #[cfg(feature = "stratum-v2")]
    P2poolRollback,
    #[cfg(feature = "stratum-v2")]
    P2poolAuto(bool),
}

/// The newest release's cell.
pub fn latest_text(view: &PickaxeView) -> String {
    match &view.state {
        State::Idle if !view.checking => "not checked: checks are off in this run".into(),
        State::Idle => "not checked yet".into(),
        State::Checking => "checking…".into(),
        State::UpToDate => format!("{} is the newest", view.running),
        State::Failed { offer: None, error } => format!("unknown: {error}"),
        State::Ready { version, .. } => format!("{version}, installed"),
        state => state.offer().map_or_else(String::new, |offer| {
            format!("{} available", offer.version())
        }),
    }
}

/// The signature line for a release on offer.
pub fn signature_text(trust: &Trust) -> String {
    match trust {
        Trust::Signed { key } => format!("signed with Pickaxe's key ({key}…)"),
        Trust::Unsigned(why) => format!("Unsigned release: {}", why.why()),
    }
}

/// Draws Pickaxe's section; what was clicked.
pub fn pickaxe_section(
    ui: &mut egui::Ui,
    view: &PickaxeView,
    between_sessions: bool,
    answer: Option<&str>,
) -> Vec<Action> {
    let mut out = Vec::new();
    ui.heading("Pickaxe");
    egui::Grid::new("updates-pickaxe")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label("This version");
            ui.label(style::number(view.running.to_string(), 14.0));
            ui.end_row();
            ui.label("Newest");
            ui.horizontal(|ui| {
                ui.label(latest_text(view));
                if let Some(offer) = view.state.offer() {
                    ui.hyperlink_to("Release notes", &offer.release.notes_url);
                }
            });
            ui.end_row();
            if let Some(offer) = view.state.offer() {
                ui.label("Signature");
                let text = signature_text(&offer.trust);
                if offer.trust.is_signed() {
                    ui.add(egui::Label::new(text).wrap());
                } else {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text).color(ui.visuals().warn_fg_color),
                        )
                        .wrap(),
                    );
                }
                ui.end_row();
            }
        });
    ui.add_space(8.0);
    match &view.state {
        State::Available(offer) | State::Confirm(offer) => {
            let label = format!("Update to {}", offer.version());
            if ui.add(style::primary(ui, label)).clicked() {
                out.push(Action::Update);
            }
            if !offer.trust.is_signed() {
                ui.label("Unsigned release: update manually. Pickaxe asks before it installs it.");
            }
        }
        State::Downloading { percent, .. } => {
            ui.add(
                egui::ProgressBar::new(f32::from(*percent) / 100.0)
                    .text(format!("Updating… {percent}%")),
            );
        }
        State::Ready {
            version, waiting, ..
        } => {
            ui.label(format!("Updated to {version} · restart to apply"));
            let restart = ui
                .add_enabled(between_sessions, egui::Button::new("Restart Pickaxe"))
                .on_disabled_hover_text(
                    "Stop mining first: Pickaxe never restarts a session by itself. The update \
                     also applies the next time Pickaxe starts.",
                );
            if restart.clicked() {
                out.push(Action::Restart);
            }
            if let Some(why) = waiting {
                ui.add(egui::Label::new(egui::RichText::new(why).weak()).wrap());
            }
        }
        State::Failed {
            error,
            offer: Some(offer),
        } => {
            style::answer(
                ui,
                &format!("Update to {} failed: {error}", offer.version()),
                true,
            );
            if ui.button("Try again").clicked() {
                out.push(Action::Update);
            }
        }
        State::Checking => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking GitHub for a newer Pickaxe…");
            });
        }
        State::Idle | State::UpToDate | State::Failed { offer: None, .. } => {
            if ui
                .add_enabled(view.checking, egui::Button::new("Check now"))
                .on_disabled_hover_text("Checks are off in this run")
                .clicked()
            {
                out.push(Action::CheckNow);
            }
        }
    }
    if let Some(answer) = answer.filter(|answer| !answer.is_empty()) {
        style::answer(ui, answer, false);
    }
    ui.add_space(8.0);
    let mut auto = view.mode == Mode::On;
    let response = ui
        .checkbox(
            &mut auto,
            "Auto-update: install signed releases by themselves",
        )
        .on_hover_text(
            "A release installs by itself only when its checksum file is signed with the key \
             built into Pickaxe. It applies at the next start, never during mining.",
        );
    if response.changed() {
        out.push(Action::SetMode(if auto { Mode::On } else { Mode::Notify }));
    }
    let mut check = view.mode != Mode::Off;
    if ui
        .checkbox(&mut check, "Check for updates (at start and every 6 hours)")
        .changed()
    {
        out.push(Action::SetMode(if check {
            Mode::Notify
        } else {
            Mode::Off
        }));
    }
    if !view.has_key {
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "This build has no signing key, so no release installs by itself: you \
                     confirm each one.",
                )
                .weak(),
            )
            .wrap(),
        );
    }
    // An unsigned release a person asked for: say why, then ask.
    if let State::Confirm(offer) = &view.state {
        let mut open = true;
        let why = match &offer.trust {
            Trust::Unsigned(why) => why.why(),
            Trust::Signed { .. } => "",
        };
        let body = format!(
            "Pickaxe {} is an unsigned release: {why}. Install it only if you checked its \
             release notes ({}). It is still checked against the release's SHA256SUMS, and it \
             applies at the next start.",
            offer.version(),
            offer.release.notes_url
        );
        match style::confirm(
            ui,
            &mut open,
            "pickaxe.updates.unsigned",
            "Install an unsigned release?",
            &body,
            "Install anyway",
        ) {
            Some(true) => out.push(Action::ConfirmUnsigned),
            Some(false) => out.push(Action::Keep),
            None => {}
        }
    }
    out
}

/// Draws P2Pool v2's section; what was clicked.
#[cfg(feature = "stratum-v2")]
pub fn p2pool_section(ui: &mut egui::Ui, view: &P2poolView) -> Vec<Action> {
    let mut out = Vec::new();
    ui.heading("P2Pool v2");
    egui::Grid::new("updates-p2pool")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label("Installed");
            ui.label(view.installed.clone().unwrap_or_else(|| {
                "not installed: it installs when a pool mode first starts it".into()
            }));
            ui.end_row();
            ui.label("Newest");
            ui.label(match &view.latest {
                P2poolLatest::Unknown => "not checked yet".to_owned(),
                P2poolLatest::Checking => "checking…".to_owned(),
                P2poolLatest::UpToDate => "the installed one is the newest".to_owned(),
                P2poolLatest::Available {
                    tag, prerelease, ..
                } => format!(
                    "{tag} available{}",
                    if *prerelease { " (pre-release)" } else { "" }
                ),
                P2poolLatest::Failed(error) => format!("unknown: {error}"),
            });
            ui.end_row();
            if let P2poolLatest::Available { signature, .. } = &view.latest {
                ui.label("Signature");
                ui.add(egui::Label::new(*signature).wrap());
                ui.end_row();
            }
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let idle = view.busy.is_none();
        let available = matches!(view.latest, P2poolLatest::Available { .. });
        if ui
            .add_enabled(idle && available, egui::Button::new("Update P2Pool"))
            .clicked()
        {
            out.push(Action::P2poolUpdate);
        }
        if ui
            .add_enabled(idle && view.can_roll_back, egui::Button::new("Roll back"))
            .on_hover_text("Go back to the P2Pool version installed before this one")
            .clicked()
        {
            out.push(Action::P2poolRollback);
        }
        if ui
            .add_enabled(idle, egui::Button::new("Check now"))
            .clicked()
        {
            out.push(Action::P2poolCheck);
        }
        if let Some(busy) = &view.busy {
            ui.spinner();
            ui.label(busy);
        }
    });
    if let Some((answer, error)) = &view.answer {
        style::answer(ui, answer, *error);
    }
    let mut auto = view.auto_update;
    if ui
        .checkbox(
            &mut auto,
            "Auto-update: install a newer P2Pool before it starts",
        )
        .changed()
    {
        out.push(Action::P2poolAuto(auto));
    }
    out
}

/// The panel's own state between frames.
pub struct Panel {
    answer: Option<String>,
    #[cfg(feature = "stratum-v2")]
    p2pool: P2pool,
}

impl Panel {
    /// The panel for the configuration at `config_path` (P2Pool's folder
    /// is beside it).
    pub fn new(config_path: &std::path::Path) -> Self {
        #[cfg(not(feature = "stratum-v2"))]
        let _ = config_path;
        Self {
            #[cfg(feature = "stratum-v2")]
            p2pool: P2pool::new(crate::p2pool::home(config_path)),
            answer: None,
        }
    }

    /// Draws the panel while it is open; true when Restart was pressed
    /// (`between_sessions`: no session runs, so it may be).
    pub fn show(&mut self, ctx: &egui::Context, between_sessions: bool) -> bool {
        if !ctx.data(|data| data.get_temp(open_id()).unwrap_or(false)) {
            return false;
        }
        #[cfg(feature = "stratum-v2")]
        self.p2pool.check_once(ctx);
        let view = PickaxeView::now();
        let mut open = true;
        let mut actions = Vec::new();
        egui::Window::new("Updates")
            .id(egui::Id::unique("pickaxe.updates.window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 56.0])
            .show(ctx, |ui| {
                actions = pickaxe_section(ui, &view, between_sessions, self.answer.as_deref());
                #[cfg(feature = "stratum-v2")]
                {
                    ui.add_space(6.0);
                    ui.separator();
                    actions.extend(p2pool_section(ui, &self.p2pool.view()));
                }
            });
        if !open {
            ctx.data_mut(|data| data.insert_temp(open_id(), false));
        }
        let mut restart = false;
        for action in actions {
            restart |= self.act(ctx, action, between_sessions);
        }
        if matches!(view.state, State::Downloading { .. } | State::Checking) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        restart
    }

    fn act(&mut self, ctx: &egui::Context, action: Action, between_sessions: bool) -> bool {
        #[cfg(not(feature = "stratum-v2"))]
        let _ = ctx;
        let updater = update::updater();
        match action {
            Action::Update | Action::ConfirmUnsigned | Action::CheckNow => {
                self.answer = match updater.map(|updater| updater.press(between_sessions)) {
                    Some(update::Press::Said(text)) => Some(text),
                    Some(update::Press::Restart) => return true,
                    None => Some("Checks are off in this run (--no-update-check).".into()),
                };
            }
            Action::Keep => {
                if let Some(updater) = updater {
                    updater.dismiss();
                }
                self.answer = Some(format!("Kept Pickaxe {}.", Version::running()));
            }
            Action::Restart => return true,
            Action::SetMode(mode) => {
                let mut settings = update::settings();
                settings.set_mode(mode);
                self.answer = Some(match update::save_settings(&settings) {
                    Ok(()) if updater.is_none() && mode != Mode::Off => {
                        "Saved; checks begin the next time Pickaxe starts.".into()
                    }
                    Ok(()) => format!("Pickaxe updates: {}.", settings.describe()),
                    Err(error) => error,
                });
            }
            #[cfg(feature = "stratum-v2")]
            Action::P2poolCheck => self.p2pool.check(ctx),
            #[cfg(feature = "stratum-v2")]
            Action::P2poolUpdate => self.p2pool.update(ctx),
            #[cfg(feature = "stratum-v2")]
            Action::P2poolRollback => self.p2pool.rollback(ctx),
            #[cfg(feature = "stratum-v2")]
            Action::P2poolAuto(on) => self.p2pool.set_auto(on),
        }
        false
    }
}

/// P2Pool v2's updater, run off the window's thread: one check when the
/// panel first opens, and installs and rollbacks on request.
#[cfg(feature = "stratum-v2")]
struct P2pool {
    home: PathBuf,
    view: Arc<Mutex<P2poolView>>,
    release: Arc<Mutex<Option<crate::p2pool::release::Release>>>,
    checked: bool,
}

#[cfg(feature = "stratum-v2")]
impl P2pool {
    fn new(home: PathBuf) -> Self {
        let mut view = P2poolView::default();
        Self::read_local(&home, &mut view);
        Self {
            home,
            view: Arc::new(Mutex::new(view)),
            release: Arc::new(Mutex::new(None)),
            checked: false,
        }
    }

    /// What is on this computer: the installed version and the settings.
    fn read_local(home: &std::path::Path, view: &mut P2poolView) {
        let installed = crate::p2pool::release::installed(home).ok().flatten();
        view.installed = installed.as_ref().map(|installed| {
            format!(
                "{}{}",
                installed.tag,
                if installed.signed {
                    " (signed)"
                } else {
                    " (unsigned)"
                }
            )
        });
        view.can_roll_back = installed.is_some_and(|installed| installed.previous.is_some());
        view.auto_update = crate::p2pool::Settings::load(home)
            .map(|settings| settings.auto_update)
            .unwrap_or(true);
    }

    fn view(&self) -> P2poolView {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn check_once(&mut self, ctx: &egui::Context) {
        if !self.checked {
            self.checked = true;
            self.check(ctx);
        }
    }

    /// Runs `work` on a thread with the view marked `busy`; its answer and
    /// what it learned are shown when it ends.
    fn run(
        &self,
        ctx: &egui::Context,
        busy: Option<&str>,
        work: impl FnOnce(
                &std::path::Path,
                &Mutex<Option<crate::p2pool::release::Release>>,
                &mut P2poolView,
            ) + Send
            + 'static,
    ) {
        {
            let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
            if view.busy.is_some() {
                return;
            }
            view.busy = busy.map(str::to_owned);
            if busy.is_none() {
                view.latest = P2poolLatest::Checking;
            }
        }
        let (home, view, release, ctx) = (
            self.home.clone(),
            Arc::clone(&self.view),
            Arc::clone(&self.release),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let mut next = view.lock().unwrap_or_else(|e| e.into_inner()).clone();
            work(&home, &release, &mut next);
            next.busy = None;
            Self::read_local(&home, &mut next);
            *view.lock().unwrap_or_else(|e| e.into_inner()) = next;
            ctx.request_repaint();
        });
    }

    fn check(&self, ctx: &egui::Context) {
        self.run(ctx, None, |home, release, view| {
            use crate::p2pool::release::{check, Check};
            view.latest = match check(home) {
                Ok(Check::UpToDate(_)) => P2poolLatest::UpToDate,
                Ok(Check::Available { release: found, .. }) => {
                    let latest = P2poolLatest::Available {
                        tag: found.tag.clone(),
                        prerelease: found.prerelease,
                        signature: found.signature_text(),
                    };
                    *release.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
                    latest
                }
                Err(error) => P2poolLatest::Failed(error),
            };
        });
    }

    fn update(&self, ctx: &egui::Context) {
        self.run(ctx, Some("Installing P2Pool…"), |home, release, view| {
            let Some(found) = release.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
                view.answer = Some(("Check for a release first.".into(), true));
                return;
            };
            view.answer = Some(match crate::p2pool::release::install(home, &found) {
                Ok(installed) => {
                    view.latest = P2poolLatest::UpToDate;
                    (
                        format!(
                            "Installed P2Pool {}; it runs from P2Pool's next start.",
                            installed.tag
                        ),
                        false,
                    )
                }
                Err(error) => (error, true),
            });
        });
    }

    fn rollback(&self, ctx: &egui::Context) {
        self.run(ctx, Some("Rolling back…"), |home, _, view| {
            view.answer = Some(match crate::p2pool::release::rollback(home) {
                Ok(back) => (
                    format!(
                        "Back to P2Pool {}; it runs from P2Pool's next start.",
                        back.tag
                    ),
                    false,
                ),
                Err(error) => (error, true),
            });
            view.latest = P2poolLatest::Unknown;
        });
    }

    fn set_auto(&self, on: bool) {
        let saved = crate::p2pool::Settings::load(&self.home).and_then(|mut settings| {
            settings.auto_update = on;
            settings.save(&self.home)
        });
        let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        match saved {
            Ok(()) => view.auto_update = on,
            Err(error) => view.answer = Some((error, true)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::update::release::Release;
    use crate::update::trust::Unsigned;
    use crate::update::Offer;

    pub(crate) fn offer(signed: bool) -> Offer {
        Offer {
            release: Release {
                tag: "pickaxe-miner-v0.0.5".into(),
                version: Version(0, 0, 5),
                notes_url:
                    "https://github.com/CyberAshven/pickaxe-miner/releases/tag/pickaxe-miner-v0.0.5"
                        .into(),
                asset: "pickaxe-miner-v0.0.5-windows-x86_64.zip".into(),
                asset_url: String::new(),
                asset_size: 1,
                digest: None,
                sums_url: String::new(),
                signature_url: None,
            },
            trust: if signed {
                Trust::Signed {
                    key: "RWQf6LRCGA9i".into(),
                }
            } else {
                Trust::Unsigned(Unsigned::NoKey)
            },
            expected: [0; 32],
        }
    }

    #[test]
    fn the_newest_cell_and_signature_say_each_state() {
        let mut view = PickaxeView {
            running: Version(0, 0, 4),
            state: State::Idle,
            mode: Mode::Notify,
            has_key: false,
            checking: false,
        };
        assert_eq!(
            latest_text(&view),
            "not checked: checks are off in this run"
        );
        view.checking = true;
        assert_eq!(latest_text(&view), "not checked yet");
        view.state = State::UpToDate;
        assert_eq!(latest_text(&view), "v0.0.4 is the newest");
        view.state = State::Available(offer(false));
        assert_eq!(latest_text(&view), "v0.0.5 available");
        view.state = State::Ready {
            version: Version(0, 0, 5),
            signed: false,
            waiting: None,
        };
        assert_eq!(latest_text(&view), "v0.0.5, installed");
        assert!(signature_text(&offer(false).trust).starts_with("Unsigned release: "));
        assert!(signature_text(&offer(true).trust).starts_with("signed with Pickaxe's key"));
    }
}
