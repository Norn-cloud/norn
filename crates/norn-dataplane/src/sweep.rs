//! Pure keyset-page planning for a Frappe completeness sweep.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::frappe::FrappeSource;
use crate::raw_observation::RawObservation;
use crate::webhook::{IngestError, format_frappe_datetime};

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
