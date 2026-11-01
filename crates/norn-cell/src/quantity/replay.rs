//! Reconstruct quantity caches exclusively from verified, typed receipts.
use super::{
    BTreeMap, BTreeSet, CellError, QuantityError, QuantityOperation, QuantityPosting,
    QuantityReservation, QuantityState, Receipt, SqlEngine, SqlValue, canonical_json,
    prepare_quantity, to_i64,
};

#[derive(Default)]
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

#[derive(Default)]
struct Replay {
    ledgers: BTreeMap<(String, String), Ledger>,
    heads: Rows,
    balances: Rows,
    history: Rows,
    reservations: Rows,
    postings: Rows,
    timers: Rows,
}
impl Replay {
    #[allow(
        clippy::too_many_lines,
        reason = "one ruled request updates its complete derived projections"
    )]
    fn request(
        &mut self,
        body: &crate::store::ReceiptBody,
        request: &super::QuantityRequest,
        result: &norn_kernel::quantity::QuantityResult,
    ) -> Result<(), CellError> {
        let Self {
            ledgers,
            heads,
            balances,
            history,
            reservations,
            postings,
            timers,
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
    let mut replay = Replay::default();
    let mut floor = 0;
    for receipt in receipts {
        let receipt = receipt?;
        let body = &receipt.body;
        if body.logical_time < floor {
            return Err(QuantityError::Corrupt.into());
        }
        floor = body.logical_time;
        if let Some(requests) = &body.quantity {
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
                replay.request(body, request, result)?;
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
        }
    }
    let Replay {
        heads,
        balances,
        history,
        reservations,
        postings,
        mut timers,
        ..
    } = replay;
    let limit = to_i64(
        u64::try_from(timers.len())
            .map_err(|_range| QuantityError::Corrupt)?
            .checked_add(1)
            .ok_or(QuantityError::Corrupt)?,
        "quantity timer count",
    )?;
    for row in engine.query(r#"SELECT timer_id,due,timer,commit_seq FROM "_norn_timer" WHERE "timer_id">='norn.quantity.expiry/' AND "timer_id"<'norn.quantity.expiry0' LIMIT ?1"#, &[limit.into()])? {
        if timers.remove(&key(&row, 1)?).as_ref() != Some(&row) { return Err(QuantityError::Corrupt.into()); }
    }
    if !timers.is_empty() {
        return Err(QuantityError::Corrupt.into());
    }
    for (table, columns, count, mut expected) in [
        (
            "_norn_quantity_ledger",
            "world,ledger,version,pack,unrestricted,defined_at",
            2,
            heads,
        ),
        (
            "_norn_quantity_balance",
            "world,ledger,account,unit,layer,posted,held,version",
            5,
            balances,
        ),
        (
            "_norn_quantity_history",
            "world,ledger,account,unit,layer,posted,held,version,commit_seq",
            5,
            history,
        ),
        (
            "_norn_quantity_reservation",
            "world,ledger,id,record,version,commit_seq,created_at",
            3,
            reservations,
        ),
        (
            "_norn_quantity_posting",
            "world,ledger,id,record,reverses,version,commit_seq",
            3,
            postings,
        ),
    ] {
        // One surplus row suffices to refuse; never materialize an attacker-
        // enlarged cache beyond the number of rows the immutable fold proved.
        let limit = expected
            .len()
            .checked_add(1)
            .ok_or(QuantityError::Corrupt)?;
        let limit = i64::try_from(limit).map_err(|_range| QuantityError::Corrupt)?;
        for row in engine.query(
            &format!("SELECT {columns} FROM \"{table}\" LIMIT ?1"),
            &[limit.into()],
        )? {
            let mut row_key = key(&row, count)?;
            if table == "_norn_quantity_history" {
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
    }
    Ok(())
}
