//! Renders the window's screens offscreen to PNG files, with made-up data,
//! to look at them without a desktop:
//! `cargo test --features gui-preview -- --ignored gui_preview`
//! writes them to `PICKAXE_GUI_PREVIEW_DIR` (default `target/gui-preview`).
#[cfg(feature = "stratum-v2")]
use super::asic::{self, AsicBoard, AsicLink, Page};
use super::dashboard;
use super::session::{Board, GpuLine, Link, TokenLine, HISTORY_CAP};
use super::setup::Setup;
use crate::backend::{BackendKind, GpuDevice};
use crate::config::{MiningNetwork, MiningProfiles, RuntimeConfig, SavedConfig};
use crate::runtime::{RuntimeSnapshot, SupervisorState};
#[cfg(feature = "stratum-v2")]
use crate::stratum_v2::telemetry::DeviceSnapshot;
use crate::telemetry::GpuTelemetry;
use crate::tui::{SetupDriver, SetupInput};
use eframe::egui;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SIZE: [f32; 2] = [1040.0, 760.0];
/// The window's smallest size.
const SMALL: [f32; 2] = [640.0, 480.0];
/// Tall enough to show the whole dashboard in one picture.
const TALL: [f32; 2] = [1040.0, 1240.0];

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("PICKAXE_GUI_PREVIEW_DIR")
        .map_or_else(|| PathBuf::from("target/gui-preview"), PathBuf::from);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save(name: &str, size: [f32; 2], ui: impl FnMut(&mut egui::Ui)) {
    // The window's fonts from the first frame, which the builder draws.
    let mut harness = egui_kittest::Harness::builder()
        .with_size(size)
        .with_context_setup(|ctx| ctx.set_fonts(super::fonts()))
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

#[cfg(feature = "stratum-v2")]
fn asic_device(label: &str, connected: bool, rate: f64, model: &str) -> DeviceSnapshot {
    DeviceSnapshot {
        label: label.into(),
        protocol: "SV1",
        connected,
        channels: 1,
        accepted: if connected { 1_284 } else { 312 },
        rejected: if connected { 3 } else { 9 },
        adapter_rejected: 0,
        last_rejection: (!connected).then_some("stale job"),
        hashrate_estimate: connected.then_some(rate),
        hashrate_hour: Some(rate * 0.98),
        difficulty: Some(65_536.0),
        best_share: Some(2.5e6),
        estimate_seconds: 300.0,
        last_share_seconds: Some(if connected { 4 } else { 900 }),
        connection_error: (!connected).then_some("SV1 disconnected"),
        adapter_error: None,
        reported_hashrate: connected.then_some(rate * 1.02),
        temperature_c: connected.then_some(64.0),
        fan: connected.then(|| "62%".into()),
        model: Some(model.into()),
        firmware: Some("2025.03".into()),
        power_w: connected.then_some(140.0),
        donation_bps: None,
        own_donation_bps: None,
    }
}

/// A solo server with three workers, as a Chipnet test farm shows it.
#[cfg(feature = "stratum-v2")]
fn asic_board() -> AsicBoard {
    use crate::stratum_v2::view::{ConnectInfo, ConnectLine, ServeMode, ServerFrame};
    let devices = vec![
        asic_device("rig1 #1", true, 6.1e12, "Avalonminer AvalonNano3s"),
        asic_device("rig2 #2", true, 1.2e12, "Bitaxe Gamma"),
        asic_device("garage #3", false, 9.8e12, "Antminer S19"),
    ];
    let line = |place, sv2, url: &str| ConnectLine {
        place,
        sv2,
        url: url.into(),
    };
    use crate::reach::Place;
    let key = "9fXpAuthorityKeyExampleOnly2kq";
    let stats = crate::stratum_v2::server::ServerStats {
        connections: 2,
        shares_accepted: 2_568,
        shares_rejected: 6,
        blocks_accepted: 1,
        blocks_pending: 1,
        template_ready: true,
        height: Some(298_114),
        nodes: 1,
        best_share: Some((2.5e6, "rig1 #1".into())),
        sessions_started: 5,
        ..Default::default()
    };
    let frame = ServerFrame {
        network: MiningNetwork::Chipnet,
        title: "Pickaxe · BCH ASIC mining",
        mode: ServeMode::Solo,
        joining: false,
        pool: None,
        node_client: Some("Bitcoin Cash Node 29.1.0".into()),
        stats,
        devices,
        lights: vec![(
            "Node",
            crate::p2pool::launch::Light::Green,
            "templates ready · height 298114".into(),
        )],
        donation: crate::donation::bch::BchDonation::default(),
        setting_error: None,
        header: "chipnet · Node Ready (Bitcoin Cash Node 29.1.0) · Height 298114 · Donation 1.50% \
                 (0.50% of work · 1.00% of block rewards and token wins)\n2 of 3 workers online · \
                 7.30 TH/s · Shares 2568 accepted / 6 rejected · Blocks 1 accepted / 1 pending\n\
                 Point devices at: SV1 stratum+tcp://192.168.1.20:3333 · SV2 192.168.1.20:3336"
            .into(),
        overview:
            "chipnet · Node Ready (Bitcoin Cash Node 29.1.0) · Height 298114 · Donation 1.50% \
                   (0.50% of work · 1.00% of block rewards and token wins)\nDevices 2 · Sessions 5 \
                   · Shares 2568 accepted / 6 rejected (0 at SV1 adapter)\nValidated work: \
                   donation 0.51% · fee 0.00%\nBlocks 1 accepted / 1 pending / 0 rejected · \
                   Retries 0 · Last accepted\nConnection errors: SV2 0 / SV1 1\nTemplate errors 0 \
                   · Last None · ZMQ 14 notices\nSV2 0.0.0.0:3336 · SV1 0.0.0.0:3333\nAuthority \
                   9fXpAuthorityKeyExampleOnly2kq\nBest share 2.50M by rig1 #1 · Recent blocks \
                   #298113 rig1 #1 accepted 12m ago"
                .into(),
        advanced: "0.50% of mining work and 1.00% of each block reward and merge-mined token win \
                   go to the Pickaxe donation address. A token whose covenant takes no split \
                   gives the whole 1.50% as mining work instead.\nThe default is 1.50%; any \
                   setting from 0% to 100% works, in 0.5% steps. Changes apply to new jobs and \
                   are saved."
            .into(),
        started: "Set when the server started (change them in the setup's Advanced section or on \
                  the command line; they apply after a restart):\nListening  SV2 0.0.0.0:3336 · \
                  SV1 0.0.0.0:3333 · Templates 0.0.0.0:48442\nStart difficulty  4,096; vardiff \
                  then moves each device toward 20 shares a minute\n"
            .into(),
        devices_hint:
            "Point devices at: SV1 stratum+tcp://192.168.1.20:3333 · SV2 192.168.1.20:3336".into(),
        connect: ConnectInfo {
            lines: vec![
                line(
                    Place::LocalNetwork,
                    false,
                    "stratum+tcp://192.168.1.20:3333",
                ),
                line(Place::Tailscale, false, "stratum+tcp://100.64.0.7:3333"),
                line(
                    Place::LocalNetwork,
                    true,
                    &format!("stratum2+tcp://192.168.1.20:3336/{key}"),
                ),
                line(
                    Place::Tailscale,
                    true,
                    &format!("stratum2+tcp://100.64.0.7:3336/{key}"),
                ),
            ],
            templates: vec![line(Place::LocalNetwork, true, "192.168.1.20:48442")],
            notes: "\nUsername: any name for the device; it names the device on the workers \
                    page. Every block pays this server's payout address.\nPassword: anything; it \
                    is not checked.\nASICs in another place connect to a computer there running \
                    Pickaxe with Join a pool, pointed at an SV2 line above; only encrypted SV2 \
                    crosses the internet.\n"
                .into(),
            template_config: Some(format!(
                "[template_provider_type.Sv2Tp]\naddress = \"192.168.1.20:48442\"\npublic_key = \
                 \"{key}\"\n"
            )),
        },
        panel: None,
    };
    AsicBoard {
        live: true,
        frame: Some(frame),
        started: Instant::now().checked_sub(Duration::from_secs(26 * 3600 + 4 * 60)),
        ..AsicBoard::default()
    }
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
    for (name, state) in update_states() {
        let view = updates_view(state);
        save(&format!("updates-{name}.png"), [560.0, 560.0], |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                super::updates::pickaxe_section(ui, &view, true, None);
                #[cfg(feature = "stratum-v2")]
                {
                    ui.separator();
                    super::updates::p2pool_section(ui, &p2pool_view());
                }
            });
        });
    }
}

/// Each state of Pickaxe's updates the panel shows, by name.
fn update_states() -> Vec<(&'static str, crate::update::State)> {
    use super::updates::tests::offer;
    use crate::update::{release::Version, State};
    vec![
        ("available", State::Available(offer(true))),
        ("unsigned", State::Available(offer(false))),
        ("confirm", State::Confirm(offer(false))),
        (
            "downloading",
            State::Downloading {
                offer: offer(true),
                percent: 42,
            },
        ),
        (
            "ready",
            State::Ready {
                version: Version(0, 0, 5),
                signed: true,
                waiting: None,
            },
        ),
        ("up-to-date", State::UpToDate),
    ]
}

fn updates_view(state: crate::update::State) -> super::updates::PickaxeView {
    super::updates::PickaxeView {
        running: crate::update::release::Version(0, 0, 4),
        state,
        mode: crate::update::Mode::Notify,
        has_key: false,
        checking: true,
    }
}

#[cfg(feature = "stratum-v2")]
fn p2pool_view() -> super::updates::P2poolView {
    super::updates::P2poolView {
        installed: Some("v0.0.1-alpha (unsigned)".into()),
        latest: super::updates::P2poolLatest::Available {
            tag: "v0.0.2".into(),
            prerelease: true,
            signature: "unsigned: checked against its published SHA-256 only",
        },
        busy: None,
        answer: None,
        auto_update: true,
        can_roll_back: true,
    }
}

/// The Updates panel says each state and offers its action; Restart waits
/// for the session to end, and an unsigned release asks first.
#[test]
fn the_updates_panel_shows_each_state() {
    use super::updates::{pickaxe_section, Action};
    use egui_kittest::kittest::Queryable;
    let expected = [
        ("available", "Update to v0.0.5"),
        ("unsigned", "Unsigned release: update manually"),
        ("confirm", "Install an unsigned release?"),
        ("downloading", "v0.0.5 available"),
        ("ready", "Updated to v0.0.5 · restart to apply"),
        ("up-to-date", "v0.0.4 is the newest"),
    ];
    for ((name, state), (same, text)) in update_states().into_iter().zip(expected) {
        assert_eq!(name, same);
        let view = updates_view(state);
        let mut harness = egui_kittest::Harness::builder()
            .with_size([560.0, 560.0])
            .with_context_setup(|ctx| ctx.set_fonts(super::fonts()))
            .build_ui(|ui| {
                pickaxe_section(ui, &view, false, None);
            });
        harness.run_steps(2);
        assert!(
            harness.query_by_label_contains(text).is_some(),
            "{name}: {text}"
        );
        assert!(harness
            .query_by_label_contains("Auto-update: install signed releases")
            .is_some());
        assert!(harness
            .query_by_label_contains("you confirm each one")
            .is_some());
    }
    // The buttons answer with their actions: Update, and Restart only
    // between sessions.
    let available = updates_view(update_states().remove(0).1);
    let mut clicked = Vec::new();
    let mut harness = egui_kittest::Harness::builder()
        .with_size([560.0, 560.0])
        .build_ui(|ui| clicked.extend(pickaxe_section(ui, &available, true, None)));
    harness.run_steps(2);
    harness.get_by_label("Update to v0.0.5").click();
    harness.run_steps(2);
    drop(harness);
    assert!(clicked.contains(&Action::Update), "{clicked:?}");
    for between_sessions in [false, true] {
        let ready = updates_view(update_states().remove(4).1);
        let mut clicked = Vec::new();
        let mut harness = egui_kittest::Harness::builder()
            .with_size([560.0, 560.0])
            .build_ui(|ui| clicked.extend(pickaxe_section(ui, &ready, between_sessions, None)));
        harness.run_steps(2);
        harness.get_by_label("Restart Pickaxe").click();
        harness.run_steps(2);
        drop(harness);
        assert_eq!(clicked.contains(&Action::Restart), between_sessions);
    }
}

/// P2Pool v2's section uses P2Pool's own updater: its version, the newest
/// release, unsigned as it is, and its own Update and auto-update.
#[cfg(feature = "stratum-v2")]
#[test]
fn the_p2pool_section_offers_its_own_update() {
    use super::updates::{p2pool_section, Action};
    use egui_kittest::kittest::Queryable;
    let view = p2pool_view();
    let mut clicked = Vec::new();
    let mut harness = egui_kittest::Harness::builder()
        .with_size([560.0, 400.0])
        .build_ui(|ui| clicked.extend(p2pool_section(ui, &view)));
    harness.run_steps(2);
    assert!(harness
        .query_by_label_contains("v0.0.2 available (pre-release)")
        .is_some());
    assert!(harness
        .query_by_label_contains("unsigned: checked against its published SHA-256 only")
        .is_some());
    harness.get_by_label("Update P2Pool").click();
    harness.run_steps(2);
    harness
        .get_by_label("Auto-update: install a newer P2Pool before it starts")
        .click();
    harness.run_steps(2);
    drop(harness);
    assert!(clicked.contains(&Action::P2poolUpdate), "{clicked:?}");
    assert!(clicked.contains(&Action::P2poolAuto(false)), "{clicked:?}");
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

/// Steps the item called `label` on the setup's screen, as its > does.
fn step(driver: &mut SetupDriver, label: &str) {
    let item = driver
        .view()
        .items
        .iter()
        .position(|item| item.label == label)
        .unwrap_or_else(|| panic!("no {label:?} on the screen"));
    driver.act(SetupInput::Step {
        item,
        forward: true,
    });
}

/// #### Pool modes: Run a pool, Chipnet, ASIC pool, then P2Pool v2 (two
/// steps past a Public Solo Endpoint) with the Advanced section open.
fn p2pool_page() -> SetupDriver {
    let mut driver = SetupDriver::preview(preview_gpus(), MiningProfiles::default());
    click(&mut driver, "Run a pool");
    click(&mut driver, "Chipnet");
    click(&mut driver, "ASIC");
    step(&mut driver, "Mode");
    step(&mut driver, "Mode");
    click(&mut driver, "Advanced");
    driver
}

/// #### Pool modes: the Run a pool page in P2Pool v2 names every control,
/// with its two donations apart.
#[test]
fn the_pool_modes_page_names_every_control() {
    use egui_kittest::kittest::Queryable;
    let driver = p2pool_page();
    let view = driver.view();
    let mode = view
        .items
        .iter()
        .find(|item| item.label == "Mode")
        .expect("the Mode row");
    assert_eq!(mode.value_text(), "P2Pool v2");
    for label in [
        "Peers",
        "Pickaxe donation",
        "P2Pool donation",
        "Fallback pools",
    ] {
        assert!(view.items.iter().any(|item| item.label == label), "{label}");
    }
    let mut setup = Setup::ready(driver);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(TALL)
        .with_context_setup(|ctx| ctx.set_fonts(super::fonts()))
        .build_ui(|ui| {
            setup.show(ui);
        });
    harness.run_steps(4);
    assert!(harness
        .query_by_label_contains("linked with other P2Pool servers")
        .is_some());
    assert!(harness.query_by_label_contains("Start P2Pool v2").is_some());
    harness.check_accessibility();
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
        .with_context_setup(|ctx| ctx.set_fonts(super::fonts()))
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
    // Esc on the first screen asks before the window closes.
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(harness.query_by_label("Quit Pickaxe?").is_some());
    harness.get_by_label("Stay").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Quit Pickaxe?").is_none());
    assert!(harness.query_by_label("Setup   1/4 Hardware").is_some());
}

/// A profile's own row opens, renames and deletes that profile, whatever
/// row the cursor is on.
#[test]
fn a_profiles_buttons_act_on_their_row() {
    use egui_kittest::kittest::Queryable;
    let mut setup = Setup::ready(SetupDriver::preview(preview_gpus(), preview_profiles()));
    let mut harness = egui_kittest::Harness::builder()
        .with_size(SIZE)
        .with_context_setup(|ctx| ctx.set_fonts(super::fonts()))
        .build_ui(|ui| {
            setup.show(ui);
        });
    // A few frames after each click: the bottom panel takes a frame to
    // grow to a status line, which moves its buttons.
    harness.run_steps(4);
    harness.get_by_label("Delete Chipnet test").click();
    harness.run_steps(4);
    assert!(harness
        .query_by_label_contains("Delete profile \u{201c}Chipnet test\u{201d}?")
        .is_some());
    harness.get_by_label("Keep").click();
    harness.run_steps(4);
    assert!(harness.query_by_label_contains("Delete profile").is_none());
    harness.get_by_label("Open Chipnet test").click();
    harness.run_steps(4);
    assert!(harness
        .query_by_label_contains("profile: Chipnet test")
        .is_some());
    harness.check_accessibility();
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
    // #### Pool modes: Run a pool with P2Pool v2 chosen.
    shot("setup-run-p2pool.png", TALL, p2pool_page());
    // The window's smallest size.
    let mut driver = settings();
    click(&mut driver, "Address");
    driver.act(SetupInput::SetText(crate::config::DONATION_ADDRESS.into()));
    shot("setup-editing-small.png", SMALL, driver);
    shot(
        "setup-profiles-small.png",
        SMALL,
        SetupDriver::preview(preview_gpus(), preview_profiles()),
    );
}

/// The ASIC server's pages and its Device panel.
#[cfg(feature = "stratum-v2")]
#[test]
#[ignore = "renders with the GPU; run on request"]
fn gui_preview_asic() {
    let mut asic = asic_board();
    let servers = AsicLink::default();
    for (name, page) in [
        ("asic-workers.png", Page::Workers),
        ("asic-overview.png", Page::Overview),
        ("asic-connection.png", Page::Connection),
        ("asic-advanced.png", Page::Advanced),
    ] {
        save(name, SIZE, |ui| {
            asic::open_page(ui.ctx(), page);
            asic::show(ui, &asic, &servers)
        });
    }
    if let Some(frame) = asic.frame.as_mut() {
        frame.panel = Some(crate::stratum_v2::view::PanelFrame {
            text: "Worker  rig1 #1 · Online\nAvalonminer AvalonNano3s · firmware 2025.03 · 140 \
                   W\nReached on: local network\nIdentified by asic-rs: AvalonMiner\n\n1  \
                   Restart\n2  Pause mining\n3  Locate (blink the LED)\n4  Fan speed…\n5  Power \
                   mode…\n6  Pools…\nl  Enter this device's login\n\nEsc  Back to workers\n\n\
                   Actions go straight to the device, on your local network or Tailscale. Each \
                   one needs your confirmation. The device's address is never shown."
                .into(),
            input: crate::stratum_v2::view::PanelInput::Keys,
        });
    }
    save("asic-panel.png", SIZE, |ui| asic::show(ui, &asic, &servers));
}
