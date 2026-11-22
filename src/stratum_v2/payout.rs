//! #### PR #38
//! Bind BCH donation amounts to exact coinbase outputs, without changing the
//! GBT subsidy/fee budget. The setting and recipient are immutable per job.

use crate::{
    config::{self, MiningNetwork},
    donation::bch::{BchDonation, BchPayout, PoolFee},
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

// #### merge-mining v2: login options
// What: a public pool's username is `ADDRESS[.worker][,key=value]...`. The
// options come after the first comma: `donation=<percent>` names this
// miner's Pickaxe donation (0 to 100 in 0.5% steps), as does
// `pickaxe-donation=`, the key only Pickaxe pools read (Pickaxe's own SV1
// adapter names it, see `sv1::open`); `p2p=` is BCH P2Pool's share address
// and is ignored here, and other keys are ignored too. A malformed
// donation, or two of them, refuses the login.
// #### one donation, the higher rate: the named rate can raise the
// server's, never lower it (see `effective_donation`).
// Why: a miner may pay the donation on someone else's server even when its
// owner set 0; without the option the server's own setting applies.
// Look here if: a username with options is refused, or a donation option
// does not raise a miner's donation.
/// A public pool miner's login: the payout address and the donation it
/// names, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub payout: String,
    pub donation: Option<BchDonation>,
}

// #### one donation, the higher rate
// What: a Public Solo Endpoint's (or a pool's) miner pays the higher of the
// server's Pickaxe donation rate and the rate their login names, once: host
// 1.5% and a login's 3% is 3%, host 1.5% and a login's 0 is 1.5%, host 0
// and a login's 1.5% is 1.5%. Every per-miner use of the rate goes through
// here: the server's work rotation, the coinbase donation output and the
// token leaves' donation slot (through the jobs' `BchPayout::donation`), and
// the Job Declaration minimum (the allocated token's rate). What the
// miner's own Pickaxe visibly donates on the same connection is credited
// after this (`own_donation`), never what a login says.
// Why: the donation is owed by whoever runs Pickaxe; the server's owner
// sets the server's rate, and a miner may pay more but never less.
// Look here if: a login lowers a miner's donation, or two rates are added
// instead of the higher one taken.
/// The Pickaxe donation rate a server's miner pays: the higher of `server`,
/// the server's rate, and `login`, the rate their login names, if any.
pub fn effective_donation(server: BchDonation, login: Option<BchDonation>) -> BchDonation {
    login.map_or(server, |login| login.max(server))
}

/// #### one donation, the higher rate: whether a login's payout is the
/// Pickaxe donation's own address, as the SV1 adapter's donation channel
/// opens at a remote pool: that channel's work is the donation.
pub fn is_donation(network: MiningNetwork, payout: &str) -> bool {
    payout == crate::donation::bch::address(network)
}

/// The message a malformed `donation=` refuses a login with.
pub const DONATION_OPTION_ERROR: &str =
    "donation= takes a percentage from 0 to 100 in 0.5% steps, such as ,donation=0.5";

/// Reads a public pool username: the payout and its options.
pub fn identity_login(network: MiningNetwork, identity: &str) -> Result<Login, String> {
    let identity = identity.trim();
    let (user, options) = identity.split_once(',').unwrap_or((identity, ""));
    let mut donation = None;
    for option in options
        .split(',')
        .filter(|option| !option.trim().is_empty())
    {
        let (key, value) = option.split_once('=').unwrap_or((option, ""));
        if !matches!(key.trim(), "donation" | "pickaxe-donation") {
            continue;
        }
        let rate: BchDonation = value
            .trim()
            .parse()
            .map_err(|_| DONATION_OPTION_ERROR.to_owned())?;
        if donation.is_some() || u16::from(rate) % 50 != 0 {
            return Err(DONATION_OPTION_ERROR.into());
        }
        donation = Some(rate);
    }
    Ok(Login {
        payout: user_payout(network, user)?,
        donation,
    })
}

/// The payout a public pool's miner connects with: their address, with or
/// without its network prefix, optionally followed by `.worker` (and, ####
/// merge-mining v2, login options after a comma). The server reads the
/// whole login (`identity_login`); this is its address alone.
#[cfg(test)]
pub fn identity_payout(network: MiningNetwork, identity: &str) -> Result<String, String> {
    identity_login(network, identity).map(|login| login.payout)
}

fn user_payout(network: MiningNetwork, user: &str) -> Result<String, String> {
    let address = user.trim().split('.').next().unwrap_or_default();
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
        return vec![(0, beneficiary(scripts, policy).to_vec())];
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

/// #### PR #42
/// Who a job pays as a whole: the donation in its work jobs, a public
/// pool's operator in its fee-work jobs, the miner otherwise; the same
/// rule as `BchPayout::amounts`. A SAFA forwarder pays this script.
pub fn beneficiary(scripts: &[Vec<u8>], policy: BchPayout) -> &[u8] {
    if policy.donation_work {
        &scripts[1]
    } else if policy.fee_work && scripts.len() > 2 {
        &scripts[2]
    } else {
        &scripts[0]
    }
}

// #### merge-mining v2: what a job's token leaves bind
// What: a split token's leaf pays the miner with the donation's reward share
// (two thirds of the miner's donation setting) in the donation slot and, on
// a public pool, the operator's coinbase-mode fee share in the fee slot; a
// work job's leaf pays its recipient whole with no split (the donation in
// donation work, the operator in fee work). A token without split support
// pays the miner, or the donation or operator in their token work
// (`BchPayout::token_donation_work`, `BchPayout::token_fee_work`), with no
// split.
// Why: merge-mined tokens split as BCH does: the donation and the fee take
// their coinbase share from each win and their work share as work.
// Look here if: a token's leaf pays another recipient or split than the
// BCH coinbase of the same job would.
/// The payouts and split a job's token leaves bind, for the recipients
/// `scripts` (`scripts()`'s: the miner, the donation and a public pool's
/// operator) and the job's policy.
pub fn leaf_terms(
    scripts: &[Vec<u8>],
    policy: BchPayout,
) -> Result<super::merge::set::LeafTerms<'_>, String> {
    use super::merge::split::{Slot, Split};
    let operator = scripts.get(2);
    let (payout, split) = if policy.donation_work {
        (&scripts[1], Split::NONE)
    } else if let (true, Some(operator)) = (policy.fee_work, operator) {
        (operator, Split::NONE)
    } else {
        let donation = Slot::new(policy.donation.reward_bps(), &scripts[1]);
        let fee = match (policy.fee, operator) {
            (Some(fee), Some(operator)) => Slot::new(fee.reward_bps(), operator),
            _ => Ok(Slot::EMPTY),
        };
        let split = Split::new(
            donation.map_err(|error| error.to_string())?,
            fee.map_err(|error| error.to_string())?,
        );
        (&scripts[0], split)
    };
    let plain = if policy.donation_work || policy.token_donation_work == Some(true) {
        &scripts[1]
    } else {
        match operator {
            Some(operator) if policy.fee_work || policy.token_fee_work == Some(true) => operator,
            _ => &scripts[0],
        }
    };
    Ok(super::merge::set::LeafTerms {
        payout,
        split,
        plain_payout: plain,
    })
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

    // #### merge-mining v2: login options
    #[test]
    fn a_login_may_choose_its_donation() {
        let network = MiningNetwork::Chipnet;
        let payout = super::super::template_tests::payout();
        let bare = payout.split_once(':').unwrap().1.to_owned();
        let login = |identity: &str| identity_login(network, identity);
        let rate = |text: &str| Some(text.parse::<BchDonation>().unwrap());
        for (identity, donation) in [
            (payout.clone(), None),
            (format!("{payout},donation=0"), rate("0")),
            (format!("{bare}.rig1,donation=0.5"), rate("0.5")),
            (
                format!("{payout}.rig1,p2p=bchtest:qq,donation=2"),
                rate("2"),
            ),
            (format!("{bare},donation=100"), rate("100")),
            (format!("{bare}.rig1,p2p=x"), None),
            (format!("{bare},other=1,donation=1.5"), rate("1.5")),
            (format!("{bare}.rig1,pickaxe-donation=0"), rate("0")),
            (
                format!("{bare},p2p=bchtest:qq,pickaxe-donation=0.5"),
                rate("0.5"),
            ),
        ] {
            assert_eq!(
                login(&identity),
                Ok(Login {
                    payout: payout.clone(),
                    donation
                }),
                "{identity}"
            );
            assert_eq!(identity_payout(network, &identity), Ok(payout.clone()));
        }
        for bad in [
            ",donation=0.25",
            ",donation=101",
            ",donation=",
            ",donation=x",
            ",donation=-1",
            ",donation",
            ",donation=1,donation=1",
            ",donation=1,pickaxe-donation=0",
            ",pickaxe-donation=0.25",
        ] {
            assert_eq!(
                login(&format!("{bare}.rig1{bad}")),
                Err(DONATION_OPTION_ERROR.into()),
                "{bad}"
            );
        }
        assert!(login(",donation=1").is_err());
    }

    // #### one donation, the higher rate
    // What: the higher of the server's rate and the login's applies, once:
    // host 0 and miner 1.5% is 1.5%, host 1.5% and a typed 0 is 1.5%, host
    // 1.5% and miner 3% is 3% (not 4.5%); no option leaves the server's.
    // The donation's own address is a donation channel's payout, with or
    // without its prefix or a P2Pool share address.
    #[test]
    fn the_higher_of_the_two_donation_rates_applies_once() {
        let rate = |text: &str| text.parse::<BchDonation>().unwrap();
        for (server, login, effective) in [
            ("0", Some("1.5"), "1.5"),
            ("1.5", Some("0"), "1.5"),
            ("1.5", Some("3"), "3"),
            ("2", Some("2"), "2"),
            ("1.5", None, "1.5"),
            ("0", None, "0"),
            ("100", Some("0"), "100"),
        ] {
            assert_eq!(
                effective_donation(rate(server), login.map(rate)),
                rate(effective),
                "{server} {login:?}"
            );
        }
        let network = MiningNetwork::Chipnet;
        let donation = crate::donation::bch::address(network);
        let bare = donation.split_once(':').unwrap().1;
        for identity in [
            donation.to_owned(),
            bare.to_owned(),
            format!("{donation},p2p=bchtest:qq"),
        ] {
            let login = identity_login(network, &identity).unwrap();
            assert!(is_donation(network, &login.payout), "{identity}");
        }
        let payout = super::super::template_tests::payout();
        assert!(!is_donation(network, &payout));
        assert!(!is_donation(MiningNetwork::Mainnet, donation));
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

    // #### merge-mining v2: what a job's token leaves bind
    // What: a miner job's split leaf pays the miner with the donation's
    // reward share and the operator's coinbase share in its split; work jobs
    // pay their recipient whole with no split; a token without split
    // support pays the miner unless its token work says otherwise.
    #[test]
    fn leaf_terms_follow_the_bch_split() {
        use super::super::merge::split::{Slot, Split};
        use crate::donation::bch::{BchDonation, FeeMode};
        let network = MiningNetwork::Chipnet;
        let operator = crate::tx::cashaddr_with_version(1 << 3, &[0x33; 20], network);
        let scripts = scripts(
            network,
            &super::super::template_tests::payout(),
            Some(&operator),
        )
        .unwrap();
        let fee = |mode| {
            Some(PoolFee {
                rate: "2".parse().unwrap(),
                mode,
            })
        };
        let donation = |bps| Slot::new(bps, &scripts[1]).unwrap();
        let operator_slot = |bps| Slot::new(bps, &scripts[2]).unwrap();
        // The default donation, no fee: 1% to the donation.
        let terms = leaf_terms(&scripts, BchPayout::default()).unwrap();
        assert_eq!(terms.payout, &scripts[0][..]);
        assert_eq!(terms.split, Split::new(donation(100), Slot::EMPTY));
        assert_eq!(terms.plain_payout, &scripts[0][..]);
        // A 2% fee: all of it in coinbase mode, two thirds in both, none in
        // work mode; a miner at 0% donation has the fee slot alone.
        for (mode, bps) in [
            (FeeMode::Coinbase, 200),
            (FeeMode::Both, 133),
            (FeeMode::Work, 0),
        ] {
            let policy = BchPayout {
                donation: BchDonation::try_from(0).unwrap(),
                fee: fee(mode),
                ..BchPayout::default()
            };
            let terms = leaf_terms(&scripts, policy).unwrap();
            let expected = if bps == 0 {
                Split::NONE
            } else {
                Split::new(Slot::EMPTY, operator_slot(bps))
            };
            assert_eq!(terms.split, expected, "{mode:?}");
        }
        // Work jobs: their recipient whole, no split.
        let donation_job = BchPayout {
            donation_work: true,
            fee: fee(FeeMode::Work),
            ..BchPayout::default()
        };
        let terms = leaf_terms(&scripts, donation_job).unwrap();
        assert_eq!(
            (terms.payout, terms.split, terms.plain_payout),
            (&scripts[1][..], Split::NONE, &scripts[1][..])
        );
        let fee_job = BchPayout {
            fee_work: true,
            fee: fee(FeeMode::Work),
            ..BchPayout::default()
        };
        let terms = leaf_terms(&scripts, fee_job).unwrap();
        assert_eq!(
            (terms.payout, terms.split, terms.plain_payout),
            (&scripts[2][..], Split::NONE, &scripts[2][..])
        );
        // A token without split support in its token work.
        let token_donation = BchPayout {
            token_donation_work: Some(true),
            ..BchPayout::default()
        };
        assert_eq!(
            leaf_terms(&scripts, token_donation).unwrap().plain_payout,
            &scripts[1][..]
        );
        let token_fee = BchPayout {
            fee: fee(FeeMode::Coinbase),
            token_fee_work: Some(true),
            ..BchPayout::default()
        };
        let terms = leaf_terms(&scripts, token_fee).unwrap();
        assert_eq!(terms.plain_payout, &scripts[2][..]);
        assert_eq!(terms.payout, &scripts[0][..]);
        // Solo: no operator, never a fee.
        let solo = super::scripts(network, &super::super::template_tests::payout(), None).unwrap();
        let terms = leaf_terms(&solo, token_fee).unwrap();
        assert_eq!(terms.split.fee_slot(), &Slot::EMPTY);
        assert_eq!(terms.plain_payout, &solo[0][..]);
        // A zero-value coinbase pays the beneficiary, as before.
        for policy in [BchPayout::default(), donation_job, fee_job] {
            assert_eq!(
                outputs(0, &scripts, policy),
                vec![(0, beneficiary(&scripts, policy).to_vec())]
            );
        }
    }
}
