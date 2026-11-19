//! The desktop window (`pickaxe gui`): the mining setup and dashboard drawn
//! with egui.
//!
//! The setup is the terminal's own (`tui::SetupDriver`): the same steps,
//! profiles, choices and checks. Sessions run exactly as they do from the
//! terminal (`launch`): same reads, claims, fee pots, donation work and
//! saved settings. The window takes the terminal dashboard's place through
//! `tui::SessionView` and `rigs::RigView`, so it only reads and steers a
//! running session.
use crate::backend::{BackendKind, DeviceSelection};
use crate::config::RuntimeConfig;
use crate::tui::SetupResult;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

/// The BCH ASIC server's dashboard.
#[cfg(feature = "stratum-v2")]
mod asic;
mod dashboard;
mod launch;
#[cfg(all(test, feature = "gui-preview"))]
mod preview;
mod rig;
mod session;
mod setup;

/// What the window starts from: the saved settings and the command line, as
/// the terminal setup reads them, and where they are saved.
pub struct Start {
    pub cfg: RuntimeConfig,
    pub config_path: PathBuf,
    pub backend: BackendKind,
    pub selection: DeviceSelection,
    /// Start mining as soon as the GPUs are found (`pickaxe gui --start`).
    pub start: bool,
    /// The command line named its own ZMQ block notices (`--node-zmq`),
    /// which then win over a profile's in the first session.
    pub cli_node_zmq: bool,
    /// What the command line changes in a saved profile the first setup
    /// opens, as in the terminal (`--network`, `--token`, `--intensity`,
    /// `--fulcrum`, `--node-rpc`, `--source`). Its GPUs are left out: the
    /// window finds them.
    pub overrides: crate::tui::SetupOverrides,
    /// The command line names the GPUs (`--backend`, `--device`,
    /// `--include-integrated`), so they win over a profile's too.
    pub cli_gpus: bool,
}

/// Opens the window and returns when it closes. A session still mining
/// when the window is closed finishes its pending rewards and stops first.
pub fn run(start: Start) -> Result<(), String> {
    let link = Arc::new(session::Link::default());
    crate::tui::show_sessions_in(Box::new(session::WindowView(Arc::clone(&link))))?;
    let rigs = Arc::new(rig::RigLink::default());
    crate::rigs::show_rigs_in(Box::new(rig::WindowRigView(Arc::clone(&rigs))))?;
    #[cfg(feature = "stratum-v2")]
    let servers = Arc::new(asic::AsicLink::default());
    #[cfg(feature = "stratum-v2")]
    crate::stratum_v2::view::show_servers_in(Box::new(asic::WindowServerView(Arc::clone(
        &servers,
    ))))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pickaxe")
            .with_app_id("pickaxe")
            .with_inner_size([1040.0, 760.0])
            .with_min_inner_size([640.0, 480.0]),
        wgpu_options: window_renderer(),
        ..Default::default()
    };
    let shown = eframe::run_native(
        "Pickaxe",
        options,
        Box::new(move |cc| {
            release_own_console();
            cc.egui_ctx.set_fonts(fonts());
            link.set_context(cc.egui_ctx.clone());
            rigs.set_context(cc.egui_ctx.clone());
            #[cfg(feature = "stratum-v2")]
            servers.set_context(cc.egui_ctx.clone());
            Ok(Box::new(App::new(
                start,
                link,
                rigs,
                #[cfg(feature = "stratum-v2")]
                servers,
            )))
        }),
    )
    .map_err(|error| format!("open the window: {error}"));
    // The window never hands profiles back to the terminal.
    crate::session_control::take_profiles_request();
    shown
}

/// A console Windows opened only for this process (a double-click or a
/// shortcut) is released once the window is up, so no empty terminal sits
/// behind it. A console the miner was started from stays.
fn release_own_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
        let mut processes = [0u32; 2];
        // SAFETY: the buffer holds `processes.len()` ids, as passed.
        let attached = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), 2) };
        if attached == 1 {
            // SAFETY: detaching from the console has no preconditions;
            // later writes to stdout and stderr are discarded.
            if unsafe { FreeConsole() } != 0 {
                // Programs started from now on open no console of their own.
                crate::child_process::console_released();
            }
        }
    }
}

/// egui's bundled fonts, with the monospace font (Hack) last in the
/// proportional chain: it has the arrows, blocks and box lines the
/// terminal's text uses (← →, █ ░, ▾ ▸), which the proportional font lacks.
pub(crate) fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(chain) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        chain.push("Hack".to_owned());
    }
    fonts
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
    /// A session runs, on the screen `launch::screen_for` chose for it.
    Mining(launch::Screen),
}

struct App {
    start: Start,
    link: Arc<session::Link>,
    rigs: Arc<rig::RigLink>,
    #[cfg(feature = "stratum-v2")]
    servers: Arc<asic::AsicLink>,
    setup: setup::Setup,
    screen: Screen,
    worker: Option<JoinHandle<Result<(), String>>>,
    /// The window was asked to close while a session ran.
    closing: bool,
    /// The window's own stop flag, bound while no session has bound its
    /// own: Ctrl+C in the setup closes the window instead of stopping the
    /// next session, and a stop before a session binds is not lost.
    interrupt: Arc<std::sync::atomic::AtomicBool>,
    /// A stop was asked for before the session could take it; it is asked
    /// again every frame until the session ends.
    stop_asked: bool,
}

impl App {
    fn new(
        start: Start,
        link: Arc<session::Link>,
        rigs: Arc<rig::RigLink>,
        #[cfg(feature = "stratum-v2")] servers: Arc<asic::AsicLink>,
    ) -> Self {
        let setup = setup::Setup::new(setup::Inputs {
            cfg: start.cfg.clone(),
            config_path: start.config_path.clone(),
            backend: start.backend,
            selection: start.selection.clone(),
            overrides: start.overrides.clone(),
            cli_gpus: start.cli_gpus,
        });
        let interrupt = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Err(error) = crate::session_control::install_stop(&interrupt) {
            crate::tui::append_tui_log(&format!("event window {error}"));
        }
        Self {
            start,
            link,
            rigs,
            #[cfg(feature = "stratum-v2")]
            servers,
            setup,
            screen: Screen::Setup,
            worker: None,
            closing: false,
            interrupt,
            stop_asked: false,
        }
    }

    /// Runs what the setup chose on a thread of its own, as `pickaxe mine`
    /// runs it after its setup, and shows it.
    fn begin(&mut self, result: Box<SetupResult>) {
        let screen = launch::screen_for(&result);
        self.link.reset();
        self.rigs.reset();
        #[cfg(feature = "stratum-v2")]
        self.servers.reset();
        let config_path = self.start.config_path.clone();
        // The command line's block notices serve the first session only:
        // the setup after it is made without the command line, as the
        // terminal's return to its profiles is.
        let cli_node_zmq = std::mem::take(&mut self.start.cli_node_zmq);
        let link = Arc::clone(&self.link);
        #[cfg(feature = "stratum-v2")]
        let servers = Arc::clone(&self.servers);
        self.interrupt
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.stop_asked = false;
        self.worker = Some(std::thread::spawn(move || {
            // The request to save on stop never outlives the session, even
            // if it panics, so nothing later mistakes it for "profiles".
            struct Unrequest;
            impl Drop for Unrequest {
                fn drop(&mut self) {
                    crate::session_control::take_profiles_request();
                }
            }
            let _unrequest = Unrequest;
            let mined = launch::run(&result, &config_path, cli_node_zmq);
            let error = mined.as_ref().err().cloned();
            #[cfg(feature = "stratum-v2")]
            servers.finished(error.clone());
            link.finished(error);
            mined
        }));
        self.screen = Screen::Mining(screen);
    }

    fn ended(&mut self, done: setup::Done, ctx: &egui::Context) {
        match done {
            setup::Done::Start(result) => self.begin(result),
            setup::Done::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// Stops the session shown: tokens settle a pending reward first; the
    /// ASIC server stops through its own control; rig mode stops as Ctrl+C
    /// stops it. A token session still starting (not yet showing) is
    /// stopped as Ctrl+C would, since nothing reads its controls yet.
    fn stop(&self) {
        match self.screen {
            Screen::Mining(launch::Screen::Tokens) if !self.link.board().live => {
                crate::session_control::stop_session()
            }
            Screen::Mining(launch::Screen::Tokens) => {
                self.link.send(session::Control::Stop { now: false })
            }
            #[cfg(feature = "stratum-v2")]
            Screen::Mining(launch::Screen::Asic) => self
                .servers
                .send(crate::stratum_v2::view::ServerControl::Stop),
            Screen::Mining(_) | Screen::Setup => crate::session_control::stop_session(),
        }
    }

    /// A close while a session runs waits for it to stop as Stop does; the
    /// request is repeated until the session has bound its controls, then
    /// the window closes. Runs from `logic`, so a minimized window too.
    fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().close_requested()) && self.worker.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
        }
        if self
            .interrupt
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            // Ctrl+C with no session closes the window; a stop that reached
            // the window's flag before the session bound its own is kept.
            if self.worker.is_some() {
                self.stop_asked = true;
            } else {
                self.closing = true;
            }
        }
        if self.worker.is_some() {
            if self.closing || self.stop_asked {
                self.stop();
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        } else if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Back to the setup once the session's worker has returned, made again
    /// from what is saved, as the terminal returns to its profiles.
    fn collect(&mut self) {
        if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        let outcome = match self.worker.take().map(JoinHandle::join) {
            Some(Ok(outcome)) => outcome,
            Some(Err(_)) => Err("the mining session stopped unexpectedly".into()),
            None => Ok(()),
        };
        self.setup.reload(
            outcome
                .err()
                .map(|error| format!("Mining stopped: {error}")),
        );
        self.screen = Screen::Setup;
        self.stop_asked = false;
        // The setup takes Ctrl+C again.
        self.interrupt
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = crate::session_control::install_stop(&self.interrupt);
    }
}

impl Drop for App {
    /// The window can close without the app seeing the request (an error
    /// in the window system, the process asked to exit): the session is
    /// stopped and its worker waited for, so claims and saves finish.
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        while !worker.is_finished() {
            self.stop();
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let _ = worker.join();
    }
}

impl eframe::App for App {
    // Also runs while the window is hidden or not drawn (minimized, a locked
    // screen), so a start and a finished session never wait for a frame.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.collect();
        self.handle_close(ctx);
        if self.worker.is_some() {
            return;
        }
        if self.start.start && self.setup.start_now() {
            self.start.start = false;
        }
        if let Some(done) = self.setup.poll() {
            self.ended(done, ctx);
        }
        if self.start.start || self.setup.waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let live = self.worker.is_some();
        match self.screen {
            Screen::Setup => {
                if let Some(done) = self.setup.show(ui) {
                    self.ended(done, &ctx);
                }
            }
            Screen::Mining(launch::Screen::Tokens) => {
                dashboard::show(ui, &self.link.board(), &self.link)
            }
            Screen::Mining(launch::Screen::Rig) => rig::show(
                ui,
                &self.rigs.board(),
                live,
                &crate::session_control::stop_session,
            ),
            #[cfg(feature = "stratum-v2")]
            Screen::Mining(launch::Screen::Asic) => {
                asic::show(ui, &self.servers.board(), &self.servers)
            }
            #[cfg(not(feature = "stratum-v2"))]
            Screen::Mining(launch::Screen::Asic) => without_asic_server(ui),
        }
        if live {
            // The worker also asks for a frame whenever the session moves.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

/// A build without Stratum V2 has no ASIC server; its start fails at once
/// and the window returns to the setup with the reason.
#[cfg(not(feature = "stratum-v2"))]
fn without_asic_server(ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.label("This build does not include the BCH ASIC server (Stratum V2).");
    });
}
