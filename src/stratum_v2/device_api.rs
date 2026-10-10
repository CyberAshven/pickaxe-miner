//! #### PR #38
//! Read-only device reports for the workers table: what the miner itself says
//! about its hash rate, temperature and fans. Only standard read commands are
//! sent (CGMiner API `summary` and `estats` on port 4028; Bitaxe's
//! `/api/system/info`); nothing here changes a device setting. Only devices
//! on the local network are asked, and each device gets one time limit for
//! its whole report so a slow one cannot hold up the others or shutdown.
//!
//! #### PR #40
//! This is now the extension to asic-rs (see `fleet`): it answers for devices
//! asic-rs cannot identify, supplies the windowed CGMiner rate and Avalon's
//! hottest reading, and sends Canaan's work-level commands.

use serde::Serialize;
use serde_json::Value;
use std::{
    io::{ErrorKind, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};

/// Time allowed for one device's whole report: connecting, asking, reading.
const DEADLINE: Duration = Duration::from_secs(3);
const CONNECT: Duration = Duration::from_millis(1500);
const MAX_REPLY: usize = 256 * 1024;

/// What a device reports about itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeviceReport {
    /// The device's own hash rate, in hashes per second.
    pub hashrate: Option<f64>,
    /// Hottest reported temperature, in degrees Celsius.
    pub temperature_c: Option<f64>,
    /// Fan speed as the device gives it: "3200 rpm" or "45%".
    pub fan: Option<String>,
    /// Make and model, as asic-rs identifies them.
    pub model: Option<String>,
    /// Firmware version the device reports.
    pub firmware: Option<String>,
    /// Power draw the device reports, in watts.
    pub power_w: Option<f64>,
}

impl DeviceReport {
    pub fn is_empty(&self) -> bool {
        self.hashrate.is_none()
            && self.temperature_c.is_none()
            && self.fan.is_none()
            && self.model.is_none()
            && self.firmware.is_none()
            && self.power_w.is_none()
    }
}

/// Whether a device at this address may be asked for its report: private,
/// link-local and shared (100.64.0.0/10, used by Tailscale) IPv4 ranges, and
/// IPv6 unique-local and link-local ranges. Loopback is the SV1 adapter's own
/// link. Public addresses are never asked: a miner reaching the server over
/// the internet shows its router's address, not the miner's.
pub fn queryable(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            ip.is_private() || ip.is_link_local() || (first == 100 && second & 0xc0 == 0x40)
        }
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

/// Asks a device on the local network for its own report: first the CGMiner
/// API used by Avalon and most SHA-256 miners, then Bitaxe's web API.
pub fn poll(ip: IpAddr) -> Option<DeviceReport> {
    if !queryable(ip) {
        return None;
    }
    let ip = ip.to_canonical();
    let deadline = Instant::now() + DEADLINE;
    cgminer(ip, deadline).or_else(|| bitaxe(ip, deadline))
}

/// Sends one request and reads until `complete` accepts the reply, the device
/// closes the connection, or the deadline passes.
fn exchange(
    address: SocketAddr,
    request: &[u8],
    deadline: Instant,
    complete: fn(&[u8]) -> bool,
) -> Option<String> {
    let left = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    };
    let mut stream = TcpStream::connect_timeout(&address, left()?.min(CONNECT)).ok()?;
    stream.set_write_timeout(Some(left()?)).ok()?;
    stream.write_all(request).ok()?;
    let mut reply = Vec::new();
    let mut chunk = [0; 4096];
    while !complete(&reply) {
        stream.set_read_timeout(Some(left()?)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                reply.extend_from_slice(&chunk[..read]);
                if reply.len() > MAX_REPLY {
                    return None;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => (),
            Err(_) => return None,
        }
    }
    Some(String::from_utf8_lossy(&reply).into_owned())
}

fn cgminer(ip: IpAddr, deadline: Instant) -> Option<DeviceReport> {
    // Each CGMiner API reply ends in a NUL byte.
    let reply = exchange(
        SocketAddr::new(ip, 4028),
        br#"{"command":"summary+estats"}"#,
        deadline,
        |reply| reply.contains(&0),
    )?;
    parse_cgminer(&reply)
}

fn bitaxe(ip: IpAddr, deadline: Instant) -> Option<DeviceReport> {
    let host = match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let request =
        format!("GET /api/system/info HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let reply = exchange(
        SocketAddr::new(ip, 80),
        request.as_bytes(),
        deadline,
        http_complete,
    )?;
    let body = reply.split_once("\r\n\r\n")?.1;
    parse_bitaxe(body)
}

/// An HTTP reply is whole once its headers and its `Content-Length` body
/// arrived; without that header, the device closing the connection ends it.
fn http_complete(reply: &[u8]) -> bool {
    let Some(end) = reply.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    String::from_utf8_lossy(&reply[..end])
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, length)| length.trim().parse::<usize>().ok())
        .is_some_and(|length| reply.len() >= end + 4 + length)
}

/// #### PR #40
/// What the user may ask a device to do from the workers page, each only
/// after confirming it there. asic-rs sends what it supports for the device's
/// make and firmware (see `fleet`); Pickaxe's own commands follow Canaan's
/// CGMiner fork (`ascset` with `reboot` and `worklevel`) and Bitaxe's restart
/// endpoint. The device checks every value itself and replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceAction {
    Restart,
    Pause,
    Resume,
    LocateOn,
    LocateOff,
    LowerPower,
    RaisePower,
    /// #### PR #42: the fan follows the device's temperature.
    FanAuto,
    /// #### PR #42: the fan at a fixed percentage.
    FanPercent(u8),
    /// #### PR #42: a power limit in watts.
    PowerWatts(u32),
    /// #### PR #42: a named power mode.
    PowerMode(PowerMode),
}

impl DeviceAction {
    /// The actions Pickaxe's own code can send, for devices asic-rs has not
    /// identified.
    pub const OWN: [Self; 3] = [Self::Restart, Self::LowerPower, Self::RaisePower];

    pub fn label(self) -> String {
        match self {
            Self::Restart => "Restart".into(),
            Self::Pause => "Pause mining".into(),
            Self::Resume => "Resume mining".into(),
            Self::LocateOn => "Blink its light (to find it)".into(),
            Self::LocateOff => "Stop blinking its light".into(),
            Self::LowerPower => "Lower power (one work level down)".into(),
            Self::RaisePower => "Raise power (one work level up)".into(),
            Self::FanAuto => "Fan speed: automatic".into(),
            Self::FanPercent(percent) => format!("Fan speed: {percent}%"),
            Self::PowerWatts(watts) => format!("Power limit: {watts} W"),
            Self::PowerMode(mode) => format!("Power mode: {}", mode.name()),
        }
    }
}

/// #### PR #42: a power mode, for firmwares that name their modes: asic-rs's
/// mining modes (stock Antminer, WhatsMiner) and Canaan's work modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerMode {
    Low,
    Normal,
    High,
}

impl PowerMode {
    pub const ALL: [Self; 3] = [Self::Low, Self::Normal, Self::High];

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Normal => "Normal",
            Self::High => "High",
        }
    }

    /// Canaan's work mode number (0 Eco, 1 Standard, 2 Super on the Avalon
    /// Q).
    fn avalon(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
        }
    }
}

/// Sends one of Pickaxe's own confirmed actions to a device on the local
/// network and returns the device's own reply.
pub fn control(ip: IpAddr, action: DeviceAction) -> Result<String, String> {
    if !queryable(ip) {
        return Err("only devices on the local network can be controlled".into());
    }
    let ip = ip.to_canonical();
    control_at(SocketAddr::new(ip, 4028), SocketAddr::new(ip, 80), action)
}

/// One of Pickaxe's own actions, sent to the device's CGMiner API and, for a
/// restart it refuses, its web API (Bitaxe). Restart is Canaan's documented
/// reboot, `ascset 0,reboot,0`, on every Avalon (PR #42, see `fleet`).
pub(super) fn control_at(
    cgminer: SocketAddr,
    web: SocketAddr,
    action: DeviceAction,
) -> Result<String, String> {
    match action {
        DeviceAction::Restart => {
            ascset(cgminer, "0,reboot,0").or_else(|error| bitaxe_restart(web).map_err(|_| error))
        }
        DeviceAction::LowerPower => adjust_level(cgminer, false),
        DeviceAction::RaisePower => adjust_level(cgminer, true),
        DeviceAction::Pause
        | DeviceAction::Resume
        | DeviceAction::LocateOn
        | DeviceAction::LocateOff
        | DeviceAction::FanAuto
        | DeviceAction::FanPercent(_)
        | DeviceAction::PowerWatts(_)
        | DeviceAction::PowerMode(_) => Err(NOT_OFFERED.into()),
    }
}

const NOT_OFFERED: &str = "this device does not offer that action";

/// The answer when an Avalon's pools need its web login and none is known.
pub const WEB_LOGIN_NEEDED: &str = "its web login is needed to change its pools";

/// #### PR #42
/// What: whether a device's or asic-rs's error says the login was refused,
/// so the Device panel asks for the device's own login.
/// Why: firmwares word it differently: HTTP 401 or 403, Canaan's "username
/// err" and "userpass err", WhatsMiner's failed decryption with a wrong
/// password.
/// Look here if: a refused login is not followed by the login page, or an
/// unrelated error is.
pub fn login_refused(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "status code 401",
        "status code 403",
        "unauthorized",
        "forbidden",
        "authentication failed",
        "username err",
        "userpass err",
        "invalid token",
        "aes decryption failed",
        "wrong password",
        "invalid password",
        WEB_LOGIN_NEEDED,
    ]
    .iter()
    .any(|sign| text.contains(sign))
}
const NO_ANSWER: &str = "the device did not answer";

// #### PR #42: fan and power settings through Pickaxe's own commands
// What: Canaan's `ascset 0,fan-spd` (automatic is -1, otherwise 15% to 100%)
// and `ascset 0,workmode,set` for Avalons, and AxeOS's settings for a Bitaxe
// or NerdAxe (`PATCH /api/system`). An Avalon's `ascset 0,help` lists which
// of these it accepts.
// Why: asic-rs sets fans only on stock Antminer, ePIC and Proto firmware, and
// sets no fan or power mode on these devices.
// Look here if: an Avalon or Bitaxe refuses a fan or power setting its panel
// offered, or a Bitaxe on Tailscale answers 401.
/// The API Pickaxe's own settings go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnApi {
    /// Canaan's CGMiner fork, port 4028.
    Avalon,
    /// Bitaxe's and NerdAxe's web API, port 80.
    AxeOs,
}

/// The fan percentages an Avalon accepts (`fan-spd`); -1 is automatic.
pub const AVALON_FAN: (u8, u8) = (15, 100);

/// The answer AxeOS gives (401) to a request from outside its local network.
const AXEOS_LOCAL_ONLY: &str = "the Bitaxe only accepts changes from its own local network \
     (10.x, 172.16-31.x or 192.168.x), not through Tailscale or a VPN";

/// Sends a fan or power setting through Pickaxe's own commands to a device
/// on the local network and returns the device's own reply.
pub fn setting(ip: IpAddr, api: OwnApi, action: DeviceAction) -> Result<String, String> {
    if !queryable(ip) {
        return Err("only devices on the local network can be controlled".into());
    }
    let ip = ip.to_canonical();
    setting_at(
        SocketAddr::new(ip, 4028),
        SocketAddr::new(ip, 80),
        api,
        action,
    )
}

pub(super) fn setting_at(
    cgminer: SocketAddr,
    web: SocketAddr,
    api: OwnApi,
    action: DeviceAction,
) -> Result<String, String> {
    match (api, action) {
        (OwnApi::Avalon, DeviceAction::FanAuto) => ascset(cgminer, "0,fan-spd,-1"),
        (OwnApi::Avalon, DeviceAction::FanPercent(percent)) => {
            let (low, high) = AVALON_FAN;
            if !(low..=high).contains(&percent) {
                return Err(format!(
                    "an Avalon's fan takes {low}% to {high}%; nothing was sent"
                ));
            }
            ascset(cgminer, &format!("0,fan-spd,{percent}"))
        }
        (OwnApi::Avalon, DeviceAction::PowerMode(mode)) => {
            ascset(cgminer, &format!("0,workmode,set,{}", mode.avalon()))
        }
        (OwnApi::AxeOs, DeviceAction::FanAuto) => axeos_fan(web, None),
        (OwnApi::AxeOs, DeviceAction::FanPercent(percent)) if percent <= 100 => {
            axeos_fan(web, Some(percent))
        }
        _ => Err(NOT_OFFERED.into()),
    }
}

/// The options an Avalon lists in its `ascset 0,help` reply, in lower case;
/// `None` when it does not answer or lists nothing readable. Read-only.
pub fn avalon_options(ip: IpAddr) -> Option<Vec<String>> {
    if !queryable(ip) {
        return None;
    }
    avalon_options_at(SocketAddr::new(ip.to_canonical(), 4028))
}

pub(super) fn avalon_options_at(cgminer: SocketAddr) -> Option<Vec<String>> {
    let message = ascset(cgminer, "0,help").ok()?;
    let options: Vec<String> = message
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    (!options.is_empty()).then_some(options)
}

/// Sets a Bitaxe's fan: automatic, or a fixed percentage under the key the
/// firmware reads (`manualFanSpeed` from AxeOS v2.12, `fanspeed` before).
fn axeos_fan(web: SocketAddr, percent: Option<u8>) -> Result<String, String> {
    let body = match percent {
        None => serde_json::json!({ "autofanspeed": 1 }),
        Some(percent) => {
            let info = axeos_request(web, "GET", "/api/system/info", None)?;
            let newer = serde_json::from_str::<Value>(&info)
                .ok()
                .is_some_and(|info| info.get("manualFanSpeed").is_some());
            let key = if newer { "manualFanSpeed" } else { "fanspeed" };
            let mut body = serde_json::json!({ "autofanspeed": 0 });
            body[key] = percent.into();
            body
        }
    };
    axeos_request(web, "PATCH", "/api/system", Some(&body.to_string()))?;
    Ok(match percent {
        None => "fan set to automatic".into(),
        Some(percent) => format!("fan set to {percent}%"),
    })
}

/// One AxeOS request; `Ok` carries the reply's body when its status is
/// 2xx.
fn axeos_request(
    web: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let host = match web.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let body = body.unwrap_or("");
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    let request = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let reply = exchange(
        web,
        request.as_bytes(),
        Instant::now() + DEADLINE,
        http_complete,
    )
    .ok_or(NO_ANSWER)?;
    let (head, body) = reply.split_once("\r\n\r\n").unwrap_or((&reply, ""));
    let status = head.lines().next().unwrap_or_default();
    match status
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
    {
        Some(200..=299) => Ok(body.to_owned()),
        Some(401) => Err(AXEOS_LOCAL_ONLY.into()),
        _ => Err(format!("the device answered {status}")),
    }
}
// #### end PR #42 ####

// #### PR #42: pools, through Pickaxe's own commands
// What: an Avalon's pools are read with CGMiner's `pools` and written with
// Canaan's `setpool` (its web login, slots 0 to 2, then a reboot); a Bitaxe's
// are read from and written to AxeOS (`stratumURL`, `fallbackStratumURL` and
// their ports and users, then a restart). A pool's address must carry its
// port, and an Avalon's fields may hold no comma, since `setpool` splits on
// commas.
// Why: asic-rs writes pools on most makes, but not on these.
// Look here if: an Avalon or Bitaxe gets the wrong pools, or a reply shows a
// worker's password.
/// One pool as a device holds it. Its password is never shown.
#[derive(Clone, PartialEq, Eq)]
pub struct PoolEntry {
    /// `stratum+tcp://host:port`, or with SV2 `stratum2+tcp://host:port/KEY`.
    pub url: String,
    pub user: String,
    /// The password written back; "x" where the firmware does not return it.
    pub password: String,
    /// Whether the device mines on it now.
    pub active: bool,
}

impl std::fmt::Debug for PoolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PoolEntry({}, password hidden)", self.url)
    }
}

impl PoolEntry {
    /// The pool's `host:port`, to compare entries and to find this server.
    pub fn host_port(&self) -> String {
        host_port(&self.url)
    }
}

/// The `host:port` of a pool address, without scheme or key.
pub fn host_port(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_ascii_lowercase()
}

/// A device's pools after a change: the new pool first, then the pools it
/// held (each once), as many as the device holds; the rest are listed as
/// dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolPlan {
    pub pools: Vec<PoolEntry>,
    pub dropped: Vec<PoolEntry>,
}

impl PoolPlan {
    pub fn new(current: &[PoolEntry], new: PoolEntry, slots: usize) -> Self {
        let mut pools = vec![new];
        for pool in current {
            if !pools
                .iter()
                .any(|kept| kept.host_port() == pool.host_port() && kept.user == pool.user)
            {
                pools.push(PoolEntry {
                    active: false,
                    ..pool.clone()
                });
            }
        }
        let dropped = pools.split_off(slots.clamp(1, pools.len()));
        Self { pools, dropped }
    }
}

/// Checks a pool address Pickaxe would write: an optional scheme
/// (`stratum+tcp`, `stratum+ssl`, `stratum2+tcp`), a host and an explicit
/// port (asic-rs would read a missing port as port 80), with no spaces.
pub fn check_pool_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() || url.len() > 255 || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("a pool address has no spaces and at most 255 characters".into());
    }
    if let Some((scheme, _)) = url.split_once("://") {
        if !["stratum+tcp", "stratum+ssl", "stratum2+tcp"].contains(&scheme) {
            return Err(format!(
                "a pool address starts with stratum+tcp://, not {scheme}://"
            ));
        }
    }
    let host_port = host_port(url);
    match host_port.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0) => {
            Ok(())
        }
        _ => Err("a pool address needs its port, as in stratum+tcp://pool.example:3333".into()),
    }
}

/// An Avalon's pools, from CGMiner's `pools`. Passwords are not reported,
/// so each comes back as "x".
pub fn avalon_pools(ip: IpAddr) -> Result<Vec<PoolEntry>, String> {
    if !queryable(ip) {
        return Err("only devices on the local network can be controlled".into());
    }
    avalon_pools_at(SocketAddr::new(ip.to_canonical(), 4028))
}

pub(super) fn avalon_pools_at(cgminer: SocketAddr) -> Result<Vec<PoolEntry>, String> {
    let reply = exchange(
        cgminer,
        br#"{"command":"pools"}"#,
        Instant::now() + DEADLINE,
        |reply| reply.contains(&0),
    )
    .ok_or(NO_ANSWER)?;
    parse_pools(&reply).ok_or_else(|| "unexpected reply from the device".into())
}

/// The `POOLS` list of a CGMiner `pools` reply, in priority order.
pub fn parse_pools(reply: &str) -> Option<Vec<PoolEntry>> {
    let value: Value = serde_json::from_str(reply.trim_end_matches('\0').trim()).ok()?;
    let mut pools: Vec<(i64, PoolEntry)> = value
        .get("POOLS")?
        .as_array()?
        .iter()
        .filter_map(|pool| {
            let url = pool.get("URL")?.as_str()?.trim();
            (!url.is_empty()).then(|| {
                (
                    pool.get("Priority").and_then(Value::as_i64).unwrap_or(0),
                    PoolEntry {
                        url: url.to_owned(),
                        user: pool
                            .get("User")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        password: "x".into(),
                        active: pool
                            .get("Stratum Active")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    },
                )
            })
        })
        .collect();
    pools.sort_by_key(|(priority, _)| *priority);
    Some(pools.into_iter().map(|(_, pool)| pool).collect())
}

/// Writes an Avalon's pools with Canaan's `setpool`, slot by slot, then
/// reboots it so they take effect. `login` is its web login. Nothing is sent
/// when a field holds a comma, a control character or over 255 bytes. The
/// device's success message repeats the worker and its password, so it is
/// never passed on.
pub fn avalon_set_pools(
    ip: IpAddr,
    login: (&str, &str),
    pools: &[PoolEntry],
) -> Result<String, String> {
    if !queryable(ip) {
        return Err("only devices on the local network can be controlled".into());
    }
    avalon_set_pools_at(SocketAddr::new(ip.to_canonical(), 4028), login, pools)
}

pub(super) fn avalon_set_pools_at(
    cgminer: SocketAddr,
    (web_user, web_password): (&str, &str),
    pools: &[PoolEntry],
) -> Result<String, String> {
    for field in [web_user, web_password]
        .into_iter()
        .chain(pools.iter().flat_map(|pool| {
            [
                pool.url.as_str(),
                pool.user.as_str(),
                pool.password.as_str(),
            ]
        }))
    {
        if field.contains(',') || field.chars().any(char::is_control) || field.len() > 255 {
            return Err("an Avalon's pool fields cannot hold a comma; nothing was sent".into());
        }
    }
    for (slot, pool) in pools.iter().take(3).enumerate() {
        let parameter = format!(
            "{web_user},{web_password},{slot},{},{},{}",
            pool.url, pool.user, pool.password
        );
        let request = serde_json::json!({"command": "setpool", "parameter": parameter}).to_string();
        let reply = exchange(
            cgminer,
            request.as_bytes(),
            Instant::now() + DEADLINE,
            |reply| reply.contains(&0),
        )
        .ok_or(NO_ANSWER)?;
        // The device's own message is passed on only when it refuses, and
        // then without anything that was sent.
        parse_ascset(&reply).map_err(|refusal| {
            [web_password, pool.password.as_str(), pool.user.as_str()]
                .iter()
                .filter(|text| text.len() > 1)
                .fold(refusal, |refusal, text| refusal.replace(text, "…"))
        })?;
    }
    ascset(cgminer, "0,reboot,0")?;
    Ok(format!(
        "{} pool(s) set; the device restarts to use them",
        pools.len().min(3)
    ))
}

/// A Bitaxe's pool and fallback pool, from AxeOS. Passwords are not
/// reported, so each comes back as "x".
pub fn axeos_pools(ip: IpAddr) -> Result<Vec<PoolEntry>, String> {
    if !queryable(ip) {
        return Err("only devices on the local network can be controlled".into());
    }
    axeos_pools_at(SocketAddr::new(ip.to_canonical(), 80))
}

pub(super) fn axeos_pools_at(web: SocketAddr) -> Result<Vec<PoolEntry>, String> {
    let info = axeos_request(web, "GET", "/api/system/info", None)?;
    let info: Value =
        serde_json::from_str(&info).map_err(|_| "unexpected reply from the device")?;
    let text = |key: &str| info.get(key).and_then(Value::as_str).unwrap_or_default();
    let using_fallback = info
        .get("isUsingFallbackStratum")
        .and_then(Value::as_u64)
        .is_some_and(|flag| flag == 1);
    Ok([
        ("stratumURL", "stratumPort", "stratumUser", !using_fallback),
        (
            "fallbackStratumURL",
            "fallbackStratumPort",
            "fallbackStratumUser",
            using_fallback,
        ),
    ]
    .into_iter()
    .filter(|(url, ..)| !text(url).trim().is_empty())
    .map(|(url, port, user, active)| PoolEntry {
        url: format!(
            "stratum+tcp://{}:{}",
            text(url).trim(),
            info.get(port).and_then(Value::as_u64).unwrap_or(0)
        ),
        user: text(user).to_owned(),
        password: "x".into(),
        active,
    })
    .collect())
}

/// Writes a Bitaxe's pool and fallback pool through AxeOS, then restarts it
/// so they take effect.
pub fn axeos_set_pools(ip: IpAddr, pools: &[PoolEntry]) -> Result<String, String> {
    if !queryable(ip) {
        return Err("only devices on the local network can be controlled".into());
    }
    axeos_set_pools_at(SocketAddr::new(ip.to_canonical(), 80), pools)
}

pub(super) fn axeos_set_pools_at(web: SocketAddr, pools: &[PoolEntry]) -> Result<String, String> {
    let split = |pool: &PoolEntry| -> Result<(String, u16), String> {
        let host_port = pool.host_port();
        let (host, port) = host_port
            .rsplit_once(':')
            .ok_or("a pool address needs its port")?;
        Ok((
            host.to_owned(),
            port.parse().map_err(|_| "a pool address needs its port")?,
        ))
    };
    let primary = pools.first().ok_or("no pool to set")?;
    let (host, port) = split(primary)?;
    let mut body = serde_json::json!({
        "stratumURL": host,
        "stratumPort": port,
        "stratumUser": primary.user,
        "stratumPassword": primary.password,
    });
    if let Some(fallback) = pools.get(1) {
        let (host, port) = split(fallback)?;
        body["fallbackStratumURL"] = host.into();
        body["fallbackStratumPort"] = port.into();
        body["fallbackStratumUser"] = fallback.user.clone().into();
        body["fallbackStratumPassword"] = fallback.password.clone().into();
    }
    axeos_request(web, "PATCH", "/api/system", Some(&body.to_string()))?;
    axeos_request(web, "POST", "/api/system/restart", None)?;
    Ok("pools set; the device restarts to use them".into())
}
// #### end PR #42 ####

/// One CGMiner `ascset` command for the first device; `Ok` carries the
/// device's message when it reports success or information.
fn ascset(address: SocketAddr, parameter: &str) -> Result<String, String> {
    let request = serde_json::json!({"command": "ascset", "parameter": parameter}).to_string();
    let reply = exchange(
        address,
        request.as_bytes(),
        Instant::now() + DEADLINE,
        |reply| reply.contains(&0),
    )
    .ok_or(NO_ANSWER)?;
    parse_ascset(&reply)
}

/// Reads the work level, then asks for one step lower or higher; the device
/// rejects a level outside its own range.
fn adjust_level(address: SocketAddr, raise: bool) -> Result<String, String> {
    let current = ascset(address, "0,worklevel,get")?;
    let level = parse_level(&current).ok_or("the device did not report its work level")?;
    let next = if raise { level + 1 } else { level - 1 };
    ascset(address, &format!("0,worklevel,set,{next}"))
        .map(|reply| format!("{reply} (work level {level} to {next})"))
}

fn bitaxe_restart(address: SocketAddr) -> Result<String, String> {
    let host = match address.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let request = format!(
        "POST /api/system/restart HTTP/1.0\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let reply = exchange(
        address,
        request.as_bytes(),
        Instant::now() + DEADLINE,
        http_complete,
    )
    .ok_or("the device did not answer")?;
    let status = reply.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) == Some("200") {
        Ok("restarting".into())
    } else {
        Err(format!("the device answered {status}"))
    }
}

/// A CGMiner reply's status: "S" (success) and "I" (information) are kept,
/// anything else is the device's refusal.
pub fn parse_ascset(reply: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(reply.trim_end_matches('\0').trim())
        .map_err(|_| "unexpected reply from the device")?;
    let status = value
        .get("STATUS")
        .and_then(|status| status.get(0))
        .ok_or("unexpected reply from the device")?;
    let message = status
        .get("Msg")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match status.get("STATUS").and_then(Value::as_str) {
        Some("S") | Some("I") => Ok(message),
        _ if message.is_empty() => Err("the device refused".into()),
        _ => Err(message),
    }
}

/// The level in a reply such as "ASC 0 set info: worklevel 2".
fn parse_level(message: &str) -> Option<i32> {
    message
        .split("worklevel")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Reads a CGMiner API reply: hash rate from `summary`, temperatures and fans
/// from `estats` (Avalon's `MM ID` text uses `Key[value]` pairs).
pub fn parse_cgminer(reply: &str) -> Option<DeviceReport> {
    // Replies end in a NUL byte.
    let value: Value = serde_json::from_str(reply.trim_end_matches('\0').trim()).ok()?;
    let summary = value
        .get("summary")
        .and_then(|s| s.get(0))
        .and_then(|s| s.get("SUMMARY"))
        .and_then(|s| s.get(0))
        .or_else(|| value.get("SUMMARY").and_then(|s| s.get(0)));
    // Prefer the 5-minute rate, the window of "Now (5m)": the 5-second rate
    // swings widely and "MHS av" averages over idle time since start. Stock
    // Antminer firmware reports GH/s instead, sometimes as "13,500.00".
    let hashrate = summary.and_then(|s| {
        [
            ("MHS 5m", 1e6),
            ("MHS 1m", 1e6),
            ("MHS 15m", 1e6),
            ("MHS 5s", 1e6),
            ("GHS 5s", 1e9),
            ("MHS av", 1e6),
            ("GHS av", 1e9),
        ]
        .iter()
        .find_map(|(key, unit)| s.get(*key).and_then(number).map(|rate| rate * unit))
    });
    let mut report = DeviceReport {
        hashrate,
        ..DeviceReport::default()
    };
    let stats = value
        .get("estats")
        .and_then(|s| s.get(0))
        .and_then(|s| s.get("STATS"))
        .and_then(Value::as_array);
    for entry in stats.into_iter().flatten() {
        let Some(map) = entry.as_object() else {
            continue;
        };
        for text in map.values().filter_map(Value::as_str) {
            if let Some(temperature) =
                bracket_number(text, "TMax").or_else(|| bracket_number(text, "Temp"))
            {
                report.temperature_c = Some(
                    report
                        .temperature_c
                        .map_or(temperature, |t: f64| t.max(temperature)),
                );
            }
            if report.fan.is_none() {
                report.fan = bracket_value(text, "FanR")
                    .map(str::to_owned)
                    .or_else(|| bracket_number(text, "Fan1").map(|rpm| format!("{rpm:.0} rpm")));
            }
            // #### PR #40
            // Avalon Nano power: `PS[0 0 0 4 2756 126 330]` holds the input
            // voltage (27.56 V) fifth and the watts sixth. Larger Avalons put
            // watts fifth, which is what asic-rs reads for every model.
            if report.power_w.is_none()
                && bracket_value(text, "Ver")
                    .is_some_and(|version| version.to_ascii_lowercase().starts_with("nano"))
            {
                report.power_w = bracket_value(text, "PS")
                    .and_then(|values| values.split_whitespace().nth(5)?.parse().ok())
                    .filter(|watts: &f64| watts.is_finite() && *watts > 0.0);
            }
        }
    }
    (!report.is_empty()).then_some(report)
}

/// Reads Bitaxe's `/api/system/info` JSON (hash rate in GH/s).
pub fn parse_bitaxe(body: &str) -> Option<DeviceReport> {
    let value: Value = serde_json::from_str(body.trim()).ok()?;
    let report = DeviceReport {
        hashrate: value
            .get("hashRate")
            .and_then(Value::as_f64)
            .map(|ghs| ghs * 1e9),
        temperature_c: value.get("temp").and_then(Value::as_f64),
        fan: value
            .get("fanrpm")
            .and_then(Value::as_f64)
            .map(|rpm| format!("{rpm:.0} rpm"))
            .or_else(|| {
                value
                    .get("fanspeed")
                    .and_then(Value::as_f64)
                    .map(|percent| format!("{percent:.0}%"))
            }),
        ..DeviceReport::default()
    };
    (!report.is_empty()).then_some(report)
}

/// A JSON number, or a number written as text such as "13,500.00".
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.replace(',', "").trim().parse().ok())
        .filter(|value| value.is_finite())
}

/// The text inside `Key[...]`, matched as a whole key.
fn bracket_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = text;
    while let Some(index) = rest.find(&format!("{key}[")) {
        let starts_key = index == 0
            || !rest[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric());
        let after = &rest[index + key.len() + 1..];
        if starts_key {
            return after.split_once(']').map(|(value, _)| value.trim());
        }
        rest = after;
    }
    None
}

fn bracket_number(text: &str, key: &str) -> Option<f64> {
    bracket_value(text, key)?
        .split_whitespace()
        .next()?
        .trim_end_matches('%')
        .parse()
        .ok()
        .filter(|value: &f64| value.is_finite())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn reads_an_avalon_summary_and_its_temperature_and_fan() {
        // Fields and values from an Avalon Nano 3 (cgminer 4.11.1) reply.
        let reply = concat!(
            r#"{"summary":[{"STATUS":[{"STATUS":"S"}],"SUMMARY":[{"Elapsed":40583,"#,
            r#""MHS av":2364812.63,"MHS 5s":6048809.53,"MHS 1m":4551161.05,"#,
            r#""MHS 5m":4176169.11,"MHS 15m":4020576.90,"Accepted":260039}],"id":1}],"#,
            r#""estats":[{"STATUS":[{"STATUS":"S"}],"STATS":[{"STATS":0,"ID":"AVANANO0","#,
            r#""MM ID0":"HashStatus[1] Ver[nano3-25103101_0736b2e] Elapsed[40600] "#,
            r#"DH[1.849%] Temp[54] OTemp[46] TMax[95] TAvg[91] TarT[90] Fan1[5280] "#,
            r#"FanR[75%] GHSspd[4115.12] PS[0 0 0 4 2756 126 330] WORKLEVEL[2]","#,
            r#""MM Count":1}],"id":1}],"id":1}"#,
            "\0"
        );
        let report = parse_cgminer(reply).unwrap();
        // The 5-minute rate, not the 6.05 TH/s 5-second spike.
        assert!((report.hashrate.unwrap() - 4.17616911e12).abs() < 1.0);
        assert_eq!(report.temperature_c, Some(95.0));
        assert_eq!(report.fan.as_deref(), Some("75%"));
        // Watts, not the 27.56 V input voltage before them.
        assert_eq!(report.power_w, Some(126.0));
    }

    #[test]
    fn reads_a_plain_summary_without_estats() {
        let reply = r#"{"STATUS":[{"STATUS":"S"}],"SUMMARY":[{"MHS av":13500000.0}],"id":1}"#;
        let report = parse_cgminer(reply).unwrap();
        assert_eq!(report.hashrate, Some(13.5e12));
        assert_eq!(report.temperature_c, None);
        assert_eq!(report.fan, None);
        // Stock Antminer firmware: GH/s, sometimes written as text.
        let reply = r#"{"SUMMARY":[{"GHS 5s":"13,500.00","GHS av":13400.0}],"id":1}"#;
        assert_eq!(parse_cgminer(reply).unwrap().hashrate, Some(13.5e12));
    }

    #[test]
    fn reads_bitaxe_system_info() {
        let body = r#"{"hashRate":1203.5,"temp":58.2,"fanrpm":4100,"fanspeed":60,"power":15.1}"#;
        let report = parse_bitaxe(body).unwrap();
        assert_eq!(report.hashrate, Some(1203.5e9));
        assert_eq!(report.temperature_c, Some(58.2));
        assert_eq!(report.fan.as_deref(), Some("4100 rpm"));
    }

    #[test]
    fn rejects_unrelated_or_empty_replies() {
        assert!(parse_cgminer("not json").is_none());
        assert!(parse_cgminer(r#"{"STATUS":[{"STATUS":"E"}]}"#).is_none());
        assert!(parse_bitaxe(r#"{"version":"x"}"#).is_none());
        // A key is matched whole: "MaxTemp" is not "Temp".
        assert_eq!(bracket_number("MaxTemp[90] Temp[50]", "Temp"), Some(50.0));
        assert_eq!(bracket_value("FanR[42%]", "FanR"), Some("42%"));
    }

    #[test]
    fn only_local_network_addresses_are_asked() {
        for local in [
            "192.168.1.20",
            "10.0.0.2",
            "172.16.5.4",
            "172.31.255.1",
            "169.254.3.3",
            "100.64.0.7",
            "100.127.255.1",
            "fd12::5",
            "fe80::1",
            "::ffff:192.168.1.20",
        ] {
            assert!(queryable(local.parse().unwrap()), "{local}");
        }
        for other in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "192.0.2.9",
            "127.0.0.1",
            "0.0.0.0",
            "::1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!queryable(other.parse().unwrap()), "{other}");
        }
        // Refused before any connection is attempted.
        assert_eq!(poll("8.8.8.8".parse().unwrap()), None);
    }

    /// A device stand-in that answers one connection per reply, in order,
    /// and hands each request it received to the test. (PR #42: shared with
    /// the `fleet` tests.)
    fn pool(url: &str, user: &str) -> PoolEntry {
        PoolEntry {
            url: url.into(),
            user: user.into(),
            password: "x".into(),
            active: false,
        }
    }

    // #### PR #42
    // What: a pool plan puts the new pool first, keeps each old one once as
    // a backup, and lists what does not fit; pool addresses need a port and
    // a stratum scheme.
    // Look here if: PoolPlan::new or check_pool_url changes.
    #[test]
    fn pool_plans_put_the_new_pool_first_and_need_a_port() {
        let current = [
            pool("stratum+tcp://192.168.0.55:3333", "rig1"),
            pool("stratum+tcp://backup.example:3333", "rig1"),
            pool("stratum+tcp://other.example:3333", "rig1"),
        ];
        let plan = PoolPlan::new(&current, pool("stratum+tcp://pool.example:3333", "rig1"), 3);
        let urls: Vec<_> = plan.pools.iter().map(|pool| pool.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "stratum+tcp://pool.example:3333",
                "stratum+tcp://192.168.0.55:3333",
                "stratum+tcp://backup.example:3333"
            ]
        );
        assert_eq!(plan.dropped[0].url, "stratum+tcp://other.example:3333");
        // The same pool twice is kept once; a Bitaxe holds two.
        let again = PoolPlan::new(
            &current,
            pool("stratum+tcp://BACKUP.example:3333", "rig1"),
            2,
        );
        assert_eq!(again.pools.len(), 2);
        assert_eq!(again.pools[1].url, "stratum+tcp://192.168.0.55:3333");
        for good in [
            "stratum+tcp://pool.example:3333",
            "pool.example:3333",
            "stratum2+tcp://pool.example:3336/9bXiEd8boQVhq7WddEcERUL5tyyJVFYdU8th3HfbNXK3Yw6GRXh",
        ] {
            assert!(check_pool_url(good).is_ok(), "{good}");
        }
        for bad in [
            "stratum+tcp://pool.example",
            "pool.example",
            "http://pool.example:3333",
            "stratum+tcp://pool example:3333",
            "stratum+tcp://pool.example:0",
        ] {
            assert!(check_pool_url(bad).is_err(), "{bad}");
        }
    }

    // #### PR #42
    // What: an Avalon's pools are read in priority order; `setpool` is sent
    // slot by slot with the web login and the device is rebooted; the
    // success message, which repeats the worker and its password, is never
    // passed on; a refusal is, without anything that was sent; a comma in any
    // field stops everything before a request.
    // Look here if: parse_pools or avalon_set_pools_at changes.
    #[test]
    fn avalon_pools_are_read_and_set_without_showing_the_password() {
        let read = parse_pools(
            r#"{"STATUS":[{"STATUS":"S"}],"POOLS":[
                {"POOL":1,"URL":"stratum+tcp://backup.example:3333","User":"rig1","Priority":1,"Stratum Active":false},
                {"POOL":0,"URL":"stratum+tcp://192.168.0.55:3333","User":"rig1","Priority":0,"Stratum Active":true},
                {"POOL":2,"URL":"","User":"","Priority":2}]}"#,
        )
        .unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].url, "stratum+tcp://192.168.0.55:3333");
        assert!(read[0].active && !read[1].active);
        let (avalon, requests) = recording_device(vec![
            br#"{"STATUS":[{"STATUS":"S","Msg":"pool 0 success set to stratum+tcp://pool.example:3333\nworker is rig1\nworkerpassword is secretpw\nPlease reboot miner to make config work."}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"S","Msg":"pool 1 success set to stratum+tcp://192.168.0.55:3333\nworker is rig1\nworkerpassword is x\nPlease reboot miner to make config work."}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"I","Msg":"ASC 0 set info: reboot"}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"E","Msg":"userpass err for webpw"}],"id":1}"#,
        ]);
        let mut first = pool("stratum+tcp://pool.example:3333", "rig1");
        first.password = "secretpw".into();
        let pools = [first, pool("stratum+tcp://192.168.0.55:3333", "rig1")];
        let done = avalon_set_pools_at(avalon, ("admin", "webpw"), &pools).unwrap();
        assert_eq!(done, "2 pool(s) set; the device restarts to use them");
        assert!(!done.contains("secretpw") && !done.contains("webpw"));
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"setpool","parameter":"admin,webpw,0,stratum+tcp://pool.example:3333,rig1,secretpw"}"#
        );
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"setpool","parameter":"admin,webpw,1,stratum+tcp://192.168.0.55:3333,rig1,x"}"#
        );
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,reboot,0"}"#
        );
        let refused = avalon_set_pools_at(avalon, ("admin", "webpw"), &pools).unwrap_err();
        assert!(login_refused(&refused), "{refused}");
        assert!(!refused.contains("webpw"), "{refused}");
        requests.recv().unwrap();
        // A comma anywhere: nothing is sent.
        let mut comma = pools.clone();
        comma[1].password = "a,b".into();
        assert!(avalon_set_pools_at(avalon, ("admin", "webpw"), &comma)
            .unwrap_err()
            .contains("comma; nothing was sent"));
        assert!(avalon_set_pools_at(avalon, ("ad,min", "webpw"), &pools).is_err());
        assert!(requests.recv_timeout(Duration::from_millis(200)).is_err());
    }

    // #### PR #42
    // What: a Bitaxe's pool and fallback are read from AxeOS, written as
    // host, port, user and password (the old primary becomes the fallback in
    // a plan), and the device is restarted.
    // Look here if: axeos_pools_at or axeos_set_pools_at changes.
    #[test]
    fn axeos_pools_are_read_and_set_then_the_device_restarts() {
        let (http, requests) = recording_device(vec![
            b"HTTP/1.0 200 OK\r\n\r\n{\"stratumURL\":\"192.168.0.55\",\"stratumPort\":3333,\"stratumUser\":\"rig1\",\"fallbackStratumURL\":\"backup.example\",\"fallbackStratumPort\":3334,\"fallbackStratumUser\":\"rig1\",\"isUsingFallbackStratum\":0}",
            b"HTTP/1.0 200 OK\r\n\r\n",
            b"HTTP/1.0 200 OK\r\n\r\nSystem will restart shortly.",
        ]);
        let read = axeos_pools_at(http).unwrap();
        assert_eq!(read[0].url, "stratum+tcp://192.168.0.55:3333");
        assert_eq!(read[1].url, "stratum+tcp://backup.example:3334");
        assert!(read[0].active && !read[1].active);
        requests.recv().unwrap();
        let plan = PoolPlan::new(&read, pool("stratum+tcp://pool.example:3333", "rig1"), 2);
        assert_eq!(
            axeos_set_pools_at(http, &plan.pools).unwrap(),
            "pools set; the device restarts to use them"
        );
        let patch = requests.recv().unwrap();
        assert!(
            patch.starts_with("PATCH /api/system HTTP/1.0\r\n"),
            "{patch}"
        );
        let body: Value = serde_json::from_str(patch.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["stratumURL"], "pool.example");
        assert_eq!(body["stratumPort"], 3333);
        assert_eq!(body["fallbackStratumURL"], "192.168.0.55");
        assert_eq!(body["fallbackStratumPort"], 3333);
        assert_eq!(body["fallbackStratumUser"], "rig1");
        assert!(requests
            .recv()
            .unwrap()
            .starts_with("POST /api/system/restart HTTP/1.0\r\n"));
    }

    // #### PR #42
    // What: the ways firmwares and asic-rs say a login was refused, and
    // errors that are not refusals.
    // Look here if: login_refused changes.
    #[test]
    fn login_refusals_are_recognised() {
        for refused in [
            "HTTP request failed with status code 401 Unauthorized",
            "status code 403",
            "Forbidden",
            "Authentication failed",
            "username err",
            "userpass err",
            "invalid token",
            "AES decryption failed",
        ] {
            assert!(login_refused(refused), "{refused}");
        }
        for other in [
            "the device did not answer",
            "worklevel 4011",
            "error:1 not support set workmode max_mode[0]",
        ] {
            assert!(!login_refused(other), "{other}");
        }
    }

    // #### PR #42
    // What: Pickaxe's own Avalon fan and work-mode commands, byte for byte;
    // a fan speed outside 15% to 100% refused before anything is sent; the
    // device's own refusal shown; an Avalon's help listing read; watts never
    // sent this way.
    // Look here if: setting_at or avalon_options_at changes.
    #[test]
    fn avalon_fan_and_workmode_send_canaan_commands() {
        let (avalon, requests) = recording_device(vec![
            br#"{"STATUS":[{"STATUS":"S","Msg":"ASC 0 set OK"}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"S","Msg":"ASC 0 set OK"}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"E","Msg":"error:1 not support set workmode max_mode[0]"}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"I","Msg":"ASC 0 set info: help: fan-spd|reboot|worklevel|ledset"}],"id":1}"#,
        ]);
        let web = SocketAddr::from(([127, 0, 0, 1], 9));
        let set = |action| setting_at(avalon, web, OwnApi::Avalon, action);
        assert_eq!(set(DeviceAction::FanAuto).unwrap(), "ASC 0 set OK");
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,fan-spd,-1"}"#
        );
        // Below 15% nothing is sent: the next request is the 60%.
        assert!(set(DeviceAction::FanPercent(10))
            .unwrap_err()
            .contains("15% to 100%; nothing was sent"));
        assert_eq!(set(DeviceAction::FanPercent(60)).unwrap(), "ASC 0 set OK");
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,fan-spd,60"}"#
        );
        assert_eq!(
            set(DeviceAction::PowerMode(PowerMode::High)).unwrap_err(),
            "error:1 not support set workmode max_mode[0]"
        );
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,workmode,set,2"}"#
        );
        let options = avalon_options_at(avalon).unwrap();
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,help"}"#
        );
        assert!(options.contains(&"fan-spd".to_owned()));
        assert!(!options.contains(&"workmode".to_owned()));
        assert_eq!(
            set(DeviceAction::PowerWatts(3000)).unwrap_err(),
            "this device does not offer that action"
        );
        assert_eq!(
            set(DeviceAction::Restart).unwrap_err(),
            "this device does not offer that action"
        );
    }

    // #### PR #42
    // What: a Bitaxe's fan is set under the key its firmware reads
    // (`manualFanSpeed` from AxeOS v2.12, `fanspeed` before), automatic needs
    // no reading first, and AxeOS's 401 for a request from outside its local
    // network is explained.
    // Look here if: axeos_fan or axeos_request changes.
    #[test]
    fn axeos_fan_uses_the_key_the_device_reports_and_explains_a_401() {
        let (http, requests) = recording_device(vec![
            b"HTTP/1.0 200 OK\r\n\r\n{\"manualFanSpeed\":55,\"autofanspeed\":1}",
            b"HTTP/1.0 200 OK\r\n\r\n",
            b"HTTP/1.0 200 OK\r\n\r\n{\"fanspeed\":55,\"autofanspeed\":1}",
            b"HTTP/1.0 200 OK\r\n\r\n",
            b"HTTP/1.0 401 Unauthorized\r\n\r\nUnauthorized",
        ]);
        let cgminer = SocketAddr::from(([127, 0, 0, 1], 9));
        let set = |action| setting_at(cgminer, http, OwnApi::AxeOs, action);
        assert_eq!(set(DeviceAction::FanPercent(60)).unwrap(), "fan set to 60%");
        assert!(requests
            .recv()
            .unwrap()
            .starts_with("GET /api/system/info HTTP/1.0\r\n"));
        let patch = requests.recv().unwrap();
        assert!(
            patch.starts_with("PATCH /api/system HTTP/1.0\r\n"),
            "{patch}"
        );
        assert!(patch.contains("Content-Type: application/json\r\n"));
        assert!(
            patch.ends_with("\r\n\r\n{\"autofanspeed\":0,\"manualFanSpeed\":60}"),
            "{patch}"
        );
        assert_eq!(set(DeviceAction::FanPercent(40)).unwrap(), "fan set to 40%");
        requests.recv().unwrap();
        assert!(requests
            .recv()
            .unwrap()
            .ends_with("\r\n\r\n{\"autofanspeed\":0,\"fanspeed\":40}"));
        let refused = set(DeviceAction::FanAuto).unwrap_err();
        assert!(
            refused.contains("only accepts changes from its own local network"),
            "{refused}"
        );
        assert!(requests
            .recv()
            .unwrap()
            .ends_with("\r\n\r\n{\"autofanspeed\":1}"));
        // Over 100% and power settings are refused without a request.
        assert!(set(DeviceAction::FanPercent(101)).is_err());
        assert!(set(DeviceAction::PowerMode(PowerMode::Low)).is_err());
        assert!(requests.recv_timeout(Duration::from_millis(200)).is_err());
    }

    pub(in crate::stratum_v2) fn recording_device(
        replies: Vec<&'static [u8]>,
    ) -> (SocketAddr, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = vec![0; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let _ = sender.send(String::from_utf8_lossy(&request[..read]).into_owned());
                let _ = stream.write_all(reply);
            }
        });
        (address, receiver)
    }

    #[test]
    fn controls_send_canaan_commands_and_report_the_reply() {
        // Raise power: read the level, then set one step up.
        let (avalon, requests) = recording_device(vec![
            br#"{"STATUS":[{"STATUS":"I","Msg":"ASC 0 set info: worklevel 1"}],"id":1}"#,
            br#"{"STATUS":[{"STATUS":"S","Msg":"ASC 0 set OK"}],"id":1}"#,
        ]);
        assert_eq!(
            adjust_level(avalon, true).unwrap(),
            "ASC 0 set OK (work level 1 to 2)"
        );
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,worklevel,get"}"#
        );
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,worklevel,set,2"}"#
        );
        // A refusal is reported with the device's own reason.
        let (refusing, _) = recording_device(vec![
            br#"{"STATUS":[{"STATUS":"E","Msg":"ASC 0 set failed: worklevel unknown argument"}]}"#,
        ]);
        assert_eq!(
            ascset(refusing, "0,worklevel,set,9").unwrap_err(),
            "ASC 0 set failed: worklevel unknown argument"
        );
        // Bitaxe restart.
        let (bitaxe, requests) =
            recording_device(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]);
        assert_eq!(bitaxe_restart(bitaxe).unwrap(), "restarting");
        assert!(requests
            .recv()
            .unwrap()
            .starts_with("POST /api/system/restart HTTP/1.0\r\n"));
        assert_eq!(parse_level("ASC 0 set info: worklevel -1"), Some(-1));
        assert_eq!(parse_level("ASC 0 set OK"), None);
        // Public addresses are never controlled.
        assert!(control("8.8.8.8".parse().unwrap(), DeviceAction::Restart).is_err());
    }

    /// A device stand-in on loopback: writes `reply` one piece at a time,
    /// `pause` apart, then holds the connection open.
    fn device(reply: &'static [&'static [u8]], pause: Duration) -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                for piece in reply {
                    std::thread::sleep(pause);
                    if stream.write_all(piece).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_secs(10));
            }
        });
        address
    }

    #[test]
    fn whole_replies_return_without_waiting_for_the_device_to_close() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(8);
        let cgminer = device(&[br#"{"SUMMARY":[{"MHS av":1.0}]}"#, b"\0"], Duration::ZERO);
        let reply = exchange(cgminer, b"{}", deadline, |reply| reply.contains(&0)).unwrap();
        assert!(parse_cgminer(&reply).is_some());
        let http = device(
            &[
                b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n",
                br#"{"temp":51.5}"#,
            ],
            Duration::ZERO,
        );
        let reply = exchange(http, b"GET / HTTP/1.0\r\n\r\n", deadline, http_complete).unwrap();
        let body = reply.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(parse_bitaxe(body).unwrap().temperature_c, Some(51.5));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn silent_or_trickling_devices_end_at_the_deadline() {
        let silent = device(&[], Duration::ZERO);
        let trickling = device(&[b" " as &[u8]; 100], Duration::from_millis(50));
        for address in [silent, trickling] {
            let start = Instant::now();
            let deadline = start + Duration::from_millis(400);
            assert_eq!(
                exchange(address, b"{}", deadline, |reply| reply.contains(&0)),
                None
            );
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
}
