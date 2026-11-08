//! LINEAGE-SUM-1: native contribution drill-through, pinned to one exact result.
use super::{Freshness, QueryContext, QueryResult, Totals, decimal, refused};
use crate::{
    CellError, CellStore, ObjectRecord, engine::SqlEngine,
    source_provenance::InstanceSourceProvenance,
};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{Decimal, FactRef, RuntimeObject};
use serde::Serialize;

/// Independent current host grants; numeric visibility grants neither disclosure.
pub struct LineageDisclosure<'a> {
    /// Permission to reveal all committed canonical property values.
    pub values: &'a mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    /// Permission to resolve source row/document provenance through Core 07.
    pub source: &'a mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
}

/// Authorized source edge; numeric completeness does not imply source disclosure.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ContributionSource {
    /// The cell authenticated and recomputed the exact producing input bundle.
    Resolved {
        /// Source row/document identities, digests, mapping pin and field provenance.
        provenance: Box<InstanceSourceProvenance>,
    },
    /// The current host policy withheld source provenance.
    Restricted,
    /// The canonical fact has no retained mapping bundle.
    Unavailable,
}

/// One numeric contribution and its canonical fact incarnation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericContribution {
    /// Stable versioned fact identity; never a catalog-enrichment edge.
    pub fact: FactRef,
    /// Sum operand, or numerator then denominator; no ratio attribution is invented.
    /// `None` when any member of the bucket is restricted (DATA-VALUES-1):
    /// amounts are row values, and one withheld amount would follow from the totals.
    pub amounts: Option<Vec<Option<Decimal>>>,
    /// Emitting receipt identity; a reference, not an independently verifiable proof.
    pub receipt_hash: String,
    /// Full-incarnation digest, disclosed only with the canonical property values.
    pub fact_digest: Option<String>,
    /// Authorized source lineage, separate from numeric amounts and catalog enrichment.
    pub source: ContributionSource,
    /// Cell commit position of the writer.
    pub commit_seq: u64,
    /// Knowledge time authenticated by the emitting receipt.
    pub recorded_at_ms: u64,
    /// The host withheld canonical property content, without hiding numeric membership.
    pub restricted: bool,
    /// All committed canonical properties, only when the host permits their disclosure.
    pub values: Option<RuntimeObject>,
}

/// Bounded native evidence for one result bucket, with its original frontier.
/// This is data, not an authorization or export capability.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericTrace {
    /// Revalidated exact cell result and definition/time/scope/frontier pins.
    pub result: QueryResult,
    /// Canonical selected grouping key.
    pub bucket: String,
    /// Deterministic identity order; restricted values stay visibly absent.
    pub contributions: Vec<NumericContribution>,
}

impl<E: SqlEngine> CellStore<E> {
    /// LINEAGE-SUM-1 / DATA-LINEAGE: resolve exact numeric membership in one transaction.
    /// The host supplies current population visibility and permission to reveal *all*
    /// canonical properties of each member. Otherwise values are visibly restricted.
    /// Source provenance uses Core 07 authority and a separate current disclosure grant.
    /// Catalog enrichment never adds numeric contributions; document content grants are separate.
    /// A changed result pin requires a fresh result, never a mixture of generations.
    /// # Errors
    /// Refuses nonexact/incomplete results, changed pins, missing evidence or budgets.
    pub fn trace_question(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        expected_lineage: &str,
        bucket: &str,
        visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
        disclosure: LineageDisclosure<'_>,
    ) -> Result<NumericTrace, CellError> {
        self.trace_question_matching(
            plan,
            context,
            &|lineage| lineage == expected_lineage,
            bucket,
            visible,
            disclosure,
        )
    }

    /// [`Self::trace_question`] for a caller that holds a handle to the
    /// result pin rather than the pin itself (D53: a keyed public handle,
    /// [`keyed_lineage_handle`]): `matches` decides whether the recomputed
    /// internal pin is the one the handle names.
    /// # Errors
    /// As [`Self::trace_question`].
    pub fn trace_question_matching(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        matches: &dyn Fn(&str) -> bool,
        bucket: &str,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
        disclosure: LineageDisclosure<'_>,
    ) -> Result<NumericTrace, CellError> {
        let LineageDisclosure { values, source } = disclosure;
        let incomplete = || refused("core.projection.lineage_incomplete");
        self.question_snapshot_with(plan, context, &mut visible, |snapshot, budget| {
            let result = snapshot.result;
            if !result.complete || result.freshness != Freshness::Exact {
                return Err(refused("core.compute.stale_input"));
            }
            if !matches(&result.lineage_ref) {
                return Err(incomplete());
            }
            let row = result.rows.get(bucket).ok_or_else(incomplete)?;
            let mut totals = Totals::default();
            let mut contributions = Vec::with_capacity(row.members.len());
            let reader = self.read();
            for (id, member) in &row.members {
                let (_, value) = snapshot.contributions.get(id).ok_or_else(incomplete)?;
                totals.adjust(value, false)?;
                let evidence = reader
                    .fact_version_evidence_with_budget(
                        &result.context.world,
                        result.grain.as_str(),
                        id,
                        member.version,
                        budget,
                    )?
                    .ok_or_else(incomplete)?;
                let snapshot = evidence.snapshot();
                let record = ObjectRecord {
                    object_type: snapshot.object_type.clone(),
                    id: snapshot.key.clone(),
                    scope: snapshot.scope.clone(),
                    version: snapshot.version,
                    values: snapshot.values.clone().ok_or_else(incomplete)?,
                };
                let receipt = evidence.receipt();
                if record.scope != result.context.scope
                    || receipt.body.commit_seq > result.revision.commits
                {
                    return Err(incomplete());
                }
                let allowed = values(&record)?;
                let fact = FactRef::new(&record.object_type, id, member.version);
                let mut source_allowed = false;
                let source = reader.instance_source_provenance(
                    &result.context.world,
                    &fact,
                    budget,
                    |source_record| {
                        source_allowed = source(source_record)?;
                        Ok(source_allowed)
                    },
                )?;
                let source = match source {
                    Some(provenance) => ContributionSource::Resolved {
                        provenance: Box::new(provenance),
                    },
                    None if source_allowed => ContributionSource::Unavailable,
                    None => ContributionSource::Restricted,
                };
                contributions.push(NumericContribution {
                    fact,
                    fact_digest: allowed.then(|| evidence.snapshot_sha256().to_owned()),
                    source,
                    amounts: member.amounts.clone(),
                    receipt_hash: receipt.hash.clone(),
                    commit_seq: receipt.body.commit_seq,
                    recorded_at_ms: receipt.body.logical_time,
                    restricted: !allowed,
                    values: allowed.then_some(record.values),
                });
            }
            let sums = totals
                .0
                .get(bucket)
                .ok_or_else(incomplete)?
                .iter()
                .map(|n| decimal(*n))
                .collect::<Result<Vec<_>, _>>()?;
            if sums != row.totals {
                return Err(incomplete());
            }
            // DATA-VALUES-1 (D53): one restricted member withholds every
            // value-bearing field of every member in the bucket: amounts,
            // canonical values and the snapshot digest (a guessing oracle for
            // the values). Otherwise the visible members subtracted from the
            // totals would reconstruct the restricted one. The same holds for the
            // source digests (bundle and payload), which hash the same values and
            // would confirm a guess of a small-domain value offline. Source
            // identities, mapping pin and field provenance carry no values and
            // follow their own Sources grant.
            if contributions
                .iter()
                .any(|contribution| contribution.restricted)
            {
                for contribution in &mut contributions {
                    contribution.amounts = None;
                    contribution.values = None;
                    contribution.fact_digest = None;
                    if let ContributionSource::Resolved { provenance } = &mut contribution.source {
                        provenance.bundle_digest = None;
                        provenance.document_digest = None;
                    }
                }
            }
            Ok(NumericTrace {
                result,
                bucket: bucket.to_owned(),
                contributions,
            })
        })
    }
}

/// D53 (3): the public handle to a result pin. The internal `lineageRef` is a
/// content hash of the unredacted rows, so publishing it would let a reader
/// verify a guess of a withheld value offline. The handle is
/// HMAC-SHA-256(`key`, pin) as lowercase hex: without the cell's key it says
/// nothing about the rows, and the host maps it back by recomputing.
#[must_use]
pub fn keyed_lineage_handle(key: &[u8], lineage_ref: &str) -> String {
    use transparency_sha2::{Digest, Sha256};
    let hashed;
    let source: &[u8] = if key.len() > 64 {
        hashed = Sha256::digest(key);
        &hashed
    } else {
        key
    };
    let mut block = [0_u8; 64];
    for (slot, byte) in block.iter_mut().zip(source) {
        *slot = *byte;
    }
    let pad = |byte: u8| block.map(|k| k ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(lineage_ref.as_bytes())
        .finalize();
    let outer = Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    let mut handle = String::with_capacity(64);
    for byte in outer {
        for nibble in [byte >> 4, byte & 0x0f] {
            // Every nibble is below 16, so a hex digit always exists.
            if let Some(digit) = char::from_digit(u32::from(nibble), 16) {
                handle.push(digit);
            }
        }
    }
    handle
}

/// Whether `handle` is the keyed handle of `lineage_ref`, compared in constant time.
#[must_use]
pub fn lineage_handle_matches(key: &[u8], lineage_ref: &str, handle: &str) -> bool {
    let expected = keyed_lineage_handle(key, lineage_ref);
    expected.len() == handle.len()
        && expected
            .bytes()
            .zip(handle.bytes())
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}
