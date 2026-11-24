//! Reconstruct quantity caches exclusively from verified, typed receipts.
use super::{
    BTreeMap, BTreeSet, CellError, QuantityError, QuantityOperation, QuantityPosting,
    QuantityReservation, QuantityState, Receipt, SqlEngine, SqlValue, canonical_json,
    prepare_quantity, to_i64,
};

#[derive(Default, Clone)]
struct Ledger {
    state: QuantityState,
    reservations: BTreeMap<String, (QuantityReservation, i64)>,
    postings: BTreeMap<String, QuantityPosting>,
    reversed: BTreeSet<String>,
    defined_at: i64,
}

type Rows = BTreeMap<Vec<String>, Vec<SqlValue>>;

// The whole row is compared, including revision, commit identity and exact text.
// Keys select correspondence only; missing and surplus rows both refuse.
fn key(row: &[SqlValue], count: usize) -> Result<Vec<String>, CellError> {
    row.iter()
        .take(count)
        .map(|value| match value {
            SqlValue::Text(text) => Ok(text.clone()),
            SqlValue::Integer(value) => Ok(value.to_string()),
            SqlValue::Null | SqlValue::Real(_) | SqlValue::Blob(_) => {
                Err(QuantityError::Corrupt.into())
            }
        })
        .collect()
}

fn put(rows: &mut Rows, count: usize, row: Vec<SqlValue>) -> Result<(), CellError> {
    rows.insert(key(&row, count)?, row);
    Ok(())
}

#[derive(Default, Clone)]
pub(crate) struct QuantityReplay {
    ledgers: BTreeMap<(String, String), Ledger>,
    heads: Rows,
    balances: Rows,
    reservations: Rows,
    timers: Rows,
    floor: u64,
}

impl QuantityReplay {
    #[allow(
        clippy::too_many_lines,
        reason = "one ruled request updates its complete derived projections"
    )]
    fn request(
        &mut self,
        body: &crate::store::ReceiptBody,
        request: &super::QuantityRequest,
        result: &norn_kernel::quantity::QuantityResult,
        history: &mut Rows,
        postings: &mut Rows,
    ) -> Result<(), CellError> {
        let Self {
            ledgers,
            heads,
            balances,
            reservations,
            timers,
            ..
        } = self;
        let world = &body.world;
        let id = &request.ledger_id;
        let ledger = ledgers.entry((world.clone(), id.clone())).or_default();
        let state = &mut ledger.state;
        state.reservation = None;
        state.original = None;
        state.original_reversed = false;
        state.identity_exists = false;
        match &request.operation {
            QuantityOperation::Define { .. } => {}
            QuantityOperation::Post { posting_id, .. } => {
                state.identity_exists = ledger.postings.contains_key(posting_id);
            }
            QuantityOperation::Reserve { reservation_id, .. } => {
                state.identity_exists = ledger.reservations.contains_key(reservation_id);
            }
            QuantityOperation::PostReservation {
                reservation_id,
                posting_id,
                ..
            } => {
                state.reservation = ledger
                    .reservations
                    .get(reservation_id)
                    .map(|(value, _)| value.clone());
                state.identity_exists = ledger.postings.contains_key(posting_id);
            }
            QuantityOperation::Void { reservation_id, .. }
            | QuantityOperation::Expire { reservation_id } => {
                state.reservation = ledger
                    .reservations
                    .get(reservation_id)
                    .map(|(value, _)| value.clone());
            }
            QuantityOperation::Reverse {
                original_id,
                posting_id,
            } => {
                state.original = ledger.postings.get(original_id).cloned();
                state.original_reversed = ledger.reversed.contains(original_id);
                state.identity_exists = ledger.postings.contains_key(posting_id);
            }
        }
        // Reuse the ruled pure kernel: no SQL projection participates in admission.
        let plan = prepare_quantity(request, state, body.logical_time)
            .map_err(|_invalid_history| QuantityError::Corrupt)?;
        if result
            != &(norn_kernel::quantity::QuantityResult {
                version: plan.version,
                balance_keys: plan.balances.keys().cloned().collect(),
                reservation: plan.reservation.clone(),
                posting: plan.posting.clone(),
            })
        {
            return Err(QuantityError::Corrupt.into());
        }
        let version = to_i64(plan.version, "quantity version")?;
        let seq = to_i64(body.commit_seq, "quantity commit seq")?;
        if let QuantityOperation::Define {
            pack,
            unrestricted_accounts,
            ..
        } = &request.operation
        {
            state.pack = Some(pack.clone());
            state
                .unrestricted_accounts
                .clone_from(unrestricted_accounts);
            ledger.defined_at = seq;
        }
        state.version = plan.version;
        put(
            heads,
            2,
            vec![
                world.into(),
                id.into(),
                version.into(),
                canonical_json(state.pack.as_ref().ok_or(QuantityError::Corrupt)?)?.into(),
                canonical_json(&state.unrestricted_accounts)?.into(),
                ledger.defined_at.into(),
            ],
        )?;
        for (balance_key, value) in plan.balances {
            let mut row = vec![
                world.into(),
                id.into(),
                balance_key.account.clone().into(),
                balance_key.unit.clone().into(),
                balance_key.layer.clone().into(),
                value.posted.clone().into(),
                value.held.clone().into(),
                to_i64(value.version, "quantity balance version")?.into(),
            ];
            put(balances, 5, row.clone())?;
            // History's primary key includes the revision (column 7).
            row.push(seq.into());
            let mut history_key = key(&row, 5)?;
            history_key.push(value.version.to_string());
            history.insert(history_key, row);
            state.balances.insert(balance_key, value);
        }
        if let Some((reservation_id, value)) = plan.reservation {
            let created = ledger
                .reservations
                .get(&reservation_id)
                .map_or(seq, |(_, created)| *created);
            put(
                reservations,
                3,
                vec![
                    world.into(),
                    id.into(),
                    reservation_id.clone().into(),
                    canonical_json(&value)?.into(),
                    version.into(),
                    seq.into(),
                    created.into(),
                ],
            )?;
            let timer_id = super::expiry_timer_id(world, id, &reservation_id)?;
            if value.status == "open" {
                put(
                    timers,
                    1,
                    vec![
                        timer_id.into(),
                        to_i64(value.expires_at, "quantity expiry")?.into(),
                        canonical_json(&super::expiry_timer(
                            world,
                            id,
                            &reservation_id,
                            value.expires_at,
                        )?)?
                        .into(),
                        seq.into(),
                    ],
                )?;
            } else {
                timers.remove(&vec![timer_id]);
            }
            ledger.reservations.insert(reservation_id, (value, created));
        }
        if let Some((posting_id, value)) = plan.posting {
            put(
                postings,
                3,
                vec![
                    world.into(),
                    id.into(),
                    posting_id.clone().into(),
                    canonical_json(&value)?.into(),
                    value.reverses.as_deref().into(),
                    version.into(),
                    seq.into(),
                ],
            )?;
            if let Some(original) = &value.reverses {
                ledger.reversed.insert(original.clone());
            }
            ledger.postings.insert(posting_id, value);
        }
        Ok(())
    }

    fn step_receipt(
        &mut self,
        receipt: &Receipt,
        history: &mut Rows,
        postings: &mut Rows,
    ) -> Result<(), CellError> {
        let body = &receipt.body;
        if body.logical_time < self.floor {
            return Err(QuantityError::Corrupt.into());
        }
        self.floor = body.logical_time;
        let Some(requests) = &body.quantity else {
            return Ok(());
        };
        let results = body
            .quantity_result
            .as_ref()
            .ok_or(QuantityError::Corrupt)?;
        if requests.is_empty()
            || requests.len() > 100
            || requests.len() != results.len()
            || requests
                .iter()
                .map(|request| &request.ledger_id)
                .collect::<BTreeSet<_>>()
                .len()
                != requests.len()
        {
            return Err(QuantityError::Corrupt.into());
        }
        let mut timers = Vec::new();
        for (request, result) in requests.iter().zip(results.iter()) {
            self.request(body, request, result, history, postings)?;
            if let Some((id, value)) = &result.reservation
                && value.status == "open"
            {
                timers.push(super::expiry_timer_id(&body.world, &request.ledger_id, id)?);
            }
        }
        let scheduled: Vec<_> = body
            .scheduled
            .iter()
            .filter(|id| id.starts_with(super::EXPIRY_TIMER_PREFIX))
            .cloned()
            .collect();
        if scheduled != timers {
            return Err(QuantityError::Corrupt.into());
        }
        Ok(())
    }

    fn live_rows(&self, stage: LiveStage) -> Rows {
        match stage {
            LiveStage::Timers => self.timers.clone(),
            LiveStage::Ledgers => self.heads.clone(),
            LiveStage::Balances => self.balances.clone(),
            LiveStage::Reservations => self.reservations.clone(),
        }
    }

    fn is_live_empty(&self) -> bool {
        self.timers.is_empty()
            && self.heads.is_empty()
            && self.balances.is_empty()
            && self.reservations.is_empty()
    }
}

const SCRUB_PAGE: usize = crate::scrub::RECEIPT_STEP;
const SCRUB_PAGE_I64: i64 = 512;

const QUANTITY_RECEIPT_RANGE_SQL: &str = r#"SELECT "commit_seq" FROM "_norn_receipt"
    WHERE "commit_seq" > ?1 AND "commit_seq" <= ?2 AND (
        json_extract("receipt", '$.quantity.version') = 'norn.quantity.commit/2'
        OR json_type("receipt", '$.quantity') = 'array'
    )
    ORDER BY "commit_seq" LIMIT ?3"#;

#[derive(Clone, Copy)]
enum LiveStage {
    Timers,
    Ledgers,
    Balances,
    Reservations,
}

impl LiveStage {
    const fn next(self) -> Option<Self> {
        match self {
            Self::Timers => Some(Self::Ledgers),
            Self::Ledgers => Some(Self::Balances),
            Self::Balances => Some(Self::Reservations),
            Self::Reservations => None,
        }
    }
}

#[derive(Clone)]
struct SeedState {
    after: u64,
    through: u64,
    expected_digest: String,
    digest: String,
}

#[derive(Clone)]
enum AppendCursor {
    History {
        after: u64,
        through: u64,
        cursor: Vec<SqlValue>,
        expected_history: Rows,
        expected_postings: Rows,
    },
    Posting {
        after: u64,
        through: u64,
        cursor: Vec<SqlValue>,
        expected_postings: Rows,
    },
}

#[derive(Clone)]
enum TailState {
    SuffixReceipts {
        after: u64,
        db_commits: u64,
        had_suffix: bool,
    },
    Live {
        checked_db_commits: u64,
        stage: LiveStage,
        cursor: Vec<SqlValue>,
        remaining: Rows,
    },
}

enum PageStep {
    Done,
    More {
        next_cursor: Vec<SqlValue>,
        remaining: Rows,
    },
}

/// Bounded (`<= 512` rows/wake) quantity verification state carried by a scrub
/// run across `Phase::History` and `Phase::QuantityTail` (S10).
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
        target_boundary: Option<super::FormatBoundary>,
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

    /// Run one bounded (`<= 512`-row) pending seed or append-table page step.
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

    /// Replay a `<= 64`-commit history chunk and verify its first `<= 512`-row
    /// append page; any overflow pages drain on subsequent wakes via `step_pending`.
    pub(crate) fn verify_chunk<S: SqlEngine>(
        &mut self,
        engine: &S,
        receipts: &[Receipt],
        after: u64,
        through: u64,
    ) -> Result<(), CellError> {
        if after == 0 {
            check_no_rows(
                engine,
                r#"SELECT 1 FROM "_norn_quantity_history" WHERE "commit_seq" <= 0 LIMIT 1"#,
                &[],
            )?;
            check_no_rows(
                engine,
                r#"SELECT 1 FROM "_norn_quantity_posting" WHERE "commit_seq" <= 0 LIMIT 1"#,
                &[],
            )?;
        }
        let mut history = Rows::new();
        let mut postings = Rows::new();
        for receipt in receipts {
            self.replay
                .step_receipt(receipt, &mut history, &mut postings)?;
        }
        self.start_append_range(engine, after, through, history, postings)
    }

    /// Advance `Phase::QuantityTail` by one bounded (`<= 512`-row) wake; returns
    /// `Ok(true)` once clock, boundary, suffix append rows, and all live tables verify.
    pub(crate) fn step_tail<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        target_commits: u64,
        target_boundary: Option<super::FormatBoundary>,
    ) -> Result<bool, CellError> {
        if self.pending_append.is_some() {
            self.step_append_page(engine)?;
            return Ok(false);
        }
        if self.tail.is_none() {
            super::clock_floor(engine)?;
            let db_commits = reader.next_commit_seq()?.saturating_sub(1);
            if db_commits < target_commits {
                return Err(QuantityError::Corrupt.into());
            }
            self.tail = Some(TailState::SuffixReceipts {
                after: target_commits,
                db_commits,
                had_suffix: false,
            });
        }
        match self.tail.take().ok_or(QuantityError::Corrupt)? {
            TailState::SuffixReceipts {
                after,
                db_commits,
                had_suffix,
            } => self.step_suffix_receipts(
                engine,
                reader,
                after,
                db_commits,
                target_boundary,
                had_suffix,
            ),
            TailState::Live {
                checked_db_commits,
                stage,
                cursor,
                remaining,
            } => self.step_live_stage(engine, reader, checked_db_commits, stage, cursor, remaining),
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
        let commits = query_quantity_commits(engine, state.after, state.through, SCRUB_PAGE_I64)?;
        let mut discard_history = Rows::new();
        let mut discard_postings = Rows::new();
        for &commit in &commits {
            let receipt = reader
                .receipt_at(commit)?
                .filter(|receipt| receipt.body.quantity.is_some())
                .ok_or(QuantityError::Corrupt)?;
            state.digest =
                crate::open_checkpoint::quantity_extend(&state.digest, commit, &receipt.hash)?;
            self.replay
                .step_receipt(&receipt, &mut discard_history, &mut discard_postings)?;
            discard_history.clear();
            discard_postings.clear();
        }
        if commits.len() < SCRUB_PAGE {
            if state.digest != state.expected_digest {
                return Err(QuantityError::Corrupt.into());
            }
            return Ok(());
        }
        state.after = *commits.last().ok_or(QuantityError::Corrupt)?;
        self.seed = Some(state);
        Ok(())
    }

    fn start_append_range<S: SqlEngine>(
        &mut self,
        engine: &S,
        after: u64,
        through: u64,
        expected_history: Rows,
        expected_postings: Rows,
    ) -> Result<(), CellError> {
        self.pending_append = Some(AppendCursor::History {
            after,
            through,
            cursor: Vec::new(),
            expected_history,
            expected_postings,
        });
        self.step_append_page(engine)
    }

    fn step_append_page<S: SqlEngine>(&mut self, engine: &S) -> Result<(), CellError> {
        let next = match self.pending_append.take().ok_or(QuantityError::Corrupt)? {
            AppendCursor::History {
                after,
                through,
                cursor,
                expected_history,
                expected_postings,
            } => match verify_append_page(engine, true, after, through, &cursor, expected_history)?
            {
                PageStep::More {
                    next_cursor,
                    remaining,
                } => Some(AppendCursor::History {
                    after,
                    through,
                    cursor: next_cursor,
                    expected_history: remaining,
                    expected_postings,
                }),
                PageStep::Done => {
                    match verify_append_page(engine, false, after, through, &[], expected_postings)?
                    {
                        PageStep::More {
                            next_cursor,
                            remaining,
                        } => Some(AppendCursor::Posting {
                            after,
                            through,
                            cursor: next_cursor,
                            expected_postings: remaining,
                        }),
                        PageStep::Done => None,
                    }
                }
            },
            AppendCursor::Posting {
                after,
                through,
                cursor,
                expected_postings,
            } => {
                match verify_append_page(engine, false, after, through, &cursor, expected_postings)?
                {
                    PageStep::More {
                        next_cursor,
                        remaining,
                    } => Some(AppendCursor::Posting {
                        after,
                        through,
                        cursor: next_cursor,
                        expected_postings: remaining,
                    }),
                    PageStep::Done => None,
                }
            }
        };
        self.pending_append = next;
        Ok(())
    }

    fn step_suffix_receipts<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        after: u64,
        db_commits: u64,
        target_boundary: Option<super::FormatBoundary>,
        had_suffix: bool,
    ) -> Result<bool, CellError> {
        if after >= db_commits {
            return self.finish_suffix(engine, db_commits, target_boundary, had_suffix);
        }
        let commits = query_quantity_commits(engine, after, db_commits, SCRUB_PAGE_I64)?;
        if commits.is_empty() {
            return self.finish_suffix(engine, db_commits, target_boundary, had_suffix);
        }
        let mut suffix_history = Rows::new();
        let mut suffix_postings = Rows::new();
        for &commit in &commits {
            let receipt = reader
                .receipt_at(commit)?
                .filter(|receipt| receipt.body.quantity.is_some())
                .ok_or(QuantityError::Corrupt)?;
            self.replay
                .step_receipt(&receipt, &mut suffix_history, &mut suffix_postings)?;
        }
        let last_commit = *commits.last().ok_or(QuantityError::Corrupt)?;
        let next_after = if commits.len() < SCRUB_PAGE {
            db_commits
        } else {
            last_commit
        };
        self.tail = Some(TailState::SuffixReceipts {
            after: next_after,
            db_commits,
            had_suffix: true,
        });
        self.start_append_range(engine, after, next_after, suffix_history, suffix_postings)?;
        Ok(false)
    }

    fn finish_suffix<S: SqlEngine>(
        &mut self,
        engine: &S,
        db_commits: u64,
        target_boundary: Option<super::FormatBoundary>,
        had_suffix: bool,
    ) -> Result<bool, CellError> {
        if had_suffix {
            super::format_boundary(engine)?;
        } else {
            super::validate_boundary(engine, target_boundary)?;
        }
        let commits_i64 = to_i64(db_commits, "quantity scrub tail")?;
        check_no_rows(
            engine,
            r#"SELECT 1 FROM "_norn_quantity_history" WHERE "commit_seq" > ?1 LIMIT 1"#,
            &[commits_i64.into()],
        )?;
        check_no_rows(
            engine,
            r#"SELECT 1 FROM "_norn_quantity_posting" WHERE "commit_seq" > ?1 LIMIT 1"#,
            &[commits_i64.into()],
        )?;
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
        self.tail = Some(TailState::Live {
            checked_db_commits: db_commits,
            stage: LiveStage::Timers,
            cursor: Vec::new(),
            remaining: self.replay.live_rows(LiveStage::Timers),
        });
        Ok(false)
    }

    fn step_live_stage<S: SqlEngine>(
        &mut self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        mut checked_db_commits: u64,
        stage: LiveStage,
        cursor: Vec<SqlValue>,
        remaining: Rows,
    ) -> Result<bool, CellError> {
        let live_db = reader.next_commit_seq()?.saturating_sub(1);
        if live_db < checked_db_commits {
            return Err(QuantityError::Corrupt.into());
        }
        if live_db > checked_db_commits {
            if !query_quantity_commits(engine, checked_db_commits, live_db, 1)?.is_empty() {
                self.tail = Some(TailState::SuffixReceipts {
                    after: checked_db_commits,
                    db_commits: live_db,
                    had_suffix: true,
                });
                return Ok(false);
            }
            checked_db_commits = live_db;
        }
        match verify_live_page(engine, stage, &cursor, remaining)? {
            PageStep::More {
                next_cursor,
                remaining,
            } => {
                self.tail = Some(TailState::Live {
                    checked_db_commits,
                    stage,
                    cursor: next_cursor,
                    remaining,
                });
                Ok(false)
            }
            PageStep::Done => match stage.next() {
                Some(next_stage) => {
                    self.tail = Some(TailState::Live {
                        checked_db_commits,
                        stage: next_stage,
                        cursor: Vec::new(),
                        remaining: self.replay.live_rows(next_stage),
                    });
                    Ok(false)
                }
                None => Ok(true),
            },
        }
    }
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

fn row_limit(expected_len: usize) -> Result<i64, CellError> {
    let limit = expected_len.checked_add(1).ok_or(QuantityError::Corrupt)?;
    i64::try_from(limit).map_err(|_range| QuantityError::Corrupt.into())
}

fn check_no_rows<S: SqlEngine>(
    engine: &S,
    sql: &str,
    params: &[SqlValue],
) -> Result<(), CellError> {
    if !engine.query(sql, params)?.is_empty() {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(())
}

fn remove_matching_rows(
    rows: Vec<Vec<SqlValue>>,
    count: usize,
    is_history: bool,
    expected: &mut Rows,
) -> Result<(), CellError> {
    for row in rows {
        let mut row_key = key(&row, count)?;
        if is_history {
            let version = row.get(7).ok_or(QuantityError::Corrupt)?;
            row_key.extend(key(std::slice::from_ref(version), 1)?);
        }
        if expected.remove(&row_key).as_ref() != Some(&row) {
            return Err(QuantityError::Corrupt.into());
        }
    }
    Ok(())
}

fn match_page_rows(
    rows: Vec<Vec<SqlValue>>,
    count: usize,
    is_history: bool,
    mut expected: Rows,
    cursor_indices: &[usize],
) -> Result<PageStep, CellError> {
    let page_full = rows.len() >= SCRUB_PAGE;
    let mut next_cursor = Vec::new();
    if page_full {
        let last = rows.last().ok_or(QuantityError::Corrupt)?;
        next_cursor.reserve(cursor_indices.len());
        for &index in cursor_indices {
            next_cursor.push(last.get(index).ok_or(QuantityError::Corrupt)?.clone());
        }
    }
    remove_matching_rows(rows, count, is_history, &mut expected)?;
    if page_full {
        return Ok(PageStep::More {
            next_cursor,
            remaining: expected,
        });
    }
    if !expected.is_empty() {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(PageStep::Done)
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
            &[after_i64.into(), through_i64.into(), SCRUB_PAGE_I64.into()],
        )?
    } else {
        let mut params = Vec::with_capacity(cursor.len().saturating_add(3));
        params.push(after_i64.into());
        params.push(through_i64.into());
        params.extend(cursor.iter().cloned());
        params.push(SCRUB_PAGE_I64.into());
        engine.query(sql_next, &params)?
    };
    match_page_rows(rows, count, is_history, expected, indices)
}

fn verify_live_page<S: SqlEngine>(
    engine: &S,
    stage: LiveStage,
    cursor: &[SqlValue],
    expected: Rows,
) -> Result<PageStep, CellError> {
    let (sql_first, sql_next, count, indices): (&str, &str, usize, &[usize]) = match stage {
        LiveStage::Timers => (
            r#"SELECT timer_id,due,timer,commit_seq FROM "_norn_timer" WHERE "timer_id">='norn.quantity.expiry/' AND "timer_id"<'norn.quantity.expiry0' ORDER BY "timer_id" LIMIT ?1"#,
            r#"SELECT timer_id,due,timer,commit_seq FROM "_norn_timer" WHERE "timer_id">='norn.quantity.expiry/' AND "timer_id"<'norn.quantity.expiry0' AND "timer_id">?1 ORDER BY "timer_id" LIMIT ?2"#,
            1,
            &[0],
        ),
        LiveStage::Ledgers => (
            r#"SELECT world,ledger,version,pack,unrestricted,defined_at FROM "_norn_quantity_ledger" ORDER BY "world","ledger" LIMIT ?1"#,
            r#"SELECT world,ledger,version,pack,unrestricted,defined_at FROM "_norn_quantity_ledger" WHERE ("world","ledger")>(?1,?2) ORDER BY "world","ledger" LIMIT ?3"#,
            2,
            &[0, 1],
        ),
        LiveStage::Balances => (
            r#"SELECT world,ledger,account,unit,layer,posted,held,version FROM "_norn_quantity_balance" ORDER BY "world","ledger","account","unit","layer" LIMIT ?1"#,
            r#"SELECT world,ledger,account,unit,layer,posted,held,version FROM "_norn_quantity_balance" WHERE ("world","ledger","account","unit","layer")>(?1,?2,?3,?4,?5) ORDER BY "world","ledger","account","unit","layer" LIMIT ?6"#,
            5,
            &[0, 1, 2, 3, 4],
        ),
        LiveStage::Reservations => (
            r#"SELECT world,ledger,id,record,version,commit_seq,created_at FROM "_norn_quantity_reservation" ORDER BY "world","ledger","id" LIMIT ?1"#,
            r#"SELECT world,ledger,id,record,version,commit_seq,created_at FROM "_norn_quantity_reservation" WHERE ("world","ledger","id")>(?1,?2,?3) ORDER BY "world","ledger","id" LIMIT ?4"#,
            3,
            &[0, 1, 2],
        ),
    };
    let rows = if cursor.is_empty() {
        engine.query(sql_first, &[SCRUB_PAGE_I64.into()])?
    } else {
        let mut params = cursor.to_vec();
        params.push(SCRUB_PAGE_I64.into());
        engine.query(sql_next, &params)?
    };
    match_page_rows(rows, count, false, expected, indices)
}

fn verify_table_rows<S: SqlEngine>(
    engine: &S,
    table: &str,
    columns: &str,
    count: usize,
    is_history: bool,
    mut expected: Rows,
) -> Result<(), CellError> {
    let limit = row_limit(expected.len())?;
    let rows = engine.query(
        &format!("SELECT {columns} FROM \"{table}\" LIMIT ?1"),
        &[limit.into()],
    )?;
    remove_matching_rows(rows, count, is_history, &mut expected)?;
    if !expected.is_empty() {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "compare every normalized projection against immutable receipt replay"
)]
pub(crate) fn verify<S: SqlEngine>(
    engine: &S,
    receipts: impl IntoIterator<Item = Result<Receipt, CellError>>,
) -> Result<(), CellError> {
    super::clock_floor(engine)?;
    let mut replay = QuantityReplay::default();
    let mut history = Rows::new();
    let mut postings = Rows::new();
    for receipt in receipts {
        let receipt = receipt?;
        replay.step_receipt(&receipt, &mut history, &mut postings)?;
    }
    for stage in [
        LiveStage::Timers,
        LiveStage::Ledgers,
        LiveStage::Balances,
        LiveStage::Reservations,
    ] {
        let mut cursor = Vec::new();
        let mut remaining = replay.live_rows(stage);
        while let PageStep::More {
            next_cursor,
            remaining: rest,
        } = verify_live_page(engine, stage, &cursor, remaining)?
        {
            cursor = next_cursor;
            remaining = rest;
        }
    }
    verify_table_rows(
        engine,
        "_norn_quantity_history",
        "world,ledger,account,unit,layer,posted,held,version,commit_seq",
        5,
        true,
        history,
    )?;
    verify_table_rows(
        engine,
        "_norn_quantity_posting",
        "world,ledger,id,record,reverses,version,commit_seq",
        3,
        false,
        postings,
    )
}
