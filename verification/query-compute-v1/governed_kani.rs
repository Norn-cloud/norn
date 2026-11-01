//! Production QUERY-FRESH-1 policy: no flags authorize an unfenced root query.
#[path = "../../crates/norn-cell/src/compute/governed_policy.rs"]
mod policy;

#[kani::proof]
fn every_root_query_has_a_stable_refusal() {
    let cell: bool = kani::any();
    let exact: bool = kani::any();
    let complete: bool = kani::any();
    let refusal = policy::root_refusal(cell, exact, complete);
    assert_eq!(refusal.code(), "core.compute.stale_input");
    assert_eq!(refusal == policy::GovernedReadRefusal::UnfencedRoot, cell && exact && complete);
}
