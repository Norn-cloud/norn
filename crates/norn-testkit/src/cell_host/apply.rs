//! Concrete `StateMachineTest` execution and fault/network application for
//! [`CellHostMachine`].

use std::convert::Infallible;

use norn_kernel::{CellHost, CommitError, Consumed, LogicalTime};
use proptest_state_machine::StateMachineTest;

use super::{
    ArmedFault, CellHostMachine, CellOp, CellReference, Dispatching, Exchange, Expect, FaultKind,
    FaultTarget, Faulted, Firing, LawCommit, Reinjecting, ReissueAuth, Route, SimulatedCell,
    law_observed, snapshot,
};
use crate::coverage;

/// Rebuild every listed field-only variant with an uninhabited host error.
/// The generated `match` is exhaustive, so a new [`CommitError`] variant
/// fails to compile until it is listed here.
macro_rules! rehost {
    ($error:expr; $($variant:ident $({ $($field:ident),* })?),* $(,)?) => {
        match $error {
            $(norn_kernel::CommitError::$variant $({ $($field),* })? =>
                norn_kernel::CommitError::$variant $({ $($field),* })?,)*
            norn_kernel::CommitError::Host(error) => return Err(error),
        }
    };
}

/// The refusal without its host error, or the host error itself.
fn without_host<X>(error: CommitError<X>) -> Result<CommitError<Infallible>, X> {
    Ok(rehost!(error;
        IdentityReuse, AtomicBoundary { key }, AtomicDeliveryBoundary { consumed }, ImmutableObject { key },
        LifecycleInitialization { key, refusal },
        Law { key, reason }, IdentityCollision { owner, key, conflicts },
        OwnedRowsProtected { key }, DuplicateKey { key }, NotPending { consumed }, ReceiptNotAccepted { consumed },
        Conflict { key, expected, current }, DuplicateLink { key },
        IdentifierTooLong { field, actual_bytes, max_bytes },
        LinkConflict { key, expected, current }, LinkReadConflict { source_key, link },
        UnsupportedLinkRead { source_key, link }, DuplicateLinkRead { source_key, link },
        LinkReadTooLarge { source_key, link, actual, max }, InvalidLinkRead { source_key, link },
        PredicateReadConflict { object_type }, UnsupportedPredicateRead { object_type },
        InvalidPredicateRead { object_type, reason }, DanglingLink { key, missing },
        LinkTarget { key }, LinkCardinality { key, endpoint }, LinkRequired { parent, link },
        TypeMismatch { key, concrete }, EffectiveValues { key, reason },
        TraversalLimitExceeded { key, reason }, AccountingPosting { key, reason },
        InvariantViolation { key, owner, version, id, message, outcome },
        InvariantBudgetExceeded { key, limit }, InvariantModel { key, interface },
        ObservationAuthorization, EventLimitExceeded { limit, actual }, Overflow,
    ))
}

fn consumed_label(consumed: Option<&Consumed>) -> &'static str {
    match consumed {
        None => "commit.accepted",
        Some(Consumed::Timer { .. }) => "commit.accepted.consumed_timer",
        Some(Consumed::Result { .. }) => "commit.accepted.consumed_result",
        Some(Consumed::Observation { .. }) => "commit.accepted.consumed_observation",
    }
}

/// Disarms the host's fault and checks it fired exactly when the model says
/// the operation runs the faulted write transaction (law 6).
fn fault_fired<H: SimulatedCell>(host: &mut H, fault: ArmedFault) -> bool {
    let fired = host.sim_disarm_fault();
    match fault.firing {
        Firing::Fires => assert!(
            fired,
            "law violated: a fault armed for a write transaction the operation runs did not fire\nfault: {:?}",
            fault.plan
        ),
        Firing::Never => assert!(
            !fired,
            "law violated: a fault fired in a write transaction the operation must not run (a commit is one transaction, a fresh perform two)\nfault: {:?}",
            fault.plan
        ),
        Firing::Either => {}
    }
    let label = if fired {
        match fault.plan.kind {
            FaultKind::Crash => "fault.crash",
            FaultKind::IoError => "fault.io_error",
            FaultKind::CommitFails => "fault.commit_failed",
        }
    } else {
        "fault.unfired"
    };
    coverage::hit_prefixed("cell", label);
    fired
}

fn apply_commit<H: SimulatedCell>(
    host: &mut H,
    commit: &LawCommit,
    expected: &Expect,
    fault: Option<ArmedFault>,
) {
    let before = snapshot(host);
    if let Some(fault) = fault {
        host.sim_arm_fault(fault.plan);
    }
    // A host failure is reported as its message and can never equal the
    // model's expectation.
    let actual = match host.commit(commit.clone()) {
        Ok(receipt) => Ok(Ok(receipt)),
        Err(error) => without_host(error)
            .map(Err)
            .map_err(|error| error.to_string()),
    };
    if fault.is_some_and(|fault| fault_fired(host, fault)) {
        coverage::hit_prefixed("cell", "fault.commit");
        // Refinement against the unchanged model follows.
        assert!(
            actual.is_err(),
            "law violated: a faulted commit did not fail with a host error\ncommit: {commit:?}\noutcome: {actual:?}"
        );
        return;
    }
    assert_eq!(
        actual.clone().map(Expect::Commit),
        Ok(expected.clone()),
        "law violated: commit outcome differs from check-then-apply\ncommit: {commit:?}"
    );
    match actual {
        Ok(Ok(receipt)) => {
            coverage::hit_prefixed("cell", consumed_label(commit.consumes.as_ref()));
            if !receipt.linked.is_empty() {
                coverage::hit_prefixed("cell", "commit.accepted.linked");
            }
            let published = host.publish(&receipt).map_err(|error| error.to_string());
            assert_eq!(published, Ok(()), "publish of an accepted commit failed");
        }
        Ok(Err(refusal)) => {
            coverage::hit_prefixed("cell", refusal.code());
            assert_eq!(
                snapshot(host),
                before,
                "law violated: a refused commit changed the cell\ncommit: {commit:?}"
            );
        }
        Err(_) => {}
    }
}

impl<H: SimulatedCell> StateMachineTest for CellHostMachine<H> {
    type SystemUnderTest = H;
    type Reference = Self;

    fn init_test(_: &CellReference) -> H {
        H::sim_new()
    }

    fn apply(mut host: H, reference: &CellReference, op: CellOp) -> H {
        let expected = reference.expected.clone().unwrap_or(Expect::Nothing);
        match op {
            CellOp::Commit(commit) => {
                authorize_reissue(&mut host, reference.auth_before_commit.as_ref());
                apply_commit(&mut host, &commit, &expected, None);
            }
            CellOp::Perform => apply_perform(&mut host, expected, None),
            CellOp::Observe => {
                let observed = host.observe().map_err(|error| error.to_string());
                if matches!(observed, Ok(Some(_))) {
                    coverage::hit_prefixed("cell", "observe.delivered");
                }
                assert_eq!(
                    observed.map(Expect::Observe),
                    Ok(expected),
                    "law violated: observe delivered the wrong observation"
                );
            }
            CellOp::Clock => {
                let due = host.clock().map_err(|error| error.to_string());
                if matches!(due, Ok(Some(_))) {
                    coverage::hit_prefixed("cell", "clock.due");
                }
                assert_eq!(
                    due.map(Expect::Clock),
                    Ok(expected),
                    "law violated: clock delivered the wrong timer"
                );
            }
            CellOp::Advance(_) => {
                host.sim_advance_to(LogicalTime::from_millis(reference.model.now));
            }
            CellOp::Inject { id, payload } => apply_inject(&mut host, id, payload, &expected),
            CellOp::Reinject(reinjecting) => {
                apply_reinjecting(&mut host, &reinjecting, &reference.chain);
            }
            CellOp::Exchange(exchange) => apply_exchange(
                &mut host,
                &exchange,
                &reference.chain,
                reference.auth_before_prepared.as_ref(),
                reference.auth_before_reissue.as_ref(),
            ),
            CellOp::Dispatch(dispatching) => {
                apply_dispatching(
                    &mut host,
                    &dispatching,
                    &reference.chain,
                    reference.auth_before_prepared.as_ref(),
                    reference.auth_before_reissue.as_ref(),
                );
            }
            CellOp::Deliver(_) | CellOp::Lose(_) => apply_network(&mut host, &op, &expected),
            CellOp::Faulted(faulted) => {
                let Faulted {
                    prepare, target, ..
                } = *faulted;
                if let Some(commit) = prepare {
                    authorize_reissue(&mut host, reference.auth_before_prepared.as_ref());
                    let prepared = reference.prepared.clone().unwrap_or(Expect::Nothing);
                    apply_commit(&mut host, &commit, &prepared, None);
                }
                match target {
                    FaultTarget::Commit(commit) => {
                        apply_commit(&mut host, &commit, &expected, reference.fault);
                    }
                    FaultTarget::Perform => apply_perform(&mut host, expected, reference.fault),
                }
            }
        }
        Self::refines(host, reference)
    }
}

/// One observation arrives: ingested once; a seen id, before or after its
/// consumption, is ignored (law 3; law 7 redelivers them).
fn apply_inject<H: SimulatedCell>(host: &mut H, id: u8, payload: u8, expected: &Expect) {
    host.sim_inject(law_observed(id, payload));
    if let Expect::Inject {
        duplicate: true,
        late,
    } = *expected
    {
        let label = if late {
            "observe.late_ignored"
        } else {
            "observe.duplicate_ignored"
        };
        coverage::hit_prefixed("cell", label);
    }
}

/// A [`Reinjecting`] (law 7): each arrival ingested at most once, the
/// consumption as a commit.
fn apply_reinjecting<H: SimulatedCell>(host: &mut H, reinjecting: &Reinjecting, chain: &[Expect]) {
    let mut expected = chain.iter().cloned();
    let mut next = || expected.next().unwrap_or(Expect::Nothing);
    let (id, payload) = (reinjecting.id, reinjecting.payload);
    apply_inject(host, id, payload, &next());
    apply_inject(host, id, payload, &next());
    apply_commit(host, &reinjecting.consume, &next(), None);
    apply_inject(host, id, payload, &next());
}

fn authorize_reissue<H: SimulatedCell>(host: &mut H, target: Option<&ReissueAuth>) {
    if let Some(auth) = target {
        let authorized = host.sim_authorize_reissue(&auth.command, auth.from, auth.next);
        assert_eq!(
            authorized,
            Ok(()),
            "law violated: durable reissue authorization failed for {:?} {:?} -> {:?}",
            auth.command,
            auth.from,
            auth.next
        );
    }
}

/// An [`Exchange`] (law 7): its commits as commits, each dispatch and
/// delivery as the model predicts.
fn apply_exchange<H: SimulatedCell>(
    host: &mut H,
    exchange: &Exchange,
    chain: &[Expect],
    auth_prepare: Option<&ReissueAuth>,
    auth_reissue: Option<&ReissueAuth>,
) {
    let mut expected = chain.iter().cloned();
    let mut next = || expected.next().unwrap_or(Expect::Nothing);
    if let Some(commit) = &exchange.prepare {
        authorize_reissue(host, auth_prepare);
        apply_commit(host, commit, &next(), None);
    }
    dispatch_once(host, next());
    authorize_reissue(host, auth_reissue);
    apply_commit(host, &exchange.reissue, &next(), None);
    dispatch_once(host, next());
    for step in exchange.tail() {
        let op = step.map_or(CellOp::Lose(0), |delivery| {
            CellOp::Deliver(Box::new(delivery))
        });
        apply_network(host, &op, &next());
    }
}

/// A [`Dispatching`] (law 7): its commits as commits, and each dispatch
/// claims exactly what the model predicts.
fn apply_dispatching<H: SimulatedCell>(
    host: &mut H,
    dispatching: &Dispatching,
    chain: &[Expect],
    auth_prepare: Option<&ReissueAuth>,
    auth_reissue: Option<&ReissueAuth>,
) {
    let mut expected = chain.iter().cloned();
    let mut next = || expected.next().unwrap_or(Expect::Nothing);
    if let Some(commit) = &dispatching.prepare {
        authorize_reissue(host, auth_prepare);
        apply_commit(host, commit, &next(), None);
    }
    dispatch_once(host, next());
    if let Some(commit) = &dispatching.reissue {
        authorize_reissue(host, auth_reissue);
        apply_commit(host, commit, &next(), None);
        dispatch_once(host, next());
    }
}

fn dispatch_once<H: SimulatedCell>(host: &mut H, expected: Expect) {
    let dispatched = host.sim_dispatch();
    let label = if matches!(dispatched, Ok(Some(_))) {
        "network.dispatched"
    } else {
        "network.dispatch_idle"
    };
    coverage::hit_prefixed("cell", label);
    assert_eq!(
        dispatched.map(Expect::Dispatch),
        Ok(expected),
        "law violated: dispatch claims a command other than the fresh head"
    );
}

/// One network transition (law 7): a delivery is recorded exactly when the
/// model records it, and a loss is the network's alone.
fn apply_network<H: SimulatedCell>(host: &mut H, op: &CellOp, expected: &Expect) {
    match op {
        CellOp::Deliver(delivery) => {
            let recorded = host.sim_deliver(&delivery.command, &delivery.input);
            if let Expect::Deliver { stale, delayed, .. } = *expected {
                let outcome = match (&recorded, stale) {
                    (Ok(true), _) => "network.delivered",
                    (_, true) => "result.stale_refused",
                    (_, false) => "network.duplicate_ignored",
                };
                coverage::hit_prefixed("cell", outcome);
                if delayed {
                    coverage::hit_prefixed("cell", "network.delayed");
                }
            }
            match delivery.route {
                Route::InOrder => {}
                Route::OutOfOrder => coverage::hit_prefixed("cell", "network.out_of_order"),
                Route::Duplicate => coverage::hit_prefixed("cell", "network.duplicate"),
            }
            let expected_recorded = matches!(expected, Expect::Deliver { recorded: true, .. });
            assert_eq!(
                recorded,
                Ok(expected_recorded),
                "law violated: a result is recorded only as the current head's first result at its generation\ndelivery: {delivery:?}"
            );
        }
        CellOp::Lose(_) => coverage::hit_prefixed("cell", "network.dropped"),
        CellOp::Dispatch(_)
        | CellOp::Reinject(_)
        | CellOp::Exchange(_)
        | CellOp::Commit(_)
        | CellOp::Perform
        | CellOp::Observe
        | CellOp::Clock
        | CellOp::Advance(_)
        | CellOp::Inject { .. }
        | CellOp::Faulted(_) => {}
    }
}

/// One `perform`, under `fault` if armed: exactly once per generation, and
/// never delivered or executed when the model expects it to fail.
fn apply_perform<H: SimulatedCell>(host: &mut H, expected: Expect, fault: Option<ArmedFault>) {
    let executions = host.sim_executions();
    if let Some(fault) = fault {
        host.sim_arm_fault(fault.plan);
    }
    let delivered = host.perform().map_err(|error| error.to_string());
    let fired = fault.is_some_and(|fault| fault_fired(host, fault));
    let executed = host.sim_executions() != executions;
    if expected == Expect::Failed {
        let label = match fault {
            Some(fault) if fired && fault.plan.transaction == 0 => "fault.perform.claim",
            Some(_) if fired => "fault.perform.result",
            Some(_) | None => "perform.outcome_unknown",
        };
        coverage::hit_prefixed("cell", label);
        assert!(
            delivered.is_err() && !executed,
            "law violated: a perform whose claim or result is lost delivered or executed\noutcome: {delivered:?}, executed: {executed}"
        );
        return;
    }
    let label = match (&delivered, executed) {
        (Ok(None), _) => "perform.empty",
        (_, true) => "perform.executed",
        (_, false) => "perform.redelivered",
    };
    coverage::hit_prefixed("cell", label);
    assert_eq!(
        delivered.map(|delivered| Expect::Perform {
            delivered,
            executed
        }),
        Ok(expected),
        "law violated: perform is not exactly-once per generation"
    );
}
