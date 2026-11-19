//! Rig mode in the window: this computer's GPUs mining for a coordinator
//! (Join a GPU pool or farm). Rig mode has no dashboard of its own; the
//! window shows the status its status file holds and the events it prints.
use crate::telemetry::format_hash_rate;
use eframe::egui;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const EVENT_CAP: usize = 200;

#[derive(Debug, Clone, Default)]
pub struct RigBoard {
    /// The latest status (`rigs::rig_status_json`).
    pub status: Option<Value>,
    pub events: VecDeque<String>,
    pub started: Option<Instant>,
}

impl RigBoard {
    fn push_event(&mut self, line: String) {
        if self.events.len() == EVENT_CAP {
            self.events.pop_front();
        }
        self.events.push_back(line);
    }
}

#[derive(Default)]
pub struct RigLink {
    board: Mutex<RigBoard>,
    context: OnceLock<egui::Context>,
}

impl RigLink {
    pub fn set_context(&self, context: egui::Context) {
        let _ = self.context.set(context);
    }

    pub fn board(&self) -> RigBoard {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn reset(&self) {
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = RigBoard {
            started: Some(Instant::now()),
            ..RigBoard::default()
        };
    }

    fn update(&self, change: impl FnOnce(&mut RigBoard)) {
        change(&mut self.board.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(context) = self.context.get() {
            context.request_repaint();
        }
    }
}

/// Receives rig mode's events and status (see `rigs::RigView`).
pub struct WindowRigView(pub Arc<RigLink>);

impl crate::rigs::RigView for WindowRigView {
    fn event(&self, event: &str, detail: &Value) {
        let line = match detail {
            Value::Object(map) if map.is_empty() => event.to_owned(),
            Value::Null => event.to_owned(),
            detail => format!("{event}: {detail}"),
        };
        crate::tui::append_tui_log(&format!("event rig {line}"));
        self.0.update(|board| board.push_event(line));
    }

    fn status(&self, status: &Value) {
        let status = status.clone();
        self.0.update(|board| board.status = Some(status));
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

/// The state line for a rig's status.
pub fn state_text(status: Option<&Value>) -> String {
    let Some(status) = status else {
        return "Connecting to the coordinator…".into();
    };
    let coordinator = text(status, "coordinator");
    let backup = status.get("backup").and_then(Value::as_bool) == Some(true);
    match text(status, "state") {
        "mining" if backup => format!("Mining for {coordinator} (backup coordinator)"),
        "mining" => format!("Mining for {coordinator}"),
        "paused" => format!("Paused by {coordinator}"),
        _ => "Waiting for a coordinator".into(),
    }
}

/// Draws rig mode; `stop` is called when Stop is pressed.
pub fn show(ui: &mut egui::Ui, board: &RigBoard, live: bool, stop: &dyn Fn()) {
    egui::Panel::top("rig-bar").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Pickaxe").size(20.0).strong());
            ui.label(egui::RichText::new("rig").monospace());
            ui.separator();
            // Stop holds until rig mode has stopped its GPUs.
            let stopping_id = egui::Id::unique("pickaxe.rig.stopping");
            let stopping = live && ui.data(|data| data.get_temp(stopping_id).unwrap_or(false));
            if !live {
                ui.data_mut(|data| data.remove::<bool>(stopping_id));
            }
            ui.label(if stopping {
                "Stopping…".into()
            } else if live {
                state_text(board.status.as_ref())
            } else {
                "Stopped".into()
            });
            if let Some(started) = board.started.filter(|_| live) {
                ui.weak(super::dashboard::clock(started.elapsed()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if live
                    && ui
                        .add_enabled(!stopping, egui::Button::new("Stop"))
                        .clicked()
                {
                    ui.data_mut(|data| data.insert_temp(stopping_id, true));
                    stop();
                }
            });
        });
        ui.add_space(4.0);
    });
    egui::Panel::bottom("rig-events")
        .resizable(true)
        .default_size(170.0)
        .show(ui, |ui| {
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
        });
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let status = board.status.as_ref();
            ui.horizontal_wrapped(|ui| {
                let rate = status
                    .and_then(|s| number(s, "current_rate"))
                    .unwrap_or(0.0);
                let winners = status
                    .and_then(|s| s.get("verified_winners"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                for (title, value) in [
                    ("Hash rate", format_hash_rate(rate)),
                    ("Winners sent", winners.to_string()),
                ] {
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_min_width(150.0);
                            ui.weak(title);
                            ui.heading(value);
                        });
                }
            });
            ui.add_space(10.0);
            ui.weak(
                "The coordinator chooses the jobs and claims the rewards; this computer \
                 mines its share and sends its winners back.",
            );
            ui.add_space(10.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.strong("GPUs");
                    ui.add_space(4.0);
                    let gpus = status
                        .and_then(|s| s.get("gpus"))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if gpus.is_empty() {
                        ui.weak("The GPUs show here once the rig reports them.");
                        return;
                    }
                    egui::Grid::new("rig-gpus")
                        .num_columns(7)
                        .spacing([14.0, 6.0])
                        .show(ui, |ui| {
                            for heading in ["", "Status", "Rate", "Winners", "Temp", "Power", "Fan"]
                            {
                                ui.weak(heading);
                            }
                            ui.end_row();
                            for gpu in &gpus {
                                let telemetry = gpu.get("gpu_telemetry").unwrap_or(&Value::Null);
                                let reading = |key: &str, unit: &str| {
                                    number(telemetry, key)
                                        .map_or_else(|| "–".into(), |v| format!("{v:.0}{unit}"))
                                };
                                ui.label(text(gpu, "name"));
                                ui.label(text(gpu, "status"));
                                ui.monospace(format_hash_rate(
                                    number(gpu, "active_rate").unwrap_or(0.0),
                                ));
                                ui.monospace(
                                    gpu.get("winners")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0)
                                        .to_string(),
                                );
                                ui.monospace(reading("temperature_c", " °C"));
                                ui.monospace(reading("power_watts", " W"));
                                ui.monospace(reading("fan_percent", "%"));
                                ui.end_row();
                                if let Some(error) = gpu.get("last_error").and_then(Value::as_str) {
                                    ui.label("");
                                    ui.colored_label(ui.visuals().error_fg_color, error);
                                    ui.end_row();
                                }
                            }
                        });
                });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rigs::RigView;

    #[test]
    fn the_state_line_follows_the_rig_status() {
        assert!(state_text(None).contains("Connecting"));
        let mining =
            crate::rigs::rig_status_json(Some(("192.0.2.1:3340", false)), true, 1.0, 0, 0, &[]);
        assert_eq!(state_text(Some(&mining)), "Mining for 192.0.2.1:3340");
        let backup =
            crate::rigs::rig_status_json(Some(("192.0.2.2:3340", true)), true, 1.0, 0, 0, &[]);
        assert!(state_text(Some(&backup)).contains("backup"));
        let paused =
            crate::rigs::rig_status_json(Some(("192.0.2.1:3340", false)), false, 0.0, 0, 0, &[]);
        assert!(state_text(Some(&paused)).starts_with("Paused"));
        let waiting = crate::rigs::rig_status_json(None, false, 0.0, 0, 0, &[]);
        assert_eq!(state_text(Some(&waiting)), "Waiting for a coordinator");
    }

    #[test]
    fn events_and_status_reach_the_board() {
        let link = Arc::new(RigLink::default());
        link.reset();
        let view = WindowRigView(Arc::clone(&link));
        view.event(
            "connected",
            &serde_json::json!({"coordinator": "192.0.2.1:3340"}),
        );
        view.event("waiting", &serde_json::json!({}));
        view.status(&crate::rigs::rig_status_json(None, false, 0.0, 0, 0, &[]));
        let board = link.board();
        assert_eq!(board.events.len(), 2);
        assert!(board.events[0].starts_with("connected: "));
        assert_eq!(board.events[1], "waiting");
        assert!(board.status.is_some());
    }
}
