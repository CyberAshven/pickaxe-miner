//! #### PR #65: one app process can leave mining, choose a profile, and
//! start another session after its workers and journal locks have closed.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock, Weak,
};

static PROFILES: AtomicBool = AtomicBool::new(false);
#[derive(Default)]
struct StopState {
    target: Weak<AtomicBool>,
    pending: bool,
}

impl StopState {
    fn bind(&mut self, stop: &Arc<AtomicBool>) {
        self.target = Arc::downgrade(stop);
        if std::mem::take(&mut self.pending) {
            stop.store(true, Ordering::SeqCst);
        }
    }

    fn signal(&mut self) {
        if let Some(stop) = self.target.upgrade() {
            stop.store(true, Ordering::SeqCst);
        } else {
            // The previous UI/workers are gone. Carry the request across the
            // handoff so the next owner can unwind normally, never exit here.
            self.pending = true;
        }
    }
}

type StopTarget = Arc<Mutex<StopState>>;
static STOP: OnceLock<Result<StopTarget, String>> = OnceLock::new();

fn handle_signal(target: &StopTarget) {
    PROFILES.store(false, Ordering::Release);
    target.lock().unwrap_or_else(|e| e.into_inner()).signal();
}

pub fn request_profiles() {
    PROFILES.store(true, Ordering::Release);
}
pub fn profiles_requested() -> bool {
    PROFILES.load(Ordering::Acquire)
}
pub fn take_profiles_request() -> bool {
    PROFILES.swap(false, Ordering::AcqRel)
}

/// Install once; the signal follows the active session rather than retaining
/// a closed worker's stop flag across visits to the profile picker.
pub fn install_stop(stop: &Arc<AtomicBool>) -> Result<(), String> {
    let target = STOP
        .get_or_init(|| {
            let target = Arc::new(Mutex::new(StopState::default()));
            let handler = Arc::clone(&target);
            ctrlc::set_handler(move || handle_signal(&handler))
                .map_err(|e| format!("install Ctrl+C handler: {e}"))?;
            Ok(target)
        })
        .as_ref()
        .map_err(Clone::clone)?;
    target.lock().unwrap_or_else(|e| e.into_inner()).bind(stop);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_shutdown_signal_can_be_rebound_after_a_profile_visit() {
        let first = Arc::new(AtomicBool::new(false));
        install_stop(&first).unwrap();
        let next = Arc::new(AtomicBool::new(false));
        install_stop(&next).unwrap();
        let target = STOP.get().unwrap().as_ref().unwrap();
        target.lock().unwrap().signal();
        assert!(!first.load(Ordering::Relaxed));
        assert!(next.load(Ordering::Relaxed));
    }

    #[test]
    fn signals_follow_electric_profiles_and_photon_without_abrupt_exit() {
        let mut routing = StopState::default();
        let electric = Arc::new(AtomicBool::new(false));
        routing.bind(&electric);
        routing.signal();
        assert!(electric.load(Ordering::SeqCst));
        drop(electric);
        let profiles = Arc::new(AtomicBool::new(false));
        routing.bind(&profiles);
        assert!(!profiles.load(Ordering::SeqCst));
        routing.signal();
        assert!(profiles.load(Ordering::SeqCst));
        drop(profiles);
        let photon = Arc::new(AtomicBool::new(false));
        routing.bind(&photon);
        assert!(!photon.load(Ordering::SeqCst));
        routing.signal();
        assert!(photon.load(Ordering::SeqCst));
    }

    #[test]
    fn signal_during_handoff_is_delivered_once_to_next_owner() {
        let mut routing = StopState::default();
        let previous = Arc::new(AtomicBool::new(false));
        routing.bind(&previous);
        drop(previous);
        routing.signal();
        let profiles = Arc::new(AtomicBool::new(false));
        routing.bind(&profiles);
        assert!(profiles.load(Ordering::SeqCst));
        drop(profiles);
        let next = Arc::new(AtomicBool::new(false));
        routing.bind(&next);
        assert!(!next.load(Ordering::SeqCst));
    }
}
