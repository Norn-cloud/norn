//! The generic pull engine (`norn.pull/1`, #921): one sans-IO adapter for every
//! HTTP source, driven by a declared profile carried in the source's
//! `connector` artifact at `extensions["norn.pull/1"]`.
//!
//! The engine knows no vendor. A profile names, per source-schema dataset, the
//! request to send (method, path, query and JSON body templates), where the
//! items are in a response, how pages advance (a keyset over the source's own
//! version and id, or a server cursor), and the JSON pointer of every dataset
//! field inside an item. The host owns the network, the clock and the
//! credentials: it sends each [`Request`](crate::pull::Request) the engine plans (adding the
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
//! PULL-BOUND-1: every feed stops after [`Limits::max_pages`](crate::pull::Limits::max_pages) pages and every
//! page after [`Limits::max_items`](crate::pull::Limits::max_items) items; a cursor or keyset that does not
//! advance fails the feed instead of looping.
//! PULL-FIELD-1: a payload carries exactly the declared fields, in the source
//! schema's names; an absent pointer is `null`, which the mapping refuses or
//! accepts by the schema's own `required`. A `text:` pointer projects a
//! scalar as its text (a numeric source id as the text identity a schema
//! declares).

use std::collections::BTreeMap;

use norn_ir::{FeedMode, RuntimeValue, SourceVersion, VersionKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pull_feed::{
    Tokens, check_keyset_order, extract_version, form_urlencoded_pairs, project_patch, scalar_text,
    update_greatest_version,
};

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
    /// Where an `OAuth2` access token goes, for a connector whose auth is
    /// `oauth2` (client credentials).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<TokenPlacement>,
}

/// The header an `OAuth2` access token is sent in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenPlacement {
    /// The header name, e.g. `Authorization`.
    pub header: String,
    /// Text before the token, e.g. `Bearer `.
    #[serde(default)]
    pub prefix: String,
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
    /// Snapshot vs partial-patch delivery semantics (`None` adopts the mapping's
    /// `norn.feed/1` mode or defaults to `full_snapshot`; `Some` refuses
    /// contradictory mapping declarations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FeedMode>,
    /// Typed source-version parser and comparator (`None` adopts the mapping's
    /// `norn.feed/1` `versionKind` or defaults to `lexicographical`; `Some`
    /// refuses contradictory mapping declarations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_kind: Option<VersionKind>,
}

impl Feed {
    /// Effective delivery [`FeedMode`], defaulting to [`FeedMode::FullSnapshot`].
    #[must_use]
    pub fn effective_mode(&self) -> FeedMode {
        self.mode.unwrap_or_default()
    }

    /// Effective [`VersionKind`], defaulting to [`VersionKind::Lexicographical`].
    #[must_use]
    pub fn effective_version_kind(&self) -> VersionKind {
        self.version_kind.unwrap_or_default()
    }
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
        #[serde(deserialize_with = "whole")]
        size: u32,
    },
    /// A server cursor over rows at or after the stored version.
    Cursor {
        /// Rows per page.
        #[serde(deserialize_with = "whole")]
        size: u32,
        /// Pointer to the next cursor in a response.
        next_cursor_path: String,
        /// Pointer to the boolean that says another page exists.
        has_next_path: String,
    },
}

/// A whole number however a JSON reader spelled it (`100` or `100.0`).
fn whole<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = Value::deserialize(deserializer)?;
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| {
                    n.fract().abs() < f64::EPSILON && (0.0..=f64::from(u32::MAX)).contains(n)
                })
                .and_then(|n| format!("{n}").parse().ok())
        })
        .ok_or_else(|| serde::de::Error::custom("expected a whole number"))
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
    /// The tie-breaking id, kept as an exact [`RuntimeValue`] (preserving
    /// decimal precision without binary64 rounding).
    pub id: RuntimeValue,
}

impl Watermark {
    /// Deterministic JSON encoding used as the durable cursor value in
    /// `_norn_source_cursor`.
    ///
    /// # Errors
    /// Serialization failure.
    pub fn cursor_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Ephemeral cursor advancement produced by [`Pull::accept`].
///
/// SOURCE-CAPTURE-1: accepting an HTTP page never mutates durable watermarks;
/// the caller passes this advance into the cell's atomic page-capture
/// transaction and calls [`Pull::confirm`] only after durable commit succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAdvance {
    /// Dataset whose cursor is being advanced.
    pub dataset: String,
    /// Previously confirmed watermark for `dataset` (`None` if never committed).
    pub expected_previous: Option<Watermark>,
    /// Next durable watermark to commit atomically with this page (`None` for
    /// non-terminal cursor-paged pages where the durable watermark only
    /// advances once the feed completes).
    pub next: Option<Watermark>,
    /// Whether this page completed the dataset's feed (including empty terminal
    /// pages).
    pub terminal: bool,
}

/// One accepted HTTP page: its projected rows and ephemeral cursor advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedPage {
    /// Dataset this page belongs to.
    pub dataset: String,
    /// Projected rows (may be empty on a terminal page).
    pub rows: Vec<Row>,
    /// Ephemeral cursor advance; not recorded in [`Pull::watermarks`] until
    /// [`Pull::confirm`] is called after durable capture.
    pub advance: CursorAdvance,
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
        let encoded =
            serde_json::to_string(value).map_err(|error| PullError::Profile(error.to_string()))?;
        let profile: Self = serde_json::from_str(&encoded)
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
            let version_kind = feed.effective_version_kind();
            if !version_kind.is_orderable() {
                return bad(format!(
                    "{}: versionKind {:?} cannot order keyset or cursor watermarks",
                    feed.dataset,
                    version_kind.as_str()
                ));
            }
            if version_kind != VersionKind::Lexicographical
                && let Err(error) = SourceVersion::parse(version_kind, &feed.initial.version)
            {
                return bad(format!(
                    "{}: invalid initial version: {error}",
                    feed.dataset
                ));
            }
        }
        Ok(())
    }

    /// Declared [`FeedMode`] for `dataset`, defaulting to [`FeedMode::FullSnapshot`].
    #[must_use]
    pub fn feed_mode(&self, dataset: &str) -> FeedMode {
        self.feeds
            .iter()
            .find(|feed| feed.dataset == dataset)
            .map_or(FeedMode::FullSnapshot, Feed::effective_mode)
    }

    /// Declared [`VersionKind`] for `dataset`, defaulting to [`VersionKind::Lexicographical`].
    #[must_use]
    pub fn version_kind(&self, dataset: &str) -> VersionKind {
        self.feeds
            .iter()
            .find(|feed| feed.dataset == dataset)
            .map_or(VersionKind::Lexicographical, Feed::effective_version_kind)
    }

    /// Bind a mapping's `norn.feed/1` declaration onto this profile before
    /// projection and pagination, failing closed on explicit contradictions.
    ///
    /// # Errors
    /// A feed whose explicit `mode` or `versionKind` contradicts `declaration`,
    /// or whose resulting initial watermark / orderability is invalid.
    pub fn bind_feed_declaration(
        &mut self,
        declaration: &norn_ir::feed::FeedDeclaration,
    ) -> Result<(), PullError> {
        crate::pull_feed::bind_feed_declaration(self, declaration)
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
                id: RuntimeValue::Null,
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
            self.greatest =
                (!self.position.version.is_empty()).then(|| self.position.version.clone());
        }
    }

    /// The profile this pull runs.
    #[must_use]
    pub const fn profile(&self) -> &PullProfile {
        &self.profile
    }

    /// The durably confirmed watermark of every dataset.
    ///
    /// SOURCE-CAPTURE-1: [`Self::accept`] keeps fetched cursor progress
    /// ephemeral and never mutates this map until the caller confirms durable
    /// capture via [`Self::confirm`].
    #[must_use]
    pub const fn watermarks(&self) -> &BTreeMap<String, Watermark> {
        &self.watermarks
    }

    /// Confirm that a previously accepted page and its [`CursorAdvance`] were
    /// durably captured by the cell, updating [`Self::watermarks`].
    pub fn confirm(&mut self, advance: &CursorAdvance) {
        if let Some(next) = &advance.next {
            self.watermarks
                .insert(advance.dataset.clone(), next.clone());
        }
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
            .map(|body| tokens.fill_json(body));
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

    /// Accept the response to the outstanding request: its projected rows and
    /// an ephemeral [`CursorAdvance`]. Durable [`Self::watermarks`] stays
    /// unchanged until [`Self::confirm`] is called after atomic cell capture.
    ///
    /// # Errors
    /// A refused status, a response that is not the declared shape, or a
    /// bound reached. The pull stops; its confirmed watermarks stay at the
    /// last confirmed page's position.
    pub fn accept(&mut self, status: u16, body: &[u8]) -> Result<AcceptedPage, PullError> {
        if !self.waiting {
            return Err(PullError::NotWaiting);
        }
        self.waiting = false;
        let Some(feed) = self.profile.feeds.get(self.feed) else {
            return Err(PullError::NotWaiting);
        };
        let dataset_name = feed.dataset.clone();
        let shape = |reason: String| PullError::Shape {
            dataset: dataset_name.clone(),
            reason,
        };
        if status != 200 {
            return Err(PullError::Status {
                dataset: dataset_name,
                status,
            });
        }
        let response = RuntimeValue::from_json_text(body).map_err(|e| shape(e.to_string()))?;
        let Some(RuntimeValue::Array(items)) = response.pointer(&feed.items_path) else {
            return Err(shape(format!("no item array at {}", feed.items_path)));
        };
        if items.len() > self.limits.max_items {
            return Err(PullError::Bound {
                dataset: dataset_name,
                reason: format!("a page held more than {} items", self.limits.max_items),
            });
        }
        let (rows, ids) = project_page(feed, items)?;
        update_greatest_version(
            &mut self.greatest,
            feed.effective_version_kind(),
            &rows,
            &shape,
        )?;
        self.page = self.page.saturating_add(1);
        let done = self.advance_paging(&response, &rows, &ids, items.len())?;
        let expected_previous = self.watermarks.get(&dataset_name).cloned();
        let next = self.next_watermark(done, &dataset_name)?;
        Ok(AcceptedPage {
            dataset: dataset_name.clone(),
            rows,
            advance: CursorAdvance {
                dataset: dataset_name,
                expected_previous,
                next,
                terminal: done,
            },
        })
    }

    fn advance_paging(
        &mut self,
        response: &RuntimeValue,
        rows: &[Row],
        ids: &[RuntimeValue],
        items_len: usize,
    ) -> Result<bool, PullError> {
        let Some(feed) = self.profile.feeds.get(self.feed) else {
            return Err(PullError::NotWaiting);
        };
        let dataset_name = feed.dataset.clone();
        let dataset = || dataset_name.clone();
        let version_kind = feed.effective_version_kind();
        match &feed.paging {
            Paging::Keyset { size } => {
                if let Some((row, id)) = rows.last().zip(ids.last()) {
                    let next = Watermark {
                        version: row.version.clone(),
                        id: id.clone(),
                    };
                    if next == self.position {
                        return Err(PullError::Bound {
                            dataset: dataset_name,
                            reason: "the keyset position did not advance".into(),
                        });
                    }
                    check_keyset_order(version_kind, &self.position, rows, ids, &dataset)?;
                    self.position = next;
                }
                Ok(items_len < usize::try_from(*size).unwrap_or(usize::MAX))
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
                        .ok_or_else(|| PullError::Shape {
                            dataset: dataset_name.clone(),
                            reason: format!("no next cursor at {next_cursor_path}"),
                        })?;
                    if self.cursor.as_str() == Some(next) {
                        return Err(PullError::Bound {
                            dataset: dataset_name,
                            reason: "the server cursor did not advance".into(),
                        });
                    }
                    self.cursor = Value::String(next.to_owned());
                }
                Ok(!more)
            }
        }
    }

    fn next_watermark(
        &mut self,
        done: bool,
        dataset_name: &str,
    ) -> Result<Option<Watermark>, PullError> {
        let Some(feed) = self.profile.feeds.get(self.feed) else {
            return Err(PullError::NotWaiting);
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
            self.feed = self.feed.saturating_add(1);
            self.enter_feed();
            Ok(Some(finished))
        } else if self.page >= self.limits.max_pages {
            Err(PullError::Bound {
                dataset: dataset_name.to_owned(),
                reason: format!("more than {} pages", self.limits.max_pages),
            })
        } else if matches!(feed.paging, Paging::Keyset { .. }) {
            // A completed keyset page yields a durable cursor advance once
            // atomically captured by the cell.
            Ok(Some(self.position.clone()))
        } else {
            Ok(None)
        }
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
    let mut input = norn_ir::runtime_object! {
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
    };
    if let RuntimeValue::Object(map) = &mut input {
        if profile.feed_mode(&row.dataset) == FeedMode::Patch {
            map.insert(
                "feed_mode".to_owned(),
                RuntimeValue::String(FeedMode::Patch.as_str().to_owned()),
            );
        }
        let version_kind = profile.version_kind(&row.dataset);
        if version_kind != VersionKind::Lexicographical {
            map.insert(
                "version_kind".to_owned(),
                RuntimeValue::String(version_kind.as_str().to_owned()),
            );
        }
    }
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
        input,
    })
}

/// A page's rows and their exact keyset ids.
type Page = (Vec<Row>, Vec<RuntimeValue>);

fn project_page(feed: &Feed, items: &[RuntimeValue]) -> Result<Page, PullError> {
    let shape = |reason: String| PullError::Shape {
        dataset: feed.dataset.clone(),
        reason,
    };
    let mode = feed.effective_mode();
    let version_kind = feed.effective_version_kind();
    let mut rows = Vec::with_capacity(items.len());
    let mut ids = Vec::with_capacity(items.len());
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
        let version = extract_version(item, &feed.version_path, version_kind, &shape)?;
        let mut payload = BTreeMap::new();
        for (field, pointer) in &feed.fields {
            match mode {
                FeedMode::FullSnapshot => {
                    payload.insert(field.clone(), project(item, pointer));
                }
                FeedMode::Patch => {
                    if let Some(value) = project_patch(item, field, pointer, &shape)? {
                        payload.insert(field.clone(), value);
                    }
                }
            }
        }
        ids.push(exact_keyset_id(id));
        rows.push(Row {
            dataset: feed.dataset.clone(),
            source_id,
            version,
            payload: RuntimeValue::Object(payload),
        });
    }
    Ok((rows, ids))
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

/// Exact keyset id preserved as [`RuntimeValue`] (exact decimal or string).
fn exact_keyset_id(value: &RuntimeValue) -> RuntimeValue {
    match value {
        RuntimeValue::Number(number) => RuntimeValue::Number(*number),
        RuntimeValue::String(text) => RuntimeValue::String(text.clone()),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => RuntimeValue::Null,
    }
}

/// Accumulate one mapping outcome into landed / quarantined / reason counts.
pub(crate) fn tally_outcome(
    outcome: &norn_cell::MappingOutcome,
    landed: &mut u64,
    quarantined: &mut u64,
    reasons: &mut BTreeMap<String, u64>,
) {
    match outcome {
        norn_cell::MappingOutcome::Landed | norn_cell::MappingOutcome::AlreadyLanded => {
            *landed = landed.saturating_add(1);
        }
        held @ norn_cell::MappingOutcome::Quarantined(_) => {
            *quarantined = quarantined.saturating_add(1);
            for code in held.codes() {
                let count = reasons.entry(code).or_insert(0);
                *count = count.saturating_add(1);
            }
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
        serde_json::from_str(
            &serde_json::json!({
                "source": "test", "pipeline": "test/1",
                "feeds": [{
                    "dataset": "Item",
                    "request": {"method": "POST", "path": "/items", "body": {"after": ["{{version}}", "{{id}}"], "limit": "{{size}}", "q": "v>={{version}}", "j": "{{json:version}}"}},
                    "itemsPath": "/data", "paging": {"kind": "keyset", "size": size},
                    "idPath": "/id", "versionPath": "/v",
                    "fields": {"code": "/code", "missing": "/nope"},
                    "initial": {"version": "", "id": 0}
                }]
            })
            .to_string(),
        )
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
        let mut pull = Pull::new(profile, BTreeMap::new(), Limits::default());
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
        let profile: PullProfile = serde_json::from_str(
            &serde_json::json!({
                "source": "test", "pipeline": "test/1",
                "feeds": [{
                    "dataset": "Item",
                    "request": {"method": "GET", "path": "/api/resource/Item", "query": {"filters": "[[\"modified\",\">=\",{{json:version}}]]"}},
                    "itemsPath": "/data", "paging": {"kind": "keyset", "size": 1},
                    "idPath": "/name", "versionPath": "/modified", "fields": {"name": "/name"},
                    "initial": {"version": "a\"b", "id": ""}
                }]
            })
            .to_string(),
        )
        .expect("profile");
        let mut pull = Pull::new(profile, BTreeMap::new(), Limits::default());
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
                id: RuntimeValue::from(3),
            },
        )]);
        let mut pull = Pull::new(profile, stored.clone(), Limits::default());
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
        let mut pull = Pull::new(profile, BTreeMap::new(), Limits::default());
        let _request = pull.next_request().expect("request");
        let page = pull
            .accept(200, br#"{"data":[{"id":7,"v":"1","code":9000.10}]}"#)
            .expect("page");
        let rows = &page.rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_id, "7");
        assert_eq!(
            rows[0].payload.get("code"),
            Some(&RuntimeValue::from_json_text("9000.10").expect("decimal"))
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
        // SOURCE-CAPTURE-1: fetched cursor stays ephemeral until confirmed.
        assert!(pull.watermarks().is_empty());
        pull.confirm(&page.advance);
        assert_eq!(
            pull.watermarks()["Item"],
            Watermark {
                version: "1".into(),
                id: RuntimeValue::from(7)
            }
        );
    }

    #[test]
    fn a_cursor_feed_reads_every_page_and_keeps_the_greatest_version() {
        let profile: PullProfile = serde_json::from_str(
            &serde_json::json!({
                "source": "test", "pipeline": "test/1",
                "feeds": [{
                    "dataset": "Variant",
                    "request": {"method": "POST", "path": "/graphql", "body": {"variables": {"after": "{{cursor}}", "q": "updated_at:>='{{version}}'", "n": "{{size}}"}}},
                    "itemsPath": "/data/v/nodes", "paging": {"kind": "cursor", "size": 2, "nextCursorPath": "/data/v/pageInfo/endCursor", "hasNextPath": "/data/v/pageInfo/hasNextPage"},
                    "idPath": "/id", "versionPath": "/updatedAt", "fields": {"id": "/id"},
                    "initial": {"version": "1970-01-01T00:00:00Z", "id": ""}
                }]
            })
            .to_string(),
        )
        .expect("profile");
        let mut pull = Pull::new(profile, BTreeMap::new(), Limits::default());
        let first = pull.next_request().expect("first");
        let body: Value = serde_json::from_str(first.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["variables"]["after"], Value::Null);
        assert_eq!(
            body["variables"]["q"],
            "updated_at:>='1970-01-01T00:00:00Z'"
        );
        let page1 = pull
            .accept(200, br#"{"data":{"v":{"nodes":[{"id":"a","updatedAt":"2026-11-02T00:00:00Z"},{"id":"b","updatedAt":"2026-11-03T00:00:00Z"}],"pageInfo":{"hasNextPage":true,"endCursor":"c1"}}}}"#)
            .expect("page 1");
        assert_eq!(page1.rows.len(), 2);
        assert!(page1.advance.next.is_none());
        pull.confirm(&page1.advance);
        let second = pull.next_request().expect("second");
        let body: Value =
            serde_json::from_str(second.body.as_deref().expect("body")).expect("json");
        assert_eq!(body["variables"]["after"], "c1");
        let page2 = pull
            .accept(200, br#"{"data":{"v":{"nodes":[{"id":"c","updatedAt":"2026-11-01T00:00:00Z"}],"pageInfo":{"hasNextPage":false,"endCursor":"c2"}}}}"#)
            .expect("page 2");
        assert!(pull.next_request().is_none());
        assert!(pull.watermarks().is_empty());
        pull.confirm(&page2.advance);
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
                let accepted = pull.accept(200, &serve(&rows, &request)).expect("page");
                for row in &accepted.rows {
                    seen.insert((row.version.clone(), row.source_id.clone()));
                }
                pull.confirm(&accepted.advance);
                page += 1;
            }
            for (version, id) in &rows {
                prop_assert!(seen.contains(&(version.clone(), id.to_string())), "missed {id}@{version}");
            }
            let mut again = Pull::new(profile, pull.watermarks().clone(), Limits::default());
            let request = again.next_request().expect("request");
            prop_assert!(again.accept(200, &serve(&rows, &request)).expect("page").rows.is_empty());
        }
    }
}
