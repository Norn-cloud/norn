//! The generic pull engine (`norn.pull/1`, #921): one sans-IO adapter for every
//! HTTP source, driven by a declared profile carried in the source's
//! `connector` artifact at `extensions["norn.pull/1"]`.
//!
//! The engine knows no vendor. A profile names, per source-schema dataset, the
//! request to send (method, path, query and JSON body templates), where the
//! items are in a response, how pages advance (a keyset over the source's own
//! version and id, or a server cursor), and the JSON pointer of every dataset
//! field inside an item. The host owns the network, the clock and the
//! credentials: it sends each [`Request`] the engine plans (adding the
//! connector's declared auth) and hands back the status and body. The engine
//! never sees a secret.
//!
//! Templates read these tokens. A string that is exactly one token is
//! replaced by that value with its JSON type (`"{{id}}"` becomes `42` when the
//! position's id is a number, `"{{cursor}}"` becomes `null` on a first page);
//! inside a longer string a token is replaced by its text, and `{{json:x}}` by
//! its JSON literal (quoted and escaped), for sources that take JSON inside a
//! query parameter.
//!
//! | token | value |
//! |---|---|
//! | `version`, `id` | the keyset position: the stored watermark, then the last row of each page |
//! | `cursor` | the server cursor (`null` on the first page) |
//! | `size` | the declared page size |
//!
//! PULL-WATERMARK-1: a keyset feed asks for rows after `(version, id)` in
//! `(version, id)` order and advances to the last row of each page, so a row
//! that changes while the feed runs moves behind the position and is read
//! again, never skipped. A cursor feed reads rows at or after the stored
//! version; its next watermark is the greatest version it saw. Rows read twice
//! are the same observation (same source, id and version) and the cell
//! captures them once.
//! PULL-BOUND-1: every feed stops after [`Limits::max_pages`] pages and every
//! page after [`Limits::max_items`] items; a cursor or keyset that does not
//! advance fails the feed instead of looping.
//! PULL-FIELD-1: a payload carries exactly the declared fields, in the source
//! schema's names; an absent pointer is `null`, which the mapping refuses or
//! accepts by the schema's own `required`. A `text:` pointer projects a
//! scalar as its text (a numeric source id as the text identity a schema
//! declares).

use std::collections::BTreeMap;

use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The connector extension key of a pull profile.
pub const PULL_EXTENSION_KEY: &str = "norn.pull/1";

/// A declared pull profile: the source it observes and its feeds, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PullProfile {
    /// The source system every observation names (the source schema's
    /// `source.system`).
    pub source: String,
    /// The extract pipeline and its version, recorded on every observation.
    pub pipeline: String,
    /// One feed per source-schema dataset, pulled in this order.
    pub feeds: Vec<Feed>,
}

/// One dataset's feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Feed {
    /// The source-schema dataset the rows belong to.
    pub dataset: String,
    /// The request template.
    pub request: RequestTemplate,
    /// RFC 6901 pointer to the item array in a response.
    pub items_path: String,
    /// How pages advance.
    pub paging: Paging,
    /// Pointer, inside an item, to the record's identity in the source.
    pub id_path: String,
    /// Pointer, inside an item, to the source's own version (its
    /// last-modified value); it orders the keyset and is the watermark.
    pub version_path: String,
    /// Dataset field name to its pointer inside an item.
    pub fields: BTreeMap<String, String>,
    /// The position before the first sync.
    pub initial: Watermark,
}

/// An HTTP request with templated parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestTemplate {
    /// `GET` or `POST`; a pull never writes, so `POST` is only for sources
    /// whose read queries take a body.
    pub method: Method,
    /// A path relative to the connection's base URL, starting with `/`.
    pub path: String,
    /// Query parameters; values are templates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, String>,
    /// Fixed, non-secret headers (for example a content type).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// A JSON body template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// The two request methods a pull may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    /// A read.
    #[serde(rename = "GET")]
    Get,
    /// A read whose query is a body.
    #[serde(rename = "POST")]
    Post,
}

impl Method {
    /// The method's name on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// How a feed's pages advance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Paging {
    /// Rows after `(version, id)`, ordered by them; a short page ends the feed.
    Keyset {
        /// Rows per page.
        size: u32,
    },
    /// A server cursor over rows at or after the stored version.
    Cursor {
        /// Rows per page.
        size: u32,
        /// Pointer to the next cursor in a response.
        next_cursor_path: String,
        /// Pointer to the boolean that says another page exists.
        has_next_path: String,
    },
}

impl Paging {
    const fn size(&self) -> u32 {
        match self {
            Self::Keyset { size } | Self::Cursor { size, .. } => *size,
        }
    }
}

/// A feed position: the source's own version and the record id that breaks
/// ties among rows sharing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Watermark {
    /// The version (for example a last-modified timestamp), as the source writes it.
    pub version: String,
    /// The tie-breaking id, typed as the source types it (text or number).
    pub id: Value,
}

/// Bounds on one pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Pages per feed.
    pub max_pages: u32,
    /// Items per page.
    pub max_items: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pages: 1000,
            max_items: 1000,
        }
    }
}

/// One planned request. The host adds the connector's auth and sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// The dataset the request reads.
    pub dataset: String,
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The path and encoded query, relative to the base URL.
    pub path: String,
    /// Fixed non-secret headers.
    pub headers: Vec<(String, String)>,
    /// The JSON body, when the request has one.
    pub body: Option<String>,
}

/// One pulled row, projected onto its dataset's declared fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The dataset.
    pub dataset: String,
    /// The record's identity in the source, as text.
    pub source_id: String,
    /// The source's version of the record.
    pub version: String,
    /// The declared fields only, keyed by the dataset's field names.
    pub payload: RuntimeValue,
}

/// Why a pull stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullError {
    /// The profile is not a valid `norn.pull/1` profile.
    Profile(String),
    /// The source answered with a status other than 200.
    Status {
        /// The feed's dataset.
        dataset: String,
        /// The HTTP status.
        status: u16,
    },
    /// The response is not the declared shape.
    Shape {
        /// The feed's dataset.
        dataset: String,
        /// What was wrong.
        reason: String,
    },
    /// A bound was reached or a position did not advance.
    Bound {
        /// The feed's dataset.
        dataset: String,
        /// Which bound.
        reason: String,
    },
    /// [`Pull::accept`] was called with no request outstanding.
    NotWaiting,
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Profile(reason) => write!(f, "invalid pull profile: {reason}"),
            Self::Status { dataset, status } => {
                write!(f, "the source refused the read of {dataset}: HTTP {status}")
            }
            Self::Shape { dataset, reason } => {
                write!(
                    f,
                    "the source's {dataset} response is not the declared shape: {reason}"
                )
            }
            Self::Bound { dataset, reason } => write!(f, "the {dataset} feed stopped: {reason}"),
            Self::NotWaiting => f.write_str("no request is outstanding"),
        }
    }
}

impl std::error::Error for PullError {}

impl PullProfile {
    /// The profile in a connector payload's `extensions`, if it declares one.
    ///
    /// # Errors
    /// A present extension that is not a valid profile.
    pub fn from_connector(payload: &Value) -> Result<Option<Self>, PullError> {
        let Some(value) = payload
            .get("extensions")
            .and_then(|extensions| extensions.get(PULL_EXTENSION_KEY))
        else {
            return Ok(None);
        };
        let profile: Self = serde_json::from_value(value.clone())
            .map_err(|error| PullError::Profile(error.to_string()))?;
        profile.validate()?;
        Ok(Some(profile))
    }

    /// Check what serde cannot.
    ///
    /// # Errors
    /// Blank names, a relative path, duplicate datasets, a zero page size or
    /// a pointer that is not RFC 6901.
    pub fn validate(&self) -> Result<(), PullError> {
        let bad = |reason: String| Err(PullError::Profile(reason));
        if self.source.trim().is_empty() || self.pipeline.trim().is_empty() {
            return bad("source and pipeline must be nonblank".into());
        }
        if self.feeds.is_empty() {
            return bad("a profile declares at least one feed".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for feed in &self.feeds {
            if !seen.insert(feed.dataset.as_str()) {
                return bad(format!("dataset {} has two feeds", feed.dataset));
            }
            if !feed.request.path.starts_with('/') || feed.request.path.contains("://") {
                return bad(format!(
                    "{}: the path must be relative and start with '/'",
                    feed.dataset
                ));
            }
            if feed.paging.size() == 0 {
                return bad(format!("{}: the page size must be positive", feed.dataset));
            }
            if feed.fields.is_empty() {
                return bad(format!("{}: a feed declares its fields", feed.dataset));
            }
            let mut pointers = vec![&feed.items_path, &feed.id_path, &feed.version_path];
            if let Paging::Cursor {
                next_cursor_path,
                has_next_path,
                ..
            } = &feed.paging
            {
                pointers.push(next_cursor_path);
                pointers.push(has_next_path);
            }
            pointers.extend(feed.fields.values());
            if let Some(pointer) = pointers.iter().find(|p| {
                let p = p.strip_prefix("text:").unwrap_or(p);
                !p.is_empty() && !p.starts_with('/')
            }) {
                return bad(format!("{}: {pointer} is not a JSON pointer", feed.dataset));
            }
        }
        Ok(())
    }
}

/// One pull over a profile: plan a request, accept its response, repeat.
#[derive(Debug, Clone)]
pub struct Pull {
    profile: PullProfile,
    limits: Limits,
    feed: usize,
    page: u32,
    position: Watermark,
    cursor: Value,
    /// The greatest version a cursor feed has seen.
    greatest: Option<String>,
    waiting: bool,
    watermarks: BTreeMap<String, Watermark>,
}

impl Pull {
    /// Start a pull from stored watermarks (absent datasets start at their
    /// declared initial position).
    #[must_use]
    pub fn new(
        profile: PullProfile,
        watermarks: BTreeMap<String, Watermark>,
        limits: Limits,
    ) -> Self {
        let mut pull = Self {
            profile,
            limits,
            feed: 0,
            page: 0,
            position: Watermark {
                version: String::new(),
                id: Value::Null,
            },
            cursor: Value::Null,
            greatest: None,
            waiting: false,
            watermarks,
        };
        pull.enter_feed();
        pull
    }

    fn enter_feed(&mut self) {
        if let Some(feed) = self.profile.feeds.get(self.feed) {
            self.position = self
                .watermarks
                .get(&feed.dataset)
                .cloned()
                .unwrap_or_else(|| feed.initial.clone());
            self.page = 0;
            self.cursor = Value::Null;
            self.greatest = None;
        }
    }

    /// The profile this pull runs.
    #[must_use]
    pub const fn profile(&self) -> &PullProfile {
        &self.profile
    }

    /// The watermark of every dataset, including those this pull advanced.
    #[must_use]
    pub const fn watermarks(&self) -> &BTreeMap<String, Watermark> {
        &self.watermarks
    }

    /// The next request to send, or `None` when every feed is done.
    #[must_use]
    pub fn next_request(&mut self) -> Option<Request> {
        let feed = self.profile.feeds.get(self.feed)?;
        let tokens = Tokens {
            version: &self.position.version,
            id: &self.position.id,
            cursor: &self.cursor,
            size: feed.paging.size(),
        };
        let mut query = form_urlencoded_pairs(&feed.request.query, &tokens);
        let path = if query.is_empty() {
            feed.request.path.clone()
        } else {
            query.insert(0, '?');
            format!("{}{query}", feed.request.path)
        };
        let body = feed
            .request
            .body
            .as_ref()
            .map(|body| tokens.fill(body).to_string());
        self.waiting = true;
        Some(Request {
            dataset: feed.dataset.clone(),
            method: feed.request.method.as_str(),
            path,
            headers: feed
                .request
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            body,
        })
    }

    /// Accept the response to the outstanding request: its rows, projected.
    ///
    /// # Errors
    /// A refused status, a response that is not the declared shape, or a
    /// bound reached. The pull stops; its watermarks keep the last completed
    /// page's position.
    pub fn accept(&mut self, status: u16, body: &[u8]) -> Result<Vec<Row>, PullError> {
        if !self.waiting {
            return Err(PullError::NotWaiting);
        }
        self.waiting = false;
        let Some(feed) = self.profile.feeds.get(self.feed) else {
            return Err(PullError::NotWaiting);
        };
        let dataset = || feed.dataset.clone();
        let shape = |reason: String| PullError::Shape {
            dataset: dataset(),
            reason,
        };
        if status != 200 {
            return Err(PullError::Status {
                dataset: dataset(),
                status,
            });
        }
        let response = RuntimeValue::from_json_text(body).map_err(|e| shape(e.to_string()))?;
        let Some(RuntimeValue::Array(items)) = response.pointer(&feed.items_path) else {
            return Err(shape(format!("no item array at {}", feed.items_path)));
        };
        if items.len() > self.limits.max_items {
            return Err(PullError::Bound {
                dataset: dataset(),
                reason: format!("a page held more than {} items", self.limits.max_items),
            });
        }
        let mut rows = Vec::with_capacity(items.len());
        let mut last = None;
        for item in items {
            let id = item
                .pointer(&feed.id_path)
                .ok_or_else(|| shape(format!("an item has no id at {}", feed.id_path)))?;
            let source_id = scalar_text(id).ok_or_else(|| {
                shape(format!(
                    "the id at {} is not text or a number",
                    feed.id_path
                ))
            })?;
            let version = item
                .pointer(&feed.version_path)
                .and_then(RuntimeValue::as_str)
                .ok_or_else(|| {
                    shape(format!(
                        "an item has no version text at {}",
                        feed.version_path
                    ))
                })?
                .to_owned();
            let payload = feed
                .fields
                .iter()
                .map(|(field, pointer)| (field.clone(), project(item, pointer)))
                .collect();
            if self.greatest.as_ref().is_none_or(|g| version > *g) {
                self.greatest = Some(version.clone());
            }
            last = Some((version.clone(), json_of(id)));
            rows.push(Row {
                dataset: feed.dataset.clone(),
                source_id,
                version,
                payload: RuntimeValue::Object(payload),
            });
        }
        self.page = self.page.saturating_add(1);
        let done = match &feed.paging {
            Paging::Keyset { size } => {
                if let Some((version, id)) = last {
                    let next = Watermark { version, id };
                    if next == self.position {
                        return Err(PullError::Bound {
                            dataset: dataset(),
                            reason: "the keyset position did not advance".into(),
                        });
                    }
                    self.position = next;
                }
                items.len() < usize::try_from(*size).unwrap_or(usize::MAX)
            }
            Paging::Cursor {
                next_cursor_path,
                has_next_path,
                ..
            } => {
                let more = matches!(
                    response.pointer(has_next_path),
                    Some(RuntimeValue::Bool(true))
                );
                if more {
                    let next = response
                        .pointer(next_cursor_path)
                        .and_then(RuntimeValue::as_str)
                        .ok_or_else(|| shape(format!("no next cursor at {next_cursor_path}")))?;
                    if self.cursor.as_str() == Some(next) {
                        return Err(PullError::Bound {
                            dataset: dataset(),
                            reason: "the server cursor did not advance".into(),
                        });
                    }
                    self.cursor = Value::String(next.to_owned());
                }
                !more
            }
        };
        if done {
            let finished = match &feed.paging {
                Paging::Keyset { .. } => self.position.clone(),
                Paging::Cursor { .. } => Watermark {
                    version: self
                        .greatest
                        .clone()
                        .unwrap_or_else(|| self.position.version.clone()),
                    id: self.position.id.clone(),
                },
            };
            self.watermarks.insert(feed.dataset.clone(), finished);
            self.feed = self.feed.saturating_add(1);
            self.enter_feed();
        } else if self.page >= self.limits.max_pages {
            return Err(PullError::Bound {
                dataset: dataset(),
                reason: format!("more than {} pages", self.limits.max_pages),
            });
        } else if matches!(feed.paging, Paging::Keyset { .. }) {
            // A completed keyset page is durable progress, even if a later page fails.
            self.watermarks
                .insert(feed.dataset.clone(), self.position.clone());
        }
        Ok(rows)
    }
}

/// Seal a pulled row as the raw observation (`norn.raw_observation/v1`) the
/// cell's mapping path takes, observed at `extracted_at_ms`. The source's own
/// version is the event identity, so a changed record is a new observation
/// and a re-read of the same version is the same one.
///
/// # Errors
/// A payload that cannot be hashed.
pub fn observation(
    profile: &PullProfile,
    row: &Row,
    extracted_at_ms: u64,
) -> Result<norn_kernel::Observed<RuntimeValue>, String> {
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(&row.payload).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(norn_kernel::Observed {
        id: norn_kernel::ObservationId(format!(
            "{}/{}/{}@{}",
            profile.source, row.dataset, row.source_id, row.version
        )),
        provenance: norn_kernel::Provenance {
            source: profile.source.clone(),
            source_id: row.source_id.clone(),
            mapping_version: profile.pipeline.clone(),
            observed_at: norn_kernel::LogicalTime::from_millis(extracted_at_ms),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => profile.source.as_str(),
            "source_id" => row.source_id.as_str(),
            "event_id" => row.version.as_str(),
            "source_version" => row.version.as_str(),
            "pipeline_version" => profile.pipeline.as_str(),
            "kind" => "upsert",
            "extracted_at" => extracted_at_ms,
            "payload" => row.payload.clone(),
            "payload_digest" => digest,
        },
    })
}

/// One field of an item: the value at its pointer (`null` when absent), or,
/// for a `text:` pointer, that scalar's text (a source's numeric id becomes
/// the text identity a source schema declares).
fn project(item: &RuntimeValue, pointer: &str) -> RuntimeValue {
    match pointer.strip_prefix("text:") {
        Some(pointer) => item
            .pointer(pointer)
            .and_then(scalar_text)
            .map_or(RuntimeValue::Null, RuntimeValue::String),
        None => item.pointer(pointer).cloned().unwrap_or_default(),
    }
}

/// A scalar's text: a string as is, a number as its exact decimal.
fn scalar_text(value: &RuntimeValue) -> Option<String> {
    match value {
        RuntimeValue::String(text) => (!text.is_empty()).then(|| text.clone()),
        RuntimeValue::Number(number) => Some(number.to_string()),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => None,
    }
}

/// The JSON of a keyset id, kept as the source typed it.
fn json_of(value: &RuntimeValue) -> Value {
    match value {
        RuntimeValue::Number(number) => number
            .to_string()
            .parse::<serde_json::Number>()
            .map_or(Value::Null, Value::Number),
        RuntimeValue::String(text) => Value::String(text.clone()),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => Value::Null,
    }
}

struct Tokens<'t> {
    version: &'t str,
    id: &'t Value,
    cursor: &'t Value,
    size: u32,
}

impl Tokens<'_> {
    fn value(&self, name: &str) -> Option<Value> {
        match name {
            "version" => Some(Value::String(self.version.to_owned())),
            "id" => Some(self.id.clone()),
            "cursor" => Some(self.cursor.clone()),
            "size" => Some(Value::from(self.size)),
            _ => None,
        }
    }

    fn text(&self, name: &str) -> Option<String> {
        if let Some(inner) = name.strip_prefix("json:") {
            return self.value(inner).map(|value| value.to_string());
        }
        self.value(name).map(|value| match value {
            Value::String(text) => text,
            Value::Null => String::new(),
            Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
                value.to_string()
            }
        })
    }

    /// A template string: a lone token keeps its JSON type; otherwise each
    /// token is spliced in as text. Unknown tokens stay as written.
    fn fill_str(&self, template: &str) -> Value {
        if let Some(name) = template
            .strip_prefix("{{")
            .and_then(|rest| rest.strip_suffix("}}"))
            .filter(|name| !name.contains("{{") && !name.starts_with("json:"))
            && let Some(value) = self.value(name)
        {
            return value;
        }
        Value::String(self.splice(template))
    }

    fn splice(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some((before, after)) = rest.split_once("{{") {
            out.push_str(before);
            match after
                .split_once("}}")
                .and_then(|(name, tail)| Some((self.text(name)?, tail)))
            {
                Some((text, tail)) => {
                    out.push_str(&text);
                    rest = tail;
                }
                None => {
                    out.push_str("{{");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn fill(&self, template: &Value) -> Value {
        match template {
            Value::String(text) => self.fill_str(text),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.fill(v)).collect()),
            Value::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), self.fill(value)))
                    .collect(),
            ),
            Value::Null | Value::Bool(_) | Value::Number(_) => template.clone(),
        }
    }
}

/// `application/x-www-form-urlencoded` query text, in declared key order.
fn form_urlencoded_pairs(query: &BTreeMap<String, String>, tokens: &Tokens<'_>) -> String {
    let mut out = String::new();
    for (key, template) in query {
        if !out.is_empty() {
            out.push('&');
        }
        encode_component(key, &mut out);
        out.push('=');
        encode_component(&tokens.splice(template), &mut out);
    }
    out
}

fn encode_component(text: &str, out: &mut String) {
    const HEX: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
    ];
    let digit = |nibble: u8| HEX.get(usize::from(nibble)).copied().unwrap_or('0');
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(digit(byte >> 4));
            out.push(digit(byte & 0x0F));
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "synthetic pull scenarios"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn keyset_profile(size: u32) -> PullProfile {
        serde_json::from_value(serde_json::json!({
            "source": "test", "pipeline": "test/1",
            "feeds": [{
                "dataset": "Item",
                "request": {"method": "POST", "path": "/items", "body": {"after": ["{{version}}", "{{id}}"], "limit": "{{size}}", "q": "v>={{version}}", "j": "{{json:version}}"}},
                "itemsPath": "/data", "paging": {"kind": "keyset", "size": size},
                "idPath": "/id", "versionPath": "/v",
                "fields": {"code": "/code", "missing": "/nope"},
                "initial": {"version": "", "id": 0}
            }]
        }))
        .expect("profile")
    }

    /// A source whose rows are `(version, id)` pairs; a keyset read returns
    /// rows after the requested position in order.
    fn serve(rows: &[(String, u64)], request: &Request) -> Vec<u8> {
        let body: Value =
            serde_json::from_str(request.body.as_deref().expect("body")).expect("json");
        let version = body["after"][0].as_str().expect("version").to_owned();
        let id = body["after"][1].as_u64().expect("id");
        let limit = usize::try_from(body["limit"].as_u64().expect("limit")).expect("limit");
        let mut sorted = rows.to_vec();
        sorted.sort();
        let page: Vec<Value> = sorted
            .into_iter()
            .filter(|(v, i)| (v.as_str(), *i) > (version.as_str(), id))
            .take(limit)
            .map(|(v, i)| serde_json::json!({"id": i, "v": v, "code": format!("C{i}")}))
            .collect();
        serde_json::json!({ "data": page }).to_string().into_bytes()
    }

    #[test]
    fn templates_keep_lone_token_types_and_splice_text() {
        let profile = keyset_profile(2);
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let request = pull.next_request().expect("request");
        let body: Value =
            serde_json::from_str(request.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["after"], serde_json::json!(["", 0]));
        assert_eq!(body["limit"], serde_json::json!(2));
        assert_eq!(body["q"], serde_json::json!("v>="));
        assert_eq!(body["j"], serde_json::json!("\"\""));
    }

    #[test]
    fn query_values_are_form_encoded_and_json_tokens_quoted() {
        let profile: PullProfile = serde_json::from_value(serde_json::json!({
            "source": "test", "pipeline": "test/1",
            "feeds": [{
                "dataset": "Item",
                "request": {"method": "GET", "path": "/api/resource/Item", "query": {"filters": "[[\"modified\",\">=\",{{json:version}}]]"}},
                "itemsPath": "/data", "paging": {"kind": "keyset", "size": 1},
                "idPath": "/name", "versionPath": "/modified", "fields": {"name": "/name"},
                "initial": {"version": "a\"b", "id": ""}
            }]
        }))
        .expect("profile");
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let request = pull.next_request().expect("request");
        assert_eq!(
            request.path,
            "/api/resource/Item?filters=%5B%5B%22modified%22%2C%22%3E%3D%22%2C%22a%5C%22b%22%5D%5D"
        );
    }

    #[test]
    fn a_refused_status_stops_without_moving_the_watermark() {
        let profile = keyset_profile(2);
        let stored = BTreeMap::from([(
            "Item".to_owned(),
            Watermark {
                version: "5".into(),
                id: Value::from(3),
            },
        )]);
        let mut pull = Pull::new(profile.clone(), stored.clone(), Limits::default());
        let _request = pull.next_request().expect("request");
        assert!(matches!(
            pull.accept(401, b"{}"),
            Err(PullError::Status { status: 401, .. })
        ));
        assert_eq!(pull.watermarks(), &stored);
    }

    #[test]
    fn projection_keeps_exact_decimals_and_nulls_absent_fields() {
        let profile = keyset_profile(5);
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let _request = pull.next_request().expect("request");
        let rows = pull
            .accept(200, br#"{"data":[{"id":7,"v":"1","code":9000.10}]}"#)
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_id, "7");
        assert_eq!(
            rows[0].payload.get("code").map(RuntimeValue::canonical),
            Some("9000.10".into())
        );
        assert_eq!(rows[0].payload.get("missing"), Some(&RuntimeValue::Null));
        assert_eq!(
            project(
                &RuntimeValue::from_json_text(r#"{"id":7}"#).expect("item"),
                "text:/id"
            ),
            RuntimeValue::String("7".into())
        );
        assert!(pull.next_request().is_none());
        assert_eq!(
            pull.watermarks()["Item"],
            Watermark {
                version: "1".into(),
                id: Value::from(7)
            }
        );
    }

    #[test]
    fn a_cursor_feed_reads_every_page_and_keeps_the_greatest_version() {
        let profile: PullProfile = serde_json::from_value(serde_json::json!({
            "source": "test", "pipeline": "test/1",
            "feeds": [{
                "dataset": "Variant",
                "request": {"method": "POST", "path": "/graphql", "body": {"variables": {"after": "{{cursor}}", "q": "updated_at:>='{{version}}'", "n": "{{size}}"}}},
                "itemsPath": "/data/v/nodes", "paging": {"kind": "cursor", "size": 2, "nextCursorPath": "/data/v/pageInfo/endCursor", "hasNextPath": "/data/v/pageInfo/hasNextPage"},
                "idPath": "/id", "versionPath": "/updatedAt", "fields": {"id": "/id"},
                "initial": {"version": "1970-01-01T00:00:00Z", "id": ""}
            }]
        }))
        .expect("profile");
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let first = pull.next_request().expect("first");
        let body: Value = serde_json::from_str(first.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["variables"]["after"], Value::Null);
        assert_eq!(
            body["variables"]["q"],
            "updated_at:>='1970-01-01T00:00:00Z'"
        );
        let rows = pull
            .accept(200, br#"{"data":{"v":{"nodes":[{"id":"a","updatedAt":"2026-11-02T00:00:00Z"},{"id":"b","updatedAt":"2026-11-03T00:00:00Z"}],"pageInfo":{"hasNextPage":true,"endCursor":"c1"}}}}"#)
            .expect("page 1");
        assert_eq!(rows.len(), 2);
        let second = pull.next_request().expect("second");
        let body: Value =
            serde_json::from_str(second.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["variables"]["after"], "c1");
        pull.accept(200, br#"{"data":{"v":{"nodes":[{"id":"c","updatedAt":"2026-11-01T00:00:00Z"}],"pageInfo":{"hasNextPage":false,"endCursor":"c2"}}}}"#)
            .expect("page 2");
        assert!(pull.next_request().is_none());
        assert_eq!(pull.watermarks()["Variant"].version, "2026-11-03T00:00:00Z");
    }

    #[test]
    fn a_profile_with_an_absolute_url_is_refused() {
        let mut profile = keyset_profile(1);
        profile.feeds[0].request.path = "https://elsewhere.example/items".into();
        assert!(matches!(profile.validate(), Err(PullError::Profile(_))));
    }

    proptest! {
        /// PULL-WATERMARK-1: whatever the rows, page size and one row moved
        /// to a newer version between pages, a keyset pull reads every
        /// row's final version exactly as a full scan would, and the next
        /// pull from its watermark reads nothing.
        #[test]
        fn keyset_pull_never_skips_a_row_that_moves(
            versions in prop::collection::vec(0u32..20, 1..30),
            size in 1u32..6,
            moved in any::<prop::sample::Index>(),
            after_page in 0usize..4,
        ) {
            let profile = keyset_profile(size);
            let mut rows: Vec<(String, u64)> = versions.iter().enumerate()
                .map(|(i, v)| (format!("{v:03}"), u64::try_from(i).expect("id") + 1)).collect();
            let moved = moved.index(rows.len());
            let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
            let mut seen = std::collections::BTreeSet::new();
            let mut page = 0;
            while let Some(request) = pull.next_request() {
                if page == after_page {
                    rows[moved].0 = "999".into();
                }
                for row in pull.accept(200, &serve(&rows, &request)).expect("page") {
                    seen.insert((row.version.clone(), row.source_id.clone()));
                }
                page += 1;
            }
            for (version, id) in &rows {
                prop_assert!(seen.contains(&(version.clone(), id.to_string())), "missed {id}@{version}");
            }
            let mut again = Pull::new(profile.clone(), pull.watermarks().clone(), Limits::default());
            let request = again.next_request().expect("request");
            prop_assert!(again.accept(200, &serve(&rows, &request)).expect("page").is_empty());
        }
    }
}
