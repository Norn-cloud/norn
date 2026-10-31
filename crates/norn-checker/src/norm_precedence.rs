//! Pointwise consistency of a closed, grounded deontic model. Grounding must
//! exhaust the actual definition domains and preserve conditions/exceptions.
//! This module checks that model; it does not issue permissions or prove that
//! obligations have been fulfilled. Authority order is a trusted policy input.
use norn_ir::deontic::DeonticModality;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One definition norm grounded over one common finite action/subject universe.
pub struct GroundedNorm {
    /// Exact definition rule identity, used to explain every result.
    pub id: String,
    /// Original normative grade.
    pub modality: DeonticModality,
    /// Selected source identity and exact version (neither is inferred).
    pub authority: (String, String),
    /// Explicit tenant-selected order: larger levels are superior. Missing
    /// authority selection is unknown, never a default priority.
    pub authority_level: Option<u32>,
    /// Comparable enactment ordinal within this exact authority lineage only.
    pub enacted: u64,
    /// Pre-exception applicability. Specificity uses this common universe.
    pub applicable: BTreeSet<usize>,
    /// Contexts in which a declared exception applies.
    pub excepted: BTreeSet<usize>,
}

/// First decisive precedence criterion, in normative order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityReason {
    /// Lex superior: explicitly selected authority level.
    Superior,
    /// Lex posterior: later enactment within the same authority/version.
    Posterior,
    /// Lex specialis: strict subset of pre-exception applicability.
    Specialis,
}

/// A surviving norm defeats an opposing applicable norm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormDefeat {
    /// Index of the surviving norm in the supplied catalog.
    pub winner: usize,
    /// Index of the norm it defeats.
    pub loser: usize,
    /// Why it wins; both catalog identities remain available to the caller.
    pub reason: PriorityReason,
}

/// One pointwise result. Empty survivors mean uncovered, never permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormContext {
    /// Undefeated applicable norms, by catalog index.
    pub survivors: Vec<usize>,
    /// Outstanding obligations among survivors; consistency is not discharge.
    pub obligations: Vec<usize>,
    /// Each pair of opposing survivors is an unresolved conflict.
    pub conflicts: Vec<(usize, usize)>,
    /// Explanations cite only winners that actually survived.
    pub defeats: Vec<NormDefeat>,
}

/// No exhaustive consistency result was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormUnknown {
    /// Empty/malformed identities, inconsistent source levels or invalid masks.
    Invalid,
    /// A source has no selected authority order.
    MissingAuthority,
    /// The finite model exceeds the admitted bound.
    Limit,
    /// The derived strict preference relation contains a cycle.
    Cycle,
}

fn opposed(left: DeonticModality, right: DeonticModality) -> bool {
    (left == DeonticModality::Prohibition) != (right == DeonticModality::Prohibition)
}

fn preferred(high: &GroundedNorm, low: &GroundedNorm) -> Option<PriorityReason> {
    if high.authority_level > low.authority_level {
        return Some(PriorityReason::Superior);
    }
    // Different authorities at the same level are incomparable, not equal.
    if high.authority_level != low.authority_level || high.authority != low.authority {
        return None;
    }
    if high.enacted > low.enacted {
        return Some(PriorityReason::Posterior);
    }
    if high.enacted == low.enacted
        && high.applicable != low.applicable
        && high.applicable.is_subset(&low.applicable)
    {
        return Some(PriorityReason::Specialis);
    }
    None
}

/// Evaluate every context, retaining conflicts and outstanding obligations.
/// Callers must bind catalog, grounding, domain and authority selection into
/// evidence; this is not evidence about arbitrary ungrounded definitions.
///
/// # Errors
/// Refuses malformed, unselected, cyclic or oversized models. The model must
/// describe a single bound action/subject; timed discharge is outside its scope.
pub fn check_norm_precedence(
    norms: &[GroundedNorm],
    contexts: usize,
) -> Result<Vec<NormContext>, NormUnknown> {
    if contexts == 0 || contexts > crate::table_analysis::MAX_CONTEXTS || norms.len() > 128 {
        return Err(NormUnknown::Limit);
    }
    let mut ids = BTreeSet::new();
    for norm in norms {
        if norm.authority_level.is_none() {
            return Err(NormUnknown::MissingAuthority);
        }
        if norm.id.trim().is_empty()
            || norm.id.len() > 256
            || !ids.insert(&norm.id)
            || norm.authority.0.trim().is_empty()
            || norm.authority.1.trim().is_empty()
            || norm
                .applicable
                .iter()
                .chain(&norm.excepted)
                .any(|index| *index >= contexts)
            || norms.iter().any(|other| {
                other.authority == norm.authority && other.authority_level != norm.authority_level
            })
        {
            return Err(NormUnknown::Invalid);
        }
    }
    let mut edges: BTreeMap<usize, Vec<(usize, PriorityReason)>> = BTreeMap::new();
    let mut incoming: BTreeMap<usize, usize> =
        norms.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    for (high, winner) in norms.iter().enumerate() {
        for (low, loser) in norms.iter().enumerate() {
            if opposed(winner.modality, loser.modality)
                && let Some(reason) = preferred(winner, loser)
            {
                edges.entry(high).or_default().push((low, reason));
                let count = incoming.get_mut(&low).ok_or(NormUnknown::Invalid)?;
                *count = count.checked_add(1).ok_or(NormUnknown::Limit)?;
            }
        }
    }
    let mut queue: VecDeque<_> = incoming
        .iter()
        .filter_map(|(i, count)| (*count == 0).then_some(*i))
        .collect();
    let mut order = Vec::new();
    while let Some(index) = queue.pop_front() {
        order.push(index);
        for (low, _) in edges.get(&index).into_iter().flatten() {
            let count = incoming.get_mut(low).ok_or(NormUnknown::Invalid)?;
            *count = count.checked_sub(1).ok_or(NormUnknown::Invalid)?;
            if *count == 0 {
                queue.push_back(*low);
            }
        }
    }
    if order.len() != norms.len() {
        return Err(NormUnknown::Cycle);
    }
    let mut results = Vec::new();
    for context in 0..contexts {
        let active: BTreeSet<_> = norms
            .iter()
            .enumerate()
            .filter_map(|(i, norm)| {
                (norm.applicable.contains(&context) && !norm.excepted.contains(&context))
                    .then_some(i)
            })
            .collect();
        let mut defeated = BTreeSet::new();
        let mut result = NormContext {
            survivors: Vec::new(),
            obligations: Vec::new(),
            conflicts: Vec::new(),
            defeats: Vec::new(),
        };
        for high in &order {
            if !active.contains(high) || defeated.contains(high) {
                continue;
            }
            result.survivors.push(*high);
            if norms.get(*high).ok_or(NormUnknown::Invalid)?.modality == DeonticModality::Obligation
            {
                result.obligations.push(*high);
            }
            for (low, reason) in edges.get(high).into_iter().flatten() {
                if active.contains(low) && !defeated.contains(low) {
                    defeated.insert(*low);
                    result.defeats.push(NormDefeat {
                        winner: *high,
                        loser: *low,
                        reason: *reason,
                    });
                }
            }
        }
        for (offset, left) in result.survivors.iter().enumerate() {
            for right in result.survivors.iter().skip(offset.saturating_add(1)) {
                if opposed(
                    norms.get(*left).ok_or(NormUnknown::Invalid)?.modality,
                    norms.get(*right).ok_or(NormUnknown::Invalid)?.modality,
                ) {
                    result.conflicts.push((*left, *right));
                }
            }
        }
        results.push(result);
    }
    Ok(results)
}
