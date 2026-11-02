//! #### PR #51: persist a pot-only recovery key before showing a funding
//! address; never replace an existing key, including a damaged one.

use secp256k1::{PublicKey, SecretKey};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub(super) struct RecoveryKey(SecretKey);

impl RecoveryKey {
    pub(super) fn load(path: &Path, create: bool) -> Result<Self, String> {
        if create {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent).map_err(|_| "cannot create pot key directory")?;
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(mut file) => {
                    // Restrict the empty file before writing any secret bytes.
                    crate::config::restrict_private_config(path)?;
                    let key = Self(crate::crypto::random_secret_key());
                    file.write_all(&key.0.to_secret_bytes())
                        .and_then(|()| file.sync_all())
                        .map_err(|_| "cannot save pot recovery key; no funding address created")?;
                    #[cfg(unix)]
                    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        fs::File::open(parent)
                            .and_then(|dir| dir.sync_all())
                            .map_err(|_| {
                                "cannot sync recovery key directory; no funding address created"
                            })?;
                    }
                    return Ok(key);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("cannot create pot recovery key".into()),
            }
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            "pot recovery key unavailable; restore your backup, or run `electric pot` for a NEW pot"
        })?;
        if !metadata.is_file() || metadata.len() != 32 {
            return Err(
                "invalid pot recovery key file; restore its backup, never replace it".into(),
            );
        }
        crate::config::restrict_private_config(path)?;
        let mut bytes = [0u8; 32];
        fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|_| "cannot read pot recovery key")?;
        SecretKey::from_secret_bytes(bytes)
            .map(Self)
            .map_err(|_| "invalid pot recovery key; restore its backup".into())
    }

    pub(super) fn public(&self) -> PublicKey {
        PublicKey::from_secret_key(&self.0)
    }

    pub(super) fn secret(&self) -> &SecretKey {
        &self.0
    }
}

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        self.0.non_secure_erase();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_persist_and_invalid_or_missing_files_are_never_replaced() {
        let dir =
            std::env::temp_dir().join(format!("pickaxe-pot-key-{:016x}", rand::random::<u64>()));
        let path = dir.join("recovery.pot-key");
        assert!(RecoveryKey::load(&path, false).is_err());
        assert!(!path.exists());
        let first = RecoveryKey::load(&path, true).unwrap().public();
        assert_eq!(fs::metadata(&path).unwrap().len(), 32);
        assert_eq!(first, RecoveryKey::load(&path, true).unwrap().public());
        assert_eq!(first, RecoveryKey::load(&path, false).unwrap().public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let link = dir.join("link.pot-key");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(RecoveryKey::load(&link, true).is_err());
        }
        for bytes in [&[0u8; 32][..], &[1, 2, 3][..]] {
            fs::write(&path, bytes).unwrap();
            assert!(RecoveryKey::load(&path, true).is_err());
            assert!(RecoveryKey::load(&path, false).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
