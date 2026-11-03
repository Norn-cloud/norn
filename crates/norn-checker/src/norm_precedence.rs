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
    validate(norms, contexts)?;
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

fn validate(norms: &[GroundedNorm], contexts: usize) -> Result<(), NormUnknown> {
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
    Ok(())
}

/// Trusted constitution layers, highest first; no domain laws live in the checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConstitutionLayer {
    /// Highest declared normative layer.
    Religious,
    /// National or jurisdictional law.
    Legal,
    /// Industry standards.
    Industry,
    /// Company policy.
    Company,
    /// Lowest declared preference layer.
    Preference,
}

/// Diagnostic-only result for a supplied grounding of one action/subject.
/// This is not source lowering, admission, fulfilment or Datalog evidence.
pub struct DeclaredOrderReport {
    /// Exact rule-index cores; all declarations are protected from repair.
    pub diagnosis: crate::consistency::diagnosis::Diagnosis,
    /// Source-located errors and constitutional-policy gap notes (#767).
    pub diagnostics: Vec<crate::Diagnostic>,
}

/// No normative conclusion was established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredOrderUnknown {
    /// Finite analysis ceiling exceeded.
    Limit,
    /// Grounding is malformed or unsupported.
    Grounding(NormUnknown),
    /// Layer, edge or exact source binding is invalid.
    InvalidBinding,
    /// CD diagnosis did not complete; no partial cores.
    Diagnosis(crate::consistency::diagnosis::Unknown<std::convert::Infallible>),
}

struct OrderOracle<'a> {
    edges: &'a [(usize, usize)],
    conflicts: Vec<BTreeSet<usize>>,
}

impl crate::consistency::diagnosis::Oracle for OrderOracle<'_> {
    type Unknown = std::convert::Infallible;
    fn check(
        &mut self,
        active: &BTreeSet<usize>,
    ) -> crate::consistency::diagnosis::Verdict<Self::Unknown> {
        use crate::consistency::diagnosis::Verdict;
        let reach = order_closure(self.edges, active);
        if active.iter().any(|i| reach.contains(&(*i, *i)))
            || self.conflicts.iter().any(|pair| pair.is_subset(active))
        {
            Verdict::Unsat
        } else {
            Verdict::Sat
        }
    }
}

fn order_closure(edges: &[(usize, usize)], active: &BTreeSet<usize>) -> BTreeSet<(usize, usize)> {
    let mut reach: BTreeSet<_> = edges
        .iter()
        .copied()
        .filter(|(a, b)| active.contains(a) && active.contains(b))
        .collect();
    for k in active {
        for i in active {
            for j in active {
                if reach.contains(&(*i, *k)) && reach.contains(&(*k, *j)) {
                    reach.insert((*i, *j));
                }
            }
        }
    }
    reach
}

/// LAW-RETAIN-1 / OVERLAY-MONOTONE-1: diagnose cross-layer clashes, even when
/// legacy precedence records only a defeat. Superior already wins first; within a
/// layer only declared edges (winner, loser), including their transitive closure,
/// resolve opposing rules. An absent edge never acquires an implicit tie-break.
///
/// This first cut checks trusted exhaustive masks for one common action/subject,
/// not `DeonticIR` lowering. Preserve scope and explicit exceptions when grounding;
/// CTD, temporal and Datalog applicability remain unknown. Layers/edges are trusted.
/// Protected laws accept no incoming defeat. Select effective versions before
/// grounding; recorded recency is not enactment. Exceptions belong to their own rule.
///
/// # Errors
/// Returns unknown on malformed grounding, missing/duplicate sources, invalid
/// edges, or exhausted CD diagnosis budget. No partial result is returned.
pub fn diagnose_declared_order(
    norms: &[GroundedNorm],
    origins: &[crate::consistency::RuleOrigin],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
    max_calls: usize,
) -> Result<DeclaredOrderReport, DeclaredOrderUnknown> {
    use crate::consistency::{diagnosis, report::core_findings};
    if norms.len() > diagnosis::MAX_DECLARATIONS || edges.len() > 4096 {
        return Err(DeclaredOrderUnknown::Limit);
    }
    validate(norms, contexts).map_err(DeclaredOrderUnknown::Grounding)?;
    if origins.len() != norms.len()
        || layers.len() != norms.len()
        || origins.iter().collect::<BTreeSet<_>>().len() != origins.len()
        || origins.iter().any(|o| {
            o.artifact_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
                || (!o.pointer.is_empty() && !o.pointer.starts_with('/'))
        })
        || edges
            .iter()
            .any(|(a, b)| *a >= norms.len() || *b >= norms.len())
        || edges.iter().collect::<BTreeSet<_>>().len() != edges.len()
        || norms.iter().any(|n| !n.excepted.is_subset(&n.applicable))
        || protected.iter().any(|i| *i >= norms.len())
    {
        return Err(DeclaredOrderUnknown::InvalidBinding);
    }
    let all: BTreeSet<_> = (0..norms.len()).collect();
    let reach = order_closure(edges, &all);
    let mut conflicts = Vec::new();
    // Reject even inactive or exception-masked lower-to-higher override declarations.
    for &(high, low) in edges {
        if layers.get(high) > layers.get(low) || protected.contains(&low) {
            conflicts.push(BTreeSet::from([high, low]));
        }
    }
    for (i, left) in norms.iter().enumerate() {
        for (j, right) in norms.iter().enumerate().skip(i.saturating_add(1)) {
            if opposed(left.modality, right.modality)
                && (0..contexts).any(|c| {
                    left.applicable.contains(&c)
                        && right.applicable.contains(&c)
                        && !left.excepted.contains(&c)
                        && !right.excepted.contains(&c)
                })
                && (layers.get(i) != layers.get(j)
                    || (!reach.contains(&(i, j)) && !reach.contains(&(j, i))))
            {
                conflicts.push(BTreeSet::from([i, j]));
            }
        }
    }
    let mut oracle = OrderOracle { edges, conflicts };
    let diagnosis = diagnosis::diagnose(&mut oracle, norms.len(), &BTreeSet::new(), max_calls)
        .map_err(DeclaredOrderUnknown::Diagnosis)?;
    let mut diagnostics = Vec::new();
    for core in &diagnosis.cores {
        let ids: Vec<_> = core
            .iter()
            .map(|i| norms.get(*i).map_or("", |n| n.id.as_str()))
            .collect();
        let located: Vec<_> = core.iter().filter_map(|i| origins.get(*i)).collect();
        diagnostics.extend(core_findings("consistency.normative.conflict", &crate::DiagnosticSeverity::Error,
            &format!("Declared-order/layer conflict among rules: {}. Grounding is conditional; this is not admission or fulfilment evidence.", ids.join(", ")), &located));
    }
    diagnostics.extend(legacy_notes(norms, contexts, origins, &diagnosis));
    Ok(DeclaredOrderReport {
        diagnosis,
        diagnostics,
    })
}

fn legacy_notes(
    norms: &[GroundedNorm],
    contexts: usize,
    origins: &[crate::consistency::RuleOrigin],
    diagnosis: &crate::consistency::diagnosis::Diagnosis,
) -> Vec<crate::Diagnostic> {
    use crate::consistency::report::core_findings;
    let mut diagnostics = Vec::new();
    if let Ok(legacy) = check_norm_precedence(norms, contexts) {
        let mut noted = BTreeSet::new();
        for context in legacy {
            for defeat in context.defeats {
                if diagnosis
                    .cores
                    .iter()
                    .any(|core| core.contains(&defeat.winner) && core.contains(&defeat.loser))
                    && noted.insert((defeat.winner, defeat.loser, format!("{:?}", defeat.reason)))
                {
                    diagnostics.extend(core_findings("consistency.normative.legacy_resolution", &crate::DiagnosticSeverity::Info,
                        &format!("Legacy norm_precedence records {} over {} by {:?} in defeats; the constitutional clash remains diagnosed here. Superior wins first; posterior/specialis require identical authority/version/level. See #767; admission is unchanged.", norms.get(defeat.winner).map_or("", |n| n.id.as_str()), norms.get(defeat.loser).map_or("", |n| n.id.as_str()), defeat.reason),
                        &[origins.get(defeat.winner), origins.get(defeat.loser)].into_iter().flatten().collect::<Vec<_>>()));
                }
            }
        }
    }
    diagnostics
}
