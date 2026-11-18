//! Renders the window's screens offscreen to PNG files, with made-up data,
//! to look at them without a desktop:
//! `cargo test --features gui-preview -- --ignored gui_preview`
//! writes them to `PICKAXE_GUI_PREVIEW_DIR` (default `target/gui-preview`).
use super::dashboard;
use super::session::{Board, GpuLine, Link, TokenLine, HISTORY_CAP};
use super::setup::{Choice, Entry, Form};
use crate::backend::{BackendKind, GpuDevice};
use crate::config::{MiningNetwork, RuntimeConfig};
use crate::runtime::{RuntimeSnapshot, SupervisorState};
use crate::telemetry::GpuTelemetry;
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
            },
            TokenLine {
                name: "ELECTRIC".into(),
                share: Some(20),
                rate: 2.1e9,
                rewards: 1,
                state: "Searching at difficulty 3".into(),
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
        ..Board::default()
    }
}

#[test]
#[ignore = "renders with the GPU; run on request"]
fn gui_preview() {
    let mut entry = Entry::from_config(&RuntimeConfig::default());
    entry.choice = Choice::Split;
    let mut form = Form::found(
        entry,
        vec![
            (
                gpu(
                    0,
                    "NVIDIA GeForce RTX 5070 Ti Laptop GPU",
                    BackendKind::Cuda,
                    false,
                    12,
                ),
                true,
            ),
            (gpu(1, "AMD Radeon 610M", BackendKind::Wgpu, true, 2), false),
        ],
    );
    let base = RuntimeConfig::default();
    save("setup.png", SIZE, |ui| {
        form.show(ui, &base);
    });
    let board = board();
    let link = Link::default();
    save("dashboard.png", TALL, |ui| {
        dashboard::show(ui, &board, &link)
    });
}
