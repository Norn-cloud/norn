//! In-memory transport for separately loaded, replaceable solver modules.

use norn_authz::symbolic::SatDecision as Sat;
use norn_authz::symbolic::{DecisionWithModel, SmtLibScript, Solver, SolverError};
use std::future::Future;

/// In-memory transport implemented by the host's separately loaded official
/// cvc5 WASM module. SAT must include a model; request get-model only after SAT.
pub trait WasmModule: Send {
    /// Execute the complete buffered `SMTLib` query. No subprocess transport.
    ///
    /// # Errors
    /// Returns the module or transport failure without treating it as UNSAT.
    fn solve(
        &mut self,
        input: Vec<u8>,
    ) -> impl Future<Output = Result<DecisionWithModel, SolverError>> + Send;
}

/// `SymCC`'s Solver adapter. Uses only upstream-generated `SMTLib`.
#[derive(Debug)]
pub struct MemorySolver<M> {
    input: Vec<u8>,
    module: M,
}

impl<M: WasmModule> MemorySolver<M> {
    /// Bind one replaceable module instance to a fresh in-memory buffer.
    pub const fn new(module: M) -> Self {
        Self {
            input: Vec::new(),
            module,
        }
    }
}

impl<M: WasmModule> Solver for MemorySolver<M> {
    fn smtlib_input(&mut self) -> &mut (dyn tokio::io::AsyncWrite + Unpin + Send) {
        &mut self.input
    }

    async fn enable_models(&mut self) -> Result<(), SolverError> {
        self.input.set_option("produce-models", "true").await?;
        Ok(())
    }

    async fn check_sat(&mut self) -> Result<Sat, SolverError> {
        Ok(match self.check_sat_with_model().await? {
            DecisionWithModel::Sat { .. } => Sat::Sat,
            DecisionWithModel::Unsat => Sat::Unsat,
            DecisionWithModel::Unknown => Sat::Unknown,
        })
    }

    async fn check_sat_with_model(&mut self) -> Result<DecisionWithModel, SolverError> {
        self.input.check_sat().await?;
        self.module.solve(std::mem::take(&mut self.input)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_authz::{
        ValidatedPolicySet,
        symbolic::{Analysis, Property, analyze_in_worker},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::task::{Poll, Waker};
    fn ready<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        match future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(waker))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test double must complete synchronously"),
        }
    }

    fn sets() -> (ValidatedPolicySet, ValidatedPolicySet) {
        let schema = norn_authz::AuthzSchema::from_projected_cedarschema(include_str!(
            "../../norn-authz/tests/fixtures/order.cedarschema"
        ))
        .expect("schema");
        let access = serde_json::from_str(include_str!(
            "../../norn-authz/tests/fixtures/order_access.json"
        ))
        .expect("AccessIR");
        let set = norn_authz::compile(&schema, &access).expect("admitted fixture");
        let empty = norn_authz::compile_all(&schema, &[]).expect("empty set");
        (set, empty)
    }

    struct Double(DecisionWithModel, Arc<AtomicUsize>);
    impl WasmModule for Double {
        fn solve(
            &mut self,
            input: Vec<u8>,
        ) -> impl Future<Output = Result<DecisionWithModel, SolverError>> + Send {
            self.1.fetch_add(1, Ordering::Relaxed);
            let text = String::from_utf8(input).expect("SMTLib is UTF8");
            assert!(text.contains("(check-sat)"));
            std::future::ready(Ok(self.0.clone()))
        }
    }

    #[test]
    fn upstream_queries_use_memory_transport_for_every_environment() {
        let (candidate, previous) = sets();
        let calls = Arc::new(AtomicUsize::new(0));
        for property in [Property::Refinement, Property::Equivalence] {
            let result = ready(analyze_in_worker(
                MemorySolver::new(Double(DecisionWithModel::Unsat, Arc::clone(&calls))),
                &candidate,
                &previous,
                property,
            ));
            assert_eq!(result, Analysis::Unsat);
        }
        assert!(calls.load(Ordering::Relaxed) > 0);
        let before = calls.load(Ordering::Relaxed);
        let result = ready(analyze_in_worker(
            MemorySolver::new(Double(DecisionWithModel::Unknown, Arc::clone(&calls))),
            &candidate,
            &previous,
            Property::Refinement,
        ));
        assert_eq!(result, Analysis::Unknown);
        assert!(calls.load(Ordering::Relaxed) > before);
    }
}
