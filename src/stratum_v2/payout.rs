//! #### PR #38
//! Bind BCH donation amounts to exact coinbase outputs, without changing the
//! GBT subsidy/fee budget. The setting and recipient are immutable per job.

use crate::{
    config::{self, MiningNetwork},
    donation::bch::BchPayout,
    tx::cashaddr_to_p2pkh_locking,
};

pub fn scripts(network: MiningNetwork, miner: &str) -> Result<[Vec<u8>; 2], String> {
    let miner = config::validate_payout_address(network, miner)?;
    let donation = config::reprefix_p2pkh_payout(config::DONATION_ADDRESS, network)?;
    Ok([
        cashaddr_to_p2pkh_locking(&miner)?,
        cashaddr_to_p2pkh_locking(&donation)?,
    ])
}

pub fn outputs(value: u64, scripts: &[Vec<u8>; 2], policy: BchPayout) -> Vec<(u64, Vec<u8>)> {
    if scripts[0] == scripts[1] {
        return vec![(value, scripts[0].clone())];
    }
    if value == 0 {
        return vec![(0, scripts[usize::from(policy.donation_work)].clone())];
    }
    policy
        .amounts(value)
        .into_iter()
        .zip(scripts)
        .filter(|(amount, _)| *amount != 0)
        .map(|(amount, script)| (amount, script.clone()))
        .collect()
}
