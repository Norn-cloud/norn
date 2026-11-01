//! "Latest observation wins": the pure current-state view over raw
//! observations.
//!
//! The log itself is never changed. This view is a deterministic fold:
//!
//! - A repeated `(source, source_id, event_id)` (the observation id) is a
//!   duplicate delivery and is ignored.
//! - Per `(source, source_id)` the observation with the greatest
//!   `(source_version, kind, event_id)` wins. The order is total, so the
//!   result does not depend on delivery order: webhook redeliveries, late
//!   webhooks, and completeness sweeps all converge to the same state.
//! - A delete is an ordinary observation that leaves a tombstone.

//!
//! DP-CONV-1: reordering distinct immutable observations and replaying identical
//! deliveries yields the same view. DP-CONV-2: delete wins an equal-version tie;
//! source and record identities remain separate. Sampled evidence: `convergence_conforms`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::raw_observation::{ObservationKind, RawObservation};

/// The current state of one source record.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentRecord {
    /// Upsert (live) or delete (tombstone).
    pub kind: ObservationKind,
    /// The winning observation's event id.
    pub event_id: String,
    /// The winning observation's source version.
    pub source_version: String,
    /// The winning observation's payload.
    pub payload: Value,
}

impl CurrentRecord {
    fn order_key(&self) -> (&str, ObservationKind, &str) {
        (
            self.source_version.as_str(),
            self.kind,
            self.event_id.as_str(),
        )
    }
}

/// What [`Convergence::apply`] did with one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The observation id was seen before; nothing changed.
    Duplicate,
    /// A newer observation of the record is already current.
    Stale,
    /// The observation is now the record's current state.
    Advanced,
}

/// The fold state: seen event ids and the current record per source id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Convergence {
    seen: BTreeSet<(String, String, String)>,
    current: BTreeMap<(String, String), CurrentRecord>,
}

impl Convergence {
    /// An empty view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a sequence of observations.
    #[must_use]
    pub fn from_log<'a>(observations: impl IntoIterator<Item = &'a RawObservation>) -> Self {
        let mut view = Self::new();
        for observation in observations {
            view.apply(observation);
        }
        view
    }

    /// Fold one observation into the view.
    pub fn apply(&mut self, observation: &RawObservation) -> Applied {
        let event = (
            observation.source.clone(),
            observation.source_id.clone(),
            observation.event_id.clone(),
        );
        if !self.seen.insert(event) {
            return Applied::Duplicate;
        }
        let candidate = CurrentRecord {
            kind: observation.kind,
            event_id: observation.event_id.clone(),
            source_version: observation.source_version.clone(),
            payload: observation.payload.clone(),
        };
        let record = (observation.source.clone(), observation.source_id.clone());
        let stale = self
            .current
            .get(&record)
            .is_some_and(|existing| existing.order_key() >= candidate.order_key());
        if stale {
            return Applied::Stale;
        }
        self.current.insert(record, candidate);
        Applied::Advanced
    }

    /// Whether the observation `(source, source_id, event_id)` has been
    /// folded.
    #[must_use]
    pub fn has_seen(&self, source: &str, source_id: &str, event_id: &str) -> bool {
        self.seen
            .contains(&(source.to_owned(), source_id.to_owned(), event_id.to_owned()))
    }

    /// The current state of one record.
    #[must_use]
    pub fn current(&self, source: &str, source_id: &str) -> Option<&CurrentRecord> {
        self.current.get(&(source.to_owned(), source_id.to_owned()))
    }

    /// Every record's current state, ordered by `(source, source_id)`.
    pub fn records(&self) -> impl Iterator<Item = (&str, &str, &CurrentRecord)> {
        self.current
            .iter()
            .map(|((source, source_id), record)| (source.as_str(), source_id.as_str(), record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObservationDraft;
    #[test]
    #[allow(clippy::expect_used, reason = "conformance assertions")]
    fn convergence_conforms() {
        let make = |event, version, kind| {
            ObservationDraft {
                source: "fixture".into(),
                source_id: "record".into(),
                event_id: event,
                kind,
                source_version: version,
                payload: Value::Null,
                extracted_at: 1,
                cursor: None,
                pipeline_version: "fixture/v1".into(),
            }
            .seal(None)
            .expect("seal")
        };
        let old = make("old".into(), "1".into(), ObservationKind::Upsert);
        let live = make("live".into(), "2".into(), ObservationKind::Upsert);
        let gone = make("gone".into(), "2".into(), ObservationKind::Delete);
        let mut view = Convergence::from_log([&old, &live, &gone]);
        assert_eq!(view, Convergence::from_log([&gone, &live, &old]));
        assert_eq!(view.apply(&gone), Applied::Duplicate);
        assert_eq!(
            view.current("fixture", "record").expect("record").kind,
            ObservationKind::Delete
        );
        assert!(view.current("other", "record").is_none());
        assert!(view.current("fixture", "other").is_none());
        assert_eq!(view.records().count(), 1);
        let table = crate::iceberg::create_table_request();
        assert_eq!(table["properties"]["norn.append-only"], "true");
        assert_eq!(table["properties"]["format-version"], "3");
        assert_eq!(table["schema"]["fields"][6]["type"], "variant");
    }
}
