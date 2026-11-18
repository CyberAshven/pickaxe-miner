//! The desktop window (`pickaxe gui`): the mining setup and dashboard drawn
//! with egui.
//!
//! Sessions run exactly as they do from the terminal (`runtime`,
//! `hash_split`, `electric::miner`): same reads, claims, fee pots, donation
//! work and saved settings. The window takes the terminal dashboard's place
//! through `tui::SessionView`, so it only reads and steers a running session.
use crate::backend::{BackendKind, DeviceSelection};
use crate::config::RuntimeConfig;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

mod dashboard;
#[cfg(all(test, feature = "gui-preview"))]
mod preview;
mod session;
mod setup;

/// What the window starts from: the saved settings as the terminal setup
/// reads them, and where they are saved.
pub struct Start {
    pub cfg: RuntimeConfig,
    pub config_path: PathBuf,
    pub backend: BackendKind,
    pub selection: DeviceSelection,
    /// Start mining as soon as the GPUs are found (`pickaxe gui --start`).
    pub start: bool,
}

/// Opens the window and returns when it closes. A session still mining
/// when the window is closed finishes its pending rewards and stops first.
pub fn run(start: Start) -> Result<(), String> {
    let link = Arc::new(session::Link::default());
    crate::tui::show_sessions_in(Box::new(session::WindowView(Arc::clone(&link))))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pickaxe")
            .with_app_id("pickaxe")
            .with_inner_size([1040.0, 760.0])
            .with_min_inner_size([640.0, 480.0]),
        wgpu_options: window_renderer(),
        ..Default::default()
    };
    eframe::run_native(
        "Pickaxe",
        options,
        Box::new(move |cc| {
            link.set_context(cc.egui_ctx.clone());
            Ok(Box::new(App::new(start, link)))
        }),
    )
    .map_err(|error| format!("open the window: {error}"))
}

/// The window draws on the low-power GPU where a computer has two, so the
/// GPU that mines keeps its time; drawing takes little either way.
fn window_renderer() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
            power_preference: eframe::wgpu::PowerPreference::LowPower,
            ..WgpuSetupCreateNew::without_display_handle()
        }),
        ..Default::default()
    }
}

enum Screen {
    Setup,
    Mining,
}

struct App {
    start: Start,
    link: Arc<session::Link>,
    form: setup::Form,
    screen: Screen,
    worker: Option<JoinHandle<Result<(), String>>>,
    /// The window was asked to close while a session ran.
    closing: bool,
}

impl App {
    fn new(start: Start, link: Arc<session::Link>) -> Self {
        let form = setup::Form::new(&start.cfg, start.backend, &start.selection);
        Self {
            start,
            link,
            form,
            screen: Screen::Setup,
            worker: None,
            closing: false,
        }
    }

    fn begin(&mut self, launch: setup::Launch) {
        self.link.reset();
        // Mining from the window starts from what was entered here, so the
        // next start offers it again.
        self.start.cfg = launch.cfg.clone();
        let config_path = self.start.config_path.clone();
        let link = Arc::clone(&self.link);
        self.worker = Some(std::thread::spawn(move || {
            let mined = session::mine(&launch, &config_path);
            link.finished(mined.as_ref().err().cloned());
            mined
        }));
        self.screen = Screen::Mining;
    }

    /// Back to the setup once the session's worker has returned.
    fn collect(&mut self) {
        if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        let outcome = match self.worker.take().map(JoinHandle::join) {
            Some(Ok(outcome)) => outcome,
            Some(Err(_)) => Err("the mining session stopped unexpectedly".into()),
            None => Ok(()),
        };
        self.form.finished(outcome.err());
        self.screen = Screen::Setup;
    }
}

impl eframe::App for App {
    // Also runs while the window is hidden or not drawn (minimized, a locked
    // screen), so a start and a finished session never wait for a frame.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.collect();
        if self.start.start && self.worker.is_none() {
            match self.form.start_now(&self.start.cfg) {
                Some(Ok(launch)) => {
                    self.start.start = false;
                    self.begin(launch);
                }
                // The setup shows why; the miner chooses from there.
                Some(Err(())) => self.start.start = false,
                None => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|input| input.viewport().close_requested()) && self.worker.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.link.send(session::Control::Stop { now: false });
            self.closing = true;
        }
        if self.closing && self.worker.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        match self.screen {
            Screen::Setup => {
                if let Some(launch) = self.form.show(ui, &self.start.cfg) {
                    self.begin(launch);
                }
            }
            Screen::Mining => dashboard::show(ui, &self.link.board(), &self.link),
        }
        if self.worker.is_some() {
            // The worker also asks for a frame whenever the session moves.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}
