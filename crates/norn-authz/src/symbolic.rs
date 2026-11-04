//! Runtime admission analysis using upstream SymCC queries, never a proof
//! certificate. The embedding host must run the *entire* analysis in a
//! terminable, memory-limited worker (compilation and replay included).
//! No cvc5 binary or library is linked or distributed by this crate.

use std::future::Future;
use cedar_policy::{Authorizer, Decision};
use cedar_policy_symcc::{CedarSymCompiler, CompiledPolicySet, Env};
pub use cedar_policy_symcc::{solver::Solver, SmtLibScript};
// Hosts implement the transport without a direct dependency on SymCC.
pub use cedar_policy_symcc::solver::{Decision as SatDecision, DecisionWithModel, SolverError};
use crate::ValidatedPolicySet;

/// Built-in whole-policy-set property to ask of the candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Property {
    /// Every candidate Allow was already allowed by the previous set.
    Refinement,
    /// Both sets allow exactly the same requests.
    Equivalence,
}

/// Solver evidence, deliberately distinct from checked proof evidence.
#[derive(Debug, Clone, PartialEq)]
pub enum Analysis {
    /// All request environments were UNSAT; no proof certificate was checked.
    Unsat,
    /// A concrete request and entity store, replayed through Cedar.
    Counterexample(Box<Env>),
    /// Missing module, changed schema, unsupported fragment, resource limit,
    /// transport error, invalid model or failed replay. Retain this visibly.
    Unknown,
}

/// The admission obligation's treatment of incomplete evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obligation {
    /// Required refinement or tenant isolation: UNKNOWN refuses admission.
    Required,
    /// Diagnostic only: retain the result even when admission proceeds.
    Informational,
}

impl Analysis {
    /// Whether this evidence satisfies the requested admission obligation.
    #[must_use]
    pub fn admits(&self, obligation: Obligation) -> bool {
        obligation == Obligation::Informational || matches!(self, Self::Unsat)
    }
}

/// Host-enforced limits covering the entire operation, not a solver tlimit.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Wall-clock limit including compilation, solving and replay.
    pub milliseconds: u64,
    /// Worker memory ceiling, including the external WASM module.
    pub bytes: u64,
}

/// Boundary implemented by the embedding runtime's isolated worker executor.
/// The host must terminate the worker at either limit and return UNKNOWN.
/// An async timer around a synchronous solver does not satisfy this contract.
pub trait AnalysisHost {
    /// Run the complete query under the given limits. The module is loaded
    /// separately by this host and remains user-replaceable.
    fn run(
        &mut self,
        candidate: &ValidatedPolicySet,
        previous: &ValidatedPolicySet,
        property: Property,
        budget: Budget,
    ) -> impl Future<Output = Analysis>;
}

/// Runtime admission entry point. No configured host/module means UNKNOWN.
pub async fn analyze<H: AnalysisHost>(
    host: Option<&mut H>,
    candidate: &ValidatedPolicySet,
    previous: &ValidatedPolicySet,
    property: Property,
    budget: Budget,
) -> Analysis {
    if budget.milliseconds == 0 || budget.bytes == 0 {
        return Analysis::Unknown;
    }
    match host {
        Some(host) => host.run(candidate, previous, property, budget).await,
        None => Analysis::Unknown,
    }
}

/// Worker-side implementation. Call only inside the host's whole-analysis
/// resource boundary. Schema changes conservatively return UNKNOWN rather
/// than omitting candidate environments. No custom symbolic assertions.
pub async fn analyze_in_worker<S: Solver>(
    solver: S,
    candidate: &ValidatedPolicySet,
    previous: &ValidatedPolicySet,
    property: Property,
) -> Analysis {
    let schema = candidate.schema().cedar();
    if candidate.schema().symbolic_identity != previous.schema().symbolic_identity {
        return Analysis::Unknown;
    }
    let Ok(mut compiler) = CedarSymCompiler::new(solver) else {
        return Analysis::Unknown;
    };
    for environment in schema.request_envs() {
        let Ok(new) = CompiledPolicySet::compile(candidate.cedar_policies(), &environment, schema) else {
            return Analysis::Unknown;
        };
        let Ok(old) = CompiledPolicySet::compile(previous.cedar_policies(), &environment, schema) else {
            return Analysis::Unknown;
        };
        let result = match property {
            Property::Refinement => compiler.check_implies_with_counterexample_opt(&new, &old).await,
            Property::Equivalence => compiler.check_equivalent_with_counterexample_opt(&new, &old).await,
        };
        match result {
            Ok(None) => {},
            Ok(Some(witness)) => return replay(witness, candidate.cedar_policies(), previous.cedar_policies(), property),
            Err(_) => return Analysis::Unknown,
        }
    }
    Analysis::Unsat
}

fn replay(
    witness: Env,
    candidate: &cedar_policy::PolicySet,
    previous: &cedar_policy::PolicySet,
    property: Property,
) -> Analysis {
    let authorizer = Authorizer::new();
    let new = authorizer.is_authorized(&witness.request, candidate, &witness.entities);
    let old = authorizer.is_authorized(&witness.request, previous, &witness.entities);
    let differs = match property {
        Property::Refinement => new.decision() == Decision::Allow && old.decision() == Decision::Deny,
        Property::Equivalence => new.decision() != old.decision(),
    };
    if differs && new.diagnostics().errors().next().is_none() && old.diagnostics().errors().next().is_none() {
        Analysis::Counterexample(Box::new(witness))
    } else {
        Analysis::Unknown
    }
}

#[cfg(test)]
mod tests;
