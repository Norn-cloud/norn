//! AUTH-INDEPENDENT-PERSON (#618): accountable-person identity and the exact
//! prepared intent an approval binds to.
//!
//! An approval is recorded by the host's decision ledger under authenticated
//! principals; the kernel's part is the commit boundary. A call made under an
//! approval carries the digest the approval was granted for, and the kernel
//! commits it only when the commit it is about to hand to the host has that
//! same digest.

use std::collections::BTreeMap;

use norn_ir::quantity_ledger::{QuantityOperation, QuantityRequest};
use serde::Serialize;

use crate::cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, Consumed, DueTimer, FactKey,
    FactState, FactWrite, JsonObject, LinkKey, LinkReadTarget, LinkState, LinkWrite, Observed,
    OwnedRowsCommitProof, Performed, PredicateObject, PredicateQuery, SerializationLane,
};
use crate::decision_commit::{DecisionRefusal, FactEvent, TypeCatalog};
use crate::effect::Effect;
use crate::limits::EnvelopeLimits;
use crate::observations::World;

use super::types::{ActionAuthorizer, ActionCall, ActionReferenceReads, ActionRefusal};

/// Maximum UTF-8 byte length of an accountable-person identifier.
const MAX_ACCOUNTABLE_PERSON_BYTES: usize = 256;

const PREPARED_INTENT_DOMAIN: &[u8] = b"norn.action.prepared-intent.v2\0";

fn is_forbidden_identity_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// Validate an authenticated accountable-person identifier fail-closed.
///
/// Returns `None` when absent, empty/whitespace, padded, over-length, containing
/// control/bidi characters, or not Unicode NFC normalized.
#[must_use]
pub fn validate_accountable_person(person: Option<&str>) -> Option<&str> {
    person.filter(|value| {
        !value.is_empty()
            && value.trim() == *value
            && value.len() <= MAX_ACCOUNTABLE_PERSON_BYTES
            && !value.chars().any(is_forbidden_identity_char)
            && norn_ir::canonical::nfc(value) == *value
    })
}

/// Return `true` iff both inputs are valid, canonical, authenticated accountable-person
/// identifiers and refer to two distinct people (`AUTH-INDEPENDENT-PERSON`).
///
/// Missing or malformed identities always fail closed (`false`).
#[must_use]
pub fn independent_accountable_people(first: Option<&str>, second: Option<&str>) -> bool {
    match (
        validate_accountable_person(first),
        validate_accountable_person(second),
    ) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    }
}

/// Whether a call by `current` matches a record retained for `recorded`.
///
/// A record retained before accountable people were bound names none: its
/// authenticated subject alone identifies the caller, and its bytes stay as
/// they were. A record that names a person matches only that same person, so
/// a retry can never swap who is accountable for a call.
#[must_use]
pub fn retained_person_matches(recorded: Option<&str>, current: Option<&str>) -> bool {
    recorded.is_none_or(|person| current == Some(person))
}

/// The commit an action call is about to hand to its host, as its approval
/// digest binds it: everything that commit changes in the world.
pub(super) struct PreparedCommit<'a> {
    pub(super) target: &'a FactKey,
    pub(super) effective_input: &'a JsonObject,
    /// The call's own idempotency record, which is bookkeeping, not intent.
    pub(super) record: &'a FactKey,
    pub(super) consumes: Option<&'a Consumed>,
    pub(super) writes: &'a [FactWrite],
    pub(super) links: &'a [LinkWrite],
    pub(super) effects: &'a [Effect<JsonObject, JsonObject>],
    pub(super) quantity: Option<&'a QuantityRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedIntentCanonical<'a> {
    action_id: &'a str,
    action_artifact_id: &'a str,
    action_version: &'a str,
    scope: &'a BTreeMap<String, String>,
    world: &'a World,
    turn: &'a str,
    target: &'a FactKey,
    raw_input: &'a JsonObject,
    effective_input: &'a JsonObject,
    consumes: Option<&'a Consumed>,
    writes: Vec<&'a FactWrite>,
    links: &'a [LinkWrite],
    effects: &'a [Effect<JsonObject, JsonObject>],
    quantity: Option<QuantityRequest>,
}

/// A reservation's lifetime, not the clock reading it starts from, is what an
/// approval covers: its expiry is bound relative to the committing turn.
fn clock_relative(request: &QuantityRequest, call: &ActionCall<'_>) -> QuantityRequest {
    let mut request = request.clone();
    if let QuantityOperation::Reserve { expires_at, .. } = &mut request.operation {
        *expires_at = expires_at.saturating_sub(call.turn.now.as_millis());
    }
    request
}

fn prepared_intent_digest<X>(
    call: &ActionCall<'_>,
    commit: &PreparedCommit<'_>,
) -> Result<String, ActionRefusal<X>> {
    let canonical = PreparedIntentCanonical {
        action_id: &call.action.action_id,
        action_artifact_id: call.action_artifact_id,
        action_version: call.action_version,
        scope: &call.scope,
        world: &call.turn.world,
        turn: &call.turn.id,
        target: commit.target,
        raw_input: &call.input,
        effective_input: commit.effective_input,
        consumes: commit.consumes,
        writes: commit
            .writes
            .iter()
            .filter(|write| write.key != *commit.record)
            .collect(),
        links: commit.links,
        effects: commit.effects,
        quantity: commit.quantity.map(|request| clock_relative(request, call)),
    };
    let hex = norn_ir::canonical::domain_hash_hex(PREPARED_INTENT_DOMAIN, &canonical).map_err(
        |error| {
            ActionRefusal::Decision(DecisionRefusal::InvalidOutput {
                reason: format!("failed to compute prepared intent digest: {error}"),
            })
        },
    )?;
    Ok(format!("sha256:{hex}"))
}

/// Bind `commit` to the call's approval, or capture its digest for
/// [`prepare_action_intent`]. A call without either commits as prepared.
///
/// # Errors
/// [`ActionRefusal::ApprovedIntentChanged`] when the commit is not the one
/// the call's approval was granted for.
pub(super) fn bind_approved_intent<X>(
    call: &ActionCall<'_>,
    commit: &PreparedCommit<'_>,
    capture: Option<&mut Option<String>>,
) -> Result<(), ActionRefusal<X>> {
    if capture.is_none() && call.approved_intent.is_none() {
        return Ok(());
    }
    let digest = prepared_intent_digest(call, commit)?;
    if let Some(slot) = capture {
        *slot = Some(digest);
        return Err(ActionRefusal::TargetNotFound);
    }
    if call.approved_intent.as_deref() == Some(digest.as_str()) {
        Ok(())
    } else {
        Err(ActionRefusal::ApprovedIntentChanged)
    }
}

/// Read-only host adapter that guarantees [`prepare_action_intent`] never mutates storage.
struct ReadOnlyIntentHost<'a, H: ?Sized>(&'a H);

impl<H> CellHost for ReadOnlyIntentHost<'_, H>
where
    H: CellHost + ?Sized,
{
    type Event = H::Event;
    type Command = H::Command;
    type Input = H::Input;
    type Error = H::Error;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.0.load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        self.0.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        self.0.load_reference(key, max_bytes)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        self.0.load_bounded(key, max_bytes)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &crate::receipt_binding::ReceiptReference,
        manifest: &norn_ir::function::Digest,
    ) -> Result<
        Option<crate::receipt_binding::ReceiptBinding<JsonObject>>,
        crate::receipt_binding::ReceiptBindingError<Self::Error>,
    > {
        self.0.resolve_accepted_receipt(reference, manifest)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.0.load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        self.0
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, Self::Error> {
        self.0.read_predicate(predicate, max_members, max_bytes)
    }
    fn commit(
        &mut self,
        _commit: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        Err(CommitError::ObservationAuthorization)
    }
    fn commit_action_quantity(
        &mut self,
        _commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        _requests: &[norn_ir::quantity_ledger::QuantityRequest],
        _idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        Err(CommitError::ObservationAuthorization)
    }
    fn commit_action_references(
        &mut self,
        _commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        _proof: ActionReferenceReads,
        _quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        Err(CommitError::ObservationAuthorization)
    }
    fn commit_owned_rows(
        &mut self,
        _commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        _proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        Err(CommitError::ObservationAuthorization)
    }
    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error> {
        Ok(None)
    }
    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error> {
        Ok(None)
    }
    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error> {
        Ok(None)
    }
    fn serialize_by(&self, key: &FactKey) -> SerializationLane {
        self.0.serialize_by(key)
    }
    fn publish(&mut self, _receipt: &CommitReceipt) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The digest of the commit `call` would make now, without mutating `host`:
/// what an approval of the call is granted for and bound to at commit.
///
/// # Errors
///
/// Returns any [`ActionRefusal`] raised by scope, input, authorization, slice,
/// precondition, or effect preparation checks.
pub fn prepare_action_intent<H>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
) -> Result<String, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    prepare_action_intent_with_limits(host, catalog, call, authorizer, &EnvelopeLimits::V1)
}

/// [`prepare_action_intent`] under explicit per-decision envelope limits.
///
/// # Errors
///
/// Returns any [`ActionRefusal`] raised before commit.
pub fn prepare_action_intent_with_limits<H>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    limits: &EnvelopeLimits,
) -> Result<String, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let mut readonly = ReadOnlyIntentHost(host);
    let mut captured = None;
    let outcome = super::commit_action_inner(
        &mut readonly,
        catalog,
        call,
        authorizer,
        None,
        super::types::EnvelopePolicy {
            limits,
            meter: None,
            capture: Some(&mut captured),
        },
    );
    match (captured, outcome) {
        (Some(digest), _) => Ok(digest),
        (None, Err(refusal)) => Err(refusal),
        (None, Ok(_replayed)) => Err(ActionRefusal::IdempotencyConflict {
            key: call.idempotency_key.clone(),
        }),
    }
}
