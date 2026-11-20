//! Installing a release: download, checked against the checksum the
//! (signed) checksum file gives, unpacked into `versions/<tag>/`, recorded
//! in `installed.json` as pending. Nothing in the archive runs, and nothing
//! of it is unpacked, before its SHA-256 matches.
//!
//! The running Pickaxe is replaced only at a start (`apply`), never under a
//! session: the old files are renamed aside (Windows lets a running program
//! be renamed, not overwritten), the new ones copied in, and the version
//! that ran is kept to roll back to. The files set aside are removed at a
//! later start, once nothing runs from them.
use super::release::{Release, Source, Version, BINARY};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The largest archive downloaded.
pub const ARCHIVE_LIMIT: u64 = 512 << 20;
/// Files set aside by a swap, beside the program: `.pickaxe-old-<time>`.
const ASIDE_PREFIX: &str = ".pickaxe-old-";

/// One installed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub tag: String,
    /// The folder holding its files, `pickaxe(.exe)` among them.
    pub folder: PathBuf,
    /// The archive's SHA-256, as checked (empty for a version saved from
    /// what ran, which came from no archive here).
    #[serde(default)]
    pub sha256: String,
    /// Whether its checksum file carried a trusted signature.
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub installed_at: u64,
}

impl Entry {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag)
    }
}

/// `installed.json`: what runs, what ran before it, and what starts next.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Installed {
    /// The version an update put in place (none until the first one).
    pub current: Option<Entry>,
    /// The version before it, kept for `pickaxe update rollback`.
    pub previous: Option<Entry>,
    /// Installed and waiting for the next start.
    pub pending: Option<Entry>,
}

impl Installed {
    fn path(home: &Path) -> PathBuf {
        home.join("installed.json")
    }

    pub fn load(home: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(Self::path(home)) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", Self::path(home).display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", Self::path(home).display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        super::write_file(&Self::path(home), text.as_bytes())
    }
}

/// The SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Downloads `release`'s archive, at most its published size (and never
/// more than `ARCHIVE_LIMIT`), telling `progress` the share received.
pub fn download(
    source: &Source,
    release: &Release,
    progress: &dyn Fn(u8),
) -> Result<Vec<u8>, String> {
    let size = release.asset_size;
    if size == 0 || size > ARCHIVE_LIMIT {
        return Err(format!(
            "{} is {size} bytes; Pickaxe downloads 1 to {ARCHIVE_LIMIT}",
            release.asset
        ));
    }
    let report = |received: usize| {
        let percent = (received as u64).saturating_mul(100) / size.max(1);
        progress(percent.min(99) as u8);
    };
    let body = source
        .get(
            &release.asset_url,
            "application/octet-stream",
            size as usize,
            None,
            Some(&report),
        )?
        .ok(&release.asset)?;
    if body.len() as u64 != size {
        return Err(format!(
            "{}: {} bytes arrived of {size}; nothing was installed",
            release.asset,
            body.len()
        ));
    }
    Ok(body)
}

/// Installs a downloaded archive whose SHA-256 must be `expected`: unpacks
/// it into `versions/<tag>/` and records it as pending for the next start.
pub fn install_archive(
    home: &Path,
    tag: &str,
    asset: &str,
    archive: &[u8],
    expected: [u8; 32],
    signed: bool,
) -> Result<Entry, String> {
    let actual = sha256(archive);
    if actual != expected {
        return Err(format!(
            "{asset}: the download's SHA-256 is {}, not the published {}; nothing was installed",
            hex::encode(actual),
            hex::encode(expected)
        ));
    }
    let versions = home.join("versions");
    let folder = safe_name(tag);
    let staging = versions.join(format!(".{folder}.partial"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| format!("{}: {error}", staging.display()))?;
    let archive_path = staging.join(safe_name(asset));
    std::fs::write(&archive_path, archive)
        .map_err(|error| format!("{}: {error}", archive_path.display()))?;
    let unpacked = extract(&archive_path, &staging);
    let _ = std::fs::remove_file(&archive_path);
    if let Err(error) = unpacked {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let Some(root) = package_root(&staging, 2) else {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("{asset} has no {BINARY}; nothing was installed"));
    };
    let destination = versions.join(&folder);
    let mut record = Installed::load(home)?;
    let kept = [&record.current, &record.previous]
        .into_iter()
        .flatten()
        .find(|entry| entry.folder.starts_with(&destination) && entry.folder.join(BINARY).is_file())
        .map(|entry| entry.folder.clone());
    let folder = match kept {
        // Already here (what runs, or the one to go back to): those files
        // stay as they are.
        Some(folder) => {
            let _ = std::fs::remove_dir_all(&staging);
            folder
        }
        None => {
            if destination.exists() {
                std::fs::remove_dir_all(&destination)
                    .map_err(|error| format!("{}: {error}", destination.display()))?;
            }
            std::fs::rename(&staging, &destination)
                .map_err(|error| format!("{}: {error}", destination.display()))?;
            destination.join(root.strip_prefix(&staging).unwrap_or(Path::new("")))
        }
    };
    let entry = Entry {
        tag: tag.to_owned(),
        folder,
        sha256: hex::encode(expected),
        signed,
        installed_at: super::now(),
    };
    if !entry.folder.join(BINARY).is_file() {
        return Err(format!("{} has no {BINARY}", entry.folder.display()));
    }
    record.pending = Some(entry.clone());
    record.save(home)?;
    Ok(entry)
}

/// The folder in `dir`, or at most `depth` below it, that holds the binary.
fn package_root(dir: &Path, depth: usize) -> Option<PathBuf> {
    if dir.join(BINARY).is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    folders
        .iter()
        .find_map(|folder| package_root(folder, depth - 1))
}

/// A tag or file name safe as one path component.
fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    match safe.trim_matches('.') {
        "" => "_".into(),
        trimmed => trimmed.to_owned(),
    }
}

/// The system's `tar` (bsdtar on Windows 10 and later, which reads zip
/// too): on Windows the one in System32, never one beside Pickaxe.
fn tar() -> std::process::Command {
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("SystemRoot") {
            let system = PathBuf::from(root).join("System32").join("tar.exe");
            if system.is_file() {
                return crate::child_process::command(system);
            }
        }
    }
    crate::child_process::command("tar")
}

/// Unpacks `archive` into `into`; tar refuses members outside the folder.
fn extract(archive: &Path, into: &Path) -> Result<(), String> {
    let output = tar()
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|error| format!("unpack with tar: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "unpack {}: {}",
            archive.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// What a start did about a pending update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Nothing waits.
    Nothing,
    /// The pending version is in place; the new program must be started.
    Swapped { tag: String, program: PathBuf },
    /// It waits for a later start, for this reason.
    Waiting { tag: String, why: String },
}

/// Puts the pending version in place of the program at `program`, which
/// runs `running`; `blocked` says why it must wait, if it must (asked only
/// when an update is pending). The files it replaces are renamed aside and
/// the version that ran is kept in `versions/` to roll back to.
pub fn apply(
    home: &Path,
    program: &Path,
    running: &str,
    blocked: impl FnOnce() -> Option<String>,
) -> Result<Applied, String> {
    let mut record = Installed::load(home)?;
    let Some(pending) = record.pending.clone() else {
        return Ok(Applied::Nothing);
    };
    let waiting = |why: String| {
        Ok(Applied::Waiting {
            tag: pending.tag.clone(),
            why,
        })
    };
    if pending.tag == running {
        // It runs already (started by hand from the new files).
        record.pending = None;
        record.save(home)?;
        return Ok(Applied::Nothing);
    }
    if let Some(why) = blocked() {
        return waiting(why);
    }
    let dir = program
        .parent()
        .ok_or_else(|| format!("{} has no folder", program.display()))?;
    if program.file_name() != Some(std::ffi::OsStr::new(BINARY)) {
        return waiting(format!(
            "{} is not a release package's {BINARY}, so it is not replaced; {} has the new files",
            program.display(),
            pending.folder.display()
        ));
    }
    if program.starts_with(home) {
        return waiting(format!(
            "{} runs from Pickaxe's own versions folder; start the installed Pickaxe instead",
            program.display()
        ));
    }
    if !pending.folder.join(BINARY).is_file() {
        record.pending = None;
        record.save(home)?;
        return Err(format!(
            "{} is gone; the update was dropped",
            pending.folder.display()
        ));
    }
    let names = top_level(&pending.folder)?;
    // What runs now is kept to roll back to: from versions/ when an update
    // put it there, otherwise copied from beside the program.
    let kept_folder = home.join("versions").join(safe_name(running));
    let kept = match [&record.current, &record.previous]
        .into_iter()
        .flatten()
        .find(|entry| entry.tag == running && entry.folder.join(BINARY).is_file())
    {
        Some(entry) => entry.clone(),
        None => {
            keep_copy(dir, &names, &kept_folder)?;
            Entry {
                tag: running.to_owned(),
                folder: kept_folder,
                sha256: String::new(),
                signed: false,
                installed_at: super::now(),
            }
        }
    };
    swap(dir, &pending.folder, &names)?;
    record.previous = Some(kept);
    record.current = Some(pending.clone());
    record.pending = None;
    record.save(home)?;
    prune(home, &record);
    Ok(Applied::Swapped {
        tag: pending.tag,
        program: program.to_path_buf(),
    })
}

/// The names at the top of a package folder.
fn top_level(folder: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    let mut names: Vec<_> = std::fs::read_dir(folder)
        .map_err(|error| format!("{}: {error}", folder.display()))?
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    Ok(names)
}

/// Copies `names` from `dir` into `into` (made whole, then renamed).
fn keep_copy(dir: &Path, names: &[std::ffi::OsString], into: &Path) -> Result<(), String> {
    let partial = into.with_extension("partial");
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    for name in names {
        let from = dir.join(name);
        if from.exists() {
            copy_all(&from, &partial.join(name))?;
        }
    }
    if into.exists() {
        std::fs::remove_dir_all(into).map_err(|error| format!("{}: {error}", into.display()))?;
    }
    std::fs::rename(&partial, into).map_err(|error| format!("{}: {error}", into.display()))
}

/// Renames each of `names` in `dir` aside, then copies the new one in from
/// `from`; on any failure, everything goes back as it was.
fn swap(dir: &Path, from: &Path, names: &[std::ffi::OsString]) -> Result<(), String> {
    let aside = dir.join(format!("{ASIDE_PREFIX}{}", super::now_millis()));
    std::fs::create_dir_all(&aside).map_err(|error| {
        format!(
            "{}: {error}; Pickaxe's folder must be writable to update it",
            aside.display()
        )
    })?;
    let mut moved = Vec::new();
    let mut placed = Vec::new();
    let result = (|| {
        for name in names {
            let target = dir.join(name);
            if target.exists() {
                std::fs::rename(&target, aside.join(name))
                    .map_err(|error| format!("{}: {error}", target.display()))?;
                moved.push(name.clone());
            }
            placed.push(name.clone());
            copy_all(&from.join(name), &target)?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        for name in &placed {
            let _ = remove_any(&dir.join(name));
        }
        for name in &moved {
            let _ = std::fs::rename(aside.join(name), dir.join(name));
        }
        let _ = std::fs::remove_dir_all(&aside);
        return Err(format!("{error}; the old files are back in place"));
    }
    Ok(())
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn copy_all(from: &Path, to: &Path) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(from).map_err(|error| format!("{}: {error}", from.display()))?;
    if metadata.is_dir() {
        std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
        for entry in
            std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?
        {
            let entry = entry.map_err(|error| format!("{}: {error}", from.display()))?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.is_file() {
        std::fs::copy(from, to)
            .map(drop)
            .map_err(|error| format!("{} to {}: {error}", from.display(), to.display()))
    } else {
        // Links and other special files are not part of a package.
        Ok(())
    }
}

/// Removes what earlier swaps set aside beside `program`; files still in
/// use (a program that still runs from them) stay for a later start.
pub fn clean_aside(program: &Path) {
    let Some(dir) = program.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(ASIDE_PREFIX))
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes versions no longer recorded (current, previous or pending).
fn prune(home: &Path, record: &Installed) {
    let versions = home.join("versions");
    let Ok(entries) = std::fs::read_dir(&versions) else {
        return;
    };
    let keep: Vec<&PathBuf> = [&record.current, &record.previous, &record.pending]
        .into_iter()
        .flatten()
        .map(|entry| &entry.folder)
        .collect();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !keep.iter().any(|kept| kept.starts_with(&path)) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Makes the version before the current one start next; the current one
/// becomes the one to go back to.
pub fn rollback(home: &Path, running: &str) -> Result<Entry, String> {
    let mut record = Installed::load(home)?;
    let previous = record
        .previous
        .clone()
        .ok_or("there is no earlier Pickaxe version to go back to")?;
    if !previous.folder.join(BINARY).is_file() {
        return Err(format!("{} is gone", previous.folder.display()));
    }
    if previous.tag == running {
        // Already running it: only a pending update is dropped.
        record.pending = None;
    } else {
        record.pending = Some(previous.clone());
    }
    record.save(home)?;
    Ok(previous)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An archive of this platform's kind holding `files`, inside a folder
    /// as the Linux and macOS packages are, or at its root as Windows'.
    pub(crate) fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let dir = super::super::test_dir("archive");
        let top = dir.join("package");
        for (name, bytes) in files {
            let path = top.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let zip = cfg!(windows);
        let out = dir.join(if zip { "package.zip" } else { "package.tar.gz" });
        let mut tar = tar();
        if zip {
            tar.arg("-a")
                .arg("-c")
                .arg("-f")
                .arg(&out)
                .arg("-C")
                .arg(&top)
                .arg(".");
        } else {
            tar.arg("-c")
                .arg("-z")
                .arg("-f")
                .arg(&out)
                .arg("-C")
                .arg(&dir)
                .arg("package");
        }
        assert!(tar.status().unwrap().success());
        let bytes = std::fs::read(&out).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        bytes
    }

    fn release_files(text: &[u8]) -> Vec<u8> {
        archive(&[
            (BINARY, text),
            ("cuda/build/photon_rust.ptx", b"ptx"),
            ("README.md", b"readme"),
        ])
    }

    #[test]
    fn a_verified_archive_unpacks_into_its_version_folder_and_waits() {
        let home = super::super::test_dir("install");
        let bytes = release_files(b"pickaxe five");
        let sum = sha256(&bytes);
        // A tampered download installs nothing.
        let mut tampered = bytes.clone();
        tampered[bytes.len() / 2] ^= 1;
        let error = install_archive(&home, "pickaxe-miner-v0.0.5", "a.zip", &tampered, sum, true)
            .unwrap_err();
        assert!(error.contains("nothing was installed"), "{error}");
        assert_eq!(Installed::load(&home).unwrap(), Installed::default());
        assert!(!home.join("versions").join("pickaxe-miner-v0.0.5").exists());
        let entry =
            install_archive(&home, "pickaxe-miner-v0.0.5", "a.zip", &bytes, sum, true).unwrap();
        assert!(entry
            .folder
            .starts_with(home.join("versions").join("pickaxe-miner-v0.0.5")));
        assert_eq!(
            std::fs::read(entry.folder.join(BINARY)).unwrap(),
            b"pickaxe five"
        );
        assert!(entry.folder.join("cuda/build/photon_rust.ptx").is_file());
        assert!(entry.signed);
        let record = Installed::load(&home).unwrap();
        assert_eq!(record.pending, Some(entry));
        assert_eq!(record.current, None);
        // An archive without Pickaxe installs nothing.
        let empty = archive(&[("README.md", b"no program")]);
        let error = install_archive(
            &home,
            "pickaxe-miner-v0.0.6",
            "b.zip",
            &empty,
            sha256(&empty),
            false,
        )
        .unwrap_err();
        assert!(error.contains(BINARY), "{error}");
        assert_eq!(
            Installed::load(&home).unwrap().pending.unwrap().tag,
            "pickaxe-miner-v0.0.5"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    /// A Pickaxe installed by hand in `dir`, running `text`.
    fn installed_by_hand(dir: &Path, text: &[u8]) -> PathBuf {
        std::fs::create_dir_all(dir.join("cuda").join("build")).unwrap();
        std::fs::write(dir.join(BINARY), text).unwrap();
        std::fs::write(dir.join("cuda/build/photon_rust.ptx"), b"old ptx").unwrap();
        std::fs::write(dir.join("README.md"), b"old readme").unwrap();
        std::fs::write(dir.join("my-notes.txt"), b"mine").unwrap();
        dir.join(BINARY)
    }

    #[test]
    fn the_swap_happens_at_a_start_keeps_the_old_version_and_rolls_back() {
        let home = super::super::test_dir("swap-home");
        let folder = super::super::test_dir("swap-program");
        let program = installed_by_hand(&folder, b"pickaxe four");
        // Nothing pending: nothing to do.
        assert_eq!(
            apply(&home, &program, "pickaxe-miner-v0.0.4", || None).unwrap(),
            Applied::Nothing
        );
        let bytes = release_files(b"pickaxe five");
        install_archive(
            &home,
            "pickaxe-miner-v0.0.5",
            "a.zip",
            &bytes,
            sha256(&bytes),
            true,
        )
        .unwrap();
        // A pending claim (or another Pickaxe) makes it wait; nothing moves.
        let waiting = apply(&home, &program, "pickaxe-miner-v0.0.4", || {
            Some("a claim is pending".into())
        })
        .unwrap();
        assert_eq!(
            waiting,
            Applied::Waiting {
                tag: "pickaxe-miner-v0.0.5".into(),
                why: "a claim is pending".into()
            }
        );
        assert_eq!(std::fs::read(&program).unwrap(), b"pickaxe four");
        // A program that is not a release package's is never replaced.
        let other = folder.join("pickaxe_miner_dev");
        std::fs::write(&other, b"dev").unwrap();
        assert!(matches!(
            apply(&home, &other, "pickaxe-miner-v0.0.4", || None).unwrap(),
            Applied::Waiting { .. }
        ));
        // The start: the old files go aside, the new ones come in.
        let applied = apply(&home, &program, "pickaxe-miner-v0.0.4", || None).unwrap();
        assert_eq!(
            applied,
            Applied::Swapped {
                tag: "pickaxe-miner-v0.0.5".into(),
                program: program.clone()
            }
        );
        assert_eq!(std::fs::read(&program).unwrap(), b"pickaxe five");
        assert_eq!(
            std::fs::read(folder.join("cuda/build/photon_rust.ptx")).unwrap(),
            b"ptx"
        );
        assert_eq!(std::fs::read(folder.join("README.md")).unwrap(), b"readme");
        // Files that are not the package's stay.
        assert_eq!(std::fs::read(folder.join("my-notes.txt")).unwrap(), b"mine");
        let aside: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(ASIDE_PREFIX)
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            std::fs::read(aside[0].path().join(BINARY)).unwrap(),
            b"pickaxe four"
        );
        let record = Installed::load(&home).unwrap();
        assert_eq!(record.current.as_ref().unwrap().tag, "pickaxe-miner-v0.0.5");
        let kept = record.previous.clone().unwrap();
        assert_eq!(kept.tag, "pickaxe-miner-v0.0.4");
        assert_eq!(
            std::fs::read(kept.folder.join(BINARY)).unwrap(),
            b"pickaxe four"
        );
        assert_eq!(record.pending, None);
        // A later start cleans what was set aside.
        clean_aside(&program);
        assert!(std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(ASIDE_PREFIX)));
        // Rollback: v0.0.4 starts next, v0.0.5 is kept to go forward again.
        let back = rollback(&home, "pickaxe-miner-v0.0.5").unwrap();
        assert_eq!(back.tag, "pickaxe-miner-v0.0.4");
        let applied = apply(&home, &program, "pickaxe-miner-v0.0.5", || None).unwrap();
        assert!(
            matches!(applied, Applied::Swapped { ref tag, .. } if tag == "pickaxe-miner-v0.0.4")
        );
        assert_eq!(std::fs::read(&program).unwrap(), b"pickaxe four");
        assert_eq!(
            std::fs::read(folder.join("cuda/build/photon_rust.ptx")).unwrap(),
            b"old ptx"
        );
        let record = Installed::load(&home).unwrap();
        assert_eq!(record.current.as_ref().unwrap().tag, "pickaxe-miner-v0.0.4");
        assert_eq!(
            record.previous.as_ref().unwrap().tag,
            "pickaxe-miner-v0.0.5"
        );
        assert!(record.previous.unwrap().folder.join(BINARY).is_file());
        std::fs::remove_dir_all(home).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_failed_swap_puts_the_old_files_back() {
        let home = super::super::test_dir("swap-fail-home");
        let folder = super::super::test_dir("swap-fail-program");
        let program = installed_by_hand(&folder, b"pickaxe four");
        let bytes = release_files(b"pickaxe five");
        let entry = install_archive(
            &home,
            "pickaxe-miner-v0.0.5",
            "a.zip",
            &bytes,
            sha256(&bytes),
            true,
        )
        .unwrap();
        // The binary and the PTX are swapped, then a file the new version
        // lacks makes the copy fail halfway.
        let names = [BINARY, "cuda", "README.md", "missing-file"].map(std::ffi::OsString::from);
        let error = swap(&folder, &entry.folder, &names).unwrap_err();
        assert!(error.contains("old files are back"), "{error}");
        assert_eq!(std::fs::read(&program).unwrap(), b"pickaxe four");
        assert_eq!(
            std::fs::read(folder.join("cuda/build/photon_rust.ptx")).unwrap(),
            b"old ptx"
        );
        assert_eq!(
            std::fs::read(folder.join("README.md")).unwrap(),
            b"old readme"
        );
        assert!(!folder.join("missing-file").exists());
        assert!(std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(ASIDE_PREFIX)));
        std::fs::remove_dir_all(home).unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }
}
