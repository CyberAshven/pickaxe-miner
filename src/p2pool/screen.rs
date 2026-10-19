//! #### PR #53: `pickaxe p2pool`: install, update, roll back and run
//! P2Pool, with a screen of lights as Gupaxx shows its processes: P2Pool
//! green when it runs and miners can connect, the BCH node green when it
//! answers on its chain and is synced.
use super::launch::{self, Light, State, Supervisor};
use super::release::{self, Check, Installed, Release};
use super::{home, Settings, ShareChain};
use crate::cli::P2poolCommand;
use crate::config::{MiningNetwork, RuntimeConfig};
use crate::node::NodeCheck;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Runs one `pickaxe p2pool` command.
pub fn command(
    cfg: &RuntimeConfig,
    command: &P2poolCommand,
    config_path: &Path,
    plain: bool,
) -> Result<(), String> {
    let home = home(config_path);
    match command {
        P2poolCommand::Status => {
            let settings = Settings::load(&home)?;
            match release::installed(&home)? {
                Some(installed) => println!(
                    "installed: {} ({})",
                    installed.tag,
                    installed.binary.display()
                ),
                None => println!("installed: none"),
            }
            match release::check(&home) {
                Ok(Check::UpToDate(tag)) => println!("newest:    {tag} (up to date)"),
                Ok(Check::Available { release, .. }) => println!(
                    "newest:    {}{} (run `pickaxe p2pool update`)",
                    release.tag,
                    if release.prerelease {
                        ", pre-release"
                    } else {
                        ""
                    }
                ),
                Err(error) => println!("newest:    unknown ({error})"),
            }
            println!(
                "settings:  auto-update {}, {} share chain, {}",
                on_off(settings.auto_update),
                settings.share_chain.as_str(),
                peers_text(&settings)
            );
            println!(
                "releases:  https://github.com/{}/releases",
                release::REPOSITORY
            );
            Ok(())
        }
        P2poolCommand::Update { check } => match release::check(&home)? {
            Check::UpToDate(tag) => {
                println!("P2Pool {tag} is the newest release.");
                Ok(())
            }
            Check::Available { current, release } if *check => {
                println!(
                    "P2Pool {} is available{}; `pickaxe p2pool update` installs it.",
                    release.tag,
                    current.map_or(String::new(), |tag| format!(" (installed: {tag})"))
                );
                Ok(())
            }
            Check::Available { release, .. } => {
                let installed = release::install(&home, &release)?;
                println!(
                    "Installed P2Pool {} (SHA-256 {} as published).",
                    installed.tag, installed.sha256
                );
                Ok(())
            }
        },
        P2poolCommand::Rollback => {
            let back = release::rollback(&home)?;
            println!(
                "Back to P2Pool {}; `pickaxe p2pool rollback` again returns to {}.",
                back.tag,
                back.previous
                    .as_ref()
                    .map_or("?", |previous| previous.tag.as_str())
            );
            Ok(())
        }
        P2poolCommand::Set {
            auto_update,
            share_chain,
            peers,
            no_peers,
        } => {
            let mut settings = Settings::load(&home)?;
            if let Some(value) = auto_update {
                settings.auto_update = match value.to_ascii_lowercase().as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err(format!("--auto-update {value}: on or off")),
                };
            }
            if let Some(chain) = share_chain {
                settings.share_chain = ShareChain::parse(chain)?;
            }
            if *no_peers {
                settings.peers.clear();
            }
            settings.peers.extend(peers.iter().cloned());
            settings.peers.dedup();
            settings.save(&home)?;
            println!(
                "P2Pool: auto-update {}, {} share chain, {}.",
                on_off(settings.auto_update),
                settings.share_chain.as_str(),
                peers_text(&settings)
            );
            Ok(())
        }
        P2poolCommand::Run { no_update } => run(cfg, config_path, plain, *no_update),
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

fn peers_text(settings: &Settings) -> String {
    match settings.peers.len() {
        0 => "no peers (its own share chain)".into(),
        1 => "1 peer".into(),
        count => format!("{count} peers"),
    }
}

/// What the update line says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Update {
    UpToDate,
    Available(Release),
    /// The release list could not be read.
    Unknown(String),
    Failed(String),
}

/// The installed P2Pool after applying the update policy: install when
/// none is installed; with auto-update on, install a newer release (and
/// keep the current one if that fails); with it off, offer it.
pub(crate) fn prepare(
    home: &Path,
    settings: &Settings,
    no_update: bool,
) -> Result<(Installed, Update), String> {
    let current = release::installed(home)?.filter(|installed| installed.binary.is_file());
    let checked = release::check(home);
    match (current, checked) {
        (None, Ok(Check::Available { release, .. })) => {
            println!(
                "Installing P2Pool {} from {}…",
                release.tag,
                release::REPOSITORY
            );
            Ok((release::install(home, &release)?, Update::UpToDate))
        }
        (None, Ok(Check::UpToDate(_))) => {
            Err("P2Pool's binary is missing; run `pickaxe p2pool update`".into())
        }
        (None, Err(error)) => Err(format!(
            "P2Pool is not installed and its releases cannot be read: {error}"
        )),
        (Some(current), Ok(Check::Available { release, .. }))
            if settings.auto_update && !no_update =>
        {
            println!("Updating P2Pool {} to {}…", current.tag, release.tag);
            match release::install(home, &release) {
                Ok(installed) => Ok((installed, Update::UpToDate)),
                Err(error) => Ok((current, Update::Failed(format!("{}: {error}", release.tag)))),
            }
        }
        (Some(current), Ok(Check::Available { release, .. })) => {
            Ok((current, Update::Available(release)))
        }
        (Some(current), Ok(Check::UpToDate(_))) => Ok((current, Update::UpToDate)),
        (Some(current), Err(error)) => Ok((current, Update::Unknown(error))),
    }
}

/// The BCH node as its light shows it, checked every 15 seconds.
pub(crate) struct NodeWatch {
    state: Arc<Mutex<Option<NodeCheck>>>,
    stop: Arc<AtomicBool>,
}

impl NodeWatch {
    pub(crate) fn start(url: String, network: MiningNetwork) -> Self {
        let state = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let check = crate::node::check_node(&url, network);
                    *state.lock().unwrap_or_else(|e| e.into_inner()) = Some(check);
                    for _ in 0..150 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            });
        }
        Self { state, stop }
    }

    pub(crate) fn check(&self) -> Option<NodeCheck> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for NodeWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn node_light(check: Option<&NodeCheck>) -> (Light, String) {
    match check {
        None => (Light::Grey, "checking…".into()),
        Some(NodeCheck::Ready { info, .. }) if info.syncing || info.blocks < info.headers => {
            (Light::Yellow, info.summary())
        }
        Some(NodeCheck::Ready { info, .. }) => (Light::Green, info.summary()),
        Some(other) => (Light::Red, other.summary()),
    }
}

/// How a run of the screen ended.
enum Exit {
    Quit,
    Update(Release),
}

/// Runs P2Pool until `q` or Ctrl+C, installing an update first as the
/// policy says, and again when `u` asks for one.
fn run(
    cfg: &RuntimeConfig,
    config_path: &Path,
    plain: bool,
    no_update: bool,
) -> Result<(), String> {
    let home = home(config_path);
    let mut settings = Settings::load(&home)?;
    let (mut installed, mut update) = prepare(&home, &settings, no_update)?;
    let node = cfg
        .custom_node_endpoints()
        .first()
        .map(|node| (*node).to_owned())
        .ok_or("P2Pool needs your BCH node: give --node-rpc http://HOST:PORT")?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|error| error.to_string())?;
    }
    let node_watch = NodeWatch::start(node, cfg.network);
    loop {
        let plan = launch::plan(cfg, &home, &installed, &settings)?;
        let supervisor = Supervisor::start(plan.command()?, Some(launch::API_PORT));
        let view = View {
            network: cfg.network,
            plan: &plan,
            device_url: plan.device_url()?,
            started: Instant::now(),
        };
        let exit = if plain {
            report(&stop, &supervisor, &node_watch, &view, &update)
        } else {
            screen(
                &stop,
                &supervisor,
                &node_watch,
                &view,
                &mut update,
                &mut settings,
                &home,
            )
        };
        supervisor.stop();
        match exit? {
            Exit::Quit => return Ok(()),
            Exit::Update(release) => match release::install(&home, &release) {
                Ok(next) => {
                    installed = next;
                    update = Update::UpToDate;
                }
                Err(error) => update = Update::Failed(format!("{}: {error}", release.tag)),
            },
        }
    }
}

/// What every frame shows besides the live state.
struct View<'a> {
    network: MiningNetwork,
    plan: &'a launch::Plan,
    device_url: String,
    started: Instant,
}

/// Without a screen: each change of either light as a line.
fn report(
    stop: &AtomicBool,
    supervisor: &Supervisor,
    node_watch: &NodeWatch,
    view: &View<'_>,
    update: &Update,
) -> Result<Exit, String> {
    println!(
        "P2Pool {} on {} ({} share chain); devices: {}. Ctrl+C stops.",
        view.plan.tag,
        view.network.as_str(),
        view.plan.settings.share_chain.as_str(),
        view.device_url
    );
    match update {
        Update::Available(release) => println!(
            "P2Pool {} is available: `pickaxe p2pool update`, or auto-update on.",
            release.tag
        ),
        Update::Failed(error) => println!("P2Pool update failed: {error}"),
        Update::Unknown(error) => println!("P2Pool updates unknown: {error}"),
        Update::UpToDate => {}
    }
    let (mut last_p2pool, mut last_node) = (String::new(), String::new());
    while !stop.load(Ordering::Relaxed) {
        let p2pool = format!("P2Pool: {}", supervisor.state().headline());
        if p2pool != last_p2pool {
            println!("{p2pool}");
            last_p2pool = p2pool;
        }
        let node = format!("Node: {}", node_light(node_watch.check().as_ref()).1);
        if node != last_node {
            println!("{node}");
            last_node = node;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(Exit::Quit)
}

#[allow(clippy::too_many_arguments)]
fn screen(
    stop: &AtomicBool,
    supervisor: &Supervisor,
    node_watch: &NodeWatch,
    view: &View<'_>,
    update: &mut Update,
    settings: &mut Settings,
    home: &Path,
) -> Result<Exit, String> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::{backend::CrosstermBackend, Terminal};
    enable_raw_mode().map_err(|e| e.to_string())?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        }
    }
    let _restore = Restore;
    execute!(std::io::stdout(), EnterAlternateScreen).map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
    let mut status = String::new();
    while !stop.load(Ordering::Relaxed) {
        let shown = Shown {
            view,
            state: supervisor.state(),
            running_for: supervisor.running_for(),
            restarts: supervisor.restarts(),
            node: node_watch.check(),
            update: &*update,
            auto_update: settings.auto_update,
            log: supervisor.log(40),
            status: &status,
        };
        terminal
            .draw(|area| draw(area, &shown))
            .map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        status.clear();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Exit::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Exit::Quit)
            }
            KeyCode::Char('r') => supervisor.restart(),
            KeyCode::Char('u') => match update {
                Update::Available(release) => return Ok(Exit::Update(release.clone())),
                _ => status = "No newer P2Pool release to install.".into(),
            },
            KeyCode::Char('a') => {
                settings.auto_update = !settings.auto_update;
                match settings.save(home) {
                    Ok(()) if settings.auto_update => {
                        if let Update::Available(release) = update {
                            // Auto-update on: install what is waiting now.
                            return Ok(Exit::Update(release.clone()));
                        }
                    }
                    Ok(()) => {}
                    Err(error) => status = error,
                }
            }
            _ => {}
        }
    }
    Ok(Exit::Quit)
}

/// One frame's content.
struct Shown<'a> {
    view: &'a View<'a>,
    state: State,
    running_for: Option<Duration>,
    restarts: u32,
    node: Option<NodeCheck>,
    update: &'a Update,
    auto_update: bool,
    log: Vec<String>,
    status: &'a str,
}

pub(crate) fn light_span(light: Light) -> ratatui::text::Span<'static> {
    use ratatui::style::{Color, Style};
    let (mark, color) = match light {
        Light::Green => ("●", Color::Green),
        Light::Yellow => ("●", Color::Yellow),
        Light::Red => ("●", Color::Red),
        Light::Grey => ("○", Color::DarkGray),
    };
    ratatui::text::Span::styled(mark, Style::default().fg(color))
}

fn duration_text(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60),
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, state: &Shown<'_>) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Style},
        text::{Line, Span},
        widgets::{Block, Paragraph, Wrap},
    };
    let mut p2pool = state.state.summary();
    if let Some(running) = state.running_for {
        p2pool.push_str(&format!(" · up {}", duration_text(running)));
    }
    let (node_light, node_text) = node_light(state.node.as_ref());
    let update = match state.update {
        Update::UpToDate => "up to date".to_owned(),
        Update::Available(release) => format!(
            "{} is available{}",
            release.tag,
            if state.auto_update {
                " (installs at the next start)"
            } else {
                " · press u to install"
            }
        ),
        Update::Unknown(error) => format!("updates unknown ({error})"),
        Update::Failed(error) => format!("update failed ({error})"),
    };
    let plan = state.view.plan;
    let mut lines = vec![
        Line::from(vec![
            Span::raw("P2Pool   "),
            light_span(state.state.light()),
            Span::raw(format!(" {p2pool}")),
        ]),
        Line::from(vec![
            Span::raw("Node     "),
            light_span(node_light),
            Span::raw(format!(" {node_text}")),
        ]),
        Line::from(format!(
            "Version  {} · auto-update {} · {update}",
            plan.tag,
            on_off(state.auto_update)
        )),
        Line::from(format!(
            "Chain    {} share chain · {}",
            plan.settings.share_chain.as_str(),
            peers_text(&plan.settings)
        )),
        Line::from(format!("Devices  {}", state.view.device_url)),
        Line::from(format!(
            "Started  {} ago · {} restart{}",
            duration_text(state.view.started.elapsed()),
            state.restarts,
            if state.restarts == 1 { "" } else { "s" }
        )),
    ];
    if !state.status.is_empty() {
        lines.push(Line::from(Span::styled(
            state.status.to_owned(),
            Style::default().fg(Color::Yellow),
        )));
    }
    let areas = Layout::vertical([
        Constraint::Length(lines.len() as u16 + 2),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(
                " Pickaxe · P2Pool · {} ",
                state.view.network.as_str()
            ))),
        areas[0],
    );
    let shown = state
        .log
        .len()
        .saturating_sub(usize::from(areas[1].height.saturating_sub(2)));
    frame.render_widget(
        Paragraph::new(state.log[shown..].join("\n"))
            .block(Block::bordered().title(" P2Pool's log ")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("u install update · a auto-update on/off · r restart P2Pool · q quit"),
        areas[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeInfo;

    #[test]
    fn the_screen_shows_both_lights_the_version_and_where_devices_connect() {
        let plan = launch::Plan {
            binary: "p2poolv2.exe".into(),
            tag: "v0.0.1-alpha".into(),
            home: "p2pool".into(),
            network: MiningNetwork::Chipnet,
            settings: Settings::default(),
            node_url: "https://node.example:48332".into(),
            login: None,
            zmq: "tcp://node.example:28332".into(),
            bootstrap_address: String::new(),
            authority_secret: [7; 32],
        };
        let view = View {
            network: MiningNetwork::Chipnet,
            plan: &plan,
            device_url: plan.device_url().unwrap(),
            started: Instant::now(),
        };
        let release = Release {
            tag: "v0.0.2".into(),
            published_at: String::new(),
            prerelease: true,
            asset: String::new(),
            asset_url: String::new(),
            sha256_url: String::new(),
            digest: None,
        };
        let update = Update::Available(release);
        let node = NodeCheck::Ready {
            info: NodeInfo {
                client: "Bitcoin Cash Node 29.2.0".into(),
                chain: "chip".into(),
                blocks: 328_500,
                headers: 328_500,
                syncing: false,
                progress: 1.0,
            },
            follows_photon: true,
        };
        let text = |shown: &Shown<'_>| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
            terminal.draw(|area| draw(area, shown)).unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let running = text(&Shown {
            view: &view,
            state: State::Running {
                tip: Some(1234),
                peers: Some(0),
            },
            running_for: Some(Duration::from_secs(75)),
            restarts: 1,
            node: Some(node),
            update: &update,
            auto_update: false,
            log: vec!["INFO Stratum V2 listening on 127.0.0.1:3361".into()],
            status: "",
        });
        for expected in [
            " Pickaxe · P2Pool · chipnet ",
            "P2Pool   ● running · share chain tip 1234 · 0 peers · up 1m 15s",
            "Node     ● Bitcoin Cash Node 29.2.0 · synced at height 328500",
            "Version  v0.0.1-alpha · auto-update off · v0.0.2 is available · press u to install",
            "Chain    mini share chain · no peers (its own share chain)",
            &format!("Devices  {}", view.device_url),
            "1 restart",
            "Stratum V2 listening on",
            "u install update",
        ] {
            assert!(running.contains(expected), "{expected}: {running}");
        }
        let down = text(&Shown {
            view: &view,
            state: State::Restarting {
                reason: "the BCH node is not ready: no answer".into(),
                in_secs: 9,
            },
            running_for: None,
            restarts: 2,
            node: Some(NodeCheck::NoAnswer("connection refused".into())),
            update: &Update::UpToDate,
            auto_update: true,
            log: vec![],
            status: "",
        });
        assert!(down.contains(
            "P2Pool   ● stopped (the BCH node is not ready: no answer); starting again in 9s"
        ));
        assert!(down.contains("Node     ● no answer (connection refused)"));
        assert!(down.contains("auto-update on · up to date"));
        assert_eq!(node_light(None).0, Light::Grey);
        assert_eq!(duration_text(Duration::from_secs(3_725)), "1h 02m");
    }
}
