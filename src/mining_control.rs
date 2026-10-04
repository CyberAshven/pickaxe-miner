//! Shared intensity pacing and measured active throughput for native and browser miners.
//! #### PR #22
//! Keep timer accounting shared while each interface selects its display window.
//! Check candidate-weighted rates, live intensity changes and oversleep repayment.
use std::collections::VecDeque;
use std::time::Duration;

/// How much busy/wall history the duty pacer keeps before halving it.
const DUTY_WINDOW: Duration = Duration::from_secs(2);
/// Throttled work runs as one burst then one rest per period of this length.
const DUTY_PERIOD: Duration = Duration::from_millis(100);

// #### PR #22: share allocation capacity across both launchers. The portable
// engine starts with a tiny adaptive batch; using it as the allocation unit
// prevents later full batches. Preserve existing CUDA/HIP allocation cadence.
pub(crate) const fn work_allocation_quantum(
    backend: crate::backend_kind::BackendKind,
    initial_batch: u32,
) -> u64 {
    match backend {
        crate::backend_kind::BackendKind::Wgpu => {
            crate::gpu_types::PORTABLE_MAX_BATCH_CANDIDATES as u64
        }
        _ => initial_batch as u64 * 64,
    }
}

/// Paces throttled GPU batches to the requested duty cycle.
///
/// Sleeps round up to the OS timer tick (about 15.6 ms on Windows), so a
/// per-batch rest of a fraction of a millisecond becomes a 15 ms stall and
/// every intensity below 100 collapses to the same low rate. The pacer
/// instead accounts GPU-busy time against wall time and asks for rest only
/// while busy time is ahead of the requested share; an oversleep is repaid
/// by running the following batches back to back. Rest is taken in whole
/// periods (25% runs about 25 ms, then rests about 75 ms), so the GPU works
/// at full clocks instead of idling between tiny bursts.
#[derive(Debug, Clone)]
pub(crate) struct DutyPacer {
    intensity: u8,
    window_start: Duration,
    busy: Duration,
}

impl DutyPacer {
    /// Starts an empty pacing window.
    pub(crate) fn new(now: Duration) -> Self {
        Self {
            intensity: 100,
            window_start: now,
            busy: Duration::ZERO,
        }
    }

    /// Forgets pacing history, e.g. after a pause, so idle time is not
    /// spent later as a full-speed burst.
    pub(crate) fn reset(&mut self, now: Duration) {
        self.window_start = now;
        self.busy = Duration::ZERO;
    }

    /// Records one finished batch and returns how long to rest before the
    /// next one.
    pub(crate) fn record_batch(
        &mut self,
        intensity: u8,
        compute_time: Duration,
        now: Duration,
    ) -> Duration {
        let intensity = intensity.clamp(10, 100);
        if intensity != self.intensity {
            self.intensity = intensity;
            self.window_start = now.checked_sub(compute_time).unwrap_or(now);
            self.busy = Duration::ZERO;
        }
        if intensity >= 100 {
            self.reset(now);
            return Duration::ZERO;
        }
        self.busy = self.busy.saturating_add(compute_time);
        let elapsed = now.saturating_sub(self.window_start);
        let required = self.busy.saturating_add(duty_rest(self.busy, intensity));
        let rest = required.saturating_sub(elapsed);
        if elapsed >= DUTY_WINDOW {
            // Halve the history: the ratio is kept, old surplus or debt fades.
            self.busy /= 2;
            self.window_start = now.checked_sub(elapsed / 2).unwrap_or(now);
        }
        // Rest only in whole-period chunks. Short bursts between short rests
        // keep the GPU in a low clock state and cost throughput per busy ms.
        if rest < duty_rest_quantum(intensity) {
            return Duration::ZERO;
        }
        rest
    }
}

/// Idle part of one pacing period at the given intensity.
fn duty_rest_quantum(intensity: u8) -> Duration {
    DUTY_PERIOD * u32::from(100 - intensity.clamp(10, 100)) / 100
}

/// Calculates the pause needed to honor GPU intensity.
pub(crate) fn duty_rest(compute_time: Duration, intensity: u8) -> Duration {
    let intensity = intensity.clamp(10, 100);
    if intensity >= 100 || compute_time.is_zero() {
        return Duration::ZERO;
    }
    // Occupancy is intensity/100. A fixed short cap leaves 10% nearly as busy
    // as a full batch and collapses the 100%-to-10% candidate ratio.
    let rest_ns = compute_time
        .as_nanos()
        .saturating_mul(u128::from(100 - intensity))
        / u128::from(intensity);
    Duration::from_nanos(u64::try_from(rest_ns).unwrap_or(u64::MAX))
}

/// Recent completed throughput, including throttling and network waits.
// #### PR #22: the browser's primary rate must include intensity pauses and
// network waits. Retain bounded recent completion samples, counted only once,
// so moving the slider changes the displayed rate and an idle miner reaches zero.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct WallRate {
    samples: VecDeque<(Duration, u64)>,
    completed: u64,
    last_completed: Duration,
}

#[cfg(any(target_arch = "wasm32", test))]
impl WallRate {
    pub(crate) fn new(now: Duration) -> Self {
        Self {
            samples: VecDeque::from([(now, 0)]),
            completed: 0,
            last_completed: now,
        }
    }

    pub(crate) fn record(&mut self, candidates: u32, now: Duration) {
        self.completed += u64::from(candidates);
        if candidates > 0 {
            self.last_completed = now;
        }
        if now.saturating_sub(self.samples.back().unwrap().0) >= Duration::from_millis(250) {
            self.samples.push_back((now, self.completed));
        }
        self.trim(now);
    }

    fn trim(&mut self, now: Duration) {
        let cutoff = now.saturating_sub(Duration::from_secs(5));
        while self.samples.len() > 1 && self.samples[1].0 <= cutoff {
            self.samples.pop_front();
        }
    }

    pub(crate) fn rate(&mut self, now: Duration) -> f64 {
        self.trim(now);
        let (start, completed) = *self.samples.front().unwrap();
        let seconds = now.saturating_sub(start).as_secs_f64();
        if seconds == 0.0 || now.saturating_sub(self.last_completed) >= Duration::from_secs(5) {
            0.0
        } else {
            (self.completed - completed) as f64 / seconds
        }
    }
}

/// Time-weighted active throughput over eight completed time buckets.
/// Batches are counted once; short batches cannot dominate by averaging rates.
#[derive(Debug)]
pub(crate) struct ActiveRate {
    buckets: VecDeque<(u64, Duration)>,
    candidates: u64,
    elapsed: Duration,
    bucket_duration: Duration,
}

impl Default for ActiveRate {
    fn default() -> Self {
        Self::with_bucket_duration(Duration::from_millis(250))
    }
}

impl ActiveRate {
    pub(crate) fn with_bucket_duration(bucket_duration: Duration) -> Self {
        Self {
            buckets: VecDeque::new(),
            candidates: 0,
            elapsed: Duration::ZERO,
            bucket_duration: bucket_duration.max(Duration::from_millis(1)),
        }
    }

    pub(crate) fn record(&mut self, candidates: u32, elapsed: Duration) -> f64 {
        if candidates == 0 || elapsed.is_zero() {
            return self.rate();
        }
        self.candidates = self.candidates.saturating_add(u64::from(candidates));
        self.elapsed = self.elapsed.saturating_add(elapsed);
        if self.elapsed >= self.bucket_duration {
            self.buckets.push_back((self.candidates, self.elapsed));
            self.candidates = 0;
            self.elapsed = Duration::ZERO;
            while self.buckets.len() > 8 {
                self.buckets.pop_front();
            }
        }
        self.rate()
    }

    pub(crate) fn rate(&self) -> f64 {
        let (candidates, elapsed) = self.buckets.iter().fold(
            (self.candidates as f64, self.elapsed.as_secs_f64()),
            |(c, t), (n, d)| (c + *n as f64, t + d.as_secs_f64()),
        );
        if elapsed > 0.0 {
            candidates / elapsed
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_rate_tracks_live_intensity_and_becomes_zero_when_idle() {
        let mut rate = WallRate::new(Duration::ZERO);
        assert_eq!(rate.rate(Duration::ZERO), 0.0);
        for tick in 1..=1000 {
            rate.record(100, Duration::from_millis(tick * 10));
        }
        assert_eq!(rate.rate(Duration::from_secs(10)), 10_000.0);
        for tick in 1..=300 {
            rate.record(100, Duration::from_millis(10_000 + tick * 20));
        }
        assert_eq!(rate.rate(Duration::from_secs(16)), 5_000.0);
        // Include a partial sample bucket before a long network/submission wait.
        rate.record(100, Duration::from_millis(16_020));
        assert_eq!(rate.rate(Duration::from_secs(22)), 0.0);
        let mut fresh = WallRate::new(Duration::from_secs(30));
        fresh.record(100, Duration::from_millis(30_100));
        assert_eq!(fresh.rate(Duration::from_millis(30_100)), 1_000.0);
    }

    #[test]
    fn portable_allocation_allows_growth_and_conserves_a_full_work_cycle() {
        use crate::backend_kind::BackendKind;
        use crate::donation::{Schedule, Scheme};
        let quantum = work_allocation_quantum(BackendKind::Wgpu, 1024);
        let mut schedule = Schedule::new(Scheme::Work([200, 200]), quantum, 0).unwrap();
        let mut requested = 1024u32;
        let mut remaining = quantum * 50;
        let mut largest = 0;
        let mut completed = [0u64; 3];
        while remaining > 0 {
            let count = schedule.limit_batch(requested).min(remaining as u32);
            assert!(count > 0);
            completed[schedule.recipient() as usize] += u64::from(count);
            largest = largest.max(count);
            schedule.record(count).unwrap();
            remaining -= u64::from(count);
            requested = requested
                .saturating_mul(2)
                .min(crate::gpu_types::PORTABLE_MAX_BATCH_CANDIDATES);
        }
        assert_eq!(largest, crate::gpu_types::PORTABLE_MAX_BATCH_CANDIDATES);
        assert_eq!(completed, [quantum * 48, quantum, quantum]);
        assert_eq!(
            work_allocation_quantum(BackendKind::Cuda, 16_777_216),
            16_777_216 * 64
        );
        assert_eq!(
            work_allocation_quantum(BackendKind::Hip, 65_536),
            65_536 * 64
        );
    }
    #[test]
    fn rates_weight_work_by_time_and_forget_old_speed() {
        let mut rate = ActiveRate::default();
        rate.record(100, Duration::from_millis(1));
        assert_eq!(rate.record(900, Duration::from_millis(99)), 10_000.0);
        for _ in 0..10 {
            rate.record(500, Duration::from_millis(250));
        }
        assert_eq!(rate.rate(), 2_000.0);
        assert_eq!(rate.record(0, Duration::from_secs(20)), 2_000.0);
    }
}
