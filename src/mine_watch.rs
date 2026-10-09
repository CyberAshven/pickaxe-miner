//! #### PR #40
//! A read-only view of a miner running without a screen (a GPU farm's or
//! pool's coordinator as a service, or any `--no-tui` miner): it saves its
//! status once a second beside its config, and `pickaxe watch` shows it, as
//! `stratum-v2 watch` does for the ASIC server. The saved status holds no
//! payout address and never connects back to the miner.

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    widgets::{Block, Paragraph, Row, Table, Wrap},
    Frame, Terminal,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The status file beside a miner's config (`mainnet.json` keeps
/// `mainnet.mine-status.json`).
pub fn status_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("mine-status.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Saves a status line whole (written beside, then renamed), stamped with
/// the time and without the payout address. A failed save never stops mining.
pub fn save(path: &Path, status: &Value) {
    let mut status = status.clone();
    if let Some(object) = status.as_object_mut() {
        object.remove("payout_address");
        object.insert("updated".into(), Value::from(unix_now()));
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{:016x}", rand::random::<u64>()));
    let temp = PathBuf::from(temp);
    if std::fs::write(&temp, status.to_string()).is_ok() && std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

fn rate(value: Option<&Value>) -> String {
    crate::telemetry::format_hash_rate(value.and_then(Value::as_f64).unwrap_or(0.0))
}

fn number(status: &Value, key: &str) -> u64 {
    status.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The header and the table rows the view shows for a saved status, `now`
/// being the current time in seconds.
fn view(status: &Value, now: u64) -> (String, Vec<[String; 5]>, [&'static str; 5]) {
    let age = now.saturating_sub(number(status, "updated"));
    let freshness = if age > 5 {
        format!("Miner not updating; last status {age}s ago")
    } else {
        "Live".to_owned()
    };
    let rigs = status.get("rigs").filter(|rigs| !rigs.is_null());
    let own = status
        .get("current_rate")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let farm = rigs
        .and_then(|rigs| rigs.get("rate"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let mut header = format!(
        "PHOTON · {} · Height {} · {} · Winners {} (rejected {}) · Source {} · {freshness}",
        status
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("waiting"),
        number(status, "height"),
        crate::telemetry::format_hash_rate(own + farm),
        number(status, "verified_winners"),
        number(status, "rejected_winners"),
        status
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("—"),
    );
    let Some(rigs) = rigs else {
        let gpus = status
            .get("gpus")
            .and_then(Value::as_array)
            .map(|gpus| {
                gpus.iter()
                    .map(|gpu| {
                        [
                            gpu.get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("GPU")
                                .to_owned(),
                            gpu.get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("—")
                                .to_owned(),
                            rate(gpu.get("rate")),
                            gpu.get("winners")
                                .and_then(Value::as_u64)
                                .unwrap_or(0)
                                .to_string(),
                            gpu.get("last_error")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        ]
                    })
                    .collect()
            })
            .unwrap_or_default();
        return (
            header,
            gpus,
            ["GPU", "Status", "Rate", "Winners", "Last error"],
        );
    };
    header.push_str(&format!(
        "\nRigs {} connected · {} GPUs · {} · winners {} (rejected {}) · listening on {}",
        number(rigs, "connected"),
        number(rigs, "gpus"),
        rate(rigs.get("rate")),
        number(rigs, "winners"),
        number(rigs, "rejected"),
        rigs.get("listen").and_then(Value::as_str).unwrap_or("—"),
    ));
    let rows = rigs
        .get("rigs")
        .and_then(Value::as_array)
        .map(|each| {
            each.iter()
                .map(|rig| {
                    let connected = number(rig, "connected_secs") + age;
                    [
                        rig.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("rig")
                            .to_owned(),
                        number(rig, "gpus").to_string(),
                        rate(rig.get("rate")),
                        number(rig, "winners").to_string(),
                        format!("{}m", connected / 60),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    (
        header,
        rows,
        ["Rig", "GPUs", "Rate", "Winners", "Connected"],
    )
}

fn render(frame: &mut Frame<'_>, header: &str, rows: &[[String; 5]], titles: [&str; 5]) {
    let areas = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(header)
            .block(Block::bordered().title("Pickaxe · watch (read-only)"))
            .wrap(Wrap { trim: false }),
        areas[0],
    );
    let table = Table::new(
        rows.iter().map(|row| Row::new(row.clone())),
        [
            Constraint::Percentage(30),
            Constraint::Percentage(12),
            Constraint::Percentage(18),
            Constraint::Percentage(12),
            Constraint::Percentage(28),
        ],
    )
    .header(Row::new(titles))
    .block(Block::bordered());
    frame.render_widget(table, areas[1]);
    frame.render_widget(
        Paragraph::new("q  Leave (the miner keeps running)"),
        areas[2],
    );
}

/// Shows the status a miner without a screen saves, until `q`.
pub fn run(path: &Path) -> Result<(), String> {
    enable_raw_mode().map_err(|_| "cannot open the terminal")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|_| "cannot open the terminal")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(stdout)).map_err(|_| "cannot open the terminal")?;
    let result = (|| loop {
        let status = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let (header, rows, titles) = match &status {
            Some(status) => view(status, unix_now()),
            None => (
                format!(
                    "No status at {}. A miner saves it when it runs without a screen (--no-tui) with this config.",
                    path.display()
                ),
                Vec::new(),
                ["", "", "", "", ""],
            ),
        };
        terminal
            .draw(|frame| render(frame, &header, &rows, titles))
            .map_err(|_| "cannot draw the watch view")?;
        if event::poll(Duration::from_millis(1000)).map_err(|_| "cannot read the terminal")? {
            if let Event::Key(key) = event::read().map_err(|_| "cannot read the terminal")? {
                let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL));
                if key.kind == KeyEventKind::Press && quit {
                    return Ok(());
                }
            }
        }
    })();
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_coordinators_saved_status_shows_its_farm_and_each_rig() {
        let dir =
            std::env::temp_dir().join(format!("pickaxe-mine-watch-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = status_path(&dir.join("mainnet.json"));
        assert!(path.ends_with("mainnet.mine-status.json"));
        save(
            &path,
            &json!({
                "event": "status", "state": "mining", "height": 327034,
                "endpoint": "http://***@127.0.0.1:48342", "payout_address": "secret",
                "current_rate": 0.0, "verified_winners": 95, "rejected_winners": 0,
                "rigs": {"listen": "0.0.0.0:3340", "connected": 2, "gpus": 2,
                    "rate": 1.55e9, "winners": 95, "rejected": 0,
                    "rigs": [{"name": "pool-test-nvidia", "gpus": 1, "rate": 1.52e9,
                        "winners": 94, "connected_secs": 600}]},
            }),
        );
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.get("payout_address").is_none(), "no payout is saved");
        let now = saved["updated"].as_u64().unwrap();
        let (header, rows, titles) = view(&saved, now);
        for part in [
            "mining",
            "Height 327034",
            "1.55 GH/s",
            "Winners 95",
            "Live",
            "Rigs 2 connected",
            "0.0.0.0:3340",
        ] {
            assert!(header.contains(part), "{part}: {header}");
        }
        assert_eq!(titles[0], "Rig");
        assert_eq!(rows[0][0], "pool-test-nvidia");
        assert_eq!(rows[0][3], "94");
        assert_eq!(rows[0][4], "10m");
        // A stale status says so, and connected times count on.
        let (header, rows, _) = view(&saved, now + 60);
        assert!(header.contains("Miner not updating"), "{header}");
        assert_eq!(rows[0][4], "11m");
        // A miner without rigs lists its GPUs.
        let (_, rows, titles) = view(
            &json!({"state": "mining", "updated": now, "gpus": [
                {"name": "RTX", "status": "mining", "rate": 1.0e9, "winners": 3}]}),
            now,
        );
        assert_eq!(
            (titles[0], rows[0][0].as_str(), rows[0][3].as_str()),
            ("GPU", "RTX", "3")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
