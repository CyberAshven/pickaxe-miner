//! The window's mining dashboard: what the terminal dashboard shows, with
//! its pause, intensity, split and stop controls.
use super::session::{Board, Control, Link, NoticeFor, HISTORY_CAP};
use super::style;
use crate::telemetry::{format_hash_rate, GpuTelemetry};
use eframe::egui;
use std::time::Duration;

/// Chart colors, one per token in the session's order.
const LINE_COLORS: [egui::Color32; 4] = [
    egui::Color32::from_rgb(250, 176, 5),
    egui::Color32::from_rgb(77, 171, 247),
    egui::Color32::from_rgb(105, 219, 124),
    egui::Color32::from_rgb(218, 119, 242),
];

/// How long the answer to a control stays on screen.
const NOTICE_SHOWN: Duration = Duration::from_secs(8);

pub fn show(ui: &mut egui::Ui, board: &Board, link: &Link) {
    keys(ui, board, link);
    egui::Panel::top("bar").show(ui, |ui| top_bar(ui, board, link));
    egui::Panel::bottom("events")
        .resizable(true)
        .default_size(170.0)
        .show(ui, |ui| events(ui, board));
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::both().show(ui, |ui| {
            if starting(board) {
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                "Starting: loading the GPU kernels and reading the chain. The \
                                 first start can take a few minutes.",
                            );
                        });
                    });
                ui.add_space(10.0);
            }
            stats(ui, board, link);
            ui.add_space(10.0);
            tokens(ui, board, link);
            ui.add_space(10.0);
            chart(ui, board);
            if let Some(rigs) = board.runtime.as_ref().and_then(|s| s.rigs.as_ref()) {
                ui.add_space(10.0);
                rigs_panel(ui, rigs, &board.rig_join);
            }
            if board.electric.is_some() {
                ui.add_space(10.0);
                pot(ui, board, link);
            }
            ui.add_space(10.0);
            // GPUs, the chain and PHOTON's settings, a click away.
            egui::CollapsingHeader::new("Details")
                .id_salt("dashboard-details")
                .default_open(board.notice_for == NoticeFor::Settings && fresh(board))
                .show(ui, |ui| {
                    gpus(ui, board);
                    ui.add_space(10.0);
                    chain(ui, board);
                    if board.single_runtime && board.live {
                        ui.add_space(10.0);
                        settings(ui, board, link);
                    }
                });
        });
    });
}

/// The session is starting: nothing has reported yet.
fn starting(board: &Board) -> bool {
    !board.live && board.started.is_none() && board.error.is_none()
}

/// A control's answer is still shown for a while after it was given.
fn fresh(board: &Board) -> bool {
    board.notice.is_some()
        && board
            .notice_at
            .is_some_and(|at| at.elapsed() < NOTICE_SHOWN)
}

/// The last control's answer, if it belongs to this part of the dashboard.
fn notice_here(ui: &mut egui::Ui, board: &Board, place: NoticeFor) {
    if board.notice_for != place || !fresh(board) {
        return;
    }
    if let Some(notice) = &board.notice {
        style::answer(ui, notice, board.notice_error);
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
}

/// The terminal dashboard's keys, when no text field has the keyboard:
/// P or Space pauses or resumes, + and - step the intensity by 5, B stops
/// after pending rewards (Esc takes that back), Q closes the window.
fn keys(ui: &mut egui::Ui, board: &Board, link: &Link) {
    if !board.live || ui.ctx().memory(|memory| memory.focused().is_some()) {
        return;
    }
    let pressed = |key| ui.input(|input| input.key_pressed(key));
    if board.stopping {
        if pressed(egui::Key::Escape) {
            link.send(Control::KeepMining);
        }
        return;
    }
    if pressed(egui::Key::P) || pressed(egui::Key::Space) {
        link.send(Control::Pause(!board.paused));
    }
    if pressed(egui::Key::Plus) || pressed(egui::Key::Equals) {
        link.send(Control::Intensity(
            board.intensity.saturating_add(5).min(100),
        ));
    }
    if pressed(egui::Key::Minus) {
        link.send(Control::Intensity(
            board.intensity.saturating_sub(5).max(10),
        ));
    }
    if pressed(egui::Key::B) {
        link.send(Control::Stop { now: false });
    }
    if pressed(egui::Key::Q) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn state_text(board: &Board) -> String {
    if !board.live {
        return match &board.error {
            Some(error) => format!("Stopped: {error}"),
            None if board.started.is_some() => "Stopped".into(),
            None => "Starting…".into(),
        };
    }
    if board.stopping {
        return if board.pending {
            "Stopping after the pending reward settles…".into()
        } else {
            "Stopping…".into()
        };
    }
    if board.paused {
        return "Paused".into();
    }
    "Mining".into()
}

fn top_bar(ui: &mut egui::Ui, board: &Board, link: &Link) {
    ui.add_space(4.0);
    bar_row(ui, board, link);
    notice_here(ui, board, NoticeFor::Bar);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            style::keys(
                ui,
                "P or Space pause / resume · + and - intensity · B stop after pending \
                 rewards · Esc keep mining · Q close",
            );
        });
    });
    ui.add_space(2.0);
}

fn bar_row(ui: &mut egui::Ui, board: &Board, link: &Link) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Pickaxe").size(20.0).strong());
        if let Some(network) = board.network {
            ui.label(egui::RichText::new(network.as_str()).monospace());
        }
        ui.separator();
        ui.label(state_text(board));
        if let Some(started) = board.started.filter(|_| board.live) {
            ui.label(style::number(clock(started.elapsed()), 13.0).weak());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            super::updates::bar_button(ui);
            if !board.live {
                // Starting (shaders, the fee pot): nothing reads the
                // session's controls yet, so stop it as Ctrl+C would.
                if board.started.is_none()
                    && board.error.is_none()
                    && ui
                        .button("Stop")
                        .on_hover_text("Stop starting this session")
                        .clicked()
                {
                    crate::session_control::stop_session();
                }
                return;
            }
            if board.stopping {
                let asking = egui::Id::unique("pickaxe.stop-now");
                let mut open = ui.data(|data| data.get_temp(asking).unwrap_or(false));
                if ui.button("Stop now").clicked() {
                    open = true;
                }
                if style::confirm(
                    ui,
                    &mut open,
                    "pickaxe.stop-now.dialog",
                    "Stop now?",
                    "Mining stops without waiting for the pending reward, which is kept and \
                     resolved by the next session. This session's changes are not saved.",
                    "Stop now",
                ) == Some(true)
                {
                    link.send(Control::Stop { now: true });
                }
                ui.data_mut(|data| data.insert_temp(asking, open));
                if ui
                    .button("Keep mining")
                    .on_hover_text("Take back Stop and resume mining")
                    .clicked()
                {
                    link.send(Control::KeepMining);
                }
                return;
            }
            if ui.button("Stop").clicked() {
                link.send(Control::Stop { now: false });
            }
            let (label, pause) = if board.paused {
                ("Resume", false)
            } else {
                ("Pause", true)
            };
            if ui.button(label).clicked() {
                link.send(Control::Pause(pause));
            }
        });
    });
}

/// A titled value; `add` gets the title's id to name its widgets by.
fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui, egui::Id)) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.vertical(|ui| {
                let title = ui.weak(title);
                add(ui, title.id);
            });
        });
}

fn stats(ui: &mut egui::Ui, board: &Board, link: &Link) {
    let rewards: u64 = board.tokens.iter().map(|token| token.rewards).sum();
    ui.horizontal_wrapped(|ui| {
        // Tokens count different work, so their rates are never added.
        card(ui, "Hash rate", |ui, _| match board.tokens.as_slice() {
            [] => {
                ui.label(style::number("–", 20.0));
            }
            [token] => {
                ui.label(style::number(format_hash_rate(token.rate), 20.0));
            }
            tokens => {
                for token in tokens {
                    ui.label(style::number(
                        format!("{:<9}{}", token.name, format_hash_rate(token.rate)),
                        14.0,
                    ));
                }
            }
        });
        card(ui, "Rewards this session", |ui, _| {
            match board.tokens.as_slice() {
                [_, _, ..] => {
                    for token in &board.tokens {
                        ui.label(style::number(
                            format!("{:<9}{}", token.name, token.rewards),
                            14.0,
                        ));
                    }
                }
                _ => {
                    ui.label(style::number(rewards.to_string(), 20.0));
                }
            }
        });
        card(ui, "GPU", |ui, _| {
            ui.label(style::number(gpu_glance(board), 20.0));
        });
        card(ui, "Intensity", |ui, title| {
            intensity(ui, title, board, link)
        });
    });
}

/// The intensity slider: a change is sent when the slider is let go.
fn intensity(ui: &mut egui::Ui, title: egui::Id, board: &Board, link: &Link) {
    let id = egui::Id::unique("pickaxe.intensity");
    let held: Option<u8> = ui.data(|data| data.get_temp(id));
    let mut value = held.unwrap_or(board.intensity);
    let slider = ui
        .add_enabled(
            board.live && !board.stopping,
            egui::Slider::new(&mut value, 10..=100).suffix("%"),
        )
        .labelled_by(title);
    if slider.changed() || slider.dragged() {
        ui.data_mut(|data| data.insert_temp(id, value));
    }
    if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
        link.send(Control::Intensity(value));
    }
    if held == Some(board.intensity) && !slider.dragged() {
        ui.data_mut(|data| data.remove::<u8>(id));
    }
}

fn tokens(ui: &mut egui::Ui, board: &Board, link: &Link) {
    let split = board.tokens.iter().any(|token| token.share.is_some());
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::Grid::new("tokens")
                .num_columns(if split { 7 } else { 6 })
                .spacing([24.0, 8.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Token");
                    if split {
                        ui.strong("Share");
                    }
                    ui.strong("Hash rate");
                    ui.strong("Rewards");
                    ui.strong("Donation");
                    ui.strong("State");
                    ui.strong("");
                    ui.end_row();
                    for (index, token) in board.tokens.iter().enumerate() {
                        ui.strong(&token.name);
                        if split {
                            ui.horizontal(|ui| {
                                let can = board.live && !board.stopping;
                                if ui
                                    .add_enabled(can, egui::Button::new("−"))
                                    .accessible_name(format!("Lower the {} share", token.name))
                                    .clicked()
                                {
                                    link.send(Control::Share { index, up: false });
                                }
                                ui.monospace(format!("{:>3}%", token.share.unwrap_or(0)));
                                if ui
                                    .add_enabled(can, egui::Button::new("+"))
                                    .accessible_name(format!("Raise the {} share", token.name))
                                    .clicked()
                                {
                                    link.send(Control::Share { index, up: true });
                                }
                            });
                        }
                        ui.monospace(format_hash_rate(token.rate));
                        ui.monospace(token.rewards.to_string());
                        let can = board.live && !board.stopping;
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(can, egui::Button::new("−"))
                                .accessible_name(format!("Lower the {} donation", token.name))
                                .clicked()
                            {
                                link.send(Control::Donation { index, up: false });
                            }
                            ui.monospace(
                                token
                                    .donation
                                    .map_or_else(|| "reading".into(), |d| d.to_string()),
                            );
                            if ui
                                .add_enabled(can, egui::Button::new("+"))
                                .accessible_name(format!("Raise the {} donation", token.name))
                                .clicked()
                            {
                                link.send(Control::Donation { index, up: true });
                            }
                        });
                        ui.label(&token.state);
                        if token.runtime
                            && ui
                                .add_enabled(can, egui::Button::new("Reconnect"))
                                .on_hover_text("Reconnect this token's server")
                                .clicked()
                        {
                            link.send(Control::Reconnect { index });
                        } else if !token.runtime {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            if !board.layout.is_empty() {
                ui.add_space(4.0);
                ui.weak(&board.layout);
            }
            notice_here(ui, board, NoticeFor::Tokens);
        });
}

/// Each token's hash rate over the last ten minutes, one lane per token on
/// its own scale (tokens count different work).
fn chart(ui: &mut egui::Ui, board: &Board) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Last 10 minutes");
            if board.history.is_empty() {
                ui.weak("The hash rate shows here once the GPUs report it.");
                return;
            }
            for (name, samples) in &board.history {
                let index = board
                    .tokens
                    .iter()
                    .position(|token| token.name == *name)
                    .unwrap_or(0);
                lane(ui, name, samples, LINE_COLORS[index % LINE_COLORS.len()]);
            }
        });
}

fn lane(
    ui: &mut egui::Ui,
    name: &str,
    samples: &std::collections::VecDeque<f64>,
    color: egui::Color32,
) {
    const LABEL: f32 = 18.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 78.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let peak = samples.iter().copied().fold(0.0_f64, f64::max);
    painter.text(
        rect.left_top(),
        egui::Align2::LEFT_TOP,
        format!(
            "{name}   now {}   peak {}",
            format_hash_rate(samples.back().copied().unwrap_or(0.0)),
            format_hash_rate(peak)
        ),
        egui::FontId::proportional(12.0),
        visuals.weak_text_color(),
    );
    let plot = egui::Rect::from_min_max(rect.left_top() + egui::vec2(0.0, LABEL), rect.max);
    painter.rect_filled(plot, 4.0, visuals.extreme_bg_color);
    let inner = plot.shrink(6.0);
    if peak <= 0.0 || samples.len() < 2 {
        return;
    }
    let step = inner.width() / (HISTORY_CAP - 1) as f32;
    let start = HISTORY_CAP - samples.len();
    let points: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, rate)| {
            egui::pos2(
                inner.left() + (start + i) as f32 * step,
                inner.bottom() - (rate / peak) as f32 * inner.height(),
            )
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
}

fn reading(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(|| "–".into(), |value| format!("{value:.0}{unit}"))
}

fn telemetry_cells(ui: &mut egui::Ui, telemetry: &GpuTelemetry) {
    ui.monospace(reading(telemetry.temperature_c, " °C"));
    ui.monospace(reading(telemetry.power_watts, " W"));
    ui.monospace(reading(telemetry.gpu_utilization_percent, "%"));
    ui.monospace(reading(telemetry.fan_percent, "%"));
}

fn gpus(ui: &mut egui::Ui, board: &Board) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("GPUs");
            ui.add_space(4.0);
            egui::Grid::new("gpus")
                .num_columns(6)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    for heading in ["", "Rate", "Temp", "Power", "Load", "Fan"] {
                        ui.weak(heading);
                    }
                    ui.end_row();
                    // ELECTRIC alone reports its GPUs together; a coordinator
                    // whose rigs mine has none here.
                    if board.gpus.is_empty() && board.telemetry.samples > 0 {
                        ui.label("All GPUs");
                        ui.monospace("–");
                        telemetry_cells(ui, &board.telemetry);
                        ui.end_row();
                    } else if board.gpus.is_empty() {
                        ui.weak("No GPU mines on this computer");
                        ui.end_row();
                    }
                    for gpu in &board.gpus {
                        ui.label(&gpu.name);
                        ui.monospace(gpu.rate.map_or_else(|| "–".into(), format_hash_rate));
                        telemetry_cells(ui, &gpu.telemetry);
                        ui.end_row();
                    }
                });
        });
}

/// The hottest GPU and the power of all of them, or a dash before any reports.
fn gpu_glance(board: &Board) -> String {
    let readings: Vec<&GpuTelemetry> = if board.gpus.is_empty() {
        vec![&board.telemetry]
    } else {
        board.gpus.iter().map(|gpu| &gpu.telemetry).collect()
    };
    let hottest = readings
        .iter()
        .filter_map(|t| t.temperature_c)
        .fold(None, |most: Option<f64>, t| {
            Some(most.map_or(t, |m| m.max(t)))
        });
    let watts: f64 = readings.iter().filter_map(|t| t.power_watts).sum();
    match (hottest, watts > 0.0) {
        (Some(t), true) => format!("{t:.0} °C · {watts:.0} W"),
        (Some(t), false) => format!("{t:.0} °C"),
        (None, true) => format!("{watts:.0} W"),
        (None, false) => "–".into(),
    }
}

/// The BCH height the session last read.
fn height(board: &Board) -> Option<u32> {
    board
        .runtime
        .as_ref()
        .map(|snapshot| snapshot.height)
        .filter(|height| *height > 0)
        .or_else(|| board.electric.as_ref().and_then(|electric| electric.height))
}

fn chain(ui: &mut egui::Ui, board: &Board) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Network");
            ui.add_space(4.0);
            egui::Grid::new("chain")
                .num_columns(2)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    if let Some(height) = height(board) {
                        ui.weak("BCH height");
                        ui.monospace(height.to_string());
                        ui.end_row();
                    }
                    if let Some(snapshot) = &board.runtime {
                        ui.weak("Source");
                        ui.label(snapshot.job_source.label());
                        ui.end_row();
                        if !snapshot.endpoint.is_empty() {
                            ui.weak("Server");
                            ui.label(crate::tui::redact_endpoint(&snapshot.endpoint));
                            ui.end_row();
                        }
                        if !snapshot.baton_txid.is_empty() {
                            ui.weak("Baton");
                            ui.monospace(format!(
                                "{}…:{}",
                                &snapshot.baton_txid[..snapshot.baton_txid.len().min(16)],
                                snapshot.baton_vout
                            ));
                            ui.end_row();
                        }
                        ui.weak("Pending");
                        ui.label(snapshot.pending_winners.to_string());
                        ui.end_row();
                        ui.weak("Reconnects");
                        ui.label(snapshot.reconnects.to_string());
                        ui.end_row();
                    }
                    if let Some(electric) = &board.electric {
                        if let Some(endpoint) = &electric.endpoint {
                            ui.weak("ELECTRIC server");
                            ui.label(crate::tui::redact_endpoint(endpoint));
                            ui.end_row();
                        }
                        if let Some(difficulty) = electric.difficulty {
                            ui.weak("ELECTRIC difficulty");
                            ui.label(difficulty.to_string());
                            ui.end_row();
                        }
                        if let Some(fee) = electric.claim_fee_estimate {
                            ui.weak("Claim fee");
                            ui.label(format!("about {fee} sats"));
                            ui.end_row();
                        }
                    }
                });
        });
}

/// A one-line text field kept across frames; returns the text when its
/// button is pressed or Enter is pressed in it. The text stays until
/// `applied` says the session took it, so a refused value can be fixed.
fn apply_line(
    ui: &mut egui::Ui,
    label: &str,
    key: &str,
    hint: &str,
    button: &str,
    applied: impl Fn(&str) -> bool,
) -> Option<String> {
    let id = egui::Id::unique(key);
    let mut value: String = ui.data(|data| data.get_temp(id)).unwrap_or_default();
    if !value.is_empty() && applied(value.trim()) {
        value.clear();
    }
    let mut apply = None;
    ui.horizontal_wrapped(|ui| {
        let label = ui.add_sized([110.0, 18.0], egui::Label::new(label));
        let width = (ui.available_width() - 150.0).clamp(160.0, 420.0);
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut value)
                    .hint_text(hint)
                    .desired_width(width),
            )
            .labelled_by(label.id);
        let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if ui.button(button).clicked() || entered {
            apply = Some(value.trim().to_owned());
        }
    });
    ui.data_mut(|data| data.insert_temp(id, value));
    apply.filter(|value| !value.is_empty())
}

/// PHOTON alone: its payout and Fulcrum server change while it mines, as
/// its terminal dashboard's `address` and `endpoint` commands do.
fn settings(ui: &mut egui::Ui, board: &Board, link: &Link) {
    let Some(snapshot) = &board.runtime else {
        return;
    };
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Settings");
            ui.add_space(4.0);
            notice_here(ui, board, NoticeFor::Settings);
            ui.horizontal(|ui| {
                ui.weak("Payout");
                ui.monospace(&snapshot.payout_address);
            });
            if let Some(address) = apply_line(
                ui,
                "New payout",
                "pickaxe.payout",
                "bitcoincash:… or bchtest:…",
                "Change payout",
                |value| value == snapshot.payout_address,
            ) {
                link.send(Control::SetPayout(address));
            }
            ui.add_space(6.0);
            if let Some(url) = apply_line(
                ui,
                "Fulcrum server",
                "pickaxe.fulcrum",
                "wss://host:port",
                "Use this server",
                |value| value == snapshot.endpoint,
            ) {
                link.send(Control::SetFulcrum(Some(url)));
            }
            if ui
                .button("Automatic servers")
                .on_hover_text("Clear the custom Fulcrum server and use the built-in list")
                .clicked()
            {
                link.send(Control::SetFulcrum(None));
            }
        });
}

/// The rigs this miner coordinates, and the command each rig runs to join.
fn rigs_panel(ui: &mut egui::Ui, rigs: &crate::rigs::RigSummary, join: &[(String, String)]) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Rigs");
            ui.add_space(4.0);
            let mut line = format!(
                "{} connected · {} GPUs · {} · {} winners · {} rejected",
                rigs.connected,
                rigs.gpus,
                format_hash_rate(rigs.rate),
                rigs.winners,
                rigs.rejected
            );
            if rigs.public {
                line.push_str(&format!(
                    " · Public Solo Endpoint, fee {:.2}%",
                    f64::from(rigs.fee_bps.unwrap_or(0)) / 100.0
                ));
            }
            ui.label(line);
            if !rigs.rigs.is_empty() {
                ui.add_space(4.0);
                egui::Grid::new("rigs")
                    .num_columns(5)
                    .spacing([14.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for heading in ["Rig", "GPUs", "Rate", "Winners", "Version"] {
                            ui.weak(heading);
                        }
                        ui.end_row();
                        for rig in &rigs.rigs {
                            ui.label(&rig.name);
                            ui.monospace(rig.gpus.to_string());
                            ui.monospace(format_hash_rate(rig.rate));
                            ui.monospace(rig.winners.to_string());
                            ui.label(&rig.version);
                            ui.end_row();
                            for gpu in rig.devices.iter().filter(|gpu| gpu.error.is_some()) {
                                ui.label(format!("  {}", gpu.name));
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    gpu.error.as_deref().unwrap_or(""),
                                );
                                ui.end_row();
                            }
                        }
                    });
            }
            ui.add_space(6.0);
            ui.weak("Rigs join with:");
            for (index, (place, command)) in join.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.weak(place);
                    ui.monospace(command);
                    if ui
                        .small_button("Copy")
                        .accessible_name(format!("Copy join line {}", index + 1))
                        .clicked()
                    {
                        ui.ctx().copy_text(command.clone());
                    }
                });
            }
        });
}

/// ELECTRIC's fee pot: its address to fund, as text and a QR code, and for
/// an owner-controlled pot its Max/send.
fn pot(ui: &mut egui::Ui, board: &Board, link: &Link) {
    let Some(electric) = &board.electric else {
        return;
    };
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("ELECTRIC fee pot");
            let Some(address) = &electric.pot_address else {
                ui.label("Preparing the fee pot…");
                return;
            };
            ui.horizontal(|ui| {
                qr(ui, address, 4.0);
                ui.vertical(|ui| {
                    ui.label("Fund it with plain BCH; it pays each claim's transaction fee.");
                    ui.horizontal(|ui| {
                        ui.monospace(address);
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(address.clone());
                        }
                    });
                    match electric.pot {
                        Some((sats, claims)) => {
                            ui.label(format!("{sats} sats · pays about {claims} claims"));
                        }
                        None => {
                            ui.weak("Reading the pot's balance…");
                        }
                    }
                    if let Some(key) = &electric.pot_key {
                        ui.weak(format!("Recovery key file: {key}. Back it up."));
                    }
                });
            });
            if electric.withdrawal.enabled {
                ui.add_space(8.0);
                max_send(ui, &electric.withdrawal, board, link);
            }
        });
}

/// Max/send from an owner-controlled pot, step by step as the terminal's
/// pot view: enter a destination, preview the quote, then send exactly the
/// quote shown. The session refuses a send whose quote was not drawn.
fn max_send(
    ui: &mut egui::Ui,
    send: &crate::electric::withdrawal::View,
    board: &Board,
    link: &Link,
) {
    use super::session::Withdraw;
    use crate::electric::withdrawal::Phase;
    let network = board
        .network
        .map_or("this network", |network| network.as_str());
    ui.strong(format!(
        "Max/send · {network} · plain BCH from this fee pot"
    ));
    let id = egui::Id::unique("pickaxe.max-send");
    notice_here(ui, board, NoticeFor::Pot);
    match send.phase {
        Phase::Idle | Phase::Complete | Phase::Error => {
            if !send.message.is_empty() {
                ui.label(&send.message);
            }
            if ui
                .button("Max/send…")
                .on_hover_text("Send all of this pot's plain BCH to an address you choose")
                .clicked()
            {
                ui.data_mut(|data| data.remove::<String>(id));
                link.send(Control::Withdraw(Withdraw::Edit));
            }
        }
        Phase::Editing => {
            let mut destination: String = ui.data(|data| data.get_temp(id)).unwrap_or_default();
            let mut prepare = false;
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Destination");
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut destination)
                            .hint_text("a BCH address")
                            .desired_width((ui.available_width() - 160.0).clamp(160.0, 420.0)),
                    )
                    .labelled_by(label.id);
                prepare =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                prepare |= ui.button("Preview").clicked();
                if ui.button("Cancel").clicked() {
                    link.send(Control::Withdraw(Withdraw::Cancel));
                }
            });
            ui.weak("Type or paste a BCH address, then Preview shows what it receives.");
            if prepare && !destination.trim().is_empty() {
                link.send(Control::Withdraw(Withdraw::Prepare(
                    destination.trim().into(),
                )));
            }
            ui.data_mut(|data| data.insert_temp(id, destination));
        }
        Phase::Preparing => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Finishing any pending claim; preparing Max/send…");
            });
        }
        Phase::Preview => {
            ui.monospace(format!("To: {}", send.destination));
            ui.label(format!("Receive: {} sats", send.amount));
            ui.label(format!(
                "Total fee: {} sats · {} transaction(s)",
                send.fee, send.count
            ));
            if !send.message.is_empty() {
                ui.label(&send.message);
            }
            // This exact quote is now on screen.
            link.send(Control::Withdraw(Withdraw::Presented(send.clone())));
            let asking = egui::Id::unique("pickaxe.max-send.confirm");
            let mut open = ui.data(|data| data.get_temp(asking).unwrap_or(false));
            ui.horizontal(|ui| {
                if ui.button(format!("Send {} sats", send.amount)).clicked() {
                    open = true;
                }
                if ui.button("Cancel").clicked() {
                    link.send(Control::Withdraw(Withdraw::Cancel));
                }
            });
            if style::confirm(
                ui,
                &mut open,
                "pickaxe.max-send.dialog",
                &format!("Send {} sats?", send.amount),
                &format!(
                    "All of this pot's plain BCH goes to {}, {} sats in fees. A sent \
                     transaction cannot be undone.",
                    send.destination, send.fee
                ),
                "Send",
            ) == Some(true)
            {
                link.send(Control::Withdraw(Withdraw::Confirm));
            }
            ui.data_mut(|data| data.insert_temp(asking, open));
        }
        Phase::Sending => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if send.message.is_empty() {
                    "Sending…"
                } else {
                    send.message.as_str()
                });
            });
        }
    }
}

/// `text` as a QR code drawn in dark modules of `module` points on white,
/// with a quiet zone.
pub(super) fn qr(ui: &mut egui::Ui, text: &str, module: f32) {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
    else {
        return;
    };
    const QUIET: usize = 4;
    let width = code.width();
    let colors = code.to_colors();
    let side = (width + 2 * QUIET) as f32 * module;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                let corner = rect.left_top()
                    + egui::vec2((x + QUIET) as f32 * module, (y + QUIET) as f32 * module);
                painter.rect_filled(
                    egui::Rect::from_min_size(corner, egui::vec2(module, module)),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
}

fn events(ui: &mut egui::Ui, board: &Board) {
    ui.add_space(4.0);
    ui.strong("Events");
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for event in &board.events {
                ui.monospace(event);
            }
        });
}

/// Hours, minutes and seconds, as the terminal dashboard shows uptime.
pub(super) fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_as_hours_minutes_seconds() {
        assert_eq!(
            clock(Duration::from_secs(3 * 3600 + 7 * 60 + 9)),
            "03:07:09"
        );
    }

    #[test]
    fn the_state_line_says_why_a_stop_waits() {
        let mut board = Board {
            live: true,
            stopping: true,
            pending: true,
            ..Board::default()
        };
        assert!(state_text(&board).contains("pending reward"));
        board.pending = false;
        assert_eq!(state_text(&board), "Stopping…");
        board.live = false;
        board.error = Some("no GPU".into());
        assert_eq!(state_text(&board), "Stopped: no GPU");
    }
}
