#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "fixture assertions and deliberately synchronous test doubles"
)]

use super::*;
use cedar_policy::{Context, Entities, Request};
use std::sync::Arc;
use std::task::{Poll, Wake, Waker};

struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}
fn ready<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(Noop));
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(&waker))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("test double must complete synchronously"),
    }
}

fn sets() -> (ValidatedPolicySet, ValidatedPolicySet) {
    let schema = crate::AuthzSchema::from_projected_cedarschema(include_str!(
        "../../tests/fixtures/order.cedarschema"
    ))
    .expect("schema");
    let access = serde_json::from_str(include_str!("../../tests/fixtures/order_access.json"))
        .expect("AccessIR");
    let set = crate::compile(&schema, &access).expect("admitted fixture");
    let empty = crate::admit::admit(&schema, Vec::new()).expect("empty denies");
    (set, empty)
}

struct TimedOut;
impl AnalysisHost for TimedOut {
    async fn run(
        &mut self,
        _: &ValidatedPolicySet,
        _: &ValidatedPolicySet,
        _: Property,
        _: Budget,
    ) -> Analysis {
        Analysis::Unknown
    }
}

#[test]
fn missing_solver_and_host_timeout_remain_visible_unknown() {
    let (candidate, previous) = sets();
    let budget = Budget {
        milliseconds: 100,
        bytes: 1_000_000,
    };
    let missing = ready(analyze::<TimedOut>(
        None,
        &candidate,
        &previous,
        Property::Refinement,
        budget,
    ));
    assert_eq!(missing, Analysis::Unknown);
    assert!(!missing.admits(Obligation::Required));
    assert!(missing.admits(Obligation::Informational));
    let result = ready(analyze(
        Some(&mut TimedOut),
        &candidate,
        &previous,
        Property::Refinement,
        budget,
    ));
    assert_eq!(result, Analysis::Unknown);
    assert!(!result.admits(Obligation::Required));
}

#[test]
fn seeded_counterexample_must_replay_in_real_cedar() {
    let (_, previous) = sets();
    // Seed a privilege escalation inside the test; production admits AccessIR only.
    let candidate = "permit(principal, action, resource);"
        .parse()
        .expect("seed");
    let witness = || Env {
        request: Request::new(
            "Norn::Actor::\"outsider\"".parse().expect("principal"),
            "Norn::Action::\"post\"".parse().expect("action"),
            "commerce::Order::\"cross-tenant\""
                .parse()
                .expect("resource"),
            Context::empty(),
            None,
        )
        .expect("request"),
        entities: Entities::empty(),
    };
    let result = replay(
        witness(),
        &candidate,
        previous.cedar_policies(),
        Property::Refinement,
    );
    assert!(matches!(result, Analysis::Counterexample(_)));
    assert!(!result.admits(Obligation::Required));
    assert_eq!(
        replay(
            witness(),
            previous.cedar_policies(),
            previous.cedar_policies(),
            Property::Refinement
        ),
        Analysis::Unknown
    );
}
