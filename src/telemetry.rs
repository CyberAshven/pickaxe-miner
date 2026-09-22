//! Bounded, optional GPU telemetry shared by benchmark and live runtime views.
//!
//! Telemetry is deliberately outside the mining/search worker. A slow or
//! unavailable provider must never stall PHOTON candidate scheduling.

use crate::backend::BackendKind;
use serde::Serialize;
use serde_json::Value;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const LIVE_TELEMETRY_INTERVAL: Duration = Duration::from_secs(1);
const LIVE_TELEMETRY_SLEEP_SLICE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct GpuTelemetry {
    pub samples: u32,
    pub gpu_utilization_percent: Option<f64>,
    pub power_watts: Option<f64>,
    pub temperature_c: Option<f64>,
    pub vram_used_mib: Option<f64>,
    pub graphics_clock_mhz: Option<f64>,
    pub memory_clock_mhz: Option<f64>,
}

impl GpuTelemetry {
    pub fn candidates_per_watt(&self, candidates_per_second: f64) -> Option<f64> {
        self.power_watts
            .filter(|watts| *watts > 0.0)
            .map(|watts| candidates_per_second / watts)
    }
}

fn parse_metric(value: Option<&&str>) -> Option<f64> {
    value?.trim().parse::<f64>().ok()
}

pub(crate) fn parse_nvidia_smi_line(line: &str) -> Option<GpuTelemetry> {
    let fields = line.split(',').collect::<Vec<_>>();
    if fields.len() != 6 {
        return None;
    }
    Some(GpuTelemetry {
        samples: 1,
        gpu_utilization_percent: parse_metric(fields.first()),
        power_watts: parse_metric(fields.get(1)),
        temperature_c: parse_metric(fields.get(2)),
        vram_used_mib: parse_metric(fields.get(3)),
        graphics_clock_mhz: parse_metric(fields.get(4)),
        memory_clock_mhz: parse_metric(fields.get(5)),
    })
}

pub(crate) fn sample_nvidia_telemetry(device: u32) -> Option<GpuTelemetry> {
    let output = Command::new("nvidia-smi")
        .arg(format!("--id={device}"))
        .arg("--query-gpu=utilization.gpu,power.draw,temperature.gpu,memory.used,clocks.gr,clocks.mem")
        .arg("--format=csv,noheader,nounits")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_nvidia_smi_line(stdout.lines().next()?)
}

fn normalized_metric_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn metric_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text
            .split_whitespace()
            .next()
            .and_then(|number| number.parse::<f64>().ok()),
        Value::Object(object) => object
            .get("value")
            .and_then(metric_number)
            .or_else(|| object.get("current").and_then(metric_number)),
        _ => None,
    }
}

fn find_metric(value: &Value, aliases: &[&str]) -> Option<f64> {
    match value {
        Value::Object(object) => {
            for alias in aliases {
                let alias = normalized_metric_key(alias);
                if let Some(metric) = object.iter().find_map(|(key, value)| {
                    (normalized_metric_key(key) == alias)
                        .then(|| metric_number(value))
                        .flatten()
                }) {
                    return Some(metric);
                }
            }
            object
                .values()
                .find_map(|nested| find_metric(nested, aliases))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|nested| find_metric(nested, aliases)),
        _ => None,
    }
}

pub(crate) fn parse_amd_smi_json(stdout: &str) -> Option<GpuTelemetry> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    let telemetry = GpuTelemetry {
        samples: 1,
        gpu_utilization_percent: find_metric(
            &value,
            &["gfx_util", "gfx_usage", "gfx_activity", "gpu_utilization"],
        ),
        power_watts: find_metric(&value, &["socket_power", "power_usage", "power"]),
        temperature_c: find_metric(
            &value,
            &["gpu_temp", "edge_temperature", "temperature_edge"],
        ),
        vram_used_mib: find_metric(&value, &["vram_used", "used_vram"]),
        graphics_clock_mhz: find_metric(&value, &["gfx_clock", "graphics_clock", "gfxclk"]),
        memory_clock_mhz: find_metric(&value, &["mem_clock", "memory_clock", "mclk"]),
    };
    [
        telemetry.gpu_utilization_percent,
        telemetry.power_watts,
        telemetry.temperature_c,
        telemetry.vram_used_mib,
        telemetry.graphics_clock_mhz,
        telemetry.memory_clock_mhz,
    ]
    .iter()
    .any(Option::is_some)
    .then_some(telemetry)
}

pub(crate) fn sample_amd_telemetry(device: u32) -> Option<GpuTelemetry> {
    let output = Command::new("amd-smi")
        .arg("monitor")
        .arg("--gpu")
        .arg(device.to_string())
        .args([
            "--power-usage",
            "--temperature",
            "--gfx",
            "--mem",
            "--vram-usage",
            "--json",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_amd_smi_json(&stdout)
}

pub(crate) fn sample_gpu_telemetry(backend: BackendKind, device: u32) -> Option<GpuTelemetry> {
    match backend {
        BackendKind::Cuda => sample_nvidia_telemetry(device),
        BackendKind::Hip => sample_amd_telemetry(device),
        BackendKind::Auto | BackendKind::Wgpu => None,
    }
}

pub struct LiveTelemetrySampler {
    latest: Arc<Mutex<GpuTelemetry>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LiveTelemetrySampler {
    pub fn start(backend: BackendKind, device: u32) -> Self {
        let latest = Arc::new(Mutex::new(GpuTelemetry::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_latest = Arc::clone(&latest);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name(format!("pickaxe-telemetry-{}-{device}", backend.as_str()))
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    if let Some(mut sample) = sample_gpu_telemetry(backend, device) {
                        let mut latest = worker_latest
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        sample.samples = latest.samples.saturating_add(1);
                        *latest = sample;
                    }

                    let mut slept = Duration::ZERO;
                    while slept < LIVE_TELEMETRY_INTERVAL && !worker_stop.load(Ordering::Relaxed) {
                        let remaining = LIVE_TELEMETRY_INTERVAL.saturating_sub(slept);
                        let sleep_for = remaining.min(LIVE_TELEMETRY_SLEEP_SLICE);
                        thread::sleep(sleep_for);
                        slept += sleep_for;
                    }
                }
            })
            .ok();

        Self {
            latest,
            stop,
            worker,
        }
    }

    pub fn snapshot(&self) -> GpuTelemetry {
        self.latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for LiveTelemetrySampler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_telemetry_parser_handles_values_and_na() {
        let parsed = parse_nvidia_smi_line("87, 123.5, 68, 2048, 2450, [N/A]").unwrap();
        assert_eq!(parsed.samples, 1);
        assert_eq!(parsed.gpu_utilization_percent, Some(87.0));
        assert_eq!(parsed.power_watts, Some(123.5));
        assert_eq!(parsed.temperature_c, Some(68.0));
        assert_eq!(parsed.vram_used_mib, Some(2048.0));
        assert_eq!(parsed.graphics_clock_mhz, Some(2450.0));
        assert_eq!(parsed.memory_clock_mhz, None);
    }

    #[test]
    fn amd_telemetry_parser_handles_unit_wrapped_metrics() {
        let parsed = parse_amd_smi_json(
            r#"[
                {
                    "gpu": 0,
                    "power": {"socket_power": {"value": 171, "unit": "W"}},
                    "temperature": {"gpu_temp": {"value": 48, "unit": "C"}},
                    "usage": {"gfx_activity": {"value": 87, "unit": "%"}},
                    "clock": {
                        "gfx_clock": {"value": 2450, "unit": "MHz"},
                        "mem_clock": {"value": 1100, "unit": "MHz"}
                    },
                    "memory": {"vram_used": {"value": 2048, "unit": "MB"}}
                }
            ]"#,
        )
        .unwrap();
        assert_eq!(parsed.samples, 1);
        assert_eq!(parsed.gpu_utilization_percent, Some(87.0));
        assert_eq!(parsed.power_watts, Some(171.0));
        assert_eq!(parsed.temperature_c, Some(48.0));
        assert_eq!(parsed.vram_used_mib, Some(2048.0));
        assert_eq!(parsed.graphics_clock_mhz, Some(2450.0));
        assert_eq!(parsed.memory_clock_mhz, Some(1100.0));
    }

    #[test]
    fn amd_telemetry_parser_handles_flat_legacy_json_and_rejects_empty_metrics() {
        let parsed = parse_amd_smi_json(
            r#"{
                "GPU": 0,
                "GFX_UTIL": "63 %",
                "POWER_USAGE": "92.5 W",
                "GPU_TEMP": "71 C",
                "VRAM_USED": "4096 MB",
                "GFX_CLOCK": "2300 MHz",
                "MEM_CLOCK": "1000 MHz"
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.gpu_utilization_percent, Some(63.0));
        assert_eq!(parsed.power_watts, Some(92.5));
        assert_eq!(parsed.temperature_c, Some(71.0));
        assert_eq!(parsed.vram_used_mib, Some(4096.0));
        assert_eq!(parsed.graphics_clock_mhz, Some(2300.0));
        assert_eq!(parsed.memory_clock_mhz, Some(1000.0));
        assert!(parse_amd_smi_json(r#"[{"gpu": 0, "note": "N/A"}]"#).is_none());
    }

    #[test]
    fn efficiency_requires_positive_power() {
        let mut telemetry = GpuTelemetry::default();
        assert_eq!(telemetry.candidates_per_watt(500_000.0), None);
        telemetry.power_watts = Some(100.0);
        assert_eq!(telemetry.candidates_per_watt(500_000.0), Some(5_000.0));
        telemetry.power_watts = Some(0.0);
        assert_eq!(telemetry.candidates_per_watt(500_000.0), None);
    }
}
