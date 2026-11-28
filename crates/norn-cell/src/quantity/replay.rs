//! Reconstruct quantity caches exclusively from verified, typed receipts.
use super::{
    BTreeMap, BTreeSet, CellError, OriginalPostingState, QuantityError, QuantityOperation,
    QuantityPosting, QuantityReservation, QuantityState, Receipt, SqlEngine, SqlValue,
    canonical_json, prepare_quantity, to_i64,
};

use serde::{Deserialize, Serialize};

impl<T> std::ops::Deref for crate::store::QuantityBatch<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<T> From<T> for crate::store::QuantityBatch<T> {
    fn from(value: T) -> Self {
        Self(vec![value])
    }
}

impl<T: Serialize> Serialize for crate::store::QuantityBatch<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => one.serialize(serializer),
            many => many.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for crate::store::QuantityBatch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape<T> {
            One(T),
            Many(Vec<T>),
        }
        Ok(Self(match Shape::deserialize(deserializer)? {
            Shape::One(one) => vec![one],
            Shape::Many(many) => many,
        }))
    }
}

impl crate::store::ReceiptBody {
    pub(crate) fn quantity_requests(&self) -> &[norn_ir::quantity_ledger::QuantityRequest] {
        self.quantity.as_ref().map_or(&[], |requests| &requests.0)
    }

    pub(crate) fn quantity_result(
        &self,
        ledger: &str,
    ) -> Option<&norn_kernel::quantity::QuantityResult> {
        let requests = self.quantity.as_ref()?;
        let results = self.quantity_result.as_ref()?;
        if requests.len() != results.len() {
            return None;
        }
        requests
            .iter()
            .zip(results.iter())
            .find_map(|(request, result)| (request.ledger_id == ledger).then_some(result))
    }
}

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
        state.original = OriginalPostingState::Absent;
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
                state.original = match ledger.postings.get(original_id).cloned() {
                    None => OriginalPostingState::Absent,
                    Some(posting) if ledger.reversed.contains(original_id) => {
                        OriginalPostingState::AlreadyReversed(posting)
                    }
                    Some(posting) => OriginalPostingState::Active(posting),
                };
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
            if value.status.is_open() {
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
                && value.status.is_open()
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

mod scrub;
pub(crate) use scrub::QuantityScrub;

const ROW_PAGE: usize = 256;
const ROW_PAGE_I64: i64 = 256;

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

enum PageStep {
    Done {
        read: usize,
    },
    More {
        next_cursor: Vec<SqlValue>,
        remaining: Rows,
    },
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
    let read = rows.len();
    let page_full = read >= ROW_PAGE;
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
    Ok(PageStep::Done { read })
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
        engine.query(sql_first, &[ROW_PAGE_I64.into()])?
    } else {
        let mut params = cursor.to_vec();
        params.push(ROW_PAGE_I64.into());
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
        let (mut cursor, mut remaining) = (Vec::new(), replay.live_rows(stage));
        while let PageStep::More {
            next_cursor,
            remaining: rest,
        } = verify_live_page(engine, stage, &cursor, remaining)?
        {
            cursor = next_cursor;
            remaining = rest;
        }
    }
    for (table, cols, count, is_history, expected) in [
        (
            "_norn_quantity_history",
            "world,ledger,account,unit,layer,posted,held,version,commit_seq",
            5,
            true,
            history,
        ),
        (
            "_norn_quantity_posting",
            "world,ledger,id,record,reverses,version,commit_seq",
            3,
            false,
            postings,
        ),
    ] {
        verify_table_rows(engine, table, cols, count, is_history, expected)?;
    }
    Ok(())
}
