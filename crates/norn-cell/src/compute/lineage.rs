//! LINEAGE-SUM-1: native contribution drill-through, pinned to one exact result.
use super::{Freshness, QueryContext, QueryResult, Totals, decimal, refused};
use crate::{
    CellError, CellStore, LedgerHead, ObjectRecord, Receipt, StoreReader,
    engine::SqlEngine,
    reactive::{DecodeBudget, ReactiveLimits},
    source_provenance::InstanceSourceProvenance,
};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{Decimal, FactRef, ResourceLimitProfile, RuntimeObject};
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
    /// `None` when any member of the bucket is restricted, or the reader lacks
    /// Values on any record the emitting commit wrote (DATA-VALUES-1): the
    /// receipt hash commits to the whole commit's values, so it would confirm a guess.
    pub receipt_hash: Option<String>,
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
            let mut covers = CommitCovers::new();
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
                let covered = allowed && covers.covered(&reader, receipt, values)?;
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
                    receipt_hash: covered.then(|| receipt.hash.clone()),
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
            withhold_restricted_bucket(&mut contributions);
            Ok(NumericTrace {
                result,
                bucket: bucket.to_owned(),
                contributions,
            })
        })
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// #1175 (DATA-VALUES-1, D53): the receipt hash of commit `commit_seq`,
    /// only when `values` covers the ledger up to and including that commit
    /// (`CoveredPrefix`). The hash commits to the commit batch, values
    /// included, and to the ledger head, which chains every earlier event, so
    /// a reader missing any of those values could confirm a guess offline.
    /// `None` withholds it, also when the cover cannot be proven within the
    /// read bound. Genesis has no receipt.
    /// # Errors
    /// Missing/corrupt receipts or snapshots, or engine failures.
    pub fn disclosable_receipt_hash(
        &self,
        commit_seq: u64,
        values: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<Option<String>, CellError> {
        self.conn.transaction(|| {
            let reader = self.read();
            let mut budget = cover_budget();
            let mut prefix = CoveredPrefix::genesis();
            prefix.extend(&reader, commit_seq, values, &mut budget)?;
            Ok(if prefix.commit == commit_seq {
                prefix.receipt
            } else {
                None
            })
        })
    }

    /// #1175 (DATA-VALUES-1, D53): whether `head`'s raw ledger hash may be
    /// disclosed to a reader whose Values grant is `values`. The hash is an
    /// unkeyed chain over every event up to `head`, and events carry record
    /// values, so a reader who knows a predecessor head could confirm a
    /// guessed value of one new event offline. It is disclosable only when
    /// `values` covers the whole ledger up to `head` (`CoveredPrefix`). The
    /// genesis head chains nothing and is always disclosable.
    /// # Errors
    /// Missing/corrupt receipts or snapshots, or engine failures.
    pub fn ledger_head_disclosable(
        &self,
        head: &LedgerHead,
        values: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<bool, CellError> {
        if head.seq == 0 {
            return Ok(head.hash == crate::system::GENESIS_HASH);
        }
        self.conn.transaction(|| {
            let reader = self.read();
            let mut budget = cover_budget();
            let mut prefix = CoveredPrefix::genesis();
            prefix.extend(&reader, head.commits, values, &mut budget)?;
            Ok(prefix.commit == head.commits && prefix.seq == head.seq && prefix.head == head.hash)
        })
    }
}

fn cover_budget() -> DecodeBudget {
    DecodeBudget::new(&ReactiveLimits::from_profile(
        &ResourceLimitProfile::norn_limits_v1(),
    ))
}

/// Ledger entries read per page while extending a [`CoveredPrefix`].
const LEDGER_COVER_PAGE: usize = 256;

/// #1175 (DATA-VALUES-1, D53): the longest ledger prefix, from genesis, that
/// a Values grant covers. Receipt hashes and ledger heads chain every earlier
/// event, so either is disclosable only inside it. A commit extends the prefix
/// when `values` grants every live record it wrote, it carries no quantity
/// batch, enqueues no command and schedules no timer, and each of its events
/// is a record event (below) chained from the prefix head to its receipt's
/// ledger head. Anything else ends the prefix: other events (command
/// payloads, refusals, findings, counters, declassifications), a broken
/// chain, a cover a sibling grant cannot decide, or the read bound. Engine
/// failures still refuse.
struct CoveredPrefix {
    /// The last covered commit (`0`: none).
    commit: u64,
    /// The last covered event sequence number.
    seq: u64,
    /// The ledger head after `commit`.
    head: String,
    /// The receipt hash of `commit`.
    receipt: Option<String>,
    /// The next commit is not covered: the prefix cannot grow.
    stopped: bool,
}

impl CoveredPrefix {
    fn genesis() -> Self {
        Self {
            commit: 0,
            seq: 0,
            head: crate::system::GENESIS_HASH.to_owned(),
            receipt: None,
            stopped: false,
        }
    }

    /// Grow the prefix up to commit `upto`, until a commit is not covered.
    fn extend<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        upto: u64,
        values: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
        budget: &mut DecodeBudget,
    ) -> Result<(), CellError> {
        while !self.stopped && self.commit < upto {
            let Some(commit_seq) = self.commit.checked_add(1) else {
                self.stopped = true;
                break;
            };
            if !self.covers_next(reader, commit_seq, values, budget)? {
                self.stopped = true;
            }
        }
        Ok(())
    }

    fn covers_next<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        commit_seq: u64,
        values: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
        budget: &mut DecodeBudget,
    ) -> Result<bool, CellError> {
        let Some(receipt) = reader.receipt_at_with_budget(commit_seq, budget)? else {
            return Ok(false);
        };
        let body = &receipt.body;
        if Some(body.first_event_seq) != self.seq.checked_add(1)
            || body.quantity.is_some()
            || body.quantity_result.is_some()
            || !body.enqueued.is_empty()
            || !body.scheduled.is_empty()
            || !receipt_values_covered(reader, &receipt, values, budget)?
        {
            return Ok(false);
        }
        let Some(last) = self.seq.checked_add(body.event_count) else {
            return Ok(false);
        };
        let mut seq = self.seq;
        let mut prev = self.head.clone();
        while seq < last {
            let limit = usize::try_from(last.saturating_sub(seq))
                .unwrap_or(LEDGER_COVER_PAGE)
                .min(LEDGER_COVER_PAGE);
            let Some(page) = reader.ledger_page_with_budget(seq, limit, budget)? else {
                return Ok(false);
            };
            if page.is_empty() {
                return Ok(false);
            }
            for entry in page {
                if Some(entry.seq) != seq.checked_add(1)
                    || entry.commit_seq != commit_seq
                    || entry.prev_hash != prev
                    || !record_event(&entry.event)
                {
                    return Ok(false);
                }
                seq = entry.seq;
                prev = entry.hash;
            }
        }
        if seq != last || prev != body.ledger_head {
            return Ok(false);
        }
        self.commit = commit_seq;
        self.seq = seq;
        self.head = prev;
        self.receipt = Some(receipt.hash);
        Ok(true)
    }
}

/// A record event: its values are those of record versions the receipt
/// write-set cover decides, plus, for an observed one, source provenance,
/// whose digests are disclosed with the same Values grant (D53 item 1).
fn record_event(event: &norn_ir::RuntimeValue) -> bool {
    fn op(event: &norn_ir::RuntimeValue) -> Option<&str> {
        event.get("op").and_then(norn_ir::RuntimeValue::as_str)
    }
    match op(event) {
        Some(
            "created"
            | "set"
            | "modified"
            | "deleted"
            | "transitioned"
            | "observed_transition"
            | "observed_deleted"
            | "observed_recreated",
        ) => true,
        Some("observed_fact") => event.get("event").is_some_and(|inner| {
            matches!(
                op(inner),
                Some("created" | "set" | "modified" | "deleted" | "transitioned")
            )
        }),
        _ => false,
    }
}

/// #1175: the per-trace [`CoveredPrefix`], grown on demand to each emitting
/// commit under a bound of its own, so withholding a receipt hash never
/// starves the trace's evidence reads. Over the bound, the prefix stops and
/// later hashes are withheld.
struct CommitCovers {
    budget: DecodeBudget,
    prefix: CoveredPrefix,
}

impl CommitCovers {
    fn new() -> Self {
        Self {
            budget: cover_budget(),
            prefix: CoveredPrefix::genesis(),
        }
    }

    fn covered<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        receipt: &Receipt,
        values: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<bool, CellError> {
        let commit = receipt.body.commit_seq;
        self.prefix
            .extend(reader, commit, values, &mut self.budget)?;
        Ok(self.prefix.commit >= commit && commit > 0)
    }
}

/// Whether `values` grants every live record `receipt` wrote. Tombstones carry
/// no values. An unprovable or over-budget cover is not a cover, and neither is
/// a sibling record the grant cannot decide (a host that cannot map it): the
/// receipt hash is withheld, the trace or answer is not refused. Engine
/// failures still refuse.
fn receipt_values_covered<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    receipt: &Receipt,
    values: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    budget: &mut DecodeBudget,
) -> Result<bool, CellError> {
    let Some(written) = reader.receipt_written_with_budget(receipt, budget)? else {
        return Ok(false);
    };
    for snapshot in written {
        let Some(row_values) = snapshot.values else {
            continue;
        };
        let record = ObjectRecord {
            object_type: snapshot.object_type,
            id: snapshot.key,
            scope: snapshot.scope,
            version: snapshot.version,
            values: row_values,
        };
        if !values(&record).unwrap_or(false) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// D53 (3) for a receipt coordinate: the public handle a reader without the
/// cover sees in place of a receipt hash (HMAC under the cell's handle key,
/// domain-separated from result pins). It names the commit stably for that
/// cell and says nothing about the values the hash commits to.
#[must_use]
pub fn keyed_receipt_handle(key: &[u8], receipt_hash: &str) -> String {
    keyed_lineage_handle(key, &format!("norn.receipt/1:{receipt_hash}"))
}

/// #1175 (D53, item 3) for the ledger head: the public handle a reader
/// without [`CellStore::ledger_head_disclosable`] sees in place of
/// `revision.hash` (HMAC under the cell's handle key, domain-separated from
/// result pins and receipt handles). It is stable per revision, so equality
/// still tells two revisions apart, and says nothing about chained values.
#[must_use]
pub fn keyed_revision_handle(key: &[u8], ledger_hash: &str) -> String {
    keyed_lineage_handle(key, &format!("norn.revision/1:{ledger_hash}"))
}

/// DATA-VALUES-1 (D53): one restricted member withholds every value-bearing
/// field of every member in the bucket: amounts, canonical values, the
/// snapshot digest and the receipt hash (each a guessing oracle for the
/// values: the receipt hash commits to the commit batch, values included, so a
/// single-write commit's hash confirms a guess offline). Otherwise the visible
/// members subtracted from the totals would reconstruct the restricted one. The
/// same holds for the source digests (bundle and payload), which hash the same
/// values and would confirm a guess of a small-domain value offline. Source
/// identities, mapping pin and field provenance carry no values and follow their
/// own Sources grant.
fn withhold_restricted_bucket(contributions: &mut [NumericContribution]) {
    if !contributions
        .iter()
        .any(|contribution| contribution.restricted)
    {
        return;
    }
    for contribution in contributions {
        contribution.amounts = None;
        contribution.values = None;
        contribution.fact_digest = None;
        contribution.receipt_hash = None;
        if let ContributionSource::Resolved { provenance } = &mut contribution.source {
            provenance.bundle_digest = None;
            provenance.document_digest = None;
        }
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
