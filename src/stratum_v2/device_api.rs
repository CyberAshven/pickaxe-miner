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
}

impl DeviceAction {
    /// The actions Pickaxe's own code can send, for devices asic-rs has not
    /// identified.
    pub const OWN: [Self; 3] = [Self::Restart, Self::LowerPower, Self::RaisePower];

    pub fn label(self) -> &'static str {
        match self {
            Self::Restart => "Restart",
            Self::Pause => "Pause mining",
            Self::Resume => "Resume mining",
            Self::LocateOn => "Blink its light (to find it)",
            Self::LocateOff => "Stop blinking its light",
            Self::LowerPower => "Lower power (one work level down)",
            Self::RaisePower => "Raise power (one work level up)",
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
    let cgminer = SocketAddr::new(ip, 4028);
    match action {
        DeviceAction::Restart => ascset(cgminer, "0,reboot,0")
            .or_else(|error| bitaxe_restart(SocketAddr::new(ip, 80)).map_err(|_| error)),
        DeviceAction::LowerPower => adjust_level(cgminer, false),
        DeviceAction::RaisePower => adjust_level(cgminer, true),
        DeviceAction::Pause
        | DeviceAction::Resume
        | DeviceAction::LocateOn
        | DeviceAction::LocateOff => Err("this device does not offer that action".into()),
    }
}

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
    .ok_or("the device did not answer")?;
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
mod tests {
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
    /// and hands each request it received to the test.
    fn recording_device(
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
