//! Renders the window's screens offscreen to PNG files, with made-up data,
//! to look at them without a desktop:
//! `cargo test --features gui-preview -- --ignored gui_preview`
//! writes them to `PICKAXE_GUI_PREVIEW_DIR` (default `target/gui-preview`).
use super::dashboard;
use super::session::{Board, GpuLine, Link, TokenLine, HISTORY_CAP};
use super::setup::Setup;
use crate::backend::{BackendKind, GpuDevice};
use crate::config::{MiningNetwork, MiningProfiles, RuntimeConfig, SavedConfig};
use crate::runtime::{RuntimeSnapshot, SupervisorState};
use crate::telemetry::GpuTelemetry;
use crate::tui::{SetupDriver, SetupInput};
use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SIZE: [f32; 2] = [1040.0, 760.0];
/// Tall enough to show the whole dashboard in one picture.
const TALL: [f32; 2] = [1040.0, 1240.0];

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("PICKAXE_GUI_PREVIEW_DIR")
        .map_or_else(|| PathBuf::from("target/gui-preview"), PathBuf::from);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save(name: &str, size: [f32; 2], ui: impl FnMut(&mut egui::Ui)) {
    let mut harness = egui_kittest::Harness::builder()
        .with_size(size)
        .wgpu()
        .build_ui(ui);
    harness.run_steps(4);
    let image = harness.render().expect("render the screen");
    let path = out_dir().join(name);
    image.save(&path).unwrap();
    println!("wrote {}", path.display());
}

fn gpu(index: u32, name: &str, backend: BackendKind, integrated: bool, gb: u64) -> GpuDevice {
    GpuDevice {
        index,
        name: name.into(),
        vendor: String::new(),
        vram_bytes: Some(gb * 1_000_000_000),
        backend,
        detail: String::new(),
        integrated,
        ready: true,
        pci: None,
    }
}

fn snapshot() -> RuntimeSnapshot {
    RuntimeSnapshot {
        state: SupervisorState::Mining,
        gpu_backend: "cuda".into(),
        gpu_device: 0,
        gpus: Vec::new(),
        generation_id: 1,
        network: MiningNetwork::Mainnet,
        fee_scheme: crate::config::MiningToken::Photon
            .fee_policy(MiningNetwork::Mainnet)
            .scheme,
        payout_address: String::new(),
        endpoint: "wss://electrum.example.org:50004".into(),
        height: 948_201,
        baton_txid: "7c1f".repeat(16),
        baton_vout: 0,
        photon_target_le: String::new(),
        state_checks: 0,
        transient_refresh_failures: 0,
        transport_failures: 0,
        consecutive_refresh_failures: 0,
        source_degraded: false,
        job_changes: 0,
        reconnects: 1,
        endpoint_rotations: 0,
        stale_winners: 0,
        verified_winners: 3,
        pending_winners: 0,
        last_error: None,
        search: Default::default(),
        gpu_telemetry: Default::default(),
        rigs: None,
        job_source: Default::default(),
        started_at: 0,
        token_donation: crate::donation::TokenDonation::from_bps(400),
        donation_default: crate::donation::TokenDonation::from_bps(400),
    }
}

fn wave(base: f64, swing: f64, phase: f64) -> VecDeque<f64> {
    (0..HISTORY_CAP)
        .map(|i| base + swing * ((i as f64 / 17.0) + phase).sin())
        .collect()
}

fn board() -> Board {
    let telemetry = GpuTelemetry {
        samples: 10,
        gpu_utilization_percent: Some(99.0),
        power_watts: Some(96.0),
        temperature_c: Some(71.0),
        graphics_clock_mhz: Some(2310.0),
        ..GpuTelemetry::default()
    };
    let electric = crate::electric::ui::Summary {
        state: "Searching at difficulty 3".into(),
        rate: 2.1e9,
        claims: 1,
        pot_address: Some(crate::config::DONATION_ADDRESS.into()),
        pot: Some((12_400, 18)),
        endpoint: Some("wss://electrum.example.org:50004".into()),
        difficulty: Some(3),
        claim_fee_estimate: Some(520),
        height: Some(948_201),
        intensity: 100,
        ..Default::default()
    };
    Board {
        live: true,
        network: Some(MiningNetwork::Mainnet),
        tokens: vec![
            TokenLine {
                name: "PHOTON".into(),
                share: Some(80),
                rate: 780e6,
                rewards: 3,
                state: "Mining · height 948201".into(),
                donation: Some(crate::donation::TokenDonation::from_bps(400)),
                runtime: true,
            },
            TokenLine {
                name: "ELECTRIC".into(),
                share: Some(20),
                rate: 2.1e9,
                rewards: 1,
                state: "Searching at difficulty 3".into(),
                donation: Some(crate::donation::TokenDonation::from_bps(400)),
                runtime: false,
            },
        ],
        gpus: vec![GpuLine {
            name: "NVIDIA GeForce RTX 5070 Ti Laptop GPU".into(),
            rate: Some(780e6),
            telemetry: telemetry.clone(),
        }],
        telemetry,
        runtime: Some(snapshot()),
        electric: Some(electric),
        intensity: 100,
        layout: "Every 2 s: PHOTON 1.6 s, ELECTRIC 0.4 s".into(),
        history: vec![
            ("PHOTON".into(), wave(760e6, 30e6, 0.0)),
            ("ELECTRIC".into(), wave(2.0e9, 0.2e9, 1.3)),
        ],
        events: [
            "Mining session started",
            "PHOTON Connected: wss://electrum.example.org:50004",
            "ELECTRIC Fee pot holds 12400 sats (18 claims)",
            "PHOTON job refreshed at height 948201",
            "PHOTON Reward accepted",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        started: Instant::now().checked_sub(Duration::from_secs(3 * 3600 + 25 * 60)),
        notice: Some("Donation 4.00%".into()),
        ..Board::default()
    }
}

/// PHOTON alone, coordinating two rigs.
fn coordinator_board() -> Board {
    let mut board = board();
    board.tokens.truncate(1);
    board.tokens[0].share = None;
    board.history.truncate(1);
    board.electric = None;
    board.layout.clear();
    board.single_runtime = true;
    let rig = |name: &str, rate: f64| crate::rigs::RigLine {
        name: name.into(),
        gpus: 2,
        rate,
        winners: 1,
        version: env!("CARGO_PKG_VERSION").into(),
        ..Default::default()
    };
    if let Some(snapshot) = board.runtime.as_mut() {
        snapshot.payout_address = crate::config::DONATION_ADDRESS.into();
        snapshot.rigs = Some(crate::rigs::RigSummary {
            listen: "0.0.0.0:3340".into(),
            key: "9b1f…".into(),
            connected: 2,
            gpus: 4,
            rate: 2.9e9,
            winners: 2,
            rigs: vec![rig("garage", 1.5e9), rig("office", 1.4e9)],
            ..Default::default()
        });
    }
    board.rig_join = vec![
        (
            "local network".into(),
            "pickaxe mine --coordinator stratum2+tcp://192.168.1.20:3340/9b1f…".into(),
        ),
        (
            "Tailscale".into(),
            "pickaxe mine --coordinator stratum2+tcp://100.64.0.7:3340/9b1f…".into(),
        ),
    ];
    board
}

#[test]
#[ignore = "renders with the GPU; run on request"]
fn gui_preview() {
    setup_previews();
    let board = board();
    let link = Link::default();
    save("dashboard.png", TALL, |ui| {
        dashboard::show(ui, &board, &link)
    });
    let coordinator = coordinator_board();
    save("coordinator.png", TALL, |ui| {
        dashboard::show(ui, &coordinator, &link)
    });
    let rig = super::rig::RigBoard {
        status: Some(crate::rigs::rig_status_json(
            Some(("192.168.1.20:3340", false)),
            true,
            1.5e9,
            3,
            0,
            &[crate::rigs::RigGpu {
                name: "NVIDIA GeForce RTX 3080".into(),
                status: "mining".into(),
                rate: 1.5e9,
                winners: 3,
                temperature_c: Some(66.0),
                power_watts: Some(240.0),
                fan_percent: Some(55.0),
                ..Default::default()
            }],
        )),
        events: [
            "connected: {\"coordinator\":\"192.168.1.20:3340\"}",
            "job: {\"height\":948201}",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        started: Some(Instant::now()),
    };
    save("rig.png", SIZE, |ui| {
        super::rig::show(ui, &rig, true, &|| {})
    });
}

/// The GPUs the setup previews list.
fn preview_gpus() -> Vec<GpuDevice> {
    vec![
        gpu(
            0,
            "NVIDIA GeForce RTX 5070 Ti Laptop GPU",
            BackendKind::Cuda,
            false,
            12,
        ),
        gpu(1, "AMD Radeon 610M", BackendKind::Wgpu, true, 2),
    ]
}

/// Two saved profiles, paying the project's own donation addresses.
fn preview_profiles() -> MiningProfiles {
    let mut profiles = MiningProfiles::default();
    let mut mainnet = RuntimeConfig::default();
    mainnet
        .set_payout(crate::config::DONATION_ADDRESS.into())
        .unwrap();
    let mut chipnet = RuntimeConfig::default();
    chipnet.set_network(MiningNetwork::Chipnet);
    chipnet
        .set_payout(crate::config::CHIPNET_DONATION_ADDRESS.into())
        .unwrap();
    chipnet.set_intensity(60).unwrap();
    for (name, config) in [("Laptop", &mainnet), ("Chipnet test", &chipnet)] {
        profiles
            .upsert(
                None,
                name,
                SavedConfig::from_effective(
                    "auto",
                    &crate::backend::DeviceSelection::Default,
                    config,
                ),
            )
            .unwrap();
    }
    profiles
}

/// Clicks the item called `label` on the setup's screen.
fn click(driver: &mut SetupDriver, label: &str) {
    let item = driver
        .view()
        .items
        .iter()
        .position(|item| item.label == label)
        .unwrap_or_else(|| panic!("no {label:?} on the screen"));
    driver.act(SetupInput::Click(item));
}

/// Clicks and the terminal's keys move the window's setup, with no GPU.
#[test]
fn the_setup_answers_clicks_and_keys() {
    use egui_kittest::kittest::Queryable;
    let mut setup = Setup::ready(SetupDriver::preview(
        preview_gpus(),
        MiningProfiles::default(),
    ));
    let mut harness = egui_kittest::Harness::builder()
        .with_size(SIZE)
        .build_ui(|ui| {
            setup.show(ui);
        });
    harness.run_steps(2);
    assert!(harness.query_by_label("Setup   1/4 Hardware").is_some());
    harness.get_by_label("GPU mining").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Setup   2/4 Network").is_some());
    // Down moves to Chipnet and Enter goes on, as in the terminal.
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    assert!(harness
        .query_by_label_contains("Chipnet selected")
        .is_some());
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(harness.query_by_label("Setup   3/4 Token").is_some());
    harness.check_accessibility();
    harness.get_by_label("Back").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Setup   2/4 Network").is_some());
}

/// The setup's screens, reached by clicks as a miner would reach them.
fn setup_previews() {
    let shot = |name: &str, size: [f32; 2], driver: SetupDriver| {
        let mut setup = Setup::ready(driver);
        save(name, size, |ui| {
            setup.show(ui);
        });
    };
    shot(
        "setup-profiles.png",
        SIZE,
        SetupDriver::preview(preview_gpus(), preview_profiles()),
    );
    let fresh = || SetupDriver::preview(preview_gpus(), MiningProfiles::default());
    shot("setup-hardware.png", SIZE, fresh());
    let mut driver = fresh();
    click(&mut driver, "GPU mining");
    click(&mut driver, "Mainnet");
    shot("setup-token.png", SIZE, driver);
    let settings = || {
        let mut driver = fresh();
        click(&mut driver, "GPU mining");
        click(&mut driver, "Mainnet");
        click(&mut driver, "PHOTON");
        click(&mut driver, "Advanced");
        driver
    };
    shot("setup-settings.png", TALL, settings());
    let mut driver = settings();
    click(&mut driver, "Address");
    driver.act(SetupInput::SetText(crate::config::DONATION_ADDRESS.into()));
    shot("setup-editing.png", SIZE, driver);
    let mut driver = settings();
    click(&mut driver, "Fulcrum");
    shot("setup-connections.png", SIZE, driver);
}
