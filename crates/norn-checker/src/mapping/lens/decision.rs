//! Finite write-back lens decision core: table lookups and the three lens laws.
//!
//! This file imports nothing from the crate, so the pinned Charon/Aeneas
//! extract the production body standalone (`scripts/verify-checker-certificates-aeneas.sh`).
//! `lens.rs` evaluates the mapping's Get expression into the `get` table and
//! delegates every lookup and law check here.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Aeneas models indexing and `+` as checked; the Lean proof shows neither fails"
)]

/// Why a lookup or a law check refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The source state is outside the declared domain.
    UnknownSource,
    /// The put table names no declared destination state.
    UnknownPutBack,
    /// `put(s, get(s)) != s` at this source row.
    GetPut(usize),
    /// `get(put(s, v)) != v` at this source row and view.
    PutGet(usize, bool),
    /// `put(put(s, v), w) != put(s, w)` at this source row and views.
    PutPut(usize, bool, bool),
}

fn same<S: PartialEq>(left: &S, right: &S) -> bool {
    left == right
}

/// The first row of the domain equal to `source`.
fn position<S: PartialEq>(sources: &[S], source: &S) -> Option<usize> {
    let mut row = 0;
    while row < sources.len() {
        if same(&sources[row], source) {
            return Some(row);
        }
        row += 1;
    }
    None
}

/// The checked view of a source state.
///
/// # Errors
/// `UnknownSource` for a state outside the domain or without a view.
pub fn get<S: PartialEq>(sources: &[S], get: &[bool], source: &S) -> Result<bool, Refusal> {
    let Some(row) = position(sources, source) else {
        return Err(Refusal::UnknownSource);
    };
    if row < get.len() {
        Ok(get[row])
    } else {
        Err(Refusal::UnknownSource)
    }
}

/// The checked put-back of `view` into a source state.
///
/// # Errors
/// `UnknownSource` outside the domain; `UnknownPutBack` for a missing destination.
pub fn put<'a, S: PartialEq>(
    sources: &'a [S],
    put: &[[usize; 2]],
    source: &S,
    view: bool,
) -> Result<&'a S, Refusal> {
    let Some(row) = position(sources, source) else {
        return Err(Refusal::UnknownSource);
    };
    if row >= put.len() {
        return Err(Refusal::UnknownPutBack);
    }
    let destination = if view { put[row][1] } else { put[row][0] };
    if destination < sources.len() {
        Ok(&sources[destination])
    } else {
        Err(Refusal::UnknownPutBack)
    }
}

/// `PutGet` for one view and `PutPut` for both following views.
fn check_view<S: PartialEq>(
    sources: &[S],
    get_table: &[bool],
    put_table: &[[usize; 2]],
    row: usize,
    view: bool,
) -> Result<(), Refusal> {
    let source = &sources[row];
    let updated = put(sources, put_table, source, view)?;
    if get(sources, get_table, updated)? != view {
        return Err(Refusal::PutGet(row, view));
    }
    if !same(
        put(sources, put_table, updated, false)?,
        put(sources, put_table, source, false)?,
    ) {
        return Err(Refusal::PutPut(row, view, false));
    }
    if !same(
        put(sources, put_table, updated, true)?,
        put(sources, put_table, source, true)?,
    ) {
        return Err(Refusal::PutPut(row, view, true));
    }
    Ok(())
}

/// `GetPut`, `PutGet` and `PutPut` at every source state and every view, in row order.
///
/// # Errors
/// The first refuted law, or a lookup outside the declared tables.
pub fn check_laws<S: PartialEq>(
    sources: &[S],
    get_table: &[bool],
    put_table: &[[usize; 2]],
) -> Result<(), Refusal> {
    let mut row = 0;
    while row < sources.len() {
        let source = &sources[row];
        let current = get(sources, get_table, source)?;
        if !same(put(sources, put_table, source, current)?, source) {
            return Err(Refusal::GetPut(row));
        }
        check_view(sources, get_table, put_table, row, false)?;
        check_view(sources, get_table, put_table, row, true)?;
        row += 1;
    }
    Ok(())
}
