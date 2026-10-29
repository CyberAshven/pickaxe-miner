//! #### PR #42
//! Device logins the owner entered on the Device panel, for devices whose
//! firmware refuses its default login. They are kept in
//! `<config>.sv2-logins.json`, readable by the owner alone (0600 on Unix; the
//! owner's account alone on Windows), as plain text at rest like the config's
//! node RPC credentials. A login is used only for the firmware it was saved
//! for, since a DHCP lease can move an address to another device, and only
//! for addresses on the local network or Tailscale. It never reaches the
//! status file, the screen, a log or `Debug` output.

use super::device_api::queryable;
use asic_rs::core::traits::miner::{ExposeSecret, MinerAuth, SecretString};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    io::ErrorKind,
    net::IpAddr,
    path::{Path, PathBuf},
};

/// The logins file beside a server's config (`chipnet.json` keeps
/// `chipnet.sv2-logins.json`).
pub fn logins_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("sv2-logins.json")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    devices: Vec<Saved>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    ip: IpAddr,
    firmware: String,
    username: String,
    password: String,
}

/// One device's login, for the firmware it was saved for.
#[derive(Clone)]
pub struct Login {
    pub firmware: String,
    pub username: String,
    password: SecretString,
}

impl Login {
    pub fn auth(&self) -> MinerAuth {
        MinerAuth::new(self.username.clone(), self.password.expose_secret())
    }

    /// The password, for Pickaxe's own commands that need it (Avalon's
    /// `setpool`); never shown.
    pub fn password(&self) -> &str {
        self.password.expose_secret()
    }
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Login({}, password hidden)", self.firmware)
    }
}

/// The saved logins.
pub struct Logins {
    path: PathBuf,
    entries: BTreeMap<IpAddr, Login>,
}

impl fmt::Debug for Logins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Logins({} saved)", self.entries.len())
    }
}

impl Logins {
    /// Reads the logins beside this config; none when the file does not
    /// exist. A symlink or anything but a plain file is refused, the file is
    /// made owner-only again, and entries for addresses outside the local
    /// network are skipped.
    pub fn load(config_path: &Path) -> Result<Self, String> {
        let path = logins_path(config_path);
        let entries = read(&path)?;
        Ok(Self { path, entries })
    }

    /// The saved login for this device, only when it was saved for this
    /// firmware.
    #[cfg(test)]
    pub fn get(&self, ip: IpAddr, firmware: &str) -> Option<Login> {
        self.entries
            .get(&ip)
            .filter(|login| login.firmware == firmware)
            .cloned()
    }

    /// Every saved login, by address.
    pub fn all(&self) -> Vec<(IpAddr, Login)> {
        self.entries
            .iter()
            .map(|(ip, login)| (*ip, login.clone()))
            .collect()
    }

    /// Saves a login the device accepted. The file is read again first, so
    /// a login another Pickaxe (the server or its watch view) saved is kept,
    /// then written whole.
    pub fn save(
        &mut self,
        ip: IpAddr,
        firmware: &str,
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        if !queryable(ip) {
            return Err("only devices on the local network keep a login".into());
        }
        let mut entries = read(&self.path)?;
        entries.insert(
            ip,
            Login {
                firmware: firmware.to_owned(),
                username: username.to_owned(),
                password: SecretString::from(password.to_owned()),
            },
        );
        write(&self.path, &entries)?;
        self.entries = entries;
        Ok(())
    }

    /// Forgets a device's login.
    #[cfg(test)]
    pub fn forget(&mut self, ip: IpAddr) -> Result<(), String> {
        let mut entries = read(&self.path)?;
        if entries.remove(&ip).is_some() {
            write(&self.path, &entries)?;
        }
        self.entries = entries;
        Ok(())
    }
}

fn read(path: &Path) -> Result<BTreeMap<IpAddr, Login>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("read device logins {}: {error}", path.display())),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(format!(
                "device logins {} is not a plain file, so it is not read",
                path.display()
            ))
        }
        Ok(_) => (),
    }
    crate::config::restrict_private_config(path)?;
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read device logins {}: {error}", path.display()))?;
    let file: File = serde_json::from_slice(&bytes)
        .map_err(|error| format!("read device logins {}: {error}", path.display()))?;
    if file.version != 1 {
        return Err(format!(
            "device logins {} has version {}; this Pickaxe reads version 1",
            path.display(),
            file.version
        ));
    }
    Ok(file
        .devices
        .into_iter()
        .filter(|saved| queryable(saved.ip))
        .map(|saved| {
            (
                saved.ip,
                Login {
                    firmware: saved.firmware,
                    username: saved.username,
                    password: SecretString::from(saved.password),
                },
            )
        })
        .collect())
}

fn write(path: &Path, entries: &BTreeMap<IpAddr, Login>) -> Result<(), String> {
    let file = File {
        version: 1,
        devices: entries
            .iter()
            .map(|(ip, login)| Saved {
                ip: *ip,
                firmware: login.firmware.clone(),
                username: login.username.clone(),
                password: login.password().to_owned(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("write device logins {}: {error}", path.display()))?;
    crate::config::write_private_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (super::super::journal::TestDirectory, PathBuf) {
        let dir = super::super::journal::TestDirectory::new();
        let config = dir.0.join("chipnet.json");
        (dir, config)
    }

    // #### PR #42
    // What: a saved login comes back for its own firmware only, never for a
    // public address, never in Debug output, and its file is owner-only.
    // Look here if: Logins or write_private_atomic changes.
    #[test]
    fn logins_are_owner_only_bound_to_their_firmware_and_never_in_debug() {
        let (_dir, config) = store();
        let rig: IpAddr = "192.168.7.40".parse().unwrap();
        let mut logins = Logins::load(&config).unwrap();
        assert!(logins.all().is_empty());
        logins
            .save(rig, "AntMiner Stock", "root", "owner-secret")
            .unwrap();
        assert!(logins
            .save(
                "203.0.113.9".parse().unwrap(),
                "AntMiner Stock",
                "root",
                "x"
            )
            .is_err());
        let again = Logins::load(&config).unwrap();
        let login = again.get(rig, "AntMiner Stock").unwrap();
        assert_eq!(login.username, "root");
        assert_eq!(login.password(), "owner-secret");
        assert_eq!(login.auth().username(), "root");
        assert!(again.get(rig, "Braiins").is_none(), "another firmware");
        assert!(again
            .get("192.168.7.41".parse().unwrap(), "AntMiner Stock")
            .is_none());
        for debug in [format!("{again:?}"), format!("{login:?}")] {
            assert!(!debug.contains("owner-secret"), "{debug}");
            assert!(!debug.contains("192.168"), "{debug}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(logins_path(&config))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Saving again keeps what another Pickaxe saved meanwhile.
        let mut other = Logins::load(&config).unwrap();
        other
            .save("192.168.7.41".parse().unwrap(), "VNish", "", "second")
            .unwrap();
        logins
            .save(rig, "AntMiner Stock", "root", "changed")
            .unwrap();
        let both = Logins::load(&config).unwrap();
        assert_eq!(both.all().len(), 2);
        assert_eq!(
            both.get(rig, "AntMiner Stock").unwrap().password(),
            "changed"
        );
        logins.forget(rig).unwrap();
        assert_eq!(Logins::load(&config).unwrap().all().len(), 1);
    }

    // #### PR #42
    // What: a file listing a public address loads without it, and anything
    // but a plain file is refused.
    #[test]
    fn public_addresses_are_skipped_and_odd_files_refused() {
        let (dir, config) = store();
        std::fs::write(
            logins_path(&config),
            br#"{"version":1,"devices":[
                {"ip":"8.8.8.8","firmware":"AntMiner Stock","username":"root","password":"a"},
                {"ip":"100.101.102.103","firmware":"Braiins","username":"root","password":"b"}]}"#,
        )
        .unwrap();
        let logins = Logins::load(&config).unwrap();
        let all = logins.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "100.101.102.103".parse::<IpAddr>().unwrap());
        let folder = dir.0.join("folder.json");
        std::fs::create_dir(folder.with_extension("sv2-logins.json")).unwrap();
        assert!(Logins::load(&folder)
            .unwrap_err()
            .contains("not a plain file"));
        #[cfg(unix)]
        {
            let linked = dir.0.join("linked.json");
            std::os::unix::fs::symlink(logins_path(&config), logins_path(&linked)).unwrap();
            assert!(Logins::load(&linked)
                .unwrap_err()
                .contains("not a plain file"));
        }
    }
}
