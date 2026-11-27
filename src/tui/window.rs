//! The terminal setup as plain data, for the desktop window: the same
//! steps, rows, words and keys, steered by clicks.
//!
//! `SetupDriver` holds the terminal's own setup (`SetupFlow`). `view` gives
//! the screen shown as text pieces, taken from the same functions the
//! terminal draws with; `act` turns a click or a key into what the
//! terminal's keys do there, by moving its cursors and calling its key
//! handlers; `tick` makes the polls the terminal makes before each draw.
use super::*;

/// How a piece of setup text is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Secondary: explanations, hints and values not set.
    Dim,
    /// Needs attention (yellow in the terminal).
    Warn,
    /// Ready (green in the terminal).
    Good,
}

/// A piece of setup text and how it is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub tone: Tone,
}

impl Piece {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Plain)
    }

    pub(super) fn dim(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Dim)
    }

    pub(super) fn warn(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Warn)
    }

    pub(super) fn good(text: impl Into<String>) -> Self {
        Self::new(text, Tone::Good)
    }
}

/// What an item is, and what a click on it does (`SetupInput::Click`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// A line of text; nothing to choose.
    Info,
    /// One of a list: a click chooses it, as Enter on it does.
    Choice,
    /// A click does what Enter on it does: opens a list, adds what was
    /// found.
    Action,
    /// Start: a click checks the settings and starts, as Enter does.
    Start,
    /// A value that < > step (`SetupInput::Step`, Left/Right); a click only
    /// moves the cursor to it. `enter` names what Enter also does there.
    Cycle { enter: Option<&'static str> },
    /// On or off: a click turns it over.
    Toggle { on: bool },
    /// Typed in: a click opens its field (`SetupView::edit`).
    Text,
}

/// Where an item puts the terminal's cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    None,
    Profile(usize),
    Mode(MiningMode),
    Network(MiningNetwork),
    Pool(usize),
    Asic(usize),
    Token(usize),
    /// A settings row, by its place on the page.
    Row(usize),
    Gpu(usize),
    Connection(usize),
    Peer(usize),
}

/// One row of a setup screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupItem {
    /// Its name; empty for a line of text.
    pub label: String,
    /// What it holds, or the line's text.
    pub value: Vec<Piece>,
    /// The terminal's key hint for it, if any.
    pub hint: String,
    /// The terminal's cursor is on it.
    pub selected: bool,
    pub kind: ItemKind,
    /// What the row itself offers besides a click, with button text: a
    /// profile's Open, Rename and Delete, a saved node's Remove
    /// (`SetupInput::Row`). The terminal takes these keys on the row under
    /// its cursor.
    pub actions: Vec<(SetupCommand, &'static str)>,
    pub(super) target: Target,
}

impl SetupItem {
    pub(super) fn new(
        target: Target,
        kind: ItemKind,
        label: impl Into<String>,
        selected: bool,
    ) -> Self {
        Self {
            label: label.into(),
            value: Vec::new(),
            hint: String::new(),
            selected,
            kind,
            actions: Vec::new(),
            target,
        }
    }

    /// A line of text (empty: a blank line).
    pub(super) fn info(value: Vec<Piece>) -> Self {
        Self {
            value,
            ..Self::new(Target::None, ItemKind::Info, "", false)
        }
    }

    pub(super) fn with_value(mut self, value: Vec<Piece>) -> Self {
        self.value = value;
        self
    }

    pub(super) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// The value as plain text.
    pub fn value_text(&self) -> String {
        self.value.iter().map(|piece| piece.text.as_str()).collect()
    }
}

/// The field being typed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditView {
    /// What is typed: the row's name, or what the field asks for.
    pub label: String,
    /// The text typed so far.
    pub text: String,
    /// The item it changes, where it is typed in place; `None` for a
    /// field of its own (another computer, a cookie file).
    pub item: Option<usize>,
}

/// ELECTRIC's fee pot, shown beside the settings or full-size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PotView {
    pub title: &'static str,
    /// Shown full-size, to scan (F).
    pub full: bool,
    /// The lines above its QR code: its balance and address.
    pub before: Vec<Vec<Piece>>,
    /// The text its QR code holds: the pot's address, once there is a pot
    /// to fund.
    pub qr: Option<String>,
    /// The lines under it: the refund key's file.
    pub after: Vec<Vec<Piece>>,
}

/// A command of the screen shown, or of one of its rows (a key in the
/// terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupCommand {
    /// Opens the profile (Enter).
    Open,
    /// Renames the profile under the cursor (R).
    Rename,
    /// Deletes the profile under the cursor once confirmed (D), or removes
    /// the server or node under the cursor (Del).
    Delete,
    /// Yes to the status line's question (Y): delete the profile, or
    /// search a public address.
    Yes,
    /// Keeps the profile (N).
    No,
    /// Searches another computer by its name or address (F).
    OtherComputer,
    /// Lists the Tailscale network's computers (T).
    Tailscale,
    /// The cookie file of the node under the cursor (C).
    CookieFile,
    /// Mines on the GPU under the cursor, or not (Space).
    ToggleGpu,
    /// Mines on every GPU (A).
    AllGpus,
    /// Shows the fee pot full-size (F).
    PotView,
}

impl SetupCommand {
    /// The terminal's key for the command on `step`.
    fn key(self, step: SetupStep) -> KeyCode {
        match self {
            Self::Open => KeyCode::Enter,
            Self::Rename => KeyCode::Char('r'),
            Self::Delete if step == SetupStep::Connections => KeyCode::Delete,
            Self::Delete => KeyCode::Char('d'),
            Self::Yes => KeyCode::Char('y'),
            Self::No => KeyCode::Char('n'),
            Self::OtherComputer | Self::PotView => KeyCode::Char('f'),
            Self::Tailscale => KeyCode::Char('t'),
            Self::CookieFile => KeyCode::Char('c'),
            Self::ToggleGpu => KeyCode::Char(' '),
            Self::AllGpus => KeyCode::Char('a'),
        }
    }
}

/// A key of the window's keyboard, which the terminal's setup takes as it
/// takes its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Delete,
    Backspace,
    Char(char),
}

impl SetupKey {
    fn code(self) -> KeyCode {
        match self {
            Self::Up => KeyCode::Up,
            Self::Down => KeyCode::Down,
            Self::Left => KeyCode::Left,
            Self::Right => KeyCode::Right,
            Self::Enter => KeyCode::Enter,
            Self::Esc => KeyCode::Esc,
            Self::Delete => KeyCode::Delete,
            Self::Backspace => KeyCode::Backspace,
            Self::Char(ch) => KeyCode::Char(ch),
        }
    }
}

/// What the window asks of the setup. Items are named by their place in
/// `SetupView::items`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupInput {
    /// Moves the cursor to the item, as Up/Down would.
    Select(usize),
    /// A click on the item: what its kind says (`ItemKind`).
    Click(usize),
    /// Enter on the item: a Cycle item's own action (`enter`).
    Press(usize),
    /// < (`forward: false`) or > on the item: Left or Right on it.
    Step { item: usize, forward: bool },
    /// A key, as the terminal takes it.
    Key(SetupKey),
    /// Esc: back a step, closes what is open, or quits on the first screen.
    Back,
    /// One of the screen's commands (`SetupView::commands`).
    Command(SetupCommand),
    /// One of a row's own commands (`SetupItem::actions`): the cursor moves
    /// to the row, then the command's key.
    Row { item: usize, command: SetupCommand },
    /// The GPU token list's search, as typed.
    Search(String),
    /// The open field's text, as typed.
    SetText(String),
    /// Text pasted into the open field; line breaks become spaces.
    Paste(String),
    /// Saves the open field (Enter).
    Commit,
    /// Closes the open field without saving (Esc).
    CancelEdit,
}

/// What an input led to.
#[derive(Debug, Clone)]
pub enum SetupOutcome {
    Continue,
    /// Start: the profile is saved, and this is the session to start.
    Complete(Box<SetupResult>),
    /// The setup was left on its first screen (Esc there, or Q).
    Cancel,
}

/// The screen shown, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupView {
    /// What the terminal's top line shows after "PICKAXE MINER".
    pub heading: String,
    /// The screen's title.
    pub title: String,
    /// The screen's rows, in the terminal's order.
    pub items: Vec<SetupItem>,
    /// The GPU token list's search, and what its box says while empty.
    pub search: Option<String>,
    pub search_hint: &'static str,
    /// The field being typed into, if any.
    pub edit: Option<EditView>,
    /// The terminal's status line: a check's result, a question, an error.
    pub status: String,
    /// The screen's commands that no row carries, with their button text.
    pub commands: Vec<(SetupCommand, &'static str)>,
    /// What Esc does here: "Back", "Cancel" a field, or "Quit".
    pub back: &'static str,
    /// The terminal's key line for the screen.
    pub keys: &'static str,
    /// ELECTRIC's fee pot, while it shows.
    pub pot: Option<PotView>,
}

/// The terminal's setup, steered by the desktop window.
pub struct SetupDriver {
    flow: SetupFlow,
}

impl SetupDriver {
    /// The setup `run_setup` runs in the terminal, from the same inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: RuntimeConfig,
        devices: Vec<GpuDevice>,
        prefer: BackendKind,
        default_gpus: &[GpuDevice],
        profile_path: &Path,
        profiles: MiningProfiles,
        sources_path: &Path,
        sources: SharedSources,
        cookies_path: &Path,
        overrides: SetupOverrides,
    ) -> Result<Self, String> {
        SetupFlow::for_setup(
            config,
            devices,
            prefer,
            default_gpus,
            profile_path,
            profiles,
            sources_path,
            sources,
            cookies_path,
            overrides,
        )
        .map(|flow| Self { flow })
    }

    /// A setup of `devices` with `profiles` that saves nothing and looks
    /// for nothing (the tests' checks), for screen previews.
    #[cfg(test)]
    pub fn preview(devices: Vec<GpuDevice>, profiles: MiningProfiles) -> Self {
        let mut flow = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .expect("preview GPUs");
        if !profiles.profiles.is_empty() {
            flow.step = SetupStep::Profiles;
        }
        flow.profiles = profiles;
        Self { flow }
    }

    /// Shows `status` on the status line, as a check's result would.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.flow.status_line = status.into();
    }

    /// Takes in the background checks' results and ELECTRIC's fee pot, as
    /// the terminal does before each draw, and says how soon to look again:
    /// sooner while a check runs or the fee pot's balance is read.
    pub fn tick(&mut self) -> Duration {
        self.flow.poll();
        self.flow.wait()
    }

    /// The screen shown.
    pub fn view(&self) -> SetupView {
        let flow = &self.flow;
        let (title, items) = self.rows();
        let edit = flow.editing.map(|field| EditView {
            label: self.edit_label(field),
            text: flow.text_input.clone(),
            item: match field {
                TextField::Computer | TextField::CookieFile => None,
                _ => items
                    .iter()
                    .position(|item| item.selected && item.kind != ItemKind::Info),
            },
        });
        SetupView {
            heading: setup_heading(flow),
            title,
            items,
            search: (flow.step == SetupStep::Token && flow.mode == MiningMode::Gpu)
                .then(|| flow.token_input.clone()),
            search_hint: TOKEN_SEARCH_HINT,
            edit,
            status: flow.status_line.clone(),
            commands: self.commands(),
            back: self.back(),
            keys: setup_keys(flow),
            pot: self.pot(),
        }
    }

    /// The screen's title and rows.
    fn page(&self) -> (String, Vec<SetupItem>) {
        let flow = &self.flow;
        match flow.step {
            SetupStep::Profiles => (PROFILES_TITLE.into(), profile_items(flow)),
            SetupStep::Hardware => (HARDWARE_TITLE.into(), hardware_items(flow)),
            SetupStep::Network => (NETWORK_TITLE.into(), network_items(flow)),
            SetupStep::Token => (token_title(flow).into(), token_items(flow)),
            SetupStep::Settings => ("Settings".into(), settings_items(flow)),
            SetupStep::Connections => {
                let mut items = connection_items(flow);
                items.push(SetupItem::info(Vec::new()));
                items.extend(connection_notes(flow).into_iter().map(SetupItem::info));
                (connections_title(flow), items)
            }
            SetupStep::Gpus => (GPUS_TITLE.into(), gpu_items(flow)),
            SetupStep::TailscalePeers => (tailscale_title(flow), tailscale_items(flow)),
        }
    }

    /// The rows with the screen's row commands on them: the saved profiles'
    /// Open, Rename and Delete, and the saved servers' and nodes' Remove
    /// (and a node's cookie file). The terminal takes them as keys on the
    /// row under its cursor.
    fn rows(&self) -> (String, Vec<SetupItem>) {
        let flow = &self.flow;
        let (title, mut items) = self.page();
        if flow.editing.is_some() || flow.profile_delete_pending {
            return (title, items);
        }
        let saved = flow
            .sources
            .list(flow.config.network, flow.connection_kind)
            .len();
        for item in &mut items {
            item.actions = match (flow.step, item.target) {
                (SetupStep::Profiles, Target::Profile(index))
                    if index < flow.profiles.profiles.len() =>
                {
                    vec![
                        (SetupCommand::Open, "Open"),
                        (SetupCommand::Rename, "Rename"),
                        (SetupCommand::Delete, "Delete"),
                    ]
                }
                (SetupStep::Connections, Target::Connection(index)) if index < saved => {
                    let mut actions = vec![(SetupCommand::Delete, "Remove")];
                    if flow.connection_kind == ConnectionKind::Node {
                        actions.push((SetupCommand::CookieFile, "Cookie file"));
                    }
                    actions
                }
                _ => Vec::new(),
            };
        }
        (title, items)
    }

    /// What the open field is called: its row's name, or what it asks for.
    fn edit_label(&self, field: TextField) -> String {
        let flow = &self.flow;
        match field {
            TextField::ProfileRename => "Profile name".into(),
            TextField::Connection => match flow.connection_kind {
                ConnectionKind::Fulcrum => "Fulcrum server".into(),
                ConnectionKind::Node => "BCH node".into(),
            },
            TextField::Computer | TextField::CookieFile => edit_prompt(flow).unwrap_or_default(),
            _ => settings_line(flow, flow.current_row()).0.into(),
        }
    }

    fn commands(&self) -> Vec<(SetupCommand, &'static str)> {
        let flow = &self.flow;
        if flow.editing.is_some() {
            return Vec::new();
        }
        match flow.step {
            // The profile's own Delete asked; its rows carry the rest.
            SetupStep::Profiles if flow.profile_delete_pending => {
                vec![(SetupCommand::Yes, "Delete"), (SetupCommand::No, "Keep")]
            }
            SetupStep::Connections => {
                let mut commands = Vec::new();
                if flow.public_question.is_some() {
                    commands.push((SetupCommand::Yes, "Yes"));
                }
                commands.extend([
                    (SetupCommand::OtherComputer, "Other PC"),
                    (SetupCommand::Tailscale, "Tailscale"),
                ]);
                commands
            }
            // Each GPU's checkbox ticks it (Space).
            SetupStep::Gpus => vec![(SetupCommand::AllGpus, "All")],
            SetupStep::Settings if flow.electric_in_use() && !flow.pot_view => {
                vec![(SetupCommand::PotView, "Pot QR")]
            }
            _ => Vec::new(),
        }
    }

    /// What Esc does on the screen shown.
    fn back(&self) -> &'static str {
        let flow = &self.flow;
        if flow.editing.is_some() {
            return "Cancel";
        }
        match flow.step {
            SetupStep::Profiles if !flow.profile_delete_pending => "Quit",
            SetupStep::Hardware if flow.profiles.profiles.is_empty() => "Quit",
            // The terminal's word for leaving these lists: "[Esc] done".
            SetupStep::Connections | SetupStep::Gpus => "Done",
            _ => "Back",
        }
    }

    fn pot(&self) -> Option<PotView> {
        let flow = &self.flow;
        if flow.step != SetupStep::Settings || !flow.electric_in_use() {
            return None;
        }
        if flow.pot_view {
            let (before, qr) = match pot_full_text(flow) {
                Ok((pot, text, _)) => (text, Some(pot.address.clone())),
                Err(message) => (vec![vec![Piece::plain(message)]], None),
            };
            return Some(PotView {
                title: POT_FULL_TITLE,
                full: true,
                before,
                qr,
                after: Vec::new(),
            });
        }
        let (before, pot, after) = pot_text(flow);
        Some(PotView {
            title: POT_TITLE,
            full: false,
            before,
            qr: pot.map(|pot| pot.address.clone()),
            after,
        })
    }

    /// Does what `input` asks, as the terminal's keys do it.
    pub fn act(&mut self, input: SetupInput) -> SetupOutcome {
        let action = match input {
            SetupInput::Select(item) => {
                self.select(item);
                SetupAction::Continue
            }
            SetupInput::Click(item) => self.click(item),
            SetupInput::Press(item) => self.press(item),
            SetupInput::Step { item, forward } => self.step(item, forward),
            SetupInput::Key(key) => self.key(key.code()),
            SetupInput::Back => self.key(KeyCode::Esc),
            SetupInput::Command(command) => self.command(command),
            SetupInput::Row { item, command } => self.row_command(item, command),
            SetupInput::Search(text) => self.search(&text),
            SetupInput::SetText(text) => self.set_text(&text),
            SetupInput::Paste(text) => self.paste(&text),
            SetupInput::Commit if self.flow.editing.is_some() => self.key(KeyCode::Enter),
            SetupInput::CancelEdit if self.flow.editing.is_some() => self.key(KeyCode::Esc),
            SetupInput::Commit | SetupInput::CancelEdit => SetupAction::Continue,
        };
        self.outcome(action)
    }

    /// Whether `input` presses Start while ELECTRIC mines: Start then reads
    /// the fee pot's balance from the network, which can take seconds, so
    /// the window does it away from its drawing.
    pub fn reads_pot(&self, input: &SetupInput) -> bool {
        self.flow.electric_in_use() && self.presses_start(input)
    }

    /// Whether `input` presses Start while this
    /// computer's node and usual ports are still looked at: Start waits a
    /// moment for them, so the window does it away from its drawing.
    pub fn looks_here(&self, input: &SetupInput) -> bool {
        self.flow.looking_here() && self.presses_start(input)
    }

    /// Whether `input` presses Start on the settings page.
    fn presses_start(&self, input: &SetupInput) -> bool {
        let flow = &self.flow;
        if flow.step != SetupStep::Settings || flow.editing.is_some() || flow.pot_view {
            return false;
        }
        match input {
            SetupInput::Click(item) | SetupInput::Press(item) => self
                .page()
                .1
                .get(*item)
                .is_some_and(|item| item.kind == ItemKind::Start),
            SetupInput::Key(SetupKey::Enter) => flow.current_row() == SettingsRow::Start,
            _ => false,
        }
    }

    /// `pickaxe gui --start`: opens the profile Start last started (kept
    /// beside the profiles), or the first, and presses Start, as Enter
    /// twice on it would. Without a profile, the setup stays for the miner.
    pub fn start_now(&mut self) -> SetupOutcome {
        let flow = &mut self.flow;
        if flow.step != SetupStep::Profiles || flow.profiles.profiles.is_empty() {
            return SetupOutcome::Continue;
        }
        let last = flow
            .profile_path
            .as_deref()
            .and_then(last_profile)
            .and_then(|name| {
                flow.profiles
                    .profiles
                    .iter()
                    .position(|profile| profile.name.eq_ignore_ascii_case(&name))
            });
        if let Some(index) = last {
            flow.profile_selected = index;
        }
        // Opens it with Start selected.
        self.key(KeyCode::Enter);
        if self.flow.step != SetupStep::Settings || self.flow.current_row() != SettingsRow::Start {
            return SetupOutcome::Continue;
        }
        // The terminal draws once between the two presses: ELECTRIC's fee
        // pot is made then.
        self.flow.poll();
        let action = self.key(KeyCode::Enter);
        self.outcome(action)
    }

    fn key(&mut self, code: KeyCode) -> SetupAction {
        self.flow
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn outcome(&mut self, action: SetupAction) -> SetupOutcome {
        match action {
            SetupAction::Continue => SetupOutcome::Continue,
            SetupAction::Cancel => SetupOutcome::Cancel,
            SetupAction::Complete => match self.flow.finish() {
                Ok(result) => SetupOutcome::Complete(Box::new(result)),
                Err(error) => {
                    self.flow.status_line = error;
                    SetupOutcome::Continue
                }
            },
        }
    }

    /// Moves the cursor to `index`; false when it cannot move there: no
    /// such item, a line of text, or while a field, the delete question or
    /// the full-size fee pot holds the keys.
    fn select(&mut self, index: usize) -> bool {
        let Some(target) = self.page().1.get(index).map(|item| item.target) else {
            return false;
        };
        let flow = &mut self.flow;
        if flow.editing.is_some() || flow.profile_delete_pending || flow.pot_view {
            return false;
        }
        let moved = match target {
            Target::None => return false,
            Target::Profile(index) => std::mem::replace(&mut flow.profile_selected, index) != index,
            // Hardware, the network and the targets have no cursor of their
            // own: Down steps to the choice.
            Target::Mode(mode) => {
                for _ in 0..3 {
                    if flow.mode == mode {
                        break;
                    }
                    flow.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                }
                false
            }
            Target::Network(network) => {
                if flow.config.network != network {
                    flow.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                }
                false
            }
            Target::Pool(index) => {
                if flow.pool_target != index {
                    flow.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                }
                false
            }
            Target::Asic(index) => {
                if flow.asic_target != index {
                    flow.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                }
                false
            }
            Target::Token(index) => std::mem::replace(&mut flow.token_selected, index) != index,
            // Leaving a row closes its share, as Up/Down do.
            Target::Row(index) => {
                let moved = std::mem::replace(&mut flow.settings_row, index) != index;
                if moved {
                    flow.share_editing = false;
                }
                moved
            }
            Target::Gpu(index) => std::mem::replace(&mut flow.selected, index) != index,
            // A public address's question waits for the very next key; a
            // move answers no, as Up/Down do.
            Target::Connection(index) => {
                let moved = std::mem::replace(&mut flow.connection_selected, index) != index;
                if moved {
                    flow.public_question = None;
                }
                moved
            }
            Target::Peer(index) => std::mem::replace(&mut flow.tailscale_selected, index) != index,
        };
        if moved {
            flow.status_line.clear();
        }
        true
    }

    fn click(&mut self, index: usize) -> SetupAction {
        let Some(kind) = self.page().1.get(index).map(|item| item.kind) else {
            return SetupAction::Continue;
        };
        match kind {
            ItemKind::Info => SetupAction::Continue,
            ItemKind::Cycle { .. } => {
                self.select(index);
                SetupAction::Continue
            }
            // The GPU list ticks with Space; Enter leaves it.
            ItemKind::Toggle { .. } if self.flow.step == SetupStep::Gpus => {
                if self.select(index) {
                    self.key(KeyCode::Char(' '))
                } else {
                    SetupAction::Continue
                }
            }
            _ => self.press(index),
        }
    }

    fn press(&mut self, index: usize) -> SetupAction {
        if self.select(index) {
            self.key(KeyCode::Enter)
        } else {
            SetupAction::Continue
        }
    }

    fn step(&mut self, index: usize, forward: bool) -> SetupAction {
        if !self.select(index) {
            return SetupAction::Continue;
        }
        // A share changes only once Enter opened it.
        if self.flow.step == SetupStep::Settings
            && matches!(self.flow.current_row(), SettingsRow::Share(_))
            && !self.flow.share_editing
        {
            self.key(KeyCode::Enter);
        }
        self.key(if forward {
            KeyCode::Right
        } else {
            KeyCode::Left
        })
    }

    fn command(&mut self, command: SetupCommand) -> SetupAction {
        if !self.commands().iter().any(|(shown, _)| *shown == command) {
            return SetupAction::Continue;
        }
        self.key(command.key(self.flow.step))
    }

    /// A row's own command: the cursor moves to the row, then its key.
    fn row_command(&mut self, index: usize, command: SetupCommand) -> SetupAction {
        let offered = self
            .rows()
            .1
            .get(index)
            .is_some_and(|item| item.actions.iter().any(|(shown, _)| *shown == command));
        if !offered || !self.select(index) {
            return SetupAction::Continue;
        }
        self.key(command.key(self.flow.step))
    }

    /// Types the search as the terminal takes it: Backspace to what the
    /// two have in common, then the rest.
    fn search(&mut self, text: &str) -> SetupAction {
        let flow = &self.flow;
        if flow.step != SetupStep::Token || flow.mode != MiningMode::Gpu || flow.editing.is_some() {
            return SetupAction::Continue;
        }
        while !text.starts_with(self.flow.token_input.as_str()) {
            self.key(KeyCode::Backspace);
        }
        let typed = self.flow.token_input.len();
        for ch in text[typed..].chars() {
            self.key(KeyCode::Char(ch));
        }
        SetupAction::Continue
    }

    /// Types the open field's text as the terminal takes it.
    fn set_text(&mut self, text: &str) -> SetupAction {
        if self.flow.editing.is_none() {
            return SetupAction::Continue;
        }
        while !text.starts_with(self.flow.text_input.as_str()) {
            self.key(KeyCode::Backspace);
        }
        let typed = self.flow.text_input.len();
        for ch in text[typed..].chars() {
            self.key(KeyCode::Char(ch));
        }
        SetupAction::Continue
    }

    fn paste(&mut self, text: &str) -> SetupAction {
        if self.flow.editing.is_none() {
            return SetupAction::Continue;
        }
        for ch in text.chars() {
            match ch {
                '\r' | '\n' | '\t' => self.key(KeyCode::Char(' ')),
                ch if ch.is_control() => SetupAction::Continue,
                ch => self.key(KeyCode::Char(ch)),
            };
        }
        SetupAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAINNET: &str = crate::config::DONATION_ADDRESS;

    fn devices() -> Vec<GpuDevice> {
        ["Primary CUDA", "Secondary HIP"]
            .into_iter()
            .zip([BackendKind::Cuda, BackendKind::Hip])
            .map(|(name, backend)| GpuDevice {
                index: 0,
                name: name.into(),
                vendor: "test".into(),
                vram_bytes: None,
                backend,
                detail: String::new(),
                integrated: false,
                ready: true,
                pci: None,
            })
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A folder of its own for one test's saved profiles.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pickaxe-window-{name}-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A setup as the tests build it, saving its profiles in `dir`.
    fn flow(dir: &Path) -> SetupFlow {
        let devices = devices();
        let mut flow = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        flow.profile_path = Some(dir.join("config.profiles.json"));
        flow
    }

    fn driver(dir: &Path) -> SetupDriver {
        SetupDriver { flow: flow(dir) }
    }

    /// The place of the item called `label` on the screen shown.
    fn item(driver: &SetupDriver, label: &str) -> usize {
        let view = driver.view();
        view.items
            .iter()
            .position(|item| item.label == label)
            .unwrap_or_else(|| panic!("no {label:?} in {:#?}", view.items))
    }

    fn type_keys(flow: &mut SetupFlow, text: &str) {
        for ch in text.chars() {
            flow.handle_key(key(KeyCode::Char(ch)));
        }
    }

    fn complete(outcome: SetupOutcome) -> SetupResult {
        match outcome {
            SetupOutcome::Complete(result) => *result,
            other => panic!("not started: {other:?}"),
        }
    }

    /// What a setup result starts, without the parts that differ by
    /// folder.
    fn started(result: &SetupResult) -> String {
        format!(
            "{:?} {:?} {} {:?} {:?} {} {:?}",
            result.config,
            result
                .gpus
                .iter()
                .map(|gpu| (gpu.backend, gpu.index))
                .collect::<Vec<_>>(),
            result.profile_name,
            result.server,
            result.options,
            result.electric,
            result.split
        )
    }

    #[test]
    fn clicks_start_gpu_mining_as_the_keys_do() {
        // The terminal's keys: Hardware, Network, Token, then the address,
        // the profile's name and Start.
        let keys_dir = folder("keys");
        let mut keys = flow(&keys_dir);
        keys.handle_key(key(KeyCode::Enter));
        keys.handle_key(key(KeyCode::Enter));
        let photon = keys
            .matching_tokens()
            .iter()
            .position(|token| *token == crate::config::MiningToken::Photon)
            .unwrap();
        keys.token_selected = photon;
        keys.handle_key(key(KeyCode::Enter));
        keys.open_settings(SettingsRow::Address);
        keys.handle_key(key(KeyCode::Enter));
        type_keys(&mut keys, MAINNET);
        keys.handle_key(key(KeyCode::Enter));
        keys.open_settings(SettingsRow::ProfileName);
        keys.handle_key(key(KeyCode::Enter));
        type_keys(&mut keys, "Window");
        keys.handle_key(key(KeyCode::Enter));
        keys.open_settings(SettingsRow::Start);
        assert_eq!(keys.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
        let by_keys = keys.finish().unwrap();

        // The same with clicks.
        let clicks_dir = folder("clicks");
        let mut clicks = driver(&clicks_dir);
        assert_eq!(clicks.view().heading, "Setup   1/4 Hardware");
        clicks.act(SetupInput::Click(item(&clicks, "GPU mining")));
        assert_eq!(clicks.view().heading, "Setup   2/4 Network");
        clicks.act(SetupInput::Click(item(&clicks, "Mainnet")));
        let view = clicks.view();
        assert_eq!(view.title, "GPU tokens");
        assert_eq!(view.search.as_deref(), Some(""));
        clicks.act(SetupInput::Click(item(&clicks, "PHOTON")));
        assert_eq!(clicks.flow.step, SetupStep::Settings);
        clicks.act(SetupInput::Click(item(&clicks, "Address")));
        let edit = clicks.view().edit.expect("the address field opens");
        assert_eq!(edit.label, "Address");
        assert_eq!(edit.item, Some(item(&clicks, "Address")));
        clicks.act(SetupInput::SetText(MAINNET.into()));
        clicks.act(SetupInput::Commit);
        assert!(clicks.view().edit.is_none());
        clicks.act(SetupInput::Click(item(&clicks, "Profile name")));
        clicks.act(SetupInput::SetText("Window".into()));
        clicks.act(SetupInput::Commit);
        let start = clicks
            .view()
            .items
            .iter()
            .position(|item| item.kind == ItemKind::Start)
            .unwrap();
        assert_eq!(clicks.view().items[start].value_text(), "Start mining");
        let by_clicks = complete(clicks.act(SetupInput::Click(start)));

        assert_eq!(started(&by_clicks), started(&by_keys));
        assert_eq!(by_clicks.profile_name, "Window");
        let saved =
            MiningProfiles::load_optional(&clicks_dir.join("config.profiles.json")).unwrap();
        assert_eq!(saved.profiles.len(), 1);
        // Start again saves the same profile, not another.
        complete(clicks.act(SetupInput::Click(start)));
        let saved =
            MiningProfiles::load_optional(&clicks_dir.join("config.profiles.json")).unwrap();
        assert_eq!(saved.profiles.len(), 1);
        for dir in [keys_dir, clicks_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_saved_profile_opens_and_starts_with_two_clicks() {
        let dir = folder("profile");
        let mut driver = driver(&dir);
        let mut saved = RuntimeConfig::default();
        saved.set_payout(MAINNET.into()).unwrap();
        saved.set_intensity(70).unwrap();
        driver
            .flow
            .profiles
            .upsert(
                None,
                "Rig A",
                SavedConfig::from_effective("auto", &DeviceSelection::Default, &saved),
            )
            .unwrap();
        driver.flow.step = SetupStep::Profiles;
        let view = driver.view();
        assert_eq!(view.title, "Mining profiles");
        assert_eq!(view.back, "Quit");
        assert!(view.items[0]
            .value_text()
            .contains("GPU · Mainnet · PHOTON"));
        // The profile's own row opens, renames and deletes it.
        assert_eq!(
            view.items[0].actions,
            [
                (SetupCommand::Open, "Open"),
                (SetupCommand::Rename, "Rename"),
                (SetupCommand::Delete, "Delete")
            ]
        );
        assert!(view.commands.is_empty());
        driver.act(SetupInput::Click(item(&driver, "Rig A")));
        let view = driver.view();
        assert!(view.status.contains("Loaded"), "{}", view.status);
        let start = view.items.iter().find(|item| item.selected).unwrap();
        assert_eq!(start.kind, ItemKind::Start);
        let index = view.items.iter().position(|item| item.selected).unwrap();
        let result = complete(driver.act(SetupInput::Click(index)));
        assert_eq!(result.profile_name, "Rig A");
        assert_eq!(result.config.intensity, 70);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A setup with `names` saved as profiles in `dir`.
    fn with_profiles(dir: &Path, names: &[&str]) -> SetupDriver {
        let mut driver = driver(dir);
        let mut saved = RuntimeConfig::default();
        saved.set_payout(MAINNET.into()).unwrap();
        for name in names {
            driver
                .flow
                .profiles
                .upsert(
                    None,
                    name,
                    SavedConfig::from_effective("auto", &DeviceSelection::Default, &saved),
                )
                .unwrap();
        }
        driver
            .flow
            .profiles
            .save(&dir.join("config.profiles.json"))
            .unwrap();
        driver.flow.step = SetupStep::Profiles;
        driver
    }

    #[test]
    fn start_now_starts_the_profile_last_started() {
        let dir = folder("start-now");
        // Nothing started yet: the first profile.
        let mut driver = with_profiles(&dir, &["First", "Second"]);
        assert_eq!(complete(driver.start_now()).profile_name, "First");
        // Second started from its row, then a new window's --start.
        let saved = |dir: &Path| {
            let mut driver = self::driver(dir);
            driver.flow.profiles =
                MiningProfiles::load_optional(&dir.join("config.profiles.json")).unwrap();
            driver.flow.step = SetupStep::Profiles;
            driver
        };
        let mut driver = saved(&dir);
        driver.act(SetupInput::Row {
            item: item(&driver, "Second"),
            command: SetupCommand::Open,
        });
        let start = driver.view().items.iter().position(|item| item.selected);
        complete(driver.act(SetupInput::Click(start.unwrap())));
        assert_eq!(
            last_profile(&dir.join("config.profiles.json")).as_deref(),
            Some("Second")
        );
        assert_eq!(complete(saved(&dir).start_now()).profile_name, "Second");
        // Without a profile the setup stays.
        let mut fresh = self::driver(&folder("start-none"));
        assert!(matches!(fresh.start_now(), SetupOutcome::Continue));
        assert_eq!(fresh.flow.step, SetupStep::Hardware);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_profiles_row_renames_and_deletes_that_profile() {
        let dir = folder("rows");
        let mut driver = with_profiles(&dir, &["First", "Second"]);
        // The cursor rests on First; Second's row acts on Second.
        driver.act(SetupInput::Row {
            item: item(&driver, "Second"),
            command: SetupCommand::Rename,
        });
        let edit = driver.view().edit.expect("the name opens for typing");
        assert_eq!(edit.text, "Second");
        driver.act(SetupInput::SetText("Garage".into()));
        driver.act(SetupInput::Commit);
        assert_eq!(driver.flow.profiles.profiles[1].name, "Garage");
        driver.act(SetupInput::Select(item(&driver, "First")));
        driver.act(SetupInput::Row {
            item: item(&driver, "Garage"),
            command: SetupCommand::Delete,
        });
        // Delete asks first; the rows wait for the answer.
        let view = driver.view();
        assert!(view.status.contains("Garage"), "{}", view.status);
        assert!(view.items.iter().all(|item| item.actions.is_empty()));
        assert_eq!(
            view.commands,
            [(SetupCommand::Yes, "Delete"), (SetupCommand::No, "Keep")]
        );
        driver.act(SetupInput::Command(SetupCommand::Yes));
        let names: Vec<_> = driver
            .flow
            .profiles
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect();
        assert_eq!(names, ["First"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_field_keeps_a_wrong_value_open_and_takes_pastes() {
        let dir = folder("field");
        let mut driver = driver(&dir);
        driver.flow.open_settings(SettingsRow::Address);
        let address = item(&driver, "Address");
        driver.act(SetupInput::Click(address));
        driver.act(SetupInput::SetText("not an address".into()));
        assert_eq!(driver.view().edit.unwrap().text, "not an address");
        driver.act(SetupInput::Commit);
        let view = driver.view();
        assert!(view.edit.is_some(), "a wrong address stays open");
        assert!(!view.status.is_empty());
        assert_eq!(view.back, "Cancel");
        // Other rows wait while the field is open, as in the terminal.
        driver.act(SetupInput::Click(item(&driver, "Intensity")));
        assert_eq!(driver.flow.current_row(), SettingsRow::Address);
        driver.act(SetupInput::SetText(String::new()));
        driver.act(SetupInput::Paste(format!("{MAINNET}\n")));
        assert_eq!(driver.view().edit.unwrap().text, format!("{MAINNET} "));
        driver.act(SetupInput::Commit);
        assert!(driver.view().edit.is_none());
        assert_eq!(driver.flow.config.payout_address, MAINNET);
        // Esc closes a field without saving.
        driver.act(SetupInput::Click(address));
        driver.act(SetupInput::SetText("changed".into()));
        driver.act(SetupInput::CancelEdit);
        assert_eq!(driver.flow.config.payout_address, MAINNET);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_server_is_added_and_removed_on_the_connection_list() {
        let dir = folder("connections");
        let mut driver = driver(&dir);
        driver.flow.open_settings(SettingsRow::Fulcrum);
        driver.act(SetupInput::Click(item(&driver, "Fulcrum")));
        let view = driver.view();
        assert_eq!(
            view.title,
            "Fulcrum servers · Mainnet · shared by all profiles"
        );
        assert!(view
            .items
            .iter()
            .any(|item| item.value_text().contains("Built-in")));
        driver.act(SetupInput::Click(item(&driver, "+ add server")));
        assert_eq!(driver.view().edit.unwrap().label, "Fulcrum server");
        driver.act(SetupInput::SetText(
            "wss://fulcrum.example.org:50004".into(),
        ));
        driver.act(SetupInput::Commit);
        let view = driver.view();
        assert!(
            view.status.starts_with("Saved for every profile"),
            "{}",
            view.status
        );
        assert_eq!(
            driver
                .flow
                .sources
                .list(MiningNetwork::Mainnet, ConnectionKind::Fulcrum)
                .len(),
            1
        );
        let saved = driver
            .view()
            .items
            .iter()
            .position(|item| item.target == Target::Connection(0))
            .unwrap();
        assert!(driver.view().items[saved]
            .label
            .contains("fulcrum.example.org"));
        assert_eq!(
            driver.view().items[saved].actions,
            [(SetupCommand::Delete, "Remove")]
        );
        assert_eq!(driver.view().back, "Done");
        // A screen command is not a row's: nothing is removed.
        driver.act(SetupInput::Command(SetupCommand::Delete));
        assert_eq!(
            driver
                .flow
                .sources
                .list(MiningNetwork::Mainnet, ConnectionKind::Fulcrum)
                .len(),
            1
        );
        driver.act(SetupInput::Row {
            item: saved,
            command: SetupCommand::Delete,
        });
        assert!(driver
            .flow
            .sources
            .list(MiningNetwork::Mainnet, ConnectionKind::Fulcrum)
            .is_empty());
        // A command the screen does not offer does nothing.
        driver.act(SetupInput::Command(SetupCommand::AllGpus));
        assert_eq!(driver.flow.step, SetupStep::Connections);
        driver.act(SetupInput::Back);
        assert_eq!(driver.flow.step, SetupStep::Settings);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn steps_change_values_and_shares_open_first() {
        let dir = folder("steps");
        let mut driver = driver(&dir);
        // No fee pot is made without a profile file.
        driver.flow.profile_path = None;
        driver.flow.open_settings(SettingsRow::Intensity);
        let intensity = item(&driver, "Intensity");
        let before = driver.flow.config.intensity;
        driver.act(SetupInput::Step {
            item: intensity,
            forward: false,
        });
        assert_eq!(driver.flow.config.intensity, before - 10);
        // A split's share opens with Enter first, then moves.
        driver.flow.split = true;
        driver.flow.split_shares = Some(driver.flow.shares());
        let first = driver.flow.shares().at(0).unwrap();
        let share = item(&driver, first.0.name());
        assert!(matches!(
            driver.view().items[share].kind,
            ItemKind::Cycle { enter: None }
        ));
        driver.act(SetupInput::Step {
            item: share,
            forward: false,
        });
        assert_eq!(driver.flow.shares().at(0).unwrap().1, first.1 - 5);
        // The Advanced header is a toggle.
        let advanced = item(&driver, "Advanced");
        assert_eq!(
            driver.view().items[advanced].kind,
            ItemKind::Toggle { on: false }
        );
        driver.act(SetupInput::Click(advanced));
        assert!(driver.flow.advanced_open);
        assert!(driver
            .view()
            .items
            .iter()
            .any(|item| item.label == "Donation"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gpus_tick_with_a_click_and_keep_one() {
        let dir = folder("gpus");
        let mut driver = driver(&dir);
        driver.flow.step = SetupStep::Gpus;
        let second = item(&driver, "Secondary HIP");
        driver.act(SetupInput::Click(second));
        assert_eq!(driver.flow.chosen, [true, true]);
        driver.act(SetupInput::Click(item(&driver, "Primary CUDA")));
        driver.act(SetupInput::Click(second));
        assert_eq!(driver.flow.chosen, [false, true]);
        assert_eq!(driver.view().status, "At least one GPU must mine.");
        driver.act(SetupInput::Command(SetupCommand::AllGpus));
        assert_eq!(driver.flow.chosen, [true, true]);
        driver.act(SetupInput::Key(SetupKey::Enter));
        assert_eq!(driver.flow.step, SetupStep::Settings);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_window_shows_the_terminals_words() {
        let dir = folder("words");
        let mut driver = driver(&dir);
        driver.flow.config.set_network(MiningNetwork::Chipnet);
        for step in [
            SetupStep::Hardware,
            SetupStep::Network,
            SetupStep::Token,
            SetupStep::Settings,
            SetupStep::Connections,
            SetupStep::Gpus,
        ] {
            driver.flow.step = step;
            driver.flow.advanced_open = true;
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(220, 60)).unwrap();
            terminal
                .draw(|frame| render_setup(frame, &driver.flow))
                .unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            let view = driver.view();
            assert!(screen.contains(&view.heading), "{step:?}");
            assert!(screen.contains(&view.title), "{step:?}");
            for item in &view.items {
                for text in [
                    item.label.trim(),
                    item.value_text().trim(),
                    item.hint.trim(),
                ] {
                    assert!(screen.contains(text), "{step:?}: {text:?}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    // The window chooses a pool's mode with < > and
    // starts what the terminal's keys start, P2Pool's settings included.
    #[test]
    fn a_pool_mode_starts_from_the_window_as_from_the_keys() {
        const CHIPNET: &str = crate::config::CHIPNET_DONATION_ADDRESS;
        let node = |flow: &mut SetupFlow| {
            flow.sources
                .put(
                    MiningNetwork::Chipnet,
                    ConnectionKind::Node,
                    None,
                    "http://192.168.1.20:48332",
                )
                .unwrap();
        };
        // The terminal's keys: Run a pool, Chipnet, ASIC pool, then the
        // mode, the address, the profile's name and Start.
        let keys_dir = folder("pool-keys");
        let mut keys = flow(&keys_dir);
        node(&mut keys);
        keys.handle_key(key(KeyCode::Down));
        keys.handle_key(key(KeyCode::Down));
        keys.handle_key(key(KeyCode::Enter));
        keys.handle_key(key(KeyCode::Down));
        keys.handle_key(key(KeyCode::Enter));
        keys.handle_key(key(KeyCode::Enter));
        assert_eq!(keys.current_row(), SettingsRow::PoolMode);
        keys.handle_key(key(KeyCode::Right));
        keys.open_settings(SettingsRow::Address);
        keys.handle_key(key(KeyCode::Enter));
        type_keys(&mut keys, CHIPNET);
        keys.handle_key(key(KeyCode::Enter));
        keys.open_settings(SettingsRow::ProfileName);
        keys.handle_key(key(KeyCode::Enter));
        type_keys(&mut keys, "Shared");
        keys.handle_key(key(KeyCode::Enter));
        keys.open_settings(SettingsRow::Start);
        assert_eq!(keys.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
        let by_keys = keys.finish().unwrap();

        // The same with clicks.
        let clicks_dir = folder("pool-clicks");
        let mut clicks = driver(&clicks_dir);
        node(&mut clicks.flow);
        clicks.act(SetupInput::Click(item(&clicks, "Run a pool")));
        clicks.act(SetupInput::Click(item(&clicks, "Chipnet")));
        clicks.act(SetupInput::Click(item(&clicks, "ASIC")));
        let mode = item(&clicks, "Mode");
        assert_eq!(
            clicks.view().items[mode].kind,
            ItemKind::Cycle { enter: None }
        );
        assert_eq!(
            clicks.view().items[mode].value_text(),
            "Public Solo Endpoint"
        );
        clicks.act(SetupInput::Step {
            item: mode,
            forward: true,
        });
        let view = clicks.view();
        assert_eq!(view.items[mode].value_text(), "Non-custodial Pool");
        // The line under it says what differs.
        assert_eq!(view.items[mode + 1].kind, ItemKind::Info);
        assert!(view.items[mode + 1]
            .value_text()
            .contains("P2Pool on your server alone"));
        clicks.act(SetupInput::Click(item(&clicks, "Address")));
        clicks.act(SetupInput::SetText(CHIPNET.into()));
        clicks.act(SetupInput::Commit);
        clicks.act(SetupInput::Click(item(&clicks, "Advanced")));
        for label in ["Pickaxe donation", "P2Pool donation", "Fallback pools"] {
            item(&clicks, label);
        }
        clicks.act(SetupInput::Click(item(&clicks, "Profile name")));
        clicks.act(SetupInput::SetText("Shared".into()));
        clicks.act(SetupInput::Commit);
        let start = clicks
            .view()
            .items
            .iter()
            .position(|item| item.kind == ItemKind::Start)
            .unwrap();
        assert_eq!(
            clicks.view().items[start].value_text(),
            "Start the Non-custodial Pool"
        );
        let by_clicks = complete(clicks.act(SetupInput::Click(start)));

        assert_eq!(started(&by_clicks), started(&by_keys));
        assert!(matches!(
            by_clicks.server,
            Some(ServerSetup::P2PoolPool {
                standalone: true,
                ..
            })
        ));
        assert!(by_clicks.config.p2pool.standalone);
        for dir in [keys_dir, clicks_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    // What: Start pressed while this computer is still looked at goes to
    // another thread (it waits a moment for the look), as Start with
    // ELECTRIC does; once the look is in, Start runs at once.
    // Look here if: looks_here or presses_start change.
    #[test]
    fn start_waits_away_from_the_window_while_this_computer_is_looked_at() {
        let dir = folder("looks-here");
        let mut driver = driver(&dir);
        driver.flow.probe_local_node = |_| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            crate::node::LocalNode::Missing
        };
        driver.flow.look_here();
        driver.flow.open_settings(SettingsRow::Start);
        let enter = SetupInput::Key(SetupKey::Enter);
        assert!(driver.looks_here(&enter));
        assert!(!driver.reads_pot(&enter));
        let start = driver
            .view()
            .items
            .iter()
            .position(|item| item.kind == ItemKind::Start)
            .unwrap();
        assert!(driver.looks_here(&SetupInput::Click(start)));
        assert!(!driver.looks_here(&SetupInput::Key(SetupKey::Down)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while driver.flow.looking_here() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
            driver.tick();
        }
        assert!(!driver.looks_here(&enter));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_driver_moves_to_another_thread() {
        fn sends<T: Send>() {}
        sends::<SetupDriver>();
    }
}
