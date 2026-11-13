//! PRIVACY-1: marked properties leave the server already projected.
//!
//! A property whose effective markings are non-empty never reaches a document
//! as its stored value. Each marking declares its projection in the ontology
//! ([`norn_ir::MaskPolicy`]); the strongest one among a property's markings
//! applies:
//!
//! - `pseudonym`: a stable keyed hash, `<subject> #<10 base32 digits>`. It is
//!   HMAC-SHA256 under a key derived from the host's lineage key and the
//!   cell's scope, over the subject and the value's canonical JSON. Equal
//!   values keep equal pseudonyms within a cell (joins, counts, filters and
//!   sorts still work on them), two cells never share them, and they cannot
//!   be reversed without the key. A host without a key redacts instead.
//! - `redacted`: `•••`; `null`: JSON null; `last-n`: `•••` and the final
//!   characters (a value no longer than them is fully redacted).
//! - `refuse`: the property is left out and listed in `withheldProperties`.
//!
//! A marking the cell's catalog does not declare, or a row whose marking
//! snapshot is unavailable, fails closed: the property (or every property)
//! is refused. The document says how each property was projected and whether
//! the caller may reveal it; the reveal itself is a governed action with a
//! ledger receipt ([`reveal`]). Nothing here trusts the client.

use std::collections::BTreeMap;
use std::sync::Arc;

use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::{MaskPolicy, MarkingIr, RuntimeValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::{host::DevHost, refusal::Refusal, scope::CellScope};

/// The text a redacted value is replaced with.
pub const REDACTED_TEXT: &str = "•••";

/// How a marked property was projected for this caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NornMaskMode {
    /// A stable pseudonym replaced the value.
    Pseudonym,
    /// `•••` replaced the value.
    Redacted,
    /// JSON null replaced the value.
    Null,
    /// Only the final characters are shown.
    LastN,
    /// The property was left out of `values`.
    Refuse,
}

/// One marked property of a document: how it was projected and whether the
/// caller may reveal it (`POST /v1/objects/{type}/{id}:reveal`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornMaskedProperty {
    /// The projection applied.
    pub mode: NornMaskMode,
    /// The property's effective markings.
    pub markings: Vec<String>,
    /// The caller's policy allows `norn.reveal` for every one of them. The
    /// document still never carries the value.
    pub revealable: bool,
}

/// The key a cell's pseudonyms are computed under.
#[derive(Clone)]
pub struct PseudonymKey([u8; 32]);

impl std::fmt::Debug for PseudonymKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PseudonymKey(..)")
    }
}

/// The shortest secret a pseudonym key is derived from, in bytes.
pub const MIN_PSEUDONYM_SECRET_BYTES: usize = 32;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

impl PseudonymKey {
    /// The key of the cell at `scope`, derived from the host's secret, or
    /// `None` for a secret shorter than [`MIN_PSEUDONYM_SECRET_BYTES`].
    #[must_use]
    pub fn derive(secret: &[u8], scope: &CellScope) -> Option<Self> {
        if secret.len() < MIN_PSEUDONYM_SECRET_BYTES {
            return None;
        }
        let mut info = b"norn.pseudonym.cell/1\0".to_vec();
        info.extend_from_slice(scope.org_id.as_bytes());
        info.push(0);
        info.extend_from_slice(scope.environment_id.as_bytes());
        let mut key = [0_u8; 32];
        key.copy_from_slice(&hmac(secret, &info));
        Some(Self(key))
    }

    /// The pseudonym of `value` as an identifier of `subject`.
    #[must_use]
    pub fn pseudonym(&self, subject: &str, value: &RuntimeValue) -> String {
        let canonical = value
            .canonical_json()
            .unwrap_or_else(|_| format!("{value:?}").into_bytes());
        let mut message = b"norn.pseudonym/1\0".to_vec();
        message.extend_from_slice(subject.as_bytes());
        message.push(0);
        message.extend_from_slice(&canonical);
        let digest = hmac(&self.0, &message);
        // 50 bits as ten Crockford base32 digits.
        let mut bits = digest
            .iter()
            .take(8)
            .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte))
            >> 14;
        let mut digits = [0_u8; 10];
        for digit in digits.iter_mut().rev() {
            *digit = CROCKFORD
                .get(usize::try_from(bits & 31).unwrap_or_default())
                .copied()
                .unwrap_or(b'0');
            bits >>= 5;
        }
        format!("{subject} #{}", String::from_utf8_lossy(&digits))
    }
}

/// HMAC-SHA256 (RFC 2104).
pub(crate) fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut block = if key.len() > 64 {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    block.resize(64, 0);
    let inner: Vec<u8> = block.iter().map(|b| b ^ 0x36).collect();
    let outer: Vec<u8> = block.iter().map(|b| b ^ 0x5c).collect();
    let mut hasher = Sha256::new();
    hasher.update(&inner);
    hasher.update(data);
    let inner_hash = hasher.finalize();
    let mut hasher = Sha256::new();
    hasher.update(&outer);
    hasher.update(inner_hash);
    hasher.finalize().to_vec()
}

/// The cell's declared markings, by id.
pub type MarkingCatalog = Arc<BTreeMap<String, MarkingIr>>;

/// The marking catalog of the cell's active definitions.
///
/// # Errors
/// The retained definitions cannot be read.
pub(crate) fn catalog_of<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
) -> Result<(Option<u64>, MarkingCatalog), Refusal> {
    let version = reader
        .active_definitions_version()
        .map_err(Refusal::host)?;
    let Some(set) = reader.installed_definitions(version).map_err(Refusal::host)? else {
        return Ok((version, Arc::default()));
    };
    let artifacts =
        crate::kernel_set::retained_artifacts(&set.canonical_set).map_err(Refusal::host)?;
    let catalog = norn_checker::marking_catalog_from_artifacts(&artifacts)
        .into_iter()
        .map(|(id, marking)| (id.0, marking))
        .collect();
    Ok((version, Arc::new(catalog)))
}

/// A row's effective markings by property. `None` when the snapshot is
/// unavailable, which projects every property as refused. A cell that
/// declares no marking has nothing to project.
///
/// # Errors
/// A store failure.
pub(crate) fn row_markings<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    catalog: &MarkingCatalog,
    row: &ObjectRecord,
) -> Result<Option<BTreeMap<String, Vec<String>>>, Refusal> {
    if catalog.is_empty() {
        return Ok(Some(BTreeMap::new()));
    }
    reader
        .effective_markings(MAIN_WORLD, &row.object_type, &row.id, None)
        .map_err(Refusal::host)
}

/// Whether the source lineage of `row` (one exact version) may be disclosed:
/// a source row carries stored values, so an instance with any marked
/// property, or whose marking snapshot is unavailable, discloses none.
///
/// # Errors
/// A store failure.
pub(crate) fn discloses_source<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    catalog: &MarkingCatalog,
    row: &ObjectRecord,
) -> Result<bool, norn_cell::CellError> {
    if catalog.is_empty() {
        return Ok(true);
    }
    let current = reader.version(MAIN_WORLD, &row.object_type, &row.id)? == row.version;
    Ok(reader
        .effective_markings(
            MAIN_WORLD,
            &row.object_type,
            &row.id,
            (!current).then_some(row.version),
        )?
        .is_some_and(|markings| markings.values().all(Vec::is_empty)))
}

/// A version's effective markings: the current snapshot for the current
/// version, else that version's immutable snapshot.
///
/// # Errors
/// A store failure.
pub(crate) fn version_markings<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    catalog: &MarkingCatalog,
    row: &ObjectRecord,
) -> Result<Option<BTreeMap<String, Vec<String>>>, Refusal> {
    if catalog.is_empty() {
        return Ok(Some(BTreeMap::new()));
    }
    let current = reader
        .version(MAIN_WORLD, &row.object_type, &row.id)
        .map_err(Refusal::host)?
        == row.version;
    reader
        .effective_markings(
            MAIN_WORLD,
            &row.object_type,
            &row.id,
            (!current).then_some(row.version),
        )
        .map_err(Refusal::host)
}

/// What a projection did to one row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projected {
    /// Every marked property and how it was projected.
    pub masked: BTreeMap<String, NornMaskedProperty>,
    /// Properties left out of the row's values.
    pub withheld: Vec<String>,
}

/// The strongest projection among `markings`; `None` when one is undeclared.
fn strongest(catalog: &MarkingCatalog, markings: &[String]) -> Option<MaskPolicy> {
    let mut chosen: Option<MaskPolicy> = None;
    for id in markings {
        let mask = &catalog.get(id)?.mask;
        let stronger = match (&chosen, mask) {
            (None, _) => true,
            (Some(MaskPolicy::LastN { count: old }), MaskPolicy::LastN { count }) => count < old,
            (Some(current), candidate) => candidate.strength() > current.strength(),
        };
        if stronger {
            chosen = Some(mask.clone());
        }
    }
    chosen
}

fn text_of(value: &RuntimeValue) -> String {
    value.as_str().map_or_else(
        || {
            value
                .canonical_json()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        },
        str::to_owned,
    )
}

/// Apply `mask` to `value` in place; the mode actually applied.
fn apply(mask: &MaskPolicy, key: Option<&PseudonymKey>, value: &mut RuntimeValue) -> NornMaskMode {
    if matches!(value, RuntimeValue::Null) {
        return match mask {
            MaskPolicy::Pseudonym { .. } => NornMaskMode::Pseudonym,
            MaskPolicy::Redacted {} => NornMaskMode::Redacted,
            MaskPolicy::Null {} => NornMaskMode::Null,
            MaskPolicy::LastN { .. } => NornMaskMode::LastN,
            MaskPolicy::Refuse {} => NornMaskMode::Refuse,
        };
    }
    match mask {
        MaskPolicy::Pseudonym { subject } => {
            if let Some(key) = key {
                *value = RuntimeValue::String(key.pseudonym(subject, value));
                NornMaskMode::Pseudonym
            } else {
                *value = RuntimeValue::String(REDACTED_TEXT.to_owned());
                NornMaskMode::Redacted
            }
        }
        MaskPolicy::Redacted {} => {
            *value = RuntimeValue::String(REDACTED_TEXT.to_owned());
            NornMaskMode::Redacted
        }
        MaskPolicy::Null {} => {
            *value = RuntimeValue::Null;
            NornMaskMode::Null
        }
        MaskPolicy::LastN { count } => {
            let text = text_of(value);
            let chars: Vec<char> = text.chars().collect();
            let keep = usize::try_from(*count).unwrap_or(usize::MAX);
            let tail: String = if chars.len() > keep {
                chars
                    .get(chars.len().saturating_sub(keep)..)
                    .unwrap_or_default()
                    .iter()
                    .collect()
            } else {
                String::new()
            };
            *value = RuntimeValue::String(format!("{REDACTED_TEXT}{tail}"));
            NornMaskMode::LastN
        }
        MaskPolicy::Refuse {} => NornMaskMode::Refuse,
    }
}

/// Project `row`'s values for `actor` in place, from its effective
/// `markings` (`None`: unavailable, so every property is refused).
pub(crate) fn project<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    catalog: &MarkingCatalog,
    markings: Option<&BTreeMap<String, Vec<String>>>,
    row: &mut ObjectRecord,
) -> Projected {
    let mut projected = Projected::default();
    let Some(markings) = markings else {
        for property in std::mem::take(&mut row.values).into_keys() {
            projected.masked.insert(
                property.clone(),
                NornMaskedProperty {
                    mode: NornMaskMode::Refuse,
                    markings: Vec::new(),
                    revealable: false,
                },
            );
            projected.withheld.push(property);
        }
        return projected;
    };
    for (property, ids) in markings.iter().filter(|(_, ids)| !ids.is_empty()) {
        let mask = strongest(catalog, ids);
        let revealable = mask.is_some() && ids.iter().all(|id| host.reveals(actor, id));
        let mask = mask.unwrap_or(MaskPolicy::Refuse {});
        let mode = match row.values.get_mut(property) {
            Some(value) => apply(&mask, host.pseudonym_key(), value),
            None => continue,
        };
        if mode == NornMaskMode::Refuse {
            row.values.remove(property);
            projected.withheld.push(property.clone());
        }
        projected.masked.insert(
            property.clone(),
            NornMaskedProperty {
                mode,
                markings: ids.clone(),
                revealable,
            },
        );
    }
    projected
}

/// `POST /v1/objects/{type}/{id}:reveal` request body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornRevealRequest {
    /// The marked property to reveal.
    pub property: String,
    /// Why the caller needs the value; kept in the receipt.
    #[serde(default)]
    pub purpose: Option<String>,
    /// The caller's key for this reveal: a retry with the same key replays
    /// the first receipt.
    pub idempotency_key: String,
}

/// The receipt a reveal wrote to the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRevealReceipt {
    /// The ledger position of the receipt.
    pub commit_seq: u64,
    /// The receipt's hash.
    pub receipt_hash: String,
    /// When the value was revealed (the commit's logical time, ms).
    pub revealed_at: u64,
    /// Who it was revealed to.
    pub actor: String,
    /// The object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The property revealed.
    pub property: String,
    /// The fact version revealed.
    pub version: u64,
    /// The markings the reveal was decided for.
    pub markings: Vec<String>,
    /// The purpose given; empty when none.
    pub purpose: String,
    /// The receipt replayed an earlier reveal under the same key.
    pub replayed: bool,
}

/// A revealed value and its receipt.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRevealed {
    /// The stored value.
    #[schema(value_type = Object)]
    pub value: RuntimeValue,
    /// The ledger receipt of this reveal.
    pub receipt: NornRevealReceipt,
}

/// The longest purpose a reveal keeps, in characters.
pub const MAX_PURPOSE_CHARS: usize = 500;

fn reveal_denied() -> Refusal {
    Refusal::new(
        http::StatusCode::FORBIDDEN,
        crate::refusal::codes::AUTHZ_DENIED,
        "the reveal is not authorized",
        "the caller's policy does not allow norn.reveal for every marking on this property",
    )
}

#[utoipa::path(post, path = "/v1/objects/{type}/{id}/reveal", operation_id = "reveal_object_property", params(crate::scope::ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded."), ("id" = String, Path, description = "The object identity; percent-escapes are decoded.")), request_body = NornRevealRequest, responses((status = 200, description = "The stored value of the marked property and the ledger receipt of this reveal: who, what, when and why.", body = NornRevealed), (status = 400, description = "The request was refused as invalid (`request.invalid`): a malformed body, key or purpose.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, or the caller's policy does not allow `norn.reveal` for every marking on the property.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the object is absent, quarantined or not readable, or the property is not marked; these are not told apart.", body = Refusal), (status = 409, description = "The request was refused: the object changed during the reveal (`object.snapshot_moved`, retryable) or the key was used for another reveal (`reveal.idempotency_conflict`).", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
/// PRIVACY-2: reveal one marked property of one readable object to `actor`.
/// The row gate (Population and Values) applies first, so an unreadable
/// object refuses like an absent one. The PDP then decides `norn.reveal` for
/// every effective marking of the property; the receipt is committed before
/// the value is returned, so no value leaves without one.
///
/// # Errors
/// A denied gate (403), an absent, invisible or quarantined object (404), a
/// property that is not marked or not present (404 `object.not_found`), a
/// denied reveal (403), an invalid body (400), or a host failure.
pub fn reveal<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &crate::scope::ScopeQuery,
    actor: Option<&str>,
    (object_type, id): (&str, &str),
    request: &NornRevealRequest,
    access: &dyn crate::object_routes::ObjectAccess,
) -> Result<NornRevealed, Refusal> {
    let key = request.idempotency_key.as_str();
    if key.is_empty() || key.len() > 128 {
        return Err(Refusal::invalid("idempotencyKey must be 1..=128 bytes"));
    }
    let purpose = request.purpose.clone().unwrap_or_default();
    if purpose.chars().count() > MAX_PURPOSE_CHARS || purpose.chars().any(char::is_control) {
        return Err(Refusal::invalid(format!(
            "purpose must be at most {MAX_PURPOSE_CHARS} characters without control characters"
        )));
    }
    let (_epoch, actor) =
        crate::object_routes::collection(host, scope, actor, object_type, access)?;
    let (row, markings) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        if !reader.model().has_type(object_type) {
            return Err(crate::object_routes::not_found());
        }
        let row = reader
            .object(MAIN_WORLD, object_type, id)
            .map_err(Refusal::host)?
            .ok_or_else(crate::object_routes::not_found)?;
        if reader
            .observation_is_quarantined(MAIN_WORLD, &row)
            .map_err(Refusal::host)?
        {
            return Err(crate::object_routes::not_found());
        }
        let markings = reader
            .effective_markings(MAIN_WORLD, &row.object_type, &row.id, None)
            .map_err(Refusal::host)?
            .ok_or_else(crate::object_routes::not_found)?;
        (row, markings)
    };
    if !crate::object_routes::row_readable(host, access, &actor, &row) {
        return Err(crate::object_routes::not_found());
    }
    let ids = markings
        .get(&request.property)
        .filter(|ids| !ids.is_empty())
        .cloned()
        .ok_or_else(crate::object_routes::not_found)?;
    let value = row
        .values
        .get(&request.property)
        .cloned()
        .ok_or_else(crate::object_routes::not_found)?;
    if !ids.iter().all(|marking| host.reveals(&actor, marking)) {
        return Err(reveal_denied());
    }
    let receipt = norn_cell::MarkingReveal {
        object_type: row.object_type.clone(),
        key: row.id.clone(),
        property: request.property.clone(),
        version: row.version,
        markings: ids,
        actor: actor.clone(),
        purpose,
    };
    let outcome = {
        let mut cell = host.cell()?;
        // The value read above must still be the current one.
        let current = cell
            .store()
            .read()
            .version(MAIN_WORLD, &row.object_type, &row.id)
            .map_err(Refusal::host)?;
        if current != row.version {
            return Err(crate::object_routes::snapshot_moved());
        }
        cell.record_reveal(&format!("reveal:{actor}:{key}"), &receipt)
            .map_err(|error| match error {
                norn_cell::CellError::IdempotencyConflict(_) => Refusal::new(
                    http::StatusCode::CONFLICT,
                    "reveal.idempotency_conflict",
                    "the idempotency key was used for another reveal",
                    "use a new key for a new reveal",
                ),
                other => Refusal::host(other),
            })?
    };
    let replayed = matches!(outcome, norn_cell::CommitOutcome::Replayed(_));
    let committed = outcome.receipt();
    Ok(NornRevealed {
        value,
        receipt: NornRevealReceipt {
            commit_seq: committed.body.commit_seq,
            receipt_hash: committed.hash.clone(),
            revealed_at: committed.body.logical_time,
            actor: receipt.actor,
            object_type: receipt.object_type,
            id: receipt.key,
            property: receipt.property,
            version: receipt.version,
            markings: receipt.markings,
            purpose: receipt.purpose,
            replayed,
        },
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "pseudonym fixtures")]
mod tests {
    use super::*;

    fn scope(org: &str) -> CellScope {
        CellScope {
            org_id: org.into(),
            environment_id: "dev".into(),
        }
    }

    #[test]
    fn pseudonyms_are_stable_per_cell_distinct_across_cells_and_never_the_value() {
        let secret = [7_u8; 32];
        let one = PseudonymKey::derive(&secret, &scope("org_a")).expect("key");
        let other = PseudonymKey::derive(&secret, &scope("org_b")).expect("key");
        let email = RuntimeValue::String("ada@example.com".into());
        let first = one.pseudonym("Customer", &email);
        assert_eq!(first, one.pseudonym("Customer", &email));
        assert_ne!(first, other.pseudonym("Customer", &email));
        assert_ne!(first, one.pseudonym("Supplier", &email));
        assert!(!first.contains("ada"));
        assert!(first.starts_with("Customer #") && first.len() == "Customer #".len() + 10);
        assert!(PseudonymKey::derive(&[1_u8; 31], &scope("org_a")).is_none());
    }
}
