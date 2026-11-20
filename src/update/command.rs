//! `pickaxe update`: check, install, roll back, status and settings, as
//! `pickaxe p2pool` does for P2Pool.
use super::install::{self, Installed};
use super::release::{self, Source, Version};
use super::{decide, home, now, offer, running_tag, trust, Ask, Decision, Settings};
use crate::cli::UpdateCommand;
use std::io::{IsTerminal, Write};
use std::path::Path;

/// Runs one `pickaxe update` command.
pub fn run(
    config_path: &Path,
    command: Option<&UpdateCommand>,
    check_only: bool,
    yes: bool,
) -> Result<(), String> {
    super::init(config_path);
    let home = home(config_path);
    match command {
        Some(UpdateCommand::Status) => {
            print!("{}", status(&home, || super::blocked(config_path))?);
            Ok(())
        }
        Some(UpdateCommand::Rollback) => {
            let back = install::rollback(&home, &running_tag())?;
            let version = back
                .version()
                .map_or_else(|| back.tag.clone(), |version| version.to_string());
            if back.tag == running_tag() {
                println!("Pickaxe {version} already runs; the update that waited was dropped.");
            } else {
                println!(
                    "Pickaxe {version} starts the next time you start Pickaxe (restart to \
                     apply); `pickaxe update rollback` again goes forward."
                );
            }
            Ok(())
        }
        Some(UpdateCommand::Set { auto_update, check }) => {
            let mut settings = Settings::load(&home)?;
            if let Some(value) = auto_update {
                settings.auto_update = Some(on_off(value, "--auto-update")?);
            }
            if let Some(value) = check {
                settings.check = on_off(value, "--check")?;
            }
            settings.save(&home)?;
            println!("Pickaxe updates: {}.", settings.describe());
            Ok(())
        }
        None => update(&home, check_only, yes),
    }
}

fn on_off(value: &str, flag: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!("{flag} {value}: on or off")),
    }
}

/// Checks GitHub now and installs a newer release: a signed one at once, an
/// unsigned one after saying why and getting a yes (or `--yes`).
fn update(home: &Path, check_only: bool, yes: bool) -> Result<(), String> {
    let source = Source::github();
    let release = release::check(&source, home, now())?;
    let Some(offer) = offer(&source, home, release, trust::TRUSTED_KEYS)? else {
        match Installed::load(home)?.pending {
            Some(pending) => println!(
                "Pickaxe {} is installed and starts the next time you start Pickaxe.",
                pending
                    .version()
                    .map_or_else(|| pending.tag.clone(), |version| version.to_string())
            ),
            None => println!("Pickaxe {} is up to date.", Version::running()),
        }
        return Ok(());
    };
    println!(
        "Pickaxe {} is available (this is {}). Release notes: {}",
        offer.version(),
        Version::running(),
        offer.release.notes_url
    );
    match &offer.trust {
        trust::Trust::Signed { key } => println!("Signed with Pickaxe's key ({key}…)."),
        trust::Trust::Unsigned(why) => println!("Unsigned release: {}.", why.why()),
    }
    if check_only {
        println!("`pickaxe update` installs it.");
        return Ok(());
    }
    match decide(&offer.trust, Ask::Person { confirmed: yes }) {
        Decision::Install => {}
        Decision::Skip => return Ok(()),
        Decision::Confirm => {
            if !std::io::stdin().is_terminal() {
                return Err(
                    "an unsigned release installs only when you confirm it: run `pickaxe update \
                     --yes` to install it anyway"
                        .into(),
                );
            }
            print!(
                "Install unsigned Pickaxe {} anyway? Check the release notes first. [y/N] ",
                offer.version()
            );
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
                println!("Kept Pickaxe {}.", Version::running());
                return Ok(());
            }
        }
    }
    let progress = |percent: u8| {
        eprint!("\rDownloading {}… {percent}%", offer.release.asset);
    };
    let archive = install::download(&source, &offer.release, &progress);
    eprintln!();
    let entry = install::install_archive(
        home,
        &offer.release.tag,
        &offer.release.asset,
        &archive?,
        offer.expected,
        offer.trust.is_signed(),
    )?;
    println!(
        "Installed Pickaxe {} (SHA-256 {} as the {} checksum file gives). It replaces this \
         version the next time Pickaxe starts (restart to apply).",
        offer.version(),
        entry.sha256,
        offer.trust.label()
    );
    Ok(())
}

/// `pickaxe update status`: what runs, what waits (and, from `blocked`,
/// why it would wait now), what can be rolled back to, the last check and
/// the settings. Asks GitHub nothing.
fn status(home: &Path, blocked: impl FnOnce() -> Option<String>) -> Result<String, String> {
    use std::fmt::Write;
    let record = Installed::load(home)?;
    let settings = Settings::load(home)?;
    let cache = release::Cache::load(home);
    let named = |entry: &install::Entry| {
        entry
            .version()
            .map_or_else(|| entry.tag.clone(), |version| version.to_string())
    };
    let mut out = String::new();
    let program = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let _ = writeln!(out, "running:   {} ({program})", Version::running());
    let _ = match &record.pending {
        Some(pending) => writeln!(
            out,
            "installed: {} ({}), starts at the next start{}",
            named(pending),
            if pending.signed { "signed" } else { "unsigned" },
            blocked().map_or(String::new(), |why| format!("; it waits now: {why}"))
        ),
        None => writeln!(out, "installed: nothing waits"),
    };
    let _ = match &record.previous {
        Some(previous) => writeln!(
            out,
            "previous:  {} (`pickaxe update rollback` returns to it)",
            named(previous)
        ),
        None => writeln!(out, "previous:  none"),
    };
    let _ = match (&cache.release, &cache.problem) {
        _ if cache.checked_at == 0 => writeln!(out, "newest:    not checked yet"),
        (Some(release), _) => writeln!(
            out,
            "newest:    {}, checked {} ago ({})",
            release.version,
            ago(now().saturating_sub(cache.checked_at)),
            release.notes_url
        ),
        (None, Some(problem)) => writeln!(out, "newest:    none to offer ({problem})"),
        (None, None) => writeln!(out, "newest:    unknown"),
    };
    let _ = writeln!(out, "updates:   {}", settings.describe());
    let _ = writeln!(
        out,
        "signing:   {}",
        if trust::has_key() {
            format!(
                "this build trusts {} key(s); only releases signed with one install by themselves",
                trust::TRUSTED_KEYS.len()
            )
        } else {
            "this build has no signing key; no release installs by itself".into()
        }
    );
    let _ = writeln!(
        out,
        "releases:  https://github.com/{}/releases",
        release::REPOSITORY
    );
    Ok(out)
}

fn ago(seconds: u64) -> String {
    match seconds {
        0..=119 => format!("{seconds} s"),
        120..=7199 => format!("{} min", seconds / 60),
        _ => format!("{} h", seconds / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_the_record_and_settings_without_asking_github() {
        let dir = super::super::test_dir("status");
        let home = home(&dir.join("config.json"));
        let text = status(&home, || None).unwrap();
        assert!(text.contains("installed: nothing waits"), "{text}");
        assert!(text.contains("previous:  none"), "{text}");
        assert!(text.contains("not checked yet"), "{text}");
        assert!(text.contains("no signing key"), "{text}");
        assert!(text.contains("tells you"), "{text}");
        let record = Installed {
            pending: Some(install::Entry {
                tag: "pickaxe-miner-v9.9.9".into(),
                folder: home.join("versions").join("x"),
                sha256: String::new(),
                signed: false,
                installed_at: 0,
            }),
            ..Installed::default()
        };
        record.save(&home).unwrap();
        let text = status(&home, || Some("a claim is pending".into())).unwrap();
        assert!(
            text.contains(
                "installed: v9.9.9 (unsigned), starts at the next start; it waits now: a claim \
                 is pending"
            ),
            "{text}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_take_on_and_off() {
        assert!(on_off("ON", "--check").unwrap());
        assert!(!on_off("off", "--check").unwrap());
        assert!(on_off("maybe", "--auto-update")
            .unwrap_err()
            .contains("on or off"));
        assert_eq!(ago(30), "30 s");
        assert_eq!(ago(600), "10 min");
        assert_eq!(ago(7 * 3600), "7 h");
    }
}
