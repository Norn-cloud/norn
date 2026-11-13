//! PRIVACY-GUARD-1: the AI never sees personal data.
//!
//! Every model or agent call Norn makes sends a prompt through this guard
//! first (`POST /v1/guard:prompt`):
//!
//! - **Markings first.** Values of marked properties are replaced by typed
//!   placeholder tokens numbered per call (`[CUSTOMER_1]`, `[EMAIL_1]`): the
//!   context records the caller names, and any marked value of the cell that
//!   occurs in the text (a bounded scan of the cell's marked properties). The
//!   cell knows which values are personal from the declared markings, so
//!   this needs no guessing. Equal values share one token.
//! - **Then a detector pass** over the remaining free text for email
//!   addresses, phone numbers and payment card numbers (Luhn-checked).
//! - **The token map stays in the cell.** The guard commits a
//!   `privacy.guarded` ledger receipt: who asked, the tokenized prompt that
//!   is all a model receives, and each token's source (a field reference
//!   with its markings, or a detected literal). Nothing returned to the
//!   caller carries a value.
//!
//! A reply is restored with `POST /v1/guard/{id}:restore`: a field token
//! becomes its value only when the PDP allows the reader `norn.reveal` for
//! every marking of the field, and each such restore is a logged reveal
//! (`marking.revealed`); everyone else reads the field's projection (its
//! pseudonym, or the placeholder). A detected literal is restored only to
//! the reader who typed it.

use std::collections::BTreeMap;

use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    host::DevHost,
    object_routes::{ObjectAccess, not_found, row_readable},
    privacy::{self, MarkingCatalog},
    refusal::Refusal,
    scope::ScopeQuery,
};

/// The longest prompt the guard takes, in characters.
pub const MAX_PROMPT_CHARS: usize = 20_000;
/// Context records one call may name.
pub const MAX_CONTEXT: usize = 20;
/// Rows the guard reads at most when it looks for marked values in the text.
pub const MAX_SCAN_ROWS: usize = 5_000;
/// The shortest marked value matched in free text, in characters.
pub const MIN_MATCH_CHARS: usize = 3;

/// One record whose fields are context for the call.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornGuardRef {
    /// The object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
}

/// `POST /v1/guard:prompt` request body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornGuardRequest {
    /// The prompt as the person wrote it.
    pub text: String,
    /// Records whose fields the model may use, as tokens where marked.
    #[serde(default)]
    pub context: Vec<NornGuardRef>,
    /// What the call is for (`dashboard-assistant`, `mcp`), kept in the receipt.
    #[serde(default)]
    pub purpose: Option<String>,
    /// The caller's key for this call: a retry replays the same receipt.
    pub idempotency_key: String,
}

/// Where a token's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NornGuardSource {
    /// A marked property of a record.
    Field,
    /// The detector pass over free text.
    Detected,
}

/// One placeholder token of a guarded prompt. Never its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornGuardToken {
    /// The token as the model reads it, e.g. `[EMAIL_1]`.
    pub token: String,
    /// Its type: `CUSTOMER`, `NAME`, `EMAIL`, `PHONE`, `CARD`, or a field's.
    pub kind: String,
    /// What it stands for, in words (`Email`, `Customer`).
    pub label: String,
    /// Where the value came from.
    pub source: NornGuardSource,
    /// The field it stands for (field tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The field's markings (field tokens).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<String>,
}

/// A guarded prompt: what the model may receive, and the receipt that holds
/// its token map in the cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornGuarded {
    /// The receipt's commit; restore with it.
    pub guard_id: u64,
    /// The receipt's hash.
    pub receipt_hash: String,
    /// The tokenized prompt: the only text a model receives.
    pub safe_prompt: String,
    /// Each token, in order of first appearance.
    pub tokens: Vec<NornGuardToken>,
    /// Fields withheld from the model, as `type/id.property`.
    pub withheld: Vec<String>,
}

/// `POST /v1/guard/{id}:restore` request body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornRestoreRequest {
    /// Text that uses the guard's tokens: the model's reply, or the safe
    /// prompt itself ("Show original").
    pub text: String,
    /// The caller's key for this restore; each revealed field's receipt is
    /// keyed by it.
    pub idempotency_key: String,
}

/// One piece of a restored text.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRestoredSegment {
    /// Plain text, or what replaced the token.
    pub text: String,
    /// The token this segment replaced; absent for plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The token's type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `restored` (the value, revealed to this reader), `projected` (its
    /// pseudonym or mask) or `kept` (the placeholder stays).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// A restored text and the reveals it logged.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRestored {
    /// The guard restored.
    pub guard_id: u64,
    /// The text, in pieces.
    pub segments: Vec<NornRestoredSegment>,
    /// The `marking.revealed` receipts this restore committed.
    pub reveals: Vec<u64>,
    /// Every token of the text was restored for this reader.
    pub complete: bool,
}

/// One token's entry in the cell-held map (inside the receipt event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    token: String,
    kind: String,
    label: String,
    source: NornGuardSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    object_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    property: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    markings: Vec<String>,
    /// A detected literal, kept in the cell for its author's restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    literal: Option<String>,
}

/// The `privacy.guarded` receipt event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Guarded {
    #[serde(rename = "type")]
    kind: String,
    actor: String,
    purpose: String,
    safe_prompt: String,
    tokens: Vec<Entry>,
    withheld: Vec<String>,
}

const GUARDED: &str = "privacy.guarded";

/// A marked value found for tokenizing.
struct Candidate {
    value: String,
    kind: String,
    label: String,
    row: ObjectRecord,
    property: String,
    markings: Vec<String>,
}

fn upper_snake(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

fn title_case(kind: &str) -> String {
    let lower = kind.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A field token's type: the pseudonym subject its marking declares, else
/// what the property's name says it holds.
fn field_kind(catalog: &MarkingCatalog, markings: &[String], property: &str) -> String {
    if let Some(norn_ir::MaskPolicy::Pseudonym { subject }) = privacy::strongest(catalog, markings)
    {
        return upper_snake(&subject);
    }
    let name = property.to_lowercase();
    for (needle, kind) in [
        ("email", "EMAIL"),
        ("phone", "PHONE"),
        ("mobile", "PHONE"),
        ("card", "CARD"),
        ("iban", "ACCOUNT"),
        ("address", "ADDRESS"),
        ("name", "NAME"),
    ] {
        if name.contains(needle) {
            return kind.to_owned();
        }
    }
    upper_snake(property)
}

fn text_of(value: &RuntimeValue) -> Option<String> {
    match value {
        RuntimeValue::String(text) => Some(text.clone()),
        RuntimeValue::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The marked fields of `row` for `actor`'s context.
fn marked_fields<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    catalog: &MarkingCatalog,
    row: &ObjectRecord,
) -> Result<Vec<Candidate>, Refusal> {
    let Some(markings) = privacy::row_markings(reader, catalog, row)? else {
        // An unavailable snapshot: every field is treated as marked.
        return Ok(row
            .values
            .iter()
            .filter_map(|(property, value)| {
                text_of(value).map(|value| Candidate {
                    value,
                    kind: field_kind(catalog, &[], property),
                    label: title_case(&field_kind(catalog, &[], property)),
                    row: row.clone(),
                    property: property.clone(),
                    markings: Vec::new(),
                })
            })
            .collect());
    };
    Ok(markings
        .iter()
        .filter(|(_, ids)| !ids.is_empty())
        .filter_map(|(property, ids)| {
            let value = row.values.get(property).and_then(text_of)?;
            let kind = field_kind(catalog, ids, property);
            Some(Candidate {
                value,
                label: title_case(&kind),
                kind,
                row: row.clone(),
                property: property.clone(),
                markings: ids.clone(),
            })
        })
        .collect())
}

/// Every occurrence of `needle` in `haystack` (ASCII case-insensitive), as
/// byte ranges.
fn occurrences(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let (h, n) = (haystack.to_lowercase(), needle.to_lowercase());
    if n.is_empty() || h.len() != haystack.len() || n.len() != needle.len() {
        // Lower-casing changed byte lengths: match exactly instead.
        return haystack
            .match_indices(needle)
            .map(|(at, _)| (at, at + needle.len()))
            .collect();
    }
    h.match_indices(&n).map(|(at, _)| (at, at + n.len())).collect()
}

/// A detector match: kind and byte range.
fn detect(text: &str) -> Vec<(&'static str, usize, usize)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    // Emails: local@domain.tld around each '@'.
    for (at, _) in text.match_indices('@') {
        let local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
        let domain = |b: u8| b.is_ascii_alphanumeric() || b".-".contains(&b);
        let mut start = at;
        while start > 0 && bytes.get(start - 1).copied().is_some_and(local) {
            start -= 1;
        }
        let mut end = at + 1;
        while end < bytes.len() && bytes.get(end).copied().is_some_and(domain) {
            end += 1;
        }
        while end > at + 1 && bytes.get(end - 1) == Some(&b'.') {
            end -= 1;
        }
        let host = text.get(at + 1..end).unwrap_or("");
        if start < at && host.contains('.') && !host.starts_with('.') {
            found.push(("EMAIL", start, end));
        }
    }
    // Digit runs with separators: cards (13-19 digits, Luhn) and phones (8-15).
    let mut index = 0;
    while index < bytes.len() {
        let starts = bytes.get(index).is_some_and(|b| b.is_ascii_digit() || *b == b'+')
            && (index == 0 || !bytes.get(index - 1).is_some_and(u8::is_ascii_alphanumeric));
        if !starts {
            index += 1;
            continue;
        }
        let mut end = index;
        let mut digits = String::new();
        while end < bytes.len() {
            let b = bytes.get(end).copied().unwrap_or(0);
            if b.is_ascii_digit() {
                digits.push(char::from(b));
            } else if !(b" -().".contains(&b) || (b == b'+' && end == index)) {
                break;
            }
            end += 1;
        }
        // Do not end on a separator.
        while end > index && !bytes.get(end - 1).is_some_and(u8::is_ascii_digit) {
            end -= 1;
        }
        let card = (13..=19).contains(&digits.len()) && luhn(&digits);
        let phone = !card
            && (8..=15).contains(&digits.len())
            && (bytes.get(index) == Some(&b'+')
                || text.get(index..end).is_some_and(|run| run.contains([' ', '-', '('])));
        if card {
            found.push(("CARD", index, end));
        } else if phone {
            found.push(("PHONE", index, end));
        }
        index = end.max(index + 1);
    }
    found
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0;
    for (position, c) in digits.chars().rev().enumerate() {
        let mut d = c.to_digit(10).unwrap_or(0);
        if position % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

/// The tokens assigned so far, numbered per kind.
#[derive(Default)]
struct Tokens {
    entries: Vec<Entry>,
    by_value: BTreeMap<(String, String), String>,
    counts: BTreeMap<String, usize>,
}

impl Tokens {
    fn token(&mut self, kind: &str, value: &str, make: impl FnOnce(String) -> Entry) -> String {
        let key = (kind.to_owned(), value.to_lowercase());
        if let Some(token) = self.by_value.get(&key) {
            return token.clone();
        }
        let count = self.counts.entry(kind.to_owned()).or_default();
        *count += 1;
        let token = format!("[{kind}_{count}]");
        self.by_value.insert(key, token.clone());
        self.entries.push(make(token.clone()));
        token
    }
}

/// Replace `ranges` (sorted, non-overlapping) of `text` with their tokens.
fn splice(text: &str, ranges: &[(usize, usize, String)]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for (start, end, token) in ranges {
        out.push_str(text.get(at..*start).unwrap_or(""));
        out.push_str(token);
        at = *end;
    }
    out.push_str(text.get(at..).unwrap_or(""));
    out
}

fn bad(message: impl Into<String>) -> Refusal {
    Refusal::invalid(message)
}

/// PRIVACY-GUARD-1: tokenize a prompt for a model call (see the module docs).
///
/// # Errors
/// An invalid body (400), a denied or unreadable context record (404), or a
/// host failure. Nothing is sent anywhere: the caller sends `safePrompt`.
#[utoipa::path(post, path = "/v1/guard:prompt", operation_id = "guard_prompt", params(ScopeQuery), request_body = NornGuardRequest, responses((status = 200, description = "The tokenized prompt a model may receive, its tokens (never their values) and the receipt that keeps the token map in the cell.", body = NornGuarded), (status = 400, description = "The request was refused as invalid (`request.invalid`).", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): a context record is absent or not readable.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn guard<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    request: &NornGuardRequest,
    access: &dyn ObjectAccess,
) -> Result<NornGuarded, Refusal> {
    if request.text.chars().count() > MAX_PROMPT_CHARS {
        return Err(bad(format!("text is at most {MAX_PROMPT_CHARS} characters")));
    }
    if request.context.len() > MAX_CONTEXT {
        return Err(bad(format!("at most {MAX_CONTEXT} context records")));
    }
    let key = request.idempotency_key.as_str();
    if key.is_empty() || key.len() > 128 {
        return Err(bad("idempotencyKey must be 1..=128 bytes"));
    }
    let actor = actor.ok_or_else(crate::authz::read_denied)?;
    scope_gate(host, scope, actor)?;
    // Context rows: readable by the caller, read in one section.
    let (_catalog, context, scanned) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let catalog = host.marking_catalog(&reader)?;
        let mut context = Vec::new();
        for reference in &request.context {
            if !reader.model().has_type(&reference.object_type) {
                return Err(not_found());
            }
            let row = reader
                .object(MAIN_WORLD, &reference.object_type, &reference.id)
                .map_err(Refusal::host)?
                .ok_or_else(not_found)?;
            context.push(row);
        }
        // Marked values anywhere in the cell, bounded: they are tokenized
        // wherever the text names them, whoever may read them.
        let mut scanned = Vec::new();
        if !catalog.is_empty() {
            let mut budget = MAX_SCAN_ROWS;
            let types: Vec<String> = reader
                .model()
                .object_types()
                .map(|def| def.name.as_str().to_owned())
                .collect();
            for object_type in types {
                if budget == 0 {
                    break;
                }
                let marked = reader
                    .marking_property_names(&object_type)
                    .map_err(Refusal::host)?;
                if marked.is_empty() {
                    continue;
                }
                let rows = reader
                    .list_after(MAIN_WORLD, &object_type, None, budget)
                    .map_err(Refusal::host)?;
                budget = budget.saturating_sub(rows.len());
                for row in rows.into_iter().filter(|row| row.object_type == object_type) {
                    scanned.extend(marked_fields(&reader, &catalog, &row)?);
                }
            }
        }
        let mut fields = Vec::new();
        for row in &context {
            fields.extend(marked_fields(&reader, &catalog, row)?);
        }
        (catalog, (context, fields), scanned)
    };
    let (rows, context_fields) = context;
    for row in &rows {
        if !row_readable(host, access, actor, row) {
            return Err(not_found());
        }
    }
    let mut tokens = Tokens::default();
    let mut withheld = Vec::new();
    let entry_of = |candidate: &Candidate, token: String| Entry {
        token,
        kind: candidate.kind.clone(),
        label: candidate.label.clone(),
        source: NornGuardSource::Field,
        object_type: Some(candidate.row.object_type.clone()),
        id: Some(candidate.row.id.clone()),
        property: Some(candidate.property.clone()),
        version: Some(candidate.row.version),
        markings: candidate.markings.clone(),
        literal: None,
    };
    // Context fields first, so their tokens number from 1 in reading order.
    let mut field_tokens: BTreeMap<(String, String, String), String> = BTreeMap::new();
    for candidate in &context_fields {
        let token = tokens.token(&candidate.kind, &candidate.value, |token| {
            entry_of(candidate, token)
        });
        field_tokens.insert(
            (
                candidate.row.object_type.clone(),
                candidate.row.id.clone(),
                candidate.property.clone(),
            ),
            token,
        );
        withheld.push(format!(
            "{}/{}.{}",
            candidate.row.object_type, candidate.row.id, candidate.property
        ));
    }
    // 1. Marked values in the text (longest first, never inside a token).
    let mut values: Vec<&Candidate> = context_fields
        .iter()
        .chain(&scanned)
        .filter(|candidate| candidate.value.chars().count() >= MIN_MATCH_CHARS)
        .collect();
    values.sort_by(|a, b| b.value.len().cmp(&a.value.len()).then(a.value.cmp(&b.value)));
    let mut taken: Vec<(usize, usize, String)> = Vec::new();
    let overlaps = |taken: &[(usize, usize, String)], start: usize, end: usize| {
        taken.iter().any(|(s, e, _)| start < *e && *s < end)
    };
    for candidate in values {
        for (start, end) in occurrences(&request.text, &candidate.value) {
            let bounded = !request
                .text
                .get(..start)
                .and_then(|before| before.chars().last())
                .is_some_and(char::is_alphanumeric)
                && !request
                    .text
                    .get(end..)
                    .and_then(|after| after.chars().next())
                    .is_some_and(char::is_alphanumeric);
            if bounded && !overlaps(&taken, start, end) {
                let token = tokens.token(&candidate.kind, &candidate.value, |token| {
                    entry_of(candidate, token)
                });
                let field = format!(
                    "{}/{}.{}",
                    candidate.row.object_type, candidate.row.id, candidate.property
                );
                if !withheld.contains(&field) {
                    withheld.push(field);
                }
                taken.push((start, end, token));
            }
        }
    }
    // 2. The detector pass over what is left.
    for (kind, start, end) in detect(&request.text) {
        if overlaps(&taken, start, end) {
            continue;
        }
        let literal = request.text.get(start..end).unwrap_or("").to_owned();
        let token = tokens.token(kind, &literal, |token| Entry {
            token,
            kind: kind.to_owned(),
            label: title_case(kind),
            source: NornGuardSource::Detected,
            object_type: None,
            id: None,
            property: None,
            version: None,
            markings: Vec::new(),
            literal: Some(literal.clone()),
        });
        taken.push((start, end, token));
    }
    taken.sort_by_key(|(start, _, _)| *start);
    let mut safe_prompt = splice(&request.text, &taken);
    // The context block: each record's fields, marked ones as tokens.
    for row in &rows {
        safe_prompt.push_str(&format!("\n\nRecord {} {}:", row.object_type, row.id));
        for (property, value) in &row.values {
            let shown = field_tokens
                .get(&(row.object_type.clone(), row.id.clone(), property.clone()))
                .cloned()
                .or_else(|| {
                    let marked = context_fields.iter().any(|candidate| {
                        candidate.row.id == row.id && candidate.property == *property
                    });
                    (!marked).then(|| text_of(value).unwrap_or_else(|| format!("{value:?}")))
                })
                .unwrap_or_else(|| "[WITHHELD]".to_owned());
            safe_prompt.push_str(&format!("\n- {property}: {shown}"));
        }
    }
    let event = Guarded {
        kind: GUARDED.to_owned(),
        actor: actor.to_owned(),
        purpose: request.purpose.clone().unwrap_or_default(),
        safe_prompt: safe_prompt.clone(),
        tokens: tokens.entries.clone(),
        withheld: withheld.clone(),
    };
    let encoded = norn_ir::runtime::encode(&event).map_err(Refusal::host)?;
    let outcome = {
        let mut cell = host.cell()?;
        cell.record_privacy_event(&format!("guard:{actor}:{key}"), encoded)
            .map_err(|error| match error {
                norn_cell::CellError::IdempotencyConflict(_) => Refusal::new(
                    http::StatusCode::CONFLICT,
                    "guard.idempotency_conflict",
                    "the idempotency key was used for another prompt",
                    "use a new key for a new prompt",
                ),
                other => Refusal::host(other),
            })?
    };
    let receipt = outcome.receipt();
    Ok(NornGuarded {
        guard_id: receipt.body.commit_seq,
        receipt_hash: receipt.hash.clone(),
        safe_prompt,
        tokens: tokens
            .entries
            .iter()
            .map(|entry| NornGuardToken {
                token: entry.token.clone(),
                kind: entry.kind.clone(),
                label: entry.label.clone(),
                source: entry.source,
                field: entry.property.as_ref().map(|property| {
                    format!(
                        "{}/{}.{property}",
                        entry.object_type.as_deref().unwrap_or(""),
                        entry.id.as_deref().unwrap_or("")
                    )
                }),
                markings: entry.markings.clone(),
            })
            .collect(),
        withheld,
    })
}

/// The `privacy.guarded` event committed at `guard_id`.
fn guarded_at<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    guard_id: u64,
) -> Result<Option<Guarded>, Refusal> {
    // Ledger positions grow at least as fast as commits: search upward from
    // the commit's own number for its first event.
    let entry_at = |seq: u64| -> Result<Option<norn_cell::LedgerEntry>, Refusal> {
        Ok(reader
            .ledger_page(seq.saturating_sub(1), 1)
            .map_err(Refusal::host)?
            .into_iter()
            .next())
    };
    let (mut low, mut high) = (guard_id.max(1), guard_id.max(1));
    let mut step = 1_u64;
    loop {
        match entry_at(high)? {
            Some(entry) if entry.commit_seq < guard_id => {
                low = high.saturating_add(1);
                high = high.saturating_add(step);
                step = step.saturating_mul(2);
            }
            _ => break,
        }
    }
    while low < high {
        let middle = low + (high - low) / 2;
        match entry_at(middle)? {
            Some(entry) if entry.commit_seq < guard_id => low = middle + 1,
            _ => high = middle,
        }
    }
    let Some(entry) = entry_at(low)? else {
        return Ok(None);
    };
    if entry.commit_seq != guard_id {
        return Ok(None);
    }
    let json = serde_json::to_value(&entry.event).map_err(Refusal::host)?;
    Ok(serde_json::from_value::<Guarded>(json)
        .ok()
        .filter(|event| event.kind == GUARDED))
}

fn guard_not_found() -> Refusal {
    Refusal::new(
        http::StatusCode::NOT_FOUND,
        "guard.not_found",
        "no such guarded prompt",
        "restore with the guardId the guard returned",
    )
}

/// Split `text` at the guard's tokens.
fn pieces<'a>(text: &'a str, known: &BTreeMap<&str, &Entry>) -> Vec<(&'a str, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut search = 0;
    while let Some(open) = text.get(search..).and_then(|rest| rest.find('[')) {
        let open = search + open;
        let Some(close) = text.get(open..).and_then(|rest| rest.find(']')) else {
            break;
        };
        let close = open + close + 1;
        let candidate = text.get(open..close).unwrap_or("");
        if known.contains_key(candidate) {
            if open > at {
                out.push((text.get(at..open).unwrap_or(""), false));
            }
            out.push((candidate, true));
            at = close;
        }
        search = open + 1;
    }
    if at < text.len() {
        out.push((text.get(at..).unwrap_or(""), false));
    }
    out
}

/// PRIVACY-GUARD-1: restore a text that uses a guard's tokens for `actor`.
///
/// # Errors
/// An invalid body (400), an unknown guard (404), a denied gate (403) or a
/// host failure.
#[utoipa::path(post, path = "/v1/guard/{id}:restore", operation_id = "guard_restore", params(ScopeQuery, ("id" = u64, Path, description = "The guardId of the guarded prompt.")), request_body = NornRestoreRequest, responses((status = 200, description = "The text with each token restored where the reader may reveal it (each a logged reveal), else projected or kept.", body = NornRestored), (status = 400, description = "The request was refused as invalid (`request.invalid`).", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused (`guard.not_found`).", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn restore<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    guard_id: u64,
    request: &NornRestoreRequest,
    access: &dyn ObjectAccess,
) -> Result<NornRestored, Refusal> {
    if request.text.chars().count() > MAX_PROMPT_CHARS * 2 {
        return Err(bad("text is too long"));
    }
    let key = request.idempotency_key.as_str();
    if key.is_empty() || key.len() > 96 {
        return Err(bad("idempotencyKey must be 1..=96 bytes"));
    }
    let actor = actor.ok_or_else(crate::authz::read_denied)?;
    scope_gate(host, scope, actor)?;
    let (event, catalog) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        (
            guarded_at(&reader, guard_id)?.ok_or_else(guard_not_found)?,
            host.marking_catalog(&reader)?,
        )
    };
    let known: BTreeMap<&str, &Entry> = event
        .tokens
        .iter()
        .map(|entry| (entry.token.as_str(), entry))
        .collect();
    let mut segments = Vec::new();
    let mut reveals = Vec::new();
    let mut complete = true;
    let mut revealed: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (piece, is_token) in pieces(&request.text, &known) {
        if !is_token {
            segments.push(NornRestoredSegment {
                text: piece.to_owned(),
                token: None,
                kind: None,
                state: None,
            });
            continue;
        }
        let Some(entry) = known.get(piece) else {
            continue;
        };
        let (text, state) = if let Some(done) = revealed.get(piece) {
            done.clone()
        } else {
            let done = restore_one(host, (actor, access), &catalog, entry, &event, key, &mut reveals)?;
            revealed.insert(piece.to_owned(), done.clone());
            done
        };
        if state != "restored" {
            complete = false;
        }
        segments.push(NornRestoredSegment {
            text,
            token: Some(piece.to_owned()),
            kind: Some(entry.kind.clone()),
            state: Some(state),
        });
    }
    Ok(NornRestored {
        guard_id,
        segments,
        reveals,
        complete,
    })
}

fn scope_gate<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
) -> Result<(), Refusal> {
    // The guard is a member's metadata-level call: the same grant as
    // reading the cell's definitions, in the cell's own scope.
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())
        .map_err(|_| crate::authz::read_denied())?;
    host.authorizer
        .authorize(&crate::authz::AccessRequest {
            scope: host.scope(),
            actor: Some(actor),
            operation: crate::authz::Operation::ReadDefinitions,
        })
        .map_err(|_| crate::authz::read_denied())
}

/// One token for `actor`: `(text, state)`.
fn restore_one<E: SqlEngine>(
    host: &DevHost<E>,
    (actor, access): (&str, &dyn ObjectAccess),
    catalog: &MarkingCatalog,
    entry: &Entry,
    event: &Guarded,
    key: &str,
    reveals: &mut Vec<u64>,
) -> Result<(String, String), Refusal> {
    let kept = || Ok((entry.token.clone(), "kept".to_owned()));
    match entry.source {
        NornGuardSource::Detected => match &entry.literal {
            // The author typed it; nobody else gets it back.
            Some(literal) if event.actor == actor => Ok((literal.clone(), "restored".to_owned())),
            _ => kept(),
        },
        NornGuardSource::Field => {
            let (Some(object_type), Some(id), Some(property)) =
                (&entry.object_type, &entry.id, &entry.property)
            else {
                return kept();
            };
            let row = {
                let cell = host.cell()?;
                let reader = cell.store().read();
                reader
                    .object(MAIN_WORLD, object_type, id)
                    .map_err(Refusal::host)?
            };
            let Some(mut row) = row else {
                return kept();
            };
            if !row_readable(host, access, actor, &row) {
                return kept();
            }
            let markings = entry.markings.clone();
            let allowed = !markings.is_empty()
                && markings.iter().all(|marking| host.reveals(actor, marking));
            if allowed && let Some(value) = row.values.get(property).and_then(text_of) {
                let receipt = privacy::record_reveal(
                    host,
                    actor,
                    &row,
                    (property, markings),
                    &format!("restore guarded model reply {}", entry.token),
                    &format!("{key}:{}", entry.token),
                )?;
                reveals.push(receipt.commit_seq);
                return Ok((value, "restored".to_owned()));
            }
            // Everyone else reads the field's projection.
            let mask = privacy::strongest(catalog, &entry.markings)
                .unwrap_or(norn_ir::MaskPolicy::Refuse {});
            match row.values.get_mut(property) {
                Some(value) => {
                    let mode = privacy::apply(&mask, host.pseudonym_key(), value);
                    if mode == privacy::NornMaskMode::Refuse {
                        kept()
                    } else {
                        Ok((
                            text_of(value).unwrap_or_else(|| entry.token.clone()),
                            "projected".to_owned(),
                        ))
                    }
                }
                None => kept(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{detect, luhn};

    #[test]
    fn the_detector_finds_emails_phones_and_luhn_valid_cards() {
        let text = "Reach sofia.r@fastmail.com or +1 415 555 0132; card 4539 1488 0343 6467, order 20241024.";
        let kinds: Vec<&str> = detect(text).into_iter().map(|(kind, _, _)| kind).collect();
        assert_eq!(kinds, ["EMAIL", "PHONE", "CARD"]);
        assert!(luhn("4539148803436467"));
        assert!(!luhn("4539148803436468"));
    }
}
