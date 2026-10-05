//! Bounded, optional GPU telemetry shared by benchmark and live runtime views.
//!
//! Telemetry is deliberately outside the mining/search worker. A slow or
//! unavailable provider must never stall PHOTON candidate scheduling.

use crate::backend::{BackendKind, GpuDevice};
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
    /// Fan duty in percent; laptop GPUs usually do not report it.
    pub fan_percent: Option<f64>,
}

impl GpuTelemetry {
    /// Calculates hash-rate efficiency for positive GPU power readings.
    pub fn candidates_per_watt(&self, candidates_per_second: f64) -> Option<f64> {
        self.power_watts
            .filter(|watts| *watts > 0.0)
            .map(|watts| candidates_per_second / watts)
    }
}

const HASH_RATE_UNITS: [&str; 6] = ["H/s", "KH/s", "MH/s", "GH/s", "TH/s", "PH/s"];

/// Scale a candidate rate by 1000 into H/s through PH/s.
/// At least 100 in the chosen unit uses one decimal; smaller values use two.
pub fn format_hash_rate(rate: f64) -> String {
    let mut value = if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        0.0
    };
    let mut unit = 0usize;
    while unit + 1 < HASH_RATE_UNITS.len() && value >= 1000.0 {
        value /= 1000.0;
        unit += 1;
    }
    let decimals = if value >= 100.0 { 1 } else { 2 };
    format!(
        "{value:.decimals$} {unit}",
        decimals = decimals,
        unit = HASH_RATE_UNITS[unit]
    )
}

/// Short live PHOTON target for display. Empty means the target was not read.
pub fn format_photon_target(target_le_hex: &str) -> String {
    let target = target_le_hex.trim();
    if target.is_empty() {
        return "unavailable".to_string();
    }
    if target.len() <= 18 {
        return target.to_string();
    }
    format!("{}...{}", &target[..8], &target[target.len() - 8..])
}

/// Parses an optional numeric GPU telemetry field.
fn parse_metric(value: Option<&&str>) -> Option<f64> {
    value?.trim().parse::<f64>().ok()
}

/// Decodes utilization and power metrics from nvidia-smi output.
pub(crate) fn parse_nvidia_smi_line(line: &str) -> Option<GpuTelemetry> {
    let fields = line.split(',').collect::<Vec<_>>();
    if !(6..=7).contains(&fields.len()) {
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
        fan_percent: parse_metric(fields.get(6)),
    })
}

/// Samples telemetry from one NVIDIA GPU, by nvidia-smi index or PCI address.
pub(crate) fn sample_nvidia_telemetry(id: &str) -> Option<GpuTelemetry> {
    let output = Command::new("nvidia-smi")
        .arg(format!("--id={id}"))
        .arg("--query-gpu=utilization.gpu,power.draw,temperature.gpu,memory.used,clocks.gr,clocks.mem,fan.speed")
        .arg("--format=csv,noheader,nounits")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_nvidia_smi_line(stdout.lines().next()?)
}

/// Normalizes AMD telemetry keys across output formats.
fn normalized_metric_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Extracts a numeric value from a GPU metric field.
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

/// Finds a metric in nested AMD telemetry values.
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

/// Decodes AMD GPU metrics from amd-smi JSON output.
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
        fan_percent: find_metric(&value, &["fan_speed", "fan_speed_percent", "fan_usage"]),
    };
    [
        telemetry.gpu_utilization_percent,
        telemetry.power_watts,
        telemetry.temperature_c,
        telemetry.vram_used_mib,
        telemetry.graphics_clock_mhz,
        telemetry.memory_clock_mhz,
        telemetry.fan_percent,
    ]
    .iter()
    .any(Option::is_some)
    .then_some(telemetry)
}

/// Samples telemetry from one AMD GPU, by amd-smi index or PCI address.
pub(crate) fn sample_amd_telemetry(id: &str) -> Option<GpuTelemetry> {
    let output = Command::new("amd-smi")
        .arg("monitor")
        .arg("--gpu")
        .arg(id)
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

/// Samples metrics from the selected GPU backend.
pub(crate) fn sample_gpu_telemetry(backend: BackendKind, device: u32) -> Option<GpuTelemetry> {
    match backend {
        BackendKind::Cuda => sample_nvidia_telemetry(&device.to_string()),
        BackendKind::Hip => sample_amd_telemetry(&device.to_string()),
        BackendKind::Auto | BackendKind::Wgpu => None,
    }
}

/// The vendor tool that reports one GPU, and the GPU id that tool takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetrySource {
    Nvidia(String),
    Amd(String),
}

impl TelemetrySource {
    fn sample(&self) -> Option<GpuTelemetry> {
        match self {
            Self::Nvidia(id) => sample_nvidia_telemetry(id),
            Self::Amd(id) => sample_amd_telemetry(id),
        }
    }
}

/// Picks each mining GPU's telemetry source: nvidia-smi for NVIDIA cards and
/// amd-smi for AMD cards, whichever engine mines them. A lone CUDA or HIP GPU
/// keeps its ordinal; otherwise the PCI address names the card, because the
/// vendor tools number GPUs in their own order.
pub fn telemetry_sources(gpus: &[GpuDevice]) -> Vec<Option<TelemetrySource>> {
    let vendor_count = |vendor: &str| {
        gpus.iter()
            .filter(|gpu| gpu.vendor.eq_ignore_ascii_case(vendor))
            .count()
    };
    gpus.iter()
        .map(|gpu| {
            let (native, source): (BackendKind, fn(String) -> TelemetrySource) =
                if gpu.vendor.eq_ignore_ascii_case("NVIDIA") {
                    (BackendKind::Cuda, TelemetrySource::Nvidia)
                } else if gpu.vendor.eq_ignore_ascii_case("AMD") {
                    (BackendKind::Hip, TelemetrySource::Amd)
                } else {
                    return None;
                };
            let lone_native = gpu.backend == native && vendor_count(&gpu.vendor) == 1;
            let id = match gpu.pci {
                Some(pci) if !lone_native => pci.to_string(),
                _ if gpu.backend == native => gpu.index.to_string(),
                _ => return None,
            };
            Some(source(id))
        })
        .collect()
}

/// Several GPUs' telemetry as one machine: total power and VRAM (only when
/// every GPU reports them), the hottest temperature, the average utilization
/// and the fastest fan. Clocks only mean something per GPU. A single GPU's
/// telemetry is returned unchanged.
pub fn combined_telemetry(gpus: &[GpuTelemetry]) -> GpuTelemetry {
    if let [only] = gpus {
        return only.clone();
    }
    let reported = |metric: fn(&GpuTelemetry) -> Option<f64>| {
        gpus.iter().filter_map(metric).collect::<Vec<f64>>()
    };
    let total = |metric: fn(&GpuTelemetry) -> Option<f64>| {
        let values = reported(metric);
        (!values.is_empty() && values.len() == gpus.len()).then(|| values.iter().sum())
    };
    let highest =
        |metric: fn(&GpuTelemetry) -> Option<f64>| reported(metric).into_iter().reduce(f64::max);
    let utilization = reported(|gpu| gpu.gpu_utilization_percent);
    GpuTelemetry {
        samples: gpus.iter().map(|gpu| gpu.samples).max().unwrap_or(0),
        gpu_utilization_percent: (!utilization.is_empty())
            .then(|| utilization.iter().sum::<f64>() / utilization.len() as f64),
        power_watts: total(|gpu| gpu.power_watts),
        temperature_c: highest(|gpu| gpu.temperature_c),
        vram_used_mib: total(|gpu| gpu.vram_used_mib),
        graphics_clock_mhz: None,
        memory_clock_mhz: None,
        fan_percent: highest(|gpu| gpu.fan_percent),
    }
}

pub struct LiveTelemetrySampler {
    latest: Arc<Mutex<Vec<GpuTelemetry>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LiveTelemetrySampler {
    /// Starts one background thread that samples every GPU about once a
    /// second, in the order of `sources`.
    pub fn start(sources: Vec<Option<TelemetrySource>>) -> Self {
        let latest = Arc::new(Mutex::new(vec![GpuTelemetry::default(); sources.len()]));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_latest = Arc::clone(&latest);
        let worker_stop = Arc::clone(&stop);
        let worker = if sources.iter().any(Option::is_some) {
            thread::Builder::new()
                .name("pickaxe-telemetry".into())
                .spawn(move || {
                    while !worker_stop.load(Ordering::Relaxed) {
                        for (index, source) in sources.iter().enumerate() {
                            if worker_stop.load(Ordering::Relaxed) {
                                break;
                            }
                            let Some(mut sample) =
                                source.as_ref().and_then(TelemetrySource::sample)
                            else {
                                continue;
                            };
                            let mut latest = worker_latest
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            sample.samples = latest[index].samples.saturating_add(1);
                            latest[index] = sample;
                        }

                        let mut slept = Duration::ZERO;
                        while slept < LIVE_TELEMETRY_INTERVAL
                            && !worker_stop.load(Ordering::Relaxed)
                        {
                            let remaining = LIVE_TELEMETRY_INTERVAL.saturating_sub(slept);
                            let sleep_for = remaining.min(LIVE_TELEMETRY_SLEEP_SLICE);
                            thread::sleep(sleep_for);
                            slept += sleep_for;
                        }
                    }
                })
                .ok()
        } else {
            None
        };

        Self {
            latest,
            stop,
            worker,
        }
    }

    /// Returns the most recent sample of each GPU, in the order given at start.
    pub fn snapshot_each(&self) -> Vec<GpuTelemetry> {
        self.latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Returns the most recent telemetry of all GPUs together.
    pub fn snapshot(&self) -> GpuTelemetry {
        combined_telemetry(&self.snapshot_each())
    }

    /// Stops the GPU telemetry sampler and joins its thread.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for LiveTelemetrySampler {
    /// Stops sampling when the telemetry handle is dropped.
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
        assert_eq!(parsed.fan_percent, None);

        let with_fan = parse_nvidia_smi_line("87, 123.5, 68, 2048, 2450, 12001, 45").unwrap();
        assert_eq!(with_fan.fan_percent, Some(45.0));
        let laptop = parse_nvidia_smi_line("87, 123.5, 68, 2048, 2450, 12001, [N/A]").unwrap();
        assert_eq!(laptop.fan_percent, None);
        assert!(parse_nvidia_smi_line("87, 123.5, 68, 2048, 2450, 12001, 45, 1").is_none());
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

    #[test]
    fn hash_rate_uses_1000_based_units_for_current_average_and_peak() {
        assert_eq!(format_hash_rate(0.0), "0.00 H/s");
        assert_eq!(format_hash_rate(f64::NAN), "0.00 H/s");
        assert_eq!(format_hash_rate(999.0), "999.0 H/s");
        assert_eq!(format_hash_rate(500_000.0), "500.0 KH/s");
        assert_eq!(format_hash_rate(1.2e9), "1.20 GH/s");
        assert_eq!(format_hash_rate(1.5e12), "1.50 TH/s");
        assert_eq!(format_hash_rate(2.5e15), "2.50 PH/s");
        assert_eq!(format_hash_rate(1.0e18), "1000.0 PH/s");
    }

    #[test]
    fn photon_target_text_says_unavailable_when_missing() {
        assert_eq!(format_photon_target(""), "unavailable");
        assert_eq!(format_photon_target("  "), "unavailable");
        assert_eq!(format_photon_target("abcd"), "abcd");
        let target = "ab".repeat(32);
        assert_eq!(format_photon_target(&target), "abababab...abababab");
    }

    fn gpu(vendor: &str, backend: BackendKind, index: u32, bus: Option<u8>) -> GpuDevice {
        GpuDevice {
            index,
            name: format!("{vendor} GPU"),
            vendor: vendor.into(),
            vram_bytes: None,
            backend,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: bus.map(|bus| crate::backend::PciAddress {
                domain: 0,
                bus,
                device: 0,
                function: 0,
            }),
        }
    }

    #[test]
    fn each_gpu_is_read_by_its_vendor_tool_and_named_by_pci_when_ambiguous() {
        let nvidia = |id: &str| Some(TelemetrySource::Nvidia(id.into()));
        let amd = |id: &str| Some(TelemetrySource::Amd(id.into()));
        // A lone CUDA GPU keeps the ordinal used before several GPUs could mine.
        assert_eq!(
            telemetry_sources(&[gpu("NVIDIA", BackendKind::Cuda, 0, Some(1))]),
            [nvidia("0")]
        );
        // Several NVIDIA cards, or one mined through wgpu, go by PCI address;
        // an iGPU without a vendor tool or address has no telemetry.
        assert_eq!(
            telemetry_sources(&[
                gpu("NVIDIA", BackendKind::Cuda, 0, Some(1)),
                gpu("NVIDIA", BackendKind::Wgpu, 1, Some(2)),
                gpu("AMD", BackendKind::Wgpu, 2, Some(0x65)),
                gpu("AMD", BackendKind::Hip, 0, Some(3)),
                gpu("Intel", BackendKind::Wgpu, 3, Some(0)),
                gpu("AMD", BackendKind::Wgpu, 4, None),
            ]),
            [
                nvidia("0000:01:00.0"),
                nvidia("0000:02:00.0"),
                amd("0000:65:00.0"),
                amd("0000:03:00.0"),
                None,
                None,
            ]
        );
        assert_eq!(
            telemetry_sources(&[
                gpu("NVIDIA", BackendKind::Cuda, 0, None),
                gpu("NVIDIA", BackendKind::Cuda, 1, None),
            ]),
            [nvidia("0"), nvidia("1")]
        );
    }

    #[test]
    fn combined_telemetry_reports_the_machine_and_keeps_a_single_gpu_unchanged() {
        let card = GpuTelemetry {
            samples: 4,
            gpu_utilization_percent: Some(100.0),
            power_watts: Some(120.0),
            temperature_c: Some(70.0),
            vram_used_mib: Some(900.0),
            graphics_clock_mhz: Some(2500.0),
            memory_clock_mhz: Some(12000.0),
            fan_percent: Some(40.0),
        };
        assert_eq!(combined_telemetry(std::slice::from_ref(&card)), card);
        assert_eq!(combined_telemetry(&[]), GpuTelemetry::default());

        let other = GpuTelemetry {
            samples: 3,
            gpu_utilization_percent: Some(80.0),
            power_watts: Some(30.0),
            temperature_c: Some(75.0),
            vram_used_mib: Some(100.0),
            graphics_clock_mhz: Some(1800.0),
            memory_clock_mhz: None,
            fan_percent: None,
        };
        assert_eq!(
            combined_telemetry(&[card.clone(), other]),
            GpuTelemetry {
                samples: 4,
                gpu_utilization_percent: Some(90.0),
                power_watts: Some(150.0),
                temperature_c: Some(75.0),
                vram_used_mib: Some(1000.0),
                graphics_clock_mhz: None,
                memory_clock_mhz: None,
                fan_percent: Some(40.0),
            }
        );

        // Power from only some GPUs would overstate efficiency: none is shown.
        let silent = combined_telemetry(&[card, GpuTelemetry::default()]);
        assert_eq!(silent.power_watts, None);
        assert_eq!(silent.vram_used_mib, None);
        assert_eq!(silent.temperature_c, Some(70.0));
    }
}
