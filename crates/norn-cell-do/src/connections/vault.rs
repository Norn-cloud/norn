//! The connection vault: per-cell envelope encryption of connector
//! credentials.
//!
//! VAULT-ENVELOPE-1: a credential is sealed with AES-256-GCM under the cell's
//! data key (DEK); the DEK is stored only wrapped by a host key-encryption key
//! (KEK, [`KeyWrap`]). The cell's storage holds ciphertext, the wrapping key's
//! id and non-secret status, never a plaintext credential or key.
//! VAULT-AAD-1: every ciphertext is bound by its associated data to the cell
//! scope and its row (the DEK to its version and wrapping key id, a
//! credential to its connection and revision), so a row copied to another
//! cell, connection or revision does not open.
//! VAULT-KEK-SCOPE-1: a KEK id is `<env>-<region>-v<N>`. A keyring holds the
//! keys of one environment and region only; it wraps with its current id and
//! unwraps with any id it lists, so rotation (and later per-tenant-group keys)
//! is configuration, not code.
//! VAULT-ROTATE-1: [`Vault::rewrap`] rewraps every DEK not under the current
//! KEK id. It never reads or rewrites a credential's ciphertext.
//! VAULT-KEK-LOAD-1: the KEK is read from host configuration by
//! [`LocalKeyring::from_vars`] only, in this module. No other module names
//! its variables (a test in this crate scans the sources), so Studio, the
//! read routes and the rest of the cell never hold it.
//! VAULT-NOLOG-1: [`Secret`] and the keyring print as `[redacted]`; nothing
//! here logs, and no error carries a credential or key byte.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use norn_cell::engine::{SqlEngine, SqlValue};

/// The current KEK's id, `<env>-<region>-v<N>` (for example `prod-eu-v2`).
pub const KEK_ID_VAR: &str = "NORN_VAULT_KEK_ID";
/// The current KEK: 32 bytes, standard base64.
pub const KEK_VAR: &str = "NORN_VAULT_KEK";
/// Other active KEKs, unwrap only: `id=base64,id=base64`, same environment
/// and region as the current id. Optional.
pub const KEKS_PREVIOUS_VAR: &str = "NORN_VAULT_KEKS_PREVIOUS";
/// The [`KEK_VAR`] value that selects the fixed development key
/// ([`DEV_KEK_ID`]); accepted only where the host allows it (`norn dev`).
pub const DEV_KEK_VALUE: &str = "dev-insecure";
/// The development key's id.
pub const DEV_KEK_ID: &str = "dev-local-v1";

const NONCE_BYTES: usize = 12;
const KEY_BYTES: usize = 32;

/// Why the vault refused. No variant carries secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// The host KEK configuration is malformed (names the variable).
    Config(String),
    /// A wrapped key or ciphertext did not open: wrong key, wrong binding or
    /// tampered bytes.
    Integrity,
    /// A row names a KEK id this keyring does not hold.
    UnknownKey(String),
    /// The host could not supply randomness.
    Entropy,
    /// The cell's storage failed.
    Storage(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(f, "vault configuration: {message}"),
            Self::Integrity => f.write_str("a sealed value did not open"),
            Self::UnknownKey(id) => write!(f, "no active key-encryption key with id {id}"),
            Self::Entropy => f.write_str("no randomness available"),
            Self::Storage(message) => write!(f, "vault storage: {message}"),
        }
    }
}

impl std::error::Error for VaultError {}

fn storage(error: impl fmt::Display) -> VaultError {
    VaultError::Storage(error.to_string())
}

/// Bytes that must never be printed; overwritten when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    /// Wrap `bytes`.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the one call that sends them.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// The bytes as UTF-8 text, when they are.
    #[must_use]
    pub fn expose_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

impl From<String> for Secret {
    fn from(text: String) -> Self {
        Self(text.into_bytes())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Fill `buf` with randomness from the host.
fn random(buf: &mut [u8]) -> Result<(), VaultError> {
    getrandom::fill(buf).map_err(|_error| VaultError::Entropy)
}

/// A KEK id, `<env>-<region>-v<N>`: lowercase letters and digits in the
/// environment and region, a positive version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyId {
    /// The deployment environment, e.g. `prod`.
    pub env: String,
    /// The region, e.g. `eu`.
    pub region: String,
    /// The key version.
    pub version: u32,
}

impl KeyId {
    /// Parse `<env>-<region>-v<N>`.
    ///
    /// # Errors
    /// Any other shape.
    pub fn parse(id: &str) -> Result<Self, VaultError> {
        let bad = || {
            VaultError::Config(format!(
                "key id {id:?} is not <env>-<region>-v<N> (lowercase letters and digits)"
            ))
        };
        let mut parts = id.split('-');
        let (Some(env), Some(region), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let word = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        };
        let version = version
            .strip_prefix('v')
            .filter(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u32>().ok())
            .filter(|version| *version > 0)
            .ok_or_else(bad)?;
        if !word(env) || !word(region) {
            return Err(bad());
        }
        Ok(Self {
            env: env.to_owned(),
            region: region.to_owned(),
            version,
        })
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-v{}", self.env, self.region, self.version)
    }
}

/// A DEK wrapped by a KEK: what the cell stores.
#[derive(Clone, PartialEq, Eq)]
pub struct Wrapped {
    /// The KEK id that wrapped it.
    pub key_id: String,
    /// The AES-GCM nonce.
    pub nonce: Vec<u8>,
    /// The wrapped key and its tag.
    pub ciphertext: Vec<u8>,
}

/// Wraps and unwraps data keys. One implementation today ([`LocalKeyring`]);
/// a KMS-backed one implements the same three calls.
pub trait KeyWrap: Send + Sync {
    /// The id new wraps are made under.
    fn current(&self) -> &str;

    /// Wrap `dek` under the current KEK, bound to `aad`.
    ///
    /// # Errors
    /// No randomness, or a cipher failure.
    fn wrap(&self, dek: &Secret, aad: &[u8]) -> Result<Wrapped, VaultError>;

    /// Unwrap with the KEK `wrapped` names, if this keyring lists it.
    ///
    /// # Errors
    /// An id not listed, or bytes that do not open under `aad`.
    fn unwrap(&self, wrapped: &Wrapped, aad: &[u8]) -> Result<Secret, VaultError>;
}

/// KEKs held in host memory, read from host configuration (the secrets
/// manager's environment). One environment and region per keyring.
pub struct LocalKeyring {
    current: String,
    keys: BTreeMap<String, [u8; KEY_BYTES]>,
}

impl fmt::Debug for LocalKeyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalKeyring")
            .field("current", &self.current)
            .field("ids", &self.keys.keys().collect::<Vec<_>>())
            .field("keys", &"[redacted]")
            .finish()
    }
}

impl Drop for LocalKeyring {
    fn drop(&mut self) {
        for key in self.keys.values_mut() {
            key.fill(0);
        }
    }
}

fn decode_key(variable: &str, value: &str) -> Result<[u8; KEY_BYTES], VaultError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|_error| VaultError::Config(format!("{variable} is not standard base64")))?;
    <[u8; KEY_BYTES]>::try_from(bytes.as_slice())
        .map_err(|_error| VaultError::Config(format!("{variable} is not {KEY_BYTES} bytes")))
}

impl LocalKeyring {
    /// A keyring wrapping under `current`, unwrapping with any of `keys`.
    ///
    /// # Errors
    /// `current` not among `keys`, a malformed id, or ids of more than one
    /// environment and region (VAULT-KEK-SCOPE-1).
    pub fn new(current: &str, keys: BTreeMap<String, [u8; KEY_BYTES]>) -> Result<Self, VaultError> {
        let scope = KeyId::parse(current)?;
        if !keys.contains_key(current) {
            return Err(VaultError::Config(format!(
                "the current key id {current} has no key"
            )));
        }
        for id in keys.keys() {
            let other = KeyId::parse(id)?;
            if (&other.env, &other.region) != (&scope.env, &scope.region) {
                return Err(VaultError::Config(format!(
                    "key id {id} is not of environment {} and region {}",
                    scope.env, scope.region
                )));
            }
        }
        Ok(Self {
            current: current.to_owned(),
            keys,
        })
    }

    /// The fixed development keyring ([`DEV_KEK_ID`]). Its key is public:
    /// for `norn dev` and tests only.
    #[must_use]
    pub fn dev() -> Self {
        let mut key = [0_u8; KEY_BYTES];
        for (byte, value) in key.iter_mut().zip(b"norn-dev-insecure-vault-kek-v1!!") {
            *byte = *value;
        }
        Self {
            current: DEV_KEK_ID.to_owned(),
            keys: BTreeMap::from([(DEV_KEK_ID.to_owned(), key)]),
        }
    }

    /// The keyring host configuration declares ([`KEK_ID_VAR`], [`KEK_VAR`],
    /// [`KEKS_PREVIOUS_VAR`]); `None` when no KEK is configured, so the host
    /// serves no connection. `allow_dev` admits [`DEV_KEK_VALUE`].
    ///
    /// # Errors
    /// A malformed variable (named, never its value).
    pub fn from_vars(
        vars: &BTreeMap<String, String>,
        allow_dev: bool,
    ) -> Result<Option<Self>, VaultError> {
        let Some(value) = vars.get(KEK_VAR).filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        if value == DEV_KEK_VALUE {
            return if allow_dev {
                Ok(Some(Self::dev()))
            } else {
                Err(VaultError::Config(format!(
                    "{KEK_VAR} selects the development key, which this host refuses"
                )))
            };
        }
        let current = vars
            .get(KEK_ID_VAR)
            .map(|id| id.trim().to_owned())
            .ok_or_else(|| VaultError::Config(format!("{KEK_VAR} is set without {KEK_ID_VAR}")))?;
        let mut keys = BTreeMap::from([(current.clone(), decode_key(KEK_VAR, value)?)]);
        for entry in vars
            .get(KEKS_PREVIOUS_VAR)
            .map(String::as_str)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let (id, key) = entry.split_once('=').ok_or_else(|| {
                VaultError::Config(format!("{KEKS_PREVIOUS_VAR} entries are id=base64"))
            })?;
            if keys
                .insert(id.trim().to_owned(), decode_key(KEKS_PREVIOUS_VAR, key)?)
                .is_some()
            {
                return Err(VaultError::Config(format!(
                    "{KEKS_PREVIOUS_VAR} repeats key id {id}"
                )));
            }
        }
        Self::new(&current, keys).map(Some)
    }

    /// The ids this keyring unwraps with.
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.keys.keys().map(String::as_str).collect()
    }
}

fn cipher(key: &[u8]) -> Result<Aes256Gcm, VaultError> {
    Aes256Gcm::new_from_slice(key).map_err(|_error| VaultError::Integrity)
}

fn seal_with(key: &[u8], aad: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), VaultError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    random(&mut nonce)?;
    let ciphertext = cipher(key)?
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_error| VaultError::Integrity)?;
    Ok((nonce.to_vec(), ciphertext))
}

fn open_with(
    key: &[u8],
    aad: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Secret, VaultError> {
    if nonce.len() != NONCE_BYTES {
        return Err(VaultError::Integrity);
    }
    cipher(key)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map(Secret::new)
        .map_err(|_error| VaultError::Integrity)
}

impl KeyWrap for LocalKeyring {
    fn current(&self) -> &str {
        &self.current
    }

    fn wrap(&self, dek: &Secret, aad: &[u8]) -> Result<Wrapped, VaultError> {
        let key = self
            .keys
            .get(&self.current)
            .ok_or_else(|| VaultError::UnknownKey(self.current.clone()))?;
        let aad = [aad, b"|", self.current.as_bytes()].concat();
        let (nonce, ciphertext) = seal_with(key, &aad, dek.expose())?;
        Ok(Wrapped {
            key_id: self.current.clone(),
            nonce,
            ciphertext,
        })
    }

    fn unwrap(&self, wrapped: &Wrapped, aad: &[u8]) -> Result<Secret, VaultError> {
        let key = self
            .keys
            .get(&wrapped.key_id)
            .ok_or_else(|| VaultError::UnknownKey(wrapped.key_id.clone()))?;
        let aad = [aad, b"|", wrapped.key_id.as_bytes()].concat();
        open_with(key, &aad, &wrapped.nonce, &wrapped.ciphertext)
    }
}

/// The vault's tables in the cell's own storage.
const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS "_norn_vault_dek" (
  "version" INTEGER PRIMARY KEY,
  "key_id" TEXT NOT NULL,
  "nonce" BLOB NOT NULL,
  "wrapped" BLOB NOT NULL,
  "created_ms" INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS "_norn_vault_sealed" (
  "slot" TEXT PRIMARY KEY,
  "revision" INTEGER NOT NULL,
  "dek_version" INTEGER NOT NULL,
  "nonce" BLOB NOT NULL,
  "ciphertext" BLOB NOT NULL
);
"#;

/// A sealed value as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The DEK version that sealed it.
    pub dek_version: i64,
    /// The AES-GCM nonce.
    pub nonce: Vec<u8>,
    /// The ciphertext and tag.
    pub ciphertext: Vec<u8>,
}

/// One cell's vault over its storage engine.
pub struct Vault<E: SqlEngine> {
    engine: E,
    scope: String,
    keys: Arc<dyn KeyWrap>,
}

fn int(value: &SqlValue) -> Result<i64, VaultError> {
    if let SqlValue::Integer(value) = value {
        Ok(*value)
    } else {
        Err(storage("expected an integer"))
    }
}

fn blob(value: &SqlValue) -> Result<Vec<u8>, VaultError> {
    if let SqlValue::Blob(value) = value {
        Ok(value.clone())
    } else {
        Err(storage("expected bytes"))
    }
}

fn text(value: &SqlValue) -> Result<String, VaultError> {
    if let SqlValue::Text(value) = value {
        Ok(value.clone())
    } else {
        Err(storage("expected text"))
    }
}

impl<E: SqlEngine> Vault<E> {
    /// The vault of the cell `scope` (its scope key), over `engine` (the
    /// cell's storage), wrapping with `keys`.
    ///
    /// # Errors
    /// Storage failures creating its tables.
    pub fn open(engine: E, scope: &str, keys: Arc<dyn KeyWrap>) -> Result<Self, VaultError> {
        engine.execute_batch(DDL).map_err(storage)?;
        Ok(Self {
            engine,
            scope: scope.to_owned(),
            keys,
        })
    }

    /// The cell's storage, for the connection tables beside the vault's.
    pub(crate) const fn engine(&self) -> &E {
        &self.engine
    }

    fn dek_aad(&self, version: i64) -> Vec<u8> {
        format!("norn.vault.dek/1|{}|{version}", self.scope).into_bytes()
    }

    fn sealed_aad(&self, slot: &str, revision: i64) -> Vec<u8> {
        format!("norn.vault.sealed/1|{}|{slot}|{revision}", self.scope).into_bytes()
    }

    fn wrapped_dek(&self, version: Option<i64>) -> Result<Option<(i64, Wrapped)>, VaultError> {
        let rows = match version {
            Some(version) => self.engine.query(
                r#"SELECT "version", "key_id", "nonce", "wrapped" FROM "_norn_vault_dek" WHERE "version" = ?1"#,
                &[SqlValue::Integer(version)],
            ),
            None => self.engine.query(
                r#"SELECT "version", "key_id", "nonce", "wrapped" FROM "_norn_vault_dek" ORDER BY "version" DESC LIMIT 1"#,
                &[],
            ),
        }
        .map_err(storage)?;
        rows.first()
            .map(|row| {
                Ok((
                    int(row.first().ok_or_else(|| storage("dek row"))?)?,
                    Wrapped {
                        key_id: text(row.get(1).ok_or_else(|| storage("dek row"))?)?,
                        nonce: blob(row.get(2).ok_or_else(|| storage("dek row"))?)?,
                        ciphertext: blob(row.get(3).ok_or_else(|| storage("dek row"))?)?,
                    },
                ))
            })
            .transpose()
    }

    /// The cell's current DEK, created and wrapped on first use.
    fn current_dek(&self, now_ms: u64) -> Result<(i64, Secret), VaultError> {
        if let Some((version, wrapped)) = self.wrapped_dek(None)? {
            return Ok((version, self.keys.unwrap(&wrapped, &self.dek_aad(version))?));
        }
        let mut dek = vec![0_u8; KEY_BYTES];
        random(&mut dek)?;
        let dek = Secret::new(dek);
        let version = 1;
        let wrapped = self.keys.wrap(&dek, &self.dek_aad(version))?;
        self.engine
            .execute(
                r#"INSERT INTO "_norn_vault_dek" ("version", "key_id", "nonce", "wrapped", "created_ms") VALUES (?1, ?2, ?3, ?4, ?5)"#,
                &[
                    SqlValue::Integer(version),
                    SqlValue::Text(wrapped.key_id),
                    SqlValue::Blob(wrapped.nonce),
                    SqlValue::Blob(wrapped.ciphertext),
                    SqlValue::Integer(i64::try_from(now_ms).unwrap_or(i64::MAX)),
                ],
            )
            .map_err(storage)?;
        Ok((version, dek))
    }

    /// Seal `plaintext` for `slot` at `revision` under the current DEK.
    ///
    /// # Errors
    /// Key, randomness or storage failures.
    pub fn seal(
        &self,
        slot: &str,
        revision: i64,
        plaintext: &[u8],
        now_ms: u64,
    ) -> Result<Sealed, VaultError> {
        let (dek_version, dek) = self.current_dek(now_ms)?;
        let (nonce, ciphertext) =
            seal_with(dek.expose(), &self.sealed_aad(slot, revision), plaintext)?;
        Ok(Sealed {
            dek_version,
            nonce,
            ciphertext,
        })
    }

    /// Open what [`Self::seal`] sealed for `slot` at `revision`.
    ///
    /// # Errors
    /// A DEK the keyring cannot unwrap, or a value bound to anything else.
    pub fn open_sealed(
        &self,
        slot: &str,
        revision: i64,
        sealed: &Sealed,
    ) -> Result<Secret, VaultError> {
        let (version, wrapped) = self
            .wrapped_dek(Some(sealed.dek_version))?
            .ok_or(VaultError::Integrity)?;
        let dek = self.keys.unwrap(&wrapped, &self.dek_aad(version))?;
        open_with(
            dek.expose(),
            &self.sealed_aad(slot, revision),
            &sealed.nonce,
            &sealed.ciphertext,
        )
    }

    /// Store `plaintext` sealed in `slot` at `revision`, replacing it.
    ///
    /// # Errors
    /// As [`Self::seal`].
    pub fn put(
        &self,
        slot: &str,
        revision: i64,
        plaintext: &[u8],
        now_ms: u64,
    ) -> Result<(), VaultError> {
        let sealed = self.seal(slot, revision, plaintext, now_ms)?;
        self.engine
            .execute(
                r#"INSERT OR REPLACE INTO "_norn_vault_sealed" ("slot", "revision", "dek_version", "nonce", "ciphertext") VALUES (?1, ?2, ?3, ?4, ?5)"#,
                &[
                    SqlValue::Text(slot.to_owned()),
                    SqlValue::Integer(revision),
                    SqlValue::Integer(sealed.dek_version),
                    SqlValue::Blob(sealed.nonce),
                    SqlValue::Blob(sealed.ciphertext),
                ],
            )
            .map_err(storage)
    }

    /// The sealed row of `slot`, as stored.
    ///
    /// # Errors
    /// Storage failures.
    pub fn sealed(&self, slot: &str) -> Result<Option<(i64, Sealed)>, VaultError> {
        let rows = self
            .engine
            .query(
                r#"SELECT "revision", "dek_version", "nonce", "ciphertext" FROM "_norn_vault_sealed" WHERE "slot" = ?1"#,
                &[SqlValue::Text(slot.to_owned())],
            )
            .map_err(storage)?;
        rows.first()
            .map(|row| {
                let cell = |index: usize| row.get(index).ok_or_else(|| storage("sealed row"));
                Ok((
                    int(cell(0)?)?,
                    Sealed {
                        dek_version: int(cell(1)?)?,
                        nonce: blob(cell(2)?)?,
                        ciphertext: blob(cell(3)?)?,
                    },
                ))
            })
            .transpose()
    }

    /// Open `slot`'s stored value.
    ///
    /// # Errors
    /// As [`Self::open_sealed`].
    pub fn get(&self, slot: &str) -> Result<Option<(i64, Secret)>, VaultError> {
        self.sealed(slot)?
            .map(|(revision, sealed)| Ok((revision, self.open_sealed(slot, revision, &sealed)?)))
            .transpose()
    }

    /// Delete `slot`'s stored value.
    ///
    /// # Errors
    /// Storage failures.
    pub fn delete(&self, slot: &str) -> Result<(), VaultError> {
        self.engine
            .execute(
                r#"DELETE FROM "_norn_vault_sealed" WHERE "slot" = ?1"#,
                &[SqlValue::Text(slot.to_owned())],
            )
            .map_err(storage)
    }

    /// VAULT-ROTATE-1: rewrap every DEK not under the current KEK id; the
    /// number rewrapped. Credential ciphertext is not read or written.
    ///
    /// # Errors
    /// A DEK under an id the keyring no longer lists, or storage failures.
    pub fn rewrap(&self) -> Result<usize, VaultError> {
        let rows = self
            .engine
            .query(
                r#"SELECT "version" FROM "_norn_vault_dek" WHERE "key_id" <> ?1 ORDER BY "version""#,
                &[SqlValue::Text(self.keys.current().to_owned())],
            )
            .map_err(storage)?;
        let mut rewrapped = 0;
        for row in rows {
            let version = int(row.first().ok_or_else(|| storage("dek row"))?)?;
            let (_, wrapped) = self
                .wrapped_dek(Some(version))?
                .ok_or_else(|| storage("dek row vanished"))?;
            let aad = self.dek_aad(version);
            let dek = self.keys.unwrap(&wrapped, &aad)?;
            let next = self.keys.wrap(&dek, &aad)?;
            self.engine
                .execute(
                    r#"UPDATE "_norn_vault_dek" SET "key_id" = ?1, "nonce" = ?2, "wrapped" = ?3 WHERE "version" = ?4"#,
                    &[
                        SqlValue::Text(next.key_id),
                        SqlValue::Blob(next.nonce),
                        SqlValue::Blob(next.ciphertext),
                        SqlValue::Integer(version),
                    ],
                )
                .map_err(storage)?;
            rewrapped = rewrapped.saturating_add(1);
        }
        Ok(rewrapped)
    }

    /// The KEK id each DEK version is wrapped under.
    ///
    /// # Errors
    /// Storage failures.
    pub fn dek_key_ids(&self) -> Result<Vec<(i64, String)>, VaultError> {
        self.engine
            .query(
                r#"SELECT "version", "key_id" FROM "_norn_vault_dek" ORDER BY "version""#,
                &[],
            )
            .map_err(storage)?
            .iter()
            .map(|row| {
                Ok((
                    int(row.first().ok_or_else(|| storage("dek row"))?)?,
                    text(row.get(1).ok_or_else(|| storage("dek row"))?)?,
                ))
            })
            .collect()
    }
}
