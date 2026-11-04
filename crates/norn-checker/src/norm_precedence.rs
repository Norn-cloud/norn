//! Pointwise consistency of a closed, grounded deontic model. Grounding must
//! exhaust the actual definition domains and preserve conditions/exceptions.
//! This module checks that model; it does not issue permissions or prove that
//! obligations have been fulfilled. Only declared priority edges resolve conflicts.
use norn_ir::deontic::DeonticModality;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One definition norm grounded over one common finite action/subject universe.
#[derive(Clone)]
pub struct GroundedNorm {
    /// Exact definition rule identity, used to explain every result.
    pub id: String,
    /// Original normative grade.
    pub modality: DeonticModality,
    /// Selected source identity and exact version (neither is inferred).
    pub authority: (String, String),
    /// Optional legacy ranking, consumed only by the Info discrepancy note.
    /// It never supplies a priority edge to the declared-only resolver.
    pub authority_level: Option<u32>,
    /// Legacy enactment ordinal after effective-version selection; never recorded recency.
    pub enacted: u64,
    /// Pre-exception applicability. Specificity uses this common universe.
    pub applicable: BTreeSet<usize>,
    /// Contexts in which a declared exception applies.
    pub excepted: BTreeSet<usize>,
}

/// Declared resolution or diagnostic-only historical comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityReason {
    /// Explicit declared edge or its strict transitive closure.
    Declared,
    /// Info-only legacy comparison of explicitly selected authority levels.
    Superior,
    /// Info-only legacy enactment comparison within the same authority/version.
    Posterior,
    /// Info-only legacy strict containment of pre-exception applicability.
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

/// Legacy comparison has no public resolution entry point. Its only caller
/// emits an Info note; the result never changes declared-only admission.
fn legacy_precedence(
    norms: &[GroundedNorm],
    contexts: usize,
) -> Result<Vec<NormContext>, NormUnknown> {
    validate(norms, contexts)?;
    if norms.iter().any(|n| n.authority_level.is_none()) {
        return Err(NormUnknown::MissingAuthority);
    }
    if norms.iter().any(|n| {
        norms.iter().any(|other| {
            n.authority == other.authority && n.authority_level != other.authority_level
        })
    }) {
        return Err(NormUnknown::Invalid);
    }
    let mut edges: BTreeMap<usize, Vec<(usize, PriorityReason)>> = BTreeMap::new();
    for (high, winner) in norms.iter().enumerate() {
        for (low, loser) in norms.iter().enumerate() {
            if opposed(winner.modality, loser.modality)
                && let Some(reason) = preferred(winner, loser)
            {
                edges.entry(high).or_default().push((low, reason));
            }
        }
    }
    resolve_contexts(norms, contexts, &edges)
}

fn resolve_contexts(
    norms: &[GroundedNorm],
    contexts: usize,
    edges: &BTreeMap<usize, Vec<(usize, PriorityReason)>>,
) -> Result<Vec<NormContext>, NormUnknown> {
    let mut incoming: BTreeMap<usize, usize> =
        norms.iter().enumerate().map(|(i, _)| (i, 0)).collect();
    for (low, _) in edges.values().flatten() {
        let count = incoming.get_mut(low).ok_or(NormUnknown::Invalid)?;
        *count = count.checked_add(1).ok_or(NormUnknown::Limit)?;
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
    /// Exact rule-index cores and corrections under the retained editability profile.
    pub diagnosis: crate::consistency::diagnosis::Diagnosis,
    /// Source-located errors and constitutional-policy gap notes (#767).
    pub diagnostics: Vec<crate::Diagnostic>,
}

/// Resolution was refused, or no exhaustive normative conclusion was established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormRefusal {
    /// Exact declaration-index cores; no resolved contexts are returned.
    Conflict(BTreeSet<BTreeSet<usize>>),
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
    dynamic: Option<SubsetOrder<'a>>,
}

struct SubsetOrder<'a> {
    norms: &'a [GroundedNorm],
    layers: &'a [ConstitutionLayer],
    protected: &'a BTreeSet<usize>,
    contexts: usize,
}

impl crate::consistency::diagnosis::Oracle for OrderOracle<'_> {
    type Unknown = std::convert::Infallible;
    fn check(
        &mut self,
        active: &BTreeSet<usize>,
    ) -> crate::consistency::diagnosis::Verdict<Self::Unknown> {
        use crate::consistency::diagnosis::Verdict;
        let conflicts = self.dynamic.as_ref().map(|order| {
            let edges: Vec<_> = self
                .edges
                .iter()
                .copied()
                .filter(|(a, b)| active.contains(a) && active.contains(b))
                .collect();
            let audit = crate::deontic::installed_protection(order.protected, &edges);
            order_conflicts(
                order.norms,
                order.layers,
                &edges,
                &audit,
                order.contexts,
                active,
            )
        });
        let conflicts = conflicts.as_ref().unwrap_or(&self.conflicts);
        let reach = order_closure(self.edges, active);
        if active.iter().any(|i| reach.contains(&(*i, *i)))
            || conflicts.iter().any(|pair| pair.is_subset(active))
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

/// D1 / LAW-RETAIN-1 / OVERLAY-MONOTONE-1: resolve only declared priorities.
/// Layer/protection checks and complete conflict diagnosis precede precedence.
/// Numeric authority, enactment and specificity never break a tie. Grounding
/// must select effective versions first and preserve each rule's own exceptions.
/// # Errors
/// Returns exact conflict cores, malformed-binding refusals or unknown on limits.
pub fn check_norm_precedence(
    norms: &[GroundedNorm],
    origins: &[crate::consistency::RuleOrigin],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
    max_calls: usize,
) -> Result<Vec<NormContext>, NormRefusal> {
    let report = diagnose_declared_order(
        norms, origins, layers, edges, protected, contexts, max_calls,
    )?;
    if !report.diagnosis.cores.is_empty() {
        return Err(NormRefusal::Conflict(report.diagnosis.cores));
    }
    let all: BTreeSet<_> = (0..norms.len()).collect();
    let mut declared: BTreeMap<usize, Vec<(usize, PriorityReason)>> = BTreeMap::new();
    for (high, low) in order_closure(edges, &all) {
        if let (Some(a), Some(b)) = (norms.get(high), norms.get(low))
            && opposed(a.modality, b.modality)
        {
            declared
                .entry(high)
                .or_default()
                .push((low, PriorityReason::Declared));
        }
    }
    resolve_contexts(norms, contexts, &declared).map_err(NormRefusal::Grounding)
}

/// LAW-RETAIN-1 / OVERLAY-MONOTONE-1: diagnose cross-layer clashes, even when
/// the Info-only legacy comparison records a defeat. Within a
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
) -> Result<DeclaredOrderReport, NormRefusal> {
    diagnose_order(
        norms,
        origins,
        layers,
        edges,
        protected,
        contexts,
        (max_calls, None),
    )
}

/// Admission repairs may relax only candidate declarations; base laws remain fixed.
pub(crate) fn diagnose_candidate_order(
    norms: &[GroundedNorm],
    origins: &[crate::consistency::RuleOrigin],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
    max_calls: usize,
) -> Result<DeclaredOrderReport, NormRefusal> {
    let editable = (0..norms.len())
        .filter(|i| !protected.contains(i))
        .collect();
    let mut report = diagnose_order(
        norms,
        origins,
        layers,
        edges,
        protected,
        contexts,
        (max_calls, Some(&editable)),
    )?;
    if !report.diagnosis.cores.is_empty() {
        let repairs: Vec<Vec<_>> = report
            .diagnosis
            .corrections
            .iter()
            .map(|repair| {
                repair
                    .iter()
                    .filter_map(|i| norms.get(*i).map(|n| n.id.as_str()))
                    .collect()
            })
            .collect();
        for diagnostic in &mut report.diagnostics {
            if diagnostic.is_error() {
                use std::fmt::Write;
                write!(diagnostic.message, " Irreducible conflict sets and independently rechecked candidate-only relaxation proposals: {repairs:?}. Protected installed laws cannot be edited. Query budget: {}/{}. Concrete edits require ordinary admission.", report.diagnosis.calls, report.diagnosis.max_calls).map_err(|_| NormRefusal::InvalidBinding)?;
            }
        }
    }
    Ok(report)
}

fn diagnose_order(
    norms: &[GroundedNorm],
    origins: &[crate::consistency::RuleOrigin],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
    budget: (usize, Option<&BTreeSet<usize>>),
) -> Result<DeclaredOrderReport, NormRefusal> {
    use crate::consistency::{diagnosis, report::core_findings};
    if norms.len() > diagnosis::MAX_DECLARATIONS || edges.len() > 4096 {
        return Err(NormRefusal::Limit);
    }
    validate(norms, contexts).map_err(NormRefusal::Grounding)?;
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
        return Err(NormRefusal::InvalidBinding);
    }
    let all: BTreeSet<_> = (0..norms.len()).collect();
    let conflicts = if budget.1.is_none() {
        order_conflicts(norms, layers, edges, protected, contexts, &all)
    } else {
        Vec::new()
    };
    let mut oracle = OrderOracle {
        edges,
        conflicts,
        dynamic: budget.1.map(|_| SubsetOrder {
            norms,
            layers,
            protected,
            contexts,
        }),
    };
    let diagnosis = if let Some(editable) = budget.1 {
        diagnose_subsets(&mut oracle, norms.len(), editable, budget.0)
    } else {
        diagnosis::diagnose(&mut oracle, norms.len(), &BTreeSet::new(), budget.0)
    }
    .map_err(NormRefusal::Diagnosis)?;
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
    if let Ok(legacy) = legacy_precedence(norms, contexts) {
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
                        &format!("Legacy norm_precedence records {} over {} by {:?} in defeats; the constitutional clash remains diagnosed here. Superior wins first; posterior/specialis require identical authority/version/level. See #767; this comparison has no resolution authority.", norms.get(defeat.winner).map_or("", |n| n.id.as_str()), norms.get(defeat.loser).map_or("", |n| n.id.as_str()), defeat.reason),
                        &[origins.get(defeat.winner), origins.get(defeat.loser)].into_iter().flatten().collect::<Vec<_>>()));
                }
            }
        }
    }
    diagnostics
}

fn order_conflicts(
    norms: &[GroundedNorm],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
    active: &BTreeSet<usize>,
) -> Vec<BTreeSet<usize>> {
    let mut conflicts = Vec::new();
    // Reject even inactive or exception-masked lower-to-higher override declarations.
    for &(high, low) in edges {
        if layers.get(high) > layers.get(low) || protected.contains(&low) {
            conflicts.push(BTreeSet::from([high, low]));
        }
    }
    let reach = order_closure(edges, active);
    for (i, left) in norms.iter().enumerate() {
        for (j, right) in norms.iter().enumerate().skip(i.saturating_add(1)) {
            if active.contains(&i)
                && active.contains(&j)
                && opposed(left.modality, right.modality)
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
    conflicts
}

// Declaration removal can erase a priority path, so this oracle is not
// monotone. Enumerate subsets rather than pruning by monotone hitting sets.
fn diagnose_subsets(
    oracle: &mut impl crate::consistency::diagnosis::Oracle<Unknown = std::convert::Infallible>,
    declarations: usize,
    editable: &BTreeSet<usize>,
    max_calls: usize,
) -> Result<
    crate::consistency::diagnosis::Diagnosis,
    crate::consistency::diagnosis::Unknown<std::convert::Infallible>,
> {
    use crate::consistency::diagnosis::{Diagnosis, Unknown, Verdict};
    let all: BTreeSet<_> = (0..declarations).collect();
    let calls = std::cell::Cell::new(0_usize);
    let mut unsat = |active: &BTreeSet<usize>| {
        if calls.get() >= max_calls {
            return Err(Unknown::Budget);
        }
        calls.set(calls.get().saturating_add(1));
        Ok(matches!(oracle.check(active), Verdict::Unsat))
    };
    let mut cores = BTreeSet::new();
    let mut corrections = BTreeSet::new();
    if unsat(&all)? {
        // Non-monotone diagnosis visits every subset, including inherited rules.
        // The full-set query is included in that count. At least one minimal
        // core must also be independently revalidated after enumeration.
        let minimum_calls = u32::try_from(declarations)
            .ok()
            .and_then(|n| 1_usize.checked_shl(n))
            .and_then(|count| count.checked_add(1));
        if minimum_calls.is_none_or(|count| count > max_calls) {
            return Err(Unknown::Budget);
        }
        let mut pending = vec![all.clone()];
        let mut checked = BTreeMap::from([(all.clone(), true)]);
        while let Some(active) = pending.pop() {
            for member in &active {
                let mut subset = active.clone();
                subset.remove(member);
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    checked.entry(subset.clone())
                {
                    entry.insert(unsat(&subset)?);
                    pending.push(subset);
                }
            }
        }
        for (active, conflict) in &checked {
            if *conflict {
                if !checked
                    .iter()
                    .any(|(subset, bad)| *bad && subset != active && subset.is_subset(active))
                {
                    cores.insert(active.clone());
                }
            } else {
                let repair: BTreeSet<_> = all.difference(active).copied().collect();
                if repair.is_subset(editable)
                    && !checked
                        .iter()
                        .any(|(kept, bad)| !bad && kept != active && active.is_subset(kept))
                {
                    corrections.insert(repair);
                }
            }
        }
    } else {
        corrections.insert(BTreeSet::new());
    }
    // Independent checks consume the same ceiling as subset recomputation.
    crate::consistency::diagnosis::require_validation_budget(
        &cores,
        &corrections,
        max_calls.saturating_sub(calls.get()),
    )?;
    for core in &cores {
        if !unsat(core)? {
            return Err(Unknown::InconsistentOracle);
        }
        for member in core {
            let mut subset = core.clone();
            subset.remove(member);
            if unsat(&subset)? {
                return Err(Unknown::InconsistentOracle);
            }
        }
    }
    for repair in &corrections {
        let kept: BTreeSet<_> = all.difference(repair).copied().collect();
        if unsat(&kept)? {
            return Err(Unknown::InconsistentOracle);
        }
        for member in repair {
            let mut restored = kept.clone();
            restored.insert(*member);
            if !unsat(&restored)? {
                return Err(Unknown::InconsistentOracle);
            }
        }
    }
    Ok(Diagnosis {
        cores,
        corrections,
        calls: calls.get(),
        max_calls,
    })
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    struct CountingOracle {
        calls: usize,
    }

    impl crate::consistency::diagnosis::Oracle for CountingOracle {
        type Unknown = std::convert::Infallible;

        fn check(
            &mut self,
            active: &BTreeSet<usize>,
        ) -> crate::consistency::diagnosis::Verdict<Self::Unknown> {
            use crate::consistency::diagnosis::Verdict;
            self.calls += 1;
            if active.contains(&1) && (active.contains(&0) || active.contains(&2)) {
                Verdict::Unsat
            } else {
                Verdict::Sat
            }
        }
    }

    #[test]
    fn impossible_subset_search_stops_after_full_set_query() {
        use crate::consistency::diagnosis::Unknown;
        for declarations in [12, 13, 32, 64, 65] {
            let mut oracle = CountingOracle { calls: 0 };
            assert_eq!(
                diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 4096),
                Err(Unknown::Budget)
            );
            assert_eq!(oracle.calls, 1);
        }
    }

    struct SatCountingOracle {
        calls: usize,
    }

    impl crate::consistency::diagnosis::Oracle for SatCountingOracle {
        type Unknown = std::convert::Infallible;

        fn check(
            &mut self,
            _active: &BTreeSet<usize>,
        ) -> crate::consistency::diagnosis::Verdict<Self::Unknown> {
            self.calls += 1;
            crate::consistency::diagnosis::Verdict::Sat
        }
    }

    #[test]
    fn satisfiable_sets_keep_two_call_fast_path_regardless_of_subset_count() {
        use crate::consistency::diagnosis::Unknown;
        for declarations in [13, 64, 65] {
            let mut oracle = SatCountingOracle { calls: 0 };
            let result = diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 4096)
                .expect("SAT needs no subset enumeration");
            assert!(result.cores.is_empty());
            assert_eq!(result.corrections, BTreeSet::from([BTreeSet::new()]));
            assert_eq!(result.calls, 2);
            assert_eq!(oracle.calls, 2);
            let mut oracle = SatCountingOracle { calls: 0 };
            assert_eq!(
                diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 2)
                    .expect("exact SAT budget")
                    .calls,
                2
            );
            assert_eq!(oracle.calls, 2);
            let mut oracle = SatCountingOracle { calls: 0 };
            assert_eq!(
                diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 1),
                Err(Unknown::Budget)
            );
            assert_eq!(oracle.calls, 1);
        }
    }

    #[test]
    fn twelve_declarations_search_and_recheck_overlapping_cores() {
        use crate::consistency::diagnosis::Unknown;
        let editable = (0..12).collect();
        let mut oracle = CountingOracle { calls: 0 };
        // 4096 subsets, two three-query core checks, two repair/restoration
        // checks (two and three queries): the exact complete budget is 4107.
        let result = diagnose_subsets(&mut oracle, 12, &editable, 4107).expect("fits");
        assert_eq!(result.calls, 4107);
        assert_eq!(oracle.calls, result.calls);
        assert_eq!(
            result.cores,
            BTreeSet::from([BTreeSet::from([0, 1]), BTreeSet::from([1, 2])])
        );
        assert_eq!(
            result.corrections,
            BTreeSet::from([BTreeSet::from([1]), BTreeSet::from([0, 2])])
        );
        let mut oracle = CountingOracle { calls: 0 };
        assert_eq!(
            diagnose_subsets(&mut oracle, 12, &editable, result.calls - 1),
            Err(Unknown::Budget)
        );
        // Refuse the known-incomplete validation batch before querying it.
        assert_eq!(oracle.calls, 4096);
    }

    #[test]
    fn repairs_recompute_paths_with_one_shared_budget() -> Result<(), NormRefusal> {
        use crate::consistency::{RuleOrigin, diagnosis::Unknown};
        use DeonticModality::{Obligation, Prohibition};
        let norms: Vec<_> = [Obligation, Obligation, Prohibition]
            .into_iter()
            .enumerate()
            .map(|(i, modality)| GroundedNorm {
                id: format!("{i}"),
                modality,
                authority: ("board".into(), "1".into()),
                authority_level: Some(0),
                enacted: 0,
                applicable: BTreeSet::from([0]),
                excepted: BTreeSet::new(),
            })
            .collect();
        let origins: Vec<_> = (0..3)
            .map(|i| RuleOrigin {
                artifact_id: Some("rules".into()),
                pointer: format!("/{i}"),
            })
            .collect();
        let run = |budget| {
            diagnose_candidate_order(
                &norms,
                &origins,
                &[ConstitutionLayer::Company; 3],
                &[(0, 1), (1, 1), (1, 2)],
                &BTreeSet::new(),
                1,
                budget,
            )
        };
        let report = run(100)?;
        assert_eq!(
            report.diagnosis.cores,
            BTreeSet::from([BTreeSet::from([1]), BTreeSet::from([0, 2])])
        );
        assert_eq!(
            report.diagnosis.corrections,
            BTreeSet::from([BTreeSet::from([0, 1]), BTreeSet::from([1, 2])])
        );
        assert!(report.diagnosis.calls <= 100);
        assert!(matches!(
            run(report.diagnosis.calls.saturating_sub(1)),
            Err(NormRefusal::Diagnosis(Unknown::Budget))
        ));
        assert_eq!(
            run(report.diagnosis.calls)?.diagnosis.calls,
            report.diagnosis.calls
        );
        Ok(())
    }
}
