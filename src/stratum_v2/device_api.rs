//! #### PR #38
//! Read-only device reports for the workers table: what the miner itself says
//! about its hash rate, temperature and fans. Only standard read commands are
//! sent (CGMiner API `summary` and `estats` on port 4028; Bitaxe's
//! `/api/system/info`); nothing here changes a device setting.

use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_REPLY: u64 = 256 * 1024;

/// What a device reports about itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeviceReport {
    /// The device's own hash rate, in hashes per second.
    pub hashrate: Option<f64>,
    /// Hottest reported temperature, in degrees Celsius.
    pub temperature_c: Option<f64>,
    /// Fan speed as the device gives it: "3200 rpm" or "45%".
    pub fan: Option<String>,
}

impl DeviceReport {
    fn is_empty(&self) -> bool {
        self.hashrate.is_none() && self.temperature_c.is_none() && self.fan.is_none()
    }
}

/// Asks a device on the local network for its own report: first the CGMiner
/// API used by Avalon and most SHA-256 miners, then Bitaxe's web API.
pub fn poll(ip: IpAddr) -> Option<DeviceReport> {
    cgminer(ip).or_else(|| bitaxe(ip))
}

fn exchange(address: SocketAddr, request: &[u8]) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
    stream.write_all(request).ok()?;
    let mut reply = Vec::new();
    stream.take(MAX_REPLY).read_to_end(&mut reply).ok()?;
    Some(String::from_utf8_lossy(&reply).into_owned())
}

fn cgminer(ip: IpAddr) -> Option<DeviceReport> {
    let reply = exchange(
        SocketAddr::new(ip, 4028),
        br#"{"command":"summary+estats"}"#,
    )?;
    parse_cgminer(&reply)
}

fn bitaxe(ip: IpAddr) -> Option<DeviceReport> {
    let request =
        format!("GET /api/system/info HTTP/1.0\r\nHost: {ip}\r\nConnection: close\r\n\r\n");
    let reply = exchange(SocketAddr::new(ip, 80), request.as_bytes())?;
    let body = reply.split_once("\r\n\r\n")?.1;
    parse_bitaxe(body)
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
    let mhs = summary.and_then(|s| {
        ["MHS 5s", "MHS 1m", "MHS av"]
            .iter()
            .find_map(|key| s.get(*key).and_then(Value::as_f64))
    });
    let mut report = DeviceReport {
        hashrate: mhs.map(|mhs| mhs * 1e6),
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
    };
    (!report.is_empty()).then_some(report)
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
        let reply = concat!(
            r#"{"summary":[{"STATUS":[{"STATUS":"S"}],"SUMMARY":[{"Elapsed":600,"#,
            r#""MHS av":3912345.67,"MHS 5s":4012345.67,"Accepted":120}],"id":1}],"#,
            r#""estats":[{"STATUS":[{"STATUS":"S"}],"STATS":[{"STATS":0,"ID":"AVALON0","#,
            r#""MM ID0":"Ver[Nano3-25021401] DNA[0201] Elapsed[600] Temp[48] TMax[63] TAvg[55] "#,
            r#"Fan1[3120] FanR[42%] GHSspd[4012.34] WORKMODE[2]"}],"id":1}],"id":1}"#,
            "\0"
        );
        let report = parse_cgminer(reply).unwrap();
        assert_eq!(report.hashrate, Some(4_012_345.67e6));
        assert_eq!(report.temperature_c, Some(63.0));
        assert_eq!(report.fan.as_deref(), Some("42%"));
    }

    #[test]
    fn reads_a_plain_summary_without_estats() {
        let reply = r#"{"STATUS":[{"STATUS":"S"}],"SUMMARY":[{"MHS av":13500000.0}],"id":1}"#;
        let report = parse_cgminer(reply).unwrap();
        assert_eq!(report.hashrate, Some(13.5e12));
        assert_eq!(report.temperature_c, None);
        assert_eq!(report.fan, None);
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
}
