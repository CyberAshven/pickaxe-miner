//! One GPU mining session per GPU and operating-system user, across Pickaxe
//! windows (#### PR #60: per GPU, not per PC, when GPUs report their PCI location).

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

fn gpu_lock_path() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is unavailable; cannot enforce the GPU session lock")?;
    #[cfg(not(windows))]
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable; cannot enforce the GPU session lock")?;
    Ok(root.join("Pickaxe").join("gpu.lock"))
}

fn try_lock(path: &Path) -> Result<File, String> {
    lock_file(
        path,
        false,
        "another Pickaxe GPU miner is already running; close it before starting this one",
    )
}

/// Locks `path` exclusively, or shared beside other shared holders; `busy`
/// is the error when another process holds it.
fn lock_file(path: &Path, shared: bool, busy: &str) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create GPU lock directory: {error}"))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("open GPU session lock: {error}"))?;
    let locked = if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    };
    locked.map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => busy.to_string(),
        std::fs::TryLockError::Error(error) => format!("enforce GPU session lock: {error}"),
    })?;
    Ok(file)
}

// #### PR #52: current processes mark themselves
// What: every Pickaxe process holds `run/<pid>.lock` beside the GPU lock
// while it runs (`register_process`). On Windows a GPU miner refuses to
// start only beside a `pickaxe_miner.exe` without that live mark: a v0.0.1
// or v0.0.2 miner, which takes no GPU lock. Current GPU miners still keep
// each other out with the GPU lock.
// Why: the name check alone also refused a GPU miner or rig beside a
// current ASIC server or a coordinator with no GPU, so one Windows PC could
// not run a GPU farm and an ASIC farm together.
// Look here if: a GPU miner starts beside an old miner, or is refused beside
// a server or coordinator.
fn process_mark_path(pid: u32) -> Result<PathBuf, String> {
    Ok(gpu_lock_path()?
        .with_file_name("run")
        .join(format!("{pid}.lock")))
}

/// This process's mark; it lasts until the value is dropped.
pub struct ProcessMark {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for ProcessMark {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

/// Marks this process as a current Pickaxe for as long as the result is
/// held. None when the mark cannot be made; other miners then treat this
/// process as an old one.
pub fn register_process() -> Option<ProcessMark> {
    let path = process_mark_path(std::process::id()).ok()?;
    let file = try_lock(&path).ok()?;
    Some(ProcessMark {
        file: Some(file),
        path,
    })
}

/// Whether process `pid` holds its mark (a current Pickaxe).
#[cfg_attr(not(windows), allow(dead_code))]
fn is_marked(pid: u32) -> bool {
    let Ok(path) = process_mark_path(pid) else {
        return false;
    };
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(_) => false,
    }
}

#[cfg(windows)]
fn old_pickaxe_process_running() -> Result<bool, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    // ponytail: legacy name detection can also block an idle setup window;
    // retire it once old miners are gone, or track process modes for ASIC mining.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "check existing Pickaxe miners: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let result = if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        Err(format!(
            "check existing Pickaxe miners: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        loop {
            let name_len = entry
                .szExeFile
                .iter()
                .position(|&character| character == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
            if entry.th32ProcessID != std::process::id()
                && (name.eq_ignore_ascii_case("pickaxe_miner.exe")
                    || name.eq_ignore_ascii_case("pickaxe.exe"))
                && !is_marked(entry.th32ProcessID)
            {
                break Ok(true);
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                let error = std::io::Error::last_os_error();
                break if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    Ok(false)
                } else {
                    Err(format!("check existing Pickaxe miners: {error}"))
                };
            }
        }
    };
    if unsafe { CloseHandle(snapshot) } == 0 {
        return Err(format!(
            "close Pickaxe process snapshot: {}",
            std::io::Error::last_os_error()
        ));
    }
    result
}

/// Every GPU, as a miner whose GPUs' PCI locations are unknown takes them.
pub fn acquire_gpu_lock() -> Result<File, String> {
    let lock = try_lock(&gpu_lock_path()?)?;
    refuse_beside_old_miner()?;
    Ok(lock)
}

fn refuse_beside_old_miner() -> Result<(), String> {
    #[cfg(windows)]
    if old_pickaxe_process_running()? {
        return Err(
            "an older Pickaxe GPU miner is already running; close it before starting this one"
                .into(),
        );
    }
    Ok(())
}

// #### PR #60
// What: a miner whose GPUs all report their PCI location shares the
// machine-wide lock and takes one lock per GPU, so two miners can run on
// different GPUs of one PC (PHOTON on one, ELECTRIC on another). A GPU with
// no PCI location, and every older build, takes the machine-wide lock
// exclusively, which still keeps every other miner off every GPU.
// Why: one lock per PC kept a second token's miner off an idle GPU.
// Look here if: two miners share one GPU, or a miner is refused beside one
// that mines another GPU.
/// What a GPU miner holds while it mines.
pub struct GpuLocks {
    _held: Vec<File>,
}

/// Locks the GPUs in `gpus` (see `GpuLocks`).
pub fn acquire_gpu_locks(gpus: &[crate::backend::GpuDevice]) -> Result<GpuLocks, String> {
    let held = lock_gpus(
        &gpu_lock_path()?,
        gpus.iter().map(|gpu| gpu.pci).collect::<Option<Vec<_>>>(),
    )?;
    refuse_beside_old_miner()?;
    Ok(GpuLocks { _held: held })
}

fn lock_gpus(
    machine: &Path,
    pcis: Option<Vec<crate::backend::PciAddress>>,
) -> Result<Vec<File>, String> {
    let Some(mut pcis) = pcis.filter(|pcis| !pcis.is_empty()) else {
        return Ok(vec![try_lock(machine)?]);
    };
    pcis.sort();
    pcis.dedup();
    let mut held = vec![lock_file(
        machine,
        true,
        "another Pickaxe GPU miner is using every GPU; close it before starting this one",
    )?];
    for pci in pcis {
        let name = format!(
            "gpu-{:04x}-{:02x}-{:02x}-{:x}.lock",
            pci.domain, pci.bus, pci.device, pci.function
        );
        let busy = format!(
            "the GPU at PCI {:02x}:{:02x}.{:x} is already mining in another Pickaxe; \
             choose another GPU with --device, or close that miner",
            pci.bus, pci.device, pci.function
        );
        held.push(lock_file(&machine.with_file_name(name), false, &busy)?);
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_gpu_session_is_rejected_until_first_closes() {
        let path =
            std::env::temp_dir().join(format!("pickaxe-gpu-lock-test-{}", std::process::id()));
        let first = try_lock(&path).unwrap();
        assert!(try_lock(&path).unwrap_err().contains("already running"));
        drop(first);
        let second = try_lock(&path).unwrap();
        drop(second);
        let _ = fs::remove_file(path);
    }

    // #### PR #60: miners on different GPUs run together; one on the same
    // GPU, or beside a miner holding every GPU, is refused.
    #[test]
    fn miners_share_the_pc_only_on_different_gpus() {
        use crate::backend::PciAddress;
        let dir = std::env::temp_dir().join(format!("pickaxe-gpu-locks-{}", std::process::id()));
        let machine = dir.join("gpu.lock");
        let pci = |bus| PciAddress {
            domain: 0,
            bus,
            device: 0,
            function: 0,
        };
        let photon = lock_gpus(&machine, Some(vec![pci(1)])).unwrap();
        let electric = lock_gpus(&machine, Some(vec![pci(0xc4), pci(0xc4)])).unwrap();
        assert!(lock_gpus(&machine, Some(vec![pci(1)]))
            .unwrap_err()
            .contains("already mining"));
        assert!(lock_gpus(&machine, None)
            .unwrap_err()
            .contains("already running"));
        drop((photon, electric));
        let every = lock_gpus(&machine, None).unwrap();
        assert!(lock_gpus(&machine, Some(vec![pci(1)]))
            .unwrap_err()
            .contains("every GPU"));
        drop(every);
        assert!(lock_gpus(&machine, Some(Vec::new())).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    // #### PR #52
    #[test]
    fn a_marked_process_is_told_from_an_unmarked_one() {
        let mark = register_process().expect("process mark");
        assert!(is_marked(std::process::id()));
        // A mark left by a process that ended is not held.
        let ended = u32::MAX - std::process::id() % 1000;
        let stale = process_mark_path(ended).unwrap();
        fs::write(&stale, b"").unwrap();
        assert!(!is_marked(ended));
        let _ = fs::remove_file(stale);
        assert!(!is_marked(ended - 1));
        let path = mark.path.clone();
        drop(mark);
        assert!(!is_marked(std::process::id()));
        assert!(!path.exists());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a separate running pickaxe_miner.exe from v0.0.2 or earlier"]
    fn detects_running_legacy_miner() {
        assert!(old_pickaxe_process_running().unwrap());
        let error = acquire_gpu_lock().unwrap_err();
        assert!(error.contains("already running"), "{error}");
    }
}
