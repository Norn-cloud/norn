//! Bounded (`<= 512` total rows/wake) quantity verification state carried by
//! a scrub run across `Phase::History` and `Phase::QuantityTail` (S10).
use super::super::{
    CellError, FormatBoundary, QuantityError, Receipt, SqlEngine, SqlValue, to_i64,
};
use super::{
    LiveStage, PageStep, QuantityReplay, ROW_PAGE_I64, Rows, check_no_rows, match_page_rows,
    verify_live_page,
};

const RECEIPT_PAGE: usize = 64;
const RECEIPT_PAGE_I64: i64 = 64;

const QUANTITY_RECEIPT_RANGE_SQL: &str = r#"SELECT "commit_seq" FROM "_norn_receipt"
    WHERE "commit_seq" > ?1 AND "commit_seq" <= ?2 AND (
        json_extract("receipt", '$.quantity.version') = 'norn.quantity.commit/2'
        OR json_type("receipt", '$.quantity') = 'array'
    )
    ORDER BY "commit_seq" LIMIT ?3"#;

#[derive(Clone)]
struct SeedState {
    after: u64,
    through: u64,
    expected_digest: String,
    digest: String,
}

#[derive(Clone)]
struct AppendCursor {
    after: u64,
    through: u64,
    is_history: bool,
    cursor: Vec<SqlValue>,
    expected_history: Rows,
    expected_postings: Rows,
}

#[derive(Clone)]
struct SuffixTail {
    after: u64,
    db_commits: u64,
    had_suffix: bool,
}

#[derive(Clone)]
struct LiveTail {
    checked_db_commits: u64,
    had_suffix: bool,
    stage: LiveStage,
    cursor: Vec<SqlValue>,
    remaining: Rows,
}

#[derive(Clone)]
enum TailState {
    SuffixReceipts(SuffixTail),
    Live(LiveTail),
}

/// Bounded (`<= 512` total rows/wake) quantity verification state carried by a
/// scrub run across `Phase::History` and `Phase::QuantityTail` (S10).
#[derive(Clone, Default)]
pub(crate) struct QuantityScrub {
    replay: QuantityReplay,
    seed: Option<SeedState>,
    pending_append: Option<AppendCursor>,
    tail: Option<TailState>,
}

impl QuantityScrub {
    pub(crate) fn new(
        start: Option<&crate::open_checkpoint::OpenCheckpoint>,
        target_boundary: Option<FormatBoundary>,
    ) -> Self {
        let seed = start
            .filter(|cp| {
                cp.head.commits > 0 && (cp.boundary.is_some() || target_boundary.is_some())
            })
            .map(|cp| SeedState {
                after: 0,
                through: cp.head.commits,
                expected_digest: cp.quantity_receipts.clone(),
                digest: String::new(),
            });
        Self {
            replay: QuantityReplay::default(),
            seed,
            pending_append: None,
            tail: None,
        }
    }

    pub(crate) const fn has_pending_work(&self) -> bool {
        self.seed.is_some() || self.pending_append.is_some()
    }

    /// Run one bounded (`<= 512` total rows) pending seed or append-table page step.
    pub(crate) fn step_pending<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
    ) -> Result<(), CellError> {
        if self.seed.is_some() {
            return self.step_seed(engine, reader);
        }
        if self.pending_append.is_some() {
            return self.step_append_page(engine);
        }
        Ok(())
    }

    /// Replay a `<= 64`-commit history chunk; ordinary-only chunks verify empty
    /// append ranges via `LIMIT 1`, while quantity chunks stage `pending_append`
    /// so append pages drain on subsequent wakes within the 512-row total bound.
    pub(crate) fn verify_chunk<S: SqlEngine>(
        &mut self,
        engine: &S,
        receipts: &[Receipt],
        after: u64,
        through: u64,
    ) -> Result<(), CellError> {
        if after == 0 {
            for sql in [
                r#"SELECT 1 FROM "_norn_quantity_history" WHERE "commit_seq" <= 0 LIMIT 1"#,
                r#"SELECT 1 FROM "_norn_quantity_posting" WHERE "commit_seq" <= 0 LIMIT 1"#,
            ] {
                check_no_rows(engine, sql, &[])?;
            }
        }
        let (mut history, mut postings) = (Rows::new(), Rows::new());
        for receipt in receipts {
            self.replay
                .step_receipt(receipt, &mut history, &mut postings)?;
        }
        if history.is_empty() && postings.is_empty() {
            return check_append_empty(engine, after, through);
        }
        self.stage_append(after, through, history, postings);
        Ok(())
    }

    /// Advance `Phase::QuantityTail` by one bounded (`<= 512` total rows/wake) step;
    /// returns `Ok(true)` once clock, boundary, suffix append rows, and live tables verify.
    pub(crate) fn step_tail<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        target_commits: u64,
        target_boundary: Option<FormatBoundary>,
    ) -> Result<bool, CellError> {
        if self.pending_append.is_some() {
            self.step_append_page(engine)?;
            return Ok(false);
        }
        if self.tail.is_none() {
            super::super::clock_floor(engine)?;
            let db_commits = reader.next_commit_seq()?.saturating_sub(1);
            if db_commits < target_commits {
                return Err(QuantityError::Corrupt.into());
            }
            self.tail = Some(TailState::SuffixReceipts(SuffixTail {
                after: target_commits,
                db_commits,
                had_suffix: false,
            }));
        }
        match self.tail.take().ok_or(QuantityError::Corrupt)? {
            TailState::SuffixReceipts(suffix) => {
                self.step_suffix_receipts(engine, reader, suffix, target_boundary)
            }
            TailState::Live(live) => self.step_live_stage(engine, reader, live),
        }
    }

    fn step_seed<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
    ) -> Result<(), CellError> {
        let mut state = self.seed.take().ok_or(QuantityError::Corrupt)?;
        if state.digest.is_empty() {
            state.digest = crate::open_checkpoint::quantity_seed()?;
        }
        let commits = query_quantity_commits(engine, state.after, state.through, RECEIPT_PAGE_I64)?;
        let (mut discard_history, mut discard_postings) = (Rows::new(), Rows::new());
        for &commit in &commits {
            let receipt = load_quantity_receipt(reader, commit)?;
            state.digest =
                crate::open_checkpoint::quantity_extend(&state.digest, commit, &receipt.hash)?;
            self.replay
                .step_receipt(&receipt, &mut discard_history, &mut discard_postings)?;
            discard_history.clear();
            discard_postings.clear();
        }
        if commits.len() < RECEIPT_PAGE {
            return if state.digest == state.expected_digest {
                Ok(())
            } else {
                Err(QuantityError::Corrupt.into())
            };
        }
        state.after = *commits.last().ok_or(QuantityError::Corrupt)?;
        self.seed = Some(state);
        Ok(())
    }

    fn stage_append(
        &mut self,
        after: u64,
        through: u64,
        expected_history: Rows,
        expected_postings: Rows,
    ) {
        self.pending_append = Some(AppendCursor {
            after,
            through,
            is_history: true,
            cursor: Vec::new(),
            expected_history,
            expected_postings,
        });
    }

    fn step_append_page<S: SqlEngine>(&mut self, engine: &S) -> Result<(), CellError> {
        let mut state = self.pending_append.take().ok_or(QuantityError::Corrupt)?;
        if state.is_history {
            match verify_append_page(
                engine,
                true,
                state.after,
                state.through,
                &state.cursor,
                state.expected_history,
            )? {
                PageStep::More {
                    next_cursor,
                    remaining,
                } => {
                    state.cursor = next_cursor;
                    state.expected_history = remaining;
                    self.pending_append = Some(state);
                    return Ok(());
                }
                PageStep::Done { read } => {
                    state.is_history = false;
                    state.cursor.clear();
                    state.expected_history = Rows::new();
                    if read > 0 {
                        self.pending_append = Some(state);
                        return Ok(());
                    }
                }
            }
        }
        if let PageStep::More {
            next_cursor,
            remaining,
        } = verify_append_page(
            engine,
            false,
            state.after,
            state.through,
            &state.cursor,
            state.expected_postings,
        )? {
            state.cursor = next_cursor;
            state.expected_postings = remaining;
            self.pending_append = Some(state);
        }
        Ok(())
    }

    fn step_suffix_receipts<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        mut suffix: SuffixTail,
        target_boundary: Option<FormatBoundary>,
    ) -> Result<bool, CellError> {
        let live_db = reader.next_commit_seq()?.saturating_sub(1);
        if live_db < suffix.db_commits {
            return Err(QuantityError::Corrupt.into());
        }
        if live_db > suffix.db_commits {
            suffix.db_commits = live_db;
        }
        if suffix.after >= suffix.db_commits {
            return self.finish_suffix(
                engine,
                suffix.db_commits,
                target_boundary,
                suffix.had_suffix,
            );
        }
        let commits =
            query_quantity_commits(engine, suffix.after, suffix.db_commits, RECEIPT_PAGE_I64)?;
        if commits.is_empty() {
            self.tail = Some(TailState::SuffixReceipts(SuffixTail {
                after: suffix.db_commits,
                ..suffix
            }));
            self.stage_append(suffix.after, suffix.db_commits, Rows::new(), Rows::new());
            self.step_append_page(engine)?;
            return Ok(false);
        }
        let (mut suffix_history, mut suffix_postings) = (Rows::new(), Rows::new());
        for &commit in &commits {
            let receipt = load_quantity_receipt(reader, commit)?;
            self.replay
                .step_receipt(&receipt, &mut suffix_history, &mut suffix_postings)?;
        }
        let last_commit = *commits.last().ok_or(QuantityError::Corrupt)?;
        let next_after = if commits.len() < RECEIPT_PAGE {
            suffix.db_commits
        } else {
            last_commit
        };
        self.tail = Some(TailState::SuffixReceipts(SuffixTail {
            after: next_after,
            db_commits: suffix.db_commits,
            had_suffix: true,
        }));
        self.stage_append(suffix.after, next_after, suffix_history, suffix_postings);
        Ok(false)
    }

    fn finish_suffix<S: SqlEngine>(
        &mut self,
        engine: &S,
        db_commits: u64,
        target_boundary: Option<FormatBoundary>,
        had_suffix: bool,
    ) -> Result<bool, CellError> {
        if had_suffix {
            super::super::format_boundary(engine)?;
        } else {
            super::super::validate_boundary(engine, target_boundary)?;
        }
        check_beyond_tail(engine, db_commits)?;
        if self.replay.is_live_empty() {
            for sql in [
                r#"SELECT 1 FROM "_norn_timer" WHERE "timer_id">='norn.quantity.expiry/' AND "timer_id"<'norn.quantity.expiry0' LIMIT 1"#,
                r#"SELECT 1 FROM "_norn_quantity_ledger" LIMIT 1"#,
                r#"SELECT 1 FROM "_norn_quantity_balance" LIMIT 1"#,
                r#"SELECT 1 FROM "_norn_quantity_reservation" LIMIT 1"#,
            ] {
                check_no_rows(engine, sql, &[])?;
            }
            return Ok(true);
        }
        self.tail = Some(TailState::Live(LiveTail {
            checked_db_commits: db_commits,
            had_suffix,
            stage: LiveStage::Timers,
            cursor: Vec::new(),
            remaining: self.replay.live_rows(LiveStage::Timers),
        }));
        Ok(false)
    }

    fn step_live_stage<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        live: LiveTail,
    ) -> Result<bool, CellError> {
        let live_db = reader.next_commit_seq()?.saturating_sub(1);
        if live_db < live.checked_db_commits {
            return Err(QuantityError::Corrupt.into());
        }
        if live_db > live.checked_db_commits {
            self.tail = Some(TailState::SuffixReceipts(SuffixTail {
                after: live.checked_db_commits,
                db_commits: live_db,
                had_suffix: live.had_suffix,
            }));
            return Ok(false);
        }
        match verify_live_page(engine, live.stage, &live.cursor, live.remaining)? {
            PageStep::More {
                next_cursor,
                remaining,
            } => {
                self.tail = Some(TailState::Live(LiveTail {
                    cursor: next_cursor,
                    remaining,
                    ..live
                }));
                Ok(false)
            }
            PageStep::Done { .. } => match live.stage.next() {
                Some(next_stage) => {
                    self.tail = Some(TailState::Live(LiveTail {
                        stage: next_stage,
                        cursor: Vec::new(),
                        remaining: self.replay.live_rows(next_stage),
                        ..live
                    }));
                    Ok(false)
                }
                None => {
                    let live_after = reader.next_commit_seq()?.saturating_sub(1);
                    if live_after < live.checked_db_commits {
                        return Err(QuantityError::Corrupt.into());
                    }
                    if live_after > live.checked_db_commits {
                        self.tail = Some(TailState::SuffixReceipts(SuffixTail {
                            after: live.checked_db_commits,
                            db_commits: live_after,
                            had_suffix: live.had_suffix,
                        }));
                        return Ok(false);
                    }
                    check_beyond_tail(engine, live.checked_db_commits)?;
                    Ok(true)
                }
            },
        }
    }
}

fn check_append_empty<S: SqlEngine>(engine: &S, after: u64, through: u64) -> Result<(), CellError> {
    let params = [
        to_i64(after, "quantity append after")?.into(),
        to_i64(through, "quantity append through")?.into(),
    ];
    for sql in [
        r#"SELECT 1 FROM "_norn_quantity_history" WHERE "commit_seq" > ?1 AND "commit_seq" <= ?2 LIMIT 1"#,
        r#"SELECT 1 FROM "_norn_quantity_posting" WHERE "commit_seq" > ?1 AND "commit_seq" <= ?2 LIMIT 1"#,
    ] {
        check_no_rows(engine, sql, &params)?;
    }
    Ok(())
}

fn check_beyond_tail<S: SqlEngine>(engine: &S, db_commits: u64) -> Result<(), CellError> {
    let commits_i64 = to_i64(db_commits, "quantity scrub tail")?;
    for sql in [
        r#"SELECT 1 FROM "_norn_quantity_history" WHERE "commit_seq" > ?1 LIMIT 1"#,
        r#"SELECT 1 FROM "_norn_quantity_posting" WHERE "commit_seq" > ?1 LIMIT 1"#,
    ] {
        check_no_rows(engine, sql, &[commits_i64.into()])?;
    }
    Ok(())
}

fn load_quantity_receipt<S: SqlEngine>(
    reader: &crate::read::StoreReader<'_, S>,
    commit: u64,
) -> Result<Receipt, CellError> {
    reader
        .receipt_at(commit)?
        .filter(|receipt| receipt.body.quantity.is_some())
        .ok_or_else(|| QuantityError::Corrupt.into())
}

fn query_quantity_commits<S: SqlEngine>(
    engine: &S,
    after: u64,
    through: u64,
    limit: i64,
) -> Result<Vec<u64>, CellError> {
    let rows = engine.query(
        QUANTITY_RECEIPT_RANGE_SQL,
        &[
            to_i64(after, "quantity receipt after")?.into(),
            to_i64(through, "quantity receipt through")?.into(),
            limit.into(),
        ],
    )?;
    rows.into_iter()
        .map(|row| crate::read::to_u64(crate::engine::SqlRow(row).get(0)?, "quantity commit"))
        .collect()
}

fn verify_append_page<S: SqlEngine>(
    engine: &S,
    is_history: bool,
    after: u64,
    through: u64,
    cursor: &[SqlValue],
    expected: Rows,
) -> Result<PageStep, CellError> {
    let (sql_first, sql_next, count, indices): (&str, &str, usize, &[usize]) = if is_history {
        (
            r#"SELECT world,ledger,account,unit,layer,posted,held,version,commit_seq FROM "_norn_quantity_history" WHERE "commit_seq">?1 AND "commit_seq"<=?2 ORDER BY "commit_seq","world","ledger","account","unit","layer","version" LIMIT ?3"#,
            r#"SELECT world,ledger,account,unit,layer,posted,held,version,commit_seq FROM "_norn_quantity_history" WHERE "commit_seq">?1 AND "commit_seq"<=?2 AND ("commit_seq","world","ledger","account","unit","layer","version")>(?3,?4,?5,?6,?7,?8,?9) ORDER BY "commit_seq","world","ledger","account","unit","layer","version" LIMIT ?10"#,
            5,
            &[8, 0, 1, 2, 3, 4, 7],
        )
    } else {
        (
            r#"SELECT world,ledger,id,record,reverses,version,commit_seq FROM "_norn_quantity_posting" WHERE "commit_seq">?1 AND "commit_seq"<=?2 ORDER BY "commit_seq","world","ledger","id" LIMIT ?3"#,
            r#"SELECT world,ledger,id,record,reverses,version,commit_seq FROM "_norn_quantity_posting" WHERE "commit_seq">?1 AND "commit_seq"<=?2 AND ("commit_seq","world","ledger","id")>(?3,?4,?5,?6) ORDER BY "commit_seq","world","ledger","id" LIMIT ?7"#,
            3,
            &[6, 0, 1, 2],
        )
    };
    let after_i64 = to_i64(after, "quantity append after")?;
    let through_i64 = to_i64(through, "quantity append through")?;
    let rows = if cursor.is_empty() {
        engine.query(
            sql_first,
            &[after_i64.into(), through_i64.into(), ROW_PAGE_I64.into()],
        )?
    } else {
        let mut params = Vec::with_capacity(cursor.len().saturating_add(3));
        params.push(after_i64.into());
        params.push(through_i64.into());
        params.extend(cursor.iter().cloned());
        params.push(ROW_PAGE_I64.into());
        engine.query(sql_next, &params)?
    };
    match_page_rows(rows, count, is_history, expected, indices)
}
