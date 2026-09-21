//! Offline throughput benchmark for the production PHOTON CUDA pipeline.
//! The benchmark never reads live baton state and never broadcasts.

use crate::cuda_photon::CudaPhotonEngine;
use crate::{reward, search, tx};
use secp256k1::{PublicKey, SecretKey};
use serde::Serialize;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TX_BYTES: usize = 615;
const TARGET_OFFSET: usize = 394;
const BENCHMARK_INTENSITIES: [u8; 5] = [10, 25, 50, 75, 100];
const WINNER_BUFFER_CAP: u32 = 8;
const VECTOR_BATON_TXID: &str = "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042";
const VECTOR_TARGET_LE: &str = "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000";
const VECTOR_AGE: u32 = 10;
const VECTOR_CONTRACT_VALUE_SATS: u64 = 15_971_500;
const VECTOR_TOKEN_AMOUNT: u128 = 2_099_905_002_035_715;
const VECTOR_REWARD_RAW: u128 = 4_999_773_813;
const TELEMETRY_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Default, Serialize)]
pub struct NvidiaTelemetry {
    pub samples: u32,
    pub gpu_utilization_percent: Option<f64>,
    pub power_watts: Option<f64>,
    pub temperature_c: Option<f64>,
    pub vram_used_mib: Option<f64>,
    pub graphics_clock_mhz: Option<f64>,
    pub memory_clock_mhz: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkSample {
    pub intensity: u8,
    pub requested_seconds: u64,
    pub elapsed_seconds: f64,
    pub candidates: u64,
    pub batches: u64,
    pub candidates_per_second: f64,
    pub candidates_per_watt: Option<f64>,
    pub gpu_winners: u64,
    pub telemetry: NvidiaTelemetry,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkReport {
    pub status: &'static str,
    pub backend: &'static str,
    pub device: u32,
    pub device_name: String,
    pub table_source: String,
    pub persistent_device_bytes: usize,
    pub samples: Vec<BenchmarkSample>,
    pub network_access: bool,
    pub broadcast: bool,
}

struct BenchmarkFixture {
    template: [u8; TX_BYTES],
    target: [u8; 32],
    private_key: [u8; 32],
}

#[derive(Default)]
struct TelemetryAccumulator {
    samples: u32,
    utilization_sum: f64,
    utilization_count: u32,
    power_sum: f64,
    power_count: u32,
    temperature_sum: f64,
    temperature_count: u32,
    vram_sum: f64,
    vram_count: u32,
    graphics_clock_sum: f64,
    graphics_clock_count: u32,
    memory_clock_sum: f64,
    memory_clock_count: u32,
}

impl TelemetryAccumulator {
    fn add(&mut self, sample: &NvidiaTelemetry) {
        self.samples = self.samples.saturating_add(1);
        add_metric(
            sample.gpu_utilization_percent,
            &mut self.utilization_sum,
            &mut self.utilization_count,
        );
        add_metric(
            sample.power_watts,
            &mut self.power_sum,
            &mut self.power_count,
        );
        add_metric(
            sample.temperature_c,
            &mut self.temperature_sum,
            &mut self.temperature_count,
        );
        add_metric(
            sample.vram_used_mib,
            &mut self.vram_sum,
            &mut self.vram_count,
        );
        add_metric(
            sample.graphics_clock_mhz,
            &mut self.graphics_clock_sum,
            &mut self.graphics_clock_count,
        );
        add_metric(
            sample.memory_clock_mhz,
            &mut self.memory_clock_sum,
            &mut self.memory_clock_count,
        );
    }

    fn finish(self) -> NvidiaTelemetry {
        NvidiaTelemetry {
            samples: self.samples,
            gpu_utilization_percent: average(self.utilization_sum, self.utilization_count),
            power_watts: average(self.power_sum, self.power_count),
            temperature_c: average(self.temperature_sum, self.temperature_count),
            vram_used_mib: average(self.vram_sum, self.vram_count),
            graphics_clock_mhz: average(self.graphics_clock_sum, self.graphics_clock_count),
            memory_clock_mhz: average(self.memory_clock_sum, self.memory_clock_count),
        }
    }
}

fn add_metric(value: Option<f64>, sum: &mut f64, count: &mut u32) {
    if let Some(value) = value {
        *sum += value;
        *count = count.saturating_add(1);
    }
}

fn average(sum: f64, count: u32) -> Option<f64> {
    (count > 0).then_some(sum / f64::from(count))
}

fn parse_metric(value: Option<&&str>) -> Option<f64> {
    value?.trim().parse::<f64>().ok()
}

fn parse_nvidia_smi_line(line: &str) -> Option<NvidiaTelemetry> {
    let fields = line.split(',').collect::<Vec<_>>();
    if fields.len() != 6 {
        return None;
    }
    Some(NvidiaTelemetry {
        samples: 1,
        gpu_utilization_percent: parse_metric(fields.first()),
        power_watts: parse_metric(fields.get(1)),
        temperature_c: parse_metric(fields.get(2)),
        vram_used_mib: parse_metric(fields.get(3)),
        graphics_clock_mhz: parse_metric(fields.get(4)),
        memory_clock_mhz: parse_metric(fields.get(5)),
    })
}

fn sample_nvidia_telemetry(device: u32) -> Option<NvidiaTelemetry> {
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

fn start_telemetry_sampler(
    device: u32,
) -> (
    Arc<AtomicBool>,
    Arc<Mutex<TelemetryAccumulator>>,
    thread::JoinHandle<()>,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let samples = Arc::new(Mutex::new(TelemetryAccumulator::default()));
    let worker_stop = Arc::clone(&stop);
    let worker_samples = Arc::clone(&samples);
    let handle = thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            if let Some(sample) = sample_nvidia_telemetry(device) {
                if let Ok(mut accumulator) = worker_samples.lock() {
                    accumulator.add(&sample);
                }
            }
            thread::sleep(TELEMETRY_INTERVAL);
        }
    });
    (stop, samples, handle)
}

fn finish_telemetry_sampler(
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<TelemetryAccumulator>>,
    handle: thread::JoinHandle<()>,
) -> NvidiaTelemetry {
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    match Arc::try_unwrap(samples) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default().finish(),
        Err(samples) => samples
            .lock()
            .map(|mut accumulator| std::mem::take(&mut *accumulator).finish())
            .unwrap_or_default(),
    }
}

fn deterministic_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[31] = 1;
    secret
}

fn benchmark_fixture() -> Result<BenchmarkFixture, String> {
    let target = search::parse_hex32(VECTOR_TARGET_LE)?;
    let private_key = deterministic_secret();
    let secret = SecretKey::from_secret_bytes(private_key)
        .map_err(|error| format!("benchmark key: {error}"))?;
    let public_key = PublicKey::from_secret_key(&secret).serialize();
    let payout_address = reward::p2pkh_cashaddr_from_public_key(&public_key)?;
    let payout_locking = tx::cashaddr_to_p2pkh_locking(&payout_address)?;
    let params = tx::TemplateParams {
        prev_tx_hash_hex: VECTOR_BATON_TXID.into(),
        prev_index: 0,
        age: VECTOR_AGE,
        public_key_hex: hex::encode(public_key),
        target_hex: VECTOR_TARGET_LE.into(),
        signature_hex: "00".repeat(64),
        nonce: 0,
        contract_value_sats: VECTOR_CONTRACT_VALUE_SATS,
        contract_token_amount: VECTOR_TOKEN_AMOUNT,
        reward_amount: VECTOR_REWARD_RAW,
        payout_locking,
    };
    let bytes = tx::build_photon_template_bytes(&params)?;
    let template: [u8; TX_BYTES] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "benchmark PHOTON template is {} bytes; expected {TX_BYTES}",
            bytes.len()
        )
    })?;
    if template[TARGET_OFFSET..TARGET_OFFSET + 32] != target {
        return Err("benchmark PHOTON target bytes do not match the reference vector".into());
    }
    Ok(BenchmarkFixture {
        template,
        target,
        private_key,
    })
}

fn run_intensity_window(
    engine: &mut CudaPhotonEngine,
    device: u32,
    intensity: u8,
    seconds: u64,
    nonce_base: &mut u32,
) -> Result<BenchmarkSample, String> {
    let requested = Duration::from_secs(seconds);
    let batch_candidates = search::batch_candidates(intensity);
    let (telemetry_stop, telemetry_samples, telemetry_worker) = start_telemetry_sampler(device);
    let started = Instant::now();
    let mut candidates = 0u64;
    let mut batches = 0u64;
    let mut winners = 0u64;

    while started.elapsed() < requested {
        let batch_started = Instant::now();
        let result = match engine.search_batch(*nonce_base, batch_candidates) {
            Ok(result) => result,
            Err(error) => {
                let _ =
                    finish_telemetry_sampler(telemetry_stop, telemetry_samples, telemetry_worker);
                return Err(error);
            }
        };
        let compute_time = batch_started.elapsed();
        candidates = candidates.saturating_add(u64::from(result.candidates));
        winners = winners.saturating_add(u64::from(result.total_winners));
        batches = batches.saturating_add(1);
        *nonce_base = (*nonce_base).wrapping_add(result.candidates);

        let remaining = requested.saturating_sub(started.elapsed());
        let rest = search::duty_rest(compute_time, intensity).min(remaining);
        if !rest.is_zero() {
            thread::sleep(rest);
        }
    }

    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let telemetry = finish_telemetry_sampler(telemetry_stop, telemetry_samples, telemetry_worker);
    let candidates_per_second = candidates as f64 / elapsed;
    let candidates_per_watt = telemetry
        .power_watts
        .filter(|watts| *watts > 0.0)
        .map(|watts| candidates_per_second / watts);
    Ok(BenchmarkSample {
        intensity,
        requested_seconds: seconds,
        elapsed_seconds: elapsed,
        candidates,
        batches,
        candidates_per_second,
        candidates_per_watt,
        gpu_winners: winners,
        telemetry,
    })
}

pub fn run_cuda_benchmark(
    device: u32,
    device_name: String,
    seconds: u64,
) -> Result<BenchmarkReport, String> {
    if !(1..=300).contains(&seconds) {
        return Err("benchmark --seconds must be 1..=300".into());
    }
    if cfg!(debug_assertions) {
        return Err("benchmark requires an optimized release build".into());
    }
    let fixture = benchmark_fixture()?;
    let mut engine = CudaPhotonEngine::new(
        device as usize,
        search::MAX_BATCH_CANDIDATES,
        WINNER_BUFFER_CAP,
    )?;
    let persistent_device_bytes = engine.persistent_device_bytes();
    let table_source = format!("{:?}", engine.table_source());
    engine.set_job(&fixture.template, &fixture.target, &fixture.private_key)?;

    // Prime kernels and page residency before measuring any intensity window.
    let mut nonce_base = 0u32;
    let warmup = engine.search_batch(nonce_base, search::batch_candidates(25))?;
    nonce_base = nonce_base.wrapping_add(warmup.candidates);

    let mut samples = Vec::with_capacity(BENCHMARK_INTENSITIES.len());
    for intensity in BENCHMARK_INTENSITIES {
        samples.push(run_intensity_window(
            &mut engine,
            device,
            intensity,
            seconds,
            &mut nonce_base,
        )?);
    }

    Ok(BenchmarkReport {
        status: "PASS",
        backend: "cuda",
        device,
        device_name,
        table_source,
        persistent_device_bytes,
        samples,
        network_access: false,
        broadcast: false,
    })
}

fn fmt_metric(value: Option<f64>, suffix: &str) -> String {
    value
        .map(|value| format!("{value:.1}{suffix}"))
        .unwrap_or_else(|| "n/a".into())
}

pub fn print_report(report: &BenchmarkReport, json: bool) {
    if json {
        match serde_json::to_string_pretty(report) {
            Ok(value) => println!("{value}"),
            Err(error) => eprintln!("error: serialize benchmark report: {error}"),
        }
        return;
    }

    println!("Pickaxe PHOTON benchmark: {}", report.status);
    println!("CUDA device {}: {}", report.device, report.device_name);
    println!("Generator table: {}", report.table_source);
    println!(
        "Persistent CUDA allocation: {} bytes",
        report.persistent_device_bytes
    );
    println!("Network access: none; broadcast: none");
    println!(
        "intensity  candidates/s  work/W  candidates  batches  winners  gpu util  power  temp  vram"
    );
    for sample in &report.samples {
        println!(
            "{:>8}%  {:>12.0}  {:>6}  {:>10}  {:>7}  {:>7}  {:>8}  {:>6}  {:>5}  {:>7}",
            sample.intensity,
            sample.candidates_per_second,
            fmt_metric(sample.candidates_per_watt, ""),
            sample.candidates,
            sample.batches,
            sample.gpu_winners,
            fmt_metric(sample.telemetry.gpu_utilization_percent, "%"),
            fmt_metric(sample.telemetry.power_watts, "W"),
            fmt_metric(sample.telemetry.temperature_c, "C"),
            fmt_metric(sample.telemetry.vram_used_mib, "MiB"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_matrix_covers_required_intensities() {
        assert_eq!(BENCHMARK_INTENSITIES, [10, 25, 50, 75, 100]);
    }

    #[test]
    fn benchmark_fixture_matches_reference_layout() {
        let fixture = benchmark_fixture().unwrap();
        assert_eq!(fixture.template.len(), TX_BYTES);
        assert_eq!(
            fixture.template[TARGET_OFFSET..TARGET_OFFSET + 32],
            fixture.target
        );
        assert_eq!(fixture.private_key[31], 1);
    }

    #[test]
    fn nvidia_telemetry_parser_handles_values_and_na() {
        let parsed = parse_nvidia_smi_line("87, 123.5, 68, 2048, 2450, N/A").unwrap();
        assert_eq!(parsed.gpu_utilization_percent, Some(87.0));
        assert_eq!(parsed.power_watts, Some(123.5));
        assert_eq!(parsed.temperature_c, Some(68.0));
        assert_eq!(parsed.vram_used_mib, Some(2048.0));
        assert_eq!(parsed.graphics_clock_mhz, Some(2450.0));
        assert_eq!(parsed.memory_clock_mhz, None);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn benchmark_refuses_debug_measurements_before_gpu_initialization() {
        let error = run_cuda_benchmark(0, "unused".into(), 1).unwrap_err();
        assert!(error.contains("release build"));
    }
}
