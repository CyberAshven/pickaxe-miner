//! #### PR #65: one app process can leave mining, choose a profile, and
//! start another session after its workers and journal locks have closed.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock, Weak,
};

static PROFILES: AtomicBool = AtomicBool::new(false);
type StopTarget = Arc<Mutex<Weak<AtomicBool>>>;
static STOP: OnceLock<Result<StopTarget, String>> = OnceLock::new();

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
            let target = Arc::new(Mutex::new(Weak::<AtomicBool>::new()));
            let handler = Arc::clone(&target);
            ctrlc::set_handler(move || {
                if let Some(stop) = handler.lock().unwrap_or_else(|e| e.into_inner()).upgrade() {
                    stop.store(true, Ordering::SeqCst);
                }
            })
            .map_err(|e| format!("install Ctrl+C handler: {e}"))?;
            Ok(target)
        })
        .as_ref()
        .map_err(Clone::clone)?;
    *target.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(stop);
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
        target
            .lock()
            .unwrap()
            .upgrade()
            .unwrap()
            .store(true, Ordering::SeqCst);
        assert!(!first.load(Ordering::Relaxed));
        assert!(next.load(Ordering::Relaxed));
    }
}
