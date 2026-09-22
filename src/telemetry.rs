//! Bounded, optional GPU telemetry shared by benchmark and live runtime views.
//!
//! Telemetry is deliberately outside the mining/search worker. A slow or
//! unavailable provider must never stall PHOTON candidate scheduling.

use crate::backend::BackendKind;
use serde::Serialize;
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

fn sample_gpu_telemetry(backend: BackendKind, device: u32) -> Option<GpuTelemetry> {
    match backend {
        BackendKind::Cuda => sample_nvidia_telemetry(device),
        BackendKind::Auto | BackendKind::Hip => None,
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
    fn efficiency_requires_positive_power() {
        let mut telemetry = GpuTelemetry::default();
        assert_eq!(telemetry.candidates_per_watt(500_000.0), None);
        telemetry.power_watts = Some(100.0);
        assert_eq!(telemetry.candidates_per_watt(500_000.0), Some(5_000.0));
        telemetry.power_watts = Some(0.0);
        assert_eq!(telemetry.candidates_per_watt(500_000.0), None);
    }
}
