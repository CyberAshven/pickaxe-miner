//! One GPU mining session per operating-system user, across Pickaxe windows.

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
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            "another Pickaxe GPU miner is already running; close it before starting this one"
                .to_string()
        }
        std::fs::TryLockError::Error(error) => format!("enforce GPU session lock: {error}"),
    })?;
    Ok(file)
}

#[cfg(windows)]
fn another_pickaxe_process_running() -> Result<bool, String> {
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

pub fn acquire_gpu_lock() -> Result<File, String> {
    let lock = try_lock(&gpu_lock_path()?)?;
    #[cfg(windows)]
    if another_pickaxe_process_running()? {
        return Err(
            "another Pickaxe GPU miner is already running; close it before starting this one"
                .into(),
        );
    }
    Ok(lock)
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

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a separate running pickaxe_miner.exe"]
    fn detects_running_legacy_miner() {
        assert!(another_pickaxe_process_running().unwrap());
        let error = acquire_gpu_lock().unwrap_err();
        assert!(error.contains("already running"), "{error}");
    }
}
