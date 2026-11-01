//! Governed-read integration with Core 04's one-use action read proof.
pub use super::governed_policy::GovernedReadRefusal;
use super::{Engine, Freshness, QueryResult, governed_policy::root_refusal};
use norn_kernel::{CellCommit, FactExpectation, action_commit::ActionReferenceReads};

/// Server-declared dependency used while preparing an admitted action.
/// This adapter grants no authorization to a query envelope or point expectation.
pub enum GovernedRead<'a> {
    /// Fixed point dependencies; the kernel remains their commit-fence authority.
    PointFacts(&'a [FactExpectation]),
    /// Every currently admitted question ranges over a root population.
    RootQuestion(&'a QueryResult),
}

/// QUERY-FRESH-1: attach point reads using Lane A's existing action seam.
/// Root questions always refuse until their membership proof can be captured
/// through that shared contract, including empty and exact/complete populations.
/// A refusal leaves the proposal unchanged and consumes the one-use action proof.
///
/// # Errors
/// Returns the stable stale-input diagnostic for analytical/stale/incomplete
/// answers, unfenced roots, invalid action proofs or conflicting point versions.
pub fn bind_governed_read<E: Clone, C: Clone, I: Clone>(
    proof: ActionReferenceReads,
    proposal: &mut CellCommit<E, C, I>,
    read: GovernedRead<'_>,
) -> Result<ActionReferenceReads, GovernedReadRefusal> {
    match read {
        GovernedRead::RootQuestion(result) => Err(root_refusal(
            result.engine == Engine::Cell,
            result.freshness == Freshness::Exact,
            result.complete,
        )),
        GovernedRead::PointFacts(guards) => {
            let mut staged = proposal.clone();
            let proof = proof
                .with_read_guards(&mut staged, guards)
                .ok_or(GovernedReadRefusal::ConflictingReadSet)?;
            *proposal = staged;
            Ok(proof)
        }
    }
}
