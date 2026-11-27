//! Core 08 Fork Unit 2b (E4, #1947, `FORK-EVIDENCE-1` Lemma L6): pin-bound
//! fork writes and auditable fork-local simulated evidence for Decision Cards.
//!
//! Under storage version 12, every commit newly minted inside a forked store
//! binds its durable [`ForkPin`] directly into:
//! - [`ReceiptBody::fork_pin`](crate::store::ReceiptBody::fork_pin) (hashed
//!   into [`Receipt::hash`]),
//! - [`WrittenRow::fork_world`](crate::store::WrittenRow::fork_world), and
//! - [`LedgerEntry::fork_pin`](crate::read::LedgerEntry::fork_pin) /
//!   [`LedgerEntry::hash`](crate::read::LedgerEntry::hash).
//!
//! Ordinary `main`-world receipt bodies and ledger entries omit these fields
//! and keep byte-identical hashes (`ExportRecord::storage_version` records
//! `STORAGE_VERSION` 12 per `ENGINE-UPGRADE-1`). Two invariants hold across
//! arbitrary fork nesting depths (`D_i`, nested `S_i`, ...):
//! 1. **Never main evidence (Lemma L6):** any fork-bound receipt, row, ledger
//!    entry, or export record refuses `core.world.fork_evidence_forbidden` if
//!    presented as main-world evidence ([`verify_main_receipt`],
//!    [`verify_main_export_record`], [`CellStore::export_record`],
//!    [`ExportRecord::advance`], [`ExportRecord::validate_manifest`], and
//!    restore/rebuild/open paths).
//! 2. **Auditable simulated evidence outliving its fork:** while a fork is open,
//!    [`CellStore::export_fork_commit_evidence`] exports explicitly simulated,
//!    pin-bound [`ForkCommitEvidence`] (carrying cryptographic lineage records
//!    linking inherited commits directly to `exporting_pin.base`), and
//!    [`SimulatedDecisionCardEvidence`] bundles fork question/comparison pins
//!    with those commits so a main-world Decision Card can persist and
//!    independently verify the simulated evidence even after the fork is
//!    discarded, expired, or promoted.

use norn_ir::cell_id::CellId;
use norn_ir::{Decimal, RuntimeValue};
use serde::{Deserialize, Serialize};

use crate::codec::hash_json;
use crate::compute::fork::{verify_fork_result_lineage, verify_main_result_lineage};
use crate::compute::{
    ForkQuestionResult, QueryResult, WorldCompareDelta, WorldCompareResult, compare_forks,
    compare_main_and_fork, fork_query_pin, fork_result_pin,
};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::export::{EXPORT_FORMAT, ExportRecord};
use crate::fork::ForkPin;
use crate::read::{LedgerEntry, LedgerHead, entry_hash_with_pin};
use crate::store::{CellStore, MAIN_WORLD, Receipt, ReceiptBody, WrittenRow};
use crate::system::STORE_FORMAT;

const FORK_COMMIT_EVIDENCE_DOMAIN: &str = "norn.fork.commit_evidence/1";
const DECISION_CARD_EVIDENCE_DOMAIN: &str = "norn.decision_card.simulated_evidence/1";

impl Receipt {
    /// Whether this receipt was minted inside a forked world (`FORK-EVIDENCE-1`).
    #[must_use]
    pub const fn is_fork_minted(&self) -> bool {
        self.body.fork_pin.is_some()
    }

    /// The durable [`ForkPin`] bound into a fork-minted receipt, or `None` for
    /// a `main`-world receipt.
    #[must_use]
    pub const fn fork_pin(&self) -> Option<&ForkPin> {
        self.body.fork_pin.as_ref()
    }
}

impl WrittenRow {
    pub(crate) const fn new(object_type: String, key: String, version: u64) -> Self {
        Self {
            object_type,
            key,
            version,
            snapshot_sha256: None,
            fork_world: None,
        }
    }
}

pub(crate) fn verify_receipt_fork_binding(
    store_pin: Option<&ForkPin>,
    body: &ReceiptBody,
) -> Result<(), CellError> {
    let corrupt = |reason: &str| CellError::LedgerCorrupt {
        seq: body.commit_seq,
        reason: reason.to_owned(),
    };
    if let Some(commit_pin) = &body.fork_pin {
        let Some(pin) = store_pin else {
            return Err(CellError::ForkEvidenceForbidden);
        };
        if commit_pin.world.is_empty()
            || commit_pin.world == MAIN_WORLD
            || commit_pin.world == commit_pin.parent
            || commit_pin.main_horizon > commit_pin.base.commits
            || body.commit_seq <= commit_pin.main_horizon
        {
            return Err(corrupt("invalid fork pin bound into receipt"));
        }
        if body.commit_seq <= pin.main_horizon {
            return Err(corrupt("fork-bound receipt precedes store main horizon"));
        }
        if commit_pin.main_horizon != pin.main_horizon {
            return Err(corrupt(
                "fork receipt main horizon disagrees with store fork pin",
            ));
        }
        if body.commit_seq > pin.base.commits && commit_pin != pin {
            return Err(corrupt("fork receipt pin disagrees with store fork pin"));
        }
        if body
            .written
            .iter()
            .any(|row| row.fork_world.as_deref() != Some(commit_pin.world.as_str()))
        {
            return Err(corrupt(
                "fork receipt written row world disagrees with receipt fork pin",
            ));
        }
    } else if body.written.iter().any(|row| row.fork_world.is_some()) {
        return Err(corrupt(
            "non-fork receipt carries a fork-world tag on a written row",
        ));
    }
    Ok(())
}

pub(crate) fn tag_written_for_fork(written: &mut [WrittenRow], fork_pin: Option<&ForkPin>) {
    if let Some(pin) = fork_pin {
        for row in written {
            row.fork_world = Some(pin.world.clone());
        }
    }
}

pub(crate) fn entry_hash_matches(entry: &LedgerEntry) -> Result<bool, CellError> {
    Ok(entry_hash_with_pin(
        entry.seq,
        entry.commit_seq,
        &entry.world,
        &entry.event,
        &entry.prev_hash,
        entry.fork_pin.as_ref(),
    )? == entry.hash)
}

pub(crate) fn receipt_entry_hash_ok(
    receipt: &ReceiptBody,
    seq: u64,
    event: &RuntimeValue,
    prev: &str,
    expected_hash: &str,
) -> Result<bool, CellError> {
    Ok(entry_hash_with_pin(
        seq,
        receipt.commit_seq,
        &receipt.world,
        event,
        prev,
        receipt.fork_pin.as_ref(),
    )? == expected_hash)
}

pub(crate) fn make_ledger_entry(
    seq: u64,
    commit_seq: u64,
    world: &str,
    event: &RuntimeValue,
    stored: RuntimeValue,
    prev: &mut String,
    fork_pin: Option<&ForkPin>,
) -> Result<LedgerEntry, CellError> {
    let hash = entry_hash_with_pin(seq, commit_seq, world, event, prev, fork_pin)?;
    Ok(LedgerEntry {
        seq,
        commit_seq,
        world: world.to_owned(),
        event: stored,
        prev_hash: std::mem::replace(prev, hash.clone()),
        hash,
        fork_pin: fork_pin.cloned(),
    })
}

/// Authenticated baseline world answer for [`SimulatedDecisionCardEvidence::build`].
#[derive(Debug, Clone, Copy)]
pub enum ComparisonBaseline<'a> {
    /// Compare `main` (baseline) against the fork answer (candidate).
    Main(&'a QueryResult),
    /// Compare another fork (baseline) against the fork answer (candidate).
    Fork(&'a ForkQuestionResult),
}

/// Explicitly simulated, pin-bound evidence for one commit minted inside a
/// fork lineage (`commit_seq > main_horizon`) under storage version 12.
///
/// Independently verifiable after the originating fork is discarded via
/// [`verify_fork_commit_evidence`], and always refused as main-world evidence
/// via [`verify_main_export_record`] / [`verify_main_receipt`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkCommitEvidence {
    /// Logical cell identity inherited unchanged from `main` (`CELL-ID-1`).
    pub cell_id: String,
    /// Always `true`: marks this commit evidence as fork-simulated.
    pub simulated: bool,
    /// Durable pin of the fork that exported this evidence (`D_i` or nested `S_i`).
    pub exporting_pin: ForkPin,
    /// Durable pin cryptographically bound into `record.receipt.body.fork_pin`
    /// (equals `exporting_pin` for commits minted in this fork, or an ancestor
    /// fork's pin for commits inherited above `main_horizon`).
    pub commit_pin: ForkPin,
    /// Contiguous local export records `(commit_seq + 1)..=exporting_pin.base.commits`
    /// cryptographically chaining `record` directly into `exporting_pin.base`
    /// when `commit_seq < exporting_pin.base.commits` (empty otherwise).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage_chain: Vec<ExportRecord>,
    /// Full local export record for this fork-minted commit.
    pub record: ExportRecord,
    /// Domain-separated Norn1 digest binding cell id, pins, lineage chain, and record.
    pub evidence_digest: String,
}

/// Persisted, auditable simulated evidence handle suitable for attachment to a
/// main-world Decision Card (`FORK-EVIDENCE-1`, Core 08 Fork Unit 2b).
///
/// Outlives fork discard/expiry/promotion: [`Self::verify`] authenticates the
/// question pins, optional world comparison, and backing
/// [`ForkCommitEvidence`] chain up to `question_revision` without needing the
/// fork store open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulatedDecisionCardEvidence {
    /// Logical cell identity inherited unchanged from `main`.
    pub cell_id: String,
    /// Always `true`: marks this Decision Card evidence as explicitly simulated.
    pub simulated: bool,
    /// Simulated execution world (`World::Fork { id }`).
    pub world: norn_kernel::World,
    /// Durable fork pin (`world`, `parent`, `base`, `main_horizon`).
    pub pin: ForkPin,
    /// Shared pre-fork admitted question pin (`norn.query/1`).
    pub base_query_pin: String,
    /// Fork-bound query pin (`norn.query.fork/1`).
    pub fork_query_pin: String,
    /// Fork-bound question lineage reference (`norn.lineage.fork/1`).
    pub question_lineage_ref: String,
    /// Fork-bound question result pin (`norn.result.fork/1`).
    pub question_result_pin: String,
    /// Fork ledger head at which the simulated question was evaluated.
    pub question_revision: LedgerHead,
    /// Optional validated world comparison (`main` vs `D_i` or `D_i` vs `S_i`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<WorldCompareResult>,
    /// Contiguous suffix of pin-bound fork commits ending at
    /// `question_revision` (empty iff `question_revision.commits <= pin.main_horizon`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commit_evidence: Vec<ForkCommitEvidence>,
    /// Domain-separated Norn1 digest over the complete simulated evidence bundle.
    pub evidence_digest: String,
}

impl SimulatedDecisionCardEvidence {
    /// Assemble and authenticate a [`SimulatedDecisionCardEvidence`] bundle
    /// from an unredacted [`ForkQuestionResult`], optional authenticated
    /// [`ComparisonBaseline`], and exported [`ForkCommitEvidence`] records.
    ///
    /// Both `fork_result` and any `baseline` have their `lineage_ref`
    /// recomputed over their exact `rows` before computing the comparison so
    /// altered operands or caller-fabricated deltas can never be sealed.
    ///
    /// # Errors
    /// `core.world.fork_invalid_world`, `core.world.fork_evidence_forbidden`,
    /// or compare/encoding errors if any input fails validation.
    pub fn build(
        cell_id: &CellId,
        fork_result: &ForkQuestionResult,
        baseline: Option<ComparisonBaseline<'_>>,
        commit_evidence: Vec<ForkCommitEvidence>,
    ) -> Result<Self, CellError> {
        verify_fork_result_lineage(fork_result)?;
        let comparison = match baseline {
            Some(ComparisonBaseline::Main(main_result)) => {
                verify_main_result_lineage(main_result)?;
                Some(compare_main_and_fork(main_result, fork_result)?)
            }
            Some(ComparisonBaseline::Fork(peer_result)) => {
                verify_fork_result_lineage(peer_result)?;
                Some(compare_forks(peer_result, fork_result)?)
            }
            None => None,
        };
        let mut bundle = Self {
            cell_id: cell_id.as_str().to_owned(),
            simulated: fork_result.simulated,
            world: fork_result.world.clone(),
            pin: fork_result.pin.clone(),
            base_query_pin: fork_result.base_query_pin.clone(),
            fork_query_pin: fork_result.fork_query_pin.clone(),
            question_lineage_ref: fork_result.result.lineage_ref.clone(),
            question_result_pin: fork_result.result_pin.clone(),
            question_revision: fork_result.result.revision.clone(),
            comparison,
            commit_evidence,
            evidence_digest: String::new(),
        };
        bundle.evidence_digest = compute_card_evidence_digest(&bundle)?;
        bundle.verify(cell_id)?;
        Ok(bundle)
    }

    /// Independently verify this persisted simulated evidence bundle without
    /// requiring the originating fork store to remain open.
    ///
    /// # Errors
    /// `core.world.fork_invalid_world`, `core.world.fork_evidence_forbidden`,
    /// `BindingMismatch`, or `LedgerCorrupt` on any tampering.
    pub fn verify(&self, expected_cell_id: &CellId) -> Result<(), CellError> {
        if self.cell_id != expected_cell_id.as_str() {
            return Err(CellError::BindingMismatch {
                key: crate::cell_identity::KEY.to_owned(),
                expected: expected_cell_id.as_str().to_owned(),
                found: self.cell_id.clone(),
            });
        }
        validate_fork_pin(&self.pin)?;
        let valid_world = matches!(
            &self.world,
            norn_kernel::World::Fork { id } if id.0 == self.pin.world
        );
        if !self.simulated
            || !valid_world
            || fork_query_pin(&self.base_query_pin, &self.pin)? != self.fork_query_pin
            || fork_result_pin(
                &self.fork_query_pin,
                &self.question_lineage_ref,
                &self.question_revision,
            )? != self.question_result_pin
        {
            return Err(CellError::ForkInvalidWorld);
        }
        if let Some(compare) = &self.comparison {
            self.verify_comparison(compare)?;
        }
        self.verify_supporting_commits(expected_cell_id)?;
        if self.evidence_digest != compute_card_evidence_digest(self)? {
            return Err(CellError::ForkEvidenceForbidden);
        }
        Ok(())
    }

    fn verify_supporting_commits(&self, expected_cell_id: &CellId) -> Result<(), CellError> {
        if self.question_revision.commits <= self.pin.main_horizon {
            if !self.commit_evidence.is_empty() || self.question_revision != self.pin.base {
                return Err(CellError::ForkInvalidWorld);
            }
            return Ok(());
        }
        if self.commit_evidence.is_empty() {
            return Err(CellError::ForkInvalidWorld);
        }
        let mut previous: Option<&ForkCommitEvidence> = None;
        for evidence in &self.commit_evidence {
            verify_fork_commit_evidence(evidence, expected_cell_id)?;
            if evidence.exporting_pin != self.pin {
                return Err(CellError::ForkInvalidWorld);
            }
            let body = &evidence.record.receipt.body;
            if body.commit_seq > self.question_revision.commits {
                return Err(CellError::ForkInvalidWorld);
            }
            if let Some(prior) = previous {
                verify_adjacent_export_records(&prior.record, &evidence.record)?;
            }
            previous = Some(evidence);
        }
        let Some(last) = previous else {
            return Err(CellError::ForkInvalidWorld);
        };
        let last_body = &last.record.receipt.body;
        if last_body.commit_seq != self.question_revision.commits
            || last_body.ledger_head != self.question_revision.hash
            || commit_end_seq(last_body)? != self.question_revision.seq
        {
            return Err(CellError::ForkInvalidWorld);
        }
        Ok(())
    }

    fn verify_comparison(&self, compare: &WorldCompareResult) -> Result<(), CellError> {
        let matches_self = |side: &crate::compute::WorldFrontierPin| {
            side.simulated
                && side.world == self.world
                && side.base_query_pin == self.base_query_pin
                && side.query_pin == self.fork_query_pin
                && side.result_pin == self.question_result_pin
                && side.revision == self.question_revision
        };
        let valid_peer = |side: &crate::compute::WorldFrontierPin| {
            side.base_query_pin == self.base_query_pin
                && match &side.world {
                    norn_kernel::World::Main => {
                        !side.simulated && side.query_pin == side.base_query_pin
                    }
                    norn_kernel::World::Fork { id } => side.simulated && !id.0.is_empty(),
                    norn_kernel::World::Historical { .. } => false,
                }
        };
        let valid_sides = (matches_self(&compare.candidate) && valid_peer(&compare.baseline))
            || (matches_self(&compare.baseline) && valid_peer(&compare.candidate));
        if !valid_sides {
            return Err(CellError::ForkInvalidWorld);
        }
        for (key, delta) in &compare.deltas {
            if delta.bucket_key != *key || !delta_arithmetic_valid(delta) {
                return Err(CellError::ForkInvalidWorld);
            }
        }
        Ok(())
    }

    /// Encode as an exact [`RuntimeValue`] for persistence inside a main-world
    /// Decision Card record.
    ///
    /// # Errors
    /// Exact runtime encoding errors.
    pub fn to_runtime_value(&self) -> Result<RuntimeValue, CellError> {
        norn_ir::runtime::encode(self).map_err(CellError::Encoding)
    }

    /// Decode from an exact [`RuntimeValue`] stored on a Decision Card record.
    ///
    /// # Errors
    /// Exact runtime decoding errors.
    pub fn from_runtime_value(value: &RuntimeValue) -> Result<Self, CellError> {
        norn_ir::runtime::decode(value).map_err(CellError::Encoding)
    }
}

/// FORK-EVIDENCE-1 (Lemma L6): authenticate a receipt as genuine `main`-world
/// evidence, refusing `core.world.fork_evidence_forbidden` if it carries any
/// fork pin or fork-minted written row tag (at any nesting depth).
///
/// # Errors
/// `core.world.fork_evidence_forbidden` for any fork-minted receipt;
/// [`CellError::LedgerCorrupt`] if `receipt.hash` disagrees with `receipt.body`
/// (preventing stripping of `fork_pin` to forge a main receipt).
pub fn verify_main_receipt(receipt: &Receipt) -> Result<(), CellError> {
    if hash_json(&receipt.body)? != receipt.hash {
        return Err(CellError::LedgerCorrupt {
            seq: receipt.body.commit_seq,
            reason: "receipt hash does not verify".to_owned(),
        });
    }
    if receipt.body.fork_pin.is_some()
        || receipt
            .body
            .written
            .iter()
            .any(|row| row.fork_world.is_some())
    {
        return Err(CellError::ForkEvidenceForbidden);
    }
    Ok(())
}

/// FORK-EVIDENCE-1 (Lemma L6): authenticate an [`ExportRecord`] as genuine
/// `main`-world evidence, refusing `core.world.fork_evidence_forbidden` if its
/// receipt, written rows, or ledger entries carry a fork pin (at any nesting
/// depth).
///
/// # Errors
/// `core.world.fork_evidence_forbidden` for any fork-minted record;
/// [`CellError::LedgerCorrupt`] if envelope, receipt, batch, or ledger entry
/// hashes do not verify.
pub fn verify_main_export_record(record: &ExportRecord) -> Result<(), CellError> {
    verify_main_receipt(&record.receipt)?;
    if record.entries.iter().any(|entry| entry.fork_pin.is_some()) {
        return Err(CellError::ForkEvidenceForbidden);
    }
    verify_export_record_integrity(record, None)
}

/// Independently verify one exported [`ForkCommitEvidence`] record without
/// needing the fork store open (`FORK-EVIDENCE-1`, Unit 2b).
///
/// # Errors
/// `core.world.fork_evidence_forbidden`, `core.world.fork_invalid_world`,
/// `BindingMismatch`, or `LedgerCorrupt` if any pin, lineage chain, cell
/// identity, receipt hash, written-row world tag, ledger entry hash, or
/// evidence digest fails.
pub fn verify_fork_commit_evidence(
    evidence: &ForkCommitEvidence,
    expected_cell_id: &CellId,
) -> Result<(), CellError> {
    if !evidence.simulated {
        return Err(CellError::ForkInvalidWorld);
    }
    if evidence.cell_id != expected_cell_id.as_str() {
        return Err(CellError::BindingMismatch {
            key: crate::cell_identity::KEY.to_owned(),
            expected: expected_cell_id.as_str().to_owned(),
            found: evidence.cell_id.clone(),
        });
    }
    validate_fork_pin(&evidence.exporting_pin)?;
    validate_fork_pin(&evidence.commit_pin)?;
    let body = &evidence.record.receipt.body;
    if evidence.commit_pin.main_horizon != evidence.exporting_pin.main_horizon
        || body.commit_seq <= evidence.exporting_pin.main_horizon
        || body.commit_seq <= evidence.commit_pin.base.commits
    {
        return Err(CellError::ForkInvalidWorld);
    }
    if body.fork_pin.as_ref() != Some(&evidence.commit_pin)
        || body
            .written
            .iter()
            .any(|row| row.fork_world.as_deref() != Some(evidence.commit_pin.world.as_str()))
    {
        return Err(CellError::ForkEvidenceForbidden);
    }
    verify_record_cell_id(&evidence.record, expected_cell_id)?;
    verify_export_record_integrity(&evidence.record, Some(&evidence.commit_pin))?;
    verify_lineage_membership(evidence, expected_cell_id)?;
    let expected_digest = compute_fork_commit_digest(
        &evidence.cell_id,
        evidence.simulated,
        &evidence.exporting_pin,
        &evidence.commit_pin,
        &evidence.lineage_chain,
        &evidence.record.hash()?,
    )?;
    if evidence.evidence_digest != expected_digest {
        return Err(CellError::ForkEvidenceForbidden);
    }
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Core 08 Fork Unit 2b (`FORK-EVIDENCE-1`): export explicitly simulated,
    /// pin-bound evidence for one commit minted after this fork lineage's
    /// `main_horizon` (`commit_seq > pin.main_horizon`).
    ///
    /// Unlike [`Self::export_record`] (which remains fenced to `main`-world
    /// commits `<= main_horizon`), this method returns a [`ForkCommitEvidence`]
    /// bound to the fork's durable [`ForkPin`] (plus cryptographic lineage
    /// records chaining inherited commits to `pin.base`) so Decision Cards can
    /// persist and independently verify simulated evidence even after the fork
    /// is discarded.
    ///
    /// # Errors
    /// `core.world.simulation_forbidden` on `main`; `core.world.fork_closed`
    /// on a closed fork; `core.world.fork_invalid_world` for commits at or
    /// below `main_horizon`; `core.world.fork_evidence_forbidden` if a legacy
    /// fork commit lacks its pin binding; store/verification errors otherwise.
    pub fn export_fork_commit_evidence(
        &self,
        commit_seq: u64,
    ) -> Result<Option<ForkCommitEvidence>, CellError> {
        let Some(pin) = self.fork_pin().cloned() else {
            return Err(CellError::WorldSimulationForbidden);
        };
        self.refuse_closed_fork()?;
        if commit_seq <= pin.main_horizon {
            return Err(CellError::ForkInvalidWorld);
        }
        self.read().check_history("fork commit evidence")?;
        let Some(record) = self.local_export_record(commit_seq)? else {
            return Ok(None);
        };
        let Some(commit_pin) = record.receipt.body.fork_pin.clone() else {
            return Err(CellError::ForkEvidenceForbidden);
        };
        let lineage_chain = self.collect_lineage_chain(&pin, commit_seq)?;
        let Some(cell_id) = crate::cell_identity::from_bindings(&record.bindings)? else {
            return Err(CellError::BindingMismatch {
                key: crate::cell_identity::KEY.to_owned(),
                expected: "a qualified cell export".to_owned(),
                found: String::new(),
            });
        };
        self.require_cell_id(&cell_id)?;
        let evidence_digest = compute_fork_commit_digest(
            cell_id.as_str(),
            true,
            &pin,
            &commit_pin,
            &lineage_chain,
            &record.hash()?,
        )?;
        let evidence = ForkCommitEvidence {
            cell_id: cell_id.as_str().to_owned(),
            simulated: true,
            exporting_pin: pin,
            commit_pin,
            lineage_chain,
            record,
            evidence_digest,
        };
        verify_fork_commit_evidence(&evidence, &cell_id)?;
        Ok(Some(evidence))
    }

    fn collect_lineage_chain(
        &self,
        exporting_pin: &ForkPin,
        commit_seq: u64,
    ) -> Result<Vec<ExportRecord>, CellError> {
        if commit_seq >= exporting_pin.base.commits {
            return Ok(Vec::new());
        }
        let mut chain = Vec::new();
        let mut next_seq = commit_seq
            .checked_add(1)
            .ok_or(CellError::OutOfRange("commit_seq"))?;
        while next_seq <= exporting_pin.base.commits {
            let link = self
                .local_export_record(next_seq)?
                .ok_or(CellError::ForkInvalidWorld)?;
            chain.push(link);
            next_seq = next_seq
                .checked_add(1)
                .ok_or(CellError::OutOfRange("commit_seq"))?;
        }
        Ok(chain)
    }
}

fn verify_record_cell_id(record: &ExportRecord, expected: &CellId) -> Result<(), CellError> {
    let Some(found) = crate::cell_identity::from_bindings(&record.bindings)? else {
        return Err(CellError::BindingMismatch {
            key: crate::cell_identity::KEY.to_owned(),
            expected: expected.as_str().to_owned(),
            found: String::new(),
        });
    };
    if found != *expected {
        return Err(CellError::BindingMismatch {
            key: crate::cell_identity::KEY.to_owned(),
            expected: expected.as_str().to_owned(),
            found: found.as_str().to_owned(),
        });
    }
    Ok(())
}

fn verify_adjacent_export_records(
    prior: &ExportRecord,
    current: &ExportRecord,
) -> Result<(), CellError> {
    let prior_body = &prior.receipt.body;
    let cur_body = &current.receipt.body;
    if prior_body.commit_seq.checked_add(1) != Some(cur_body.commit_seq) {
        return Err(CellError::ForkInvalidWorld);
    }
    if current.previous != prior.hash()? {
        return Err(CellError::LedgerCorrupt {
            seq: cur_body.commit_seq,
            reason: "adjacent fork export record previous hash mismatch".to_owned(),
        });
    }
    let expected_first = commit_end_seq(prior_body)?
        .checked_add(1)
        .ok_or(CellError::OutOfRange("seq"))?;
    if cur_body.first_event_seq != expected_first {
        return Err(CellError::LedgerCorrupt {
            seq: cur_body.commit_seq,
            reason: "adjacent fork export record event span mismatch".to_owned(),
        });
    }
    if let Some(first_entry) = current.entries.first() {
        if first_entry.prev_hash != prior_body.ledger_head {
            return Err(CellError::LedgerCorrupt {
                seq: first_entry.seq,
                reason: "adjacent fork ledger entry prev_hash mismatch".to_owned(),
            });
        }
    } else if cur_body.ledger_head != prior_body.ledger_head {
        return Err(CellError::LedgerCorrupt {
            seq: cur_body.commit_seq,
            reason: "zero-event fork commit changed ledger head".to_owned(),
        });
    }
    Ok(())
}

fn verify_lineage_membership(
    evidence: &ForkCommitEvidence,
    expected_cell_id: &CellId,
) -> Result<(), CellError> {
    let body = &evidence.record.receipt.body;
    if body.commit_seq > evidence.exporting_pin.base.commits {
        return if evidence.commit_pin == evidence.exporting_pin && evidence.lineage_chain.is_empty()
        {
            Ok(())
        } else {
            Err(CellError::ForkInvalidWorld)
        };
    }
    if evidence.commit_pin.world == evidence.exporting_pin.world {
        return Err(CellError::ForkInvalidWorld);
    }
    let expected_len = evidence
        .exporting_pin
        .base
        .commits
        .checked_sub(body.commit_seq)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(CellError::OutOfRange("lineage chain"))?;
    if evidence.lineage_chain.len() != expected_len {
        return Err(CellError::ForkInvalidWorld);
    }
    let mut tip = &evidence.record;
    for link in &evidence.lineage_chain {
        let Some(link_pin) = link.receipt.body.fork_pin.as_ref() else {
            return Err(CellError::ForkEvidenceForbidden);
        };
        validate_fork_pin(link_pin)?;
        if link_pin.main_horizon != evidence.exporting_pin.main_horizon
            || link_pin.world == evidence.exporting_pin.world
            || link.receipt.body.commit_seq <= link_pin.base.commits
        {
            return Err(CellError::ForkInvalidWorld);
        }
        if link
            .receipt
            .body
            .written
            .iter()
            .any(|row| row.fork_world.as_deref() != Some(link_pin.world.as_str()))
        {
            return Err(CellError::ForkEvidenceForbidden);
        }
        verify_record_cell_id(link, expected_cell_id)?;
        verify_export_record_integrity(link, Some(link_pin))?;
        verify_adjacent_export_records(tip, link)?;
        tip = link;
    }
    let tip_body = &tip.receipt.body;
    if tip_body.commit_seq != evidence.exporting_pin.base.commits
        || tip_body.ledger_head != evidence.exporting_pin.base.hash
        || commit_end_seq(tip_body)? != evidence.exporting_pin.base.seq
    {
        return Err(CellError::ForkInvalidWorld);
    }
    Ok(())
}

fn commit_end_seq(body: &ReceiptBody) -> Result<u64, CellError> {
    if body.event_count == 0 {
        body.first_event_seq
            .checked_sub(1)
            .ok_or(CellError::OutOfRange("seq"))
    } else {
        body.first_event_seq
            .checked_add(body.event_count)
            .and_then(|end| end.checked_sub(1))
            .ok_or(CellError::OutOfRange("seq"))
    }
}

fn delta_arithmetic_valid(delta: &WorldCompareDelta) -> bool {
    if delta.suppressed {
        return delta.baseline_value.is_none()
            && delta.candidate_value.is_none()
            && delta.delta_value.is_none();
    }
    match (delta.baseline_value, delta.candidate_value) {
        (Some(base), Some(cand)) => delta.delta_value == cand.checked_sub(base),
        (None, Some(cand)) => delta.delta_value.is_none() || delta.delta_value == Some(cand),
        (Some(base), None) => {
            delta.delta_value.is_none() || delta.delta_value == Decimal::ZERO.checked_sub(base)
        }
        (None, None) => delta.delta_value.is_none(),
    }
}

fn verify_export_record_integrity(
    record: &ExportRecord,
    expected_pin: Option<&ForkPin>,
) -> Result<(), CellError> {
    let body = &record.receipt.body;
    let corrupt = |seq: u64, reason: &str| CellError::LedgerCorrupt {
        seq,
        reason: reason.to_owned(),
    };
    if record.format != EXPORT_FORMAT
        || record.store_format != STORE_FORMAT
        || record.ddl_hash != hash_json(&record.ddl_sql)?
    {
        return Err(corrupt(
            body.commit_seq,
            "export record envelope or DDL hash does not verify",
        ));
    }
    if hash_json(body)? != record.receipt.hash
        || body.request_hash != hash_json(&record.batch)?
        || body.world != record.batch.world
        || body.logical_time != record.batch.logical_time
        || body.idempotency_key != record.batch.idempotency_key
    {
        return Err(corrupt(
            body.commit_seq,
            "export record receipt or batch hash does not verify",
        ));
    }
    let entry_count =
        u64::try_from(record.entries.len()).map_err(|_overflow| CellError::OutOfRange("events"))?;
    if entry_count != body.event_count || record.entries.len() != record.batch.events.len() {
        return Err(corrupt(
            body.commit_seq,
            "export record event count disagrees with receipt",
        ));
    }
    let mut prev_hash: Option<&str> = None;
    for (index, (entry, event)) in record.entries.iter().zip(&record.batch.events).enumerate() {
        let offset = u64::try_from(index).map_err(|_overflow| CellError::OutOfRange("seq"))?;
        let expected_seq = body
            .first_event_seq
            .checked_add(offset)
            .ok_or(CellError::OutOfRange("seq"))?;
        if entry.fork_pin.as_ref() != expected_pin {
            return Err(CellError::ForkEvidenceForbidden);
        }
        if entry.seq != expected_seq
            || entry.commit_seq != body.commit_seq
            || entry.world != body.world
            || entry.event != *event
            || prev_hash.is_some_and(|prior| entry.prev_hash != prior)
        {
            return Err(corrupt(
                entry.seq,
                "export record ledger entry disagrees with receipt or batch",
            ));
        }
        if entry_hash_with_pin(
            entry.seq,
            entry.commit_seq,
            &entry.world,
            &entry.event,
            &entry.prev_hash,
            expected_pin,
        )? != entry.hash
        {
            return Err(corrupt(entry.seq, "ledger entry hash does not verify"));
        }
        prev_hash = Some(entry.hash.as_str());
    }
    if let Some(last_hash) = prev_hash
        && last_hash != body.ledger_head
    {
        return Err(corrupt(
            body.commit_seq,
            "export record ledger head disagrees with receipt",
        ));
    }
    Ok(())
}

fn validate_fork_pin(pin: &ForkPin) -> Result<(), CellError> {
    if pin.world.is_empty()
        || pin.world == MAIN_WORLD
        || pin.world == pin.parent
        || pin.main_horizon > pin.base.commits
    {
        return Err(CellError::ForkInvalidWorld);
    }
    Ok(())
}

fn compute_fork_commit_digest(
    cell_id: &str,
    simulated: bool,
    exporting_pin: &ForkPin,
    commit_pin: &ForkPin,
    lineage_chain: &[ExportRecord],
    record_hash: &str,
) -> Result<String, CellError> {
    let mut chain_hashes = Vec::with_capacity(lineage_chain.len());
    for link in lineage_chain {
        chain_hashes.push(link.hash()?);
    }
    hash_json(&(
        FORK_COMMIT_EVIDENCE_DOMAIN,
        cell_id,
        simulated,
        exporting_pin,
        commit_pin,
        chain_hashes,
        record_hash,
    ))
}

fn compute_card_evidence_digest(
    bundle: &SimulatedDecisionCardEvidence,
) -> Result<String, CellError> {
    let commit_digests: Vec<&str> = bundle
        .commit_evidence
        .iter()
        .map(|entry| entry.evidence_digest.as_str())
        .collect();
    hash_json(&(
        DECISION_CARD_EVIDENCE_DOMAIN,
        &bundle.cell_id,
        bundle.simulated,
        &bundle.world,
        &bundle.pin,
        &bundle.base_query_pin,
        &bundle.fork_query_pin,
        &bundle.question_lineage_ref,
        &bundle.question_result_pin,
        &bundle.question_revision,
        &bundle.comparison,
        commit_digests,
    ))
}
