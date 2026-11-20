//! The window's few shared looks: one accent (the theme's selection color)
//! for the main action and the current choice, red only for errors, plain
//! text for the rest; numbers in the monospace font so they don't shift;
//! a confirmation for what cannot be undone; the terminal's keys on hover.
use eframe::egui;

/// The one accent of every screen.
pub fn accent(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().selection.bg_fill
}

/// The screen's main action: the only filled button on it.
pub fn primary(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.into())
            .size(17.0)
            .strong()
            .color(ui.visuals().selection.stroke.color),
    )
    .fill(accent(ui))
    .min_size(egui::vec2(220.0, 38.0))
}

/// A changing number, in the monospace font so its digits don't move.
pub fn number(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text.into()).monospace().size(size)
}

/// A message next to the control it answers: red when it is an error.
pub fn answer(ui: &mut egui::Ui, text: &str, error: bool) {
    let text = egui::RichText::new(text);
    let text = if error {
        text.color(ui.visuals().error_fg_color)
    } else {
        text.color(ui.visuals().text_color())
    };
    ui.add(egui::Label::new(text).wrap());
}

/// The terminal's keys for a screen, shown when the pointer rests on
/// "Keyboard"; mouse users don't need them in the way.
pub fn keys(ui: &mut egui::Ui, keys: &str) {
    if keys.is_empty() {
        return;
    }
    ui.add(
        egui::Label::new(egui::RichText::new("Keyboard").weak().small())
            .sense(egui::Sense::hover()),
    )
    .on_hover_text(keys);
}

/// Asks before something that cannot be undone. Call it every frame while
/// `open` is set; it returns `Some(true)` when confirmed and `Some(false)`
/// when kept, and clears `open` either way.
pub fn confirm(
    ui: &mut egui::Ui,
    open: &mut bool,
    id: &str,
    title: &str,
    body: &str,
    yes: &str,
) -> Option<bool> {
    if !*open {
        return None;
    }
    let mut answer = None;
    egui::Modal::new(egui::Id::unique(id)).show(ui.ctx(), |ui| {
        ui.set_max_width(420.0);
        ui.strong(title);
        ui.add_space(6.0);
        ui.add(egui::Label::new(body).wrap());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new(yes).color(ui.visuals().error_fg_color),
                ))
                .clicked()
            {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked()
                || ui.input(|input| input.key_pressed(egui::Key::Escape))
            {
                answer = Some(false);
            }
        });
    });
    if answer.is_some() {
        *open = false;
    }
    answer
}
