//! #### PR #38
//! Bind BCH donation amounts to exact coinbase outputs, without changing the
//! GBT subsidy/fee budget. The setting and recipient are immutable per job.

use crate::{
    config::{self, MiningNetwork},
    donation::bch::{BchPayout, PoolFee},
    tx::{cashaddr_to_coinbase_locking, cashaddr_to_p2pkh_locking},
};

/// #### PR #40
/// A public pool: each miner's blocks pay the address they connect with, and
/// the operator's fee comes off what the Pickaxe donation leaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicPool {
    /// `None` charges no fee.
    pub fee: Option<PoolFee>,
    /// Where the operator's fee goes.
    pub address: String,
}

/// The payout a public pool's miner connects with: their address, with or
/// without its network prefix, optionally followed by `.worker`.
pub fn identity_payout(network: MiningNetwork, identity: &str) -> Result<String, String> {
    let address = identity.trim().split('.').next().unwrap_or_default();
    let address = if address.contains(':') {
        address.to_owned()
    } else {
        let prefix = match network {
            MiningNetwork::Mainnet => "bitcoincash",
            MiningNetwork::Chipnet => "bchtest",
        };
        format!("{prefix}:{address}")
    };
    config::validate_coinbase_address(network, &address)
        .map_err(|_| "the username is not a payout address on this network".into())
}

/// The coinbase's recipients: the miner, the donation and, in a public pool,
/// its operator (#### PR #40).
pub fn scripts(
    network: MiningNetwork,
    miner: &str,
    operator: Option<&str>,
) -> Result<Vec<Vec<u8>>, String> {
    let miner = config::validate_coinbase_address(network, miner)?;
    let donation =
        config::validate_payout_address(network, crate::donation::bch::address(network))?;
    let mut scripts = vec![
        cashaddr_to_coinbase_locking(&miner)?,
        cashaddr_to_p2pkh_locking(&donation)?,
    ];
    if let Some(operator) = operator {
        let operator = config::validate_coinbase_address(network, operator)?;
        scripts.push(cashaddr_to_coinbase_locking(&operator)?);
    }
    Ok(scripts)
}

/// The coinbase outputs for `value`: each recipient's amount, in recipient
/// order, with recipients that share a script joined and empty ones left out.
pub fn outputs(value: u64, scripts: &[Vec<u8>], policy: BchPayout) -> Vec<(u64, Vec<u8>)> {
    let amounts = policy.amounts(value);
    if value == 0 {
        let index = if policy.donation_work {
            1
        } else if policy.fee_work && scripts.len() > 2 {
            2
        } else {
            0
        };
        return vec![(0, scripts[index].clone())];
    }
    let mut outputs: Vec<(u64, Vec<u8>)> = Vec::new();
    for (amount, script) in amounts.into_iter().zip(scripts) {
        if amount == 0 {
            continue;
        }
        match outputs.iter_mut().find(|(_, existing)| existing == script) {
            Some((total, _)) => *total += amount,
            None => outputs.push((amount, script.clone())),
        }
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #40
    #[test]
    fn a_public_pool_username_is_its_payout_address() {
        let network = MiningNetwork::Chipnet;
        let payout = super::super::template_tests::payout();
        let bare = payout.split_once(':').unwrap().1.to_owned();
        assert_eq!(identity_payout(network, &payout).unwrap(), payout);
        assert_eq!(identity_payout(network, &bare).unwrap(), payout);
        assert_eq!(
            identity_payout(network, &format!("{bare}.rig-7")).unwrap(),
            payout
        );
        assert_eq!(
            identity_payout(network, &format!(" {payout}.rig-7 ")).unwrap(),
            payout
        );
        for bad in ["", "worker", "sv1-device", "qqqq.rig"] {
            assert!(identity_payout(network, bad).is_err(), "{bad}");
        }
        assert!(identity_payout(MiningNetwork::Mainnet, &payout).is_err());
    }

    // #### PR #40
    #[test]
    fn a_coinbase_pays_p2pkh_and_p2sh_including_multisig_and_p2sh32() {
        let network = MiningNetwork::Chipnet;
        let p2sh = crate::tx::cashaddr_with_version(1 << 3, &[0x33; 20], network);
        let p2sh32 = crate::tx::cashaddr_with_version((1 << 3) | 3, &[0x44; 32], network);
        assert!(p2sh.starts_with("bchtest:p"));
        let mut expected = vec![0xa9, 0x14];
        expected.extend([0x33; 20]);
        expected.push(0x87);
        assert_eq!(cashaddr_to_coinbase_locking(&p2sh).unwrap(), expected);
        let mut expected = vec![0xaa, 0x20];
        expected.extend([0x44; 32]);
        expected.push(0x87);
        assert_eq!(cashaddr_to_coinbase_locking(&p2sh32).unwrap(), expected);
        // An operator's multisig fee address and a P2SH miner both work.
        let scripts = scripts(network, &p2sh32, Some(&p2sh)).unwrap();
        assert_eq!(scripts[0][0], 0xaa);
        assert_eq!(scripts[2][0], 0xa9);
        assert_eq!(
            identity_payout(network, &format!("{p2sh}.rig")).unwrap(),
            p2sh
        );
        // PHOTON payouts stay P2PKH, and other networks stay refused.
        assert!(config::validate_payout_address(network, &p2sh).is_err());
        assert!(config::validate_coinbase_address(MiningNetwork::Mainnet, &p2sh).is_err());
        assert!(cashaddr_to_coinbase_locking("bchtest:qqqq").is_err());
    }
}
