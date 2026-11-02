//! #### PR #65: a dedicated receiving wallet for ELECTRIC rewards. This
//! owner-only key is separate from the fee-pot and GPU search keys.
use super::recovery::RecoveryKey;
use crate::config::MiningNetwork;
use std::path::{Path, PathBuf};

pub(super) struct Wallet {
    pub address: String,
    pub key_path: PathBuf,
}

pub(super) fn create(network: MiningNetwork, config_path: &Path) -> Result<Wallet, String> {
    let key_path =
        config_path.with_file_name(format!("electric-{}-payout.wallet-key", network.as_str()));
    let key = RecoveryKey::load(&key_path, true)?;
    use ripemd::Ripemd160;
    use sha2::{Digest, Sha256};
    let pkh = Ripemd160::digest(Sha256::digest(key.public().serialize()));
    let address = crate::tx::cashaddr_with_version(0, &pkh, network);
    Ok(Wallet { address, key_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wallet_persists_is_network_bound_and_does_not_import_or_replace_keys() {
        let dir =
            std::env::temp_dir().join(format!("pickaxe-payout-{:016x}", rand::random::<u64>()));
        let config = dir.join("config.json");
        let wallet = create(MiningNetwork::Mainnet, &config).unwrap();
        assert_eq!(
            wallet.address,
            create(MiningNetwork::Mainnet, &config).unwrap().address
        );
        assert!(
            crate::config::validate_payout_address(MiningNetwork::Mainnet, &wallet.address).is_ok()
        );
        assert!(
            crate::config::validate_payout_address(MiningNetwork::Chipnet, &wallet.address)
                .is_err()
        );
        let other = create(MiningNetwork::Chipnet, &config).unwrap();
        assert_ne!(wallet.key_path, other.key_path);
        assert_ne!(wallet.address, other.address);
        let key = RecoveryKey::load(&wallet.key_path, false).unwrap();
        // Independently prove that this private key controls the receiving
        // script, rather than merely naming a pot's refund destination.
        let locking = crate::tx::cashaddr_to_p2pkh_locking(&wallet.address).unwrap();
        use sha2::{Digest, Sha256};
        let public_hash = ripemd::Ripemd160::digest(Sha256::digest(key.public().serialize()));
        assert_eq!(&locking[3..23], public_hash.as_slice());
        std::fs::write(&wallet.key_path, [0u8; 32]).unwrap();
        assert!(create(MiningNetwork::Mainnet, &config).is_err());
        assert_eq!(std::fs::read(&wallet.key_path).unwrap(), [0u8; 32]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
