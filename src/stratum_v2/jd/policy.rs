//! #### PR #42
//! What a custom job's coinbase must pay at a Pickaxe public pool, and the
//! merge-mining shape it may take. A custom job cannot rotate work the way
//! the pool's own jobs do, so it pays the whole Pickaxe donation and the
//! whole pool fee in its coinbase (D19).

use super::token::PoolRates;
use crate::{
    config::{self, MiningNetwork},
    donation::bch::BchDonation,
    stratum_v2::{merge::commitment::AuxCommitment, payout::PublicPool},
    tx::{cashaddr_to_coinbase_locking, cashaddr_to_p2pkh_locking},
};
use std::collections::HashMap;

/// The first bytes of a merge-mining commitment's script: OP_RETURN, a
/// 42-byte push and the magic.
const COMMITMENT_HEAD: [u8; 6] = [0x6a, 0x2a, b'C', b'T', b'M', b'M'];

/// The payout rule for one miner's custom jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayoutRule {
    /// The miner's own script: the pool payout output.
    pub miner: Vec<u8>,
    /// The donation's script and its whole rate in hundredths of a percent.
    pub donation: Option<(Vec<u8>, u16)>,
    /// The pool operator's fee script and its whole rate, taken from what
    /// the donation leaves.
    pub fee: Option<(Vec<u8>, u16)>,
}

impl PayoutRule {
    /// The rule at a public pool with the donation setting `donation`, for
    /// the miner paid at `payout`.
    pub fn public_pool(
        network: MiningNetwork,
        payout: &str,
        pool: &PublicPool,
        donation: BchDonation,
    ) -> Result<Self, String> {
        Self::with_rates(
            network,
            payout,
            pool,
            u16::from(donation),
            pool.fee.map_or(0, |fee| u16::from(fee.rate)),
        )
    }

    /// The rule a token's rates fix.
    pub fn from_rates(
        network: MiningNetwork,
        payout: &str,
        pool: &PublicPool,
        rates: PoolRates,
    ) -> Result<Self, String> {
        Self::with_rates(network, payout, pool, rates.donation_bps, rates.fee_bps)
    }

    fn with_rates(
        network: MiningNetwork,
        payout: &str,
        pool: &PublicPool,
        donation_bps: u16,
        fee_bps: u16,
    ) -> Result<Self, String> {
        let miner = config::validate_coinbase_address(network, payout)?;
        let donation =
            config::validate_payout_address(network, crate::donation::bch::address(network))?;
        let operator = config::validate_coinbase_address(network, &pool.address)?;
        Ok(Self {
            miner: cashaddr_to_coinbase_locking(&miner)?,
            donation: (donation_bps > 0)
                .then(|| cashaddr_to_p2pkh_locking(&donation).map(|script| (script, donation_bps)))
                .transpose()?,
            fee: (fee_bps > 0)
                .then(|| cashaddr_to_coinbase_locking(&operator).map(|script| (script, fee_bps)))
                .transpose()?,
        })
    }

    /// The outputs an allocated token names, all worth 0 (the miner first,
    /// as the spec's pool payout output, then the fee, then the donation),
    /// and the rates with their output indexes.
    pub fn allocated_outputs(&self) -> (Vec<(u64, Vec<u8>)>, PoolRates) {
        let mut outputs = vec![(0, self.miner.clone())];
        let mut rates = PoolRates {
            donation_bps: 0,
            fee_bps: 0,
            donation_output: None,
            fee_output: None,
        };
        if let Some((script, bps)) = &self.fee {
            rates.fee_bps = *bps;
            rates.fee_output = Some(outputs.len() as u8);
            outputs.push((0, script.clone()));
        }
        if let Some((script, bps)) = &self.donation {
            rates.donation_bps = *bps;
            rates.donation_output = Some(outputs.len() as u8);
            outputs.push((0, script.clone()));
        }
        (outputs, rates)
    }

    /// The whole donation D and the whole fee F a coinbase worth `value`
    /// must pay: D from the value, F from what D leaves, both rounded down.
    pub fn minimums(&self, value: u64) -> (u64, u64) {
        let share = |amount: u64, bps: u16| (u128::from(amount) * u128::from(bps) / 10_000) as u64;
        let donation = self
            .donation
            .as_ref()
            .map_or(0, |(_, bps)| share(value, *bps));
        let fee = self
            .fee
            .as_ref()
            .map_or(0, |(_, bps)| share(value - donation, *bps));
        (donation, fee)
    }

    // #### PR #42: the Coinbase-only payout rule
    // What: a custom job's coinbase is worth V (the sum of its outputs). It
    // must pay the donation's script at least D = V × donation, the fee's
    // script at least F = (V − D) × fee, and the miner something when
    // anything is left; amounts are summed per script, so scripts that are
    // the same address need the sum. Other outputs (the miner's own money)
    // are allowed, and the merge-mining shape is checked as below.
    // Why: the pool never sees a custom job's transactions, so the coinbase
    // is all it can hold to its fee and the donation; a job cannot rotate
    // work, so both are paid whole there (D19).
    // Look here if: a client's custom jobs are refused with
    // invalid-coinbase-tx-outputs, or a block pays less than the pool's
    // fee or the donation.
    /// V when `outputs` meet the rule; otherwise what is missing, never
    /// naming an address.
    pub fn check(&self, outputs: &[(u64, Vec<u8>)]) -> Result<u64, String> {
        check_commitments(outputs)?;
        let mut paid: HashMap<&[u8], u64> = HashMap::new();
        let mut value = 0u64;
        for (amount, script) in outputs {
            value = value
                .checked_add(*amount)
                .filter(|value| *value <= super::codec::MAX_MONEY)
                .ok_or("the outputs exceed the money supply")?;
            *paid.entry(script.as_slice()).or_default() += amount;
        }
        let (donation, fee) = self.minimums(value);
        let mut owed: HashMap<&[u8], u64> = HashMap::new();
        if let Some((script, _)) = &self.donation {
            *owed.entry(script.as_slice()).or_default() += donation;
        }
        if let Some((script, _)) = &self.fee {
            *owed.entry(script.as_slice()).or_default() += fee;
        }
        if value - donation - fee > 0 {
            *owed.entry(self.miner.as_slice()).or_default() += 1;
        }
        for (script, owed) in owed {
            let got = paid.get(script).copied().unwrap_or(0);
            if got < owed {
                let whose = if Some(script) == self.donation.as_ref().map(|(s, _)| s.as_slice()) {
                    "the donation output"
                } else if Some(script) == self.fee.as_ref().map(|(s, _)| s.as_slice()) {
                    "the fee output"
                } else {
                    "the miner's output"
                };
                return Err(format!("{whose} needs at least {owed} satoshis"));
            }
        }
        Ok(value)
    }
    // #### end PR #42 ####
}

/// The merge-mining shape a coinbase may take: at most one commitment, as
/// output 0, worth 0 and exact; tickets anywhere; no token-prefixed output
/// (a coinbase cannot create CashTokens); fewer than 0xfd outputs with a
/// commitment.
pub fn check_commitments(outputs: &[(u64, Vec<u8>)]) -> Result<(), String> {
    let mut commitment = false;
    for (index, (value, script)) in outputs.iter().enumerate() {
        if script.first() == Some(&0xef) {
            return Err("a coinbase output cannot carry CashTokens".into());
        }
        if script.starts_with(&COMMITMENT_HEAD) {
            if index != 0 || *value != 0 || AuxCommitment::parse_script(script).is_none() {
                return Err("a merge-mining commitment must be output 0, exact and worth 0".into());
            }
            commitment = true;
        }
    }
    if commitment && outputs.len() >= 0xfd {
        return Err("a merge-mined coinbase needs fewer than 253 outputs".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::donation::bch::{FeeMode, PoolFee};

    fn address(seed: u8) -> String {
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[seed; 20], MiningNetwork::Chipnet).unwrap()
    }

    fn pool(fee: &str) -> PublicPool {
        PublicPool {
            fee: Some(PoolFee {
                rate: fee.parse().unwrap(),
                mode: FeeMode::Work,
            }),
            address: address(0x34),
        }
    }

    fn rule(donation: &str, fee: &str) -> PayoutRule {
        PayoutRule::public_pool(
            MiningNetwork::Chipnet,
            &address(0x12),
            &pool(fee),
            donation.parse().unwrap(),
        )
        .unwrap()
    }

    fn paying(rule: &PayoutRule, miner: u64, fee: u64, donation: u64) -> Vec<(u64, Vec<u8>)> {
        vec![
            (miner, rule.miner.clone()),
            (fee, rule.fee.as_ref().unwrap().0.clone()),
            (donation, rule.donation.as_ref().unwrap().0.clone()),
        ]
    }

    // #### PR #42
    // What: at 1.5% donation and a 1% fee, a 312,500,000-satoshi coinbase
    // owes the donation 4,687,500 and the fee 3,078,125 (from what the
    // donation leaves), in every fee mode; the exact amounts pass and one
    // satoshi less on either fails. The allocated outputs list the miner,
    // the fee and the donation, worth 0, with their indexes.
    // Look here if: PayoutRule's amounts or allocated_outputs change.
    #[test]
    fn custom_jobs_pay_the_whole_donation_and_fee() {
        let rule = rule("1.5", "1");
        assert_eq!(rule.minimums(312_500_000), (4_687_500, 3_078_125));
        let exact = paying(&rule, 304_734_375, 3_078_125, 4_687_500);
        assert_eq!(rule.check(&exact), Ok(312_500_000));
        assert!(rule
            .check(&paying(&rule, 304_734_376, 3_078_124, 4_687_500))
            .unwrap_err()
            .contains("the fee output needs at least"));
        assert!(rule
            .check(&paying(&rule, 304_734_376, 3_078_125, 4_687_499))
            .unwrap_err()
            .contains("the donation output needs at least"));
        let (outputs, rates) = rule.allocated_outputs();
        assert_eq!(outputs.len(), 3);
        assert!(outputs.iter().all(|(value, _)| *value == 0));
        assert_eq!(
            rates,
            PoolRates {
                donation_bps: 150,
                fee_bps: 100,
                donation_output: Some(2),
                fee_output: Some(1),
            }
        );
        let from_token =
            PayoutRule::from_rates(MiningNetwork::Chipnet, &address(0x12), &pool("1"), rates)
                .unwrap();
        assert_eq!(from_token, rule);
        let short = rule
            .check(&paying(&rule, 304_734_376, 3_078_124, 4_687_500))
            .unwrap_err();
        assert!(
            !short.contains("bchtest") && !short.contains(':'),
            "{short}"
        );
    }

    // #### PR #42
    // What: amounts are summed per script (a fee paid to the miner's own
    // address needs the sum), other outputs are allowed, and at a 100%
    // donation nothing is owed to the miner.
    // Look here if: PayoutRule::check changes.
    #[test]
    fn the_rule_sums_per_script_and_waives_the_miner_at_100_percent() {
        let mut own_pool = pool("1");
        own_pool.address = address(0x12);
        let rule = PayoutRule::public_pool(
            MiningNetwork::Chipnet,
            &address(0x12),
            &own_pool,
            "1.5".parse().unwrap(),
        )
        .unwrap();
        let donation = rule.donation.clone().unwrap().0;
        let split = vec![
            (304_734_375, rule.miner.clone()),
            (3_078_125, rule.miner.clone()),
            (4_687_500, donation.clone()),
        ];
        assert_eq!(rule.check(&split), Ok(312_500_000));
        let all_to_fee = vec![
            (307_812_500, rule.miner.clone()),
            (4_687_500, donation.clone()),
        ];
        assert!(rule.check(&all_to_fee).is_ok());
        // Another output is the miner's own business; the owed amounts follow
        // the whole value.
        let extra = vec![
            (304_733_375, rule.miner.clone()),
            (3_078_125, rule.miner.clone()),
            (1_000, vec![0x51]),
            (4_687_500, donation.clone()),
        ];
        assert_eq!(rule.check(&extra), Ok(312_500_000));
        let all = rule_with_donation("100");
        let donation = all.donation.clone().unwrap().0;
        assert_eq!(all.minimums(1_000), (1_000, 0));
        assert_eq!(all.check(&[(1_000, donation)]), Ok(1_000));
    }

    fn rule_with_donation(donation: &str) -> PayoutRule {
        PayoutRule::public_pool(
            MiningNetwork::Chipnet,
            &address(0x12),
            &pool("1"),
            donation.parse().unwrap(),
        )
        .unwrap()
    }

    // #### PR #42
    // What: a commitment passes only as output 0, worth 0 and exact; at
    // output 1, valued, malformed or twice it is refused; 253 outputs with
    // a commitment are refused; a token-prefixed output is refused.
    // Look here if: check_commitments changes.
    #[test]
    fn the_commitment_must_be_output_zero_exact_and_zero_valued_and_tokens_are_refused() {
        let script = AuxCommitment {
            root: [7; 32],
            height: 2,
            nonce: 9,
        }
        .script()
        .to_vec();
        let pay = (100, vec![0x51]);
        assert!(check_commitments(&[(0, script.clone()), pay.clone()]).is_ok());
        assert!(check_commitments(&[pay.clone(), (0, script.clone())]).is_err());
        assert!(check_commitments(&[(1, script.clone()), pay.clone()]).is_err());
        let mut short = script.clone();
        short.pop();
        assert!(check_commitments(&[(0, short), pay.clone()]).is_err());
        assert!(check_commitments(&[(0, script.clone()), (0, script.clone())]).is_err());
        let mut many = vec![(0, script)];
        many.extend(std::iter::repeat_n(pay.clone(), 252));
        assert!(check_commitments(&many).is_err());
        many.pop();
        assert!(check_commitments(&many).is_ok());
        assert!(check_commitments(&[(100, vec![0xef, 0x01])]).is_err());
    }
}
