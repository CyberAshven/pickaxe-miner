//! Offline throughput benchmark for the selected production PHOTON GPU pipeline.
//! The benchmark never reads live baton state and never broadcasts.

use crate::backend::{BackendKind, GpuDevice};
use crate::telemetry::{sample_gpu_telemetry, GpuTelemetry};
use crate::{reward, search, tui, tx};
use secp256k1::{PublicKey, SecretKey};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TX_BYTES: usize = 615;
const TARGET_OFFSET: usize = 394;
const BENCHMARK_INTENSITIES: [u8; 5] = [10, 25, 50, 75, 100];
const MAX_BENCHMARK_WINDOW_SECONDS: u64 = 12 * 60;
const VECTOR_BATON_TXID: &str = "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042";
const VECTOR_TARGET_LE: &str = "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000";
const VECTOR_AGE: u32 = 10;
const VECTOR_CONTRACT_VALUE_SATS: u64 = 15_971_500;
const VECTOR_TOKEN_AMOUNT: u128 = 2_099_905_002_035_715;
const VECTOR_REWARD_RAW: u128 = 4_999_773_813;
const TELEMETRY_INTERVAL: Duration = Duration::from_millis(500);
const MATRIX_MONOTONIC_TOLERANCE_PERCENT: f64 = 12.5;
const MIN_MATRIX_100_TO_10_THROUGHPUT_RATIO: f64 = 2.0;

fn benchmark_intensities(requested: Option<u8>) -> Result<Vec<u8>, String> {
    match requested {
        Some(intensity) if (10..=100).contains(&intensity) => Ok(vec![intensity]),
        Some(_) => Err("benchmark intensity must be 10..=100".into()),
        None => Ok(BENCHMARK_INTENSITIES.to_vec()),
    }
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
    pub telemetry: GpuTelemetry,
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
    pub matrix_validation: Option<BenchmarkMatrixValidation>,
    pub ui_comparison: Option<UiComparison>,
    pub network_access: bool,
    pub broadcast: bool,
}

impl BenchmarkReport {
    pub fn passed(&self) -> bool {
        self.status == "PASS"
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkMatrixValidation {
    pub required_intensities_present: bool,
    pub approximately_monotonic_throughput: bool,
    pub monotonic_tolerance_percent: f64,
    pub minimum_100_to_10_throughput_ratio: f64,
    pub observed_100_to_10_throughput_ratio: Option<f64>,
    pub real_scaling_observed: bool,
    pub telemetry_load_increase_observed: Option<bool>,
}

impl BenchmarkMatrixValidation {
    fn passed(&self) -> bool {
        self.required_intensities_present
            && self.approximately_monotonic_throughput
            && self.real_scaling_observed
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UiComparison {
    pub renderer: &'static str,
    pub requested_seconds: u64,
    pub draw_interval_millis: u64,
    pub tui_draws: u64,
    pub throughput_delta_percent: f64,
    pub headless: BenchmarkSample,
    pub tui: BenchmarkSample,
}

struct BenchmarkWindow {
    sample: BenchmarkSample,
    tui_draws: u64,
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
    fn add(&mut self, sample: &GpuTelemetry) {
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

    fn finish(self) -> GpuTelemetry {
        GpuTelemetry {
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

fn validate_intensity_matrix(samples: &[BenchmarkSample]) -> BenchmarkMatrixValidation {
    let required_intensities_present = samples.len() == BENCHMARK_INTENSITIES.len()
        && samples
            .iter()
            .map(|sample| sample.intensity)
            .eq(BENCHMARK_INTENSITIES);
    let tolerance_multiplier = 1.0 - MATRIX_MONOTONIC_TOLERANCE_PERCENT / 100.0;
    let approximately_monotonic_throughput = required_intensities_present
        && samples.windows(2).all(|pair| {
            pair[1].candidates_per_second >= pair[0].candidates_per_second * tolerance_multiplier
        });

    let observed_100_to_10_throughput_ratio = match (samples.first(), samples.last()) {
        (Some(low), Some(high)) if low.candidates_per_second > 0.0 => {
            Some(high.candidates_per_second / low.candidates_per_second)
        }
        _ => None,
    };
    let real_scaling_observed = required_intensities_present
        && observed_100_to_10_throughput_ratio
            .is_some_and(|ratio| ratio >= MIN_MATRIX_100_TO_10_THROUGHPUT_RATIO);

    let telemetry_load_increase_observed = match (samples.first(), samples.last()) {
        (Some(low), Some(high)) => {
            let utilization_increased = match (
                low.telemetry.gpu_utilization_percent,
                high.telemetry.gpu_utilization_percent,
            ) {
                (Some(low), Some(high)) => Some(high > low),
                _ => None,
            };
            let power_increased = match (low.telemetry.power_watts, high.telemetry.power_watts) {
                (Some(low), Some(high)) => Some(high > low),
                _ => None,
            };
            match (utilization_increased, power_increased) {
                (Some(utilization), Some(power)) => Some(utilization || power),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            }
        }
        _ => None,
    };

    BenchmarkMatrixValidation {
        required_intensities_present,
        approximately_monotonic_throughput,
        monotonic_tolerance_percent: MATRIX_MONOTONIC_TOLERANCE_PERCENT,
        minimum_100_to_10_throughput_ratio: MIN_MATRIX_100_TO_10_THROUGHPUT_RATIO,
        observed_100_to_10_throughput_ratio,
        real_scaling_observed,
        telemetry_load_increase_observed,
    }
}

fn start_telemetry_sampler(
    backend: BackendKind,
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
            if let Some(sample) = sample_gpu_telemetry(backend, device) {
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
) -> GpuTelemetry {
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
    engine: &mut search::PhotonEngine,
    backend: BackendKind,
    device: u32,
    intensity: u8,
    seconds: u64,
    nonce_base: &mut u32,
    render_tui: bool,
) -> Result<BenchmarkWindow, String> {
    let requested = Duration::from_secs(seconds);
    let (telemetry_stop, telemetry_samples, telemetry_worker) =
        start_telemetry_sampler(backend, device);
    let tui_worker = render_tui.then(|| {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || tui::benchmark_render_load(worker_stop));
        (stop, handle)
    });
    let started = Instant::now();
    let mut candidates = 0u64;
    let mut batches = 0u64;
    let mut winners = 0u64;

    while started.elapsed() < requested {
        let batch_candidates = engine.scheduled_batch_candidates(intensity);
        let batch_started = Instant::now();
        let result = match engine.search_batch(*nonce_base, batch_candidates) {
            Ok(result) => result,
            Err(error) => {
                if let Some((stop, handle)) = tui_worker {
                    stop.store(true, Ordering::Release);
                    let _ = handle.join();
                }
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
            thread::park_timeout(rest);
        }
    }

    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let tui_draws = if let Some((stop, handle)) = tui_worker {
        stop.store(true, Ordering::Release);
        handle
            .join()
            .map_err(|_| "Ratatui benchmark renderer panicked".to_string())??
    } else {
        0
    };
    let telemetry = finish_telemetry_sampler(telemetry_stop, telemetry_samples, telemetry_worker);
    let candidates_per_second = candidates as f64 / elapsed;
    let candidates_per_watt = telemetry
        .power_watts
        .filter(|watts| *watts > 0.0)
        .map(|watts| candidates_per_second / watts);
    Ok(BenchmarkWindow {
        sample: BenchmarkSample {
            intensity,
            requested_seconds: seconds,
            elapsed_seconds: elapsed,
            candidates,
            batches,
            candidates_per_second,
            candidates_per_watt,
            gpu_winners: winners,
            telemetry,
        },
        tui_draws,
    })
}

pub fn run_gpu_benchmark(
    device: &GpuDevice,
    seconds: u64,
    requested_intensity: Option<u8>,
    ui_compare: bool,
) -> Result<BenchmarkReport, String> {
    if !(1..=MAX_BENCHMARK_WINDOW_SECONDS).contains(&seconds) {
        return Err(format!(
            "benchmark --seconds must be 1..={MAX_BENCHMARK_WINDOW_SECONDS}"
        ));
    }
    if cfg!(debug_assertions) {
        return Err("benchmark requires an optimized release build".into());
    }
    let intensities = benchmark_intensities(requested_intensity)?;
    let fixture = benchmark_fixture()?;
    let mut engine = search::PhotonEngine::new(
        device.backend,
        device.index as usize,
        search::production_max_batch_candidates(device.backend),
        search::WINNER_BUFFER_CAP,
    )?;
    let persistent_device_bytes = engine.persistent_device_bytes();
    let table_source = engine.table_source();
    engine.set_job(&fixture.template, &fixture.target, &fixture.private_key)?;

    // Prime kernels and page residency before measuring any intensity window.
    let mut nonce_base = 0u32;
    let warmup = engine.search_batch(nonce_base, engine.scheduled_batch_candidates(100))?;
    nonce_base = nonce_base.wrapping_add(warmup.candidates);

    let mut samples = Vec::with_capacity(intensities.len());
    for intensity in intensities {
        samples.push(
            run_intensity_window(
                &mut engine,
                device.backend,
                device.index,
                intensity,
                seconds,
                &mut nonce_base,
                false,
            )?
            .sample,
        );
    }
    let matrix_validation = requested_intensity
        .is_none()
        .then(|| validate_intensity_matrix(&samples));
    let status = if matrix_validation
        .as_ref()
        .is_none_or(BenchmarkMatrixValidation::passed)
    {
        "PASS"
    } else {
        "FAIL"
    };

    let ui_comparison = if ui_compare {
        let headless = run_intensity_window(
            &mut engine,
            device.backend,
            device.index,
            100,
            seconds,
            &mut nonce_base,
            false,
        )?;
        let tui = run_intensity_window(
            &mut engine,
            device.backend,
            device.index,
            100,
            seconds,
            &mut nonce_base,
            true,
        )?;
        let throughput_delta_percent = if headless.sample.candidates_per_second > 0.0 {
            (tui.sample.candidates_per_second / headless.sample.candidates_per_second - 1.0) * 100.0
        } else {
            0.0
        };
        Some(UiComparison {
            renderer: "ratatui-crossterm-sink",
            requested_seconds: seconds,
            draw_interval_millis: tui::benchmark_draw_interval().as_millis() as u64,
            tui_draws: tui.tui_draws,
            throughput_delta_percent,
            headless: headless.sample,
            tui: tui.sample,
        })
    } else {
        None
    };

    Ok(BenchmarkReport {
        status,
        backend: device.backend.as_str(),
        device: device.index,
        device_name: device.name.clone(),
        table_source,
        persistent_device_bytes,
        samples,
        matrix_validation,
        ui_comparison,
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
    println!("GPU backend: {}", report.backend);
    println!("Device {}: {}", report.device, report.device_name);
    println!("Generator table: {}", report.table_source);
    println!(
        "Persistent GPU allocation: {} bytes",
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
    if let Some(validation) = &report.matrix_validation {
        let span = validation
            .observed_100_to_10_throughput_ratio
            .map(|value| format!("{value:.2}x"))
            .unwrap_or_else(|| "n/a".into());
        let telemetry = validation
            .telemetry_load_increase_observed
            .map(|value| if value { "yes" } else { "no" })
            .unwrap_or("n/a");
        println!(
            "Intensity validation: required={} monotonic={} (tolerance {:.1}%) 100/10={} (min {:.1}x) telemetry-load-increase={}",
            validation.required_intensities_present,
            validation.approximately_monotonic_throughput,
            validation.monotonic_tolerance_percent,
            span,
            validation.minimum_100_to_10_throughput_ratio,
            telemetry,
        );
    }
    if let Some(comparison) = &report.ui_comparison {
        println!(
            "Ratatui render comparison @100%: headless {:.0}/s, TUI {:.0}/s ({:+.2}%), {} draws every {} ms [{}]",
            comparison.headless.candidates_per_second,
            comparison.tui.candidates_per_second,
            comparison.throughput_delta_percent,
            comparison.tui_draws,
            comparison.draw_interval_millis,
            comparison.renderer,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::parse_nvidia_smi_line;

    fn benchmark_sample(intensity: u8, candidates_per_second: f64) -> BenchmarkSample {
        BenchmarkSample {
            intensity,
            requested_seconds: 1,
            elapsed_seconds: 1.0,
            candidates: candidates_per_second as u64,
            batches: 1,
            candidates_per_second,
            candidates_per_watt: None,
            gpu_winners: 0,
            telemetry: GpuTelemetry::default(),
        }
    }

    fn benchmark_device(backend: BackendKind) -> GpuDevice {
        GpuDevice {
            index: 0,
            name: "unused".into(),
            vendor: "unused".into(),
            vram_bytes: None,
            backend,
            detail: String::new(),
        }
    }

    #[test]
    fn benchmark_matrix_covers_required_intensities() {
        assert_eq!(benchmark_intensities(None).unwrap(), [10, 25, 50, 75, 100]);
        assert_eq!(
            BENCHMARK_INTENSITIES.len() as u64 * MAX_BENCHMARK_WINDOW_SECONDS,
            60 * 60
        );
    }

    #[test]
    fn benchmark_explicit_intensity_selects_one_window() {
        assert_eq!(benchmark_intensities(Some(30)).unwrap(), [30]);
    }

    #[test]
    fn intensity_matrix_validation_accepts_real_monotonic_scaling() {
        let samples = [
            benchmark_sample(10, 20_000.0),
            benchmark_sample(25, 49_000.0),
            benchmark_sample(50, 96_000.0),
            benchmark_sample(75, 145_000.0),
            benchmark_sample(100, 190_000.0),
        ];
        let validation = validate_intensity_matrix(&samples);
        assert!(validation.passed());
        assert_eq!(validation.observed_100_to_10_throughput_ratio, Some(9.5));
    }

    #[test]
    fn intensity_matrix_validation_rejects_display_only_intensity() {
        let samples = BENCHMARK_INTENSITIES.map(|intensity| benchmark_sample(intensity, 100_000.0));
        let validation = validate_intensity_matrix(&samples);
        assert!(validation.required_intensities_present);
        assert!(validation.approximately_monotonic_throughput);
        assert!(!validation.real_scaling_observed);
        assert!(!validation.passed());
    }

    #[test]
    fn intensity_matrix_validation_tolerates_small_measurement_noise() {
        let samples = [
            benchmark_sample(10, 20_000.0),
            benchmark_sample(25, 51_000.0),
            benchmark_sample(50, 48_000.0),
            benchmark_sample(75, 138_000.0),
            benchmark_sample(100, 191_000.0),
        ];
        let validation = validate_intensity_matrix(&samples);
        assert!(validation.approximately_monotonic_throughput);
        assert!(validation.passed());
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
        let error =
            run_gpu_benchmark(&benchmark_device(BackendKind::Cuda), 1, None, false).unwrap_err();
        assert!(error.contains("release build"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn benchmark_accepts_all_production_backends_before_gpu_initialization() {
        for backend in [BackendKind::Cuda, BackendKind::Hip, BackendKind::Wgpu] {
            let error = run_gpu_benchmark(&benchmark_device(backend), 1, None, false).unwrap_err();
            assert!(
                error.contains("release build"),
                "{backend:?} failed before the release-build gate: {error}"
            );
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn benchmark_accepts_one_hour_matrix_window_without_initializing_gpu() {
        let device = benchmark_device(BackendKind::Cuda);
        let accepted =
            run_gpu_benchmark(&device, MAX_BENCHMARK_WINDOW_SECONDS, None, false).unwrap_err();
        assert!(accepted.contains("release build"));

        let rejected =
            run_gpu_benchmark(&device, MAX_BENCHMARK_WINDOW_SECONDS + 1, None, false).unwrap_err();
        assert!(rejected.contains("1..=720"));
    }
}
