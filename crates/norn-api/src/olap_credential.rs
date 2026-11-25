//! OLAP-CRED-1 (#1493): the per-cell credential a cell presents to
//! `norn-olapd`, the tenant analytics engine.
//!
//! A credential names exactly one tenant (the cell's own database), an
//! expiry and the SHA-256 of the exact body its call sends
//! ([`body_digest`]), signed with Ed25519 by the host's signing key:
//!
//! ```text
//! nolap2.<kid>.<tenant>.<expires_ms>.<body digest, hex>.<signature, base64url>
//! ```
//!
//! The signature also covers the one operation the call performs, which
//! the engine reads from the request's path; it does not travel in the
//! credential. So a credential captured from one call cannot be presented
//! for another operation (a read's credential never authorizes a write) or
//! with another body. #1747: because the body's digest travels signed in the
//! credential, the engine verifies the whole credential from the request's
//! head ([`Keyring::verify_head`]) before it reads or allocates a single body
//! byte, then checks the body it reads against the digest it already
//! verified ([`Verified::body`]). What remains is replaying the same
//! request, byte for byte, within its life; the transport to the engine must
//! therefore be protected (TLS or a private network), as the deploy docs
//! require.
//!
//! The cell mints one for every engine call, for its own tenant only, with
//! [`CREDENTIAL_LIFETIME_MS`] to live. The engine holds only public keys
//! (a [`Keyring`]), so it cannot mint, and it serves a call only when the
//! credential verifies for that operation and body, has not expired, and
//! names the tenant the request reads or writes. Key rotation is an overlap: add the new public key to
//! the engine's keyring, move the host to the new signing key, and remove
//! the old public key once [`MAX_CREDENTIAL_LIFETIME_MS`] has passed.
//! There is no shared token and no fallback to one.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use norn_cell::analytics::TenantId;
use sha2::{Digest as _, Sha256};

/// The credential format's prefix.
const PREFIX: &str = "nolap2";
/// Domain separation for the signed bytes.
const DOMAIN: &[u8] = b"norn-olapd/credential/v2";
/// The length of a [`body_digest`]: lowercase hex SHA-256.
const DIGEST_HEX: usize = 64;
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
    /// Valid, but the request's body names no tenant.
    Unscoped,
    /// #1747: valid for the body digest it declares, but the body received
    /// has another digest.
    BodyMismatch,
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
            Self::Unscoped => "unscoped",
            Self::BodyMismatch => "body_mismatch",
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

/// The digest a credential binds a call's body by: lowercase hex SHA-256
/// of the exact bytes sent.
#[must_use]
pub fn body_digest(body: &[u8]) -> String {
    Sha256::digest(body)
        .iter()
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _infallible = write!(out, "{byte:02x}");
            out
        })
}

/// What a credential's signature covers. Every part is length-prefixed, so
/// no choice of operation or body digest can shift the boundaries.
fn signed_bytes(kid: &str, tenant: &str, expires_ms: u64, op: &str, digest: &str) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    for part in [kid, tenant, &expires_ms.to_string(), op, digest] {
        let length = u64::try_from(part.len()).unwrap_or(u64::MAX);
        bytes.extend_from_slice(&length.to_be_bytes());
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
    /// (`NORN_OLAP_SIGNING_KEY`).
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

    /// A credential for `tenant` alone, for the call `op` with exactly
    /// `body`, living [`CREDENTIAL_LIFETIME_MS`] from `now_ms`.
    #[must_use]
    pub fn mint(&self, tenant: &TenantId, op: &str, body: &[u8], now_ms: u64) -> String {
        self.mint_until(
            tenant,
            op,
            body,
            now_ms.saturating_add(CREDENTIAL_LIFETIME_MS),
        )
    }

    /// A credential for `tenant` alone, for the call `op` with exactly
    /// `body`, that expires at `expires_ms`.
    #[must_use]
    pub fn mint_until(&self, tenant: &TenantId, op: &str, body: &[u8], expires_ms: u64) -> String {
        let digest = body_digest(body);
        let signature = self.key.sign(&signed_bytes(
            &self.kid,
            tenant.as_str(),
            expires_ms,
            op,
            &digest,
        ));
        format!(
            "{PREFIX}.{}.{}.{expires_ms}.{digest}.{}",
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
        for (number, line) in (1_usize..).zip(text.lines()) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (kid, bytes) = key_spec(line).map_err(|e| format!("line {number}: {e}"))?;
            let key = VerifyingKey::from_bytes(&bytes)
                .map_err(|_e| format!("line {number}: not an Ed25519 public key"))?;
            if keys.insert(kid.clone(), key).is_some() {
                return Err(format!("line {number}: key id {kid} is repeated"));
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

    /// #1747: the tenant `credential` was minted for, and the body digest
    /// it declares, when it verifies under a key of this keyring for the
    /// call `op` and is live at `now_ms`. This needs only the request's
    /// head: the declared digest is part of what was signed, so a forged,
    /// rotated-out, expired or other-operation credential is refused before
    /// any body is read. The body itself must then match ([`Verified::body`]).
    ///
    /// # Errors
    /// Why the credential is refused.
    pub fn verify_head(&self, credential: &str, op: &str, now_ms: u64) -> Result<Verified, Denied> {
        let claims = Claims::parse(credential)?;
        let key = self.keys.get(claims.kid).ok_or(Denied::UnknownKey)?;
        claims.live_at(now_ms)?;
        key.verify_strict(
            &signed_bytes(
                claims.kid,
                claims.tenant.as_str(),
                claims.expires_ms,
                op,
                claims.digest,
            ),
            &Signature::from_bytes(&claims.signature),
        )
        .map_err(|_e| Denied::BadSignature)?;
        Ok(Verified {
            tenant: claims.tenant,
            digest: claims.digest.to_owned(),
        })
    }

    /// The tenant `credential` was minted for, when it verifies under a key
    /// of this keyring for the call `op` whose body has the [`body_digest`]
    /// `digest`, and is live at `now_ms`: [`Keyring::verify_head`] and
    /// [`Verified::digest_is`]. A credential minted for another operation
    /// does not verify ([`Denied::BadSignature`]); one minted for another
    /// body is [`Denied::BodyMismatch`].
    ///
    /// # Errors
    /// Why the credential is refused.
    pub fn verify(
        &self,
        credential: &str,
        op: &str,
        digest: &str,
        now_ms: u64,
    ) -> Result<TenantId, Denied> {
        self.verify_head(credential, op, now_ms)?.digest_is(digest)
    }
}

/// #1747: a credential verified from a request's head: its tenant, and the
/// body digest its signature covers. It admits the call only once the body
/// received is shown to have that digest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a verified head admits nothing until its body matches"]
pub struct Verified {
    tenant: TenantId,
    digest: String,
}

impl Verified {
    /// The credential's tenant, when `body` is the exact body it was minted for.
    ///
    /// # Errors
    /// [`Denied::BodyMismatch`] for any other body.
    pub fn body(self, body: &[u8]) -> Result<TenantId, Denied> {
        let digest = body_digest(body);
        self.digest_is(&digest)
    }

    /// The credential's tenant, when `digest` is the body digest it was minted for.
    ///
    /// # Errors
    /// [`Denied::BodyMismatch`] for any other digest.
    pub fn digest_is(self, digest: &str) -> Result<TenantId, Denied> {
        if self.digest == digest {
            Ok(self.tenant)
        } else {
            Err(Denied::BodyMismatch)
        }
    }
}

/// The parts of a credential, parsed but not verified.
struct Claims<'a> {
    kid: &'a str,
    tenant: TenantId,
    expires_ms: u64,
    digest: &'a str,
    signature: [u8; 64],
}

impl<'a> Claims<'a> {
    fn parse(credential: &'a str) -> Result<Self, Denied> {
        let mut parts = credential.split('.');
        let (
            Some(PREFIX),
            Some(kid),
            Some(tenant),
            Some(expires),
            Some(digest),
            Some(signature),
            None,
        ) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        )
        else {
            return Err(Denied::Malformed);
        };
        if digest.len() != DIGEST_HEX
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Denied::Malformed);
        }
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
        Ok(Self {
            kid,
            tenant,
            expires_ms,
            digest,
            signature,
        })
    }

    /// Live at `now_ms`, and claiming no longer a life than the engine accepts.
    fn live_at(&self, now_ms: u64) -> Result<(), Denied> {
        if self.expires_ms <= now_ms {
            return Err(Denied::Expired);
        }
        if self.expires_ms.saturating_sub(now_ms) > MAX_CREDENTIAL_LIFETIME_MS {
            return Err(Denied::TooLong);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "credential fixtures must build before verification assertions"
)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;
    const OP: &str = "frontier";
    const BODY: &[u8] = br#"{"tenant":"a"}"#;

    fn digest() -> String {
        body_digest(BODY)
    }

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
        let credential = issuer.mint(&tenant("a"), OP, BODY, NOW);
        assert_eq!(
            keyring.verify(&credential, OP, &digest(), NOW),
            Ok(tenant("a"))
        );
        assert_eq!(
            keyring.verify(&credential, OP, &digest(), NOW + CREDENTIAL_LIFETIME_MS - 1),
            Ok(tenant("a"))
        );
        assert_eq!(
            keyring.verify(&credential, OP, &digest(), NOW + CREDENTIAL_LIFETIME_MS),
            Err(Denied::Expired)
        );
    }

    /// OLAP-03: a credential minted for one call verifies for that operation
    /// and exact body only; a read's credential never authorizes a write.
    #[test]
    fn a_credential_is_bound_to_its_operation_and_body() {
        let issuer = issuer("k1", 7);
        let keyring = Keyring::parse(&issuer.public_spec()).unwrap();
        let credential = issuer.mint(&tenant("a"), OP, BODY, NOW);
        assert_eq!(
            keyring.verify(&credential, OP, &digest(), NOW),
            Ok(tenant("a"))
        );
        for op in ["sync", "load", "export", "frontierx", ""] {
            assert_eq!(
                keyring.verify(&credential, op, &digest(), NOW),
                Err(Denied::BadSignature),
                "{op}"
            );
        }
        let other = body_digest(br#"{"tenant":"a","x":1}"#);
        assert_eq!(
            keyring.verify(&credential, OP, &other, NOW),
            Err(Denied::BodyMismatch)
        );
        assert_eq!(
            body_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// #1747: the head alone verifies the signature over the declared body
    /// digest; the body read afterwards must have exactly that digest.
    #[test]
    fn the_head_verifies_before_the_body_and_the_body_must_match() {
        let host = issuer("k1", 7);
        let keyring = Keyring::parse(&host.public_spec()).unwrap();
        let credential = host.mint(&tenant("a"), OP, BODY, NOW);
        let verified = keyring.verify_head(&credential, OP, NOW).unwrap();
        assert_eq!(verified.clone().body(BODY), Ok(tenant("a")));
        assert_eq!(
            verified.clone().body(br#"{"tenant":"b"}"#),
            Err(Denied::BodyMismatch)
        );
        assert_eq!(verified.body(b""), Err(Denied::BodyMismatch));
        // A declared digest swapped for another body's breaks the signature.
        let swapped = credential.replace(&digest(), &body_digest(b"{}"));
        assert_eq!(
            keyring.verify_head(&swapped, OP, NOW),
            Err(Denied::BadSignature)
        );
        // A forged signature under a key id the keyring holds is refused
        // from the head alone, as are an unknown key and an expired one.
        let forged = issuer("k1", 9).mint(&tenant("a"), OP, BODY, NOW);
        assert_eq!(
            keyring.verify_head(&forged, OP, NOW),
            Err(Denied::BadSignature)
        );
        let unknown = issuer("k2", 7).mint(&tenant("a"), OP, BODY, NOW);
        assert_eq!(
            keyring.verify_head(&unknown, OP, NOW),
            Err(Denied::UnknownKey)
        );
        assert_eq!(
            keyring.verify_head(&credential, OP, NOW + CREDENTIAL_LIFETIME_MS),
            Err(Denied::Expired)
        );
        // A digest that is not lowercase hex SHA-256 is not a credential.
        let declared = digest();
        for bent in [
            declared.to_uppercase(),
            declared.get(1..).unwrap().to_owned(),
            String::new(),
            format!("{}g", declared.get(1..).unwrap()),
        ] {
            let bent_credential = credential.replace(&declared, &bent);
            assert_eq!(
                keyring.verify_head(&bent_credential, OP, NOW),
                Err(Denied::Malformed),
                "{bent}"
            );
        }
    }

    #[test]
    fn a_credential_claiming_a_long_life_is_refused() {
        let issuer = issuer("k1", 7);
        let keyring = Keyring::parse(&issuer.public_spec()).unwrap();
        let far = issuer.mint_until(&tenant("a"), OP, BODY, NOW + MAX_CREDENTIAL_LIFETIME_MS + 1);
        assert_eq!(
            keyring.verify(&far, OP, &digest(), NOW),
            Err(Denied::TooLong)
        );
        let edge = issuer.mint_until(&tenant("a"), OP, BODY, NOW + MAX_CREDENTIAL_LIFETIME_MS);
        assert_eq!(keyring.verify(&edge, OP, &digest(), NOW), Ok(tenant("a")));
    }

    #[test]
    fn a_tampered_credential_is_refused() {
        let issuer = issuer("k1", 7);
        let keyring = Keyring::parse(&issuer.public_spec()).unwrap();
        let credential = issuer.mint(&tenant("a"), OP, BODY, NOW);
        let other = credential.replace(tenant("a").as_str(), tenant("b").as_str());
        assert_eq!(
            keyring.verify(&other, OP, &digest(), NOW),
            Err(Denied::BadSignature)
        );
        let later = credential.replace(
            &(NOW + CREDENTIAL_LIFETIME_MS).to_string(),
            &(NOW + CREDENTIAL_LIFETIME_MS + 1).to_string(),
        );
        assert_eq!(
            keyring.verify(&later, OP, &digest(), NOW),
            Err(Denied::BadSignature)
        );
        let padded = credential.replace(
            &format!(".{}.", NOW + CREDENTIAL_LIFETIME_MS),
            &format!(".0{}.", NOW + CREDENTIAL_LIFETIME_MS),
        );
        assert_eq!(
            keyring.verify(&padded, OP, &digest(), NOW),
            Err(Denied::Malformed)
        );
        // The retired `nolap1` format carried no digest: it is not one of these.
        let retired = credential.replacen("nolap2", "nolap1", 1);
        for junk in [
            "",
            "nolap2",
            "Bearer x",
            &format!("{credential}.x"),
            &retired,
        ] {
            assert_eq!(
                keyring.verify(junk, OP, &digest(), NOW),
                Err(Denied::Malformed),
                "{junk}"
            );
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
        let (by_old, by_new) = (
            old.mint(&tenant("a"), OP, BODY, NOW),
            new.mint(&tenant("a"), OP, BODY, NOW),
        );
        assert_eq!(both.verify(&by_old, OP, &digest(), NOW), Ok(tenant("a")));
        assert_eq!(both.verify(&by_new, OP, &digest(), NOW), Ok(tenant("a")));
        assert_eq!(
            only_new.verify(&by_old, OP, &digest(), NOW),
            Err(Denied::UnknownKey)
        );
        assert_eq!(
            only_new.verify(&by_new, OP, &digest(), NOW),
            Ok(tenant("a"))
        );
        // The same key id with another key does not verify the old credentials.
        let impostor = Keyring::parse(&issuer("old", 3).public_spec()).unwrap();
        assert_eq!(
            impostor.verify(&by_old, OP, &digest(), NOW),
            Err(Denied::BadSignature)
        );
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
        assert_eq!(Denied::BodyMismatch.label(), "body_mismatch");
    }
}
