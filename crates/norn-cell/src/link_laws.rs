//! LAW-LINK-1: the link targets a governing law reads, from the store.
//!
//! Recovery re-evaluates a finding against the subject's *current* link and
//! its target's current values ([`CurrentLinks`]): either side may have
//! changed. Verifying a finding retained in history replays the evaluation
//! against the exact target versions the finding pinned ([`PinnedLinks`]),
//! so a later change to the target never alters what was proven.

use std::collections::BTreeMap;

use norn_ir::FactRef;
use norn_kernel::FactKey;
use norn_kernel::governing_laws::{LawEvaluationError, LinkTargets, LinkedTarget};

use crate::StoreReader;
use crate::engine::SqlEngine;

fn unreadable(link: &str, error: impl std::fmt::Display) -> LawEvaluationError {
    LawEvaluationError::Unsupported(format!(
        "LAW-LINK-1: the target of {link} cannot be read: {error}"
    ))
}

/// A subject's live link targets in `world`, as the store holds them now.
pub(crate) struct CurrentLinks<'r, 'c, S: SqlEngine> {
    pub(crate) reader: &'r StoreReader<'c, S>,
    pub(crate) world: &'r str,
    pub(crate) subject: FactKey,
}

impl<S: SqlEngine> LinkTargets for CurrentLinks<'_, '_, S> {
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError> {
        let subject_type = self.subject.object_type.as_str();
        let edges = self
            .reader
            .links_touching(self.world, subject_type, &self.subject.key)
            .map_err(|error| unreadable(link, error))?;
        let mut targets = edges.into_iter().filter(|edge| {
            edge.link == link
                && edge.source == self.subject.key
                && self.reader.model().is_a(subject_type, &edge.source_type)
        });
        let Some(edge) = targets.next() else {
            return Ok(None);
        };
        if targets.next().is_some() {
            return Err(unreadable(link, "more than one target on a to-one link"));
        }
        let record = self
            .reader
            .object(self.world, &edge.target_type, &edge.target)
            .map_err(|error| unreadable(link, error))?
            .ok_or_else(|| unreadable(link, "the target is absent"))?;
        Ok(Some(LinkedTarget {
            key: FactKey::new(record.object_type.as_str(), record.id.clone()),
            version: record.version,
            values: record.values,
        }))
    }
}

/// The exact target versions a retained finding pinned, by link id.
pub(crate) struct PinnedLinks<'r, 'c, S: SqlEngine> {
    pub(crate) reader: &'r StoreReader<'c, S>,
    pub(crate) world: &'r str,
    pub(crate) links: BTreeMap<String, FactRef>,
}

impl<S: SqlEngine> LinkTargets for PinnedLinks<'_, '_, S> {
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError> {
        let Some(pinned) = self.links.get(link) else {
            return Ok(None);
        };
        let record = self
            .reader
            .object_at_version(
                self.world,
                pinned.object_type.as_str(),
                &pinned.key,
                pinned.version,
            )
            .map_err(|error| unreadable(link, error))?
            .ok_or_else(|| unreadable(link, "the pinned version is a tombstone"))?;
        Ok(Some(LinkedTarget {
            key: FactKey::new(record.object_type.as_str(), record.id),
            version: record.version,
            values: record.values,
        }))
    }
}

/// The target versions a finding's outcome pinned (empty for a law that read
/// no link, or whose links had no target).
pub(crate) fn pinned_links(finding: &norn_ir::DataQualityFinding) -> BTreeMap<String, FactRef> {
    finding.links.clone()
}
