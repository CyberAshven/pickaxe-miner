//! #### PR #42
//! Pickaxe's Job Declaration tokens. The spec leaves `mining_job_token`
//! opaque; a Pickaxe pool's tokens carry its rates (PX v1, 26 bytes), so a
//! Pickaxe client can build a coinbase the pool accepts, and other clients
//! keep them opaque.
//!
//! ```text
//! 0  2 "PX" | 2 1 version 01 | 3 2 donation_bps u16le | 5 2 fee_bps u16le
//! 7  1 donation_output (ff = none) | 8 1 fee_output (ff = none)
//! 9  1 flags: bit 0 declared, bit 1 full-template connection
//! 10 8 serial u64le | 18 8 secret (never logged)
//! ```

use super::{MAX_TOKENS, TOKEN_TTL};
use std::{collections::BTreeMap, fmt, time::Instant};

pub const PX_LEN: usize = 26;
const MAGIC: [u8; 2] = *b"PX";
const VERSION: u8 = 1;

/// What a pool's custom jobs must pay: the whole donation and fee, in
/// hundredths of a percent, and where in the allocated outputs each sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolRates {
    pub donation_bps: u16,
    pub fee_bps: u16,
    pub donation_output: Option<u8>,
    pub fee_output: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PxToken {
    pub rates: PoolRates,
    pub declared: bool,
    pub full_template: bool,
    pub serial: u64,
    pub secret: [u8; 8],
}

/// Never prints the secret.
impl fmt::Debug for PxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PxToken")
            .field("rates", &self.rates)
            .field("declared", &self.declared)
            .field("full_template", &self.full_template)
            .field("serial", &self.serial)
            .finish_non_exhaustive()
    }
}

impl PxToken {
    pub fn encode(&self) -> [u8; PX_LEN] {
        let mut bytes = [0; PX_LEN];
        bytes[..2].copy_from_slice(&MAGIC);
        bytes[2] = VERSION;
        bytes[3..5].copy_from_slice(&self.rates.donation_bps.to_le_bytes());
        bytes[5..7].copy_from_slice(&self.rates.fee_bps.to_le_bytes());
        bytes[7] = self.rates.donation_output.unwrap_or(0xff);
        bytes[8] = self.rates.fee_output.unwrap_or(0xff);
        bytes[9] = u8::from(self.declared) | (u8::from(self.full_template) << 1);
        bytes[10..18].copy_from_slice(&self.serial.to_le_bytes());
        bytes[18..].copy_from_slice(&self.secret);
        bytes
    }

    /// A PX v1 token, or `None` for anything else (another pool's opaque
    /// token, such as SRI's 8-byte counter).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; PX_LEN] = bytes.try_into().ok()?;
        if bytes[..2] != MAGIC || bytes[2] != VERSION || bytes[9] & !0b11 != 0 {
            return None;
        }
        let index = |byte: u8| (byte != 0xff).then_some(byte);
        Some(Self {
            rates: PoolRates {
                donation_bps: u16::from_le_bytes([bytes[3], bytes[4]]),
                fee_bps: u16::from_le_bytes([bytes[5], bytes[6]]),
                donation_output: index(bytes[7]),
                fee_output: index(bytes[8]),
            },
            declared: bytes[9] & 1 != 0,
            full_template: bytes[9] & 2 != 0,
            serial: u64::from_le_bytes(bytes[10..18].try_into().ok()?),
            secret: bytes[18..].try_into().ok()?,
        })
    }
}

struct Entry {
    owner: u64,
    payout: String,
    rates: PoolRates,
    secret: [u8; 8],
    issued: Instant,
}

/// The tokens a pool has allocated and not yet redeemed. Each is bound to
/// the connection that asked for it and to the payout address its
/// `user_identifier` names, lives 10 minutes and is redeemed once.
#[derive(Default)]
pub struct TokenBook {
    next_serial: u64,
    entries: BTreeMap<u64, Entry>,
}

impl TokenBook {
    /// A new token for `owner` (a connection) whose jobs pay `payout`.
    pub fn allocate(
        &mut self,
        owner: u64,
        payout: String,
        rates: PoolRates,
        now: Instant,
    ) -> Result<PxToken, &'static str> {
        self.expire(now);
        if self.entries.len() >= MAX_TOKENS {
            // The owner's oldest unredeemed token gives way; another owner's
            // never does.
            let oldest = self
                .entries
                .iter()
                .find(|(_, entry)| entry.owner == owner)
                .map(|(serial, _)| *serial)
                .ok_or("the pool holds too many tokens")?;
            self.entries.remove(&oldest);
        }
        self.next_serial = self.next_serial.checked_add(1).ok_or("tokens exhausted")?;
        let secret: [u8; 8] = rand::random();
        self.entries.insert(
            self.next_serial,
            Entry {
                owner,
                payout,
                rates,
                secret,
                issued: now,
            },
        );
        Ok(PxToken {
            rates,
            declared: false,
            full_template: false,
            serial: self.next_serial,
            secret,
        })
    }

    /// The rates a token's job must pay when `payout` (the channel's) is
    /// the address it was allocated for; the token is spent either way once
    /// it matched its secret.
    pub fn redeem(
        &mut self,
        token: &[u8],
        payout: &str,
        now: Instant,
    ) -> Result<PoolRates, &'static str> {
        const INVALID: &str = "invalid-mining-job-token";
        self.expire(now);
        let token = PxToken::decode(token).ok_or(INVALID)?;
        let entry = self.entries.get(&token.serial).ok_or(INVALID)?;
        if entry.secret != token.secret {
            return Err(INVALID);
        }
        let entry = self.entries.remove(&token.serial).ok_or(INVALID)?;
        if entry.payout != payout {
            return Err(INVALID);
        }
        Ok(entry.rates)
    }

    /// Drops tokens older than `TOKEN_TTL`.
    pub fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.issued) < TOKEN_TTL);
    }

    /// Drops the tokens of a connection that closed.
    pub fn drop_owner(&mut self, owner: u64) {
        self.entries.retain(|_, entry| entry.owner != owner);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rates() -> PoolRates {
        PoolRates {
            donation_bps: 150,
            fee_bps: 100,
            donation_output: Some(2),
            fee_output: Some(1),
        }
    }

    // #### PR #42
    // What: a PX v1 token is the design's 26 bytes; another pool's token
    // (SRI's 8-byte counter, another version or magic) is opaque.
    // Look here if: PxToken's layout changes.
    #[test]
    fn px_v1_layout_is_26_bytes_and_foreign_tokens_are_opaque() {
        let token = PxToken {
            rates: rates(),
            declared: false,
            full_template: true,
            serial: 7,
            secret: [0x5a; 8],
        };
        let bytes = token.encode();
        assert_eq!(
            hex::encode(bytes),
            format!("5058019600640002010207{}{}", "00".repeat(7), "5a".repeat(8))
        );
        assert_eq!(PxToken::decode(&bytes), Some(token));
        assert_eq!(PxToken::decode(&7u64.to_le_bytes()), None);
        let mut other = bytes;
        other[2] = 2;
        assert_eq!(PxToken::decode(&other), None);
        let mut flags = bytes;
        flags[9] = 4;
        assert_eq!(PxToken::decode(&flags), None);
        let none = PxToken {
            rates: PoolRates {
                donation_output: None,
                fee_output: None,
                ..rates()
            },
            ..token
        };
        assert_eq!(&none.encode()[7..9], &[0xff, 0xff]);
        assert!(!format!("{token:?}").contains("5a"));
    }

    // #### PR #42
    // What: a token redeems once, for the payout it was allocated to,
    // with its own secret, within 10 minutes; a closed connection's tokens
    // go; a full book evicts only the asking connection's oldest.
    // Look here if: TokenBook changes.
    #[test]
    fn book_binds_identity_secret_and_ttl_and_is_single_use() {
        let now = Instant::now();
        let mut book = TokenBook::default();
        let token = book.allocate(1, "payout-a".into(), rates(), now).unwrap();
        let bytes = token.encode();
        assert_eq!(
            book.redeem(&bytes, "payout-b", now),
            Err("invalid-mining-job-token"),
            "another identity spends it"
        );
        assert!(book.redeem(&bytes, "payout-a", now).is_err(), "spent");
        let token = book.allocate(1, "payout-a".into(), rates(), now).unwrap();
        let mut forged = token.encode();
        forged[25] ^= 1;
        assert!(book.redeem(&forged, "payout-a", now).is_err());
        assert_eq!(book.redeem(&token.encode(), "payout-a", now), Ok(rates()));
        let late = book.allocate(1, "payout-a".into(), rates(), now).unwrap();
        assert!(book
            .redeem(&late.encode(), "payout-a", now + TOKEN_TTL)
            .is_err());
        book.allocate(2, "payout-c".into(), rates(), now).unwrap();
        book.allocate(3, "payout-d".into(), rates(), now).unwrap();
        book.drop_owner(2);
        assert_eq!(book.len(), 1);
        let mut full = TokenBook::default();
        let first = full.allocate(9, "x".into(), rates(), now).unwrap();
        for _ in 1..MAX_TOKENS {
            full.allocate(8, "y".into(), rates(), now).unwrap();
        }
        assert!(full.allocate(10, "z".into(), rates(), now).is_err());
        full.allocate(9, "x".into(), rates(), now + Duration::from_secs(1))
            .unwrap();
        assert!(
            full.redeem(&first.encode(), "x", now).is_err(),
            "the owner's oldest gave way"
        );
    }
}
