//! The window's mining setup: the terminal setup itself
//! (`tui::SetupDriver`), with its steps, rows and words, drawn with egui.
//! A click does what the terminal's key on that row does, and the
//! terminal's keys work here too while no field is being typed into.
use crate::backend::{BackendKind, DeviceSelection};
use crate::config::RuntimeConfig;
use crate::tui::{
    EditView, ItemKind, Piece, PotView, SetupDriver, SetupInput, SetupItem, SetupKey, SetupOutcome,
    SetupResult, SetupView, Tone,
};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// What the setup is made from, again after each session: the saved
/// settings and the command line, and where the settings are kept.
#[derive(Clone)]
pub struct Inputs {
    pub cfg: RuntimeConfig,
    pub config_path: PathBuf,
    pub backend: BackendKind,
    pub selection: DeviceSelection,
}

/// How the setup ended.
pub enum Done {
    /// Start was pressed: the profile is saved, and this is its session.
    Start(Box<SetupResult>),
    /// The setup was left on its first screen.
    Quit,
}

/// Green for what is ready, as the dashboard's chart draws it.
const GOOD: egui::Color32 = egui::Color32::from_rgb(105, 219, 124);

enum State {
    /// Finding the GPUs and reading the saved profiles, on another thread.
    Preparing(mpsc::Receiver<Result<SetupDriver, String>>),
    Failed(String),
    Ready(Box<SetupDriver>),
    /// A step that waits on the network (Start reading ELECTRIC's fee
    /// pot), on another thread; what the wait shows.
    Busy(
        mpsc::Receiver<(Box<SetupDriver>, SetupOutcome)>,
        &'static str,
    ),
}

pub struct Setup {
    inputs: Inputs,
    state: State,
    /// Why the last session stopped, for the status line once the setup is
    /// ready.
    notice: Option<String>,
}

impl Setup {
    pub fn new(inputs: Inputs) -> Self {
        Self {
            state: prepare(inputs.clone()),
            inputs,
            notice: None,
        }
    }

    /// A setup that is ready (screen previews).
    #[cfg(all(test, feature = "gui-preview"))]
    pub fn ready(driver: SetupDriver) -> Self {
        Self {
            inputs: Inputs {
                cfg: RuntimeConfig::default(),
                config_path: PathBuf::new(),
                backend: BackendKind::Auto,
                selection: DeviceSelection::Default,
            },
            state: State::Ready(Box::new(driver)),
            notice: None,
        }
    }

    /// Makes the setup again from what is saved, as the terminal does when
    /// a session returns to the profiles; `notice` says why it stopped.
    pub fn reload(&mut self, notice: Option<String>) {
        self.state = prepare(self.inputs.clone());
        self.notice = notice;
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
                    return ended(outcome);
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

    /// `pickaxe gui --start`: once the setup is ready, starts the profile
    /// last mined, or the first, as Enter twice on it would (`poll` gives
    /// its session). False while the GPUs are looked for.
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
        let mut done = self.poll();
        let inputs = match &mut self.state {
            State::Preparing(_) => {
                waiting(ui, "Looking for GPUs and reading your profiles…");
                ui.ctx().request_repaint_after(Duration::from_millis(200));
                return done;
            }
            State::Busy(_, what) => {
                waiting(ui, what);
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                return done;
            }
            State::Failed(error) => {
                let error = error.clone();
                if failed(ui, &error) {
                    self.reload(None);
                }
                return done;
            }
            State::Ready(driver) => {
                let wait = driver.tick();
                ui.ctx().request_repaint_after(wait);
                draw(ui, &driver.view())
            }
        };
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
            done = ended(driver.act(input));
            if done.is_some() {
                break;
            }
        }
        done
    }
}

fn ended(outcome: SetupOutcome) -> Option<Done> {
    match outcome {
        SetupOutcome::Continue => None,
        SetupOutcome::Complete(result) => Some(Done::Start(result)),
        SetupOutcome::Cancel => Some(Done::Quit),
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
/// thread, as `pickaxe mine` does before its setup.
fn prepare(inputs: Inputs) -> State {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(setup_driver(inputs));
    });
    State::Preparing(receive)
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
        crate::tui::SetupOverrides::default(),
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

/// The open field's and the token search's ids: the keys go to them while
/// they have focus.
fn edit_id() -> egui::Id {
    egui::Id::unique("pickaxe.setup.edit")
}

fn search_id() -> egui::Id {
    egui::Id::unique("pickaxe.setup.search")
}

/// Draws the screen; what was clicked or typed, in order.
fn draw(ui: &mut egui::Ui, view: &SetupView) -> Vec<SetupInput> {
    let mut out = Vec::new();
    keyboard(ui, &mut out);
    egui::Panel::top("setup-bar").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Pickaxe").size(20.0).strong());
            ui.separator();
            ui.label(&view.heading);
        });
        ui.add_space(4.0);
    });
    egui::Panel::bottom("setup-actions").show(ui, |ui| actions(ui, view, &mut out));
    let full = view.pot.as_ref().filter(|pot| pot.full);
    if let Some(pot) = view.pot.as_ref().filter(|pot| !pot.full) {
        egui::Panel::right("setup-pot")
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| pot_panel(ui, pot, 4.0));
            });
    }
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| match full {
            Some(pot) => pot_panel(ui, pot, 8.0),
            None => page(ui, view, &mut out),
        });
    });
    out
}

/// The terminal's keys, while no field has the keyboard. Buttons keep no
/// keyboard focus here: the setup's own keys move through it.
fn keyboard(ui: &mut egui::Ui, out: &mut Vec<SetupInput>) {
    let focused = ui.memory(|memory| memory.focused());
    let searching = focused == Some(search_id());
    match focused {
        Some(id) if id == edit_id() => return,
        Some(id) if id != search_id() => ui.memory_mut(|memory| memory.surrender_focus(id)),
        _ => {}
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
                    // The search box keeps its own Left, Right, Enter and
                    // typing.
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
        ui.label(egui::RichText::new(&view.status).strong());
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
    ui.label(egui::RichText::new(view.keys).small().weak());
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

/// Pieces of setup text as one label's text. A line's leading spaces (the
/// terminal's indents) are left out, and the terminal's few drawing
/// glyphs, which the window's font lacks, are said in text: the Advanced
/// header's ▾ ▸ (its checkbox shows it open) and arrows.
fn text(ui: &egui::Ui, pieces: &[Piece]) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    for (index, piece) in pieces.iter().enumerate() {
        let shown = if index == 0 {
            piece.text.trim_start()
        } else {
            piece.text.as_str()
        };
        let shown = shown
            .replace("▾ ", "")
            .replace("▸ ", "")
            .replace('←', "<-")
            .replace('→', "->");
        job.append(
            &shown,
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

/// An item's value: a bar drawn as one, or its text.
fn value(ui: &mut egui::Ui, pieces: &[Piece]) {
    match bar(pieces) {
        Some((fraction, percent)) => {
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(percent)
                    .desired_width(220.0),
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

/// The screen's title and rows: rows to choose or change in a grid, lines
/// of text and Start between them.
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
            let mut run = Vec::new();
            let mut grids = 0;
            for (index, item) in view.items.iter().enumerate() {
                match item.kind {
                    ItemKind::Info | ItemKind::Start => {
                        rows(ui, view, &run, grids, out);
                        run.clear();
                        grids += 1;
                        if item.kind == ItemKind::Start {
                            start(ui, view, index, item, out);
                        } else {
                            lines(ui, std::slice::from_ref(&item.value));
                        }
                    }
                    _ => run.push(index),
                }
            }
            rows(ui, view, &run, grids, out);
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
                    .desired_width(460.0),
            )
            .labelled_by(label.id);
        if response.changed() {
            out.push(SetupInput::Search(typed));
        }
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            out.push(SetupInput::Key(SetupKey::Enter));
        }
    });
}

/// Rows `run` of the screen: name, value or its controls, and the
/// terminal's key hint.
fn rows(
    ui: &mut egui::Ui,
    view: &SetupView,
    run: &[usize],
    grid: usize,
    out: &mut Vec<SetupInput>,
) {
    if run.is_empty() {
        return;
    }
    egui::Grid::new(("pickaxe.setup.rows", grid))
        .num_columns(3)
        .spacing([14.0, 6.0])
        .max_col_width(560.0)
        .show(ui, |ui| {
            for &index in run {
                row(ui, view, index, out);
                ui.end_row();
            }
        });
}

fn row(ui: &mut egui::Ui, view: &SetupView, index: usize, out: &mut Vec<SetupInput>) {
    let item = &view.items[index];
    // While a field is open, the other rows wait, as in the terminal.
    let free = view.edit.is_none();
    let label = match item.kind {
        // Under the cursor, a checkbox is highlighted as a selected row is.
        ItemKind::Toggle { on } => {
            let mut on = on;
            let fill = if item.selected {
                ui.visuals().selection.bg_fill
            } else {
                egui::Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(4)
                .inner_margin(egui::Margin::symmetric(4, 1))
                .show(ui, |ui| {
                    ui.add_enabled(free, egui::Checkbox::new(&mut on, item.label.as_str()))
                })
                .inner
        }
        _ => ui.add_enabled(
            free,
            egui::Button::selectable(item.selected, item.label.as_str()),
        ),
    };
    if label.clicked() {
        out.push(SetupInput::Click(index));
    }
    let edit = view.edit.as_ref().filter(|edit| edit.item == Some(index));
    match (edit, item.kind) {
        (Some(edit), _) => {
            ui.horizontal(|ui| field(ui, edit, label.id, out));
        }
        (None, ItemKind::Cycle { enter }) => {
            ui.horizontal(|ui| cycle(ui, free, index, item, enter, out));
        }
        _ => value(ui, &item.value),
    }
    ui.label(egui::RichText::new(&item.hint).small().weak());
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

/// The open field, with Save and Cancel; Enter saves and Esc cancels.
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
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut typed)
                .id(edit_id())
                .desired_width(420.0),
        )
        .labelled_by(label);
    // The field holds the keyboard until it is saved or cancelled.
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

/// Start, as the terminal colors it.
fn start(
    ui: &mut egui::Ui,
    view: &SetupView,
    index: usize,
    item: &SetupItem,
    out: &mut Vec<SetupInput>,
) {
    ui.add_space(10.0);
    let tone = item.value.first().map_or(Tone::Plain, |piece| piece.tone);
    let button = egui::Button::selectable(
        item.selected,
        egui::RichText::new(item.value_text())
            .size(17.0)
            .color(color(ui, tone)),
    )
    .min_size(egui::vec2(220.0, 38.0));
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
