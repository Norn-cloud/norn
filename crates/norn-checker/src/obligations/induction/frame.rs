//! Induction frame decision core: when a transition writer cannot affect a law.
//!
//! This file imports nothing from the crate, so the pinned Charon/Aeneas
//! extract the production body standalone (`scripts/verify-checker-certificates-aeneas.sh`).
//! `induction.rs` computes the law's direct field reads and asks this core
//! whether a writer's preservation goal is discharged by its frame.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Aeneas models indexing and `+` as checked; the Lean proof shows neither fails"
)]

fn same<F: PartialEq>(left: &F, right: &F) -> bool {
    left == right
}

fn reads_field<F: PartialEq>(reads: &[F], field: &F) -> bool {
    let mut i = 0;
    while i < reads.len() {
        if same(&reads[i], field) {
            return true;
        }
        i += 1;
    }
    false
}

/// A transition writer on `writer_object` that writes only `written` discharges
/// preservation of a law on `law_subject` when the law's complete direct reads
/// are known (`Some`) and do not include `written`.
#[must_use]
pub(crate) fn discharges<F: PartialEq>(
    law_subject: &F,
    writer_object: &F,
    written: &F,
    reads: Option<&[F]>,
) -> bool {
    match reads {
        None => false,
        Some(fields) => same(writer_object, law_subject) && !reads_field(fields, written),
    }
}
