//! OLAP-CRED-1 (#1493): the per-cell credential a cell presents to
//! `norn-olapd`, the tenant analytics engine.
//!
//! A credential names exactly one tenant (the cell's own database) and an
//! expiry, signed with Ed25519 by the host's signing key:
//!
//! ```text
//! nolap1.<kid>.<tenant>.<expires_ms>.<signature, base64url>
//! ```
//!
//! The cell mints one for every engine call, for its own tenant only, with
//! [`CREDENTIAL_LIFETIME_MS`] to live. The engine holds only public keys
//! (a [`Keyring`]), so it cannot mint, and it serves a call only when the
//! credential verifies, has not expired, and names the tenant the request
//! reads or writes. Key rotation is an overlap: add the new public key to
//! the engine's keyring, move the host to the new signing key, and remove
//! the old public key once [`MAX_CREDENTIAL_LIFETIME_MS`] has passed.
//! There is no shared token and no fallback to one.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use norn_cell::analytics::TenantId;

/// The credential format's prefix.
const PREFIX: &str = "nolap1";
/// Domain separation for the signed bytes.
const DOMAIN: &[u8] = b"norn-olapd/credential/v1";
/// How long a minted credential lives, in milliseconds.
pub const CREDENTIAL_LIFETIME_MS: u64 = 120_000;
/// The longest remaining life the engine accepts, in milliseconds: the
/// minted lifetime plus room for clock skew between host and engine. A
/// credential claiming more is refused, so none outlives a key rotation's
/// overlap window.
pub const MAX_CREDENTIAL_LIFETIME_MS: u64 = 600_000;
/// The longest key id.
const MAX_KID_BYTES: usize = 64;
/// The most keys a keyring holds.
const MAX_KEYS: usize = 16;

/// Why a credential was refused. The engine answers every one of these the
/// same way; the variant is for its operator log only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// Not a credential of this format.
    Malformed,
    /// Signed by a key the keyring does not hold (never added, or rotated out).
    UnknownKey,
    /// The signature does not verify.
    BadSignature,
    /// Past its expiry.
    Expired,
    /// Claims to live longer than [`MAX_CREDENTIAL_LIFETIME_MS`].
    TooLong,
    /// Valid, but for another tenant than the request names.
    OtherTenant,
}

impl Denied {
    /// A stable label for the operator log.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownKey => "unknown_key",
            Self::BadSignature => "bad_signature",
            Self::Expired => "expired",
            Self::TooLong => "too_long",
            Self::OtherTenant => "other_tenant",
        }
    }
}

fn valid_kid(kid: &str) -> bool {
    !kid.is_empty()
        && kid.len() <= MAX_KID_BYTES
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Split `<kid>:<base64url 32 bytes>`.
fn key_spec(spec: &str) -> Result<(String, [u8; 32]), String> {
    let (kid, key) = spec
        .trim()
        .split_once(':')
        .ok_or("a key is <kid>:<base64url of 32 bytes>")?;
    if !valid_kid(kid) {
        return Err(format!("a key id is 1 to {MAX_KID_BYTES} of [A-Za-z0-9_-]"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(key.trim())
        .map_err(|_e| "a key is base64url without padding".to_owned())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_e| "a key is exactly 32 bytes".to_owned())?;
    Ok((kid.to_owned(), bytes))
}

fn signed_bytes(kid: &str, tenant: &str, expires_ms: u64) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    for part in [kid, tenant, &expires_ms.to_string()] {
        bytes.push(0);
        bytes.extend_from_slice(part.as_bytes());
    }
    bytes
}

/// The host's signing key: what a cell mints its credentials with.
pub struct Issuer {
    kid: String,
    key: SigningKey,
}

impl std::fmt::Debug for Issuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issuer")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl Issuer {
    /// Read `<kid>:<base64url of the 32-byte Ed25519 seed>`
    /// (`NORN_SECRET_OLAP_SIGNING_KEY`).
    ///
    /// # Errors
    /// A malformed key id or key.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (kid, seed) = key_spec(spec)?;
        Ok(Self {
            kid,
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// The keyring line that verifies this issuer's credentials:
    /// `<kid>:<base64url public key>`.
    #[must_use]
    pub fn public_spec(&self) -> String {
        format!(
            "{}:{}",
            self.kid,
            URL_SAFE_NO_PAD.encode(self.key.verifying_key().as_bytes())
        )
    }

    /// A credential for `tenant` alone, living [`CREDENTIAL_LIFETIME_MS`] from `now_ms`.
    #[must_use]
    pub fn mint(&self, tenant: &TenantId, now_ms: u64) -> String {
        self.mint_until(tenant, now_ms.saturating_add(CREDENTIAL_LIFETIME_MS))
    }

    /// A credential for `tenant` alone that expires at `expires_ms`.
    #[must_use]
    pub fn mint_until(&self, tenant: &TenantId, expires_ms: u64) -> String {
        let signature = self
            .key
            .sign(&signed_bytes(&self.kid, tenant.as_str(), expires_ms));
        format!(
            "{PREFIX}.{}.{}.{expires_ms}.{}",
            self.kid,
            tenant.as_str(),
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }
}

/// The public keys the engine accepts credentials from, by key id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyring {
    keys: BTreeMap<String, VerifyingKey>,
}

impl Keyring {
    /// Read one `<kid>:<base64url public key>` per line; blank lines and
    /// lines starting with `#` are skipped. During a rotation the keyring
    /// holds both the outgoing and the incoming key.
    ///
    /// # Errors
    /// No key, too many keys, a repeated key id, or a malformed line.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut keys = BTreeMap::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (kid, bytes) = key_spec(line).map_err(|e| format!("line {}: {e}", number + 1))?;
            let key = VerifyingKey::from_bytes(&bytes)
                .map_err(|_e| format!("line {}: not an Ed25519 public key", number + 1))?;
            if keys.insert(kid.clone(), key).is_some() {
                return Err(format!("line {}: key id {kid} is repeated", number + 1));
            }
        }
        if keys.is_empty() {
            return Err("the keyring holds no key".to_owned());
        }
        if keys.len() > MAX_KEYS {
            return Err(format!("the keyring holds more than {MAX_KEYS} keys"));
        }
        Ok(Self { keys })
    }

    /// The tenant `credential` was minted for, when it verifies under a key
    /// of this keyring and is live at `now_ms`.
    ///
    /// # Errors
    /// Why the credential is refused.
    pub fn verify(&self, credential: &str, now_ms: u64) -> Result<TenantId, Denied> {
        let mut parts = credential.split('.');
        let (Some(PREFIX), Some(kid), Some(tenant), Some(expires), Some(signature), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(Denied::Malformed);
        };
        let tenant = TenantId::try_from(tenant.to_owned()).map_err(|_e| Denied::Malformed)?;
        let expires_ms: u64 = expires.parse().map_err(|_e| Denied::Malformed)?;
        if expires_ms.to_string() != expires {
            return Err(Denied::Malformed);
        }
        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(signature)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Denied::Malformed)?;
        let key = self.keys.get(kid).ok_or(Denied::UnknownKey)?;
        key.verify_strict(
            &signed_bytes(kid, tenant.as_str(), expires_ms),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_e| Denied::BadSignature)?;
        if expires_ms <= now_ms {
            return Err(Denied::Expired);
        }
        if expires_ms - now_ms > MAX_CREDENTIAL_LIFETIME_MS {
            return Err(Denied::TooLong);
        }
        Ok(tenant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn issuer(kid: &str, seed: u8) -> Issuer {
        Issuer::parse(&format!("{kid}:{}", URL_SAFE_NO_PAD.encode([seed; 32]))).unwrap()
    }

    fn tenant(scope: &str) -> TenantId {
        TenantId::of_scope(scope)
    }

    #[test]
    fn a_minted_credential_names_its_tenant_until_it_expires() {
        let issuer = issuer("k1", 7);
        let keyring = Keyring::parse(&issuer.public_spec()).unwrap();
        let credential = issuer.mint(&tenant("a"), NOW);
        assert_eq!(keyring.verify(&credential, NOW), Ok(tenant("a")));
        assert_eq!(
            keyring.verify(&credential, NOW + CREDENTIAL_LIFETIME_MS - 1),
            Ok(tenant("a"))
        );
        assert_eq!(
            keyring.verify(&credential, NOW + CREDENTIAL_LIFETIME_MS),
            Err(Denied::Expired)
        );
    }

    #[test]
    fn a_credential_claiming_a_long_life_is_refused() {
        let issuer = issuer("k1", 7);
        let keyring = Keyring::parse(&issuer.public_spec()).unwrap();
        let far = issuer.mint_until(&tenant("a"), NOW + MAX_CREDENTIAL_LIFETIME_MS + 1);
        assert_eq!(keyring.verify(&far, NOW), Err(Denied::TooLong));
        let edge = issuer.mint_until(&tenant("a"), NOW + MAX_CREDENTIAL_LIFETIME_MS);
        assert_eq!(keyring.verify(&edge, NOW), Ok(tenant("a")));
    }

    #[test]
    fn a_tampered_credential_is_refused() {
        let issuer = issuer("k1", 7);
        let keyring = Keyring::parse(&issuer.public_spec()).unwrap();
        let credential = issuer.mint(&tenant("a"), NOW);
        let other = credential.replace(tenant("a").as_str(), tenant("b").as_str());
        assert_eq!(keyring.verify(&other, NOW), Err(Denied::BadSignature));
        let later = credential.replace(
            &(NOW + CREDENTIAL_LIFETIME_MS).to_string(),
            &(NOW + CREDENTIAL_LIFETIME_MS + 1).to_string(),
        );
        assert_eq!(keyring.verify(&later, NOW), Err(Denied::BadSignature));
        let padded = credential.replace(
            &format!(".{}.", NOW + CREDENTIAL_LIFETIME_MS),
            &format!(".0{}.", NOW + CREDENTIAL_LIFETIME_MS),
        );
        assert_eq!(keyring.verify(&padded, NOW), Err(Denied::Malformed));
        for junk in ["", "nolap1", "Bearer x", &format!("{credential}.x")] {
            assert_eq!(keyring.verify(junk, NOW), Err(Denied::Malformed), "{junk}");
        }
    }

    #[test]
    fn a_key_outside_the_keyring_is_refused_and_rotation_overlaps() {
        let (old, new) = (issuer("old", 1), issuer("new", 2));
        let both = Keyring::parse(&format!(
            "# rotating\n{}\n\n{}\n",
            old.public_spec(),
            new.public_spec()
        ))
        .unwrap();
        let only_new = Keyring::parse(&new.public_spec()).unwrap();
        let (by_old, by_new) = (old.mint(&tenant("a"), NOW), new.mint(&tenant("a"), NOW));
        assert_eq!(both.verify(&by_old, NOW), Ok(tenant("a")));
        assert_eq!(both.verify(&by_new, NOW), Ok(tenant("a")));
        assert_eq!(only_new.verify(&by_old, NOW), Err(Denied::UnknownKey));
        assert_eq!(only_new.verify(&by_new, NOW), Ok(tenant("a")));
        // The same key id with another key does not verify the old credentials.
        let impostor = Keyring::parse(&issuer("old", 3).public_spec()).unwrap();
        assert_eq!(impostor.verify(&by_old, NOW), Err(Denied::BadSignature));
    }

    #[test]
    fn a_malformed_keyring_is_refused() {
        let good = issuer("k1", 7).public_spec();
        for (text, why) in [
            ("", "empty"),
            ("# only a comment\n", "no key"),
            (&format!("{good}\n{good}"), "repeated"),
            ("k1 abc", "no colon"),
            ("bad.kid:AAAA", "kid"),
            ("k1:AAAA", "short"),
        ] {
            assert!(Keyring::parse(text).is_err(), "{why}");
        }
        assert!(Issuer::parse("k1:AAAA").is_err());
        assert!(format!("{:?}", issuer("k1", 7)).contains("k1"));
    }
}
