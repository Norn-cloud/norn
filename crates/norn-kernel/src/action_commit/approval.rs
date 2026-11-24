//! AUTH-INDEPENDENT-PERSON / AUTH-ATTENUATE-1 / AUTH-REVOKE (#618):
//! Ring-0 independent accountable-person dual approval and exact prepared-intent binding.
//!
//! Two distinct sessions or delegated agents acting for the same accountable person
//! never satisfy independent-person approval. Missing, blank, or non-canonical
//! accountable-person identities fail closed without falling back to session or actor IDs.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::Decimal;
use serde::{Deserialize, Serialize};

use crate::cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactKey, FactMutation,
    FactState, FactWrite, JsonObject, JsonValue, LinkKey, LinkMutation, LinkReadTarget, LinkState,
    LinkWrite, Observed, OwnedRowsCommitProof, Performed, PredicateObject, PredicateQuery,
    SerializationLane,
};
use crate::decision_commit::{DecisionRefusal, FactEvent, TypeCatalog};
use crate::limits::EnvelopeLimits;

use super::types::{ActionAuthorizer, ActionCall, ActionReferenceReads, ActionRefusal};

/// Maximum UTF-8 byte length of an accountable-person or evidence identifier.
pub(super) const MAX_ACCOUNTABLE_PERSON_BYTES: usize = 256;

/// Maximum approval/assay evidence items admitted on one action call.
pub(super) const MAX_APPROVAL_EVIDENCE_ITEMS: usize = 64;

const PREPARED_INTENT_DOMAIN: &[u8] = b"norn.action.prepared-intent.v1\0";

/// Kind of evidence presented for independent-person approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalEvidenceKind {
    /// Human approval of the prepared action intent.
    Approval,
    /// Independent assay or measurement evidence bound to the prepared intent.
    Assay,
}

impl ApprovalEvidenceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Assay => "assay",
        }
    }
}

/// Authenticated delegation attenuation and revocation context for a session or agent
/// acting on behalf of an accountable person (`AUTH-ATTENUATE-1`, `AUTH-REVOKE`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationAttenuation {
    /// Authenticated accountable human delegator who owns this delegated grant.
    pub delegator: String,
    /// Revocation generation at which this delegated grant was issued.
    pub revocation_generation: u64,
    /// Current active revocation generation; grants below this generation are revoked.
    pub active_revocation_generation: u64,
    /// Explicit revocation flag; `true` refuses immediately (`core.authority.revoked`).
    pub revoked: bool,
    /// Delegated scope; must match the action call's exact scope (no widening).
    pub scope: BTreeMap<String, String>,
    /// Delegated action allowlist; must include the invoked action id.
    pub allowed_actions: Vec<String>,
    /// Optional exclusive expiry in cell logical milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

/// One authenticated approval or assay evidence item bound to an exact prepared intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalEvidence {
    /// Unique non-blank evidence identifier (prevents duplicate evidence reuse).
    pub evidence_id: String,
    /// Whether this item is an approval or an independent assay.
    pub kind: ApprovalEvidenceKind,
    /// Authenticated acting principal, session, or delegated agent identifier.
    pub actor: String,
    /// Authenticated accountable human person behind `actor`.
    /// `None`, blank, or non-canonical values always fail closed and never fall back to `actor`.
    pub accountable_person: Option<String>,
    /// Canonical `sha256:<64-hex>` digest of the exact prepared action intent.
    pub intent_digest: String,
    /// Optional authenticated delegation attenuation and revocation state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<DelegationAttenuation>,
}

/// Policy governing independent accountable-person approval for an action commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndependentApprovalPolicy {
    /// Minimum number of distinct authenticated accountable people required in `evidence`.
    pub required_distinct_people: u32,
    /// When `true`, the calling requester must carry a valid authenticated `accountable_person`
    /// and no approval/assay evidence may resolve to the same accountable person.
    pub require_independent_from_requester: bool,
    /// When `Some(kind)`, only evidence of that kind satisfies `required_distinct_people`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_kind: Option<ApprovalEvidenceKind>,
}

impl IndependentApprovalPolicy {
    /// Require at least two distinct authenticated accountable people across evidence items.
    #[must_use]
    pub const fn dual_independent_people() -> Self {
        Self {
            required_distinct_people: 2,
            require_independent_from_requester: false,
            required_kind: None,
        }
    }

    /// Require at least two distinct authenticated accountable people supplying assay evidence.
    #[must_use]
    pub const fn dual_independent_assays() -> Self {
        Self {
            required_distinct_people: 2,
            require_independent_from_requester: false,
            required_kind: Some(ApprovalEvidenceKind::Assay),
        }
    }

    /// Require an authenticated requester plus at least one distinct authenticated approver.
    #[must_use]
    pub const fn independent_approver_from_requester() -> Self {
        Self {
            required_distinct_people: 1,
            require_independent_from_requester: true,
            required_kind: Some(ApprovalEvidenceKind::Approval),
        }
    }

    /// Require an authenticated requester plus at least two distinct authenticated approvers.
    #[must_use]
    pub const fn dual_approvers_independent_from_requester() -> Self {
        Self {
            required_distinct_people: 2,
            require_independent_from_requester: true,
            required_kind: Some(ApprovalEvidenceKind::Approval),
        }
    }
}

/// Independent-person approval policy and evidence attached to an [`ActionCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndependentApproval {
    /// The policy to enforce before any write commits.
    pub policy: IndependentApprovalPolicy,
    /// Authenticated approval/assay evidence bound to the exact prepared intent.
    pub evidence: Vec<ApprovalEvidence>,
    /// Optional delegation attenuation and revocation context for the calling requester.
    pub requester_delegation: Option<DelegationAttenuation>,
}

/// One verified approval or assay entry retained on [`super::ActionCallRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionApprovalEvidenceRecord {
    /// Unique evidence identifier.
    pub evidence_id: String,
    /// Evidence kind (`approval` or `assay`).
    pub kind: ApprovalEvidenceKind,
    /// Authenticated acting principal/session/agent id.
    pub actor: String,
    /// Authenticated accountable human person id.
    pub accountable_person: String,
}

impl ActionApprovalEvidenceRecord {
    fn to_runtime_value(&self) -> JsonValue {
        let string = |value: &str| JsonValue::String(value.to_owned());
        JsonValue::Object(JsonObject::from([
            ("evidenceId".to_owned(), string(&self.evidence_id)),
            ("kind".to_owned(), string(self.kind.as_str())),
            ("actor".to_owned(), string(&self.actor)),
            (
                "accountablePerson".to_owned(),
                string(&self.accountable_person),
            ),
        ]))
    }

    fn is_valid(&self) -> bool {
        validate_canonical_identity(&self.evidence_id).is_some()
            && validate_canonical_identity(&self.actor).is_some()
            && validate_accountable_person(Some(&self.accountable_person)).is_some()
    }
}

/// Verified independent-person approval receipt retained on [`super::ActionCallRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionApprovalReceipt {
    /// Canonical SHA-256 digest (`sha256:<64 lower-case hex>`) of the exact prepared intent.
    pub intent_digest: String,
    /// Minimum distinct accountable people required by policy.
    pub required_distinct_people: u32,
    /// Distinct authenticated accountable people who satisfied the policy, sorted canonically.
    pub accountable_people: Vec<String>,
    /// Verified approval/assay evidence entries bound to this commit.
    pub evidence: Vec<ActionApprovalEvidenceRecord>,
}

impl ActionApprovalReceipt {
    pub(super) fn to_runtime_value(&self) -> JsonValue {
        let string = |value: &str| JsonValue::String(value.to_owned());
        JsonValue::Object(JsonObject::from([
            ("intentDigest".to_owned(), string(&self.intent_digest)),
            (
                "requiredDistinctPeople".to_owned(),
                JsonValue::Number(Decimal::from(self.required_distinct_people)),
            ),
            (
                "accountablePeople".to_owned(),
                JsonValue::Array(
                    self.accountable_people
                        .iter()
                        .map(|person| string(person))
                        .collect(),
                ),
            ),
            (
                "evidence".to_owned(),
                JsonValue::Array(
                    self.evidence
                        .iter()
                        .map(ActionApprovalEvidenceRecord::to_runtime_value)
                        .collect(),
                ),
            ),
        ]))
    }

    pub(super) fn is_valid(&self) -> bool {
        let people_ok = !self.accountable_people.is_empty()
            && self
                .accountable_people
                .iter()
                .all(|person| validate_accountable_person(Some(person)).is_some());
        let distinct_people: BTreeSet<&str> =
            self.accountable_people.iter().map(String::as_str).collect();
        is_sha256_digest(&self.intent_digest)
            && self.required_distinct_people > 0
            && people_ok
            && distinct_people.len() == self.accountable_people.len()
            && usize::try_from(self.required_distinct_people)
                .is_ok_and(|required| distinct_people.len() >= required)
            && !self.evidence.is_empty()
            && self
                .evidence
                .iter()
                .all(ActionApprovalEvidenceRecord::is_valid)
    }
}

/// Deterministic prepared intent of an action call, computed without committing.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedActionIntent {
    /// Canonical `sha256:<64-hex>` digest binding action, scope, target, inputs, writes,
    /// links, outbound commands, and quantity intent.
    pub intent_digest: String,
    /// Concrete target object key.
    pub target: FactKey,
    /// Effective input after ordered derivations.
    pub effective_input: JsonObject,
    /// Exact prepared business writes (excluding the `norn.ActionCall` idempotency record).
    pub writes: Vec<FactWrite>,
    /// Exact prepared link writes.
    pub links: Vec<LinkWrite>,
    /// Exact prepared outbound command payloads.
    pub commands: Vec<JsonObject>,
    /// Optional prepared quantity ledger request.
    pub quantity: Option<norn_ir::quantity_ledger::QuantityRequest>,
}

pub(super) struct PreparedIntentParts<'a> {
    pub(super) target: &'a FactKey,
    pub(super) effective_input: &'a JsonObject,
    pub(super) writes: &'a [FactWrite],
    pub(super) links: &'a [LinkWrite],
    pub(super) commands: &'a [JsonObject],
    pub(super) quantity: Option<&'a norn_ir::quantity_ledger::QuantityRequest>,
}

fn is_forbidden_identity_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

fn validate_canonical_identity(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed != value
        || value.len() > MAX_ACCOUNTABLE_PERSON_BYTES
        || value.chars().any(is_forbidden_identity_char)
        || norn_ir::canonical::nfc(value) != value
    {
        return None;
    }
    Some(value)
}

/// Validate an authenticated accountable-person identifier fail-closed.
///
/// Returns `None` when absent, empty/whitespace, padded, over-length, containing
/// control/bidi characters, or not Unicode NFC normalized.
#[must_use]
pub fn validate_accountable_person(person: Option<&str>) -> Option<&str> {
    person.and_then(validate_canonical_identity)
}

/// Return `true` iff both inputs are valid, canonical, authenticated accountable-person
/// identifiers and refer to two distinct people (`AUTH-INDEPENDENT-PERSON`).
///
/// Missing or malformed identities always fail closed (`false`).
#[must_use]
pub fn independent_accountable_people(first: Option<&str>, second: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (
        validate_accountable_person(first),
        validate_accountable_person(second),
    ) else {
        return false;
    };
    left != right
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IntentFactWrite<'a> {
    object_type: &'a str,
    key: &'a str,
    expected_version: u64,
    mutation: IntentFactMutation<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum IntentFactMutation<'a> {
    Put { values: &'a JsonObject },
    Delete,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IntentLinkWrite<'a> {
    source_type: &'a str,
    source_key: &'a str,
    link: &'a str,
    target_type: &'a str,
    target_key: &'a str,
    expected_version: u64,
    mutation: IntentLinkMutation<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum IntentLinkMutation<'a> {
    Add { values: &'a JsonObject },
    Remove,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedIntentCanonical<'a> {
    action_id: &'a str,
    action_artifact_id: &'a str,
    action_version: &'a str,
    scope: &'a BTreeMap<String, String>,
    target_type: &'a str,
    target_key: &'a str,
    raw_input: &'a JsonObject,
    effective_input: &'a JsonObject,
    writes: Vec<IntentFactWrite<'a>>,
    links: Vec<IntentLinkWrite<'a>>,
    commands: &'a [JsonObject],
    quantity: Option<&'a norn_ir::quantity_ledger::QuantityRequest>,
}

pub(super) fn prepared_intent_digest<X>(
    call: &ActionCall<'_>,
    parts: &PreparedIntentParts<'_>,
) -> Result<String, ActionRefusal<X>> {
    let canonical = PreparedIntentCanonical {
        action_id: &call.action.action_id,
        action_artifact_id: call.action_artifact_id,
        action_version: call.action_version,
        scope: &call.scope,
        target_type: parts.target.object_type.as_str(),
        target_key: &parts.target.key,
        raw_input: &call.input,
        effective_input: parts.effective_input,
        writes: parts
            .writes
            .iter()
            .map(|write| IntentFactWrite {
                object_type: write.key.object_type.as_str(),
                key: &write.key.key,
                expected_version: write.expected.0,
                mutation: match &write.mutation {
                    FactMutation::Put { values } => IntentFactMutation::Put { values },
                    FactMutation::Delete => IntentFactMutation::Delete,
                },
            })
            .collect(),
        links: parts
            .links
            .iter()
            .map(|write| IntentLinkWrite {
                source_type: write.key.source.object_type.as_str(),
                source_key: &write.key.source.key,
                link: &write.key.link,
                target_type: write.key.target.object_type.as_str(),
                target_key: &write.key.target.key,
                expected_version: write.expected.0,
                mutation: match &write.mutation {
                    LinkMutation::Add { values } => IntentLinkMutation::Add { values },
                    LinkMutation::Remove => IntentLinkMutation::Remove,
                },
            })
            .collect(),
        commands: parts.commands,
        quantity: parts.quantity,
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

fn check_delegation<X>(
    delegation: &DelegationAttenuation,
    expected_person: &str,
    call: &ActionCall<'_>,
) -> Result<(), ActionRefusal<X>> {
    let Some(delegator) = validate_accountable_person(Some(&delegation.delegator)) else {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "delegation is missing a valid accountable delegator".to_owned(),
        });
    };
    if delegator != expected_person {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "delegation accountable delegator does not match the accountable person"
                .to_owned(),
        });
    }
    if delegation.revoked
        || delegation.revocation_generation < delegation.active_revocation_generation
    {
        return Err(ActionRefusal::AuthorityRevoked {
            reason: "delegated authority has been revoked".to_owned(),
        });
    }
    if delegation
        .expires_at_ms
        .is_some_and(|expires| call.turn.now.as_millis() >= expires)
    {
        return Err(ActionRefusal::AuthorityRevoked {
            reason: "delegated authority has expired".to_owned(),
        });
    }
    if delegation.scope != call.scope {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "delegated scope does not match the call scope".to_owned(),
        });
    }
    if !delegation
        .allowed_actions
        .iter()
        .any(|allowed| allowed == &call.action.action_id)
    {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "delegated grant does not attenuate to this action".to_owned(),
        });
    }
    Ok(())
}

fn verify_evidence_item<'a, X>(
    call: &ActionCall<'_>,
    policy: &IndependentApprovalPolicy,
    requester_person: Option<&str>,
    intent_digest: &str,
    item: &'a ApprovalEvidence,
    seen_ids: &mut BTreeSet<&'a str>,
    seen_people: &mut BTreeSet<&'a str>,
) -> Result<(ActionApprovalEvidenceRecord, bool), ActionRefusal<X>> {
    let Some(evidence_id) = validate_canonical_identity(&item.evidence_id) else {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "approval evidence id is missing or non-canonical".to_owned(),
        });
    };
    if !seen_ids.insert(evidence_id) {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "duplicate approval or assay evidence id is refused".to_owned(),
        });
    }
    let Some(actor) = validate_canonical_identity(&item.actor) else {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "approval evidence actor identity is missing or non-canonical".to_owned(),
        });
    };
    let Some(person) = validate_accountable_person(item.accountable_person.as_deref()) else {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "approval evidence requires an authenticated accountable person".to_owned(),
        });
    };
    if item.intent_digest != intent_digest {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "approval evidence intent digest does not match the prepared intent".to_owned(),
        });
    }
    if let Some(delegation) = &item.delegation {
        check_delegation(delegation, person, call)?;
    }
    if policy.require_independent_from_requester
        && !independent_accountable_people(requester_person, Some(person))
    {
        return Err(ActionRefusal::AuthorityDenied {
            reason:
                "approver must be an independent accountable person distinct from the requester"
                    .to_owned(),
        });
    }
    if !seen_people.insert(person) {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "duplicate accountable person across approval or assay evidence is refused"
                .to_owned(),
        });
    }
    let counts = policy.required_kind.is_none_or(|kind| item.kind == kind);
    Ok((
        ActionApprovalEvidenceRecord {
            evidence_id: evidence_id.to_owned(),
            kind: item.kind,
            actor: actor.to_owned(),
            accountable_person: person.to_owned(),
        },
        counts,
    ))
}

/// Verify independent accountable-person approval against the exact prepared intent digest.
pub(super) fn verify_independent_approval<X>(
    call: &ActionCall<'_>,
    approval: &IndependentApproval,
    intent_digest: &str,
) -> Result<ActionApprovalReceipt, ActionRefusal<X>> {
    let policy = &approval.policy;
    let max_items = u32::try_from(MAX_APPROVAL_EVIDENCE_ITEMS).unwrap_or(64);
    if policy.required_distinct_people == 0 || policy.required_distinct_people > max_items {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "approval policy requires between 1 and 64 distinct accountable people"
                .to_owned(),
        });
    }
    if approval.evidence.len() > MAX_APPROVAL_EVIDENCE_ITEMS {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "approval evidence exceeds the maximum permitted item count".to_owned(),
        });
    }
    let requester_person = validate_accountable_person(call.accountable_person.as_deref());
    if call.accountable_person.is_some() && requester_person.is_none() {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "requester accountable person identity is invalid".to_owned(),
        });
    }
    if let Some(delegation) = &approval.requester_delegation {
        let Some(person) = requester_person else {
            return Err(ActionRefusal::AuthorityDenied {
                reason: "delegated requester requires an authenticated accountable person"
                    .to_owned(),
            });
        };
        check_delegation(delegation, person, call)?;
    }
    if policy.require_independent_from_requester && requester_person.is_none() {
        return Err(ActionRefusal::AuthorityDenied {
            reason: "independent approval requires an authenticated accountable person for the requester"
                .to_owned(),
        });
    }
    let mut seen_ids = BTreeSet::new();
    let mut seen_people = BTreeSet::new();
    let mut counted_people = BTreeSet::new();
    let mut verified_evidence = Vec::with_capacity(approval.evidence.len());

    for item in &approval.evidence {
        let (record, counts) = verify_evidence_item(
            call,
            policy,
            requester_person,
            intent_digest,
            item,
            &mut seen_ids,
            &mut seen_people,
        )?;
        if counts {
            counted_people.insert(record.accountable_person.clone());
        }
        verified_evidence.push(record);
    }

    let distinct_count = u32::try_from(counted_people.len()).unwrap_or(0);
    if distinct_count < policy.required_distinct_people {
        return Err(ActionRefusal::AuthorityDenied {
            reason: format!(
                "independent approval requires {} distinct accountable people, received {}",
                policy.required_distinct_people, distinct_count
            ),
        });
    }

    Ok(ActionApprovalReceipt {
        intent_digest: intent_digest.to_owned(),
        required_distinct_people: policy.required_distinct_people,
        accountable_people: counted_people.into_iter().collect(),
        evidence: verified_evidence,
    })
}

pub(super) fn enforce_or_capture_intent<X>(
    call: &ActionCall<'_>,
    parts: &PreparedIntentParts<'_>,
    capture: Option<&mut Option<PreparedActionIntent>>,
) -> Result<Option<ActionApprovalReceipt>, ActionRefusal<X>> {
    if capture.is_none() && call.approval.is_none() {
        return Ok(None);
    }
    let intent_digest = prepared_intent_digest(call, parts)?;
    if let Some(slot) = capture {
        *slot = Some(PreparedActionIntent {
            intent_digest,
            target: parts.target.clone(),
            effective_input: parts.effective_input.clone(),
            writes: parts.writes.to_vec(),
            links: parts.links.to_vec(),
            commands: parts.commands.to_vec(),
            quantity: parts.quantity.cloned(),
        });
        return Ok(None);
    }
    match &call.approval {
        Some(approval) => verify_independent_approval(call, approval, &intent_digest).map(Some),
        None => Ok(None),
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

/// Prepare an action call's exact intent and digest without mutating `host`.
///
/// The returned [`PreparedActionIntent::intent_digest`] binds the action identity,
/// scope, target, inputs, derived inputs, exact prepared writes (including expected
/// versions), links, commands, and quantity requests.
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
) -> Result<PreparedActionIntent, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    prepare_action_intent_with_limits(host, catalog, call, authorizer, &EnvelopeLimits::V1)
}

/// Prepare an action call's exact intent under explicit per-decision envelope limits.
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
) -> Result<PreparedActionIntent, ActionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let mut readonly = ReadOnlyIntentHost(host);
    let mut captured = None;
    let outcome = super::commit_action_with_capture(
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
        (Some(intent), _) => Ok(intent),
        (None, Err(refusal)) => Err(refusal),
        (None, Ok(_replayed)) => Err(ActionRefusal::IdempotencyConflict {
            key: call.idempotency_key.clone(),
        }),
    }
}
