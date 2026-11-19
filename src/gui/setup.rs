//! The window's mining setup: the terminal setup itself
//! (`tui::SetupDriver`), with its steps, rows and words, drawn with egui.
//! A click does what the terminal's key on that row does, and the
//! terminal's keys work here too while no widget has the keyboard.
use crate::backend::{BackendKind, DeviceSelection};
use crate::config::RuntimeConfig;
use crate::tui::{
    EditView, ItemKind, Piece, PotView, SetupDriver, SetupInput, SetupItem, SetupKey, SetupOutcome,
    SetupOverrides, SetupResult, SetupView, Tone,
};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// What a setup is made from: the saved settings and the command line, and
/// where the settings are kept.
#[derive(Clone)]
pub struct Inputs {
    pub cfg: RuntimeConfig,
    pub config_path: PathBuf,
    pub backend: BackendKind,
    pub selection: DeviceSelection,
    /// What the command line changes in a saved profile opened.
    pub overrides: SetupOverrides,
    /// The command line names the GPUs, so the ones it chose win over a
    /// profile's.
    pub cli_gpus: bool,
}

/// How the setup ended.
pub enum Done {
    /// Start was pressed: the profile is saved, and this is its session.
    Start(Box<SetupResult>),
    /// The setup was left on its first screen, and the miner confirmed it.
    Quit,
}

/// Green for what is ready, as the dashboard's chart draws it.
const GOOD: egui::Color32 = egui::Color32::from_rgb(105, 219, 124);
/// Start's fill: the one filled button of the setup.
const START_FILL: egui::Color32 = egui::Color32::from_rgb(35, 134, 54);
/// The width of a row's name, so values line up.
const NAME_WIDTH: f32 = 150.0;
/// From this width the fee pot shows beside the settings; under them when
/// narrower.
const POT_BESIDE: f32 = 900.0;

enum State {
    /// Finding the GPUs and reading the saved profiles, on another thread.
    Preparing(mpsc::Receiver<Result<SetupDriver, String>>),
    Failed(String),
    Ready(Box<SetupDriver>),
    /// A step that waits on the network (Start reading ELECTRIC's fee
    /// pot, `--start`), on another thread; what the wait shows.
    Busy(
        mpsc::Receiver<(Box<SetupDriver>, SetupOutcome)>,
        &'static str,
    ),
}

pub struct Setup {
    /// The command line's inputs, until a session has run; after that the
    /// setup is made from what is saved alone, as the terminal's return to
    /// its profiles is.
    first: Option<Inputs>,
    config_path: PathBuf,
    state: State,
    /// Why the last session stopped, for the status line once the setup is
    /// ready.
    notice: Option<String>,
    /// The first screen was left (Esc, Q): the window asks before closing.
    quitting: bool,
}

impl Setup {
    pub fn new(inputs: Inputs) -> Self {
        let config_path = inputs.config_path.clone();
        Self {
            state: prepare(Some(inputs.clone()), config_path.clone()),
            first: Some(inputs),
            config_path,
            notice: None,
            quitting: false,
        }
    }

    /// A setup that is ready (screen previews).
    #[cfg(all(test, feature = "gui-preview"))]
    pub fn ready(driver: SetupDriver) -> Self {
        Self {
            first: None,
            config_path: PathBuf::new(),
            state: State::Ready(Box::new(driver)),
            notice: None,
            quitting: false,
        }
    }

    /// A setup whose step on another thread has just ended with `outcome`.
    #[cfg(all(test, feature = "gui-preview"))]
    fn finishing(driver: SetupDriver, outcome: SetupOutcome) -> Self {
        let (send, receive) = mpsc::channel();
        send.send((Box::new(driver), outcome)).unwrap();
        Self {
            first: None,
            config_path: PathBuf::new(),
            state: State::Busy(receive, "Testing…"),
            notice: None,
            quitting: false,
        }
    }

    /// Makes the setup again after a session from what is saved, without
    /// the command line, as the terminal does when a session returns to its
    /// profiles; `notice` says why it stopped.
    pub fn reload(&mut self, notice: Option<String>) {
        self.first = None;
        self.state = prepare(None, self.config_path.clone());
        self.notice = notice;
        self.quitting = false;
    }

    /// Something runs on another thread, so the window looks again soon.
    pub fn waiting(&self) -> bool {
        matches!(self.state, State::Preparing(_) | State::Busy(..))
    }

    /// Takes what the other threads finished; how the setup ended when a
    /// step that waited on the network ended it.
    pub fn poll(&mut self) -> Option<Done> {
        match &self.state {
            State::Preparing(receive) => match receive.try_recv() {
                Ok(Ok(mut driver)) => {
                    if let Some(notice) = self.notice.take() {
                        driver.set_status(notice);
                    }
                    self.state = State::Ready(Box::new(driver));
                }
                Ok(Err(error)) => self.state = State::Failed(error),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.state = State::Failed("looking for GPUs stopped".into())
                }
            },
            State::Busy(receive, _) => match receive.try_recv() {
                Ok((driver, outcome)) => {
                    self.state = State::Ready(driver);
                    return self.ended(outcome);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.reload(Some("The setup stopped unexpectedly.".into()))
                }
            },
            State::Ready(_) | State::Failed(_) => {}
        }
        None
    }

    fn ended(&mut self, outcome: SetupOutcome) -> Option<Done> {
        match outcome {
            SetupOutcome::Continue => None,
            SetupOutcome::Complete(result) => Some(Done::Start(result)),
            SetupOutcome::Cancel => {
                self.quitting = true;
                None
            }
        }
    }

    /// `pickaxe gui --start`: once the setup is ready, starts the profile
    /// Start last started, or the first, as Enter twice on it would (`poll`
    /// gives its session). False while the GPUs are looked for.
    pub fn start_now(&mut self) -> bool {
        match std::mem::replace(&mut self.state, State::Failed(String::new())) {
            State::Ready(driver) => {
                self.state = busy(driver, "Opening your profile…", SetupDriver::start_now);
                true
            }
            State::Failed(error) => {
                self.state = State::Failed(error);
                true
            }
            other => {
                self.state = other;
                false
            }
        }
    }

    /// Draws the setup; how it ended, if it did.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Done> {
        // A step that ended on another thread comes first: a Start it ended
        // with has saved its profile already.
        if let Some(done) = self.poll() {
            return Some(done);
        }
        let inputs = match &mut self.state {
            State::Preparing(_) => {
                waiting(ui, "Looking for GPUs and reading your profiles…");
                ui.ctx().request_repaint_after(Duration::from_millis(200));
                return None;
            }
            State::Busy(_, what) => {
                waiting(ui, what);
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                return None;
            }
            State::Failed(error) => {
                let error = error.clone();
                if failed(ui, &error) {
                    // Again with what failed: the command line's, if no
                    // session ran yet.
                    self.state = prepare(self.first.clone(), self.config_path.clone());
                }
                return None;
            }
            State::Ready(driver) => {
                let wait = driver.tick();
                ui.ctx().request_repaint_after(wait);
                draw(ui, &driver.view(), self.quitting)
            }
        };
        if self.quitting {
            return self.confirm_quit(ui);
        }
        for input in inputs {
            let State::Ready(driver) = &mut self.state else {
                break;
            };
            if driver.reads_pot(&input) {
                if let State::Ready(driver) =
                    std::mem::replace(&mut self.state, State::Failed(String::new()))
                {
                    self.state = busy(driver, "Reading the fee pot's balance…", move |driver| {
                        driver.act(input)
                    });
                }
                break;
            }
            let outcome = driver.act(input);
            let go_on = matches!(outcome, SetupOutcome::Continue);
            if let Some(done) = self.ended(outcome) {
                return Some(done);
            }
            // Esc or Q left the first screen: the rest waits for the answer.
            if !go_on {
                break;
            }
        }
        None
    }

    /// "Quit Pickaxe?": the terminal quits on Esc or Q on its first screen;
    /// the window asks first. Enter or Quit closes it, Esc or Stay stays.
    fn confirm_quit(&mut self, ui: &mut egui::Ui) -> Option<Done> {
        let mut quit = false;
        let mut stay = false;
        let modal = egui::Modal::new(egui::Id::unique("pickaxe.setup.quit")).show(ui.ctx(), |ui| {
            ui.set_width(260.0);
            ui.heading("Quit Pickaxe?");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                quit = ui.button("Quit").clicked();
                stay = ui.button("Stay").clicked();
            });
        });
        // Enter answers Quit unless it presses a focused button.
        quit |= ui.memory(|memory| memory.focused().is_none())
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if quit {
            return Some(Done::Quit);
        }
        if stay || modal.should_close() {
            self.quitting = false;
        }
        None
    }
}

/// Runs `step` on another thread, for steps that wait on the network.
fn busy(
    mut driver: Box<SetupDriver>,
    what: &'static str,
    step: impl FnOnce(&mut SetupDriver) -> SetupOutcome + Send + 'static,
) -> State {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = step(&mut driver);
        let _ = send.send((driver, outcome));
    });
    State::Busy(receive, what)
}

/// Finds the GPUs and reads the saved profiles and connections on another
/// thread, as `pickaxe mine` does before its setup: from `first`, or from
/// what is saved alone.
fn prepare(first: Option<Inputs>, config_path: PathBuf) -> State {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let inputs = match first {
            Some(inputs) => Ok(inputs),
            None => saved_inputs(&config_path),
        };
        let _ = send.send(inputs.and_then(setup_driver));
    });
    State::Preparing(receive)
}

/// The inputs `pickaxe` starts again from for its profiles: the saved
/// configuration alone, its backend and GPUs, and no command line
/// (`run_application` in src/main.rs).
fn saved_inputs(config_path: &Path) -> Result<Inputs, String> {
    let saved = crate::config::SavedConfig::load_optional(config_path)?;
    let mut cfg = RuntimeConfig::default();
    let mut backend = BackendKind::Auto;
    let mut selection = DeviceSelection::Default;
    let mut integrated = false;
    if let Some(saved) = &saved {
        saved.apply_to_runtime(&mut cfg)?;
        if let Some(value) = &saved.backend {
            backend = BackendKind::parse(value)?;
        }
        if let Some(devices) = &saved.device {
            selection = devices.selection();
        }
        integrated = saved.include_integrated == Some(true);
    }
    Ok(Inputs {
        cfg,
        config_path: config_path.to_path_buf(),
        backend,
        selection: selection.with_integrated(integrated),
        overrides: SetupOverrides::default(),
        cli_gpus: false,
    })
}

fn setup_driver(inputs: Inputs) -> Result<SetupDriver, String> {
    use crate::config;
    let profiles_path = config::profiles_path(&inputs.config_path);
    let sources_path = config::sources_path(&inputs.config_path);
    let mut sources = config::SharedSources::load_optional(&sources_path)?;
    if config::SavedConfig::load_optional(&inputs.config_path)?
        .is_some_and(|saved| sources.adopt_saved_config(&saved))
    {
        sources.save(&sources_path)?;
    }
    let mut profiles = config::MiningProfiles::load_optional(&profiles_path)?;
    // Servers and nodes once saved inside profiles move to the shared
    // per-network lists; those are saved before the profiles.
    if sources.adopt_profile_sources(&mut profiles) {
        sources.save(&sources_path)?;
        profiles.save(&profiles_path)?;
    }
    // The setup lists each physical GPU once, numbered as `--device`
    // numbers them; a named backend lists its own.
    let devices = if inputs.backend == BackendKind::Auto {
        crate::backend::mining_gpus()
    } else {
        crate::backend::list_devices(inputs.backend)?
    };
    let chosen = crate::backend::resolve_mining_devices(inputs.backend, &inputs.selection)
        .unwrap_or_default();
    let mut overrides = inputs.overrides;
    if inputs.cli_gpus {
        overrides.gpus = Some(chosen.iter().map(|gpu| (gpu.backend, gpu.index)).collect());
    }
    SetupDriver::new(
        inputs.cfg,
        devices,
        inputs.backend,
        &chosen,
        &profiles_path,
        profiles,
        &sources_path,
        sources,
        &config::node_cookies_path(&inputs.config_path),
        overrides,
    )
}

fn waiting(ui: &mut egui::Ui, what: &str) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(what);
        });
    });
}

/// Why the setup could not be made; true when Look again is pressed.
fn failed(ui: &mut egui::Ui, error: &str) -> bool {
    let mut again = false;
    egui::CentralPanel::default().show(ui, |ui| {
        ui.add_space(24.0);
        ui.colored_label(ui.visuals().error_fg_color, error);
        ui.add_space(8.0);
        again = ui.button("Look again").clicked();
    });
    again
}

/// The open field's and the token search's ids.
fn edit_id() -> egui::Id {
    egui::Id::unique("pickaxe.setup.edit")
}

fn search_id() -> egui::Id {
    egui::Id::unique("pickaxe.setup.search")
}

/// Draws the screen; what was clicked or typed, in order.
fn draw(ui: &mut egui::Ui, view: &SetupView, quitting: bool) -> Vec<SetupInput> {
    let mut out = Vec::new();
    if !quitting {
        keyboard(ui, &mut out);
    }
    let wide = ui.available_width() >= POT_BESIDE;
    egui::Panel::top("setup-bar").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Pickaxe").size(20.0).strong());
            ui.separator();
            ui.label(&view.heading);
        });
        ui.add_space(4.0);
    });
    egui::Panel::bottom("setup-actions").show(ui, |ui| actions(ui, view, &mut out));
    let side = view.pot.as_ref().filter(|pot| !pot.full);
    if let Some(pot) = side.filter(|_| wide) {
        egui::Panel::right("setup-pot")
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::both().show(ui, |ui| pot_panel(ui, pot, 4.0));
            });
    }
    egui::CentralPanel::default().show(ui, |ui| {
        match view.pot.as_ref().filter(|pot| pot.full) {
            // The QR code keeps its size to scan; it scrolls when the window
            // is smaller.
            Some(pot) => {
                egui::ScrollArea::both().show(ui, |ui| pot_panel(ui, pot, 8.0));
            }
            None => {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    page(ui, view, &mut out);
                    if let Some(pot) = side.filter(|_| !wide) {
                        ui.add_space(10.0);
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                egui::ScrollArea::horizontal()
                                    .id_salt("setup-pot-under")
                                    .show(ui, |ui| pot_panel(ui, pot, 4.0));
                            });
                    }
                });
            }
        }
    });
    out
}

/// The terminal's keys, while no widget has the keyboard: Tab, Space and
/// Enter work a focused button, and a field keeps its typing. The token
/// search passes Up and Down on, to move through the list it filters.
fn keyboard(ui: &mut egui::Ui, out: &mut Vec<SetupInput>) {
    let focused = ui.memory(|memory| memory.focused());
    let searching = focused == Some(search_id());
    if focused.is_some() && !searching {
        return;
    }
    let events = ui.input(|input| input.events.clone());
    for event in events {
        match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if !(modifiers.ctrl || modifiers.alt || modifiers.command) => {
                let key = match key {
                    egui::Key::ArrowUp => SetupKey::Up,
                    egui::Key::ArrowDown => SetupKey::Down,
                    _ if searching => continue,
                    egui::Key::ArrowLeft => SetupKey::Left,
                    egui::Key::ArrowRight => SetupKey::Right,
                    egui::Key::Enter => SetupKey::Enter,
                    egui::Key::Escape => SetupKey::Esc,
                    egui::Key::Delete => SetupKey::Delete,
                    egui::Key::Backspace => SetupKey::Backspace,
                    _ => continue,
                };
                out.push(SetupInput::Key(key));
            }
            egui::Event::Text(text) if !searching => {
                out.extend(text.chars().map(|ch| SetupInput::Key(SetupKey::Char(ch))));
            }
            _ => {}
        }
    }
}

/// The status line, Back and the screen's commands, and the terminal's
/// keys for it.
fn actions(ui: &mut egui::Ui, view: &SetupView, out: &mut Vec<SetupInput>) {
    ui.add_space(6.0);
    if !view.status.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(&view.status).strong()).wrap());
        ui.add_space(4.0);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button(view.back).clicked() {
            out.push(SetupInput::Back);
        }
        for (command, label) in &view.commands {
            if ui.button(*label).clicked() {
                out.push(SetupInput::Command(*command));
            }
        }
    });
    ui.add_space(2.0);
    ui.add(egui::Label::new(egui::RichText::new(view.keys).weak()).wrap());
    ui.add_space(4.0);
}

fn color(ui: &egui::Ui, tone: Tone) -> egui::Color32 {
    match tone {
        Tone::Plain => ui.visuals().text_color(),
        Tone::Dim => ui.visuals().weak_text_color(),
        Tone::Warn => ui.visuals().warn_fg_color,
        Tone::Good => GOOD,
    }
}

/// Pieces of setup text as one label's text; a line's leading spaces (the
/// terminal's indents) are left out.
fn text(ui: &egui::Ui, pieces: &[Piece]) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    for (index, piece) in pieces.iter().enumerate() {
        let shown = if index == 0 {
            piece.text.trim_start()
        } else {
            piece.text.as_str()
        };
        job.append(
            shown,
            0.0,
            egui::TextFormat::simple(font.clone(), color(ui, piece.tone)),
        );
    }
    job
}

/// A terminal bar ("█████░░░░░  50%"): how full it is, and its percent.
fn bar(pieces: &[Piece]) -> Option<(f32, String)> {
    let [piece] = pieces else {
        return None;
    };
    let (blocks, percent) = piece.text.trim().split_once(' ')?;
    if blocks.is_empty() || !blocks.chars().all(|ch| ch == '█' || ch == '░') {
        return None;
    }
    let percent = percent.trim();
    let value: f32 = percent.strip_suffix('%')?.parse().ok()?;
    Some((value / 100.0, percent.to_owned()))
}

/// What a row's hint says besides the terminal's keys, which the window's
/// buttons and clicks stand for: "< >  0.5% steps" says "0.5% steps".
fn hint_words(hint: &str) -> String {
    const KEYS: [&str; 7] = [
        "[Enter] or < > change",
        "[Enter] change",
        "[Enter] list",
        "[Enter] done",
        "[Enter]",
        "< >",
        "·",
    ];
    let words = KEYS
        .iter()
        .fold(hint.to_owned(), |words, key| words.replace(key, " "));
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An item's value: a bar drawn as one, or its text.
fn value(ui: &mut egui::Ui, pieces: &[Piece]) {
    match bar(pieces) {
        Some((fraction, percent)) => {
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(percent)
                    .desired_width(ui.available_width().clamp(80.0, 220.0)),
            );
        }
        None => {
            ui.add(egui::Label::new(text(ui, pieces)).wrap());
        }
    }
}

fn lines(ui: &mut egui::Ui, lines: &[Vec<Piece>]) {
    for line in lines {
        if line.iter().all(|piece| piece.text.trim().is_empty()) {
            ui.add_space(6.0);
        } else {
            ui.add(egui::Label::new(text(ui, line)).wrap());
        }
    }
}

/// The screen's title and rows: rows to choose or change, lines of text,
/// and Start.
fn page(ui: &mut egui::Ui, view: &SetupView, out: &mut Vec<SetupInput>) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong(&view.title);
            ui.add_space(6.0);
            if let Some(search) = &view.search {
                search_box(ui, search, view.search_hint, out);
                ui.add_space(6.0);
            }
            for (index, item) in view.items.iter().enumerate() {
                match item.kind {
                    ItemKind::Info => lines(ui, std::slice::from_ref(&item.value)),
                    ItemKind::Start => start(ui, view, index, item, out),
                    _ => row(ui, view, index, out),
                }
            }
            // A field of its own (another computer, a cookie file).
            if let Some(edit) = view.edit.as_ref().filter(|edit| edit.item.is_none()) {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(&edit.label);
                    field(ui, edit, label.id, out);
                });
            }
        });
}

fn search_box(ui: &mut egui::Ui, search: &str, hint: &'static str, out: &mut Vec<SetupInput>) {
    ui.horizontal(|ui| {
        let label = ui.label("Search");
        let mut typed = search.to_owned();
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut typed)
                    .id(search_id())
                    .hint_text(hint)
                    .desired_width(ui.available_width().min(460.0)),
            )
            .labelled_by(label.id);
        if response.changed() {
            out.push(SetupInput::Search(typed));
        }
        if response.lost_focus() {
            if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                out.push(SetupInput::Key(SetupKey::Enter));
            } else if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                // Esc goes back at once, as on the terminal's token list.
                out.push(SetupInput::Back);
            }
        }
    });
}

/// One row: its name; its value or controls, or its open field; then what
/// its hint says besides keys, and its own buttons.
fn row(ui: &mut egui::Ui, view: &SetupView, index: usize, out: &mut Vec<SetupInput>) {
    let item = &view.items[index];
    // While a field is open, the other rows wait, as in the terminal.
    let free = view.edit.is_none();
    let edit = view.edit.as_ref().filter(|edit| edit.item == Some(index));
    ui.horizontal(|ui| {
        let left = ui.cursor().min.x;
        let label = name(ui, item, free);
        if label.clicked() {
            out.push(match item.kind {
                ItemKind::Cycle { .. } => SetupInput::Select(index),
                _ => SetupInput::Click(index),
            });
        }
        let used = ui.cursor().min.x - left;
        if used < NAME_WIDTH {
            ui.add_space(NAME_WIDTH - used);
        }
        if let Some(edit) = edit {
            field(ui, edit, label.id, out);
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (command, text) in item.actions.iter().rev() {
                if ui
                    .add_enabled(free, egui::Button::new(*text).small())
                    .accessible_name(format!("{text} {}", item.label))
                    .clicked()
                {
                    out.push(SetupInput::Row {
                        item: index,
                        command: *command,
                    });
                }
            }
            let words = hint_words(&item.hint);
            if !words.is_empty() {
                ui.weak(words);
            }
            ui.with_layout(
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| match item.kind {
                    ItemKind::Cycle { enter } => cycle(ui, free, index, item, enter, out),
                    ItemKind::Toggle { .. } => value(ui, &item.value),
                    // The whole row chooses, opens or edits it.
                    _ if item.value.iter().any(|piece| !piece.text.trim().is_empty()) => {
                        let value = egui::Button::new(text(ui, &item.value))
                            .frame_when_inactive(false)
                            .wrap();
                        if ui.add_enabled(free, value).clicked() {
                            out.push(SetupInput::Click(index));
                        }
                    }
                    _ => {}
                },
            );
        });
    });
}

/// A row's name, under the cursor when highlighted: a button where a click
/// does what Enter does, a checkbox for a toggle, and plain text for a
/// value its < > change (a click only moves the cursor there).
fn name(ui: &mut egui::Ui, item: &SetupItem, free: bool) -> egui::Response {
    let highlight = egui::Frame::new()
        .fill(if item.selected {
            ui.visuals().selection.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        })
        .corner_radius(4)
        // As much as a button's, so every row's name starts in one column.
        .inner_margin(egui::Margin::from(ui.spacing().button_padding));
    match item.kind {
        ItemKind::Toggle { on } => {
            let mut on = on;
            highlight
                .show(ui, |ui| {
                    ui.add_enabled(free, egui::Checkbox::new(&mut on, item.label.as_str()))
                })
                .inner
        }
        ItemKind::Cycle { .. } => {
            highlight
                .show(ui, |ui| {
                    ui.add_enabled(
                        free,
                        egui::Label::new(item.label.as_str())
                            .sense(egui::Sense::click())
                            .selectable(false),
                    )
                })
                .inner
        }
        _ => ui.add_enabled(
            free,
            egui::Button::selectable(item.selected, item.label.as_str()),
        ),
    }
}

/// A value < > step (the terminal's Left and Right), and the button for
/// what Enter also does there.
fn cycle(
    ui: &mut egui::Ui,
    free: bool,
    index: usize,
    item: &SetupItem,
    enter: Option<&'static str>,
    out: &mut Vec<SetupInput>,
) {
    if ui
        .add_enabled(free, egui::Button::new("<").small())
        .accessible_name(format!("{}: previous", item.label))
        .clicked()
    {
        out.push(SetupInput::Step {
            item: index,
            forward: false,
        });
    }
    value(ui, &item.value);
    if ui
        .add_enabled(free, egui::Button::new(">").small())
        .accessible_name(format!("{}: next", item.label))
        .clicked()
    {
        out.push(SetupInput::Step {
            item: index,
            forward: true,
        });
    }
    if let Some(enter) = enter {
        if ui
            .add_enabled(free, egui::Button::new(enter).small())
            .accessible_name(format!("{}: {enter}", item.label))
            .clicked()
        {
            out.push(SetupInput::Press(index));
        }
    }
}

/// The open field, as wide as there is room for, with Save and Cancel;
/// Enter saves and Esc cancels.
fn field(ui: &mut egui::Ui, edit: &EditView, label: egui::Id, out: &mut Vec<SetupInput>) {
    // A paste goes to the setup, which takes line breaks as spaces.
    if ui.memory(|memory| memory.has_focus(edit_id())) {
        let pasted = ui.input_mut(|input| {
            let mut pasted = Vec::new();
            input.events.retain(|event| match event {
                egui::Event::Paste(text) => {
                    pasted.push(text.clone());
                    false
                }
                _ => true,
            });
            pasted
        });
        out.extend(pasted.into_iter().map(SetupInput::Paste));
    }
    let mut typed = edit.text.clone();
    // Room for Save and Cancel beside it.
    let width = (ui.available_width() - 140.0).clamp(120.0, 480.0);
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut typed)
                .id(edit_id())
                .desired_width(width),
        )
        .labelled_by(label);
    // The field takes the keyboard while no other widget has it.
    if ui.memory(|memory| memory.focused().is_none()) {
        response.request_focus();
    }
    if response.changed() {
        out.push(SetupInput::SetText(typed));
    }
    if response.lost_focus() {
        if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            out.push(SetupInput::Commit);
        } else if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            out.push(SetupInput::CancelEdit);
        }
    }
    if ui.button("Save").clicked() {
        out.push(SetupInput::Commit);
    }
    if ui.button("Cancel").clicked() {
        out.push(SetupInput::CancelEdit);
    }
}

/// Start: a filled button when it can start, outlined while the cursor is
/// on it.
fn start(
    ui: &mut egui::Ui,
    view: &SetupView,
    index: usize,
    item: &SetupItem,
    out: &mut Vec<SetupInput>,
) {
    ui.add_space(10.0);
    let ready = item.value.first().map(|piece| piece.tone) == Some(Tone::Good);
    let text = egui::RichText::new(item.value_text()).size(17.0).strong();
    let mut button = egui::Button::new(if ready {
        text.color(egui::Color32::WHITE)
    } else {
        text
    })
    .min_size(egui::vec2(220.0, 38.0));
    if ready {
        button = button.fill(START_FILL);
    }
    if item.selected {
        button = button.stroke(egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
    }
    if ui.add_enabled(view.edit.is_none(), button).clicked() {
        out.push(SetupInput::Click(index));
    }
    ui.add_space(6.0);
}

/// ELECTRIC's fee pot: its balance and address, the QR code to fund it,
/// and its refund key's file.
fn pot_panel(ui: &mut egui::Ui, pot: &PotView, module: f32) {
    ui.add_space(6.0);
    ui.strong(pot.title);
    ui.add_space(4.0);
    let qr = |ui: &mut egui::Ui| {
        if let Some(address) = &pot.qr {
            ui.vertical(|ui| {
                super::dashboard::qr(ui, address, module);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(address.clone());
                }
            });
        }
    };
    if pot.full {
        ui.horizontal(|ui| {
            qr(ui);
            ui.vertical(|ui| lines(ui, &pot.before));
        });
    } else {
        lines(ui, &pot.before);
        qr(ui);
        lines(ui, &pot.after);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_keep_their_words_and_leave_the_keys_to_the_buttons() {
        for (hint, words) in [
            ("[Enter] or < > change", ""),
            ("< >  [Enter] list", ""),
            ("< >  0.5% steps", "0.5% steps"),
            ("< >  5% steps · [Enter] done", "5% steps"),
            ("< > halves or doubles  [Enter]", "halves or doubles"),
            ("[Enter]  most ASIC firmware", "most ASIC firmware"),
            ("< > [Enter]  a Pickaxe pool", "a Pickaxe pool"),
        ] {
            assert_eq!(hint_words(hint), words, "{hint}");
        }
    }

    #[test]
    fn terminal_bars_read_as_their_percent() {
        let bar_of = |text: &str| {
            bar(&[Piece {
                text: text.into(),
                tone: Tone::Plain,
            }])
        };
        assert_eq!(bar_of("██████████ 100%"), Some((1.0, "100%".into())));
        assert_eq!(bar_of("█████░░░░░  50%"), Some((0.5, "50%".into())));
        assert_eq!(bar_of("4.00% of PHOTON mining work"), None);
    }

    /// A Start that ended on another thread survives a click or key in the
    /// same frame.
    #[cfg(feature = "gui-preview")]
    #[test]
    fn a_start_from_another_thread_is_never_dropped() {
        let devices = vec![crate::backend::GpuDevice {
            index: 0,
            name: "Test GPU".into(),
            vendor: String::new(),
            vram_bytes: None,
            backend: BackendKind::Wgpu,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: None,
        }];
        let result = SetupResult {
            config: RuntimeConfig::default(),
            gpus: devices.clone(),
            profile_name: "Rig A".into(),
            server: None,
            options: Default::default(),
            electric: false,
            split: None,
        };
        let mut setup = Setup::finishing(
            SetupDriver::preview(devices, Default::default()),
            SetupOutcome::Complete(Box::new(result)),
        );
        let mut done = None;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui(|ui| {
                if let Some(ended) = setup.show(ui) {
                    done = Some(ended);
                }
            });
        harness.key_press(egui::Key::Enter);
        harness.run_steps(1);
        drop(harness);
        match done {
            Some(Done::Start(result)) => assert_eq!(result.profile_name, "Rig A"),
            _ => panic!("the Start was lost"),
        }
    }
}
