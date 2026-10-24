//! #### PR #42
//! The tokens Pickaxe can merge-mine, compiled in per network like PHOTON's
//! deployments. Both networks' lists are empty: no merge-minable token
//! exists yet. The Chipnet test token is for tests and the hidden Chipnet
//! test flag only, and is never in a list.

use super::{
    super::template::double_sha256,
    safa::{SafaParams, VersionRule, CANONICAL},
    tree::{MAX_HEIGHT, MAX_LEAVES},
    Hash,
};
use crate::{
    config::MiningNetwork,
    donation::{TokenDonation, NEW_TOKEN_DONATION},
};
use sha2::{Digest, Sha256};

/// At most this many tokens in one token set (+53 coinbase bytes for the
/// commitment, whatever the count).
pub const MAX_TOKENS: usize = MAX_LEAVES;

/// A token covenant's deployment, checked as `PhotonDeployment::verify`
/// checks PHOTON's: the redeem script, its P2SH32 lock and the lock's
/// Fulcrum script hash must agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CovenantDeployment {
    pub redeem_script_hex: &'static str,
    pub covenant_lock_hex: &'static str,
    pub script_hash_hex: &'static str,
}

impl CovenantDeployment {
    pub fn verify(&self) -> Result<(), String> {
        let redeem = hex::decode(self.redeem_script_hex.trim())
            .map_err(|_| "invalid token covenant redeem script")?;
        let lock =
            hex::decode(self.covenant_lock_hex).map_err(|_| "invalid token covenant lock")?;
        if lock.len() != 35 || lock[0] != 0xaa || lock[1] != 0x20 || lock[34] != 0x87 {
            return Err("token covenant lock is not P2SH32".into());
        }
        if lock[2..34] != double_sha256(&redeem) {
            return Err("token covenant redeem script does not match its lock".into());
        }
        let mut script_hash: Hash = Sha256::digest(&lock).into();
        script_hash.reverse();
        if hex::encode(script_hash) != self.script_hash_hex {
            return Err("token covenant Fulcrum script hash does not match its lock".into());
        }
        Ok(())
    }
}

/// #### merge-mining v2: a merge-mined token's registry row. A token wins
/// on any share whose hash meets its own target; v2 has no block-required
/// mode (version 03 is reserved for one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeToken {
    pub name: &'static str,
    /// One code path: rows differ per network, never the logic.
    pub network: MiningNetwork,
    /// The token category, in internal byte order.
    pub category: Hash,
    /// The covenant, once one is deployed.
    pub deployment: Option<CovenantDeployment>,
    /// The standard verifier body the covenant uses, by SHA-256.
    pub verifier_sha256: Option<Hash>,
    /// The covenant's largest aux tree height (at most 16).
    pub max_aux_height: u8,
    /// Which token stays when the tree cannot hold them all: higher stays.
    pub priority: u8,
    /// #### merge-mining v2: whether the covenant verifies the per-win split
    /// (`split::Split`). Without it the leaf's ext is 0 and the donation
    /// (and a fee) are taken as work instead.
    pub split: bool,
}

impl MergeToken {
    /// Refuses a row that could not be mined or claimed as written.
    pub fn verify(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.chars().any(char::is_control) {
            return Err("a merge-mined token needs a printable name".into());
        }
        if self.category == [0; 32] {
            return Err(format!("{} has no token category", self.name));
        }
        if self.max_aux_height > MAX_HEIGHT {
            return Err(format!("{} allows a taller aux tree than 16", self.name));
        }
        if let Some(deployment) = self.deployment {
            deployment
                .verify()
                .map_err(|error| format!("{}: {error}", self.name))?;
        }
        Ok(())
    }
}

/// The tokens registered on `network`: none yet, on either network.
pub fn tokens(network: MiningNetwork) -> &'static [MergeToken] {
    match network {
        MiningNetwork::Mainnet => &[],
        MiningNetwork::Chipnet => &[],
    }
}

/// SHA-256 of "pickaxe merge-mining test token v1" (the category is the
/// token's identity, not the format's version).
const TEST_CATEGORY: Hash = [
    0xfe, 0xab, 0x4b, 0xcd, 0x32, 0x4f, 0x72, 0x20, 0x33, 0xf5, 0xa1, 0xd6, 0x74, 0x32, 0xf4, 0x5c,
    0xae, 0xba, 0x0e, 0x50, 0x5b, 0x5c, 0xca, 0xaf, 0x66, 0x55, 0x55, 0x76, 0x94, 0x25, 0xad, 0x93,
];

/// A Chipnet-only token with no covenant: it exercises the whole path
/// (commitment, shares, proofs) without any real token. Its lock and state
/// hashes are synthetic (see `hub::TokenHub::test_token`).
pub const TEST_TOKEN: MergeToken = MergeToken {
    name: "Pickaxe test token",
    network: MiningNetwork::Chipnet,
    category: TEST_CATEGORY,
    deployment: None,
    verifier_sha256: None,
    max_aux_height: MAX_HEIGHT,
    priority: 0,
    split: true,
};

/// #### PR #42: a token mined by its own 80-byte header instead of BCH
/// (ASIC-exclusive), such as SAFA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderToken {
    pub name: &'static str,
    /// One code path: rows differ per network, never the logic.
    pub network: MiningNetwork,
    /// The token category, in internal byte order.
    pub category: Hash,
    /// The token's layout, adjustment and rules, as data.
    pub params: SafaParams,
    /// The covenant, once one is deployed.
    pub deployment: Option<CovenantDeployment>,
    /// The least share of mining work its donation takes.
    pub donation_minimum: TokenDonation,
}

/// The header-shaped tokens registered on `network`: none yet, on either
/// network.
pub fn header_tokens(network: MiningNetwork) -> &'static [HeaderToken] {
    match network {
        MiningNetwork::Mainnet => &[],
        MiningNetwork::Chipnet => &[],
    }
}

/// SHA-256 of "pickaxe asic test token v1".
const HEADER_TEST_CATEGORY: Hash = [
    0x2b, 0x7f, 0xec, 0x11, 0x5c, 0x25, 0xb0, 0x0f, 0xea, 0xa7, 0x69, 0x7c, 0xa3, 0xbf, 0xa4, 0x6f,
    0x87, 0xda, 0x6c, 0x49, 0xa4, 0x6c, 0xf2, 0xf3, 0xd7, 0xa7, 0xea, 0xd6, 0x0c, 0xbc, 0xec, 0x34,
];

/// A Chipnet-only header token with SAFA's canonical layout, BIP320
/// version rolling and no covenant: its simulated thread exercises jobs,
/// devices, wins and their checks without any real token.
pub const HEADER_TEST_TOKEN: HeaderToken = HeaderToken {
    name: "Pickaxe ASIC test token",
    network: MiningNetwork::Chipnet,
    category: HEADER_TEST_CATEGORY,
    params: SafaParams {
        version: VersionRule::Bip320,
        ..CANONICAL
    },
    deployment: None,
    donation_minimum: NEW_TOKEN_DONATION,
};

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
    // What: no header token is registered on either network; the ASIC test
    // token is Chipnet-only, SAFA's canonical layout with BIP320 rolling,
    // a 1.5% donation minimum and no covenant.
    // Look here if: the header token registry changes.
    #[test]
    fn header_registries_are_empty_and_the_asic_test_token_is_chipnet_only() {
        assert!(header_tokens(MiningNetwork::Mainnet).is_empty());
        assert!(header_tokens(MiningNetwork::Chipnet).is_empty());
        assert_eq!(HEADER_TEST_TOKEN.network, MiningNetwork::Chipnet);
        assert_eq!(
            HEADER_TEST_TOKEN.category,
            super::super::sha256(b"pickaxe asic test token v1")
        );
        assert_eq!(HEADER_TEST_TOKEN.params.version, VersionRule::Bip320);
        assert_eq!(HEADER_TEST_TOKEN.params.daa, CANONICAL.daa);
        assert_eq!(HEADER_TEST_TOKEN.donation_minimum, NEW_TOKEN_DONATION);
        assert!(HEADER_TEST_TOKEN.deployment.is_none());
    }

    // #### merge-mining v2
    #[test]
    fn registries_are_empty_and_the_test_token_is_chipnet_only() {
        assert!(tokens(MiningNetwork::Mainnet).is_empty());
        assert!(tokens(MiningNetwork::Chipnet).is_empty());
        assert_eq!(TEST_TOKEN.network, MiningNetwork::Chipnet);
        assert_eq!(
            TEST_TOKEN.category,
            super::super::sha256(b"pickaxe merge-mining test token v1")
        );
        TEST_TOKEN.verify().unwrap();
        // #### merge-mining v2: the test token exercises the split.
        const { assert!(TEST_TOKEN.split) };
        // Rows that could not be mined or claimed as written are refused.
        let refused: [fn(&mut MergeToken); 3] = [
            |t| t.name = " ",
            |t| t.category = [0; 32],
            |t| t.max_aux_height = MAX_HEIGHT + 1,
        ];
        for (index, change) in refused.into_iter().enumerate() {
            let mut token = TEST_TOKEN;
            change(&mut token);
            assert!(token.verify().is_err(), "row {index}");
        }
    }

    // #### PR #42
    #[test]
    fn a_deployment_must_match_its_lock_and_script_hash() {
        use crate::protocol::{
            CHIPNET_COVENANT_LOCKING_BYTECODE_HEX, CHIPNET_EXPECTED_SCRIPT_HASH_HEX,
            CHIPNET_REDEEM_SCRIPT_HEX,
        };
        let deployment = CovenantDeployment {
            redeem_script_hex: CHIPNET_REDEEM_SCRIPT_HEX,
            covenant_lock_hex: CHIPNET_COVENANT_LOCKING_BYTECODE_HEX,
            script_hash_hex: CHIPNET_EXPECTED_SCRIPT_HASH_HEX,
        };
        deployment.verify().unwrap();
        let mut token = TEST_TOKEN;
        token.deployment = Some(deployment);
        token.verify().unwrap();
        let mut wrong = deployment;
        wrong.script_hash_hex = CHIPNET_REDEEM_SCRIPT_HEX;
        assert!(wrong.verify().is_err());
        let mut wrong = deployment;
        wrong.redeem_script_hex = "51";
        assert!(wrong.verify().is_err());
        token.deployment = Some(wrong);
        assert!(token.verify().is_err());
    }
}
