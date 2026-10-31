//! The Frappe completeness sweep: a pure planner (T12-DP-4).
//!
//! Webhooks are the fast path; they can be lost. The sweep periodically polls
//! Frappe's REST API for every document modified (and every document deleted)
//! since a cursor, and emits the same raw observations a webhook would have.
//! Event ids match the webhook's ([`crate::frappe`]), so anything a webhook
//! already delivered converges as a duplicate and anything it missed is
//! filled in. History is never mutated: the sweep only appends observations.
//!
//! # Execution goes through `perform`
//!
//! This module performs no IO. [`FrappeSweep::plan`] produces a
//! [`SweepCommand`], a serializable description of one HTTP `GET`. The host
//! enqueues it as a kernel command; its `perform` adapter resolves the site's
//! base URL and credentials from host configuration, encodes
//! `path_segments`/`query`, executes the request, and delivers the response
//! JSON decoded through [`crate::parse_exact_json`] to preserve exact evidence.
//! For live documents, [`FrappeSweep::plan_documents`] then plans one
//! full-document GET per listing row: Frappe listings omit child tables.
//! The host executes these through `perform` and supplies their responses in
//! listing order to [`FrappeSweep::ingest`]. Only a completely hydrated page
//! produces observations and a next cursor. The host records that cursor
//! after accepting all observations, so the sweep resumes after a restart.
//!
//! Full documents use `GET /api/method/frappe.client.get` with `doctype` and
//! `name` query arguments. Its `message` dictionary preserves null parent and
//! child fields; the document resource endpoint serializes documents with
//! nulls removed. The host principal must have complete parent/child field-read
//! permissions, matching the webhook profile ([`crate::frappe`]); this planner
//! cannot detect fields omitted by Frappe's permission filtering.
//!
//! # Paging
//!
//! Pages use keyset pagination over `(modified, name)`:
//! `filters=[["modified", ">=", m]]` and
//! `or_filters=[["modified", ">", m], ["name", ">", n]]`, ordered
//! `modified asc, name asc`. That never skips a row when timestamps tie.
//! Each cycle starts [`FrappeSweep::lookback_secs`] before the previous
//! cycle's high-water mark ([`FrappeSweep::start_cycle`]) to catch rows
//! committed late by long transactions; the overlap converges as duplicates.
//!
//! Deletes are read from Frappe's `Deleted Document` doctype, whose `data`
//! column holds the deleted document as JSON.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::frappe::{FrappeSource, frappe_observation, string_field};
use crate::raw_observation::{ObservationKind, RawObservation, parse_exact_json};
use crate::webhook::{IngestError, format_frappe_datetime, normalize_frappe_datetime};

/// The pipeline version recorded on sweep observations.
pub const FRAPPE_SWEEP_PIPELINE: &str = "norn-dataplane.frappe-sweep/1";

/// Frappe's doctype recording deletions.
pub const DELETED_DOCUMENT_DOCTYPE: &str = "Deleted Document";

/// Which Frappe listing a sweep reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SweepFeed {
    /// The doctype's live documents (upserts).
    Documents,
    /// `Deleted Document` rows for the doctype (deletes).
    Deletions,
}

/// A keyset position in one feed: the last `(modified, name)` read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepCursor {
    /// The feed.
    pub feed: SweepFeed,
    /// The swept doctype.
    pub doctype: String,
    /// The listing row's normalized `modified`.
    pub modified: String,
    /// The listing row's `name` (empty: before every name).
    pub name: String,
}

/// One planned HTTP `GET`, executed by the host's `perform` adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepCommand {
    /// The feed this page reads.
    pub feed: SweepFeed,
    /// The swept doctype.
    pub doctype: String,
    /// The cursor the page starts after, if any.
    pub after: Option<SweepCursor>,
    /// Always `GET`.
    pub method: String,
    /// Unencoded path segments relative to the site's base URL
    /// (`["api", "resource", "Sales Order"]`); the host percent-encodes them.
    pub path_segments: Vec<String>,
    /// Unencoded query pairs, in order; the host encodes them.
    pub query: Vec<(String, String)>,
}

/// A full-document GET planned from one listing row.
///
/// The host executes it through `perform`, preserving listing order when
/// collecting responses. No observation or advanced cursor exists yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepDocumentCommand {
    /// The listing identity and version this fetch must hydrate.
    pub cursor: SweepCursor,
    /// Always `GET`.
    pub method: String,
    /// Unencoded path segments (`api`, `method`, `frappe.client.get`).
    /// The host percent-encodes each segment.
    pub path_segments: Vec<String>,
    /// Unencoded `doctype` and `name` query pairs. The host percent-encodes
    /// their values instead of interpolating them into the URL.
    pub query: Vec<(String, String)>,
}

/// The result of ingesting one page.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepPage {
    /// The observations, in listing order.
    pub observations: Vec<RawObservation>,
    /// The cursor after the last row, or `None` for an empty page (keep the
    /// previous cursor).
    pub next: Option<SweepCursor>,
    /// Whether the page was short, so the feed is caught up.
    pub exhausted: bool,
}

/// The sweep of one doctype on one Frappe site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrappeSweep {
    /// The source (and its redaction policy).
    pub source: FrappeSource,
    /// The doctype to sweep (`Sales Order`).
    pub doctype: String,
    /// Rows per page; at least 1.
    pub page_size: u32,
    /// How far each cycle rewinds before the previous high-water mark.
    pub lookback_secs: u32,
}

impl FrappeSweep {
    /// The first page of a sweep cycle: from the start when there is no
    /// high-water mark, else from `lookback_secs` before it.
    ///
    /// # Errors
    ///
    /// See [`FrappeSweep::plan`] and [`FrappeSweep::rewind`].
    pub fn start_cycle(
        &self,
        feed: SweepFeed,
        high_water: Option<&SweepCursor>,
    ) -> Result<SweepCommand, IngestError> {
        let start = high_water.map(|cursor| self.rewind(cursor)).transpose()?;
        self.plan(feed, start.as_ref())
    }

    /// The cursor `lookback_secs` before `high_water`, before every name.
    ///
    /// # Errors
    ///
    /// [`IngestError::InvalidTimestamp`] for an unparseable or out-of-range
    /// cursor.
    pub fn rewind(&self, high_water: &SweepCursor) -> Result<SweepCursor, IngestError> {
        let invalid = |reason: String| IngestError::InvalidTimestamp {
            value: high_water.modified.clone(),
            reason,
        };
        let modified: jiff::civil::DateTime = high_water
            .modified
            .parse()
            .map_err(|error: jiff::Error| invalid(error.to_string()))?;
        let earlier = modified
            .checked_sub(jiff::SignedDuration::from_secs(i64::from(
                self.lookback_secs,
            )))
            .map_err(|error| invalid(error.to_string()))?;
        Ok(SweepCursor {
            feed: high_water.feed,
            doctype: high_water.doctype.clone(),
            modified: format_frappe_datetime(earlier),
            name: String::new(),
        })
    }

    /// Plan the page of `feed` that starts after `after`.
    ///
    /// # Errors
    ///
    /// [`IngestError::InvalidSweep`] for an empty doctype, a zero page size,
    /// or a cursor of another feed or doctype.
    pub fn plan(
        &self,
        feed: SweepFeed,
        after: Option<&SweepCursor>,
    ) -> Result<SweepCommand, IngestError> {
        self.check()?;
        if let Some(cursor) = after
            && (cursor.feed != feed || cursor.doctype != self.doctype)
        {
            return Err(IngestError::InvalidSweep {
                reason: "the cursor belongs to another feed or doctype",
            });
        }
        let (listing, fields, mut filters) = match feed {
            SweepFeed::Documents => (
                self.doctype.as_str(),
                json!(["name", "modified"]),
                Vec::new(),
            ),
            SweepFeed::Deletions => (
                DELETED_DOCUMENT_DOCTYPE,
                json!([
                    "name",
                    "deleted_doctype",
                    "deleted_name",
                    "data",
                    "modified"
                ]),
                vec![json!(["deleted_doctype", "=", self.doctype])],
            ),
        };
        let mut query = vec![("fields".to_owned(), serde_json::to_string(&fields)?)];
        if let Some(cursor) = after {
            filters.push(json!(["modified", ">=", cursor.modified]));
            let or_filters = json!([
                ["modified", ">", cursor.modified],
                ["name", ">", cursor.name]
            ]);
            query.push(("or_filters".to_owned(), serde_json::to_string(&or_filters)?));
        }
        if !filters.is_empty() {
            query.push(("filters".to_owned(), serde_json::to_string(&filters)?));
        }
        query.push(("order_by".to_owned(), "modified asc, name asc".to_owned()));
        query.push(("limit_start".to_owned(), "0".to_owned()));
        query.push(("limit_page_length".to_owned(), self.page_size.to_string()));
        Ok(SweepCommand {
            feed,
            doctype: self.doctype.clone(),
            after: after.cloned(),
            method: "GET".to_owned(),
            path_segments: vec!["api".to_owned(), "resource".to_owned(), listing.to_owned()],
            query,
        })
    }

    /// Plan full-document fetches for every live-document listing row.
    ///
    /// Listings contain only identity/version columns; they cannot be sealed
    /// as documents because Frappe omits child tables. Planning never advances
    /// a cursor. Execute each command through `perform`, then pass the full
    /// `{"message": {...}}` responses to [`Self::ingest`] in the same order.
    ///
    /// # Errors
    ///
    /// A foreign command, a non-document feed, or a malformed listing row.
    pub fn plan_documents(
        &self,
        command: &SweepCommand,
        response: &Value,
    ) -> Result<Vec<SweepDocumentCommand>, IngestError> {
        self.check_command(command)?;
        if command.feed != SweepFeed::Documents {
            return Err(IngestError::InvalidSweep {
                reason: "only live-document listings need full-document fetches",
            });
        }
        Self::rows(response)?
            .iter()
            .map(|row| {
                let cursor = self.row_cursor(command.feed, row)?;
                Ok(SweepDocumentCommand {
                    path_segments: vec![
                        "api".to_owned(),
                        "method".to_owned(),
                        "frappe.client.get".to_owned(),
                    ],
                    query: vec![
                        ("doctype".to_owned(), self.doctype.clone()),
                        ("name".to_owned(), cursor.name.clone()),
                    ],
                    cursor,
                    method: "GET".to_owned(),
                })
            })
            .collect()
    }

    /// Turn a listing and its full-document responses into observations.
    ///
    /// `document_responses` contains one `{"message": {...}}` response per live
    /// listing row, in listing order; it must be empty for deletions, whose
    /// rows already embed the complete document. `extracted_at` is the host's
    /// receipt time in milliseconds.
    ///
    /// # Errors
    ///
    /// Missing or malformed responses, a fetched identity/version mismatch,
    /// or a refused observation. The whole page is refused without returning
    /// observations or an advanced cursor. If a document changed between the
    /// listing and the fetch, retry with a fresh listing; never label the new
    /// payload with the old version. The host persists `next` only after all
    /// returned observations have been accepted by the observe inbox.
    pub fn ingest(
        &self,
        command: &SweepCommand,
        response: &Value,
        document_responses: &[Value],
        extracted_at: u64,
    ) -> Result<SweepPage, IngestError> {
        self.check_command(command)?;
        let rows = Self::rows(response)?;
        let expected = match command.feed {
            SweepFeed::Documents => rows.len(),
            SweepFeed::Deletions => 0,
        };
        if document_responses.len() != expected {
            return Err(IngestError::InvalidSweep {
                reason: "full-document response count does not match the listing",
            });
        }
        let mut observations = Vec::with_capacity(rows.len());
        let mut next = None;
        let mut documents = document_responses.iter();
        for row in rows {
            let cursor = self.row_cursor(command.feed, row)?;
            let observation = match command.feed {
                SweepFeed::Documents => {
                    let doc = documents
                        .next()
                        .and_then(|response| response.get("message"))
                        .and_then(Value::as_object)
                        .ok_or(IngestError::MissingField {
                            field: "message",
                            expected: "full document object",
                        })?;
                    if string_field(doc, "doctype")? != self.doctype
                        || string_field(doc, "name")? != cursor.name
                        || normalize_frappe_datetime(string_field(doc, "modified")?)?
                            != cursor.modified
                    {
                        return Err(IngestError::InvalidSweep {
                            reason: "the fetched document does not match the listed identity/version",
                        });
                    }
                    frappe_observation(
                        &self.source,
                        &self.doctype,
                        ObservationKind::Upsert,
                        doc.clone(),
                        extracted_at,
                        Some(serde_json::to_value(&cursor)?),
                        FRAPPE_SWEEP_PIPELINE,
                    )?
                }
                SweepFeed::Deletions => self.deletion(
                    row.as_object().ok_or(IngestError::MissingField {
                        field: "data[]",
                        expected: "object",
                    })?,
                    &cursor,
                    extracted_at,
                )?,
            };
            observations.push(observation);
            next = Some(cursor);
        }
        let exhausted = u32::try_from(rows.len()).is_ok_and(|count| count < self.page_size);
        Ok(SweepPage {
            observations,
            next,
            exhausted,
        })
    }

    fn check_command(&self, command: &SweepCommand) -> Result<(), IngestError> {
        if *command != self.plan(command.feed, command.after.as_ref())? {
            return Err(IngestError::InvalidSweep {
                reason: "the command does not match this sweep's listing plan",
            });
        }
        Ok(())
    }

    fn rows(response: &Value) -> Result<&[Value], IngestError> {
        response
            .get("data")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .ok_or(IngestError::MissingField {
                field: "data",
                expected: "array",
            })
    }

    fn row_cursor(&self, feed: SweepFeed, row: &Value) -> Result<SweepCursor, IngestError> {
        let row = row.as_object().ok_or(IngestError::MissingField {
            field: "data[]",
            expected: "object",
        })?;
        Ok(SweepCursor {
            feed,
            doctype: self.doctype.clone(),
            modified: normalize_frappe_datetime(string_field(row, "modified")?)?,
            name: string_field(row, "name")?.to_owned(),
        })
    }

    fn deletion(
        &self,
        row: &Map<String, Value>,
        cursor: &SweepCursor,
        extracted_at: u64,
    ) -> Result<RawObservation, IngestError> {
        if string_field(row, "deleted_doctype")? != self.doctype {
            return Err(IngestError::InvalidSweep {
                reason: "a deletion row belongs to another doctype",
            });
        }
        let deleted_name = string_field(row, "deleted_name")?;
        let data: Value = parse_exact_json(string_field(row, "data")?.as_bytes())?;
        let Value::Object(doc) = data else {
            return Err(IngestError::MissingField {
                field: "data",
                expected: "JSON object string",
            });
        };
        if string_field(&doc, "name")? != deleted_name {
            return Err(IngestError::InvalidSweep {
                reason: "a deletion row's data does not match its deleted_name",
            });
        }
        frappe_observation(
            &self.source,
            &self.doctype,
            ObservationKind::Delete,
            doc,
            extracted_at,
            Some(serde_json::to_value(cursor)?),
            FRAPPE_SWEEP_PIPELINE,
        )
    }

    fn check(&self) -> Result<(), IngestError> {
        if self.doctype.trim().is_empty() {
            return Err(IngestError::InvalidSweep {
                reason: "the doctype is empty",
            });
        }
        if self.page_size == 0 {
            return Err(IngestError::InvalidSweep {
                reason: "the page size is zero",
            });
        }
        Ok(())
    }
}
