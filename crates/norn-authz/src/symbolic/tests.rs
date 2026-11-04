use super::*;
use cedar_policy::{Context, Entities, Request};
use std::sync::Arc;
use std::task::{Poll, Wake, Waker};

struct Noop;
impl Wake for Noop { fn wake(self: Arc<Self>) {} }
fn ready<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(Noop));
    match future.as_mut().poll(&mut std::task::Context::from_waker(&waker)) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("test double must complete synchronously"),
    }
}

struct Double(DecisionWithModel);
impl WasmModule for Double {
    async fn solve(&mut self, input: Vec<u8>) -> Result<DecisionWithModel, SolverError> {
        let text = String::from_utf8(input).expect("SMTLib is UTF8");
        assert!(text.contains("(check-sat)"));
        Ok(self.0.clone())
    }
}

fn sets() -> (ValidatedPolicySet, ValidatedPolicySet) {
    let schema = crate::AuthzSchema::from_projected_cedarschema(
        include_str!("../../tests/fixtures/order.cedarschema")
    ).expect("schema");
    let access = serde_json::from_str(include_str!("../../tests/fixtures/order_access.json")).expect("AccessIR");
    let set = crate::compile(&schema, &access).expect("admitted fixture");
    let empty = crate::admit::admit(&schema, Vec::new()).expect("empty denies");
    (set, empty)
}

struct TimedOut;
impl AnalysisHost for TimedOut {
    async fn run(&mut self, _: &ValidatedPolicySet, _: &ValidatedPolicySet, _: Property, _: Budget) -> Analysis {
        Analysis::Unknown
    }
}

#[test]
fn missing_solver_and_host_timeout_remain_visible_unknown() {
    let (candidate, previous) = sets();
    let budget = Budget { milliseconds: 100, bytes: 1_000_000 };
    let missing = ready(analyze::<TimedOut>(None, &candidate, &previous, Property::Refinement, budget));
    assert_eq!(missing, Analysis::Unknown);
    assert!(!missing.admits(Obligation::Required));
    assert!(missing.admits(Obligation::Informational));
    let result = ready(analyze(Some(&mut TimedOut), &candidate, &previous, Property::Refinement, budget));
    assert_eq!(result, Analysis::Unknown);
    assert!(!result.admits(Obligation::Required));
}

#[test]
fn upstream_queries_use_memory_transport_for_every_environment() {
    let (candidate, _) = sets();
    for property in [Property::Refinement, Property::Equivalence] {
        let result = ready(analyze_in_worker(MemorySolver::new(Double(DecisionWithModel::Unsat)), &candidate, &candidate, property));
        assert_eq!(result, Analysis::Unsat);
    }
    let result = ready(analyze_in_worker(MemorySolver::new(Double(DecisionWithModel::Unknown)), &candidate, &candidate, Property::Refinement));
    assert_eq!(result, Analysis::Unknown);
}

#[test]
fn seeded_counterexample_must_replay_in_real_cedar() {
    let (_, previous) = sets();
    // Seed a privilege escalation inside the test; production admits AccessIR only.
    let candidate = "permit(principal, action, resource);".parse().expect("seed");
    let witness = || Env {
        request: Request::new(
            "Norn::Actor::\"outsider\"".parse().expect("principal"),
            "Norn::Action::\"post\"".parse().expect("action"),
            "commerce::Order::\"cross-tenant\"".parse().expect("resource"),
            Context::empty(), None,
        ).expect("request"),
        entities: Entities::empty(),
    };
    let result = replay(witness(), &candidate, previous.cedar_policies(), Property::Refinement);
    assert!(matches!(result, Analysis::Counterexample(_)));
    assert!(!result.admits(Obligation::Required));
    assert_eq!(replay(witness(), previous.cedar_policies(), previous.cedar_policies(), Property::Refinement), Analysis::Unknown);
}
