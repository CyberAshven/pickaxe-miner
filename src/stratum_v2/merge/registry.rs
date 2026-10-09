//! #### PR #42
//! The tokens Pickaxe can merge-mine, compiled in per network like PHOTON's
//! deployments. Both networks' lists are empty: no merge-minable token
//! exists yet. The Chipnet test token is for tests and the hidden Chipnet
//! test flag only, and is never in a list.

use super::{super::template::double_sha256, leaf::Mode, tree::MAX_HEIGHT, Hash};
use crate::config::MiningNetwork;
use sha2::{Digest, Sha256};

/// At most this many tokens in one token set (+53 coinbase bytes for the
/// commitment, whatever the count).
pub const MAX_TOKENS: usize = 16;
/// At most this many Case B tickets in one coinbase (+46 bytes each).
pub const MAX_TICKETS: usize = 16;
/// A ticket output: value (8), script length (1), script (37).
pub const TICKET_LEN: usize = 46;
const TICKET_SCRIPT_LEN: usize = 37;

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

/// A Case B token's coinbase ticket: a keyless P2S output
/// `OP_0 OP_UTXOTOKENCATEGORY <category ‖ capability> OP_EQUAL`, spendable
/// only beside the token's baton at input 0, once the block is mature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketPolicy {
    /// The baton's NFT capability byte: 1 (mutable) or 2 (minting).
    pub capability: u8,
    /// Reserved: v1 tickets carry 0 satoshis (see [`MergeToken::verify`]).
    pub value_sats: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeToken {
    pub name: &'static str,
    /// One code path: rows differ per network, never the logic.
    pub network: MiningNetwork,
    /// The token category, in internal byte order.
    pub category: Hash,
    /// Case A, Case B or both.
    pub modes: &'static [Mode],
    /// The covenant, once one is deployed.
    pub deployment: Option<CovenantDeployment>,
    /// The standard verifier body the covenant uses, by SHA-256.
    pub verifier_sha256: Option<Hash>,
    /// The covenant's largest aux tree height (at most 16).
    pub max_aux_height: u8,
    /// Case B: the lowest BCH height whose blocks may claim.
    pub start_height: u32,
    /// Case B: the coinbase ticket.
    pub ticket: Option<TicketPolicy>,
    /// Which token stays when slots collide: higher stays.
    pub priority: u8,
}

impl MergeToken {
    pub fn supports(&self, mode: Mode) -> bool {
        self.modes.contains(&mode)
    }

    /// Refuses a row that could not be mined or claimed as written.
    pub fn verify(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.chars().any(char::is_control) {
            return Err("a merge-mined token needs a printable name".into());
        }
        if self.category == [0; 32] {
            return Err(format!("{} has no token category", self.name));
        }
        if self.modes.is_empty()
            || self
                .modes
                .iter()
                .enumerate()
                .any(|(index, mode)| self.modes[..index].contains(mode))
        {
            return Err(format!("{} must list each mode once", self.name));
        }
        if self.max_aux_height > MAX_HEIGHT {
            return Err(format!("{} allows a taller aux tree than 16", self.name));
        }
        match (self.supports(Mode::BlockRequired), self.ticket) {
            (true, None) => return Err(format!("{} needs a ticket for Case B", self.name)),
            (false, Some(_)) => {
                return Err(format!("{} has a ticket but no Case B", self.name));
            }
            (_, Some(ticket)) if !matches!(ticket.capability, 1 | 2) => {
                return Err(format!(
                    "{}'s ticket must name a mutable or minting baton",
                    self.name
                ));
            }
            // A zero-value ticket leaves BCH payouts and the block journal's
            // coinbase total unchanged; other values are not supported yet.
            (_, Some(ticket)) if ticket.value_sats != 0 => {
                return Err(format!("{}'s ticket must carry 0 satoshis", self.name));
            }
            _ => {}
        }
        if let Some(deployment) = self.deployment {
            deployment
                .verify()
                .map_err(|error| format!("{}: {error}", self.name))?;
        }
        Ok(())
    }

    /// The 46-byte ticket output of a Case B token.
    pub fn ticket_output(&self) -> Option<[u8; TICKET_LEN]> {
        let ticket = self.ticket?;
        let mut output = [0; TICKET_LEN];
        output[..8].copy_from_slice(&ticket.value_sats.to_le_bytes());
        output[8] = TICKET_SCRIPT_LEN as u8;
        output[9] = 0x00; // OP_0
        output[10] = 0xce; // OP_UTXOTOKENCATEGORY
        output[11] = 0x21; // push 33
        output[12..44].copy_from_slice(&self.category);
        output[44] = ticket.capability;
        output[45] = 0x87; // OP_EQUAL
        Some(output)
    }
}

/// Whether `script` is exactly a ticket's locking script.
pub fn is_ticket_script(script: &[u8]) -> bool {
    script.len() == TICKET_SCRIPT_LEN
        && script[..3] == [0x00, 0xce, 0x21]
        && matches!(script[35], 1 | 2)
        && script[36] == 0x87
}

/// The tokens registered on `network`: none yet, on either network.
pub fn tokens(network: MiningNetwork) -> &'static [MergeToken] {
    match network {
        MiningNetwork::Mainnet => &[],
        MiningNetwork::Chipnet => &[],
    }
}

/// SHA-256 of "pickaxe merge-mining test token v1".
const TEST_CATEGORY: Hash = [
    0xfe, 0xab, 0x4b, 0xcd, 0x32, 0x4f, 0x72, 0x20, 0x33, 0xf5, 0xa1, 0xd6, 0x74, 0x32, 0xf4, 0x5c,
    0xae, 0xba, 0x0e, 0x50, 0x5b, 0x5c, 0xca, 0xaf, 0x66, 0x55, 0x55, 0x76, 0x94, 0x25, 0xad, 0x93,
];

/// A Chipnet-only token with both modes and no covenant: it exercises the
/// whole path (commitment, shares, proofs) without any real token.
pub const TEST_TOKEN: MergeToken = MergeToken {
    name: "Pickaxe test token",
    network: MiningNetwork::Chipnet,
    category: TEST_CATEGORY,
    modes: &[Mode::ShareTarget, Mode::BlockRequired],
    deployment: None,
    verifier_sha256: None,
    max_aux_height: MAX_HEIGHT,
    start_height: 0,
    ticket: Some(TicketPolicy {
        capability: 2,
        value_sats: 0,
    }),
    priority: 0,
};

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
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
        assert!(TEST_TOKEN.supports(Mode::ShareTarget) && TEST_TOKEN.supports(Mode::BlockRequired));
        // Its ticket: OP_0 OP_UTXOTOKENCATEGORY <category ‖ 02> OP_EQUAL.
        let ticket = TEST_TOKEN.ticket_output().unwrap();
        assert_eq!(
            ticket[..12],
            [0, 0, 0, 0, 0, 0, 0, 0, 0x25, 0x00, 0xce, 0x21]
        );
        assert_eq!(ticket[12..44], TEST_CATEGORY);
        assert_eq!(ticket[44..], [0x02, 0x87]);
        assert!(is_ticket_script(&ticket[9..]));
        for (index, value) in [(9, 0x51), (10, 0xcf), (11, 0x20), (44, 0x00), (45, 0x88)] {
            let mut changed = ticket;
            changed[index] = value;
            assert!(!is_ticket_script(&changed[9..]), "byte {index}");
        }
        assert!(!is_ticket_script(&ticket[8..]));
        // Rows that could not be mined or claimed as written are refused.
        let refused: [fn(&mut MergeToken); 8] = [
            |t| t.name = " ",
            |t| t.category = [0; 32],
            |t| t.modes = &[],
            |t| t.modes = &[Mode::ShareTarget, Mode::ShareTarget],
            |t| t.max_aux_height = MAX_HEIGHT + 1,
            |t| t.ticket = None,
            |t| t.ticket.as_mut().unwrap().capability = 0,
            |t| t.ticket.as_mut().unwrap().value_sats = 2_000,
        ];
        for (index, change) in refused.into_iter().enumerate() {
            let mut token = TEST_TOKEN;
            change(&mut token);
            assert!(token.verify().is_err(), "row {index}");
        }
        let mut a_only = TEST_TOKEN;
        a_only.modes = &[Mode::ShareTarget];
        assert!(a_only.verify().is_err());
        a_only.ticket = None;
        a_only.verify().unwrap();
        assert_eq!(a_only.ticket_output(), None);
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
