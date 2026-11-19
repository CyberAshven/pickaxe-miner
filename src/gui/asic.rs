//! The BCH ASIC server in the window: its workers, overview, Connection
//! info, Advanced settings and Device panel. The server runs exactly as
//! from the terminal (`stratum_v2::command::run`); the window takes its
//! dashboard's place through `stratum_v2::view::ServerView`, so it shows
//! what the terminal pages show and sends each request back to the server,
//! which applies it as the terminal's keys do.
use crate::p2pool::launch::Light;
use crate::stratum_v2::view::{
    self, ConnectInfo, ConnectLine, PanelKey, ServeMode, ServerControl, ServerFrame, ServerView,
};
use eframe::egui;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// What the window shows of the server.
#[derive(Debug, Clone, Default)]
pub struct AsicBoard {
    /// The server shows its frames.
    pub live: bool,
    /// The server's latest tick.
    pub frame: Option<ServerFrame>,
    /// When the first frame came.
    pub started: Option<Instant>,
    /// Stop was asked for.
    pub stopping: bool,
    /// The server has returned.
    pub ended: bool,
    /// Why it stopped, if it failed.
    pub error: Option<String>,
}

impl AsicBoard {
    /// Whether the server takes requests now (or will at its first tick).
    fn steerable(&self) -> bool {
        !self.ended && !self.stopping
    }
}

/// What the window and the server's view share.
#[derive(Default)]
pub struct AsicLink {
    board: Mutex<AsicBoard>,
    controls: Mutex<Vec<ServerControl>>,
    context: OnceLock<egui::Context>,
}

impl AsicLink {
    pub fn set_context(&self, context: egui::Context) {
        let _ = self.context.set(context);
    }

    pub fn board(&self) -> AsicBoard {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Asks the server for `control` at its next tick.
    pub fn send(&self, control: ServerControl) {
        if control == ServerControl::Stop {
            self.update(|board| board.stopping = true);
        }
        self.controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(control);
    }

    /// A new server starts with an empty board and no old requests.
    pub fn reset(&self) {
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = AsicBoard::default();
        self.controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The server returned, with its error if it failed.
    pub fn finished(&self, error: Option<String>) {
        self.update(|board| {
            board.live = false;
            board.ended = true;
            board.stopping = false;
            board.error = error;
        });
    }

    fn take(&self) -> Vec<ServerControl> {
        std::mem::take(&mut *self.controls.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn update(&self, change: impl FnOnce(&mut AsicBoard)) {
        change(&mut self.board.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(context) = self.context.get() {
            context.request_repaint();
        }
    }
}

/// Shows the ASIC server in the window (see `stratum_v2::view::ServerView`).
pub struct WindowServerView(pub Arc<AsicLink>);

impl ServerView for WindowServerView {
    fn show(&self, frame: ServerFrame) {
        self.0.update(|board| {
            board.live = true;
            board.started.get_or_insert_with(Instant::now);
            board.frame = Some(frame);
        });
    }

    fn controls(&self) -> Vec<ServerControl> {
        self.0.take()
    }
}

/// The terminal dashboard's pages: Tab, i and a there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Workers,
    Overview,
    Connection,
    Advanced,
}

fn page_id() -> egui::Id {
    egui::Id::unique("pickaxe.asic.page")
}

fn copied_id() -> egui::Id {
    egui::Id::unique("pickaxe.asic.copied")
}

/// Shows `page` the next time the server is drawn.
pub fn open_page(context: &egui::Context, page: Page) {
    context.data_mut(|data| data.insert_temp(page_id(), page));
}

/// Draws the server: a bar with its state and Stop, then the page chosen,
/// or the Device panel over them while one is open.
pub fn show(ui: &mut egui::Ui, board: &AsicBoard, link: &AsicLink) {
    link.set_context(ui.ctx().clone());
    egui::Panel::top("asic-bar").show(ui, |ui| top_bar(ui, board, link));
    egui::CentralPanel::default().show(ui, |ui| {
        let Some(frame) = &board.frame else {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.weak(if board.ended {
                    "The server stopped before it served."
                } else {
                    "Starting the server: reading the node, binding the ports…"
                })
            });
            return;
        };
        if let Some(panel) = &frame.panel {
            device_panel(ui, panel, board, link);
            return;
        }
        let page = tabs(ui, link);
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match page {
                Page::Workers => workers(ui, frame, board, link),
                Page::Overview => overview(ui, frame),
                Page::Connection => connection(ui, &frame.connect),
                Page::Advanced => advanced(ui, frame, board, link),
            });
    });
}

/// The server's state, as the bar says it.
pub fn state_text(board: &AsicBoard) -> String {
    if board.ended {
        return match &board.error {
            Some(error) => format!("Stopped: {error}"),
            None => "Stopped".into(),
        };
    }
    if board.stopping {
        return "Stopping…".into();
    }
    match &board.frame {
        Some(frame) if board.live => format!("Serving · {}", mode_text(frame)),
        _ => "Starting…".into(),
    }
}

/// What devices mine here, in a few words.
fn mode_text(frame: &ServerFrame) -> String {
    match frame.mode {
        ServeMode::Solo => "solo".into(),
        ServeMode::Public => "public pool".into(),
        ServeMode::JoinPool if frame.joining => "at a pool".into(),
        ServeMode::JoinPool => "at a pool with your templates".into(),
        ServeMode::Token(token) => token.into(),
    }
}

fn top_bar(ui: &mut egui::Ui, board: &AsicBoard, link: &AsicLink) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Pickaxe").size(20.0).strong());
        if let Some(frame) = &board.frame {
            ui.label(egui::RichText::new(frame.network.as_str()).monospace());
            ui.label(frame.title.trim_start_matches("Pickaxe · "));
        }
        ui.separator();
        ui.label(state_text(board));
        if let Some(started) = board.started.filter(|_| board.live) {
            ui.weak(super::dashboard::clock(started.elapsed()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if board.steerable()
                && ui
                    .button("Stop")
                    .on_hover_text("Stop the server, as q does in the terminal")
                    .clicked()
            {
                link.send(ServerControl::Stop);
            }
        });
    });
    ui.add_space(4.0);
}

fn tabs(ui: &mut egui::Ui, link: &AsicLink) -> Page {
    let mut page: Page = ui.data(|data| data.get_temp(page_id())).unwrap_or_default();
    ui.horizontal(|ui| {
        for (choice, name) in [
            (Page::Workers, "Workers"),
            (Page::Overview, "Overview"),
            (Page::Connection, "Connection info"),
            (Page::Advanced, "Advanced settings"),
        ] {
            if ui.selectable_label(page == choice, name).clicked() && page != choice {
                // Opening Connection info reads this computer's addresses
                // again, as `i` does.
                if choice == Page::Connection {
                    link.send(ServerControl::ReadAddresses);
                }
                page = choice;
            }
        }
    });
    open_page(ui.ctx(), page);
    page
}

fn group(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui);
        });
}

fn light_color(light: Light) -> egui::Color32 {
    match light {
        Light::Green => egui::Color32::from_rgb(64, 192, 87),
        Light::Yellow => egui::Color32::from_rgb(250, 176, 5),
        Light::Red => egui::Color32::from_rgb(250, 82, 82),
        Light::Grey => egui::Color32::GRAY,
    }
}

/// The lights over the workers and overview pages.
fn lights(ui: &mut egui::Ui, lights: &[(&'static str, Light, String)]) {
    if lights.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (name, light, text) in lights {
            // Drawn, as the terminal's ● and ○: the bundled fonts may lack them.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            let color = light_color(*light);
            if *light == Light::Grey {
                ui.painter()
                    .circle_stroke(rect.center(), 5.0, egui::Stroke::new(1.5, color));
            } else {
                ui.painter().circle_filled(rect.center(), 5.5, color);
            }
            ui.strong(*name);
            ui.label(text);
            ui.add_space(16.0);
        }
    });
    ui.add_space(6.0);
}

/// Several lines of the terminal's text, the first one strong.
fn text_lines(ui: &mut egui::Ui, text: &str) {
    for (index, line) in text.lines().enumerate() {
        if index == 0 {
            ui.strong(line);
        } else {
            ui.label(line);
        }
    }
}

fn workers(ui: &mut egui::Ui, frame: &ServerFrame, board: &AsicBoard, link: &AsicLink) {
    lights(ui, &frame.lights);
    group(ui, |ui| text_lines(ui, &frame.header));
    ui.add_space(8.0);
    group(ui, |ui| {
        ui.strong(format!("Workers · {}", frame.devices.len()));
        ui.add_space(4.0);
        if frame.devices.is_empty() {
            ui.weak("Each device shows here once it connects.");
            return;
        }
        egui::ScrollArea::horizontal()
            .id_salt("asic-workers-scroll")
            .show(ui, |ui| {
                egui::Grid::new("asic-workers")
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("");
                        for heading in view::WORKER_COLUMNS {
                            ui.strong(heading);
                        }
                        ui.end_row();
                        for device in &frame.devices {
                            if ui
                                .add_enabled(
                                    board.live && board.steerable(),
                                    egui::Button::new("Panel"),
                                )
                                .accessible_name(format!(
                                    "Open the Device panel of {}",
                                    device.label
                                ))
                                .on_hover_text(
                                    "The Device panel: restart, pause, locate, power, fan, pools \
                                     and the device's login",
                                )
                                .clicked()
                            {
                                link.send(ServerControl::OpenDevice(device.label.clone()));
                            }
                            for (index, cell) in view::worker_cells(device).into_iter().enumerate()
                            {
                                match index {
                                    0 => ui.label(cell),
                                    1 if device.connected => {
                                        ui.colored_label(light_color(Light::Green), cell)
                                    }
                                    1 => ui.weak(cell),
                                    _ => ui.monospace(cell),
                                };
                            }
                            ui.end_row();
                        }
                    });
            });
    });
    ui.add_space(4.0);
    ui.weak(
        "Now and 1 hour come from validated shares (30s warm-up); Device says, Temp and Fan are \
         the device's own report.",
    );
}

/// A titled number, as the token dashboard's cards.
fn card(ui: &mut egui::Ui, title: &str, value: &str, detail: Option<&str>) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.vertical(|ui| {
                ui.weak(title);
                ui.heading(value);
                if let Some(detail) = detail {
                    ui.weak(detail);
                }
            });
        });
}

fn overview(ui: &mut egui::Ui, frame: &ServerFrame) {
    lights(ui, &frame.lights);
    let stats = &frame.stats;
    let online = frame
        .devices
        .iter()
        .filter(|device| device.connected)
        .count();
    let rate: f64 = frame
        .devices
        .iter()
        .filter(|device| device.connected)
        .filter_map(|device| device.hashrate_estimate)
        .sum();
    ui.horizontal_wrapped(|ui| {
        card(
            ui,
            "Workers online",
            &format!("{online} of {}", frame.devices.len()),
            Some(&crate::telemetry::format_hash_rate(rate)),
        );
        card(
            ui,
            "Shares",
            &format!("{} accepted", stats.shares_accepted),
            Some(&format!("{} rejected", stats.shares_rejected)),
        );
        // A pool that builds the blocks finds them, not this server.
        if !frame.joining {
            card(
                ui,
                "Blocks",
                &format!("{} accepted", stats.blocks_accepted),
                Some(&format!(
                    "{} pending · {} rejected",
                    stats.blocks_pending, stats.blocks_rejected
                )),
            );
            card(
                ui,
                "Height",
                &stats
                    .height
                    .map_or_else(|| "Waiting".into(), |height| height.to_string()),
                None,
            );
        }
        let (best, by) = stats.best_share.as_ref().map_or_else(
            || ("none yet".to_owned(), None),
            |(difficulty, worker)| {
                (
                    view::format_difficulty(Some(*difficulty)),
                    Some(format!("by {worker}")),
                )
            },
        );
        card(ui, "Best share", &best, by.as_deref());
    });
    ui.add_space(8.0);
    group(ui, |ui| text_lines(ui, &frame.overview));
    ui.add_space(8.0);
    group(ui, |ui| {
        ui.strong(format!("Devices · {} sessions", frame.devices.len()));
        ui.add_space(4.0);
        egui::ScrollArea::horizontal()
            .id_salt("asic-devices-scroll")
            .show(ui, |ui| {
                egui::Grid::new("asic-devices")
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for heading in view::DEVICE_COLUMNS {
                            ui.strong(heading);
                        }
                        ui.end_row();
                        for device in &frame.devices {
                            for (index, cell) in view::device_cells(device).into_iter().enumerate()
                            {
                                if index == 0 {
                                    ui.label(cell);
                                } else {
                                    ui.monospace(cell);
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
    });
    ui.add_space(4.0);
    ui.weak("Rate uses validated shares; 30s warm-up, up to 5m window.");
}

/// One address with its place and a Copy button.
fn address_rows(ui: &mut egui::Ui, id: &str, lines: &[ConnectLine]) {
    egui::Grid::new(id).spacing([14.0, 6.0]).show(ui, |ui| {
        for line in lines {
            ui.weak(line.place.label());
            ui.monospace(&line.url);
            let kind = if line.sv2 { "SV2" } else { "SV1" };
            if ui
                .small_button("Copy")
                .accessible_name(format!(
                    "Copy the {kind} address for {}",
                    line.place.label()
                ))
                .clicked()
            {
                copy(ui, &line.url);
            }
            ui.end_row();
        }
    });
}

/// Copies `text` and says so under Connection info, as its number keys do.
fn copy(ui: &egui::Ui, text: &str) {
    ui.ctx().copy_text(text.to_owned());
    ui.data_mut(|data| data.insert_temp(copied_id(), format!("Copied {text}")));
}

fn connection(ui: &mut egui::Ui, connect: &ConnectInfo) {
    group(ui, |ui| {
        ui.strong("How devices connect");
        if connect.lines.is_empty() {
            ui.label(view::NO_LISTENER);
        }
        // SV1 lines come first, then SV2, each under its heading.
        let mut start = 0;
        while start < connect.lines.len() {
            let sv2 = connect.lines[start].sv2;
            let end = connect.lines[start..]
                .iter()
                .position(|line| line.sv2 != sv2)
                .map_or(connect.lines.len(), |offset| start + offset);
            ui.add_space(6.0);
            ui.label(format!("{}:", view::connect_heading(sv2)));
            address_rows(
                ui,
                if sv2 {
                    "asic-sv2-lines"
                } else {
                    "asic-sv1-lines"
                },
                &connect.lines[start..end],
            );
            start = end;
        }
        ui.add_space(6.0);
        ui.label(connect.notes.trim());
    });
    if !connect.templates.is_empty() {
        ui.add_space(8.0);
        group(ui, |ui| {
            ui.label(format!("{}:", view::TEMPLATES_HEADING));
            address_rows(ui, "asic-template-lines", &connect.templates);
            if let Some(config) = &connect.template_config {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("An SRI pool or Job Declaration client takes them with:");
                    if ui
                        .small_button("Copy")
                        .accessible_name("Copy the template provider settings")
                        .clicked()
                    {
                        copy(ui, config);
                    }
                });
                ui.code(config.trim_end());
            }
        });
    }
    if let Some(note) = ui.data(|data| data.get_temp::<String>(copied_id())) {
        ui.add_space(6.0);
        ui.weak(note);
    }
}

/// A ◀ or ▶ button, drawn: the bundled fonts have no such glyphs.
fn step_button(ui: &mut egui::Ui, enabled: bool, up: bool, name: &str) -> egui::Response {
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new("").min_size(egui::vec2(30.0, 24.0)),
        )
        .accessible_name(name);
    let color = if enabled {
        ui.style().interact(&response).fg_stroke.color
    } else {
        ui.visuals().weak_text_color()
    };
    let mark = response.rect.shrink2(egui::vec2(11.0, 7.0));
    let points = if up {
        vec![mark.left_top(), mark.left_bottom(), mark.right_center()]
    } else {
        vec![mark.right_top(), mark.right_bottom(), mark.left_center()]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    response
}

fn advanced(ui: &mut egui::Ui, frame: &ServerFrame, board: &AsicBoard, link: &AsicLink) {
    let can = board.live && board.steerable();
    group(ui, |ui| {
        ui.strong("Donation");
        ui.horizontal(|ui| {
            if step_button(ui, can, false, "Lower the donation by 0.5%").clicked() {
                link.send(ServerControl::Donation { up: false });
            }
            ui.heading(frame.donation.to_string());
            if step_button(ui, can, true, "Raise the donation by 0.5%").clicked() {
                link.send(ServerControl::Donation { up: true });
            }
        });
        // The terminal's keys work here too: ←/→, + (or =) and −.
        if can {
            let (down, up) = ui.input(|input| {
                let pressed = |keys: &[egui::Key]| keys.iter().any(|key| input.key_pressed(*key));
                (
                    pressed(&[egui::Key::ArrowLeft, egui::Key::Minus]),
                    pressed(&[egui::Key::ArrowRight, egui::Key::Plus, egui::Key::Equals]),
                )
            });
            if down || up {
                link.send(ServerControl::Donation { up });
            }
        }
        ui.add_space(4.0);
        ui.label(&frame.advanced);
        if let Some(error) = frame.setting_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
    ui.add_space(8.0);
    group(ui, |ui| text_lines(ui, &frame.started));
}

/// A Device panel line that names keys, such as "1  Restart" or "Tab  Next
/// field · Enter  Continue · Esc  Back", as each key and what it does;
/// `None` for any other line.
pub fn key_line(line: &str) -> Option<Vec<(PanelKey, String)>> {
    // A pool's line names an address, not a key.
    if line.contains("://") {
        return None;
    }
    line.split(" · ")
        .map(|part| {
            let (key, label) = part.split_once("  ")?;
            let key = match key {
                "Esc" | "any other key" => PanelKey::Esc,
                "Enter" => PanelKey::Enter,
                "Tab" => PanelKey::Tab,
                key => {
                    let mut chars = key.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) if c.is_ascii_alphanumeric() => PanelKey::Char(c),
                        _ => return None,
                    }
                }
            };
            let label = label.trim();
            (!label.is_empty()).then(|| (key, label.to_owned()))
        })
        .collect()
}

/// The keys typed in the window, as the Device panel reads them.
pub fn panel_keys(events: &[egui::Event]) -> Vec<PanelKey> {
    let mut keys = Vec::new();
    for event in events {
        match event {
            egui::Event::Text(text) | egui::Event::Paste(text) => {
                keys.extend(text.chars().filter(|c| !c.is_control()).map(PanelKey::Char))
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => keys.extend(match key {
                egui::Key::Enter => Some(PanelKey::Enter),
                egui::Key::Escape => Some(PanelKey::Esc),
                egui::Key::Tab if modifiers.shift => Some(PanelKey::BackTab),
                egui::Key::Tab => Some(PanelKey::Tab),
                egui::Key::Backspace => Some(PanelKey::Backspace),
                _ => None,
            }),
            _ => (),
        }
    }
    keys
}

/// The Device panel, as the terminal draws it: its text, with the keys it
/// names as buttons. Typed keys go to the panel, so its number, letter,
/// Enter, Esc, Tab and Backspace keys work as in the terminal.
fn device_panel(ui: &mut egui::Ui, text: &str, board: &AsicBoard, link: &AsicLink) {
    let can = board.live && board.steerable();
    ui.horizontal(|ui| {
        ui.strong("Device panel");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(can, egui::Button::new("Back to workers"))
                .clicked()
            {
                link.send(ServerControl::ClosePanel);
            }
        });
    });
    ui.weak(
        "Type the keys the panel names, or press their buttons. Nothing reaches the device \
         without your confirmation.",
    );
    // The panel holds the keyboard: Tab and Enter go to it, never to a
    // button that happens to have focus.
    let holder = ui
        .interact(
            egui::Rect::from_min_size(ui.cursor().min, egui::Vec2::ZERO),
            egui::Id::unique("pickaxe.asic.panel-keys"),
            egui::Sense::focusable_noninteractive(),
        )
        .accessible_name("Device panel keys");
    if can {
        holder.request_focus();
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                holder.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        for key in panel_keys(&ui.input(|input| input.events.clone())) {
            link.send(ServerControl::PanelKey(key));
        }
    }
    ui.add_space(6.0);
    group(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in text.lines() {
                    match key_line(line) {
                        Some(keys) => {
                            ui.horizontal_wrapped(|ui| {
                                for (key, label) in keys {
                                    ui.label(egui::RichText::new(key_name(key)).monospace().weak());
                                    if ui
                                        .add_enabled(can, egui::Button::new(&label))
                                        .on_hover_text(format!("Key {}", key_name(key)))
                                        .clicked()
                                    {
                                        link.send(ServerControl::PanelKey(key));
                                    }
                                }
                            });
                        }
                        None => {
                            ui.label(line);
                        }
                    }
                }
            });
    });
}

fn key_name(key: PanelKey) -> String {
    match key {
        PanelKey::Char(c) => c.to_string(),
        PanelKey::Enter => "Enter".into(),
        PanelKey::Esc => "Esc".into(),
        PanelKey::Tab => "Tab".into(),
        PanelKey::BackTab => "Shift+Tab".into(),
        PanelKey::Backspace => "Backspace".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> ServerFrame {
        ServerFrame {
            network: crate::config::MiningNetwork::Chipnet,
            title: "Pickaxe · BCH ASIC mining",
            mode: ServeMode::Solo,
            joining: false,
            pool: None,
            node_client: None,
            stats: Default::default(),
            devices: Vec::new(),
            lights: Vec::new(),
            donation: Default::default(),
            setting_error: None,
            header: String::new(),
            overview: String::new(),
            advanced: String::new(),
            started: String::new(),
            devices_hint: String::new(),
            connect: ConnectInfo::default(),
            panel: None,
        }
    }

    #[test]
    fn frames_reach_the_board_and_requests_reach_the_server() {
        let link = Arc::new(AsicLink::default());
        let view = WindowServerView(Arc::clone(&link));
        assert_eq!(state_text(&link.board()), "Starting…");
        view.show(frame());
        let board = link.board();
        assert!(board.live && board.started.is_some());
        assert_eq!(state_text(&board), "Serving · solo");
        link.send(ServerControl::Donation { up: true });
        link.send(ServerControl::OpenDevice("rig1 #1".into()));
        assert_eq!(
            view.controls(),
            vec![
                ServerControl::Donation { up: true },
                ServerControl::OpenDevice("rig1 #1".into())
            ]
        );
        assert!(view.controls().is_empty(), "each request is sent once");
        link.send(ServerControl::Stop);
        assert_eq!(state_text(&link.board()), "Stopping…");
        assert_eq!(view.controls(), vec![ServerControl::Stop]);
        link.finished(Some("cannot bind SV1 listener".into()));
        let board = link.board();
        assert!(!board.live && !board.steerable());
        assert_eq!(state_text(&board), "Stopped: cannot bind SV1 listener");
    }

    #[test]
    fn a_new_server_starts_from_an_empty_board() {
        let link = Arc::new(AsicLink::default());
        WindowServerView(Arc::clone(&link)).show(frame());
        link.send(ServerControl::Stop);
        link.reset();
        let board = link.board();
        assert!(board.frame.is_none() && !board.stopping && !board.ended);
        assert!(link.take().is_empty());
    }

    #[test]
    fn the_panels_key_lines_become_buttons() {
        assert_eq!(
            key_line("1  Restart"),
            Some(vec![(PanelKey::Char('1'), "Restart".into())])
        );
        assert_eq!(
            key_line("y  Yes · any other key  No"),
            Some(vec![
                (PanelKey::Char('y'), "Yes".into()),
                (PanelKey::Esc, "No".into())
            ])
        );
        assert_eq!(
            key_line("Tab  Next field · Enter  Continue · Esc  Back"),
            Some(vec![
                (PanelKey::Tab, "Next field".into()),
                (PanelKey::Enter, "Continue".into()),
                (PanelKey::Esc, "Back".into())
            ])
        );
        // Text, a worker's line and a pool's line name no keys.
        assert_eq!(key_line("Worker  rig1 #3 · Online"), None);
        assert_eq!(key_line("1  stratum+tcp://192.168.1.20:3333  rig1"), None);
        assert_eq!(key_line("Reached on: local network"), None);
        assert_eq!(key_line(""), None);
    }

    #[test]
    fn typed_keys_reach_the_panel() {
        let key = |key, shift| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        };
        let events = [
            egui::Event::Text("2y".into()),
            key(egui::Key::Enter, false),
            key(egui::Key::Tab, true),
            key(egui::Key::Tab, false),
            key(egui::Key::Backspace, false),
            key(egui::Key::Escape, false),
            key(egui::Key::A, false),
            egui::Event::Copy,
        ];
        assert_eq!(
            panel_keys(&events),
            vec![
                PanelKey::Char('2'),
                PanelKey::Char('y'),
                PanelKey::Enter,
                PanelKey::BackTab,
                PanelKey::Tab,
                PanelKey::Backspace,
                PanelKey::Esc,
            ]
        );
    }
}
