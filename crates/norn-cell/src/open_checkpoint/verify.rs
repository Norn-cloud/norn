//! OPEN-CHECKPOINT-1 (#1533): open-time history verification, definition walk,
//! value-history range verification, and checkpoint advancement on [`CellStore`].

use std::cell::RefCell;

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::history::{decode_snapshot, unproven_snapshot};
use crate::open_checkpoint::{self, Loaded, OpenCheckpoint};
use crate::quantity::FormatBoundary;
use crate::read::{LedgerHead, StoreReader, active_definition_version, to_i64, to_u64};
use crate::store::{CellStore, InstalledDefinitionSet, Receipt};
use crate::system::{FACT_VALUES, GENESIS_HASH, RECEIPT};
use crate::transparency::ReceiptLog;

/// Where a walk of the definition history stands: the next version it
/// expects, how many retained versions it has passed, the version in force
/// and the commit that installed it.
#[derive(Debug, Clone)]
pub(crate) struct DefinitionWalk {
    pub(crate) next_version: u64,
    pub(crate) version_index: usize,
    pub(crate) active_version: u64,
    pub(crate) previous_install_seq: u64,
}

impl Default for DefinitionWalk {
    fn default() -> Self {
        Self {
            next_version: 1,
            version_index: 0,
            active_version: 0,
            previous_install_seq: 0,
        }
    }
}

impl DefinitionWalk {
    pub(crate) fn installed(&mut self, version: &InstalledDefinitionSet) -> Result<(), CellError> {
        self.active_version = version.version;
        self.previous_install_seq = version.commit_seq;
        self.next_version = self
            .next_version
            .checked_add(1)
            .ok_or(CellError::OutOfRange("definitions version"))?;
        self.version_index = self
            .version_index
            .checked_add(1)
            .ok_or(CellError::OutOfRange("definition history index"))?;
        Ok(())
    }
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Every value version written after `commits` matches its receipt's
    /// digest (OPEN-CHECKPOINT-1: `commits` is the open checkpoint's, or 0).
    pub(crate) fn verify_value_history_after(&self, commits: u64) -> Result<(), CellError> {
        self.verify_value_history_range(commits, u64::MAX)
    }

    /// Verify one committed written fact row against `_norn_fact_values`.
    pub(crate) fn verify_written_snapshot(
        &self,
        world: &str,
        commit_seq: u64,
        row: &crate::store::WrittenRow,
    ) -> Result<(), CellError> {
        let stored: Option<(String, String, String)> = self.conn.query_optional(
            &format!("SELECT root_type,snapshot,hash FROM \"{FACT_VALUES}\" WHERE world=?1 AND commit_seq=?2 AND object_type=?3 AND id=?4 AND version=?5"),
            params![
                world,
                to_i64(commit_seq, "fact commit")?,
                row.object_type,
                row.key,
                to_i64(row.version, "fact version")?,
            ],
            |record| Ok((record.get(0)?, record.get(1)?, record.get(2)?)),
        )?;
        let (root, encoded, hash) = stored.ok_or_else(unproven_snapshot)?;
        let snapshot = decode_snapshot(
            world,
            &row.object_type,
            &row.key,
            row.version,
            &encoded,
            &hash,
        )?;
        if snapshot.root_type != root
            || snapshot.object_type != row.object_type
            || !crate::receipt_commitment::receipt_digest_matches(
                row.snapshot_sha256.as_deref(),
                &hash,
            )
        {
            return Err(unproven_snapshot());
        }
        Ok(())
    }

    /// [`Self::verify_value_history_after`] for the commits after `after`
    /// through `through` only.
    pub(crate) fn verify_value_history_range(
        &self,
        after: u64,
        through: u64,
    ) -> Result<(), CellError> {
        for receipt in self.receipt_stream_from(after) {
            let receipt = receipt?;
            if receipt.body.commit_seq > through {
                break;
            }
            for row in &receipt.body.written {
                self.verify_written_snapshot(&receipt.body.world, receipt.body.commit_seq, row)?;
            }
        }
        Ok(())
    }
}

/// Result of a periodic post-commit checkpoint attempt.
enum CheckpointAdvance {
    Recorded {
        checkpoint: OpenCheckpoint,
        genesis_dv: Option<u64>,
    },
    Skipped,
    NeedsGenesisScrub,
}

/// What [`CellStore::verify_history`] admits and caches on open.
pub(crate) struct VerifiedHistory {
    pub(crate) head: LedgerHead,
    pub(crate) boundary: Option<FormatBoundary>,
    pub(crate) transparency: ReceiptLog,
    pub(crate) trusted: Option<OpenCheckpoint>,
    pub(crate) data_version: Option<u64>,
}

impl<E: SqlEngine> CellStore<E> {
    /// S12 (#1551 round 5): retry any pending fence persistence and fail closed
    /// on either a local or durable fence (sibling handle).
    pub(crate) fn check_writable(&self) -> Result<(), CellError> {
        self.persist_pending_fence();
        self.scrub.borrow().refuse_fenced()?;
        open_checkpoint::check_fence(&self.conn)
    }

    /// Run `f` inside a transaction after checking local and durable fences.
    pub(crate) fn write_tx<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.check_writable()?;
        self.conn.transaction(|| {
            open_checkpoint::check_fence(&self.conn)?;
            f()
        })
    }

    /// Retry persisting a local fence marker that previously failed transiently
    /// (S12, #1551 round 5).
    pub(crate) fn persist_pending_fence(&self) {
        let Some(finding) = self.scrub.borrow().pending_fence_finding() else {
            return;
        };
        match open_checkpoint::fence_durably(&self.conn, &finding) {
            Ok(()) => self.scrub.borrow_mut().mark_fence_persisted(),
            Err(fence_error) => {
                tracing::warn!(%fence_error, "history scrub: durable fence write failed; will retry");
            }
        }
    }

    /// Observe any durable fence recorded by a sibling handle (S12).
    pub(crate) fn sync_durable_fence(&self) {
        if self.scrub.borrow().is_fenced() {
            return;
        }
        if let Ok(Some(finding)) = open_checkpoint::read_fence(&self.conn) {
            self.scrub.borrow_mut().record_durable_fence(finding);
        }
    }

    /// Return the scrub gate after retrying any pending fence persistence.
    pub(crate) fn served_gate(&self) -> crate::scrub::Gate {
        if self.scrub.borrow().needs_fence_persist() {
            self.persist_pending_fence();
        }
        self.scrub.borrow().gate()
    }

    /// Served reader carrying the history gate and live fence tracking (S12).
    pub(crate) fn served_reader(&self) -> StoreReader<'_, E> {
        self.verifier()
            .with_history_gate(self.served_gate())
            .with_live_scrub(&self.scrub)
    }

    /// Complete open/provisioning-replay checkpoint recording and scrub setup.
    pub(crate) fn finish_open_scrub(
        &self,
        upgrading: bool,
        verified: &VerifiedHistory,
    ) -> Result<(), CellError> {
        let recorded = if upgrading && verified.trusted.is_none() {
            self.record_open_checkpoint(
                None,
                &verified.head,
                verified.boundary,
                &verified.transparency,
            )?
        } else {
            None
        };
        let keyed = self.conn.checkpoint_key().is_some()
            && crate::cell_identity::read(&self.conn)?.is_some();
        self.scrub.replace(crate::scrub::Scrub::after_open(
            keyed,
            verified.trusted.clone(),
            recorded,
            &verified.head,
            verified.boundary,
            verified.data_version,
        ));
        Ok(())
    }

    /// Admit the file's history (OPEN-CHECKPOINT-1, #1533): from its open
    /// checkpoint when it has one, else from genesis. Refuses immediately if
    /// the cell carries a durable scrub fence (S12).
    pub(crate) fn verify_history(
        &self,
        pass: &RefCell<crate::history::VerifiedReceipts>,
    ) -> Result<VerifiedHistory, CellError> {
        // S12: a cell fenced durably by an earlier scrub refuses reopen.
        open_checkpoint::check_fence(&self.conn)?;
        let mut drop_stale_checkpoint = false;
        let checkpoint = match open_checkpoint::load(&self.conn)? {
            Loaded::Authentic(checkpoint) => {
                let last = self.verifier().next_commit_seq()?.saturating_sub(1);
                if last < checkpoint.head.commits {
                    tracing::warn!(
                        checkpoint = checkpoint.head.commits,
                        last,
                        "open checkpoint: past the file's history (a trimmed restore); verifying from genesis"
                    );
                    drop_stale_checkpoint = true;
                    None
                } else {
                    Some(checkpoint)
                }
            }
            Loaded::Absent | Loaded::Unkeyed => None,
            loaded @ (Loaded::Unbound | Loaded::Unauthentic) => {
                tracing::warn!(
                    ?loaded,
                    "open checkpoint: not authentic under the host's key; verifying from genesis"
                );
                None
            }
        };
        let from = checkpoint
            .as_ref()
            .map_or_else(LedgerHead::genesis, |checkpoint| checkpoint.head.clone());
        let transparency = crate::transparency::load_bound(&self.conn, checkpoint.as_ref())?;
        crate::cell_identity::verify_history_after(&self.conn, checkpoint.as_ref())?;
        let reader = self.verifier().with_transaction_verified_receipts(pass);
        let (head, boundary) = match &checkpoint {
            Some(checkpoint) => {
                reader.verify_ledger_from_open_checkpoint(&from, checkpoint.boundary)?
            }
            None => reader.verify_ledger_with_boundary(&from)?,
        };
        self.verify_definition_history(checkpoint.as_ref())?;
        self.verifier().verify_value_history_after(from.commits)?;
        crate::identity::validate(&self.verifier())?;
        // A genesis verification replayed the quantity projections above.
        if let Some(checkpoint) = &checkpoint {
            self.verify_quantity_from_checkpoint(checkpoint)?;
        }
        if transparency.len() != head.commits {
            return Err(open_checkpoint::corrupt(
                head.commits,
                "the receipt log and the ledger disagree on the commit count",
            ));
        }
        // S10 (#1551 round 5): after a trimmed restore passes genesis
        // verification, drop the stale past-end checkpoint immediately so even
        // histories <= MAX_ADVANCE (<= 256 commits) do not retain it.
        if drop_stale_checkpoint {
            open_checkpoint::drop_row(&self.conn)?;
        }
        let data_version = self.conn.external_data_version()?;
        Ok(VerifiedHistory {
            head,
            boundary,
            transparency,
            trusted: checkpoint,
            data_version,
        })
    }

    /// S1 (#1551): replay the quantity projections from every quantity
    /// receipt, as a genesis open does, reading only those receipts.
    fn verify_quantity_from_checkpoint(
        &self,
        checkpoint: &OpenCheckpoint,
    ) -> Result<(), CellError> {
        let covered = checkpoint.head.commits;
        let mut digest = open_checkpoint::quantity_seed()?;
        let mut commits = Vec::new();
        for row in self
            .conn
            .query(open_checkpoint::QUANTITY_RECEIPTS_SQL, params![])?
        {
            let row = SqlRow(row);
            let commit = to_u64(row.get(0)?, "quantity receipt commit")?;
            if commit > covered {
                break;
            }
            digest = open_checkpoint::quantity_extend(&digest, commit, &row.get::<String>(1)?)?;
            commits.push(commit);
        }
        let reader = self.verifier();
        if digest != checkpoint.quantity_receipts {
            tracing::warn!(
                commits = covered,
                "open checkpoint: the indexed quantity receipts do not fold to its digest; replaying every receipt"
            );
            return crate::quantity::verify_projections(&self.conn, reader.receipt_stream());
        }
        let covered_receipts = commits.into_iter().map(|commit| {
            reader
                .receipt_at(commit)?
                .filter(|receipt| receipt.body.quantity.is_some())
                .ok_or_else(|| {
                    open_checkpoint::corrupt(commit, "a quantity receipt it names is gone")
                })
        });
        let suffix = reader
            .receipt_stream_from(covered)
            .filter(|receipt| !matches!(receipt, Ok(receipt) if receipt.body.quantity.is_none()));
        crate::quantity::verify_projections(&self.conn, covered_receipts.chain(suffix))
    }

    /// The `quantity_receipts` digest through commit `to`: `previous`'s,
    /// extended by the quantity receipts after it (each body hash-checked as
    /// it is read), or folded from the first receipt when there is none.
    fn quantity_receipts_through(
        &self,
        previous: Option<OpenCheckpoint>,
        to: u64,
    ) -> Result<String, CellError> {
        let (from, mut digest) = match previous {
            Some(previous) => (previous.head.commits, previous.quantity_receipts),
            None => (0, open_checkpoint::quantity_seed()?),
        };
        for receipt in self.verifier().receipt_stream_from(from) {
            let receipt = receipt?;
            if receipt.body.commit_seq > to {
                break;
            }
            if receipt.body.quantity.is_some() {
                digest = open_checkpoint::quantity_extend(
                    &digest,
                    receipt.body.commit_seq,
                    &receipt.hash,
                )?;
            }
        }
        Ok(digest)
    }

    /// OPEN-CHECKPOINT-1: record `head`, which this engine verified, with the
    /// root of `log`, its receipt digests, as the cell's open checkpoint.
    pub(crate) fn record_open_checkpoint(
        &self,
        previous: Option<OpenCheckpoint>,
        head: &LedgerHead,
        boundary: Option<FormatBoundary>,
        log: &ReceiptLog,
    ) -> Result<Option<OpenCheckpoint>, CellError> {
        if log.len() != head.commits {
            return Err(open_checkpoint::corrupt(
                head.commits,
                "the receipt log and the ledger disagree on the commit count",
            ));
        }
        let quantity_receipts = self.quantity_receipts_through(previous, head.commits)?;
        let checkpoint = self.checkpoint_at(
            head,
            boundary,
            open_checkpoint::root_hex(log.root().as_bytes()),
            quantity_receipts,
        )?;
        Ok(open_checkpoint::record(&self.conn, &checkpoint)?.then_some(checkpoint))
    }

    /// The checkpoint at `head` with its receipt root and quantity digest:
    /// the export record and definition pins are read here.
    pub(crate) fn checkpoint_at(
        &self,
        head: &LedgerHead,
        boundary: Option<FormatBoundary>,
        receipts_root: String,
        quantity_receipts: String,
    ) -> Result<OpenCheckpoint, CellError> {
        Ok(OpenCheckpoint {
            head: head.clone(),
            receipts_root,
            boundary,
            quantity_receipts,
            export: crate::cell_identity::last_record_through(&self.conn, head.commits)?,
            definitions: open_checkpoint::definitions_digest(
                &self.verifier().installed_definition_versions()?,
                head.commits,
            )?,
        })
    }

    /// S10: verify inside the checkpoint's own transaction that `target` still
    /// anchors `_norn_ledger` and `_norn_receipt`, and that the receipt digest
    /// at `target.commits` matches `expected_receipt_hash` (when non-empty).
    pub(crate) fn verify_target_tail_anchor(
        &self,
        target: &LedgerHead,
        expected_receipt_hash: Option<&str>,
    ) -> Result<(), CellError> {
        let reader = self.verifier();
        let tail_commits = reader.next_commit_seq()?.saturating_sub(1);
        if tail_commits < target.commits {
            return Err(open_checkpoint::corrupt(
                target.commits,
                "the ledger shrank behind the target checkpoint",
            ));
        }
        if target.commits == 0 {
            if target.seq != 0 || target.hash != GENESIS_HASH {
                return Err(open_checkpoint::corrupt(0, "invalid genesis target head"));
            }
            return Ok(());
        }
        let Some(receipt) = reader.receipt_at(target.commits)? else {
            return Err(open_checkpoint::corrupt(
                target.commits,
                "the target checkpoint receipt disappeared",
            ));
        };
        let end_seq = crate::history_selection::receipt_last_sequence(
            receipt.body.first_event_seq,
            receipt.body.event_count,
        );
        if receipt.body.ledger_head != target.hash
            || end_seq != Some(target.seq)
            || expected_receipt_hash.is_some_and(|hash| receipt.hash != hash)
        {
            return Err(open_checkpoint::corrupt(
                target.commits,
                "the target checkpoint no longer anchors the receipt log",
            ));
        }
        if target.seq > 0 {
            let entry = reader.ledger_page(target.seq.saturating_sub(1), 1)?;
            if !entry
                .first()
                .is_some_and(|e| e.seq == target.seq && e.hash == target.hash)
            {
                return Err(open_checkpoint::corrupt(
                    target.commits,
                    "the target checkpoint no longer anchors the ledger",
                ));
            }
        }
        Ok(())
    }

    /// OPEN-CHECKPOINT-1: after `receipt`'s commit, every [`open_checkpoint::EVERY`]
    /// commits, advance the checkpoint inside its own transaction after checking
    /// the live database tail and durable fence (S3, S10, S12).
    pub(crate) fn checkpoint_after_commit(&self, receipt: &Receipt) {
        self.persist_pending_fence();
        let commits = receipt.body.commit_seq;
        if commits == 0
            || !commits.is_multiple_of(open_checkpoint::EVERY)
            || self.fork_pin().is_some()
            || self.conn.checkpoint_key().is_none()
        {
            return;
        }
        let gap_ok = |from: u64| commits.saturating_sub(from) <= open_checkpoint::MAX_ADVANCE;
        let advance = self.scrub.borrow().advancement();
        let previous = match advance {
            crate::scrub::Advance::Hold => return,
            crate::scrub::Advance::From(previous) if gap_ok(previous.head.commits) => {
                Some(previous)
            }
            crate::scrub::Advance::Seed if gap_ok(0) => None,
            crate::scrub::Advance::From(_) | crate::scrub::Advance::Seed => {
                self.start_catch_up(receipt);
                return;
            }
        };
        let baseline_dv = self.scrub.borrow().baseline_dv();
        let recorded = self
            .conn
            .transaction(|| self.advance_checkpoint_tx(receipt, previous.as_ref(), baseline_dv));
        match recorded {
            Ok(CheckpointAdvance::Recorded {
                checkpoint,
                genesis_dv,
            }) => self.scrub.borrow_mut().validated(checkpoint, genesis_dv),
            Ok(CheckpointAdvance::Skipped) => self.scrub.borrow_mut().invalidate(),
            Ok(CheckpointAdvance::NeedsGenesisScrub) => {
                self.scrub.borrow_mut().invalidate();
                self.start_catch_up(receipt);
            }
            Err(error) => {
                tracing::warn!(%error, commits, "open checkpoint not recorded; the last one stands");
            }
        }
    }

    fn advance_checkpoint_tx(
        &self,
        receipt: &Receipt,
        previous: Option<&OpenCheckpoint>,
        baseline_dv: Option<u64>,
    ) -> Result<CheckpointAdvance, CellError> {
        let commits = receipt.body.commit_seq;
        let expected_tail_seq = crate::history_selection::receipt_last_sequence(
            receipt.body.first_event_seq,
            receipt.body.event_count,
        );
        // S12: never record a checkpoint on a durably fenced cell.
        open_checkpoint::check_fence(&self.conn)?;
        // S10 (#1551 round 9): detect any external write since the last
        // fully verified baseline; never advance via suffix-only checks or
        // absorb a sibling edit into `baseline_dv`.
        let current_dv = self.conn.external_data_version()?;
        let external_write = (baseline_dv.is_some() && current_dv != baseline_dv)
            || (self.conn.read_isolation() != crate::engine::ReadIsolation::ExclusiveSynchronous
                && current_dv.is_none());
        if external_write {
            return Ok(CheckpointAdvance::NeedsGenesisScrub);
        }
        // S10: check the database tail inside the checkpoint transaction
        // BEFORE verifying, so concurrent commits never advance the
        // checkpoint past unverified state or exceed MAX_ADVANCE.
        let tail_commits = self.verifier().next_commit_seq()?.saturating_sub(1);
        if tail_commits != commits {
            return Ok(CheckpointAdvance::Skipped);
        }
        let (tail_seq, tail_hash) = self.verifier().ledger_tail()?;
        if expected_tail_seq != Some(tail_seq) || tail_hash != receipt.body.ledger_head {
            return Ok(CheckpointAdvance::Skipped);
        }
        match (previous, open_checkpoint::load(&self.conn)?) {
            (Some(prev), Loaded::Authentic(row)) if row == *prev => {}
            (Some(_), _) => return Ok(CheckpointAdvance::Skipped),
            (None, Loaded::Authentic(row))
                if row.head.commits <= tail_commits && row.head.commits >= commits =>
            {
                return Ok(CheckpointAdvance::Skipped);
            }
            (None, _) => {}
        }
        let from = previous.map_or_else(LedgerHead::genesis, |prev| prev.head.clone());
        if from.commits >= commits
            || commits.saturating_sub(from.commits) > open_checkpoint::MAX_ADVANCE
        {
            return Ok(CheckpointAdvance::Skipped);
        }
        let pass = RefCell::default();
        let reader = self.verifier().with_transaction_verified_receipts(&pass);
        let (head, boundary) = match previous {
            Some(prev) => reader.verify_ledger_from_open_checkpoint(&from, prev.boundary)?,
            None => reader.verify_ledger_with_boundary(&from)?,
        };
        if head.commits != commits || head.seq != tail_seq || head.hash != tail_hash {
            return Ok(CheckpointAdvance::Skipped);
        }
        let log = self.transparency.borrow();
        self.receipt_digests_match_log(from.commits, &log)?;
        crate::cell_identity::verify_history_after(&self.conn, previous)?;
        self.verify_definition_history(previous)?;
        self.verifier().verify_value_history_after(from.commits)?;
        let recorded = self.record_open_checkpoint(previous.cloned(), &head, boundary, &log)?;
        Ok(recorded.map_or(CheckpointAdvance::Skipped, |checkpoint| {
            CheckpointAdvance::Recorded {
                checkpoint,
                genesis_dv: previous.is_none().then_some(current_dv).flatten(),
            }
        }))
    }

    /// S10 (#1551 round 3): hand a checkpoint gap longer than a commit may
    /// verify to the scrub, which closes it off the commit path from the
    /// validated checkpoint (from genesis with none) to `receipt`'s head.
    fn start_catch_up(&self, receipt: &Receipt) {
        let Some(seq) = crate::history_selection::receipt_last_sequence(
            receipt.body.first_event_seq,
            receipt.body.event_count,
        ) else {
            return;
        };
        let target = LedgerHead {
            seq,
            hash: receipt.body.ledger_head.clone(),
            commits: receipt.body.commit_seq,
        };
        let boundary = self
            .quantity_boundary_cached()
            .filter(|boundary| boundary.seq <= seq);
        self.scrub.borrow_mut().catch_up(target, boundary);
    }

    /// The stored receipt digests after commit `from` are the leaves of `log`
    /// at their positions, and `log` holds no more: the root a checkpoint
    /// records is built from the receipts it verified.
    fn receipt_digests_match_log(&self, from: u64, log: &ReceiptLog) -> Result<(), CellError> {
        let mut expected = from;
        for row in self.conn.query(
            &format!(
                "SELECT \"commit_seq\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" > ?1 ORDER BY \"commit_seq\""
            ),
            params![to_i64(from, "commit_seq")?],
        )? {
            let row = SqlRow(row);
            let commit = to_u64(row.get(0)?, "commit_seq")?;
            expected = expected
                .checked_add(1)
                .ok_or(CellError::OutOfRange("commit_seq"))?;
            let leaf = usize::try_from(commit.saturating_sub(1))
                .ok()
                .and_then(|index| log.get(index));
            if commit != expected || leaf != Some(&row.get::<String>(1)?) {
                return Err(open_checkpoint::corrupt(
                    commit,
                    "a stored receipt digest is not the one this handle verified",
                ));
            }
        }
        if log.len() != expected {
            return Err(open_checkpoint::corrupt(
                expected,
                "the receipt log and the ledger disagree on the commit count",
            ));
        }
        Ok(())
    }

    /// Verify the retained definition history from the receipts after
    /// `checkpoint` (OPEN-CHECKPOINT-1; from genesis with none).
    pub(crate) fn verify_definition_history(
        &self,
        checkpoint: Option<&OpenCheckpoint>,
    ) -> Result<(), CellError> {
        let commits = checkpoint.map_or(0, |checkpoint| checkpoint.head.commits);
        let reader = self.verifier();
        let versions = reader.installed_definition_versions()?;
        let mut walk = match checkpoint {
            Some(checkpoint) => self.definitions_at_checkpoint(&reader, &versions, checkpoint)?,
            None => DefinitionWalk::default(),
        };
        for receipt in reader.receipt_stream_from(commits) {
            self.definition_walk_step(&reader, &versions, &mut walk, &receipt?)?;
        }
        if walk.version_index != versions.len() {
            return Err(CellError::LedgerCorrupt {
                seq: versions
                    .get(walk.version_index)
                    .map_or(0, |version| version.commit_seq),
                reason: "definition history extends beyond the receipt ledger".to_owned(),
            });
        }
        let active = active_definition_version(&self.conn)?;
        if active != (walk.active_version != 0).then_some(walk.active_version) {
            return Err(CellError::LedgerCorrupt {
                seq: walk.active_version,
                reason: "active definitions pointer disagrees with retained commit history"
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// The definition pins of `checkpoint`, shared by the open, the scrub and
    /// every advancement (S3, S6, S9).
    pub(crate) fn definitions_at_checkpoint(
        &self,
        reader: &StoreReader<'_, E>,
        versions: &[InstalledDefinitionSet],
        checkpoint: &OpenCheckpoint,
    ) -> Result<DefinitionWalk, CellError> {
        let commits = checkpoint.head.commits;
        if open_checkpoint::definitions_digest(versions, commits)? != checkpoint.definitions {
            return Err(open_checkpoint::corrupt(
                commits,
                "the retained definition versions it covers changed",
            ));
        }
        let mut walk = DefinitionWalk::default();
        for version in versions
            .iter()
            .take_while(|version| version.commit_seq <= commits)
        {
            if version.version != walk.next_version
                || version.commit_seq <= walk.previous_install_seq
            {
                return Err(CellError::LedgerCorrupt {
                    seq: version.commit_seq,
                    reason: "definition history version or receipt order is inconsistent"
                        .to_owned(),
                });
            }
            walk.installed(version)?;
        }
        if let Some(index) = walk.version_index.checked_sub(1) {
            let version = versions
                .get(index)
                .ok_or(CellError::OutOfRange("definition history index"))?;
            let receipt = reader
                .receipt_at(version.commit_seq)?
                .filter(|receipt| receipt.body.definitions_version == version.version)
                .ok_or_else(|| CellError::LedgerCorrupt {
                    seq: version.commit_seq,
                    reason: "the active definitions have no installing receipt".to_owned(),
                })?;
            self.verify_definition_install_live(reader, versions, index, &receipt)?;
        }
        if commits > 0
            && reader
                .receipts_after(commits.saturating_sub(1), 1)?
                .first()
                .is_none_or(|receipt| receipt.body.definitions_version != walk.active_version)
        {
            return Err(CellError::LedgerCorrupt {
                seq: commits,
                reason: "the open checkpoint's receipt names a definitions version not in force"
                    .to_owned(),
            });
        }
        Ok(walk)
    }

    /// One receipt of the definition history: an install is proven in full
    /// (its typed export record, admission receipt, readmission, ledger
    /// event and retained model, superseded or not: S9), and every other
    /// receipt names the version in force.
    pub(crate) fn definition_walk_step(
        &self,
        reader: &StoreReader<'_, E>,
        versions: &[InstalledDefinitionSet],
        walk: &mut DefinitionWalk,
        receipt: &Receipt,
    ) -> Result<(), CellError> {
        if let Some(version) = versions.get(walk.version_index)
            && version.commit_seq == receipt.body.commit_seq
        {
            if version.version != walk.next_version
                || version.commit_seq <= walk.previous_install_seq
                || receipt.body.definitions_version != version.version
            {
                return Err(CellError::LedgerCorrupt {
                    seq: receipt.body.commit_seq,
                    reason: "definition history version or receipt order is inconsistent"
                        .to_owned(),
                });
            }
            self.verify_definition_install_live(reader, versions, walk.version_index, receipt)?;
            walk.installed(version)?;
        } else if receipt.body.definitions_version != walk.active_version {
            return Err(CellError::LedgerCorrupt {
                seq: receipt.body.commit_seq,
                reason: "receipt names a definitions version not in force".to_owned(),
            });
        }
        Ok(())
    }

    fn verify_definition_install_live(
        &self,
        reader: &StoreReader<'_, E>,
        versions: &[InstalledDefinitionSet],
        version_index: usize,
        receipt: &Receipt,
    ) -> Result<(), CellError> {
        let version = versions
            .get(version_index)
            .ok_or(CellError::OutOfRange("definition history index"))?;
        let record = self
            .local_export_record(version.commit_seq)?
            .ok_or_else(|| CellError::LedgerCorrupt {
                seq: version.commit_seq,
                reason: "installed definitions have no typed export record".to_owned(),
            })?;
        let entries = reader.ledger_page(receipt.body.first_event_seq.saturating_sub(1), 1)?;
        Self::verify_definition_install(versions, version_index, receipt, &record, &entries)
    }

    /// S10 (#1551 round 7): snapshot only the scalar live database head and
    /// quantity boundary in O(1) inside a scrub restart transaction; suffix
    /// receipt hashes beyond `loaded_len` are verified and appended in bounded
    /// `RECEIPT_STEP` pages by Phase 1 (`scrub_receipts`).
    pub(crate) fn restart_scrub_target(
        &self,
        loaded_len: u64,
    ) -> Result<(LedgerHead, Option<FormatBoundary>), CellError> {
        let reader = self.verifier();
        let commits = reader.next_commit_seq()?.saturating_sub(1);
        if commits < loaded_len {
            return Err(open_checkpoint::corrupt(
                loaded_len,
                "the ledger shrank behind the open handle",
            ));
        }
        let (seq, hash) = reader.ledger_tail()?;
        let target = LedgerHead { seq, hash, commits };
        let boundary =
            crate::quantity::format_boundary(&self.conn)?.filter(|boundary| boundary.seq <= seq);
        Ok((target, boundary))
    }

    /// Record `checkpoint` inside the scrub seed transaction unless a sibling
    /// already recorded an authentic checkpoint at or past `checkpoint.head.commits`
    /// that does not exceed the live receipt tail (S10).
    pub(crate) fn record_scrub_checkpoint(
        &self,
        checkpoint: OpenCheckpoint,
    ) -> Result<Option<OpenCheckpoint>, CellError> {
        let db_commits: i64 = self.conn.query_row(
            &format!("SELECT COALESCE(MAX(\"commit_seq\"), 0) FROM \"{RECEIPT}\""),
            &[],
            |row| row.get(0),
        )?;
        let db_commits = to_u64(db_commits, "receipt head")?;
        if let Loaded::Authentic(existing) = open_checkpoint::load(&self.conn)?
            && existing.head.commits <= db_commits
            && existing.head.commits >= checkpoint.head.commits
        {
            return Ok(Some(existing));
        }
        let recorded = open_checkpoint::record(&self.conn, &checkpoint)?;
        Ok(recorded.then_some(checkpoint))
    }
}
