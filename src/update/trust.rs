//! Which releases install by themselves (docs/updates.md).
//!
//! A GitHub release alone is not trusted: the account that publishes them
//! was used to push malicious code in 2026, so whoever holds it can publish
//! a release too. A release is trusted only when its checksum file
//! (`SHA256SUMS.txt`) carries a minisign signature (`SHA256SUMS.txt.minisig`)
//! from a public key compiled in below, whose secret key the owner keeps
//! offline. The archive must then match the checksum the signed file gives.
//!
//! Without a key here, or without a valid signature on a release, Pickaxe
//! still checks for releases and shows them, but installs one only when
//! asked, after saying it is unsigned and why.

/// Minisign public keys whose signatures Pickaxe trusts: the second line of
/// `minisign.pub` (it starts with `RW`), one string per key. Empty: no
/// release is trusted, so none installs by itself.
pub const TRUSTED_KEYS: &[&str] = &[];

/// The same for P2Pool's releases (`p2pool::release`), when they publish a
/// signed `SHA256SUMS`. Empty: P2Pool installs as before, checked against
/// its published SHA-256 only, and shows as unsigned.
pub const P2POOL_TRUSTED_KEYS: &[&str] = &[];

/// The checksum file a release publishes beside its archives, and the
/// older name it may have.
pub const SUMS_NAMES: [&str; 2] = ["SHA256SUMS.txt", "SHA256SUMS"];

/// The signature file for a checksum file.
pub fn signature_name(sums: &str) -> String {
    format!("{sums}.minisig")
}

/// Whether this build trusts any key for Pickaxe's releases.
pub fn has_key() -> bool {
    !TRUSTED_KEYS.is_empty()
}

/// What a release's signature says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Signed by one of the trusted keys (the start of the key, to tell
    /// keys apart).
    Signed {
        key: String,
    },
    Unsigned(Unsigned),
}

impl Trust {
    pub fn is_signed(&self) -> bool {
        matches!(self, Self::Signed { .. })
    }

    /// One short word or phrase for status lines.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Signed { .. } => "signed",
            Self::Unsigned(_) => "unsigned",
        }
    }
}

/// Why a release counts as unsigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsigned {
    /// This build has no key to check a signature with.
    NoKey,
    /// The release publishes no signature file.
    NoSignature,
    /// The signature is from a key this build does not trust.
    UnknownKey,
    /// The signature does not match the checksum file: the file was changed
    /// after it was signed, or the signature was made for another file.
    BadSignature,
    /// The signature file is not a minisign signature.
    Unreadable,
}

impl Unsigned {
    /// Why, in a sentence a person can act on.
    pub fn why(&self) -> &'static str {
        match self {
            Self::NoKey => {
                "this build of Pickaxe has no signing key to check releases with, so it cannot \
                 tell a release the owner made from one anyone with access to the GitHub \
                 account could publish"
            }
            Self::NoSignature => {
                "the release has no signature from Pickaxe's signing key, so it cannot be told \
                 from a release anyone with access to the GitHub account could publish"
            }
            Self::UnknownKey => "the release is signed with a key this build does not trust",
            Self::BadSignature => {
                "the release's signature does not match its checksum file: the file was changed \
                 after it was signed"
            }
            Self::Unreadable => "the release's signature file is not a minisign signature",
        }
    }
}

/// Checks `signature` (a `.minisig` file's text) over `sums` against
/// `keys`. Only minisign's default, pre-hashed signatures count.
pub fn verify(sums: &[u8], signature: Option<&str>, keys: &[&str]) -> Trust {
    use minisign_verify::{Error, PublicKey, Signature};
    if keys.is_empty() {
        return Trust::Unsigned(Unsigned::NoKey);
    }
    let Some(signature) = signature else {
        return Trust::Unsigned(Unsigned::NoSignature);
    };
    let Ok(signature) = Signature::decode(signature) else {
        return Trust::Unsigned(Unsigned::Unreadable);
    };
    let mut why = Unsigned::UnknownKey;
    for key in keys {
        // A key that does not decode trusts nothing.
        let Ok(public) = PublicKey::from_base64(key.trim()) else {
            continue;
        };
        match public.verify(sums, &signature, false) {
            Ok(()) => {
                return Trust::Signed {
                    key: key.trim().chars().take(12).collect(),
                }
            }
            Err(Error::UnexpectedKeyId) => {}
            Err(_) => why = Unsigned::BadSignature,
        }
    }
    Trust::Unsigned(why)
}

/// The SHA-256 a checksum file (`sha256sum` output: `HEX  NAME` or
/// `HEX *NAME` per line) gives for the file named exactly `asset`.
pub fn sums_entry(sums: &str, asset: &str) -> Result<[u8; 32], String> {
    let mut found = None;
    for line in sums.lines() {
        let Some((hex_text, name)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        if name != asset {
            continue;
        }
        if found.is_some() {
            return Err(format!("the checksum file names {asset} twice"));
        }
        let bytes = hex::decode(hex_text).map_err(|_| format!("bad SHA-256 for {asset}"))?;
        found = Some(<[u8; 32]>::try_from(bytes).map_err(|_| format!("bad SHA-256 for {asset}"))?);
    }
    found.ok_or_else(|| format!("the checksum file names no {asset}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Minisign's published test vectors (minisign-verify's own tests): the
    // key below signed the four bytes `test`, pre-hashed. Only public data;
    // no secret key exists in this repository.
    pub(crate) const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    pub(crate) const SIGNED: &[u8] = b"test";
    pub(crate) const SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==
";
    /// The same vector signed the legacy way (not pre-hashed).
    const LEGACY: &str = "untrusted comment: signature from minisign secret key
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=
trusted comment: timestamp:1555779966\tfile:test
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==
";
    /// The key's id with other key bytes: a different key, same id.
    const SAME_ID_OTHER_KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO4";
    /// Another key id entirely.
    const OTHER_ID: &str = "RWQg6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    #[test]
    fn a_valid_signature_from_a_trusted_key_is_signed() {
        assert_eq!(
            verify(SIGNED, Some(SIGNATURE), &[KEY]),
            Trust::Signed {
                key: "RWQf6LRCGA9i".into()
            }
        );
        // A trusted key among others still counts.
        assert!(verify(SIGNED, Some(SIGNATURE), &[OTHER_ID, "not a key", KEY]).is_signed());
    }

    #[test]
    fn the_wrong_key_tampered_sums_and_a_missing_signature_are_unsigned() {
        // A key with another id never signed it.
        assert_eq!(
            verify(SIGNED, Some(SIGNATURE), &[OTHER_ID]),
            Trust::Unsigned(Unsigned::UnknownKey)
        );
        // Same id, other key: the signature does not verify.
        assert_eq!(
            verify(SIGNED, Some(SIGNATURE), &[SAME_ID_OTHER_KEY]),
            Trust::Unsigned(Unsigned::BadSignature)
        );
        // One changed byte in the checksum file.
        assert_eq!(
            verify(b"tesT", Some(SIGNATURE), &[KEY]),
            Trust::Unsigned(Unsigned::BadSignature)
        );
        // No signature file, or none that decodes.
        assert_eq!(
            verify(SIGNED, None, &[KEY]),
            Trust::Unsigned(Unsigned::NoSignature)
        );
        assert_eq!(
            verify(SIGNED, Some("untrusted comment: x\nnot base64\n"), &[KEY]),
            Trust::Unsigned(Unsigned::Unreadable)
        );
        // A changed trusted comment breaks the global signature.
        let changed = SIGNATURE.replace("file:test", "file:tesT");
        assert_eq!(
            verify(SIGNED, Some(&changed), &[KEY]),
            Trust::Unsigned(Unsigned::BadSignature)
        );
        // Legacy (not pre-hashed) signatures are refused.
        assert_eq!(
            verify(SIGNED, Some(LEGACY), &[KEY]),
            Trust::Unsigned(Unsigned::BadSignature)
        );
        // No key compiled in: nothing is signed, whatever the release has.
        assert_eq!(
            verify(SIGNED, Some(SIGNATURE), &[]),
            Trust::Unsigned(Unsigned::NoKey)
        );
    }

    #[test]
    fn this_build_ships_without_a_key() {
        // The owner adds the key, made offline; until then no release
        // installs by itself (docs/updates.md).
        assert!(TRUSTED_KEYS.is_empty() && P2POOL_TRUSTED_KEYS.is_empty());
        assert!(!has_key());
        assert_eq!(signature_name("SHA256SUMS.txt"), "SHA256SUMS.txt.minisig");
    }

    #[test]
    fn a_checksum_file_names_each_archive_once_exactly() {
        let a = "ab".repeat(32);
        let b = "cd".repeat(32);
        let sums = format!(
            "{a}  pickaxe-miner-v0.0.5-windows-x86_64.zip\n{b} *pickaxe-miner-v0.0.5-linux-x86_64.tar.gz\n"
        );
        assert_eq!(
            sums_entry(&sums, "pickaxe-miner-v0.0.5-windows-x86_64.zip").unwrap(),
            [0xab; 32]
        );
        assert_eq!(
            sums_entry(&sums, "pickaxe-miner-v0.0.5-linux-x86_64.tar.gz").unwrap(),
            [0xcd; 32]
        );
        // Another version's archive, a bare checksum and a prefix match
        // name nothing.
        assert!(sums_entry(&sums, "pickaxe-miner-v0.0.4-windows-x86_64.zip").is_err());
        assert!(sums_entry(&a, "x.zip").is_err());
        assert!(sums_entry(&sums, "pickaxe-miner-v0.0.5-windows-x86_64").is_err());
        let twice = format!("{sums}{b}  pickaxe-miner-v0.0.5-windows-x86_64.zip\n");
        assert!(
            sums_entry(&twice, "pickaxe-miner-v0.0.5-windows-x86_64.zip")
                .unwrap_err()
                .contains("twice")
        );
        assert!(sums_entry("zz  a.zip", "a.zip").is_err());
    }
}
