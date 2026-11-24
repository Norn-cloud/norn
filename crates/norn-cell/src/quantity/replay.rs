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

    /// Verify one bounded scrub history chunk `(after, through]`: replay each
    /// receipt and compare the append-only `_norn_quantity_history` and
    /// `_norn_quantity_posting` rows emitted in that commit range (S10).
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
            self.step_receipt(receipt, &mut history, &mut postings)?;
        }
        verify_append_range(engine, after, through, history, postings)
    }

    /// Verify `clock_floor`, `validate_boundary`, surplus append-only rows, and
    /// all live quantity projections at the end of a full scrub (`through`).
    /// Returns `Ok(false)` only if more than 512 local quantity commits landed
    /// after `through` while the scrub ran, signaling a bounded target refresh.
    pub(crate) fn verify_tail<S: SqlEngine>(
        &self,
        engine: &S,
        reader: &crate::read::StoreReader<'_, S>,
        through: u64,
        target_boundary: Option<super::FormatBoundary>,
    ) -> Result<bool, CellError> {
        super::clock_floor(engine)?;
        let db_commits = reader.next_commit_seq()?.saturating_sub(1);
        if db_commits < through {
            return Err(QuantityError::Corrupt.into());
        }
        let through_i64 = to_i64(through, "quantity scrub target")?;
        let suffix_commits = if db_commits == through {
            Vec::new()
        } else {
            let rows = engine.query(
                r#"SELECT "commit_seq" FROM "_norn_receipt"
                   WHERE "commit_seq" > ?1 AND (
                       json_extract("receipt", '$.quantity.version') = 'norn.quantity.commit/2'
                       OR json_type("receipt", '$.quantity') = 'array'
                   )
                   ORDER BY "commit_seq" LIMIT 513"#,
                &[through_i64.into()],
            )?;
            if rows.len() > crate::scrub::RECEIPT_STEP {
                return Ok(false);
            }
            rows.into_iter()
                .map(|row| {
                    crate::read::to_u64(crate::engine::SqlRow(row).get(0)?, "quantity commit")
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        if suffix_commits.is_empty() {
            super::validate_boundary(engine, target_boundary)?;
            check_no_rows(
                engine,
                r#"SELECT 1 FROM "_norn_quantity_history" WHERE "commit_seq" > ?1 LIMIT 1"#,
                &[through_i64.into()],
            )?;
            check_no_rows(
                engine,
                r#"SELECT 1 FROM "_norn_quantity_posting" WHERE "commit_seq" > ?1 LIMIT 1"#,
                &[through_i64.into()],
            )?;
            self.verify_live_tables(engine)?;
            return Ok(true);
        }
        let mut tail = self.clone();
        let mut suffix_history = Rows::new();
        let mut suffix_postings = Rows::new();
        for commit in suffix_commits {
            let receipt = reader
                .receipt_at(commit)?
                .filter(|receipt| receipt.body.quantity.is_some())
                .ok_or(QuantityError::Corrupt)?;
            tail.step_receipt(&receipt, &mut suffix_history, &mut suffix_postings)?;
        }
        verify_append_after(engine, through_i64, suffix_history, suffix_postings)?;
        super::format_boundary(engine)?;
        tail.verify_live_tables(engine)?;
        Ok(true)
    }

    fn verify_live_tables<S: SqlEngine>(&self, engine: &S) -> Result<(), CellError> {
        let mut timers = self.timers.clone();
        let limit = row_limit(timers.len())?;
        for row in engine.query(
            r#"SELECT timer_id,due,timer,commit_seq FROM "_norn_timer" WHERE "timer_id">='norn.quantity.expiry/' AND "timer_id"<'norn.quantity.expiry0' LIMIT ?1"#,
            &[limit.into()],
        )? {
            if timers.remove(&key(&row, 1)?).as_ref() != Some(&row) {
                return Err(QuantityError::Corrupt.into());
            }
        }
        if !timers.is_empty() {
            return Err(QuantityError::Corrupt.into());
        }
        for (table, columns, count, expected) in [
            (
                "_norn_quantity_ledger",
                "world,ledger,version,pack,unrestricted,defined_at",
                2,
                self.heads.clone(),
            ),
            (
                "_norn_quantity_balance",
                "world,ledger,account,unit,layer,posted,held,version",
                5,
                self.balances.clone(),
            ),
            (
                "_norn_quantity_reservation",
                "world,ledger,id,record,version,commit_seq,created_at",
                3,
                self.reservations.clone(),
            ),
        ] {
            verify_table_rows(engine, table, columns, count, false, expected)?;
        }
        Ok(())
    }
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

fn verify_rows_match(
    rows: Vec<Vec<SqlValue>>,
    count: usize,
    is_history: bool,
    mut expected: Rows,
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
    if !expected.is_empty() {
        return Err(QuantityError::Corrupt.into());
    }
    Ok(())
}

fn verify_table_rows<S: SqlEngine>(
    engine: &S,
    table: &str,
    columns: &str,
    count: usize,
    is_history: bool,
    expected: Rows,
) -> Result<(), CellError> {
    let limit = row_limit(expected.len())?;
    let rows = engine.query(
        &format!("SELECT {columns} FROM \"{table}\" LIMIT ?1"),
        &[limit.into()],
    )?;
    verify_rows_match(rows, count, is_history, expected)
}

fn verify_append_range<S: SqlEngine>(
    engine: &S,
    after: u64,
    through: u64,
    history: Rows,
    postings: Rows,
) -> Result<(), CellError> {
    let after_i64 = to_i64(after, "quantity history after")?;
    let through_i64 = to_i64(through, "quantity history through")?;
    let hist_limit = row_limit(history.len())?;
    let hist_rows = engine.query(
        r#"SELECT world,ledger,account,unit,layer,posted,held,version,commit_seq FROM "_norn_quantity_history" WHERE "commit_seq" > ?1 AND "commit_seq" <= ?2 LIMIT ?3"#,
        &[after_i64.into(), through_i64.into(), hist_limit.into()],
    )?;
    verify_rows_match(hist_rows, 5, true, history)?;
    let post_limit = row_limit(postings.len())?;
    let post_rows = engine.query(
        r#"SELECT world,ledger,id,record,reverses,version,commit_seq FROM "_norn_quantity_posting" WHERE "commit_seq" > ?1 AND "commit_seq" <= ?2 LIMIT ?3"#,
        &[after_i64.into(), through_i64.into(), post_limit.into()],
    )?;
    verify_rows_match(post_rows, 3, false, postings)
}

fn verify_append_after<S: SqlEngine>(
    engine: &S,
    after_i64: i64,
    history: Rows,
    postings: Rows,
) -> Result<(), CellError> {
    let hist_limit = row_limit(history.len())?;
    let hist_rows = engine.query(
        r#"SELECT world,ledger,account,unit,layer,posted,held,version,commit_seq FROM "_norn_quantity_history" WHERE "commit_seq" > ?1 LIMIT ?2"#,
        &[after_i64.into(), hist_limit.into()],
    )?;
    verify_rows_match(hist_rows, 5, true, history)?;
    let post_limit = row_limit(postings.len())?;
    let post_rows = engine.query(
        r#"SELECT world,ledger,id,record,reverses,version,commit_seq FROM "_norn_quantity_posting" WHERE "commit_seq" > ?1 LIMIT ?2"#,
        &[after_i64.into(), post_limit.into()],
    )?;
    verify_rows_match(post_rows, 3, false, postings)
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
    replay.verify_live_tables(engine)?;
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

/// Verify quantity projections for a bounded catch-up scrub chunk starting from
/// an already-validated checkpoint (mirrors `read::ledger`'s base > 0 check).
pub(crate) fn verify_catch_up_chunk<S: SqlEngine>(
    engine: &S,
    receipts: &[Receipt],
) -> Result<(), CellError> {
    for receipt in receipts {
        if let Some(requests) = &receipt.body.quantity {
            for request in requests.iter() {
                super::verify_transition_projection(
                    engine,
                    &receipt.body.world,
                    receipt.body.commit_seq,
                    request,
                )?;
            }
        }
    }
    Ok(())
}
