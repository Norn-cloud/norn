//! File uploads (#921): a person brings a dataset. The bytes go from the
//! browser straight to S3-compatible storage through a URL this cell signs;
//! the cell then reads the object back, records its URI and SHA-256, profiles
//! it (CSV), and, once the person accepts the column types, installs a source
//! schema, an object type and a mapping for it as the next governed
//! definitions version and loads every row through its own mapping path,
//! with lineage to the object and the row.
//!
//! UPLOAD-SIGN-1: storage credentials are server configuration; only this
//! module reads them, and they leave the cell only inside a SigV4 signature.
//! UPLOAD-DIGEST-1: the digest is computed by the cell from the bytes it
//! read, and a load re-reads the object and refuses it if the digest changed.
//! UPLOAD-NULL-1 (agreed with the missingness work): an empty or whitespace
//! cell, quoted or not, is absent (null), never `""` or `0`; a column with
//! any absent cell is nullable; typing ignores absent cells; no sentinel
//! (`NA`, `null`, `-`) is guessed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_cell::MappingOutcome;
use norn_cell::engine::SqlEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::host::DevHost;
use crate::pull::{PullProfile, Row, observation};

/// The largest object a load reads, in bytes.
pub const MAX_OBJECT_BYTES: u64 = 20 * 1024 * 1024;
/// The most rows one upload loads.
pub const MAX_ROWS: usize = 200_000;
/// The most columns one upload declares.
pub const MAX_COLUMNS: usize = 200;
/// How long a signed URL stays valid, in seconds.
pub const URL_SECONDS: u32 = 900;
/// The source system uploads are observed as.
pub const UPLOAD_SOURCE: &str = "upload";

/// S3-compatible storage the cell signs for. Server configuration.
#[derive(Clone)]
pub struct S3Config {
    /// `https://host[:port]`, without a path.
    pub endpoint: String,
    /// The bucket.
    pub bucket: String,
    /// The `SigV4` region (`auto` for R2).
    pub region: String,
    access_key_id: String,
    secret_access_key: String,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// Which bucket an upload goes to; the server decides, never the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// Datasets and other files (`NORN_S3_BUCKET`).
    Uploads,
    /// Scanned documents: images and PDFs (`NORN_S3_DOCUMENTS_BUCKET`).
    Documents,
}

impl Store {
    /// The store a file of `content_type` belongs in.
    #[must_use]
    pub fn for_content_type(content_type: &str) -> Self {
        let kind = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if kind.starts_with("image/") || kind == "application/pdf" {
            Self::Documents
        } else {
            Self::Uploads
        }
    }
}

impl S3Config {
    /// The storage of `store` named by `vars`: `NORN_S3_ENDPOINT` and
    /// `NORN_S3_REGION` (shared), then `NORN_S3_BUCKET` with
    /// `NORN_SECRET_S3_ACCESS_KEY_ID` / `NORN_SECRET_S3_SECRET_ACCESS_KEY`,
    /// or `NORN_S3_DOCUMENTS_BUCKET` with
    /// `NORN_SECRET_S3_DOCUMENTS_ACCESS_KEY_ID` /
    /// `NORN_SECRET_S3_DOCUMENTS_SECRET_ACCESS_KEY`. `None` when not configured.
    #[must_use]
    pub fn from_vars(vars: &BTreeMap<String, String>, store: Store) -> Option<Self> {
        let get = |name: &str| vars.get(name).filter(|v| !v.is_empty()).cloned();
        let endpoint = get("NORN_S3_ENDPOINT")?;
        let host = endpoint.strip_prefix("https://")?;
        if host.is_empty() || host.contains(['/', '?', '@', '#']) {
            return None;
        }
        let (bucket, id, secret) = match store {
            Store::Uploads => (
                "NORN_S3_BUCKET",
                "NORN_SECRET_S3_ACCESS_KEY_ID",
                "NORN_SECRET_S3_SECRET_ACCESS_KEY",
            ),
            Store::Documents => (
                "NORN_S3_DOCUMENTS_BUCKET",
                "NORN_SECRET_S3_DOCUMENTS_ACCESS_KEY_ID",
                "NORN_SECRET_S3_DOCUMENTS_SECRET_ACCESS_KEY",
            ),
        };
        Some(Self {
            endpoint,
            bucket: get(bucket)?,
            region: get("NORN_S3_REGION").unwrap_or_else(|| "auto".to_owned()),
            access_key_id: get(id)?,
            secret_access_key: get(secret)?,
        })
    }

    #[cfg(test)]
    fn fixed(endpoint: &str, bucket: &str, region: &str, id: &str, secret: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            bucket: bucket.to_owned(),
            region: region.to_owned(),
            access_key_id: id.to_owned(),
            secret_access_key: secret.to_owned(),
        }
    }

    /// A seal of `text` under this store's secret (HMAC-SHA256, 128 bits as
    /// hex): proves the cell issued an upload id, without keeping it.
    #[must_use]
    pub fn seal(&self, text: &str) -> String {
        hex(&hmac(self.secret_access_key.as_bytes(), text.as_bytes()))
            .chars()
            .take(32)
            .collect()
    }

    /// The object's URI as the cell records it.
    #[must_use]
    pub fn uri(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    /// A `SigV4` query-signed URL for `method` on `key`, valid for
    /// `expires` seconds from `now_ms` (path-style addressing).
    #[must_use]
    pub fn presign(&self, method: &str, key: &str, now_ms: u64, expires: u32) -> String {
        let host = self.endpoint.trim_start_matches("https://");
        let path = format!("/{}/{}", encode(&self.bucket, false), encode(key, true));
        presign(&Presign {
            method,
            host,
            path: &path,
            region: &self.region,
            access_key_id: &self.access_key_id,
            secret_access_key: &self.secret_access_key,
            now_ms,
            expires,
        })
        .map_or_else(String::new, |query| {
            format!("{}{path}?{query}", self.endpoint)
        })
    }
}

struct Presign<'a> {
    method: &'a str,
    host: &'a str,
    path: &'a str,
    region: &'a str,
    access_key_id: &'a str,
    secret_access_key: &'a str,
    now_ms: u64,
    expires: u32,
}

/// The signed query string (`SigV4`, `UNSIGNED-PAYLOAD`, only `host` signed).
fn presign(p: &Presign<'_>) -> Option<String> {
    let at = chrono::DateTime::from_timestamp_millis(i64::try_from(p.now_ms).ok()?)?;
    let stamp = at.format("%Y%m%dT%H%M%SZ").to_string();
    let date = at.format("%Y%m%d").to_string();
    let scope = format!("{date}/{}/s3/aws4_request", p.region);
    let mut query = BTreeMap::from([
        ("X-Amz-Algorithm".to_owned(), "AWS4-HMAC-SHA256".to_owned()),
        (
            "X-Amz-Credential".to_owned(),
            format!("{}/{scope}", p.access_key_id),
        ),
        ("X-Amz-Date".to_owned(), stamp.clone()),
        ("X-Amz-Expires".to_owned(), p.expires.to_string()),
        ("X-Amz-SignedHeaders".to_owned(), "host".to_owned()),
    ]);
    let canonical_query = query
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false)))
        .collect::<Vec<_>>()
        .join("&");
    let request = format!(
        "{}\n{}\n{canonical_query}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
        p.method, p.path, p.host
    );
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(&Sha256::digest(request.as_bytes()))
    );
    let mut key = hmac(
        format!("AWS4{}", p.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    for part in [p.region, "s3", "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    query.insert(
        "X-Amz-Signature".to_owned(),
        hex(&hmac(&key, to_sign.as_bytes())),
    );
    Some(
        query
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k, false), encode(v, false)))
            .collect::<Vec<_>>()
            .join("&"),
    )
}

/// HMAC-SHA256 (RFC 2104).
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
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

/// `bytes` as lowercase hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        // Writing to a String cannot fail.
        let _written = write!(out, "{byte:02x}");
    }
    out
}

/// URI-encode per `SigV4`: unreserved characters stay; `/` stays when asked.
fn encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && byte == b'/')
        {
            out.push(char::from(byte));
        } else {
            let _written = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The SHA-256 of `bytes`, as lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// How a column is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    /// Whole numbers.
    Integer,
    /// Exact decimals.
    Decimal,
    /// `true` / `false`.
    Boolean,
    /// RFC 3339 date-times.
    Datetime,
    /// Anything else.
    Text,
}

impl ColumnKind {
    const fn candidate(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Boolean => "boolean",
            // Text the mapping converts with `timestamp()`, as the packs do.
            Self::Datetime | Self::Text => "string",
        }
    }
}

/// One profiled column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    /// The header as written.
    pub name: String,
    /// Its property id: lowercase, `[a-z][a-z0-9_]*`, unique.
    pub id: String,
    /// The inferred type.
    pub inferred: ColumnKind,
    /// Whether any cell is absent.
    pub nullable: bool,
    /// How many cells are absent.
    pub null_count: u64,
    /// Up to three present values, as written.
    pub samples: Vec<String>,
    /// The largest decimal scale seen.
    #[serde(skip)]
    pub scale: u32,
}

/// A parsed CSV document: its columns and its rows of cells (`None` = absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The profiled columns, in order.
    pub columns: Vec<Column>,
    /// The rows; each row has one cell per column.
    pub rows: Vec<Vec<Option<String>>>,
}

/// Parse RFC 4180 CSV (comma, double quotes, CRLF or LF) with a header row.
///
/// # Errors
/// No header, an unterminated quote, a row with the wrong cell count, or a
/// bound passed.
pub fn parse_csv(text: &str) -> Result<Table, String> {
    let records = records(text)?;
    let mut records = records.into_iter();
    let header = records.next().ok_or("the file has no header row")?;
    if header.len() > MAX_COLUMNS {
        return Err(format!("more than {MAX_COLUMNS} columns"));
    }
    let mut seen = BTreeSet::new();
    let mut columns: Vec<Column> = header
        .iter()
        .map(|name| Column {
            name: name.trim().to_owned(),
            id: unique_id(name, &mut seen),
            inferred: ColumnKind::Integer,
            nullable: false,
            null_count: 0,
            samples: Vec::new(),
            scale: 0,
        })
        .collect();
    let mut rows = Vec::new();
    let mut present: Vec<bool> = vec![false; columns.len()];
    for (index, record) in records.enumerate() {
        if record.len() == 1 && record.first().is_some_and(|c| c.trim().is_empty()) {
            continue; // a blank line
        }
        if record.len() != columns.len() {
            return Err(format!(
                "row {} has {} cells, the header {}",
                index.saturating_add(2),
                record.len(),
                columns.len()
            ));
        }
        let mut cells = Vec::with_capacity(columns.len());
        for ((value, column), seen) in record
            .into_iter()
            .zip(columns.iter_mut())
            .zip(present.iter_mut())
        {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                column.nullable = true;
                column.null_count = column.null_count.saturating_add(1);
                cells.push(None);
                continue;
            }
            column.inferred = if *seen {
                widen(column.inferred, trimmed)
            } else {
                classify(trimmed)
            };
            *seen = true;
            if column.samples.len() < 3 {
                column.samples.push(trimmed.to_owned());
            }
            if let Some((_, fraction)) = trimmed
                .split_once('.')
                .filter(|_| number(trimmed).is_some())
            {
                column.scale = column
                    .scale
                    .max(u32::try_from(fraction.len()).unwrap_or(12).min(12));
            }
            cells.push(Some(trimmed.to_owned()));
        }
        rows.push(cells);
    }
    for (column, seen) in columns.iter_mut().zip(present) {
        if !seen {
            column.inferred = ColumnKind::Text;
        }
    }
    Ok(Table { columns, rows })
}

/// The records of RFC 4180 text, header included.
fn records(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut pending = false;
    while let Some(c) = chars.next() {
        pending = true;
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cell.push(c);
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() && !quoted => {
                in_quotes = true;
                quoted = true;
            }
            ',' => {
                record.push(std::mem::take(&mut cell));
                quoted = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                record.push(std::mem::take(&mut cell));
                quoted = false;
                records.push(std::mem::take(&mut record));
                pending = false;
                if records.len() > MAX_ROWS {
                    return Err(format!("more than {MAX_ROWS} rows"));
                }
            }
            other => cell.push(other),
        }
    }
    if in_quotes {
        return Err("a quoted cell is not closed".into());
    }
    if pending {
        record.push(cell);
        records.push(record);
    }
    Ok(records)
}

/// A plain JSON number: optional sign, digits, at most one point, no exponent.
fn number(value: &str) -> Option<norn_ir::Decimal> {
    let plain = value
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'.'));
    plain.then(|| norn_ir::Decimal::parse(value).ok()).flatten()
}

/// The kind of a column's first present value.
fn classify(value: &str) -> ColumnKind {
    if number(value).is_some() {
        if value.contains('.') {
            ColumnKind::Decimal
        } else {
            ColumnKind::Integer
        }
    } else if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
        ColumnKind::Boolean
    } else if chrono::DateTime::parse_from_rfc3339(value).is_ok() {
        ColumnKind::Datetime
    } else {
        ColumnKind::Text
    }
}

/// The narrowest kind that admits both `kind` and `value`.
fn widen(kind: ColumnKind, value: &str) -> ColumnKind {
    match (kind, classify(value)) {
        (ColumnKind::Integer, ColumnKind::Integer) => ColumnKind::Integer,
        (ColumnKind::Integer | ColumnKind::Decimal, ColumnKind::Integer | ColumnKind::Decimal) => {
            ColumnKind::Decimal
        }
        (ColumnKind::Boolean, ColumnKind::Boolean) => ColumnKind::Boolean,
        (ColumnKind::Datetime, ColumnKind::Datetime) => ColumnKind::Datetime,
        (
            ColumnKind::Integer
            | ColumnKind::Decimal
            | ColumnKind::Boolean
            | ColumnKind::Datetime
            | ColumnKind::Text,
            ColumnKind::Integer
            | ColumnKind::Decimal
            | ColumnKind::Boolean
            | ColumnKind::Datetime
            | ColumnKind::Text,
        ) => ColumnKind::Text,
    }
}

fn unique_id(name: &str, seen: &mut BTreeSet<String>) -> String {
    let mut id: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    id = id.trim_matches('_').to_owned();
    while id.contains("__") {
        id = id.replace("__", "_");
    }
    if id.is_empty() || !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id = format!("c_{id}");
    }
    if id == "row_id" {
        "row_id_column".clone_into(&mut id);
    }
    id.truncate(60);
    let mut candidate = id.clone();
    let mut n = 1_u32;
    while !seen.insert(candidate.clone()) {
        n = n.saturating_add(1);
        candidate = format!("{id}_{n}");
    }
    candidate
}

/// The person's accepted typing of an upload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadRequest {
    /// The object type's name without its namespace: `[A-Z][A-Za-z0-9]{0,59}`.
    pub type_name: String,
    /// Each column's accepted kind; a column left out is loaded as inferred.
    #[serde(default)]
    pub columns: Vec<ColumnChoice>,
    /// Column ids whose values together name one row (a declared unique key,
    /// beside the row id); each is an included, never-empty text or integer
    /// column, and no two rows share the tuple (UPLOAD-KEY-1). Empty: no key.
    #[serde(default)]
    pub key: Vec<String>,
    /// The SHA-256 `:complete` returned; a cell that no longer remembers the
    /// upload checks the object against it (UPLOAD-DIGEST-1).
    #[serde(default)]
    pub sha256: Option<String>,
}

/// One column's accepted kind, or its exclusion.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColumnChoice {
    /// The column id.
    pub id: String,
    /// The accepted kind.
    #[serde(rename = "type")]
    pub kind: ColumnKind,
    /// `false` leaves the column out.
    #[serde(default = "yes")]
    pub include: bool,
    /// `true` declares an integer or decimal column additive, so the cell may
    /// sum it (UPLOAD-MEASURE-1); a measure is never part of the key.
    #[serde(default)]
    pub measure: bool,
}

const fn yes() -> bool {
    true
}

/// The definitions an accepted upload installs.
#[derive(Debug, Clone)]
pub struct Generated {
    /// The object type, e.g. `upload.Sales`.
    pub object_type: String,
    /// The source schema id.
    pub source_schema: String,
    /// The mapping id.
    pub mapping: String,
    /// The envelopes: interface, entity, source schema, mapping.
    pub artifacts: Vec<ArtifactEnvelope>,
    /// The loaded columns, in order, with their accepted kinds.
    pub columns: Vec<(Column, ColumnKind)>,
}

fn envelope(value: Value) -> Result<ArtifactEnvelope, String> {
    serde_json::from_value(value).map_err(|e| format!("generated envelope: {e}"))
}

/// Generate the definitions for `table` as `request` accepts it, read from
/// `uri` with digest `sha256` at `observed_at` (RFC 3339).
///
/// # Errors
/// An invalid type name, an unknown column, or an envelope that does not decode.
pub fn generate(
    table: &Table,
    request: &LoadRequest,
    uri: &str,
    sha256: &str,
    observed_at: &str,
) -> Result<Generated, String> {
    let name = &request.type_name;
    let valid = name.len() <= 60
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid {
        return Err("the type name is a capitalized letters-and-digits name of at most 60".into());
    }
    let choices: BTreeMap<&str, &ColumnChoice> =
        request.columns.iter().map(|c| (c.id.as_str(), c)).collect();
    if let Some(unknown) = choices
        .keys()
        .find(|id| !table.columns.iter().any(|c| c.id == **id))
    {
        return Err(format!("no column {unknown}"));
    }
    let columns: Vec<(Column, ColumnKind)> = table
        .columns
        .iter()
        .filter_map(|column| match choices.get(column.id.as_str()) {
            Some(choice) if !choice.include => None,
            Some(choice) => Some((column.clone(), choice.kind)),
            None => Some((column.clone(), column.inferred)),
        })
        .collect();
    let measures = measures_of(request, &columns)?;
    let key = key_of(table, &columns, &request.key, &measures)?;
    let snake = name.to_ascii_lowercase();
    let object_type = format!("upload.{name}");
    let interface = format!("upload.{name}Record");
    let source_schema = format!("upload.{snake}.source");
    let mapping = format!("upload.{snake}.mapping");
    let (properties, fields, map_properties) = shapes(&columns, &measures);
    let mut identity = vec![json!({"name": "primary", "properties": ["row_id"]})];
    if !key.is_empty() {
        identity.push(json!({"name": "key", "properties": key}));
    }
    let base = |id: &str, kind: &str, title: &str, payload: Value, dependencies: Value| {
        json!({"artifactId": id, "dependencies": dependencies, "irVersion": "0.1", "kind": kind, "lifecycle": "active",
            "name": title, "namespace": "upload", "payload": payload, "provenance": {"source": "system"}, "scope": {}, "version": "0.1.0"})
    };
    let interface_envelope = envelope(base(
        &format!("{interface}.interface"),
        "entity",
        &format!("{name} rows"),
        json!({"entityVersion": "2.0", "interface": {"name": interface, "properties": [{"id": "row_id", "type": {"kind": "text"}}]}}),
        json!([]),
    ))?;
    let entity = envelope(base(
        &format!("{object_type}.entity"),
        "entity",
        &format!("{name} (uploaded)"),
        json!({"entityVersion": "2.0", "objectType": {"identity": identity,
            "implements": [interface], "links": [], "name": object_type, "properties": properties}}),
        json!([]),
    ))?;
    let source = envelope(base(
        &source_schema,
        "source_schema",
        &format!("{name} upload"),
        json!({"format": "norn.source-schema/1", "source": {"system": UPLOAD_SOURCE, "instance": "s3", "version": "csv/1"},
            "observedAt": observed_at, "adapter": {"name": "norn-upload", "version": "1"},
            "rawMetadata": {"uri": uri, "sha256": sha256},
            "datasets": [{"id": "rows", "nativeName": uri, "kind": "document"}], "fields": fields,
            "constraints": [{"id": "row_id", "kind": "primary_key", "dataset": "rows", "fields": ["row_id"], "origin": "declared"}]}),
        json!([]),
    ))?;
    let pin = norn_checker::ArtifactRef::of(&source)
        .map_err(|e| e.to_string())?
        .sha256;
    let mapping_envelope = envelope(base(
        &mapping,
        "mapping",
        &format!("{name} rows to {object_type}"),
        json!({"format": "norn.mapping/1", "extensions": {"norn.mapping.generic/1.0.0": {"parity": "unqualified"}},
            "source": {"schemaId": source_schema, "version": "0.1.0", "sha256": pin},
            "interfaces": [format!("{interface}.interface")],
            "maps": [{"id": "row", "source": {"dataset": "rows"}, "target": {"entity": format!("{object_type}.entity"), "interface": format!("{interface}.interface")},
                "subject": "input.row_id", "sourceRecord": "input.row_id", "properties": map_properties}]}),
        json!([
            {"artifactId": source_schema, "kind": "source_schema", "required": true, "version": "0.1.0"},
            {"artifactId": format!("{interface}.interface"), "kind": "entity", "required": true, "version": "0.1.0"},
            {"artifactId": format!("{object_type}.entity"), "kind": "entity", "required": true, "version": "0.1.0"}
        ]),
    ))?;
    Ok(Generated {
        object_type,
        source_schema,
        mapping,
        artifacts: vec![interface_envelope, entity, source, mapping_envelope],
        columns,
    })
}

/// UPLOAD-MEASURE-1: the included columns declared measures, once each is
/// an integer or a decimal.
fn measures_of<'a>(
    request: &'a LoadRequest,
    columns: &[(Column, ColumnKind)],
) -> Result<BTreeSet<&'a str>, String> {
    let measures: BTreeSet<&str> = request
        .columns
        .iter()
        .filter(|c| c.include && c.measure)
        .map(|c| c.id.as_str())
        .collect();
    match columns.iter().find(|(column, kind)| {
        measures.contains(column.id.as_str())
            && !matches!(kind, ColumnKind::Integer | ColumnKind::Decimal)
    }) {
        Some((column, _)) => Err(format!("{} is a measure but not a number", column.id)),
        None => Ok(measures),
    }
}

/// UPLOAD-KEY-1: the declared key's column ids, once each names an included,
/// never-empty text or integer column that is not a measure, and no two rows
/// share the key's values.
fn key_of(
    table: &Table,
    columns: &[(Column, ColumnKind)],
    key: &[String],
    measures: &BTreeSet<&str>,
) -> Result<Vec<String>, String> {
    if key.is_empty() {
        return Ok(Vec::new());
    }
    let mut positions = Vec::with_capacity(key.len());
    let mut named = BTreeSet::new();
    for id in key {
        let Some((column, kind)) = columns.iter().find(|(c, _)| &c.id == id) else {
            return Err(format!("the key column {id} is not an included column"));
        };
        if !named.insert(id.as_str()) {
            return Err(format!("the key names {id} twice"));
        }
        if !matches!(kind, ColumnKind::Text | ColumnKind::Integer) {
            return Err(format!("the key column {id} is not text or a whole number"));
        }
        if column.nullable {
            return Err(format!("the key column {id} has empty cells"));
        }
        if measures.contains(id.as_str()) {
            return Err(format!("the key column {id} is a measure"));
        }
        let index = table
            .columns
            .iter()
            .position(|c| &c.id == id)
            .ok_or_else(|| format!("no column {id}"))?;
        positions.push(index);
    }
    let mut seen = BTreeMap::new();
    for (number, row) in (1_usize..).zip(&table.rows) {
        let tuple: Vec<Option<&String>> = positions
            .iter()
            .map(|index| row.get(*index).and_then(Option::as_ref))
            .collect();
        if let Some(first) = seen.insert(tuple, number) {
            return Err(format!("rows {first} and {number} share the key"));
        }
    }
    Ok(key.to_vec())
}

/// Each loaded column as an object property, a source field and a mapping
/// property, after the row id.
fn shapes(
    columns: &[(Column, ColumnKind)],
    measures: &BTreeSet<&str>,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let property_type = |kind: ColumnKind, scale: u32, measure: bool| match (kind, measure) {
        (ColumnKind::Integer, false) => json!({"kind": "integer"}),
        (ColumnKind::Integer, true) => {
            json!({"kind": "integer", "additivity": {"kind": "additive"}})
        }
        (ColumnKind::Decimal, false) => json!({"kind": "decimal", "scale": scale.max(1)}),
        (ColumnKind::Decimal, true) => {
            json!({"kind": "decimal", "scale": scale.max(1), "additivity": {"kind": "additive"}})
        }
        (ColumnKind::Boolean, _) => json!({"kind": "boolean"}),
        (ColumnKind::Datetime, _) => json!({"kind": "ref", "name": "iso8601.timestamp"}),
        (ColumnKind::Text, _) => json!({"kind": "text"}),
    };
    let mut properties = vec![json!({"id": "row_id", "type": {"kind": "text"}})];
    let mut fields = vec![
        json!({"dataset": "rows", "name": "row_id", "ordinal": 0, "nativeType": "csv.row", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true}),
    ];
    let mut map_properties = vec![json!({"property": "row_id", "value": "input.row_id"})];
    for (ordinal, (column, kind)) in (1_u32..).zip(columns) {
        // A nullable date-time stays text: the mapping has no null-safe timestamp conversion.
        let kind = &if *kind == ColumnKind::Datetime && column.nullable {
            ColumnKind::Text
        } else {
            *kind
        };
        let measure = measures.contains(column.id.as_str());
        let mut property =
            json!({"id": column.id, "type": property_type(*kind, column.scale, measure)});
        if column.nullable
            && let Some(object) = property.as_object_mut()
        {
            object.insert("constraints".to_owned(), json!({"nullable": true}));
        }
        properties.push(property);
        fields.push(json!({"dataset": "rows", "name": column.id, "ordinal": ordinal, "nativeType": "csv",
            "typeCandidate": {"type": kind.candidate(), "origin": "declared"}, "required": !column.nullable}));
        let input = format!("input.{}", column.id);
        let value = match (kind, column.nullable) {
            (ColumnKind::Datetime, true) => format!("{input} == null ? null : timestamp({input})"),
            (ColumnKind::Datetime, false) => format!("timestamp({input})"),
            (
                ColumnKind::Integer | ColumnKind::Decimal | ColumnKind::Boolean | ColumnKind::Text,
                _,
            ) => input,
        };
        map_properties.push(json!({"property": column.id, "value": value}));
    }
    (properties, fields, map_properties)
}

/// Install `generated` with the active set as the next governed definitions
/// version; an earlier install of the same upload type is replaced.
///
/// # Errors
/// The checker's refusal (its diagnostics in the message) or a store failure.
pub fn install<E: SqlEngine>(
    host: &DevHost<E>,
    generated: &Generated,
    installed_at: u64,
) -> Result<u64, String> {
    let active = host
        .active_definitions()
        .map_err(|r| r.message)?
        .ok_or("the cell has no definitions to extend")?;
    let base = crate::kernel_set::base_ids(active.admission_receipt.as_deref())?;
    let ours: BTreeSet<&str> = generated
        .artifacts
        .iter()
        .map(|a| a.artifact_id.as_str())
        .collect();
    let mut artifacts: Vec<ArtifactEnvelope> =
        crate::kernel_set::retained_artifacts(&active.canonical_set)?
            .into_iter()
            .filter(|a| !ours.contains(a.artifact_id.as_str()))
            .collect();
    artifacts.extend(generated.artifacts.iter().cloned());
    let admitted =
        crate::kernel_set::admit_kernel_set_with_base(&artifacts, &base).map_err(|refusal| {
            match refusal {
                crate::kernel_set::KernelRefusal::Refused(findings) => findings
                    .iter()
                    .map(|f| format!("{}: {}", f.code, f.message))
                    .collect::<Vec<_>>()
                    .join("; "),
                crate::kernel_set::KernelRefusal::Invalid(detail) => detail,
            }
        })?;
    let installed = host
        .install_definitions(
            admitted.model,
            norn_cell::InstalledDefinitionInput {
                canonical_set: admitted.canonical_set,
                registry: vec![],
                receipt_sha256: admitted.receipt_sha256,
                admission_receipt: Some(admitted.receipt),
                readmission: None,
                diff: json!({}),
                installed_at,
                layout: None,
            },
            Some(active.version),
        )
        .map_err(|r| format!("{}: {}", r.code, r.message))?;
    Ok(installed.version)
}

/// What loading an upload did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Loaded {
    /// Rows read from the object.
    pub rows_read: u64,
    /// Rows in the cell.
    pub rows_landed: u64,
    /// Rows held back as quarantined evidence.
    pub rows_quarantined: u64,
    /// Why, by code.
    pub quarantine_reasons: BTreeMap<String, u64>,
}

/// Load every row of `table` through `generated`'s mapping, each row's
/// lineage `<uri>#row=N` (N counts data rows from 1).
///
/// # Errors
/// A row the cell cannot capture.
pub fn load_rows<E: SqlEngine>(
    host: &DevHost<E>,
    generated: &Generated,
    table: &Table,
    uri: &str,
    sha256: &str,
    now_ms: u64,
) -> Result<Loaded, String> {
    let profile = PullProfile {
        source: UPLOAD_SOURCE.to_owned(),
        pipeline: format!("norn-upload/1+sha256.{sha256}"),
        feeds: Vec::new(),
        token: None,
    };
    let indices: Vec<Option<usize>> = generated
        .columns
        .iter()
        .map(|(column, _)| table.columns.iter().position(|c| c.id == column.id))
        .collect();
    let recorded_at = norn_ir::provenance::UtcTimestamp(crate::sources::rfc3339(now_ms));
    let mut loaded = Loaded {
        rows_read: 0,
        rows_landed: 0,
        rows_quarantined: 0,
        quarantine_reasons: BTreeMap::new(),
    };
    for (chunk_start, chunk) in (0_usize..).step_by(500).zip(table.rows.chunks(500)) {
        let mut batch = Vec::with_capacity(chunk.len());
        for (offset, cells) in chunk.iter().enumerate() {
            let number = chunk_start.saturating_add(offset).saturating_add(1);
            let row_id = format!("{uri}#row={number}");
            let mut payload =
                BTreeMap::from([("row_id".to_owned(), RuntimeValue::String(row_id.clone()))]);
            for ((column, kind), index) in generated.columns.iter().zip(&indices) {
                let cell = index.and_then(|i| cells.get(i)).cloned().flatten();
                payload.insert(column.id.clone(), typed(cell.as_deref(), *kind));
            }
            let row = Row {
                dataset: "rows".to_owned(),
                source_id: row_id,
                version: sha256.to_owned(),
                payload: RuntimeValue::Object(payload),
            };
            let raw = observation(&profile, &row, now_ms)?;
            batch.push((
                raw,
                MappingInput {
                    mapping: generated.mapping.clone(),
                    dataset: "rows".to_owned(),
                    connector_instance: "s3".to_owned(),
                    partition: uri.to_owned(),
                    recorded_at: recorded_at.clone(),
                },
            ));
        }
        loaded.rows_read = loaded
            .rows_read
            .saturating_add(u64::try_from(batch.len()).unwrap_or(u64::MAX));
        for ingested in host.ingest(&batch).map_err(|r| r.message)? {
            match ingested.outcome {
                MappingOutcome::Landed | MappingOutcome::AlreadyLanded => {
                    loaded.rows_landed = loaded.rows_landed.saturating_add(1);
                }
                MappingOutcome::Quarantined(codes) => {
                    loaded.rows_quarantined = loaded.rows_quarantined.saturating_add(1);
                    for code in codes {
                        let n = loaded.quarantine_reasons.entry(code).or_insert(0);
                        *n = n.saturating_add(1);
                    }
                }
            }
        }
    }
    Ok(loaded)
}

/// A cell as its accepted kind; a value that does not parse stays text, so
/// the mapping refuses it and the row is quarantined with the reason.
fn typed(cell: Option<&str>, kind: ColumnKind) -> RuntimeValue {
    let Some(text) = cell else {
        return RuntimeValue::Null;
    };
    let parsed = match kind {
        ColumnKind::Integer | ColumnKind::Decimal => number(text).map(RuntimeValue::Number),
        ColumnKind::Boolean => {
            if text.eq_ignore_ascii_case("true") {
                Some(RuntimeValue::Bool(true))
            } else if text.eq_ignore_ascii_case("false") {
                Some(RuntimeValue::Bool(false))
            } else {
                None
            }
        }
        ColumnKind::Datetime | ColumnKind::Text => None,
    };
    parsed.unwrap_or_else(|| RuntimeValue::String(text.to_owned()))
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "upload scenarios"
)]
mod tests {
    use super::*;

    /// The AWS `SigV4` query-auth example (`GET /test.txt`, 2013-05-24, us-east-1).
    #[test]
    fn presign_matches_the_published_sigv4_example() {
        let query = presign(&Presign {
            method: "GET",
            host: "examplebucket.s3.amazonaws.com",
            path: "/test.txt",
            region: "us-east-1",
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            now_ms: 1_369_353_600_000,
            expires: 86_400,
        })
        .expect("signed");
        assert!(
            query.contains(
                "X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
            ),
            "{query}"
        );
    }

    #[test]
    fn presigned_urls_are_path_style_on_the_configured_endpoint() {
        let s3 = S3Config::fixed("https://store.example", "uploads", "auto", "id", "secret");
        let url = s3.presign(
            "PUT",
            "uploads/org/env/u1/sales 2026.csv",
            1_794_484_800_000,
            URL_SECONDS,
        );
        assert!(url.starts_with("https://store.example/uploads/uploads/org/env/u1/sales%202026.csv?X-Amz-Algorithm=AWS4-HMAC-SHA256&"), "{url}");
        assert!(url.contains("X-Amz-Expires=900"));
        assert_eq!(s3.uri("k"), "s3://uploads/k");
        assert_eq!(Store::for_content_type("image/png"), Store::Documents);
        assert_eq!(
            Store::for_content_type("application/pdf; charset=binary"),
            Store::Documents
        );
        assert_eq!(Store::for_content_type("text/csv"), Store::Uploads);
    }

    #[test]
    fn empty_cells_are_absent_and_typing_ignores_them() {
        let table = parse_csv("Price,Qty,When,Note,Flag\r\n1.5,3,2026-01-01T00:00:00Z,\"a, b\",true\r\n,4,,\"\",false\r\n2,,2026-02-01T00:00:00Z,x,\n").expect("parsed");
        let kinds: Vec<_> = table
            .columns
            .iter()
            .map(|c| (c.id.as_str(), c.inferred, c.nullable, c.null_count))
            .collect();
        assert_eq!(
            kinds,
            [
                ("price", ColumnKind::Decimal, true, 1),
                ("qty", ColumnKind::Integer, true, 1),
                ("when", ColumnKind::Datetime, true, 1),
                ("note", ColumnKind::Text, true, 1),
                ("flag", ColumnKind::Boolean, true, 1),
            ]
        );
        assert_eq!(table.rows[0][3].as_deref(), Some("a, b"));
        assert_eq!(table.rows[1][0], None);
        assert_eq!(
            typed(Some("1.50"), ColumnKind::Decimal),
            RuntimeValue::from_json_text("1.50").expect("n")
        );
        assert_eq!(typed(None, ColumnKind::Integer), RuntimeValue::Null);
        assert_eq!(
            typed(Some("NA"), ColumnKind::Integer),
            RuntimeValue::String("NA".into())
        );
    }

    #[test]
    fn a_ragged_row_is_refused() {
        assert!(parse_csv("a,b\n1\n").is_err());
        assert!(parse_csv("a,b\n\"1,2\n").is_err());
    }

    fn choice(id: &str, kind: ColumnKind, measure: bool) -> ColumnChoice {
        ColumnChoice {
            id: id.into(),
            kind,
            include: true,
            measure,
        }
    }

    fn entity_of(generated: &Generated) -> Value {
        serde_json::to_value(generated.artifacts[1].payload.to_json_value()).expect("payload")
    }

    /// UPLOAD-MEASURE-1 and UPLOAD-KEY-1: a measure declares its property
    /// additive; a key is a second unique identity beside the row id.
    #[test]
    fn measures_are_additive_and_the_key_is_a_unique_identity() {
        let csv = "sku,qty,price,note\nA-1,3,2.50,x\nA-2,4,1.25,\n";
        let table = parse_csv(csv).expect("parsed");
        let sha = sha256_hex(csv.as_bytes());
        let request = |columns: Vec<ColumnChoice>, key: Vec<&str>| LoadRequest {
            type_name: "Stock".into(),
            columns,
            key: key.into_iter().map(Into::into).collect(),
            sha256: None,
        };
        let generated = generate(
            &table,
            &request(
                vec![
                    choice("qty", ColumnKind::Integer, true),
                    choice("price", ColumnKind::Decimal, true),
                ],
                vec!["sku"],
            ),
            "u",
            &sha,
            "2026-11-12T12:00:00.000Z",
        )
        .expect("generated");
        let entity = entity_of(&generated);
        let object = &entity["objectType"];
        assert_eq!(
            object["identity"],
            json!([{"name": "primary", "properties": ["row_id"]}, {"name": "key", "properties": ["sku"]}])
        );
        let property = |id: &str| {
            object["properties"]
                .as_array()
                .expect("properties")
                .iter()
                .find(|p| p["id"] == id)
                .cloned()
                .expect("property")
        };
        assert_eq!(
            property("qty")["type"]["additivity"],
            json!({"kind": "additive"})
        );
        assert_eq!(
            property("price")["type"]["additivity"],
            json!({"kind": "additive"})
        );
        assert_eq!(property("sku")["type"].get("additivity"), None);

        let refused = |columns: Vec<ColumnChoice>, key: Vec<&str>| {
            generate(&table, &request(columns, key), "u", &sha, "t").expect_err("refused")
        };
        assert!(
            refused(vec![choice("sku", ColumnKind::Text, true)], vec![]).contains("not a number")
        );
        assert!(refused(vec![], vec!["note"]).contains("empty cells"));
        assert!(refused(vec![], vec!["price"]).contains("not text or a whole number"));
        assert!(
            refused(vec![choice("qty", ColumnKind::Integer, true)], vec!["qty"])
                .contains("is a measure")
        );
        assert!(refused(vec![], vec!["sku", "sku"]).contains("twice"));
        assert!(refused(vec![], vec!["nope"]).contains("not an included column"));
        let mut excluded = choice("sku", ColumnKind::Text, false);
        excluded.include = false;
        assert!(refused(vec![excluded], vec!["sku"]).contains("not an included column"));

        let duplicated = parse_csv("sku,qty\nA,1\nB,2\nA,3\n").expect("parsed");
        let reason = generate(&duplicated, &request(vec![], vec!["sku"]), "u", &sha, "t")
            .expect_err("refused");
        assert_eq!(reason, "rows 1 and 3 share the key");
        // No key: rows are told apart by their row ids alone.
        assert!(generate(&duplicated, &request(vec![], vec![]), "u", &sha, "t").is_ok());
    }

    #[test]
    fn generated_definitions_pin_their_source_schema() {
        let table = parse_csv("x,y\n1,2.5\n").expect("parsed");
        let generated = generate(
            &table,
            &LoadRequest {
                type_name: "Points".into(),
                columns: vec![],
                key: vec![],
                sha256: None,
            },
            "s3://uploads/k.csv",
            &sha256_hex(b"x,y\n1,2.5\n"),
            "2026-11-12T12:00:00.000Z",
        )
        .expect("generated");
        assert_eq!(generated.object_type, "upload.Points");
        assert_eq!(generated.artifacts.len(), 4);
        let source = &generated.artifacts[2];
        let pin = norn_checker::ArtifactRef::of(source).expect("pin").sha256;
        let mapping =
            serde_json::to_value(generated.artifacts[3].payload.to_json_value()).expect("payload");
        assert_eq!(mapping["source"]["sha256"], pin);
        assert!(
            generate(
                &table,
                &LoadRequest {
                    type_name: "lower".into(),
                    columns: vec![],
                    key: vec![],
                    sha256: None,
                },
                "u",
                "h",
                "t"
            )
            .is_err()
        );
    }
}
