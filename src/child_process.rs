//! Programs Pickaxe starts: GPU telemetry (`nvidia-smi`, `amd-smi`), file
//! permissions (`whoami`, `icacls`), Tailscale, P2Pool and its archives.

use std::ffi::OsStr;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

static WITHOUT_CONSOLE: AtomicBool = AtomicBool::new(false);

/// This process gave up its console: the desktop window released the one
/// Windows opened for a double-click. Programs it starts from then on open
/// no console window of their own.
pub fn console_released() {
    WITHOUT_CONSOLE.store(true, Ordering::Relaxed);
}

/// `Command::new(program)`; once this process has no console, on Windows
/// the program runs without opening one (its output is still captured).
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    if WITHOUT_CONSOLE.load(Ordering::Relaxed) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
