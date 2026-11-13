//! A UI machine composed with a backend lifecycle (#1236, UI-MACHINE-1 × LIFE-SOUND).
//!
//! [`compose`] builds the reachable product of two [`Net`]s. A UI edge either fires together
//! with a backend transition (the UI's `accepted` edge from a state awaiting action A fires
//! with A's transition) or alone while the backend stays put (refused, timeout, network failure,
//! cancelled, plain user edges). Backend transitions that no UI edge is synchronised with (other
//! actors, timers) interleave freely. A product edge is may when either side's edge is may, so
//! the product never proves more than its parts. The product is an ordinary [`Net`]: `analyse`,
//! `witness` and `input_free_cycles` apply unchanged, and witnesses are edge-index paths that
//! [`Product::origins`] maps back to each side.

use std::collections::{BTreeMap, BTreeSet};

use super::{Ending, Modality, Net, NetEdge};

/// Stable codes of composition refusals (registered, en and ar).
pub(crate) mod codes {
    /// A sync table names one UI edge label twice.
    pub(crate) const SYNC_CONFLICT: &str = "lifecycle.soundness.sync_conflict";
    /// The composed machine exceeds [`super::MAX_PRODUCT_STATES`].
    pub(crate) const PRODUCT_LIMIT: &str = "lifecycle.soundness.product_limit";
    /// A Quint module name is not a plain identifier.
    pub(crate) const INVALID_MODULE_NAME: &str = "lifecycle.soundness.invalid_module_name";
}

/// Why a composition or its Quint export is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeError {
    /// [`codes::SYNC_CONFLICT`]: the UI edge label named twice.
    SyncConflict(usize),
    /// [`codes::PRODUCT_LIMIT`].
    ProductLimit,
    /// [`codes::INVALID_MODULE_NAME`].
    InvalidModuleName,
}

impl ComposeError {
    /// The registered diagnostic code.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::SyncConflict(_) => codes::SYNC_CONFLICT,
            Self::ProductLimit => codes::PRODUCT_LIMIT,
            Self::InvalidModuleName => codes::INVALID_MODULE_NAME,
        }
    }
}

/// The ending of a product state (coordinator rule, 2026-11-13). Both sides must have ended or
/// be resting; otherwise the pair has no ending, so "the UI shows Done while the backend is still
/// pending or stuck" is never Good. A breached, cancelled or compensated side makes the pair that
/// outcome, worst first; the pair is Good only when the UI is Good, and Resting when the UI rests.
pub(crate) const fn product_ending(ui: Option<Ending>, backend: Option<Ending>) -> Option<Ending> {
    match (ui, backend) {
        (None, _) | (_, None) => None,
        (Some(Ending::BreachedVisible), _) | (_, Some(Ending::BreachedVisible)) => {
            Some(Ending::BreachedVisible)
        }
        (Some(Ending::Cancelled), _) | (_, Some(Ending::Cancelled)) => Some(Ending::Cancelled),
        (Some(Ending::Compensated), _) | (_, Some(Ending::Compensated)) => {
            Some(Ending::Compensated)
        }
        (Some(Ending::Good), Some(Ending::Good | Ending::Resting)) => Some(Ending::Good),
        (Some(Ending::Resting), Some(Ending::Good | Ending::Resting)) => Some(Ending::Resting),
    }
}

/// Largest product the composition builds; UI machines are tens of states, lifecycles under 20.
pub(crate) const MAX_PRODUCT_STATES: usize = 65_536;

/// How one UI edge label moves the backend. A UI label that no entry names fires alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sync {
    /// UI edges labelled `ui` fire only together with a backend edge labelled `backend`
    /// enabled in the current backend state. A backend label named here never fires alone.
    Together {
        /// The UI edge label.
        ui: usize,
        /// The backend edge label (a lifecycle transition).
        backend: usize,
    },
    /// UI edges labelled `ui` fire while the backend stays in its state.
    Alone {
        /// The UI edge label.
        ui: usize,
    },
}

/// Which side's edges one product edge took: indices into `ui.edges` and `backend.edges`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Origin {
    /// The UI edge, unless the backend moved alone.
    pub(crate) ui: Option<usize>,
    /// The backend edge, unless the UI moved alone.
    pub(crate) backend: Option<usize>,
}

/// The reachable product of a UI net and a backend net.
#[derive(Debug, Clone)]
pub(crate) struct Product {
    /// The product as a net, read through [`Product::net`]. Each edge's `label` is its own index
    /// into [`Product::origins`]. Endings follow [`product_ending`]; a caller can only remove one
    /// ([`Product::narrow_endings`]), never add or upgrade one.
    net: Net,
    /// The (UI state, backend state) pair of each product state.
    pub(crate) pairs: Vec<(usize, usize)>,
    /// The side edges of each product edge.
    pub(crate) origins: Vec<Origin>,
    index: BTreeMap<(usize, usize), usize>,
}

impl Product {
    /// The product as a net, for `analyse`, `witness` and `input_free_cycles`.
    pub(crate) const fn net(&self) -> &Net {
        &self.net
    }

    /// Remove the ending of every product state for which `keep(state, (ui, backend), ending)`
    /// is false. Endings can only narrow: no state gains or changes an ending.
    pub(crate) fn narrow_endings(&mut self, keep: impl Fn(usize, (usize, usize), Ending) -> bool) {
        for (state, slot) in self.net.endings.iter_mut().enumerate() {
            if let (Some(ending), Some(pair)) = (*slot, self.pairs.get(state))
                && !keep(state, *pair, ending)
            {
                *slot = None;
            }
        }
    }

    /// The product state of a pair, when that pair is reachable.
    pub(crate) fn state(&self, ui: usize, backend: usize) -> Option<usize> {
        self.index.get(&(ui, backend)).copied()
    }

    /// LIFE-SOUND coverage for #1236's `uncovered_transition`: the labels of the backend edges
    /// some reachable product state takes.
    pub(crate) fn backend_labels(&self, backend: &Net) -> BTreeSet<usize> {
        self.origins
            .iter()
            .filter_map(|origin| origin.backend)
            .filter_map(|edge| backend.edges.get(edge))
            .map(|edge| edge.label)
            .collect()
    }

    /// Every reachable product state whose pair breaks the UI's backend assumption
    /// (`assumes(ui_state, backend_state)` is false), with a shortest witness run (product
    /// edge indices from an initial state).
    pub(crate) fn assumption_violations(
        &self,
        assumes: impl Fn(usize, usize) -> bool,
    ) -> Vec<(usize, Vec<usize>)> {
        self.pairs
            .iter()
            .enumerate()
            .filter(|(_, (ui, backend))| !assumes(*ui, *backend))
            .filter_map(|(state, _)| super::witness(&self.net, state).map(|run| (state, run)))
            .collect()
    }
}

fn combine(first: Modality, second: Modality) -> Modality {
    if first == Modality::Must && second == Modality::Must {
        Modality::Must
    } else {
        Modality::May
    }
}

/// The reachable product of `ui` and `backend` under `sync`, from every pair of initial states.
///
/// # Errors
/// The product exceeds [`MAX_PRODUCT_STATES`], or a sync entry names one UI label twice.
#[allow(
    clippy::too_many_lines,
    reason = "one breadth-first product construction"
)]
pub(crate) fn compose(ui: &Net, backend: &Net, sync: &[Sync]) -> Result<Product, ComposeError> {
    let mut together: BTreeMap<usize, usize> = BTreeMap::new();
    let mut alone: BTreeSet<usize> = BTreeSet::new();
    for entry in sync {
        let fresh = match *entry {
            Sync::Together { ui, backend } => {
                !alone.contains(&ui) && together.insert(ui, backend).is_none()
            }
            Sync::Alone { ui } => !together.contains_key(&ui) && alone.insert(ui),
        };
        if !fresh {
            let (Sync::Together { ui, .. } | Sync::Alone { ui }) = *entry;
            return Err(ComposeError::SyncConflict(ui));
        }
    }
    let synced: BTreeSet<usize> = together.values().copied().collect();
    let mut product = Product {
        net: Net::default(),
        pairs: Vec::new(),
        origins: Vec::new(),
        index: BTreeMap::new(),
    };
    let intern = |product: &mut Product, pair: (usize, usize)| -> Result<usize, ComposeError> {
        if let Some(state) = product.index.get(&pair) {
            return Ok(*state);
        }
        if product.pairs.len() >= MAX_PRODUCT_STATES {
            return Err(ComposeError::ProductLimit);
        }
        let state = product.pairs.len();
        product.pairs.push(pair);
        product.net.endings.push(product_ending(
            ui.endings.get(pair.0).copied().flatten(),
            backend.endings.get(pair.1).copied().flatten(),
        ));
        product.index.insert(pair, state);
        Ok(state)
    };
    for &u in &ui.initial {
        for &b in &backend.initial {
            let state = intern(&mut product, (u, b))?;
            product.net.initial.push(state);
        }
    }
    let mut next = 0;
    while next < product.pairs.len() {
        let state = next;
        next = next.saturating_add(1);
        let Some(&(u, b)) = product.pairs.get(state) else {
            break;
        };
        let mut moves: Vec<((usize, usize), Origin, Modality, bool)> = Vec::new();
        for (index, edge) in ui.edges.iter().enumerate().filter(|(_, e)| e.from == u) {
            if let Some(label) = together.get(&edge.label) {
                for (other, step) in backend.edges.iter().enumerate() {
                    if step.from == b && step.label == *label {
                        moves.push((
                            (edge.to, step.to),
                            Origin {
                                ui: Some(index),
                                backend: Some(other),
                            },
                            combine(edge.modality(), step.modality()),
                            edge.input || step.input,
                        ));
                    }
                }
            } else {
                moves.push((
                    (edge.to, b),
                    Origin {
                        ui: Some(index),
                        backend: None,
                    },
                    edge.modality(),
                    edge.input,
                ));
            }
        }
        for (other, step) in backend.edges.iter().enumerate() {
            if step.from == b && !synced.contains(&step.label) {
                moves.push((
                    (u, step.to),
                    Origin {
                        ui: None,
                        backend: Some(other),
                    },
                    step.modality(),
                    step.input,
                ));
            }
        }
        for (pair, origin, modality, input) in moves {
            let to = intern(&mut product, pair)?;
            let label = product.origins.len();
            product.origins.push(origin);
            product.net.edges.push(NetEdge {
                from: state,
                to,
                label,
                input,
                modality,
            });
        }
    }
    product.net.states = product.pairs.len();
    Ok(product)
}

/// The net as a Quint module for the obligations path (PROOF-QUINT-1 style syntax emission):
/// `s` is the state index, `step` is any declared edge (may and must alike, so it
/// over-approximates), and `ending` names the ending states. Liveness and fairness questions
/// (leads-to under fairness) are asked of this module; option to complete stays [`super::analyse`].
///
/// # Errors
/// `module` is not a plain identifier.
pub(crate) fn quint(net: &Net, module: &str) -> Result<String, ComposeError> {
    let mut chars = module.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ComposeError::InvalidModuleName);
    }
    let set = |states: Vec<usize>| -> String {
        let items: Vec<String> = states.iter().map(ToString::to_string).collect();
        format!("Set({})", items.join(", "))
    };
    let initial = set(net.initial.clone());
    let endings = set(net
        .endings
        .iter()
        .enumerate()
        .filter(|(_, ending)| ending.is_some())
        .map(|(state, _)| state)
        .collect());
    let steps: Vec<String> = net
        .edges
        .iter()
        .map(|edge| format!("    all {{ s == {}, s' = {} }},", edge.from, edge.to))
        .collect();
    let step = if steps.is_empty() {
        "  action step = s' = s".to_owned()
    } else {
        format!("  action step = any {{\n{}\n  }}", steps.join("\n"))
    };
    Ok(format!(
        "module {module} {{\n  var s: int\n  action init = {{\n    nondet i = {initial}.oneOf()\n    s' = i\n  }}\n{step}\n  val ending = {endings}.contains(s)\n  val inRange = s >= 0 and s < {}\n}}\n",
        net.states
    ))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "fixed test nets")]
mod tests {
    use super::super::{Completion, Ending, analyse, witness};
    use super::*;

    /// UI: 0 idle --submit--> 1 pending --accepted(sync)--> 2 done (terminal); 1 --refused--> 0.
    fn page() -> Net {
        Net {
            states: 3,
            initial: vec![0],
            endings: vec![Some(Ending::Resting), None, Some(Ending::Good)],
            edges: vec![
                NetEdge::unguarded(0, 1, 10, true),
                NetEdge::unguarded(1, 2, 11, true),
                NetEdge::unguarded(1, 0, 12, true),
            ],
        }
    }

    /// Backend: 0 draft --post(20)--> 1 posted --pay(21, other actor)--> 2 paid.
    fn chart() -> Net {
        Net {
            states: 3,
            initial: vec![0],
            endings: vec![None, None, Some(Ending::Good)],
            edges: vec![
                NetEdge::unguarded(0, 1, 20, true),
                NetEdge::guarded(1, 2, 21, true),
            ],
        }
    }

    fn sync() -> Vec<Sync> {
        vec![
            Sync::Together {
                ui: 11,
                backend: 20,
            },
            Sync::Alone { ui: 12 },
        ]
    }

    #[test]
    fn together_edges_move_both_sides_and_others_interleave() {
        let product = compose(&page(), &chart(), &sync()).expect("small");
        let done = product.state(2, 1).expect("accepted posts the draft");
        assert!(product.state(2, 0).is_none(), "accepted never fires alone");
        // pay interleaves, and it is guarded on the backend, so the product edge is may.
        let paid = product.state(2, 2).expect("another actor pays");
        let into_paid = product
            .net()
            .edges
            .iter()
            .find(|edge| edge.from == done && edge.to == paid)
            .expect("pay");
        assert_eq!(into_paid.modality(), Modality::May);
        let run = witness(product.net(), paid).expect("reachable");
        let sides: Vec<Origin> = run.iter().map(|edge| product.origins[*edge]).collect();
        assert_eq!(sides.len(), 3);
        assert_eq!(sides[1].ui.zip(sides[1].backend), Some((1, 0)));
        assert_eq!(product.backend_labels(&chart()), BTreeSet::from([20, 21]));
        // Done while the draft is only posted is no ending: completing needs the guarded pay.
        let verdict = analyse(product.net());
        assert_eq!(product.net().endings[done], None);
        assert_eq!(verdict.states[done].completion, Completion::Unknown);
        assert_eq!(product.net().endings[paid], Some(Ending::Good));
    }

    /// Planted: the page reaches Done on `accepted` while the backend sits in `posted`, which is
    /// no ending and has no way on. The product reports it stuck instead of counting it Good.
    #[test]
    fn a_ui_done_over_a_backend_that_has_not_ended_is_reported() {
        let mut stuck = chart();
        stuck.edges.truncate(1);
        let product = compose(&page(), &stuck, &sync()).expect("small");
        let done = product.state(2, 1).expect("accepted posts the draft");
        let verdict = analyse(product.net());
        assert!(verdict.states[done].stuck(), "{:?}", verdict.states[done]);
        assert_eq!(verdict.states[done].completion, Completion::None);
        let run = witness(product.net(), done).expect("a shortest run to the false Done");
        assert_eq!(run.len(), 2);
    }

    #[test]
    fn product_endings_need_both_sides_ended_and_callers_only_narrow() {
        use Ending::{BreachedVisible, Cancelled, Compensated, Good, Resting};
        assert_eq!(product_ending(Some(Good), None), None);
        assert_eq!(product_ending(None, Some(Good)), None);
        assert_eq!(product_ending(Some(Good), Some(Resting)), Some(Good));
        assert_eq!(product_ending(Some(Resting), Some(Good)), Some(Resting));
        assert_eq!(product_ending(Some(Good), Some(Cancelled)), Some(Cancelled));
        assert_eq!(
            product_ending(Some(Compensated), Some(Good)),
            Some(Compensated)
        );
        assert_eq!(
            product_ending(Some(Cancelled), Some(BreachedVisible)),
            Some(BreachedVisible)
        );
        let mut product = compose(&page(), &chart(), &sync()).expect("small");
        let paid = product.state(2, 2).expect("paid");
        let before = product.net().endings.clone();
        product.narrow_endings(|state, _, _| state != paid);
        assert_eq!(product.net().endings[paid], None);
        for (state, ending) in product.net().endings.iter().enumerate() {
            assert!(
                ending.is_none() || before[state] == *ending,
                "narrowing never adds"
            );
        }
    }

    #[test]
    fn a_ui_state_that_assumes_a_backend_state_reports_every_reachable_breach() {
        let product = compose(&page(), &chart(), &sync()).expect("small");
        // The pending page assumes the draft is still a draft.
        let breaches = product.assumption_violations(|ui, backend| ui != 1 || backend == 0);
        assert!(breaches.is_empty(), "pending only sees drafts here");
        // The done page assumes payment, which only another actor may make.
        let breaches = product.assumption_violations(|ui, backend| ui != 2 || backend == 2);
        let states: Vec<(usize, usize)> = breaches.iter().map(|(s, _)| product.pairs[*s]).collect();
        assert_eq!(states, [(2, 1)]);
        assert_eq!(breaches[0].1.len(), 2);
    }

    #[test]
    fn a_sync_table_names_each_ui_label_once_and_products_are_bounded() {
        let twice = [
            Sync::Alone { ui: 12 },
            Sync::Together {
                ui: 12,
                backend: 20,
            },
        ];
        let refused = compose(&page(), &chart(), &twice).expect_err("label 12 twice");
        assert_eq!(refused, ComposeError::SyncConflict(12));
        assert_eq!(refused.code(), "lifecycle.soundness.sync_conflict");
        let module = quint(
            compose(&page(), &chart(), &sync()).expect("small").net(),
            "page_post",
        )
        .expect("identifier");
        assert!(module.starts_with("module page_post {"));
        assert!(module.contains("action step = any {"));
        assert_eq!(quint(&page(), "1bad"), Err(ComposeError::InvalidModuleName));
    }
}
