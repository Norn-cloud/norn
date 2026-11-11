//! U3 (#718, lane C-C): the trusted decision core of declared norm precedence.
//!
//! This file is self-contained (no crate, collection, iterator or closure) so
//! Charon and Aeneas extract it on its own. `scripts/verify-norm-precedence-aeneas.sh`
//! proves it against `verification/semantics-v1/NornSem/Norm.lean`.
//!
//! The core only checks. The parent module's untrusted search computes the
//! priority closure, the topological order and each context's resolution, and
//! hands them in as certificates: `reach` must be exactly the transitive
//! closure of the declared edges, and a resolution must be exactly the stable
//! set of undefeated applicable norms. A certificate that does not check
//! refuses. Explanations (conflict cores, corrections) never reach this file.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "index loops keep the core extractable; the Lean proof shows every index is in bounds and no increment overflows"
)]

/// Normative grade, mirroring `norn_ir::deontic::DeonticModality`.
#[derive(Clone, Copy)]
pub(crate) enum Modality {
    /// Something must hold.
    Obligation,
    /// Something may hold. It is never inferred from an absent prohibition.
    Permission,
    /// Something must not hold.
    Prohibition,
}

/// One grounded norm as the decision sees it.
pub(crate) struct Norm {
    /// Its normative grade.
    pub(crate) modality: Modality,
    /// Constitution layer rank. A smaller rank is a higher layer.
    pub(crate) layer: u8,
    /// No declared edge may defeat this norm.
    pub(crate) protected: bool,
    /// Per context: applicable and not lifted by one of its own exceptions.
    pub(crate) active: Vec<bool>,
}

fn prohibits(modality: Modality) -> bool {
    match modality {
        Modality::Prohibition => true,
        Modality::Obligation | Modality::Permission => false,
    }
}

fn obliges(modality: Modality) -> bool {
    match modality {
        Modality::Obligation => true,
        Modality::Permission | Modality::Prohibition => false,
    }
}

/// `norm` applies, unexcepted, in `context`.
fn active_in(norm: &Norm, context: usize) -> bool {
    norm.active[context]
}

/// A prohibition opposes an obligation or a permission.
fn opposed(left: &Norm, right: &Norm) -> bool {
    prohibits(left.modality) != prohibits(right.modality)
}

/// Every row of `reach` is square over the norms and every mask covers every
/// context.
fn rows_shaped(norms: &[Norm], reach: &[Vec<bool>], contexts: usize) -> bool {
    let mut i = 0;
    while i < norms.len() {
        if reach[i].len() != norms.len() || norms[i].active.len() != contexts {
            return false;
        }
        i += 1;
    }
    true
}

/// Every edge joins two norms.
fn edges_in_range(edges: &[(usize, usize)], norms: usize) -> bool {
    let mut e = 0;
    while e < edges.len() {
        let high = edges[e].0;
        let low = edges[e].1;
        if high >= norms || low >= norms {
            return false;
        }
        e += 1;
    }
    true
}

/// `reach` is square over the norms, every mask covers every context, and every
/// edge joins two norms.
fn well_shaped(
    norms: &[Norm],
    edges: &[(usize, usize)],
    reach: &[Vec<bool>],
    contexts: usize,
) -> bool {
    reach.len() == norms.len()
        && rows_shaped(norms, reach, contexts)
        && edges_in_range(edges, norms.len())
}

/// Every declared edge points to the same or a lower layer, never into a
/// protected norm.
fn edges_respect_layers(norms: &[Norm], edges: &[(usize, usize)]) -> bool {
    let mut e = 0;
    while e < edges.len() {
        let high = edges[e].0;
        let low = edges[e].1;
        if norms[high].layer > norms[low].layer || norms[low].protected {
            return false;
        }
        e += 1;
    }
    true
}

/// `reach` contains every declared edge.
fn reach_contains_edges(edges: &[(usize, usize)], reach: &[Vec<bool>]) -> bool {
    let mut e = 0;
    while e < edges.len() {
        let high = edges[e].0;
        let low = edges[e].1;
        if !reach[high][low] {
            return false;
        }
        e += 1;
    }
    true
}

/// Everything `source` reaches, `sink` reaches too.
fn row_within(reach: &[Vec<bool>], source: usize, sink: usize) -> bool {
    let mut j = 0;
    while j < reach.len() {
        if reach[source][j] && !reach[sink][j] {
            return false;
        }
        j += 1;
    }
    true
}

/// `reach` is transitive through every step out of `i`.
fn row_transitive(reach: &[Vec<bool>], i: usize) -> bool {
    let mut k = 0;
    while k < reach.len() {
        if reach[i][k] && !row_within(reach, k, i) {
            return false;
        }
        k += 1;
    }
    true
}

/// `reach` is transitive.
fn reach_transitive(reach: &[Vec<bool>]) -> bool {
    let mut i = 0;
    while i < reach.len() {
        if !row_transitive(reach, i) {
            return false;
        }
        i += 1;
    }
    true
}

/// Some declared edge leaves `source` toward `target`: it ends at `target` or
/// at a norm that reaches `target`.
fn justified(edges: &[(usize, usize)], reach: &[Vec<bool>], source: usize, target: usize) -> bool {
    let mut e = 0;
    while e < edges.len() {
        let high = edges[e].0;
        let low = edges[e].1;
        if high == source && (low == target || reach[low][target]) {
            return true;
        }
        e += 1;
    }
    false
}

/// Every pair `reach` puts out of `i` is justified by a declared edge.
fn row_justified(edges: &[(usize, usize)], reach: &[Vec<bool>], i: usize) -> bool {
    let mut j = 0;
    while j < reach.len() {
        if reach[i][j] && !justified(edges, reach, i, j) {
            return false;
        }
        j += 1;
    }
    true
}

/// Every pair in `reach` is justified by a declared edge.
fn reach_justified(edges: &[(usize, usize)], reach: &[Vec<bool>]) -> bool {
    let mut i = 0;
    while i < reach.len() {
        if !row_justified(edges, reach, i) {
            return false;
        }
        i += 1;
    }
    true
}

/// No norm reaches itself: the declared priorities are acyclic.
fn irreflexive(reach: &[Vec<bool>]) -> bool {
    let mut i = 0;
    while i < reach.len() {
        if reach[i][i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Both norms apply, unexcepted, in some context.
fn co_active(left: &Norm, right: &Norm, contexts: usize) -> bool {
    let mut c = 0;
    while c < contexts {
        if active_in(left, c) && active_in(right, c) {
            return true;
        }
        c += 1;
    }
    false
}

/// Opposing norms that meet in some context share a layer and are ordered by
/// the declared closure.
fn clash_resolved(
    norms: &[Norm],
    reach: &[Vec<bool>],
    contexts: usize,
    i: usize,
    j: usize,
) -> bool {
    !opposed(&norms[i], &norms[j])
        || !co_active(&norms[i], &norms[j], contexts)
        || (norms[i].layer == norms[j].layer && (reach[i][j] || reach[j][i]))
}

/// Every clash between `i` and a later norm is resolved.
fn clashes_resolved_from(norms: &[Norm], reach: &[Vec<bool>], contexts: usize, i: usize) -> bool {
    let mut j = i + 1;
    while j < norms.len() {
        if !clash_resolved(norms, reach, contexts, i, j) {
            return false;
        }
        j += 1;
    }
    true
}

/// Every clash is resolved.
fn clashes_resolved(norms: &[Norm], reach: &[Vec<bool>], contexts: usize) -> bool {
    let mut i = 0;
    while i < norms.len() {
        if !clashes_resolved_from(norms, reach, contexts, i) {
            return false;
        }
        i += 1;
    }
    true
}

/// D1 / LAW-RETAIN-1 / OVERLAY-MONOTONE-1: the whole-set admission decision.
/// `reach[high][low]` must be the transitive closure of `edges`.
pub(crate) fn admits(
    norms: &[Norm],
    edges: &[(usize, usize)],
    reach: &[Vec<bool>],
    contexts: usize,
) -> bool {
    well_shaped(norms, edges, reach, contexts)
        && edges_respect_layers(norms, edges)
        && reach_contains_edges(edges, reach)
        && reach_transitive(reach)
        && reach_justified(edges, reach)
        && irreflexive(reach)
        && clashes_resolved(norms, reach, contexts)
}

/// `list` contains `x`.
fn listed(list: &[usize], x: usize) -> bool {
    let mut k = 0;
    while k < list.len() {
        if list[k] == x {
            return true;
        }
        k += 1;
    }
    false
}

/// Every listed index names a norm.
fn indices_in_range(list: &[usize], norms: usize) -> bool {
    let mut k = 0;
    while k < list.len() {
        if list[k] >= norms {
            return false;
        }
        k += 1;
    }
    true
}

/// Every mask covers `context`.
fn context_in_range(norms: &[Norm], context: usize) -> bool {
    let mut i = 0;
    while i < norms.len() {
        if context >= norms[i].active.len() {
            return false;
        }
        i += 1;
    }
    true
}

/// A listed survivor has a declared priority over `loser` and opposes it.
fn defeated_by_survivor(
    norms: &[Norm],
    reach: &[Vec<bool>],
    survivors: &[usize],
    loser: usize,
) -> bool {
    let mut k = 0;
    while k < survivors.len() {
        let winner = survivors[k];
        if reach[winner][loser] && opposed(&norms[winner], &norms[loser]) {
            return true;
        }
        k += 1;
    }
    false
}

/// `i` is listed as a survivor exactly when it is active and no survivor
/// defeats it.
fn survives_exactly(
    norms: &[Norm],
    reach: &[Vec<bool>],
    survivors: &[usize],
    context: usize,
    i: usize,
) -> bool {
    let undefeated =
        active_in(&norms[i], context) && !defeated_by_survivor(norms, reach, survivors, i);
    listed(survivors, i) == undefeated
}

/// The survivors are exactly the active norms no survivor defeats.
fn survivors_stable(
    norms: &[Norm],
    reach: &[Vec<bool>],
    survivors: &[usize],
    context: usize,
) -> bool {
    let mut i = 0;
    while i < norms.len() {
        if !survives_exactly(norms, reach, survivors, context, i) {
            return false;
        }
        i += 1;
    }
    true
}

/// Every listed obligation is a surviving obligation.
fn obligations_survive(norms: &[Norm], survivors: &[usize], obligations: &[usize]) -> bool {
    let mut k = 0;
    while k < obligations.len() {
        let norm = obligations[k];
        if !obliges(norms[norm].modality) || !listed(survivors, norm) {
            return false;
        }
        k += 1;
    }
    true
}

/// Every surviving obligation is listed.
fn obligations_listed(norms: &[Norm], survivors: &[usize], obligations: &[usize]) -> bool {
    let mut k = 0;
    while k < survivors.len() {
        let norm = survivors[k];
        if obliges(norms[norm].modality) && !listed(obligations, norm) {
            return false;
        }
        k += 1;
    }
    true
}

/// A recorded defeat cites a survivor, an active loser and a declared priority
/// between opposing norms.
fn defeat_cited(
    norms: &[Norm],
    reach: &[Vec<bool>],
    survivors: &[usize],
    context: usize,
    winner: usize,
    loser: usize,
) -> bool {
    winner < norms.len()
        && loser < norms.len()
        && listed(survivors, winner)
        && active_in(&norms[loser], context)
        && reach[winner][loser]
        && opposed(&norms[winner], &norms[loser])
}

/// Every recorded defeat is cited.
fn defeats_cited(
    norms: &[Norm],
    reach: &[Vec<bool>],
    survivors: &[usize],
    defeats: &[(usize, usize)],
    context: usize,
) -> bool {
    let mut d = 0;
    while d < defeats.len() {
        let winner = defeats[d].0;
        let loser = defeats[d].1;
        if !defeat_cited(norms, reach, survivors, context, winner, loser) {
            return false;
        }
        d += 1;
    }
    true
}

/// Some recorded defeat has `loser` as its loser.
fn loser_recorded(defeats: &[(usize, usize)], loser: usize) -> bool {
    let mut d = 0;
    while d < defeats.len() {
        let recorded = defeats[d].1;
        if recorded == loser {
            return true;
        }
        d += 1;
    }
    false
}

/// `i`, if active and not a survivor, is recorded as defeated.
fn defeat_recorded(
    survivors: &[usize],
    defeats: &[(usize, usize)],
    active: bool,
    i: usize,
) -> bool {
    !active || listed(survivors, i) || loser_recorded(defeats, i)
}

/// Every active norm that does not survive is recorded as defeated.
fn defeats_complete(
    norms: &[Norm],
    survivors: &[usize],
    defeats: &[(usize, usize)],
    context: usize,
) -> bool {
    let mut i = 0;
    while i < norms.len() {
        if !defeat_recorded(survivors, defeats, active_in(&norms[i], context), i) {
            return false;
        }
        i += 1;
    }
    true
}

/// One context's resolution is the stable labelling the semantics defines:
/// no recorded conflict, survivors exactly the undefeated active norms,
/// obligations exactly the surviving obligations, and every defeat cited and
/// recorded. Empty survivors mean uncovered, never permission.
pub(crate) fn resolution_holds(
    norms: &[Norm],
    reach: &[Vec<bool>],
    context: usize,
    survivors: &[usize],
    obligations: &[usize],
    conflicts: usize,
    defeats: &[(usize, usize)],
) -> bool {
    conflicts == 0
        && context_in_range(norms, context)
        && indices_in_range(survivors, norms.len())
        && indices_in_range(obligations, norms.len())
        && survivors_stable(norms, reach, survivors, context)
        && obligations_survive(norms, survivors, obligations)
        && obligations_listed(norms, survivors, obligations)
        && defeats_cited(norms, reach, survivors, defeats, context)
        && defeats_complete(norms, survivors, defeats, context)
}
