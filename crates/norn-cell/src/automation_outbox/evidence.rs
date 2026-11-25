//! Restore equivalence evidence for `AUTOMATION-OUTBOX-1` (#1704): what an
//! export records about a paused commit's inline candidates, and which target
//! ids an NFC export cannot tell apart.

use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use super::capture::action_calls_in_batch;
use crate::codec::canonical_json;
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::StoreReader;
use crate::store::{CommitBatch, WrittenRow};

/// Export-only evidence of the inline candidates a paused commit recorded
/// (#1704). An export carries NFC identities, so a cursor alone does not pin
/// the inline/deferred partition: two traversals can pause at the same target
/// after emitting different candidates. A restore replays the commit and
/// accepts its continuation only when the replayed candidates encode to the
/// same canonical (NFC) set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationInlineEvidence {
    /// Inline candidates the commit recorded before pausing.
    pub candidate_count: u64,
    /// Norn1 hash of the candidates' canonical JSON encodings, sorted.
    pub candidates_hash: String,
}

pub(super) fn inline_evidence<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    commit_seq: u64,
) -> Result<AutomationInlineEvidence, CellError> {
    let mut encoded = reader
        .automation_triggers_for_commit(world, commit_seq)?
        .iter()
        .map(canonical_json)
        .collect::<Result<Vec<_>, _>>()?;
    encoded.sort_unstable();
    Ok(AutomationInlineEvidence {
        candidate_count: u64::try_from(encoded.len())
            .map_err(|_overflow| CellError::OutOfRange("automation candidate count"))?,
        candidates_hash: crate::codec::hash_json(&encoded)?,
    })
}

/// Whether some other spelling of `id` normalizes to it under NFC, so an
/// NFC export cannot tell which spelling the live commit wrote. Exact for
/// ASCII: no ASCII character decomposes, and the only canonical singletons
/// onto ASCII are KELVIN SIGN, GREEK QUESTION MARK and GREEK VARIA (asserted
/// over every scalar by `nfc_ambiguity_is_exact_for_ascii`); every other
/// character counts as ambiguous.
pub(super) fn nfc_ambiguous(id: &str) -> bool {
    id.chars()
        .any(|ch| !ch.is_ascii() || NFC_SINGLETON_ASCII_TARGETS.contains(&ch))
}

/// The ASCII characters that a different code point normalizes to under NFC.
pub(super) const NFC_SINGLETON_ASCII_TARGETS: [char; 3] = ['K', ';', '`'];

/// Whether any target identity the matcher orders in this commit is
/// [`nfc_ambiguous`]: a written object id, a committed action call key, or an
/// action call's target id.
pub(super) fn commit_has_nfc_ambiguous_target(batch: &CommitBatch, written: &[WrittenRow]) -> bool {
    written.iter().any(|row| nfc_ambiguous(&row.key))
        || action_calls_in_batch(batch).iter().any(|(key, fields)| {
            nfc_ambiguous(key)
                || fields
                    .object
                    .and_then(Value::as_object)
                    .and_then(|object| object.get("key"))
                    .and_then(Value::as_str)
                    .is_some_and(nfc_ambiguous)
        })
}
