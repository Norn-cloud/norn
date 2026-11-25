//! AUTH-INDEPENDENT-PERSON (#618): accountable-person separation for approval decisions.

use norn_cell::engine::SqlEngine;

use crate::approvals::HeldCall;
use crate::host::DevHost;

/// Whether `actor` may decide the approval of a parked action call `call`.
///
/// Fails closed when the decider has no verified principal, when either party
/// lacks a valid authenticated accountable person, or when requester and decider
/// resolve to the same subject or the same accountable person across distinct
/// sessions or delegated tokens.
pub(crate) fn allows_decider<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    call: &HeldCall,
) -> bool {
    if call.call.requester.subject == actor {
        return false;
    }
    let Some(decider) = host.authorizer.verified_principal(actor) else {
        return false;
    };
    norn_kernel::independent_accountable_people(
        call.call.requester.verified_accountable_person(),
        decider.verified_accountable_person(),
    )
}
