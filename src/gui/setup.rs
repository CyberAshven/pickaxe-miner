//! The window's mining setup: network, payout, token or hash split, GPUs
//! and intensity, checked with the same rules as the terminal setup.
use crate::backend::{BackendKind, DeviceSelection, GpuDevice};
use crate::config::{MiningNetwork, MiningToken, RuntimeConfig};
use crate::hash_split::{Shares, SplitToken};
use eframe::egui;
use std::sync::mpsc;

/// The session a start runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A token the PHOTON runtime mines, alone.
    Runtime(MiningToken),
    /// ELECTRIC alone, with its fee pot.
    Electric,
    Split(Shares),
}

/// A checked start: the settings, the GPUs and the session.
#[derive(Debug, Clone)]
pub struct Launch {
    pub cfg: RuntimeConfig,
    pub gpus: Vec<GpuDevice>,
    pub plan: Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Token(SplitToken),
    Split,
}

/// What was entered, apart from the GPUs.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub network: MiningNetwork,
    pub address: String,
    pub choice: Choice,
    /// The split's shares, one per token available on `network`.
    pub shares: Vec<(SplitToken, u8)>,
    pub intensity: u8,
}

impl Entry {
    pub fn from_config(cfg: &RuntimeConfig) -> Self {
        let mut entry = Self {
            network: cfg.network,
            address: cfg.payout_address.clone(),
            choice: Choice::Token(SplitToken::Gpu(cfg.token)),
            shares: Vec::new(),
            intensity: cfg.intensity,
        };
        entry.fit_network();
        entry
    }

    /// Keeps the token and shares to those `network` has.
    fn fit_network(&mut self) {
        let available = SplitToken::available(self.network);
        if let Choice::Token(token) = self.choice {
            if !available.contains(&token) {
                self.choice = available
                    .first()
                    .map_or(Choice::Split, |first| Choice::Token(*first));
            }
        }
        if self
            .shares
            .iter()
            .map(|(token, _)| *token)
            .ne(available.iter().copied())
        {
            self.shares = Shares::even(&available).iter().collect();
        }
    }

    pub fn set_network(&mut self, network: MiningNetwork) {
        self.network = network;
        self.fit_network();
    }

    /// The session these choices start on the chosen `gpus`, or why not.
    pub fn launch(
        &self,
        base: &RuntimeConfig,
        gpus: &[(GpuDevice, bool)],
    ) -> Result<Launch, String> {
        let mut cfg = base.clone();
        cfg.set_network(self.network);
        if self.address.trim().is_empty() {
            return Err("Enter the payout address your rewards go to.".into());
        }
        cfg.set_payout(self.address.trim().to_owned())?;
        cfg.set_intensity(self.intensity)?;
        let gpus: Vec<GpuDevice> = gpus
            .iter()
            .filter(|(_, chosen)| *chosen)
            .map(|(gpu, _)| gpu.clone())
            .collect();
        if gpus.is_empty() {
            return Err("Choose at least one GPU.".into());
        }
        let plan = match self.choice {
            Choice::Token(SplitToken::Gpu(token)) => {
                cfg.set_token(token.as_str())?;
                Plan::Runtime(token)
            }
            Choice::Token(SplitToken::Electric) => Plan::Electric,
            Choice::Split => {
                let pairs: Vec<String> = self
                    .shares
                    .iter()
                    .map(|(token, share)| format!("{}={share}", token.name()))
                    .collect();
                Plan::Split(Shares::parse(&pairs, self.network)?)
            }
        };
        cfg.ensure_mining_supported()?;
        Ok(Launch { cfg, gpus, plan })
    }
}

/// Every GPU mining could use, and whether the saved selection uses it.
type Found = Result<Vec<(GpuDevice, bool)>, String>;

enum Gpus {
    Finding(mpsc::Receiver<Found>),
    Found(Vec<(GpuDevice, bool)>),
    Failed(String),
}

fn find_gpus(backend: BackendKind, selection: DeviceSelection) -> Gpus {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let found =
            crate::backend::resolve_mining_devices(backend, &DeviceSelection::WithIntegrated).map(
                |every| {
                    let chosen = crate::backend::resolve_mining_devices(backend, &selection)
                        .unwrap_or_default();
                    every
                        .into_iter()
                        .map(|gpu| {
                            let used = chosen
                                .iter()
                                .any(|c| c.backend == gpu.backend && c.index == gpu.index);
                            (gpu, used)
                        })
                        .collect()
                },
            );
        let _ = send.send(found);
    });
    Gpus::Finding(receive)
}

pub struct Form {
    entry: Entry,
    gpus: Gpus,
    backend: BackendKind,
    selection: DeviceSelection,
    error: Option<String>,
    /// Why the last session stopped, if it failed.
    notice: Option<String>,
}

fn network_name(network: MiningNetwork) -> &'static str {
    match network {
        MiningNetwork::Mainnet => "Mainnet",
        MiningNetwork::Chipnet => "Chipnet (test network)",
    }
}

impl Form {
    pub fn new(cfg: &RuntimeConfig, backend: BackendKind, selection: &DeviceSelection) -> Self {
        Self {
            entry: Entry::from_config(cfg),
            gpus: find_gpus(backend, selection.clone()),
            backend,
            selection: selection.clone(),
            error: None,
            notice: None,
        }
    }

    /// A setup whose GPUs are already found (screen previews).
    #[cfg(all(test, feature = "gui-preview"))]
    pub fn found(entry: Entry, gpus: Vec<(GpuDevice, bool)>) -> Self {
        Self {
            entry,
            gpus: Gpus::Found(gpus),
            backend: BackendKind::Auto,
            selection: DeviceSelection::Default,
            error: None,
            notice: None,
        }
    }

    pub fn finished(&mut self, error: Option<String>) {
        self.notice = error.map(|error| format!("Mining stopped: {error}"));
    }

    fn poll_gpus(&mut self) {
        if let Gpus::Finding(receive) = &self.gpus {
            match receive.try_recv() {
                Ok(Ok(found)) => self.gpus = Gpus::Found(found),
                Ok(Err(error)) => self.gpus = Gpus::Failed(error),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.gpus = Gpus::Failed("looking for GPUs stopped".into())
                }
            }
        }
    }

    /// Starts as pressing Start mining would, once the GPUs are found:
    /// `None` while they are looked for, `Err` (shown on the setup) when
    /// the saved settings cannot start.
    pub fn start_now(&mut self, base: &RuntimeConfig) -> Option<Result<Launch, ()>> {
        self.poll_gpus();
        match &self.gpus {
            Gpus::Finding(_) => None,
            Gpus::Failed(_) => Some(Err(())),
            Gpus::Found(gpus) => Some(self.entry.launch(base, gpus).map_err(|error| {
                self.error = Some(error);
            })),
        }
    }

    /// Draws the setup; a checked start when Start mining is pressed.
    pub fn show(&mut self, ui: &mut egui::Ui, base: &RuntimeConfig) -> Option<Launch> {
        self.poll_gpus();
        if matches!(self.gpus, Gpus::Finding(_)) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        let mut launch = None;
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading(egui::RichText::new("Pickaxe").size(28.0).strong());
                ui.label("Mine CashTokens on your GPUs through proof of work.");
                ui.add_space(12.0);
                if let Some(notice) = &self.notice {
                    ui.colored_label(ui.visuals().error_fg_color, notice);
                    ui.add_space(8.0);
                }
                self.settings(ui);
                ui.add_space(12.0);
                self.gpu_list(ui);
                ui.add_space(16.0);
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    ui.add_space(6.0);
                }
                let ready = matches!(self.gpus, Gpus::Found(_));
                let start = ui.add_enabled(
                    ready,
                    egui::Button::new(egui::RichText::new("Start mining").size(18.0))
                        .min_size(egui::vec2(180.0, 40.0)),
                );
                if start.clicked() {
                    if let Gpus::Found(gpus) = &self.gpus {
                        match self.entry.launch(base, gpus) {
                            Ok(checked) => {
                                self.error = None;
                                self.notice = None;
                                launch = Some(checked);
                            }
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
            });
        });
        launch
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let entry = &mut self.entry;
        egui::Grid::new("setup")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                let label = ui.label("Network");
                let mut network = entry.network;
                egui::ComboBox::from_id_salt("network")
                    .selected_text(network_name(network))
                    .show_ui(ui, |ui| {
                        for choice in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
                            ui.selectable_value(&mut network, choice, network_name(choice));
                        }
                    })
                    .response
                    .labelled_by(label.id);
                if network != entry.network {
                    entry.set_network(network);
                }
                ui.end_row();

                let label = ui.label("Payout address");
                let hint = match entry.network {
                    MiningNetwork::Mainnet => "bitcoincash:…",
                    MiningNetwork::Chipnet => "bchtest:…",
                };
                ui.add(
                    egui::TextEdit::singleline(&mut entry.address)
                        .hint_text(hint)
                        .desired_width(460.0),
                )
                .labelled_by(label.id);
                ui.end_row();

                ui.label("Mine");
                ui.horizontal_wrapped(|ui| {
                    for token in SplitToken::available(entry.network) {
                        ui.radio_value(&mut entry.choice, Choice::Token(token), token.name());
                    }
                    if SplitToken::available(entry.network).len() > 1 {
                        ui.radio_value(&mut entry.choice, Choice::Split, "Hash split");
                    }
                });
                ui.end_row();

                if entry.choice == Choice::Split {
                    ui.label("Shares");
                    ui.vertical(|ui| {
                        for (token, share) in &mut entry.shares {
                            ui.horizontal(|ui| {
                                let label =
                                    ui.add_sized([90.0, 18.0], egui::Label::new(token.name()));
                                ui.add(egui::Slider::new(share, 0..=100).step_by(5.0).suffix("%"))
                                    .labelled_by(label.id);
                            });
                        }
                        let total: u32 = entry.shares.iter().map(|(_, s)| u32::from(*s)).sum();
                        ui.weak(format!(
                            "{total}% of GPU time in all; at most 100%, in 5% steps."
                        ));
                    });
                    ui.end_row();
                }

                let label = ui.label("Intensity");
                ui.add(egui::Slider::new(&mut entry.intensity, 10..=100).suffix("%"))
                    .labelled_by(label.id);
                ui.end_row();
            });
        if entry.choice == Choice::Token(SplitToken::Electric)
            || (entry.choice == Choice::Split
                && entry
                    .shares
                    .iter()
                    .any(|(token, share)| *token == SplitToken::Electric && *share > 0))
        {
            ui.add_space(6.0);
            ui.weak(
                "ELECTRIC pays each claim's fee from a pot you fund with plain BCH. \
                 Mining shows the pot's address and QR code; its recovery key is saved \
                 beside your settings.",
            );
        }
    }

    fn gpu_list(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("GPUs").strong());
        let mut retry = false;
        match &mut self.gpus {
            Gpus::Finding(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for GPUs…");
                });
            }
            Gpus::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                retry = ui.button("Look again").clicked();
            }
            Gpus::Found(gpus) => {
                for (gpu, chosen) in gpus.iter_mut() {
                    let mut text = format!("{}  ·  {}", gpu.name, gpu.backend.as_str());
                    if gpu.integrated {
                        text.push_str("  ·  integrated");
                    }
                    if let Some(bytes) = gpu.vram_bytes {
                        text.push_str(&format!("  ·  {:.0} GB", bytes as f64 / 1e9));
                    }
                    ui.checkbox(chosen, text);
                }
            }
        }
        if retry {
            self.gpus = find_gpus(self.backend, self.selection.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAINNET: &str = crate::config::DONATION_ADDRESS;

    fn gpu(index: u32) -> GpuDevice {
        GpuDevice {
            index,
            name: format!("GPU {index}"),
            vendor: "test".into(),
            vram_bytes: None,
            backend: BackendKind::Wgpu,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: None,
        }
    }

    fn entry() -> Entry {
        Entry {
            address: MAINNET.into(),
            ..Entry::from_config(&RuntimeConfig::default())
        }
    }

    #[test]
    fn a_start_takes_the_chosen_gpus_and_settings() {
        let mut entry = entry();
        entry.intensity = 60;
        let launch = entry
            .launch(
                &RuntimeConfig::default(),
                &[(gpu(0), false), (gpu(1), true)],
            )
            .unwrap();
        assert_eq!(launch.plan, Plan::Runtime(MiningToken::Photon));
        assert_eq!(
            launch.gpus.iter().map(|gpu| gpu.index).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(launch.cfg.intensity, 60);
        assert_eq!(launch.cfg.payout_address, MAINNET);
    }

    #[test]
    fn a_start_needs_an_address_for_the_network_and_a_gpu() {
        let base = RuntimeConfig::default();
        let mut entry = entry();
        assert!(entry.launch(&base, &[(gpu(0), false)]).is_err());
        entry.set_network(MiningNetwork::Chipnet);
        let wrong = entry.launch(&base, &[(gpu(0), true)]).unwrap_err();
        assert!(!wrong.is_empty());
        entry.address.clear();
        assert!(entry
            .launch(&base, &[(gpu(0), true)])
            .unwrap_err()
            .contains("payout address"));
    }

    #[test]
    fn a_split_checks_its_shares_as_the_command_line_does() {
        let mut entry = entry();
        entry.choice = Choice::Split;
        let available = SplitToken::available(entry.network);
        assert_eq!(
            entry
                .shares
                .iter()
                .map(|(token, _)| *token)
                .collect::<Vec<_>>(),
            available
        );
        let launch = entry
            .launch(&RuntimeConfig::default(), &[(gpu(0), true)])
            .unwrap();
        assert!(matches!(launch.plan, Plan::Split(_)));
        for (_, share) in &mut entry.shares {
            *share = 0;
        }
        assert!(entry
            .launch(&RuntimeConfig::default(), &[(gpu(0), true)])
            .is_err());
        entry.shares[0].1 = 7;
        assert!(entry
            .launch(&RuntimeConfig::default(), &[(gpu(0), true)])
            .unwrap_err()
            .contains("multiple of 5"));
    }

    #[test]
    fn changing_network_keeps_the_choices_to_its_tokens() {
        let mut entry = entry();
        entry.set_network(MiningNetwork::Chipnet);
        let available = SplitToken::available(MiningNetwork::Chipnet);
        assert_eq!(
            entry
                .shares
                .iter()
                .map(|(token, _)| *token)
                .collect::<Vec<_>>(),
            available
        );
        match entry.choice {
            Choice::Token(token) => assert!(available.contains(&token)),
            Choice::Split => assert!(available.len() > 1),
        }
    }
}
