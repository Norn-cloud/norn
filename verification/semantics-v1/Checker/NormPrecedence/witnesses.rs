// Pinned counterexamples for the seeded mutants of
// `crates/norn-checker/src/norm_precedence/decision.rs` (#718, lane C-C).
//
// `scripts/verify-norm-precedence-aeneas.sh` refutes each mutant by running
// the mutated core on its witness and requiring a verdict different from
// `expected`. `expected` is the oracle's verdict: for an admission witness, the
// former whole-set query on the grounding; for a resolution witness, whether
// the given resolution is the one the untrusted resolver computes. The
// norn-checker test `witnesses_match_the_oracle` checks both the oracle and the
// unmutated core against `expected`, so a witness cannot drift.
//
// This file has no dependencies and no inner attributes: the gate compiles it
// next to the decision core, and the crate `include!`s it.

/// One norm: grade (0 obligation, 1 permission, 2 prohibition), layer rank
/// (a smaller rank is a higher layer), protection, per-context activity.
pub(crate) type WitnessNorm = (u8, u8, bool, &'static [bool]);

/// One context's claimed resolution.
pub(crate) struct WitnessResolution {
    pub(crate) context: usize,
    pub(crate) survivors: &'static [usize],
    pub(crate) obligations: &'static [usize],
    pub(crate) conflicts: usize,
    pub(crate) defeats: &'static [(usize, usize)],
}

/// A pinned input and the oracle's verdict on it.
pub(crate) struct Witness {
    /// The mutant this input refutes.
    pub(crate) mutant: &'static str,
    pub(crate) norms: &'static [WitnessNorm],
    pub(crate) edges: &'static [(usize, usize)],
    /// The closure certificate handed to the core, row by row. Bogus on the
    /// certificate mutants' witnesses, exact elsewhere.
    pub(crate) reach: &'static [&'static [bool]],
    pub(crate) contexts: usize,
    pub(crate) resolution: Option<WitnessResolution>,
    pub(crate) expected: bool,
}

const ON: &[bool] = &[true];
const T: bool = true;
const F: bool = false;

/// An admitted order: 1 (permission) over 0 (prohibition) over 2 (obligation).
/// Its true resolution has survivors {1, 2}, obligations {2}, defeats {(1, 0)}.
const RESOLVED: &[WitnessNorm] = &[(2, 3, false, ON), (1, 3, false, ON), (0, 3, false, ON)];
const RESOLVED_EDGES: &[(usize, usize)] = &[(1, 0), (0, 2)];
const RESOLVED_REACH: &[&[bool]] = &[&[F, F, T], &[T, F, T], &[F, F, F]];

const fn resolution(
    survivors: &'static [usize],
    obligations: &'static [usize],
    conflicts: usize,
    defeats: &'static [(usize, usize)],
) -> WitnessResolution {
    WitnessResolution {
        context: 0,
        survivors,
        obligations,
        conflicts,
        defeats,
    }
}

pub(crate) const WITNESSES: &[Witness] = &[
    // A company rule declared over a legal one points up a layer.
    Witness {
        mutant: "layer-edge-points-up",
        norms: &[(0, 3, false, ON), (0, 1, false, ON)],
        edges: &[(0, 1)],
        reach: &[&[F, T], &[F, F]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // An edge into a protected law defeats it.
    Witness {
        mutant: "protected-target-allowed",
        norms: &[(0, 3, false, ON), (0, 3, true, ON)],
        edges: &[(0, 1)],
        reach: &[&[F, T], &[F, F]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // A two-cycle of declared priorities.
    Witness {
        mutant: "cycle-admitted",
        norms: &[(0, 3, false, ON), (0, 3, false, ON)],
        edges: &[(0, 1), (1, 0)],
        reach: &[&[T, T], &[T, T]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // A cycle hidden by a non-transitive certificate whose pairs justify each
    // other; it also claims the clashes with 2 are ordered.
    Witness {
        mutant: "closure-transitivity-unchecked",
        norms: &[(0, 3, false, ON), (0, 3, false, ON), (2, 3, false, ON)],
        edges: &[(0, 1), (1, 0)],
        reach: &[&[F, T, T], &[T, F, T], &[F, F, F]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // A certificate claims an undeclared priority that resolves a clash.
    Witness {
        mutant: "closure-justification-unchecked",
        norms: &[(2, 3, false, ON), (0, 3, false, ON)],
        edges: &[],
        reach: &[&[F, T], &[F, F]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // A legal prohibition and a company permission meet; an edge orders them
    // but cannot resolve a cross-layer clash.
    Witness {
        mutant: "cross-layer-clash-admitted",
        norms: &[(2, 1, false, ON), (1, 3, false, ON)],
        edges: &[(0, 1)],
        reach: &[&[F, T], &[F, F]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // Opposing norms meet with no declared priority.
    Witness {
        mutant: "unordered-clash-admitted",
        norms: &[(2, 3, false, ON), (1, 3, false, ON)],
        edges: &[],
        reach: &[&[F, F], &[F, F]],
        contexts: 1,
        resolution: None,
        expected: false,
    },
    // Opposing norms that never apply in the same context do not clash.
    Witness {
        mutant: "co-activity-ignored",
        norms: &[(2, 3, false, &[true, false]), (1, 3, false, &[false, true])],
        edges: &[],
        reach: &[&[F, F], &[F, F]],
        contexts: 2,
        resolution: None,
        expected: true,
    },
    // The true resolution: 2 survives, since the permission over it does not
    // oppose it.
    Witness {
        mutant: "defeat-ignores-opposition",
        norms: RESOLVED,
        edges: RESOLVED_EDGES,
        reach: RESOLVED_REACH,
        contexts: 1,
        resolution: Some(resolution(&[1, 2], &[2], 0, &[(1, 0)])),
        expected: true,
    },
    // A defeated norm listed as a survivor.
    Witness {
        mutant: "survivors-unchecked",
        norms: RESOLVED,
        edges: RESOLVED_EDGES,
        reach: RESOLVED_REACH,
        contexts: 1,
        resolution: Some(resolution(&[0, 1, 2], &[2], 0, &[])),
        expected: false,
    },
    // A surviving permission listed as an obligation.
    Witness {
        mutant: "obligation-grade-unchecked",
        norms: RESOLVED,
        edges: RESOLVED_EDGES,
        reach: RESOLVED_REACH,
        contexts: 1,
        resolution: Some(resolution(&[1, 2], &[1, 2], 0, &[(1, 0)])),
        expected: false,
    },
    // A recorded conflict.
    Witness {
        mutant: "conflicts-ignored",
        norms: RESOLVED,
        edges: RESOLVED_EDGES,
        reach: RESOLVED_REACH,
        contexts: 1,
        resolution: Some(resolution(&[1, 2], &[2], 1, &[(1, 0)])),
        expected: false,
    },
    // A defeat that cites no declared priority.
    Witness {
        mutant: "defeat-priority-unchecked",
        norms: RESOLVED,
        edges: RESOLVED_EDGES,
        reach: RESOLVED_REACH,
        contexts: 1,
        resolution: Some(resolution(&[1, 2], &[2], 0, &[(1, 0), (2, 0)])),
        expected: false,
    },
    // An active norm that neither survives nor is recorded as defeated.
    Witness {
        mutant: "defeats-incomplete",
        norms: RESOLVED,
        edges: RESOLVED_EDGES,
        reach: RESOLVED_REACH,
        contexts: 1,
        resolution: Some(resolution(&[1, 2], &[2], 0, &[])),
        expected: false,
    },
];
