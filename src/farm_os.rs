//! #### PR #42
//! Farm operating systems (HiveOS, mmpOS, RaveOS) run a miner through small
//! scripts: they start it with the flight sheet's pool, user and password,
//! and read its statistics for their dashboards. `pickaxe farm-os mine`
//! maps those flight-sheet fields onto Pickaxe's own flags, and
//! `pickaxe farm-os stats --os NAME` maps the status file `mine watch`
//! reads onto each system's format. One contract serves all three.

use serde_json::{json, Value};

/// A farm operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FarmOs {
    Hiveos,
    Mmpos,
    Raveos,
}

/// A status older than this counts as stopped (the miner writes every one
/// or two seconds).
pub const STALE_SECS: u64 = 30;

/// One GPU as the farm systems count it.
#[derive(Debug, Clone, PartialEq)]
struct Gpu {
    /// The PCI bus, when the GPU reports one.
    bus: Option<u64>,
    /// Hashes a second: the GPU's recent rate in candidates a second.
    rate: f64,
    temperature: Option<f64>,
    fan: Option<f64>,
    accepted: u64,
    rejected: u64,
}

/// What a status gives the farm systems: its GPUs, the accepted and
/// rejected winners, the uptime and the version; nothing when it is stale
/// or missing.
#[derive(Debug, Default)]
struct Reading {
    gpus: Vec<Gpu>,
    accepted: u64,
    rejected: u64,
    uptime: u64,
    version: String,
}

fn read(status: Option<&Value>, now: u64) -> Reading {
    let Some(status) = status else {
        return Reading::default();
    };
    let number = |value: &Value, key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
    let float = |value: &Value, key: &str| value.get(key).and_then(Value::as_f64);
    let version = status
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let updated = number(status, "updated");
    let fresh = now.saturating_sub(updated) <= STALE_SECS;
    // A coordinator reports its own GPUs only; each rig reports itself, so
    // farm totals never count a rig twice.
    let gpus = status
        .get("gpus")
        .and_then(Value::as_array)
        .map(|gpus| {
            gpus.iter()
                .map(|gpu| {
                    let telemetry = gpu.get("gpu_telemetry").unwrap_or(&Value::Null);
                    Gpu {
                        bus: gpu.get("pci_bus").and_then(Value::as_u64),
                        rate: if fresh {
                            float(gpu, "active_rate").unwrap_or(0.0).max(0.0)
                        } else {
                            0.0
                        },
                        temperature: float(telemetry, "temperature_c"),
                        fan: float(telemetry, "fan_percent"),
                        accepted: number(gpu, "winners"),
                        rejected: number(gpu, "rejected"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Reading {
        gpus,
        accepted: number(status, "verified_winners"),
        rejected: number(status, "rejected_winners"),
        uptime: if fresh {
            updated.saturating_sub(number(status, "started"))
        } else {
            0
        },
        version,
    }
}

/// A whole number of degrees or percent, 0 when unknown.
fn whole(value: Option<f64>) -> u64 {
    value
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or(0, |value| value.round() as u64)
}

/// HiveOS's two lines: the total in kH/s, then `$stats` (`hs` in H/s with
/// `hs_units`, `temp`, `fan`, `uptime`, `ver`, `ar` accepted and rejected,
/// `bus_numbers`, `null` for a GPU without a bus).
pub fn hiveos_stats(status: Option<&Value>, now: u64) -> String {
    let reading = read(status, now);
    let total: f64 = reading.gpus.iter().map(|gpu| gpu.rate).sum();
    let stats = json!({
        "hs": reading.gpus.iter().map(|gpu| gpu.rate).collect::<Vec<_>>(),
        "hs_units": "hs",
        "temp": reading.gpus.iter().map(|gpu| whole(gpu.temperature)).collect::<Vec<_>>(),
        "fan": reading.gpus.iter().map(|gpu| whole(gpu.fan)).collect::<Vec<_>>(),
        "uptime": reading.uptime,
        "ver": reading.version,
        "ar": [reading.accepted, reading.rejected],
        "bus_numbers": reading.gpus.iter().map(|gpu| gpu.bus).collect::<Vec<_>>(),
    });
    format!("{:.3}\n{stats}", total / 1000.0)
}

/// mmpOS's one line: `busid` and `hash` of the same length (a GPU without
/// a bus gives its position), `units`, `air` (accepted, invalid,
/// rejected) and per-GPU `shares`.
pub fn mmpos_stats(status: Option<&Value>, now: u64) -> String {
    let reading = read(status, now);
    json!({
        "busid": reading
            .gpus
            .iter()
            .enumerate()
            .map(|(position, gpu)| gpu.bus.unwrap_or(position as u64))
            .collect::<Vec<_>>(),
        "hash": reading.gpus.iter().map(|gpu| gpu.rate).collect::<Vec<_>>(),
        "units": "hs",
        "air": [reading.accepted, 0, reading.rejected],
        "miner_name": "pickaxe",
        "miner_version": reading.version,
        "shares": {
            "accepted": reading.gpus.iter().map(|gpu| gpu.accepted).collect::<Vec<_>>(),
            "rejected": reading.gpus.iter().map(|gpu| gpu.rejected).collect::<Vec<_>>(),
            "invalid": vec![0; reading.gpus.len()],
        },
    })
    .to_string()
}

/// RaveOS's one line, which its `stats.py` maps onto its GPU entries by
/// PCI bus; a GPU without a bus is left out rather than given another's.
pub fn raveos_stats(status: Option<&Value>, now: u64) -> String {
    let reading = read(status, now);
    json!({
        "gpus": reading
            .gpus
            .iter()
            .filter_map(|gpu| {
                gpu.bus.map(|bus| {
                    json!({
                        "pci_bus": bus,
                        "hash_rate": gpu.rate,
                        "temp": whole(gpu.temperature),
                        "fan": whole(gpu.fan),
                        "accepted": gpu.accepted,
                        "rejected": gpu.rejected,
                    })
                })
            })
            .collect::<Vec<_>>(),
        "accepted": reading.accepted,
        "rejected": reading.rejected,
        "invalid": 0,
    })
    .to_string()
}

/// What `pickaxe farm-os stats --os NAME` prints for a status.
pub fn stats(os: FarmOs, status: Option<&Value>, now: u64) -> String {
    match os {
        FarmOs::Hiveos => hiveos_stats(status, now),
        FarmOs::Mmpos => mmpos_stats(status, now),
        FarmOs::Raveos => raveos_stats(status, now),
    }
}

/// #### PR #42: `pickaxe farm-os mine` with a flight sheet's fields
/// What: the farm systems start the miner with `--pool`, `--user`,
/// `--password`, `--coin`, `--api-port` and `--pool-protocol` (any of them,
/// in any order); they become `pickaxe mine --no-tui --config P` with the
/// matching Pickaxe flags, and everything else passes through as given. An
/// empty pool (or solo, auto, fulcrum) mines from the Fulcrum list; a
/// coordinator's one-line address (or HOST:PORT with its key as the
/// password) mines as a rig; http(s) is a node; ws, wss or tcp a Fulcrum
/// server; an SV1 pool is refused, since Pickaxe mines PHOTON on GPUs.
/// Why: one mapping for all three systems, tested here, instead of three
/// shell scripts.
/// Look here if: a flight sheet's pool is not used, or a farm system's
/// extras stop reaching Pickaxe.
/// The argv after `pickaxe farm-os mine` (`raw`), as Pickaxe's own.
pub fn mine_argv(raw: &[String]) -> Result<Vec<String>, String> {
    let mut pools: Vec<String> = Vec::new();
    let mut user: Option<String> = None;
    let mut password: Option<String> = None;
    let mut config: Option<String> = None;
    let mut extras: Vec<String> = Vec::new();
    let mut args = raw.iter();
    let mut ended = false;
    while let Some(arg) = args.next() {
        if ended {
            extras.push(arg.clone());
            continue;
        }
        let (key, inline) = match arg.split_once('=') {
            Some((key, value)) if key.starts_with("--") => (key, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || -> Result<String, String> {
            inline
                .clone()
                .or_else(|| args.next().cloned())
                .ok_or_else(|| format!("{key} needs a value"))
        };
        match key {
            "--" => ended = true,
            "--pool" => pools.extend(value()?.split_whitespace().map(str::to_owned)),
            "--user" => user = Some(value()?),
            "--password" => password = Some(value()?),
            "--config" => config = Some(value()?),
            "--coin" | "--api-port" | "--pool-protocol" => {
                value()?;
            }
            _ => extras.push(arg.clone()),
        }
    }
    let mut argv: Vec<String> = ["pickaxe", "mine", "--no-tui"].map(str::to_owned).to_vec();
    if let Some(config) = config {
        argv.extend(["--config".to_owned(), config]);
    }
    let password = password.filter(|password| !password.trim().is_empty());
    let mut rig = false;
    for pool in &pools {
        let lower = pool.to_ascii_lowercase();
        // #### PR #42: mmpOS sends "solo:1" for a coin without a pool.
        let host = match lower.split_once(':') {
            Some((host, _)) if !lower.contains("://") => host,
            _ => lower.as_str(),
        };
        if matches!(host, "" | "solo" | "auto" | "fulcrum") {
            continue;
        }
        if lower.starts_with("stratum+tcp://") || lower.starts_with("stratum+ssl://") {
            return Err(
                "that is an SV1 pool; Pickaxe mines PHOTON on GPUs: leave the pool empty, or give \
                 a Pickaxe coordinator stratum2+tcp://HOST:3340/KEY"
                    .into(),
            );
        }
        if lower.starts_with("stratum2+tcp://") {
            argv.extend(["--coordinator".to_owned(), pool.clone()]);
            rig = true;
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            argv.extend(["--node-rpc".to_owned(), pool.clone()]);
        } else if lower.starts_with("ws://")
            || lower.starts_with("wss://")
            || lower.starts_with("tcp://")
        {
            argv.extend(["--fulcrum".to_owned(), pool.clone()]);
        } else if pool.contains(':') && !pool.contains("://") {
            // mmpOS strips the scheme: HOST:PORT with the coordinator's key
            // as the password.
            let key = password
                .as_deref()
                .filter(|key| crate::rigs::is_coordinator_key(key))
                .ok_or(
                    "give the coordinator's key as the password, or use \
                     stratum2+tcp://HOST:PORT/KEY",
                )?;
            argv.extend([
                "--coordinator".to_owned(),
                pool.clone(),
                "--coordinator-key".to_owned(),
                key.to_owned(),
            ]);
            rig = true;
        } else {
            return Err(format!("Pickaxe cannot mine at {pool}"));
        }
    }
    // A CashAddr has no dot, so ADDRESS.WORKER splits safely.
    if let Some(user) = user.filter(|user| !user.trim().is_empty()) {
        let (address, worker) = match user.split_once('.') {
            Some((address, worker)) => (address.to_owned(), Some(worker.to_owned())),
            None => (user, None),
        };
        if !address.trim().is_empty() {
            argv.extend(["--address".to_owned(), address]);
        }
        if let Some(worker) = worker.filter(|worker| rig && !worker.trim().is_empty()) {
            argv.extend(["--rig-name".to_owned(), worker]);
        }
    }
    argv.extend(extras);
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// A coordinator key that decodes (version 1, an x-only key).
    #[cfg(feature = "stratum-v2")]
    fn key() -> String {
        let mut encoded = vec![1, 0];
        encoded.extend([7u8; 32]);
        stratum_core::bitcoin::base58::encode_check(&encoded)
    }

    // #### PR #42
    // What: every pool form a flight sheet may hold maps to Pickaxe's flags,
    // failover pools in order; the user's worker names a rig; mmpOS's
    // HOST:PORT takes the key from the password; farm extras pass through.
    // Look here if: mine_argv changes.
    #[cfg(feature = "stratum-v2")]
    #[test]
    fn mine_argv_maps_every_pool_form() {
        let key = key();
        let one = format!("stratum2+tcp://192.168.0.10:3340/{key}");
        let backup = format!("stratum2+tcp://192.168.0.11:3340/{key}");
        let argv = mine_argv(&strings(&[
            "--pool",
            &format!("{one} {backup}"),
            "--user",
            "bitcoincash:qpaddress.rack-7",
            "--intensity",
            "90",
        ]))
        .unwrap();
        assert_eq!(
            argv,
            strings(&[
                "pickaxe",
                "mine",
                "--no-tui",
                "--coordinator",
                &one,
                "--coordinator",
                &backup,
                "--address",
                "bitcoincash:qpaddress",
                "--rig-name",
                "rack-7",
                "--intensity",
                "90",
            ])
        );
        let mmpos = mine_argv(&strings(&[
            "--coin",
            "PHOTON",
            "--pool",
            "h.example:3340",
            "--user",
            "a.rig",
            "--password",
            &key,
            "--api-port",
            "0",
            "--intensity",
            "90",
        ]))
        .unwrap();
        assert_eq!(
            mmpos,
            strings(&[
                "pickaxe",
                "mine",
                "--no-tui",
                "--coordinator",
                "h.example:3340",
                "--coordinator-key",
                &key,
                "--address",
                "a",
                "--rig-name",
                "rig",
                "--intensity",
                "90",
            ])
        );
        for (pool, flag) in [
            ("http://user:pass@127.0.0.1:8332", "--node-rpc"),
            ("wss://f.example:50004", "--fulcrum"),
            ("tcp://umbrel.local:50001", "--fulcrum"),
        ] {
            let argv = mine_argv(&strings(&["--pool", pool, "--config", "/hive/p.json"])).unwrap();
            assert_eq!(
                argv,
                strings(&[
                    "pickaxe",
                    "mine",
                    "--no-tui",
                    "--config",
                    "/hive/p.json",
                    flag,
                    pool
                ])
            );
        }
        // No pool, solo or auto: the Fulcrum list; a worker without a rig is
        // dropped; `--` ends the farm keys.
        let solo = mine_argv(&strings(&[
            "--pool=solo",
            "--user",
            "bitcoincash:qp.worker",
            "--password",
            "",
            "--",
            "--pool",
            "x",
        ]))
        .unwrap();
        assert_eq!(
            solo,
            strings(&[
                "pickaxe",
                "mine",
                "--no-tui",
                "--address",
                "bitcoincash:qp",
                "--pool",
                "x"
            ])
        );
    }

    // #### PR #42
    #[test]
    fn mine_argv_refuses_sv1_pools_and_keyless_host_port() {
        let error =
            mine_argv(&strings(&["--pool", "stratum+tcp://pool.example:3333"])).unwrap_err();
        assert!(error.contains("SV1 pool"), "{error}");
        let error =
            mine_argv(&strings(&["--pool", "h.example:3340", "--password", "x"])).unwrap_err();
        assert!(
            error.contains("coordinator's key as the password"),
            "{error}"
        );
        assert!(mine_argv(&strings(&["--pool"])).is_err());
        // mmpOS's way of saying no pool.
        assert_eq!(
            mine_argv(&strings(&["--pool", "solo:1", "--user", "a.rig"])).unwrap(),
            strings(&["pickaxe", "mine", "--no-tui", "--address", "a"])
        );
    }

    /// A status with two GPUs on buses 1 and 101, 5 winners and 1 rejected.
    fn status(role: &str, updated: u64) -> Value {
        json!({
            "event": "status", "role": role, "version": "0.0.4", "started": updated - 600,
            "updated": updated, "verified_winners": 5, "rejected_winners": 1,
            "gpus": [
                {"pci_bus": 1, "active_rate": 1.45e9, "winners": 3, "rejected": 1,
                    "gpu_telemetry": {"temperature_c": 61.2, "fan_percent": 40.0}},
                {"pci_bus": 101, "active_rate": 2.9e7, "winners": 0, "rejected": 0,
                    "gpu_telemetry": {}},
            ],
        })
    }

    // #### PR #42
    // What: the same status reads as HiveOS's two lines, mmpOS's line and
    // RaveOS's line, for a miner, a coordinator and a rig alike.
    // Look here if: a farm system's format changes.
    #[test]
    fn hiveos_mmpos_raveos_stats_match_their_formats() {
        for role in ["miner", "coordinator", "rig"] {
            let status = status(role, 1_000_000);
            let now = 1_000_005;
            assert_eq!(
                stats(FarmOs::Hiveos, Some(&status), now),
                "1479000.000\n{\"ar\":[5,1],\"bus_numbers\":[1,101],\"fan\":[40,0],\
                 \"hs\":[1450000000.0,29000000.0],\"hs_units\":\"hs\",\"temp\":[61,0],\
                 \"uptime\":600,\"ver\":\"0.0.4\"}"
            );
            assert_eq!(
                stats(FarmOs::Mmpos, Some(&status), now),
                "{\"air\":[5,0,1],\"busid\":[1,101],\"hash\":[1450000000.0,29000000.0],\
                 \"miner_name\":\"pickaxe\",\"miner_version\":\"0.0.4\",\"shares\":{\
                 \"accepted\":[3,0],\"invalid\":[0,0],\"rejected\":[1,0]},\"units\":\"hs\"}"
            );
            assert_eq!(
                stats(FarmOs::Raveos, Some(&status), now),
                "{\"accepted\":5,\"gpus\":[{\"accepted\":3,\"fan\":40,\"hash_rate\":1450000000.0,\
                 \"pci_bus\":1,\"rejected\":1,\"temp\":61},{\"accepted\":0,\"fan\":0,\
                 \"hash_rate\":29000000.0,\"pci_bus\":101,\"rejected\":0,\"temp\":0}],\
                 \"invalid\":0,\"rejected\":1}"
            );
        }
    }

    // #### PR #42
    // What: a stale status reports no rate and no uptime, and a missing one
    // nothing at all.
    #[test]
    fn a_stale_or_missing_status_reports_zero() {
        let status = status("miner", 1_000_000);
        let stale = stats(FarmOs::Hiveos, Some(&status), 1_000_000 + STALE_SECS + 1);
        assert!(stale.starts_with("0.000\n"), "{stale}");
        assert!(stale.contains("\"uptime\":0"), "{stale}");
        assert!(stale.contains("\"hs\":[0.0,0.0]"), "{stale}");
        assert_eq!(
            stats(FarmOs::Mmpos, None, 0),
            "{\"air\":[0,0,0],\"busid\":[],\"hash\":[],\"miner_name\":\"pickaxe\",\
             \"miner_version\":\"\",\"shares\":{\"accepted\":[],\"invalid\":[],\
             \"rejected\":[]},\"units\":\"hs\"}"
        );
    }

    // #### PR #42
    // What: a GPU without a PCI bus is null for HiveOS, its position for
    // mmpOS and left out for RaveOS; it never takes another GPU's bus.
    #[test]
    fn gpus_without_pci_never_take_another_bus() {
        let mut status = status("miner", 1_000_000);
        status["gpus"][0].as_object_mut().unwrap().remove("pci_bus");
        let hive = stats(FarmOs::Hiveos, Some(&status), 1_000_000);
        assert!(hive.contains("\"bus_numbers\":[null,101]"), "{hive}");
        let mmpos = stats(FarmOs::Mmpos, Some(&status), 1_000_000);
        assert!(mmpos.contains("\"busid\":[0,101]"), "{mmpos}");
        let rave = stats(FarmOs::Raveos, Some(&status), 1_000_000);
        assert!(!rave.contains("\"pci_bus\":1,"), "{rave}");
        assert!(rave.contains("\"pci_bus\":101"), "{rave}");
    }
}
