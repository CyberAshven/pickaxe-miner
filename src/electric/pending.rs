//! Keep a completed proof and its exact transaction across crashes and lost replies.
use super::{Baton, Coin, Deployment, Pot, Win};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Prepared {
    pub coin: Coin,
    pub locktime: u32,
    pub relay_fee: u64,
    pub raw: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub baton: Baton,
    pub win: Win,
    pub prepared: Option<Prepared>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    binding: String,
    pending: Option<Pending>,
}

pub(super) struct Journal {
    path: PathBuf,
    record: Record,
}

impl Journal {
    pub fn open(
        path: &Path,
        deployment: &Deployment,
        pot: &Pot,
        pkhs: &[[u8; 20]; 3],
    ) -> Result<Self, String> {
        let binding = format!(
            "{}:{}",
            deployment.network.as_str(),
            hex::encode(crate::proof::hash256(&pot.redeem))
        );
        let record = match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Record {
                version: 1,
                binding: binding.clone(),
                pending: None,
            },
            Err(_) => return Err("cannot inspect ELECTRIC pending journal".into()),
            Ok(meta) => {
                if !meta.is_file() || meta.len() > 65_536 {
                    return Err("invalid ELECTRIC pending journal; preserved for recovery".into());
                }
                let record: Record = serde_json::from_slice(
                    &fs::read(path).map_err(|_| "cannot read ELECTRIC pending journal")?,
                )
                .map_err(|_| "corrupt ELECTRIC pending journal; preserved for recovery")?;
                if record.version != 1 || record.binding != binding {
                    return Err("ELECTRIC journal belongs to another pot or network".into());
                }
                if let Some(pending) = &record.pending {
                    if pending.win.pkh != pkhs[pending.win.recipient as usize]
                        || pending.baton.state.difficulty < deployment.min_difficulty
                        || pending.baton.state.difficulty > deployment.max_difficulty
                        || !super::meets(
                            &crate::proof::hash256(&super::preimage(
                                pending.baton.state.last_height,
                                &pending.win.pkh,
                                pending.win.nonce,
                            )),
                            pending.baton.state.difficulty,
                        )
                    {
                        return Err("invalid saved ELECTRIC proof; journal preserved".into());
                    }
                    if let Some(prepared) = &pending.prepared {
                        let built = super::build_claim(
                            deployment,
                            pot,
                            &pending.baton,
                            &prepared.coin,
                            &pending.win,
                            prepared.locktime,
                            prepared.relay_fee,
                        )?;
                        if hex::encode(built.raw) != prepared.raw {
                            return Err(
                                "saved ELECTRIC transaction changed; journal preserved".into()
                            );
                        }
                    }
                }
                record
            }
        };
        Ok(Self {
            path: path.into(),
            record,
        })
    }

    pub fn pending(&self) -> Option<Pending> {
        self.record.pending.clone()
    }

    pub fn save(&mut self, pending: Option<Pending>) -> Result<(), String> {
        let record = Record {
            version: 1,
            binding: self.record.binding.clone(),
            pending,
        };
        let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|_| "cannot create ELECTRIC journal directory")?;
        crate::config::write_private_atomic(&self.path, &bytes)?;
        fs::File::options()
            .read(true)
            .write(true)
            .open(&self.path)
            .and_then(|f| f.sync_all())
            .map_err(|_| "cannot sync ELECTRIC pending journal")?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| "cannot sync ELECTRIC journal directory")?;
        self.record = record;
        Ok(())
    }

    pub fn retire_conflict(&mut self) -> Result<(), String> {
        // Keep the exact claim for reorg/manual recovery before clearing active work.
        let bytes = serde_json::to_vec(&self.record).map_err(|e| e.to_string())?;
        let archive = self.path.with_extension(format!(
            "{}.superseded.json",
            hex::encode(crate::proof::hash256(&bytes))
        ));
        crate::config::write_private_atomic(&archive, &bytes)?;
        fs::File::options()
            .read(true)
            .write(true)
            .open(&archive)
            .and_then(|file| file.sync_all())
            .map_err(|_| "cannot sync superseded ELECTRIC claim")?;
        self.save(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::MiningNetwork, donation::Recipient};

    #[test]
    fn completed_proof_and_exact_transaction_survive_restart_and_invalid_files_are_preserved() {
        let dir =
            std::env::temp_dir().join(format!("electric-journal-{:016x}", rand::random::<u64>()));
        let path = dir.join("pending.json");
        let deployment = Deployment::select(
            MiningNetwork::Chipnet,
            Some(&"42".repeat(32)),
            Some(100),
            Some(1),
            Some(2),
        )
        .unwrap();
        let pkhs = [[0x11; 20], [0x22; 20], [0x33; 20]];
        let locks = pkhs.map(|pkh| {
            crate::tx::cashaddr_to_p2pkh_locking(&crate::tx::cashaddr_with_version(
                0,
                &pkh,
                MiningNetwork::Chipnet,
            ))
            .unwrap()
        });
        let pot = Pot::new(&deployment, &locks).unwrap();
        let nonce = super::super::search(123, &pkhs[0], 1, u64::MAX - 100, 100_000).unwrap();
        let mut pending = Pending {
            baton: Baton {
                txid: "24".repeat(32),
                vout: 0,
                value: 1000,
                token_amount: 10_000,
                state: super::super::State {
                    difficulty: 1,
                    last_height: 123,
                    mine_count: 0,
                },
            },
            win: Win {
                recipient: Recipient::Miner,
                pkh: pkhs[0],
                nonce,
            },
            prepared: None,
        };
        let mut journal = Journal::open(&path, &deployment, &pot, &pkhs).unwrap();
        journal.save(Some(pending.clone())).unwrap();
        drop(journal);
        let mut journal = Journal::open(&path, &deployment, &pot, &pkhs).unwrap();
        assert_eq!(journal.pending().unwrap().win, pending.win);
        let coin = Coin {
            txid: "55".repeat(32),
            vout: 1,
            value: 50_000,
        };
        let built = super::super::build_claim(
            &deployment,
            &pot,
            &pending.baton,
            &coin,
            &pending.win,
            124,
            1000,
        )
        .unwrap();
        let raw = hex::encode(built.raw);
        pending.prepared = Some(Prepared {
            coin,
            locktime: 124,
            relay_fee: 1000,
            raw: raw.clone(),
        });
        journal.save(Some(pending)).unwrap();
        drop(journal);
        let mut reopened = Journal::open(&path, &deployment, &pot, &pkhs).unwrap();
        assert_eq!(reopened.pending().unwrap().prepared.unwrap().raw, raw);
        let bytes = fs::read(&path).unwrap();
        let mut other = pkhs;
        other[0][0] ^= 1;
        assert!(Journal::open(&path, &deployment, &pot, &other).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        // A failed replacement must leave the durable claim untouched.
        let blocking_path = dir.join("blocked");
        fs::create_dir(&blocking_path).unwrap();
        reopened.path = blocking_path;
        assert!(reopened.save(None).is_err());
        assert!(reopened.pending().is_some());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let mut corrupt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        corrupt["pending"]["prepared"]["raw"] = "00".into();
        let bad = serde_json::to_vec(&corrupt).unwrap();
        fs::write(&path, &bad).unwrap();
        assert!(Journal::open(&path, &deployment, &pot, &pkhs).is_err());
        assert_eq!(fs::read(&path).unwrap(), bad);
        fs::remove_dir_all(dir).unwrap();
    }
}
