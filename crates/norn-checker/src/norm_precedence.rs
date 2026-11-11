//! Pointwise consistency of a closed, grounded deontic model. Grounding must
//! exhaust the actual definition domains and preserve conditions/exceptions.
//! This module checks that model; it does not issue permissions or prove that
//! obligations have been fulfilled. Only declared priority edges resolve conflicts.
//!
//! U3 (#718): admission is one whole-set decision made by the proved core in
//! [`decision`]. The closure, the topological order and each context's
//! resolution are untrusted search the core re-checks; conflict cores and
//! corrections come from the untrusted explainer in
//! [`crate::consistency::diagnosis`] only after the core refuses.
use norn_ir::deontic::DeonticModality;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod decision;

/// One definition norm grounded over one common finite action/subject universe.
#[derive(Debug, Clone)]
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

impl ConstitutionLayer {
    /// The proved core's rank: a smaller rank is a higher layer, as `Ord` orders them.
    fn rank(self) -> u8 {
        match self {
            Self::Religious => 0,
            Self::Legal => 1,
            Self::Industry => 2,
            Self::Company => 3,
            Self::Preference => 4,
        }
    }
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
    validate_order(norms, origins, layers, edges, protected, contexts)?;
    let (core, reach) = decision_inputs(norms, layers, edges, protected, contexts);
    if !decision::admits(&core, edges, &reach, contexts) {
        let report = diagnose_declared_order(
            norms, origins, layers, edges, protected, contexts, max_calls,
        )?;
        return Err(refusal(report.diagnosis));
    }
    require_decision_budget(max_calls)?;
    let mut declared: BTreeMap<usize, Vec<(usize, PriorityReason)>> = BTreeMap::new();
    for (high, row) in reach.iter().enumerate() {
        for (low, reaches) in row.iter().enumerate() {
            if let (true, Some(a), Some(b)) = (*reaches, norms.get(high), norms.get(low))
                && opposed(a.modality, b.modality)
            {
                declared
                    .entry(high)
                    .or_default()
                    .push((low, PriorityReason::Declared));
            }
        }
    }
    let resolved = resolve_contexts(norms, contexts, &declared).map_err(NormRefusal::Grounding)?;
    for (context, result) in resolved.iter().enumerate() {
        let defeats: Vec<_> = result.defeats.iter().map(|d| (d.winner, d.loser)).collect();
        if !decision::resolution_holds(
            &core,
            &reach,
            context,
            &result.survivors,
            &result.obligations,
            result.conflicts.len(),
            &defeats,
        ) {
            return Err(NormRefusal::Diagnosis(
                crate::consistency::diagnosis::Unknown::InconsistentOracle,
            ));
        }
    }
    Ok(resolved)
}

/// The proved core's inputs: one decision norm per grounded norm, with its
/// layer rank, protection and per-context activity, and the untrusted closure
/// of the declared edges as a square certificate.
fn decision_inputs(
    norms: &[GroundedNorm],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
) -> (Vec<decision::Norm>, Vec<Vec<bool>>) {
    let core = norms
        .iter()
        .zip(layers)
        .enumerate()
        .map(|(index, (norm, layer))| decision::Norm {
            modality: match norm.modality {
                DeonticModality::Obligation => decision::Modality::Obligation,
                DeonticModality::Permission => decision::Modality::Permission,
                DeonticModality::Prohibition => decision::Modality::Prohibition,
            },
            layer: layer.rank(),
            protected: protected.contains(&index),
            active: (0..contexts)
                .map(|c| norm.applicable.contains(&c) && !norm.excepted.contains(&c))
                .collect(),
        })
        .collect();
    let all: BTreeSet<_> = (0..norms.len()).collect();
    let closure = order_closure(edges, &all);
    let reach = (0..norms.len())
        .map(|high| {
            (0..norms.len())
                .map(|low| closure.contains(&(high, low)))
                .collect()
        })
        .collect();
    (core, reach)
}

/// The former whole-set query and its independent recheck took two calls; an
/// admitted order still needs that much budget, so callers see the same refusals.
fn require_decision_budget(max_calls: usize) -> Result<(), NormRefusal> {
    if max_calls < 2 {
        return Err(NormRefusal::Diagnosis(
            crate::consistency::diagnosis::Unknown::Budget,
        ));
    }
    Ok(())
}

/// The core refused; the explainer must agree, or the refusal stays unknown.
fn refusal(diagnosis: crate::consistency::diagnosis::Diagnosis) -> NormRefusal {
    if diagnosis.cores.is_empty() {
        NormRefusal::Diagnosis(crate::consistency::diagnosis::Unknown::InconsistentOracle)
    } else {
        NormRefusal::Conflict(diagnosis.cores)
    }
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
    validate_order(norms, origins, layers, edges, protected, contexts)?;
    let audit = crate::deontic::installed_protection(protected, edges);
    let (core, reach) = decision_inputs(norms, layers, edges, &audit, contexts);
    if decision::admits(&core, edges, &reach, contexts) {
        require_decision_budget(max_calls)?;
        // Nothing to explain: no core, and the empty relaxation is the only one.
        return Ok(DeclaredOrderReport {
            diagnosis: crate::consistency::diagnosis::Diagnosis {
                cores: BTreeSet::new(),
                corrections: BTreeSet::from([BTreeSet::new()]),
                calls: 0,
                max_calls,
            },
            diagnostics: Vec::new(),
        });
    }
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
    if report.diagnosis.cores.is_empty() {
        return Err(refusal(report.diagnosis));
    }
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
    validate_order(norms, origins, layers, edges, protected, contexts)?;
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
        diagnosis::diagnose_subsets(&mut oracle, norms.len(), editable, budget.0)
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

/// Limits, grounding and exact source bindings, checked before any decision.
fn validate_order(
    norms: &[GroundedNorm],
    origins: &[crate::consistency::RuleOrigin],
    layers: &[ConstitutionLayer],
    edges: &[(usize, usize)],
    protected: &BTreeSet<usize>,
    contexts: usize,
) -> Result<(), NormRefusal> {
    if norms.len() > crate::consistency::diagnosis::MAX_DECLARATIONS || edges.len() > 4096 {
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
    Ok(())
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

#[cfg(test)]
mod admission_tests {
    use super::*;
    use proptest::prelude::*;

    const LAYERS: [ConstitutionLayer; 3] = [
        ConstitutionLayer::Legal,
        ConstitutionLayer::Company,
        ConstitutionLayer::Preference,
    ];
    const GRADES: [DeonticModality; 3] = [
        DeonticModality::Obligation,
        DeonticModality::Permission,
        DeonticModality::Prohibition,
    ];

    /// A small grounding: per norm (grade, layer, applicable bits, excepted
    /// bits), the declared edges, the protected set and the context count.
    type Grounding = (
        Vec<GroundedNorm>,
        Vec<ConstitutionLayer>,
        Vec<(usize, usize)>,
        BTreeSet<usize>,
        usize,
    );

    fn grounding() -> impl Strategy<Value = Grounding> {
        (1_usize..=5, 1_usize..=3).prop_flat_map(|(n, contexts)| {
            (
                proptest::collection::vec((0_usize..3, 0_usize..3, 0_u8..8, 0_u8..8), n),
                proptest::collection::btree_set((0..n, 0..n), 0..=n.saturating_mul(2)),
                proptest::collection::btree_set(0..n, 0..=n),
                Just(contexts),
            )
                .prop_map(|(rows, edges, protected, contexts)| {
                    let mut norms = Vec::new();
                    let mut layers = Vec::new();
                    for (index, (grade, layer, applicable, excepted)) in
                        rows.into_iter().enumerate()
                    {
                        let bits = |mask: u8| -> BTreeSet<usize> {
                            (0..contexts).filter(|c| mask >> c & 1 == 1).collect()
                        };
                        let applicable = bits(applicable);
                        let excepted = bits(excepted).intersection(&applicable).copied().collect();
                        norms.push(GroundedNorm {
                            id: format!("rule.{index}"),
                            modality: GRADES[grade],
                            authority: ("board".into(), "1".into()),
                            authority_level: None,
                            enacted: 0,
                            applicable,
                            excepted,
                        });
                        layers.push(LAYERS[layer]);
                    }
                    (
                        norms,
                        layers,
                        edges.into_iter().collect(),
                        protected,
                        contexts,
                    )
                })
        })
    }

    fn origins(norms: &[GroundedNorm]) -> Vec<crate::consistency::RuleOrigin> {
        (0..norms.len())
            .map(|i| crate::consistency::RuleOrigin {
                artifact_id: Some("rules".into()),
                pointer: format!("/{i}"),
            })
            .collect()
    }

    /// The former decision: the explainer oracle's whole-set query.
    fn oracle_admits(
        (norms, layers, edges, protected, contexts): &Grounding,
        candidate: bool,
    ) -> bool {
        use crate::consistency::diagnosis::{Oracle, Verdict};
        let all: BTreeSet<_> = (0..norms.len()).collect();
        let mut oracle = OrderOracle {
            edges,
            conflicts: if candidate {
                Vec::new()
            } else {
                order_conflicts(norms, layers, edges, protected, *contexts, &all)
            },
            dynamic: candidate.then_some(SubsetOrder {
                norms,
                layers,
                protected,
                contexts: *contexts,
            }),
        };
        matches!(oracle.check(&all), Verdict::Sat)
    }

    mod witnesses {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../verification/semantics-v1/Checker/NormPrecedence/witnesses.rs"
        ));
    }

    /// The gate's pinned mutant witnesses, as the core and the oracle see them.
    fn witness(w: &witnesses::Witness) -> (Grounding, Vec<decision::Norm>, Vec<Vec<bool>>) {
        const RANKS: [ConstitutionLayer; 5] = [
            ConstitutionLayer::Religious,
            ConstitutionLayer::Legal,
            ConstitutionLayer::Industry,
            ConstitutionLayer::Company,
            ConstitutionLayer::Preference,
        ];
        let mut norms = Vec::new();
        let mut core = Vec::new();
        let mut layers = Vec::new();
        let mut protected = BTreeSet::new();
        for (index, &(grade, layer, guarded, active)) in w.norms.iter().enumerate() {
            norms.push(GroundedNorm {
                id: format!("rule.{index}"),
                modality: GRADES[usize::from(grade)],
                authority: ("board".into(), "1".into()),
                authority_level: None,
                enacted: 0,
                applicable: (0..w.contexts).filter(|c| active[*c]).collect(),
                excepted: BTreeSet::new(),
            });
            core.push(decision::Norm {
                modality: [
                    decision::Modality::Obligation,
                    decision::Modality::Permission,
                    decision::Modality::Prohibition,
                ][usize::from(grade)],
                layer,
                protected: guarded,
                active: active.to_vec(),
            });
            layers.push(RANKS[usize::from(layer)]);
            if guarded {
                protected.insert(index);
            }
        }
        let reach = w.reach.iter().map(|row| row.to_vec()).collect();
        (
            (norms, layers, w.edges.to_vec(), protected, w.contexts),
            core,
            reach,
        )
    }

    /// Every pinned witness carries the oracle's verdict, and the unmutated
    /// core agrees with it; the gate requires each mutant to disagree.
    #[test]
    fn witnesses_match_the_oracle() {
        fn set<T: Ord + Clone>(items: &[T]) -> BTreeSet<T> {
            items.iter().cloned().collect()
        }
        for w in witnesses::WITNESSES {
            let (g, core, reach) = witness(w);
            let admitted = decision::admits(&core, w.edges, &reach, w.contexts);
            let Some(r) = &w.resolution else {
                assert_eq!(oracle_admits(&g, false), w.expected, "{}", w.mutant);
                assert_eq!(admitted, w.expected, "{}", w.mutant);
                continue;
            };
            assert!(admitted, "{}", w.mutant);
            let (norms, layers, edges, protected, contexts) = &g;
            let resolved = check_norm_precedence(
                norms,
                &origins(norms),
                layers,
                edges,
                protected,
                *contexts,
                4096,
            )
            .expect("an admitted witness resolves");
            let truth = &resolved[r.context];
            let defeats: Vec<_> = truth.defeats.iter().map(|d| (d.winner, d.loser)).collect();
            let same = set(&truth.survivors) == set(r.survivors)
                && set(&truth.obligations) == set(r.obligations)
                && truth.conflicts.len() == r.conflicts
                && set(&defeats) == set(r.defeats);
            assert_eq!(same, w.expected, "{}", w.mutant);
            assert_eq!(
                decision::resolution_holds(
                    &core,
                    &reach,
                    r.context,
                    r.survivors,
                    r.obligations,
                    r.conflicts,
                    r.defeats
                ),
                w.expected,
                "{}",
                w.mutant
            );
        }
    }

    proptest! {
        /// U3 is behaviour-preserving: the proved core admits exactly what the
        /// whole-set oracle query admitted, on both admission paths, and an
        /// admitted order resolves with every context certified.
        #[test]
        fn core_decides_exactly_as_the_whole_set_query(g in grounding()) {
            let (norms, layers, edges, protected, contexts) = &g;
            let (core, reach) = decision_inputs(norms, layers, edges, protected, *contexts);
            let admitted = decision::admits(&core, edges, &reach, *contexts);
            prop_assert_eq!(admitted, oracle_admits(&g, false));
            let audit = crate::deontic::installed_protection(protected, edges);
            let (candidate, _) = decision_inputs(norms, layers, edges, &audit, *contexts);
            prop_assert_eq!(decision::admits(&candidate, edges, &reach, *contexts), oracle_admits(&g, true));
            let checked = check_norm_precedence(norms, &origins(norms), layers, edges, protected, *contexts, 4096);
            if admitted {
                prop_assert!(checked.is_ok());
            } else {
                prop_assert!(matches!(checked, Err(NormRefusal::Conflict(_))));
            }
        }

        /// The closure certificate is exact: with any one entry of the true
        /// closure flipped, nothing is admitted.
        #[test]
        fn any_wrong_closure_entry_refuses(
            g in grounding(),
            high in any::<proptest::sample::Index>(),
            low in any::<proptest::sample::Index>(),
        ) {
            let (norms, layers, edges, protected, contexts) = &g;
            let (core, mut reach) = decision_inputs(norms, layers, edges, protected, *contexts);
            let entry = &mut reach[high.index(norms.len())][low.index(norms.len())];
            *entry = !*entry;
            prop_assert!(!decision::admits(&core, edges, &reach, *contexts));
        }
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
