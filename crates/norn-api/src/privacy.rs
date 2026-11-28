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
//! ledger receipt ([`reveal`](crate::privacy::reveal)). Nothing here trusts the client.

pub mod disclosure;
pub(crate) mod source_grants;

pub use disclosure::{
    DisclosedAnswer, DisclosedDocument, DisclosedForkAnswer, DisclosedObject,
    DisclosedObjectFindings, DisclosedObjectHistory, DisclosedObjectLinks, DisclosedObjectPage,
    DisclosedObjectQueryPage, DisclosedObjectVersion, DisclosedQuarantinedPage,
    DisclosedQueriedObject, DisclosedSourceLineage, DisclosedTrace, DisclosedTransformLineage,
    DisclosedTransformPage, serialize_disclosed,
};

use std::collections::BTreeMap;
use std::sync::Arc;

use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::{MarkingIr, MaskPolicy, RuntimeValue};
use serde::{Deserialize, Serialize};
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
        key.copy_from_slice(&crate::uploads::hmac(secret, &info));
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
        let digest = crate::uploads::hmac(&self.0, &message);
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

impl PseudonymKey {
    /// A keyed, unlinkable stand-in for a derived row's id: its plain id is
    /// a hash of grain values a dictionary could reverse.
    #[must_use]
    pub fn keyed_id(&self, id: &str) -> String {
        let mut message = b"norn.row-id/1\0".to_vec();
        message.extend_from_slice(id.as_bytes());
        crate::uploads::hex(&crate::uploads::hmac(&self.0, &message))
    }
}

/// How a derived row's id leaves the server: as it is, or keyed when a
/// grain column carries markings in a cell that declares them. A cell with
/// markings but no pseudonym key serves no such rows.
///
/// # Errors
/// The ids would need a key the host lacks (403).
fn row_ids<E: SqlEngine>(
    host: &DevHost<E>,
    catalog: &MarkingCatalog,
    grain_marked: bool,
) -> Result<Option<PseudonymKey>, Refusal> {
    if !grain_marked || catalog.is_empty() {
        return Ok(None);
    }
    host.pseudonym_key()
        .cloned()
        .map(Some)
        .ok_or_else(crate::authz::read_denied)
}

/// The cell's declared markings and the kernel's own, by id. Empty exactly
/// when the cell holds no marking label at all, current or past
/// (`DevHost::marking_catalog`): then every field is served as itself.
pub type MarkingCatalog = Arc<BTreeMap<String, MarkingIr>>;

/// The labels the kernel writes without any declaration (no declared id
/// holds a `-`): `norn.unknown-lineage` marks a value whose lineage the cell
/// cannot prove, such as a `norn.RuleRun`'s detail, and is withheld (S3,
/// #1547). They resolve as themselves, never as undeclared.
fn built_in_markings() -> impl Iterator<Item = (String, MarkingIr)> {
    [MarkingIr {
        id: norn_ir::MarkingId("norn.unknown-lineage".to_owned()),
        label: "Unknown lineage".to_owned(),
        severity: norn_ir::MarkingSeverity::High,
        category: "norn.lineage".to_owned(),
        mask: MaskPolicy::Refuse {},
    }]
    .into_iter()
    .map(|marking| (marking.id.0.clone(), marking))
}

/// Whether `catalog` holds a marking the cell's definitions declare, not
/// only the kernel's own.
fn declares(catalog: &MarkingCatalog) -> bool {
    let built_in = built_in_catalog();
    catalog.keys().any(|id| !built_in.contains_key(id))
}

/// The catalog of a cell that declares no marking but holds labels: the
/// kernel's own, so every privacy path projects and any other label (one
/// declassified, or whose declaration was removed) withholds its field.
pub(crate) fn built_in_catalog() -> MarkingCatalog {
    static BUILT_IN: std::sync::LazyLock<MarkingCatalog> =
        std::sync::LazyLock::new(|| Arc::new(built_in_markings().collect()));
    Arc::clone(&BUILT_IN)
}

/// The marking catalog of the cell's active definitions.
///
/// # Errors
/// The retained definitions cannot be read.
pub(crate) fn catalog_of<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
) -> Result<(Option<u64>, MarkingCatalog), Refusal> {
    let version = reader.active_definitions_version().map_err(Refusal::host)?;
    let Some(set) = reader
        .installed_definitions(version)
        .map_err(Refusal::host)?
    else {
        return Ok((version, Arc::default()));
    };
    let artifacts =
        crate::kernel_set::retained_artifacts(&set.canonical_set).map_err(Refusal::host)?;
    let mut catalog: BTreeMap<String, MarkingIr> =
        norn_checker::marking_catalog_from_artifacts(&artifacts)
            .into_iter()
            .map(|(id, marking)| (id.0, marking))
            .collect();
    if !catalog.is_empty() {
        catalog.extend(built_in_markings());
    }
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

fn row_version_markings<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    catalog: &MarkingCatalog,
    row: &ObjectRecord,
) -> Result<Option<BTreeMap<String, Vec<String>>>, norn_cell::CellError> {
    if catalog.is_empty() {
        return Ok(Some(BTreeMap::new()));
    }
    let current = reader.version(MAIN_WORLD, &row.object_type, &row.id)? == row.version;
    reader.effective_markings(
        MAIN_WORLD,
        &row.object_type,
        &row.id,
        (!current).then_some(row.version),
    )
}

/// Whether the source lineage of `row` (one exact version) may be disclosed:
/// a source row carries stored values, so an instance with any marked
/// property, or whose marking snapshot is unavailable, discloses none.
///
/// # Errors
/// A store failure.
fn discloses_source<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    catalog: &MarkingCatalog,
    row: &ObjectRecord,
) -> Result<bool, norn_cell::CellError> {
    Ok(row_version_markings(reader, catalog, row)?
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
    row_version_markings(reader, catalog, row).map_err(Refusal::host)
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
pub(crate) fn strongest(catalog: &MarkingCatalog, markings: &[String]) -> Option<MaskPolicy> {
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
                .map_or_default(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        },
        str::to_owned,
    )
}

/// Apply `mask` to `value` in place; the mode actually applied.
pub(crate) fn apply(
    mask: &MaskPolicy,
    key: Option<&PseudonymKey>,
    value: &mut RuntimeValue,
) -> NornMaskMode {
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
fn project<E: SqlEngine>(
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

/// Project the marked columns of one derived row (a transform's) in place:
/// each column holds the strongest projection of its lineage markings, and a
/// refused column is left out. A derived row is never revealable.
fn project_columns<E: SqlEngine>(
    host: &DevHost<E>,
    catalog: &MarkingCatalog,
    columns: &[(String, Vec<String>)],
    rows: &mut [&mut BTreeMap<String, RuntimeValue>],
) {
    if catalog.is_empty() || columns.iter().all(|(_, markings)| markings.is_empty()) {
        return;
    }
    for (column, markings) in columns.iter().filter(|(_, markings)| !markings.is_empty()) {
        let mask = strongest(catalog, markings).unwrap_or(MaskPolicy::Refuse {});
        for values in rows.iter_mut() {
            if let Some(value) = values.get_mut(column)
                && apply(&mask, host.pseudonym_key(), value) == NornMaskMode::Refuse
            {
                values.remove(column);
            }
        }
    }
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
    /// The stored value; decimals are exact JSON number tokens.
    #[schema(schema_with = revealed_value_schema)]
    pub value: RuntimeValue,
    /// The ledger receipt of this reveal.
    pub receipt: NornRevealReceipt,
}

fn revealed_value_schema() -> utoipa::openapi::schema::Schema {
    let mut schema =
        utoipa::openapi::schema::Object::with_type(utoipa::openapi::schema::SchemaType::AnyValue);
    schema.description = Some(
        "The stored value, of the property's type; decimals are exact JSON number tokens."
            .to_owned(),
    );
    utoipa::openapi::schema::Schema::Object(schema)
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
    let receipt = record_reveal(host, &actor, &row, (&request.property, ids), &purpose, key)?;
    Ok(NornRevealed { value, receipt })
}

/// Commit the receipt of revealing `property` (with its effective
/// `markings`) of `row` to `actor`, after checking the row is still at the
/// version read. The caller has already had the PDP allow every marking.
///
/// # Errors
/// The row moved (409), the key was used for another reveal (409), or a
/// host failure.
pub(crate) fn record_reveal<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    row: &ObjectRecord,
    (property, markings): (&str, Vec<String>),
    purpose: &str,
    key: &str,
) -> Result<NornRevealReceipt, Refusal> {
    let receipt = norn_cell::MarkingReveal {
        object_type: row.object_type.clone(),
        key: row.id.clone(),
        property: property.to_owned(),
        version: row.version,
        markings,
        actor: actor.to_owned(),
        purpose: purpose.to_owned(),
    };
    let outcome = {
        let mut cell = host.cell()?;
        // The value read must still be the current one.
        let current = cell
            .store()
            .read()
            .version(MAIN_WORLD, &row.object_type, &row.id)
            .map_err(Refusal::host)?;
        if current != row.version {
            return Err(crate::object_routes::snapshot_moved());
        }
        cell.record_reveal(&format!("reveal:{actor}:{key}"), &receipt)
            .map_err(|error| {
                if matches!(error, norn_cell::CellError::IdempotencyConflict(_)) {
                    Refusal::new(
                        http::StatusCode::CONFLICT,
                        "reveal.idempotency_conflict",
                        "the idempotency key was used for another reveal",
                        "use a new key for a new reveal",
                    )
                } else {
                    Refusal::host(error)
                }
            })?
    };
    let replayed = matches!(outcome, norn_cell::CommitOutcome::Replayed(_));
    let committed = outcome.receipt();
    Ok(NornRevealReceipt {
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
    })
}

/// Population decides membership; a member without Values is recorded so its
/// bucket's amounts are withheld (DATA-VALUES-1).
fn withhold_values(
    result: &mut norn_cell::compute::QueryResult,
    withheld: &std::collections::BTreeSet<String>,
    min_disclosed: usize,
) {
    for row in result.rows.values_mut() {
        row.withhold(|id| withheld.contains(id), min_disclosed.max(1));
    }
}

/// PRIVACY-1: trace contributions never disclose raw marked property values,
/// raw snapshot digests of marked versions, or source provenance for marked
/// versions. Returns `true` if any contribution carried markings (or lacked a
/// marking snapshot) so raw ledger and receipt hashes are also withheld.
fn redact_trace_contributions<E: SqlEngine>(
    host: &DevHost<E>,
    (actor, world): (&str, &str),
    reader: &StoreReader<'_, E>,
    contributions: &mut [norn_cell::compute::lineage::NumericContribution],
) -> Result<bool, Refusal> {
    let catalog = host.marking_catalog(reader)?;
    if catalog.is_empty() {
        return Ok(false);
    }
    let scope = host.scope().key();
    let mut any_marked = false;
    for contribution in contributions {
        let mut record = ObjectRecord {
            object_type: contribution.fact.object_type.0.clone(),
            id: contribution.fact.key.clone(),
            scope: scope.clone(),
            version: contribution.fact.version,
            values: contribution.values.clone().unwrap_or_default(),
        };
        let markings = reader
            .effective_markings(world, &record.object_type, &record.id, Some(record.version))
            .map_err(Refusal::host)?;
        let occurrence = reader
            .fact_occurrence_markings(world, &record.object_type, &record.id, Some(record.version))
            .map_err(Refusal::host)?;
        let property_marked = markings
            .as_ref()
            .is_none_or(|m| m.values().any(|ids| !ids.is_empty()));
        let occurrence_marked = occurrence.as_ref().is_none_or(|ids| !ids.is_empty());
        if !(property_marked || occurrence_marked) {
            continue;
        }
        any_marked = true;
        contribution.fact_digest = None;
        contribution.source = norn_cell::compute::lineage::ContributionSource::Restricted;
        if contribution.values.is_some() {
            let _ = project(host, actor, &catalog, markings.as_ref(), &mut record);
            contribution.values = Some(record.values);
        }
    }
    Ok(any_marked)
}

/// Effective transform markings and catalog checked inside the evaluation
/// transaction.
pub(crate) struct ClassifiedTransform {
    pub(crate) marked: Vec<(String, Vec<String>)>,
    pub(crate) catalog: MarkingCatalog,
}

pub(crate) fn transform_snapshot_moved() -> Refusal {
    let mut moved = Refusal::new(
        http::StatusCode::CONFLICT,
        "transform.snapshot_moved",
        "the cell committed since the cursor was issued",
        "read the transform again from its first page",
    );
    moved.retryable = true;
    moved
}

fn is_implicit_id(model: &norn_cell::Model, object_type: &str, property: &str) -> bool {
    property == "id"
        && !model
            .effective_property_markings(object_type)
            .is_ok_and(|props| props.contains_key("id"))
}

/// PRIVACY-4: refuse a transform whose `Filter` predicate or `orderBy` reads a
/// property carrying live (or, for stale materializations, historical)
/// `_norn_markings` or lacking a marking snapshot.
fn refuse_dynamic_filter_order_markings<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    model: &norn_cell::Model,
    catalog: &MarkingCatalog,
    plan: &norn_checker::transform::TransformPlan,
    include_history: bool,
) -> Result<(), Refusal> {
    if catalog.is_empty() {
        return Ok(());
    }
    for (pointer, object_type, property) in plan.filter_and_order_origins() {
        if is_implicit_id(model, &object_type, &property) {
            continue;
        }
        let live_marked = !reader
            .property_marking_summary(MAIN_WORLD, &object_type, &property)
            .map_err(Refusal::host)?
            .is_some_and(|labels| labels.is_empty());
        let history_marked = include_history
            && (reader
                .property_history_marked(MAIN_WORLD, &object_type, &property)
                .map_err(Refusal::host)?
                || reader
                    .property_history_unclassified(MAIN_WORLD, &object_type, &property)
                    .map_err(Refusal::host)?);
        if live_marked || history_marked {
            let mut refusal = Refusal::new(
                http::StatusCode::UNPROCESSABLE_ENTITY,
                "privacy.marked_column",
                "the transform filters or orders by a marked property",
                "remove the filter or order over marked properties",
            );
            refusal.diagnostics = vec![crate::refusal::RefusalDiagnostic::error(
                norn_checker::metric::codes::MARKED_FIELD,
                "this transform filters or orders by a property whose values are marked",
                pointer,
            )];
            return Err(refusal);
        }
    }
    Ok(())
}

/// Whether any grain column carries static or dynamic effective markings.
fn grain_marked(marked: &[(String, Vec<String>)], grain: &[String]) -> bool {
    marked
        .iter()
        .any(|(name, markings)| !markings.is_empty() && grain.contains(name))
}

/// Each output column with its effective markings: static lineage markings
/// unioned with live dynamic `_norn_markings` across all column origins
/// (failing closed to `norn.unknown-lineage` if any row lacks a snapshot, or if
/// a stale materialized result reads a property with historical markings).
pub(crate) fn effective_marked_columns<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    model: &norn_cell::Model,
    catalog: &MarkingCatalog,
    plan: &norn_checker::transform::TransformPlan,
    stale: bool,
) -> Result<Vec<(String, Vec<String>)>, Refusal> {
    let mut out = Vec::with_capacity(plan.output_schema().columns.len());
    for column in &plan.output_schema().columns {
        let mut markings = column.markings.clone();
        if !catalog.is_empty() {
            for origin in &column.origins {
                let Some(scan) = plan.inputs.get(&origin.alias) else {
                    continue;
                };
                if is_implicit_id(model, &scan.object_type, &origin.property) {
                    continue;
                }
                match reader
                    .property_marking_summary(MAIN_WORLD, &scan.object_type, &origin.property)
                    .map_err(Refusal::host)?
                {
                    Some(labels) => markings.extend(labels),
                    None => {
                        markings.insert("norn.unknown-lineage".to_owned());
                    }
                }
                if stale
                    && (reader
                        .property_history_marked(MAIN_WORLD, &scan.object_type, &origin.property)
                        .map_err(Refusal::host)?
                        || reader
                            .property_history_unclassified(
                                MAIN_WORLD,
                                &scan.object_type,
                                &origin.property,
                            )
                            .map_err(Refusal::host)?)
                {
                    markings.insert("norn.unknown-lineage".to_owned());
                }
            }
        }
        out.push((column.name.clone(), markings.into_iter().collect()));
    }
    Ok(out)
}

/// Verify active definitions and dynamic filter/order markings inside the
/// transform evaluation or refresh transaction.
pub(crate) fn guard_transform_in_tx<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    model: &norn_cell::Model,
    plan: &norn_checker::transform::TransformPlan,
    eval_defs: Option<&(u64, String)>,
    include_history: bool,
) -> Result<MarkingCatalog, Refusal> {
    let tx_defs = reader
        .active_definitions_identity()
        .map_err(Refusal::host)?;
    if tx_defs.as_ref() != eval_defs {
        return Err(Refusal::from_compute(norn_cell::CellError::ComputeRefused(
            "core.compute.stale_input",
        )));
    }
    let catalog = host.marking_catalog(reader)?;
    refuse_dynamic_filter_order_markings(reader, model, &catalog, plan, include_history)?;
    Ok(catalog)
}

/// Classify a transform inside the same SQLite transaction that evaluates or
/// serves it, pinning active definitions and dynamic markings to that snapshot.
pub(crate) fn classify_transform_in_tx<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    model: &norn_cell::Model,
    plan: &norn_checker::transform::TransformPlan,
    eval_defs: Option<&(u64, String)>,
    stale: bool,
) -> Result<ClassifiedTransform, Refusal> {
    let catalog = guard_transform_in_tx(host, reader, model, plan, eval_defs, stale)?;
    let marked = effective_marked_columns(reader, model, &catalog, plan, stale)?;
    Ok(ClassifiedTransform { marked, catalog })
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
