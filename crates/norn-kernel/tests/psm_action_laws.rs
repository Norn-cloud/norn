//! Decider laws for the real action transition (T2-KER-3, #2536):
//! `norn_kernel::action::VersionedAction` and `decide_authorized` (#2825).
//!
//! Unlike `psm_action_transition.rs` (model refinement from the empty
//! slice), these properties start from **arbitrary authority slices** —
//! owner-version maps including `u64::MAX - 1` / `u64::MAX`, recorded and
//! legacy (empty effect digest) bindings, foreign transition ids, bound
//! idempotency keys — and resolve each request's preconditions against the
//! slice current at that turn (current, ahead, behind, zero, `u64::MAX`), so
//! commits, replays, CAS conflicts, and overflow edges are all frequent.
//!
//! | law | property | mutant proving it can fail |
//! | --- | --- | --- |
//! | step, replay, purity, rejection | the model-free `norn_testkit::laws` hold from any slice, in any world, under any caps | (toy mutants in `psm_decider_laws.rs`) |
//! | observation independence | authority decisions ignore time, entropy, and world | `psm_mutant_action_world_sensitive_idempotency_is_caught` |
//! | idempotency | re-submitting an accepted request replays: no events, no effects, same slice; a changed binding on the same id and a reused key on a new id never commit | `psm_mutant_action_forgetful_evolve_is_caught` |
//! | CAS | a commit happens only when every precondition equals the current owner version | `psm_mutant_action_cas_bypass_is_caught` |
//! | monotonicity / overflow | an accepted turn bumps exactly the mutated owners by exactly one (checked), leaves every other owner unchanged, and never commits for an owner at `u64::MAX` | `psm_mutant_action_wrapping_overflow_is_caught` |
//! | re-authorization | `decide_authorized` never returns a replay or a proceed without a fresh, successful `authorize`, and never replays an unverified receipt | `psm_mutant_authorized_replay_before_authorization_is_caught` |

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt;

use norn_kernel::action::{
    ActionAuthorityState, ActionRejection, ActionTransitionCommitted, AuthorizedActionRequest,
    AuthorizedActionShell, AuthorizedActionState, AuthorizedBinding, AuthorizedOutcome,
    AuthorizedRejection, OwnerExpectation, ResultingVersion, TransitionBinding, VersionedAction,
    VersionedActionRequest, VersionedOutcome, decide_authorized, decide_versioned,
    evolve_versioned,
};
use norn_kernel::{
    CommitId, Decider, Decision, DecisionOf, Entropy, EnvelopeLimits, LogicalTime, Observations,
    World, WorldId, step,
};
use norn_testkit::laws::{decide_is_pure, rejection_is_transparent, replay_is_fold, step_law};
use norn_testkit::proptest::prelude::*;
use norn_testkit::proptest::test_runner::TestError;
use norn_testkit::{Settings, Turn, check_property, run_property, strategy};

type Owner = u8;
type Slice = ActionAuthorityState<Owner>;
type Request = VersionedActionRequest<Owner>;
type Committed = ActionTransitionCommitted<Owner>;

const OWNERS: u8 = 4;

// ------------------------------------------------------------- generators

fn version() -> BoxedStrategy<u64> {
    prop_oneof![
        6 => 0_u64..3,
        2 => Just(u64::MAX),
        1 => Just(u64::MAX - 1),
        1 => any::<u64>(),
    ]
    .boxed()
}

fn transition_id() -> BoxedStrategy<String> {
    (0_u8..6).prop_map(|n| format!("t{n}")).boxed()
}

fn idempotency_key() -> BoxedStrategy<String> {
    (0_u8..5).prop_map(|n| format!("k{n}")).boxed()
}

fn recorded_binding() -> BoxedStrategy<TransitionBinding> {
    (
        0_u8..2,
        prop_oneof![9 => (0_u8..2).prop_map(|n| format!("fx{n}")), 1 => Just(String::new())],
    )
        .prop_map(|(input, effect_sha256)| TransitionBinding {
            input_sha256: format!("in{input}"),
            effect_sha256,
        })
        .boxed()
}

/// An arbitrary authority slice, as a shell may load it from storage.
fn slice() -> BoxedStrategy<Slice> {
    (
        prop::collection::btree_map(0..OWNERS, version(), 0..=usize::from(OWNERS)),
        prop::collection::btree_map(transition_id(), recorded_binding(), 0..3),
        prop::collection::btree_set(transition_id(), 0..2),
        prop::collection::btree_set(idempotency_key(), 0..3),
    )
        .prop_map(
            |(owner_versions, transitions, foreign_transitions, idempotency_keys)| Slice {
                owner_versions,
                transitions,
                foreign_transitions,
                idempotency_keys,
            },
        )
        .boxed()
}

/// A precondition relative to the owner's version at the time of the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Current,
    Ahead,
    Behind,
    Zero,
    Max,
}

impl Expect {
    const fn resolve(self, current: u64) -> u64 {
        match self {
            Self::Current => current,
            Self::Ahead => current.wrapping_add(1),
            Self::Behind => current.wrapping_sub(1),
            Self::Zero => 0,
            Self::Max => u64::MAX,
        }
    }
}

fn expect() -> BoxedStrategy<Expect> {
    prop_oneof![
        8 => Just(Expect::Current),
        1 => Just(Expect::Ahead),
        1 => Just(Expect::Behind),
        1 => Just(Expect::Zero),
        1 => Just(Expect::Max),
    ]
    .boxed()
}

/// A request resolved against the slice current at its turn.
#[derive(Debug, Clone)]
struct Plan {
    transition_id: String,
    binding: TransitionBinding,
    mutation_owners: BTreeSet<Owner>,
    mutated: Expect,
    /// Drop the first mutated owner's precondition.
    drop_one: bool,
    guards: Vec<(Owner, Expect)>,
    idempotency_key: String,
    observations: Observations,
    limits: EnvelopeLimits,
}

fn plan() -> BoxedStrategy<Plan> {
    (
        (
            prop_oneof![19 => transition_id(), 1 => Just(" ".to_string())],
            0_u8..2,
            prop_oneof![9 => (0_u8..2).prop_map(|n| format!("fx{n}")), 1 => Just(String::new())],
        ),
        prop::collection::btree_set(0..OWNERS, 0..3),
        expect(),
        prop::bool::weighted(0.05),
        prop::collection::vec((0..OWNERS, expect()), 0..2),
        idempotency_key(),
        strategy::observations(),
        strategy::limits(),
    )
        .prop_map(
            |(
                (transition_id, input, effect_sha256),
                mutation_owners,
                mutated,
                drop_one,
                guards,
                idempotency_key,
                observations,
                limits,
            )| Plan {
                transition_id,
                binding: TransitionBinding {
                    input_sha256: format!("in{input}"),
                    effect_sha256,
                },
                mutation_owners,
                mutated,
                drop_one,
                guards,
                idempotency_key,
                observations,
                limits,
            },
        )
        .boxed()
}

fn plans() -> BoxedStrategy<Vec<Plan>> {
    prop::collection::vec(plan(), 0..16).boxed()
}

fn resolve(state: &Slice, plan: &Plan) -> Turn<Request> {
    let mut preconditions: Vec<_> = plan
        .mutation_owners
        .iter()
        .skip(usize::from(plan.drop_one))
        .map(|owner| OwnerExpectation {
            owner: *owner,
            expected_version: plan.mutated.resolve(state.owner_version(owner)),
        })
        .collect();
    preconditions.extend(plan.guards.iter().map(|(owner, expect)| OwnerExpectation {
        owner: *owner,
        expected_version: expect.resolve(state.owner_version(owner)),
    }));
    Turn {
        input: Request {
            transition_id: plan.transition_id.clone(),
            binding: plan.binding.clone(),
            preconditions,
            mutation_owners: plan.mutation_owners.clone(),
            idempotency_key: plan.idempotency_key.clone(),
        },
        observations: plan.observations.clone(),
        limits: plan.limits,
    }
}

/// Resolves every plan against the slice the real decider reaches.
fn concretize(initial: &Slice, plans: &[Plan]) -> Vec<Turn<Request>> {
    let decider = VersionedAction::<Owner>::new();
    let mut state = initial.clone();
    plans
        .iter()
        .map(|plan| {
            let turn = resolve(&state, plan);
            if let Ok(accepted) = step(
                &decider,
                &state,
                &turn.input,
                &turn.observations,
                &turn.limits,
            ) {
                state = accepted.state;
            }
            turn
        })
        .collect()
}

// ----------------------------------------------------- action-level laws

/// The action decider's types; mutants below implement it too.
trait ActionDecider:
    Decider<
        State = Slice,
        Input = Request,
        Event = Committed,
        Command = Infallible,
        Rejection = ActionRejection,
    >
{
}

impl<D> ActionDecider for D where
    D: Decider<
            State = Slice,
            Input = Request,
            Event = Committed,
            Command = Infallible,
            Rejection = ActionRejection,
        >
{
}

fn alternative_worlds(observations: &Observations) -> [Observations; 3] {
    [
        Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32])),
        Observations {
            now: LogicalTime::from_millis(u64::MAX),
            entropy: Entropy::from_bytes([0xff; 32]),
            world: World::Fork {
                id: WorldId("law-fork".into()),
            },
        },
        Observations {
            world: World::Historical {
                commit: CommitId("law-commit".into()),
            },
            ..observations.clone()
        },
    ]
}

fn replay_turn(request: &Request) -> Turn<Request> {
    Turn {
        input: request.clone(),
        observations: Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32])),
        limits: EnvelopeLimits::V1,
    }
}

/// Laws on `decide` alone: observation independence, CAS, overflow, and the
/// exact resulting versions of a commit.
fn decision_laws<D: ActionDecider>(
    decider: &D,
    before: &Slice,
    turn: &Turn<Request>,
) -> Result<(), TestCaseError> {
    let request = &turn.input;
    let verdict = decider.decide(before, request, &turn.observations);

    // Observation independence: time, entropy, and world never change an
    // authority decision.
    for alternative in alternative_worlds(&turn.observations) {
        prop_assert_eq!(
            &decider.decide(before, request, &alternative),
            &verdict,
            "law violated (observation independence): decide differs under {:?}\nslice: {:?}\nrequest: {:?}",
            alternative,
            before,
            request
        );
    }

    let committed: Option<&Committed> = verdict.as_ref().ok().and_then(|d| d.events.first());
    if let Ok(decision) = &verdict {
        prop_assert!(decision.events.len() <= 1 && decision.effects.is_empty());
    }

    // CAS: a commit happens only when every precondition matches.
    if let Some(event) = committed {
        for precondition in &request.preconditions {
            prop_assert_eq!(
                precondition.expected_version,
                before.owner_version(&precondition.owner),
                "law violated (CAS): committed with a stale precondition on owner {}\nslice: {:?}\nrequest: {:?}\nevent: {:?}",
                precondition.owner,
                before,
                request,
                event
            );
        }
        // Overflow: no commit for an owner already at u64::MAX.
        for owner in &request.mutation_owners {
            prop_assert!(
                before.owner_version(owner) < u64::MAX,
                "law violated (overflow): committed a bump of owner {} at u64::MAX\nevent: {:?}",
                owner,
                event
            );
        }
        let bumped: Vec<_> = request
            .mutation_owners
            .iter()
            .map(|owner| ResultingVersion {
                owner: *owner,
                version: before.owner_version(owner).saturating_add(1),
            })
            .collect();
        prop_assert_eq!(
            &event.resulting_versions,
            &bumped,
            "law violated (monotonicity): resulting versions are not current + 1 for exactly the mutated owners"
        );
    }
    Ok(())
}

/// Laws on the accepted transition: monotonicity and idempotency. Returns the
/// slice after the turn.
fn transition_laws<D: ActionDecider>(
    decider: &D,
    before: &Slice,
    turn: &Turn<Request>,
) -> Result<Slice, TestCaseError> {
    let request = &turn.input;
    let result = step(decider, before, request, &turn.observations, &turn.limits);
    let Ok(accepted) = result else {
        return Ok(before.clone());
    };
    let after = accepted.state;

    // Monotonicity: exactly the committed owners move, each by exactly one.
    let owners: BTreeSet<_> = before
        .owner_versions
        .keys()
        .chain(after.owner_versions.keys())
        .chain(&request.mutation_owners)
        .collect();
    for owner in owners {
        let (old, new) = (before.owner_version(owner), after.owner_version(owner));
        let expected = if accepted.events.is_empty() || !request.mutation_owners.contains(owner) {
            Some(old)
        } else {
            old.checked_add(1)
        };
        prop_assert_eq!(
            Some(new),
            expected,
            "law violated (monotonicity): owner {} moved {} → {}\nrequest: {:?}",
            owner,
            old,
            new,
            request
        );
    }

    if accepted.events.is_empty() {
        // An accepted turn with no events is an exact replay.
        prop_assert_eq!(
            &after,
            before,
            "law violated (idempotency): a replay changed the slice"
        );
        prop_assert!(
            before
                .transitions
                .get(&request.transition_id)
                .is_some_and(|recorded| recorded.admits_replay_of(&request.binding)),
            "law violated (idempotency): an event-free acceptance that is not an exact replay\nrequest: {:?}",
            request
        );
        return Ok(after);
    }

    // Idempotency: the same request again is a pure replay...
    let again = replay_turn(request);
    let retried = step(
        decider,
        &after,
        &again.input,
        &again.observations,
        &again.limits,
    );
    prop_assert!(
        retried.as_ref().is_ok_and(|retry| retry.events.is_empty()
            && retry.effects.is_empty()
            && retry.state == after),
        "law violated (idempotency): re-submitting an accepted request did not replay\nrequest: {:?}\nretry: {:?}",
        request,
        retried
    );
    // ...a different binding on the same transition id never commits...
    let mut rebound = request.clone();
    rebound.binding.input_sha256.push_str("-changed");
    prop_assert!(
        decider
            .decide(&after, &rebound, &again.observations)
            .is_err(),
        "law violated (idempotency): a changed binding on a committed transition id was not refused"
    );
    // ...and the bound idempotency key never commits under a new id.
    let mut fresh_id = request.clone();
    fresh_id.transition_id.push_str("-fresh");
    prop_assert!(
        !decider
            .decide(&after, &fresh_id, &again.observations)
            .is_ok_and(|decision| !decision.events.is_empty()),
        "law violated (idempotency): a bound idempotency key committed again under a new transition id"
    );
    Ok(after)
}

/// Resolves and checks every plan in turn against `decider`'s own slice.
fn run_action_laws<D: ActionDecider>(
    decider: &D,
    initial: Slice,
    plans: &[Plan],
) -> Result<(), TestCaseError> {
    let mut state = initial;
    for plan in plans {
        let turn = resolve(&state, plan);
        decision_laws(decider, &state, &turn)?;
        state = transition_laws(decider, &state, &turn)?;
    }
    Ok(())
}

fn settings() -> Settings {
    Settings::new(file!())
}

fn mutant_settings() -> Settings {
    Settings::new(file!()).without_persistence()
}

#[track_caller]
fn caught<T: fmt::Debug>(result: Result<(), TestError<T>>) -> String {
    match result {
        Err(TestError::Fail(reason, minimal)) => {
            format!("{}\nminimal: {minimal:?}", reason.message())
        }
        Err(TestError::Abort(reason)) => unreachable!("mutant run aborted: {reason}"),
        Ok(()) => unreachable!("the mutant was not caught"),
    }
}

// -------------------------------------------------------- the laws, green

#[test]
fn psm_versioned_action_obeys_model_free_laws_from_any_slice() {
    check_property(&settings(), &(slice(), plans()), |(initial, plans)| {
        let turns = concretize(&initial, &plans);
        let decider = VersionedAction::<Owner>::new();
        let trace = step_law(
            step::<VersionedAction<Owner>>,
            &decider,
            initial.clone(),
            &turns,
        )?;
        replay_is_fold(&decider, &trace)?;
        decide_is_pure(&VersionedAction::new, &initial, &turns)?;
        rejection_is_transparent(
            step::<VersionedAction<Owner>>,
            &VersionedAction::new,
            &initial,
            &turns,
        )
    });
}

#[test]
fn psm_versioned_action_obeys_action_laws_from_any_slice() {
    check_property(
        &settings().cases(256),
        &(slice(), plans()),
        |(initial, plans)| run_action_laws(&VersionedAction::<Owner>::new(), initial, &plans),
    );
}

thread_local! {
    static SEEN: RefCell<BTreeSet<&'static str>> = const { RefCell::new(BTreeSet::new()) };
}

const fn verdict_label(verdict: &Result<VersionedOutcome<Owner>, ActionRejection>) -> &'static str {
    match verdict {
        Ok(VersionedOutcome::Replay) => "replay",
        Ok(VersionedOutcome::Commit(_)) => "commit",
        Err(ActionRejection::InvalidTransitionIdentity) => "invalid_identity",
        Err(ActionRejection::TransitionUsedByOtherProtocol { .. }) => "other_protocol",
        Err(ActionRejection::TransitionBindingMismatch { .. }) => "binding_mismatch",
        Err(ActionRejection::DuplicatePrecondition) => "duplicate_precondition",
        Err(ActionRejection::MissingPrecondition) => "missing_precondition",
        Err(ActionRejection::OwnerVersionConflict { .. }) => "version_conflict",
        Err(ActionRejection::IdempotencyWithoutReceipt) => "idempotency",
        Err(ActionRejection::OwnerVersionOverflow) => "overflow",
    }
}

/// The action laws are not vacuous: the generators reach every verdict of
/// `decide_versioned` along driven sequences.
#[test]
fn psm_versioned_action_generators_reach_every_verdict() {
    SEEN.with(|seen| seen.borrow_mut().clear());
    check_property(
        &settings().cases(256),
        &(slice(), plans()),
        |(initial, plans)| {
            let decider = VersionedAction::<Owner>::new();
            let mut state = initial;
            for plan in &plans {
                let turn = resolve(&state, plan);
                let label = verdict_label(&decide_versioned(&state, &turn.input));
                SEEN.with(|seen| seen.borrow_mut().insert(label));
                if let Ok(accepted) = step(
                    &decider,
                    &state,
                    &turn.input,
                    &turn.observations,
                    &turn.limits,
                ) {
                    state = accepted.state;
                }
            }
            Ok(())
        },
    );
    let seen = SEEN.with(|seen| seen.borrow().clone());
    for label in [
        "replay",
        "commit",
        "invalid_identity",
        "other_protocol",
        "binding_mismatch",
        "duplicate_precondition",
        "missing_precondition",
        "version_conflict",
        "idempotency",
        "overflow",
    ] {
        assert!(
            seen.contains(label),
            "verdict {label} never generated: {seen:?}"
        );
    }
}

// ------------------------------------------------ action mutants, caught

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionMutant {
    /// `evolve` forgets to record the transition binding.
    ForgetfulEvolve,
    /// `decide` rewrites every precondition to the current version.
    CasBypass,
    /// On overflow, `decide` commits wrapped (`u64::MAX + 1 = 0`) versions.
    WrappingOverflow,
    /// In a fork, `decide` ignores bound idempotency keys.
    WorldSensitiveIdempotency,
}

/// `VersionedAction` with one deliberate bug.
struct Mutated(ActionMutant);

impl Decider for Mutated {
    type State = Slice;
    type Input = Request;
    type Event = Committed;
    type Command = Infallible;
    type Rejection = ActionRejection;

    fn initial_state(&self) -> Slice {
        Slice::default()
    }

    fn decide(
        &self,
        state: &Slice,
        input: &Request,
        observations: &Observations,
    ) -> Result<DecisionOf<Self>, ActionRejection> {
        let mut state = state.clone();
        let mut input = input.clone();
        match self.0 {
            ActionMutant::CasBypass => {
                for precondition in &mut input.preconditions {
                    precondition.expected_version = state.owner_version(&precondition.owner);
                }
            }
            ActionMutant::WorldSensitiveIdempotency
                if matches!(observations.world, World::Fork { .. }) =>
            {
                state.idempotency_keys.clear();
            }
            ActionMutant::ForgetfulEvolve
            | ActionMutant::WrappingOverflow
            | ActionMutant::WorldSensitiveIdempotency => {}
        }
        match decide_versioned(&state, &input) {
            Ok(VersionedOutcome::Replay) => Ok(Decision::none()),
            Ok(VersionedOutcome::Commit(event)) => Ok(Decision::events(vec![event])),
            Err(ActionRejection::OwnerVersionOverflow)
                if self.0 == ActionMutant::WrappingOverflow =>
            {
                Ok(Decision::events(vec![Committed {
                    transition_id: input.transition_id.clone(),
                    binding: input.binding.clone(),
                    idempotency_key: input.idempotency_key.clone(),
                    resulting_versions: input
                        .mutation_owners
                        .iter()
                        .map(|owner| ResultingVersion {
                            owner: *owner,
                            version: state.owner_version(owner).wrapping_add(1),
                        })
                        .collect(),
                }]))
            }
            Err(rejection) => Err(rejection),
        }
    }

    fn evolve(&self, state: &Slice, event: &Committed) -> Slice {
        let mut next = evolve_versioned(state, event);
        if self.0 == ActionMutant::ForgetfulEvolve {
            next.transitions = state.transitions.clone();
        }
        next
    }
}

fn action_mutant(mutant: ActionMutant) -> String {
    caught(run_property(
        &mutant_settings().cases(256),
        &(slice(), plans()),
        |(initial, plans)| run_action_laws(&Mutated(mutant), initial, &plans),
    ))
}

#[test]
fn psm_mutant_action_forgetful_evolve_is_caught() {
    let failure = action_mutant(ActionMutant::ForgetfulEvolve);
    assert!(failure.contains("law violated (idempotency)"), "{failure}");
}

#[test]
fn psm_mutant_action_cas_bypass_is_caught() {
    let failure = action_mutant(ActionMutant::CasBypass);
    assert!(failure.contains("law violated (CAS)"), "{failure}");
}

#[test]
fn psm_mutant_action_wrapping_overflow_is_caught() {
    let failure = action_mutant(ActionMutant::WrappingOverflow);
    assert!(failure.contains("law violated (overflow)"), "{failure}");
}

#[test]
fn psm_mutant_action_world_sensitive_idempotency_is_caught() {
    let failure = action_mutant(ActionMutant::WorldSensitiveIdempotency);
    assert!(
        failure.contains("law violated (observation independence)"),
        "{failure}"
    );
}

// ------------------------------------------- authorization stage (#2825)

/// A shell with fixed verdicts that records which checks were asked for.
struct Shell {
    integrity: bool,
    authorization: bool,
    log: Vec<&'static str>,
}

impl AuthorizedActionShell for Shell {
    type Error = &'static str;
    type Authorized = ();

    fn verify_recorded_receipt(&mut self) -> Result<(), &'static str> {
        self.log.push("verify");
        if self.integrity {
            Ok(())
        } else {
            Err("integrity")
        }
    }

    fn authorize(&mut self) -> Result<(), &'static str> {
        self.log.push("authorize");
        if self.authorization {
            Ok(())
        } else {
            Err("denied")
        }
    }
}

type AuthorizedFn = fn(
    &AuthorizedActionState,
    &AuthorizedActionRequest,
    &mut Shell,
) -> Result<AuthorizedOutcome<()>, AuthorizedRejection<&'static str>>;

fn authorized_binding(tag: u8) -> AuthorizedBinding {
    AuthorizedBinding {
        authorization_input_sha256: format!("auth{tag}"),
        transition: TransitionBinding {
            input_sha256: format!("in{tag}"),
            effect_sha256: format!("fx{tag}"),
        },
    }
}

#[derive(Debug, Clone)]
struct AuthorizedCase {
    state: AuthorizedActionState,
    request: AuthorizedActionRequest,
    integrity: bool,
    authorization: bool,
}

fn authorized_case() -> BoxedStrategy<AuthorizedCase> {
    (
        prop::option::of(0_u8..2),
        any::<bool>(),
        any::<bool>(),
        prop::option::of(prop_oneof![0_u64..3, Just(u64::MAX)]),
        0_u8..2,
        prop_oneof![0_u64..3, Just(u64::MAX)],
        prop::bool::weighted(0.8),
        prop::bool::weighted(0.8),
    )
        .prop_map(
            |(existing, authority, versioned, last, tag, time, integrity, authorization)| {
                AuthorizedCase {
                    state: AuthorizedActionState {
                        existing: existing.map(authorized_binding),
                        used_by_authority_protocol: authority,
                        used_by_versioned_protocol: versioned,
                        last_commit_logical_time: last,
                    },
                    request: AuthorizedActionRequest {
                        transition_id: "t".into(),
                        binding: authorized_binding(tag),
                        commit_logical_time: time,
                    },
                    integrity,
                    authorization,
                }
            },
        )
        .boxed()
}

/// Re-authorization law for the authorization stage.
fn authorized_laws(decide: AuthorizedFn, case: &AuthorizedCase) -> Result<(), TestCaseError> {
    let shell = || Shell {
        integrity: case.integrity,
        authorization: case.authorization,
        log: Vec::new(),
    };
    let (mut first, mut second) = (shell(), shell());
    let outcome = decide(&case.state, &case.request, &mut first);
    // Purity: the same slice, request, and shell verdicts give the same
    // outcome after the same shell calls.
    prop_assert_eq!(
        &outcome,
        &decide(&case.state, &case.request, &mut second),
        "law violated (purity): decide_authorized is not deterministic"
    );
    prop_assert_eq!(&first.log, &second.log);

    let rollback = case
        .state
        .last_commit_logical_time
        .is_some_and(|last| case.request.commit_logical_time < last);
    if let Ok(outcome) = &outcome {
        prop_assert!(
            first.log.last() == Some(&"authorize") && case.authorization,
            "law violated (re-authorization): {:?} without a fresh successful authorize (log {:?})",
            outcome,
            first.log
        );
        prop_assert!(
            !rollback,
            "law violated (re-authorization): accepted a logical-time rollback"
        );
        match outcome {
            AuthorizedOutcome::Replay => prop_assert!(
                case.state
                    .existing
                    .as_ref()
                    .is_some_and(|existing| existing.matches(&case.request.binding))
                    && first.log == ["verify", "authorize"]
                    && case.integrity,
                "law violated (re-authorization): replayed an unmatched or unverified receipt (log {:?})",
                first.log
            ),
            AuthorizedOutcome::Proceed(()) => prop_assert!(
                case.state.existing.is_none()
                    && !case.state.used_by_authority_protocol
                    && !case.state.used_by_versioned_protocol,
                "law violated (re-authorization): proceeded on a transition id another protocol owns"
            ),
        }
    }
    Ok(())
}

#[test]
fn psm_authorized_stage_never_skips_reauthorization() {
    check_property(&settings().cases(256), &authorized_case(), |case| {
        authorized_laws(decide_authorized::<Shell>, &case)
    });
}

/// Mutant: an exact retry of a recorded receipt replays before (and without)
/// re-authorization — idempotency granting continuing access.
fn replay_before_authorization(
    state: &AuthorizedActionState,
    request: &AuthorizedActionRequest,
    shell: &mut Shell,
) -> Result<AuthorizedOutcome<()>, AuthorizedRejection<&'static str>> {
    if state
        .existing
        .as_ref()
        .is_some_and(|existing| existing.matches(&request.binding))
    {
        shell
            .verify_recorded_receipt()
            .map_err(AuthorizedRejection::Shell)?;
        return Ok(AuthorizedOutcome::Replay);
    }
    decide_authorized(state, request, shell)
}

#[test]
fn psm_mutant_authorized_replay_before_authorization_is_caught() {
    let failure = caught(run_property(
        &mutant_settings().cases(256),
        &authorized_case(),
        |case| authorized_laws(replay_before_authorization, &case),
    ));
    assert!(
        failure.contains("law violated (re-authorization)"),
        "{failure}"
    );
}

/// Pins the `u64::MAX` edge exactly: a mutated owner at `u64::MAX` is refused
/// with zero events and the slice untouched, while a guard-only owner at
/// `u64::MAX` does not block the commit.
#[test]
fn psm_versioned_action_u64_max_edges() {
    let decider = VersionedAction::<Owner>::new();
    let observations = Observations::main(LogicalTime::EPOCH, Entropy::from_bytes([0; 32]));
    let state = Slice {
        owner_versions: BTreeMap::from([(0, u64::MAX), (1, u64::MAX - 1)]),
        ..Slice::default()
    };
    let request = |mutated: Owner| Request {
        transition_id: "t".into(),
        binding: TransitionBinding {
            input_sha256: "in".into(),
            effect_sha256: "fx".into(),
        },
        preconditions: vec![
            OwnerExpectation {
                owner: 0,
                expected_version: u64::MAX,
            },
            OwnerExpectation {
                owner: 1,
                expected_version: u64::MAX - 1,
            },
        ],
        mutation_owners: BTreeSet::from([mutated]),
        idempotency_key: "k".into(),
    };
    let limits = EnvelopeLimits::V1;
    assert_eq!(
        step(&decider, &state, &request(0), &observations, &limits),
        Err(norn_kernel::StepError::Rejected(
            ActionRejection::OwnerVersionOverflow
        ))
    );
    let accepted = step(&decider, &state, &request(1), &observations, &limits)
        .expect("a guard at u64::MAX does not block a bump of another owner");
    assert_eq!(accepted.state.owner_version(&1), u64::MAX);
    assert_eq!(accepted.state.owner_version(&0), u64::MAX);
    assert!(accepted.effects.is_empty());
    // Now owner 1 is at u64::MAX too: the same bump overflows.
    let mut again = request(1);
    again.transition_id = "t2".into();
    again.idempotency_key = "k2".into();
    again.preconditions[1].expected_version = u64::MAX;
    assert_eq!(
        step(&decider, &accepted.state, &again, &observations, &limits),
        Err(norn_kernel::StepError::Rejected(
            ActionRejection::OwnerVersionOverflow
        ))
    );
}
