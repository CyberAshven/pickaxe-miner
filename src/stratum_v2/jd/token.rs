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

use super::{declared::DeclaredJob, DECLARED_TTL, MAX_TOKENS, TOKEN_TTL};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Weak},
    time::Instant,
};

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
    kind: Kind,
}

enum Kind {
    /// From AllocateMiningJobToken, on a Full-Template connection or not.
    Allocated { full_template: bool },
    /// From DeclareMiningJob.Success: the job the pool checked, held by its
    /// connection (the last few it declared), so a token whose job went is
    /// spent.
    Declared(Weak<DeclaredJob>),
}

/// What a redeemed token sets.
pub enum Redeemed {
    /// A Coinbase-only custom job, which must pay these rates.
    Allocated(PoolRates),
    /// A Full-Template custom job, which must match this declared job.
    Declared(Arc<DeclaredJob>),
}

/// The tokens a pool has allocated and not yet redeemed. Each is bound to
/// the connection that asked for it and to the payout address its
/// `user_identifier` names, lives 10 minutes (60 once declared) and is
/// redeemed once.
#[derive(Default)]
pub struct TokenBook {
    next_serial: u64,
    entries: BTreeMap<u64, Entry>,
}

impl TokenBook {
    /// A new token for `owner` (a connection) whose jobs pay `payout`;
    /// `full_template` when the connection declares its templates.
    pub fn allocate(
        &mut self,
        owner: u64,
        payout: String,
        rates: PoolRates,
        full_template: bool,
        now: Instant,
    ) -> Result<PxToken, &'static str> {
        self.insert(owner, payout, rates, Kind::Allocated { full_template }, now)
    }

    fn insert(
        &mut self,
        owner: u64,
        payout: String,
        rates: PoolRates,
        kind: Kind,
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
        let (declared, full_template) = match &kind {
            Kind::Allocated { full_template } => (false, *full_template),
            Kind::Declared(_) => (true, true),
        };
        self.entries.insert(
            self.next_serial,
            Entry {
                owner,
                payout,
                rates,
                secret,
                issued: now,
                kind,
            },
        );
        Ok(PxToken {
            rates,
            declared,
            full_template,
            serial: self.next_serial,
            secret,
        })
    }

    /// Spends a Full-Template connection's allocated token on a declaration
    /// by `owner`: the address its jobs pay and its rates.
    pub fn declare(
        &mut self,
        token: &[u8],
        owner: u64,
        now: Instant,
    ) -> Result<(String, PoolRates), &'static str> {
        const INVALID: &str = "invalid-mining-job-token";
        self.expire(now);
        let token = PxToken::decode(token).ok_or(INVALID)?;
        let entry = self.entries.get(&token.serial).ok_or(INVALID)?;
        if entry.secret != token.secret
            || entry.owner != owner
            || !matches!(
                entry.kind,
                Kind::Allocated {
                    full_template: true
                }
            )
            || token.declared
            || !token.full_template
        {
            return Err(INVALID);
        }
        let entry = self.entries.remove(&token.serial).ok_or(INVALID)?;
        Ok((entry.payout, entry.rates))
    }

    /// The token of a declaration the pool checked: `job` is held by the
    /// declaring connection, which keeps its last few.
    pub fn declared(
        &mut self,
        owner: u64,
        payout: String,
        rates: PoolRates,
        job: &Arc<DeclaredJob>,
        now: Instant,
    ) -> Result<PxToken, &'static str> {
        self.insert(
            owner,
            payout,
            rates,
            Kind::Declared(Arc::downgrade(job)),
            now,
        )
    }

    /// What a token sets when `payout` (the channel's) is the address it was
    /// allocated for. The token is spent once it matched its secret, unless
    /// it is a Full-Template connection's token that was not declared yet
    /// (`job-not-yet-validated`).
    pub fn redeem(
        &mut self,
        token: &[u8],
        payout: &str,
        now: Instant,
    ) -> Result<Redeemed, &'static str> {
        const INVALID: &str = "invalid-mining-job-token";
        self.expire(now);
        let token = PxToken::decode(token).ok_or(INVALID)?;
        let entry = self.entries.get(&token.serial).ok_or(INVALID)?;
        if entry.secret != token.secret {
            return Err(INVALID);
        }
        if matches!(
            entry.kind,
            Kind::Allocated {
                full_template: true
            }
        ) && entry.payout == payout
        {
            return Err("job-not-yet-validated");
        }
        let entry = self.entries.remove(&token.serial).ok_or(INVALID)?;
        if entry.payout != payout {
            return Err(INVALID);
        }
        match entry.kind {
            Kind::Allocated { .. } => Ok(Redeemed::Allocated(entry.rates)),
            Kind::Declared(job) => job.upgrade().map(Redeemed::Declared).ok_or(INVALID),
        }
    }

    /// Drops tokens older than `TOKEN_TTL` (`DECLARED_TTL` once declared).
    pub fn expire(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            let ttl = match entry.kind {
                Kind::Allocated { .. } => TOKEN_TTL,
                Kind::Declared(_) => DECLARED_TTL,
            };
            now.saturating_duration_since(entry.issued) < ttl
        });
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

    fn allocated(result: Result<Redeemed, &'static str>) -> Result<PoolRates, &'static str> {
        match result? {
            Redeemed::Allocated(rates) => Ok(rates),
            Redeemed::Declared(_) => Err("declared"),
        }
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
        let token = book
            .allocate(1, "payout-a".into(), rates(), false, now)
            .unwrap();
        let bytes = token.encode();
        assert_eq!(
            allocated(book.redeem(&bytes, "payout-b", now)),
            Err("invalid-mining-job-token"),
            "another identity spends it"
        );
        assert!(book.redeem(&bytes, "payout-a", now).is_err(), "spent");
        let token = book
            .allocate(1, "payout-a".into(), rates(), false, now)
            .unwrap();
        let mut forged = token.encode();
        forged[25] ^= 1;
        assert!(book.redeem(&forged, "payout-a", now).is_err());
        assert_eq!(
            allocated(book.redeem(&token.encode(), "payout-a", now)),
            Ok(rates())
        );
        let late = book
            .allocate(1, "payout-a".into(), rates(), false, now)
            .unwrap();
        assert!(book
            .redeem(&late.encode(), "payout-a", now + TOKEN_TTL)
            .is_err());
        book.allocate(2, "payout-c".into(), rates(), false, now)
            .unwrap();
        book.allocate(3, "payout-d".into(), rates(), false, now)
            .unwrap();
        book.drop_owner(2);
        assert_eq!(book.len(), 1);
        let mut full = TokenBook::default();
        let first = full.allocate(9, "x".into(), rates(), false, now).unwrap();
        for _ in 1..MAX_TOKENS {
            full.allocate(8, "y".into(), rates(), false, now).unwrap();
        }
        assert!(full.allocate(10, "z".into(), rates(), false, now).is_err());
        full.allocate(9, "x".into(), rates(), false, now + Duration::from_secs(1))
            .unwrap();
        assert!(
            full.redeem(&first.encode(), "x", now).is_err(),
            "the owner's oldest gave way"
        );
    }

    // #### PR #42
    // What: a Full-Template connection's token is spent by its own
    // connection's declaration and answers SetCustomMiningJob with
    // job-not-yet-validated until then; another connection, a declared or
    // Coinbase-only token cannot declare; the declared token carries the
    // declared flag, redeems once to its job for the same payout, lives an
    // hour, and is spent once its connection dropped the job.
    // Look here if: TokenBook::declare, declared or redeem change.
    #[test]
    fn declared_tokens_bind_the_job_and_allocated_full_template_tokens_wait_for_it() {
        let now = Instant::now();
        let mut book = TokenBook::default();
        let token = book.allocate(1, "a".into(), rates(), true, now).unwrap();
        assert!(token.full_template && !token.declared);
        assert_eq!(
            book.redeem(&token.encode(), "a", now).err(),
            Some("job-not-yet-validated")
        );
        assert!(
            book.declare(&token.encode(), 2, now).is_err(),
            "another owner"
        );
        let coinbase_only = book.allocate(1, "a".into(), rates(), false, now).unwrap();
        assert!(book.declare(&coinbase_only.encode(), 1, now).is_err());
        assert_eq!(
            book.declare(&token.encode(), 1, now),
            Ok(("a".into(), rates()))
        );
        assert!(book.declare(&token.encode(), 1, now).is_err(), "spent");
        let (prefix, suffix) = super::super::declared::tests::shape_bytes(&[0x51], 32);
        let job = Arc::new(DeclaredJob {
            shape: super::super::declared::CoinbaseShape::parse(&prefix, &suffix).unwrap(),
            template: Arc::new(super::super::declared::tests::context(0)),
        });
        let declared = book.declared(1, "a".into(), rates(), &job, now).unwrap();
        assert!(declared.declared && declared.full_template);
        assert!(book.declare(&declared.encode(), 1, now).is_err());
        assert!(book.redeem(&declared.encode(), "b", now).is_err());
        let declared = book.declared(1, "a".into(), rates(), &job, now).unwrap();
        let later = now + TOKEN_TTL + Duration::from_secs(1);
        match book.redeem(&declared.encode(), "a", later) {
            Ok(Redeemed::Declared(found)) => assert!(Arc::ptr_eq(&found, &job)),
            _ => panic!("the declared job"),
        }
        let gone = book.declared(1, "a".into(), rates(), &job, now).unwrap();
        drop(job);
        assert!(book.redeem(&gone.encode(), "a", now).is_err());
        let old = book.allocate(1, "a".into(), rates(), false, now).unwrap();
        assert!(book.redeem(&old.encode(), "a", now + DECLARED_TTL).is_err());
    }
}
