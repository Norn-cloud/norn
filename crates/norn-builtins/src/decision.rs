//! `norn.decision@1`: ask a person (DECISION-1, Core 07; D58).
//!
//! A flow lowers "ask" to `perform` of this Function, followed by `route` on
//! `choice`. The run waits in `awaiting_perform_<step>` with the command
//! pending. Nothing outside the cell can complete it. The cell resolves the
//! command by issuing the receipt itself ([`issue`], [`expire`],
//! [`withdraw`]) and delivering it as the perform's result; [`IssuedOnly`]
//! makes reconciliation refuse any other receipt for this manifest. The
//! receipt's `provenance.executor` is the deciding Actor.
//!
//! Inputs travel as one canonical-JSON blob named [`REQUEST_BLOB`], because
//! Function inputs are scalars. The closed schemas cannot express length,
//! count or pattern bounds, so [`parse_request`] checks them.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::RuntimeValue;
use norn_ir::function::{
    AuthorityKind, CacheOrigin, Digest, ExecutionProvenance, FailureOutcome, FunctionInvocation,
    FunctionManifest, FunctionOutcome, FunctionReceipt, ResourceUse, SuccessOutcome,
};
use serde::{Deserialize, Serialize};

/// The Function id.
pub const FUNCTION_ID: &str = "norn.decision";
/// The Function version.
pub const VERSION: u32 = 1;
/// The input schema document (closed-schema subset).
pub const INPUT_SCHEMA: &str = include_str!("schemas/decision.input.json");
/// The output schema document.
pub const OUTPUT_SCHEMA: &str = include_str!("schemas/decision.output.json");
/// The typed error schema document (`{_tag: "Expired" | "Withdrawn"}`).
pub const ERROR_SCHEMA: &str = include_str!("schemas/decision.error.json");
/// The blob that carries the canonical-JSON request.
pub const REQUEST_BLOB: &str = "request";
/// The longest question and note, in characters.
pub const MAX_TEXT_CHARS: usize = 500;
/// The longest option id, in characters (ASCII, so also bytes).
pub const MAX_OPTION_ID_CHARS: usize = 64;
/// The most options a decision offers.
pub const MAX_OPTIONS: usize = 9;
/// The largest request blob, in bytes: the registered manifest's
/// `max_input_bytes`. [`parse_request`] and every issuance path refuse a
/// larger request before parsing or hashing it.
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// The longest a decision may wait: seven days.
pub const MAX_EXPIRES_IN_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Where the cell issues the receipt.
pub const PLACEMENT: &str = "norn.cell";

/// How much is at stake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
}

/// What choosing an option does: exactly one of an action or a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OptionEffect {
    /// The action the option starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The binding the option sets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
}

/// One option a person may choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionOption {
    /// `[a-z][a-z0-9_]*`, at most [`MAX_OPTION_ID_CHARS`] characters, unique
    /// within the decision; `choice` names it.
    pub id: String,
    /// What the decider reads.
    pub label: String,
    /// What it does.
    pub effect: OptionEffect,
    /// How far it reaches, in the decider's words.
    pub scope: String,
}

/// Who may decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decider {
    /// The deciding Actor holds at least one of these roles.
    pub roles: Vec<String>,
    /// A law the decision must satisfy for the deciding Actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub law: Option<String>,
}

/// The decision's input (a closed record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionRequest {
    /// The question, at most [`MAX_TEXT_CHARS`] characters.
    pub question: String,
    /// The risk.
    pub risk: Risk,
    /// One to [`MAX_OPTIONS`] options.
    pub options: Vec<DecisionOption>,
    /// An agent's recommended option id; a recommendation, never a decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pick: Option<String>,
    /// Who may decide.
    pub decider: Decider,
    /// How long it may wait, at most [`MAX_EXPIRES_IN_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_ms: Option<u64>,
}

/// The decider's answer (the output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    /// One of the request's option ids.
    pub choice: String,
    /// A note, at most [`MAX_TEXT_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What the cell knows about the deciding Actor, resolved on the server from
/// the verified caller. The client never supplies it.
#[derive(Debug, Clone, Copy)]
pub struct DeciderEvidence<'a> {
    /// The verified caller.
    pub actor: &'a norn_ir::function::Authority,
    /// The roles the caller holds in this scope.
    pub roles: &'a BTreeSet<String>,
    /// The laws the server evaluated as satisfied for this decision.
    pub satisfied_laws: &'a BTreeSet<String>,
}

/// Why the cell does not issue a decision receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionRefusal {
    /// The request is not a valid `norn.decision@1` input; the code names
    /// the bound it breaks.
    InvalidRequest(&'static str),
    /// The command is not a `norn.decision@1` dispatch of this manifest, or
    /// its request blob is not these bytes.
    NotThisCommand,
    /// The caller is not an Actor holding one of the decider roles.
    NotADecider,
    /// The decider law was not satisfied for this caller.
    LawNotSatisfied,
    /// `choice` names no option.
    UnknownChoice,
    /// The note is longer than [`MAX_TEXT_CHARS`].
    NoteTooLong,
    /// The answer's summary would exceed the receipt summary bound. The id
    /// and note bounds keep every accepted choice far below it; this check
    /// keeps that true if a bound moves.
    SummaryTooLarge,
    /// The decision's deadline passed; only [`expire`] can resolve it now.
    Expired,
    /// The deadline has not passed, so it cannot expire yet.
    NotExpired,
    /// Canonical encoding failed.
    Canonical,
}

impl DecisionRefusal {
    /// Stable refusal code; a law-based refusal names its law family.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(code) => code,
            Self::NotThisCommand => "decision.not_this_command",
            Self::NotADecider => "decision.not_a_decider",
            Self::LawNotSatisfied => "decision.law_not_satisfied",
            Self::UnknownChoice => "decision.unknown_choice",
            Self::NoteTooLong => "decision.note_too_long",
            Self::SummaryTooLarge => "decision.summary_too_large",
            Self::Expired => "decision.expired",
            Self::NotExpired => "decision.not_expired",
            Self::Canonical => "decision.canonical",
        }
    }
}

impl std::fmt::Display for DecisionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for DecisionRefusal {}

/// A receipt the cell issued for a decision. Only [`issue`], [`expire`] and
/// [`withdraw`] construct one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedReceipt {
    receipt: FunctionReceipt,
    digest: Digest,
}

impl IssuedReceipt {
    /// The receipt.
    #[must_use]
    pub const fn receipt(&self) -> &FunctionReceipt {
        &self.receipt
    }

    /// Its content digest (`FunctionReceipt::content_digest`, `RECEIPT_DOMAIN`).
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

fn too_large(request: &[u8]) -> bool {
    !u64::try_from(request.len()).is_ok_and(|len| len <= MAX_REQUEST_BYTES)
}

fn is_option_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_OPTION_ID_CHARS
        && chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Parse and bound-check a request.
///
/// # Errors
/// [`DecisionRefusal::InvalidRequest`] naming the broken bound.
pub fn parse_request(bytes: &[u8]) -> Result<DecisionRequest, DecisionRefusal> {
    let invalid = DecisionRefusal::InvalidRequest;
    if too_large(bytes) {
        return Err(invalid("decision.request_too_large"));
    }
    let request: DecisionRequest =
        serde_json::from_slice(bytes).map_err(|_shape| invalid("decision.request_shape"))?;
    if request.question.trim().is_empty() || request.question.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid("decision.question_length"));
    }
    if request.options.is_empty() || request.options.len() > MAX_OPTIONS {
        return Err(invalid("decision.option_count"));
    }
    let mut ids = BTreeSet::new();
    for option in &request.options {
        if !is_option_id(&option.id) || !ids.insert(option.id.as_str()) {
            return Err(invalid("decision.option_id"));
        }
        if option.effect.action.is_some() == option.effect.binding.is_some() {
            return Err(invalid("decision.option_effect"));
        }
    }
    if request
        .agent_pick
        .as_deref()
        .is_some_and(|pick| !ids.contains(pick))
    {
        return Err(invalid("decision.agent_pick"));
    }
    if request.decider.roles.is_empty() || request.decider.roles.iter().any(String::is_empty) {
        return Err(invalid("decision.decider_roles"));
    }
    if request
        .expires_in_ms
        .is_some_and(|ms| ms == 0 || ms > MAX_EXPIRES_IN_MS)
    {
        return Err(invalid("decision.expires_in_ms"));
    }
    Ok(request)
}

/// Check `invocation` is a dispatch of `manifest` carrying exactly `request`.
fn bind(
    manifest: &FunctionManifest,
    invocation: &FunctionInvocation,
    request: &[u8],
) -> Result<(DecisionRequest, Digest), DecisionRefusal> {
    if too_large(request) {
        return Err(DecisionRefusal::InvalidRequest(
            "decision.request_too_large",
        ));
    }
    let digest = manifest
        .content_digest()
        .map_err(|_canonical| DecisionRefusal::Canonical)?;
    let blob = invocation
        .blobs
        .get(REQUEST_BLOB)
        .ok_or(DecisionRefusal::NotThisCommand)?;
    let request_digest =
        Digest::of_json_document(request).map_err(|_canonical| DecisionRefusal::NotThisCommand)?;
    if manifest.function_id != FUNCTION_ID
        || manifest.version != VERSION
        || invocation.manifest != digest
        || blob.digest != request_digest
        || u64::try_from(request.len()).ok() != Some(blob.size_bytes)
        || invocation.blobs.len() != 1
        || !invocation.inputs.is_empty()
    {
        return Err(DecisionRefusal::NotThisCommand);
    }
    Ok((parse_request(request)?, digest))
}

fn receipt(
    manifest: &FunctionManifest,
    manifest_digest: Digest,
    invocation: &FunctionInvocation,
    executor: &str,
    outcome: FunctionOutcome,
) -> Result<IssuedReceipt, DecisionRefusal> {
    let canonical = |_error| DecisionRefusal::Canonical;
    let receipt = FunctionReceipt {
        command_id: invocation.command_id.clone(),
        run: invocation.run.clone(),
        attempt: 1,
        dispatch: invocation.content_digest().map_err(canonical)?,
        manifest: manifest_digest,
        execution_profile: manifest.execution_profile_digest().map_err(canonical)?,
        input_identity: invocation.input_identity().map_err(canonical)?,
        dependency_token: invocation.dependency.digest().map_err(canonical)?,
        outcome,
        provenance: ExecutionProvenance {
            executor: executor.to_owned(),
            placement: PLACEMENT.to_owned(),
            runtime: manifest.runtime.digest.clone(),
        },
        resources: ResourceUse {
            cpu_millis: 0,
            wall_millis: 0,
            peak_memory_bytes: 0,
            output_bytes: 0,
            fuel: None,
        },
        cache: CacheOrigin::Miss {},
    };
    let digest = receipt.content_digest().map_err(canonical)?;
    Ok(IssuedReceipt { receipt, digest })
}

/// Issue the receipt for the deciding Actor's answer.
///
/// # Errors
/// The command or request does not bind; the deadline passed; the caller is
/// not an Actor with a decider role, or the decider law is unsatisfied; the
/// choice is unknown or the note too long.
pub fn issue(
    manifest: &FunctionManifest,
    invocation: &FunctionInvocation,
    request: &[u8],
    decider: &DeciderEvidence<'_>,
    decision: &Decision,
    now_ms: u64,
) -> Result<IssuedReceipt, DecisionRefusal> {
    let (request, manifest_digest) = bind(manifest, invocation, request)?;
    if now_ms > invocation.deadline_millis {
        return Err(DecisionRefusal::Expired);
    }
    if decider.actor.kind != AuthorityKind::Actor
        || decider.actor.id.is_empty()
        || !request
            .decider
            .roles
            .iter()
            .any(|role| decider.roles.contains(role))
    {
        return Err(DecisionRefusal::NotADecider);
    }
    if request
        .decider
        .law
        .as_ref()
        .is_some_and(|law| !decider.satisfied_laws.contains(law))
    {
        return Err(DecisionRefusal::LawNotSatisfied);
    }
    if !request
        .options
        .iter()
        .any(|option| option.id == decision.choice)
    {
        return Err(DecisionRefusal::UnknownChoice);
    }
    let mut summary = BTreeMap::from([(
        "choice".to_owned(),
        RuntimeValue::String(decision.choice.clone()),
    )]);
    if let Some(note) = &decision.note {
        if note.chars().count() > MAX_TEXT_CHARS {
            return Err(DecisionRefusal::NoteTooLong);
        }
        summary.insert("note".to_owned(), RuntimeValue::String(note.clone()));
    }
    let size = norn_ir::canonical::canonical_json_of(&summary)
        .map_err(|_canonical| DecisionRefusal::Canonical)?
        .len();
    if !u64::try_from(size).is_ok_and(|size| size <= norn_ir::function::MAX_RECEIPT_SUMMARY_BYTES) {
        return Err(DecisionRefusal::SummaryTooLarge);
    }
    let output_manifest = norn_ir::function::output_manifest_digest(&[], &summary)
        .map_err(|_canonical| DecisionRefusal::Canonical)?;
    receipt(
        manifest,
        manifest_digest,
        invocation,
        &decider.actor.id,
        FunctionOutcome::Success(SuccessOutcome {
            output_manifest,
            outputs: Vec::new(),
            summary,
        }),
    )
}

fn failure(tag: &str) -> FunctionOutcome {
    FunctionOutcome::Failure(FailureOutcome {
        code: tag.to_owned(),
        detail: BTreeMap::from([("_tag".to_owned(), RuntimeValue::String(tag.to_owned()))]),
    })
}

/// Issue the `Expired` receipt once the command's deadline passed.
///
/// # Errors
/// The command does not bind, or the deadline has not passed.
pub fn expire(
    manifest: &FunctionManifest,
    invocation: &FunctionInvocation,
    request: &[u8],
    now_ms: u64,
) -> Result<IssuedReceipt, DecisionRefusal> {
    let (_request, manifest_digest) = bind(manifest, invocation, request)?;
    if now_ms <= invocation.deadline_millis {
        return Err(DecisionRefusal::NotExpired);
    }
    receipt(
        manifest,
        manifest_digest,
        invocation,
        PLACEMENT,
        failure("Expired"),
    )
}

/// Issue the `Withdrawn` receipt for `withdrawer`, whom the caller
/// authorized, while the decision is still open: after the deadline only
/// [`expire`] resolves it.
///
/// # Errors
/// The command does not bind, or the deadline passed.
pub fn withdraw(
    manifest: &FunctionManifest,
    invocation: &FunctionInvocation,
    request: &[u8],
    withdrawer: &norn_ir::function::Authority,
    now_ms: u64,
) -> Result<IssuedReceipt, DecisionRefusal> {
    let (_request, manifest_digest) = bind(manifest, invocation, request)?;
    if now_ms > invocation.deadline_millis {
        return Err(DecisionRefusal::Expired);
    }
    receipt(
        manifest,
        manifest_digest,
        invocation,
        &withdrawer.id,
        failure("Withdrawn"),
    )
}

/// DECISION-ISSUED-1: for the `norn.decision@1` manifest, reconciliation
/// accepts only receipts the cell issued. Other manifests go to `inner`.
pub struct IssuedOnly<'a, R: ?Sized> {
    /// The `norn.decision@1` manifest digest.
    pub manifest: &'a Digest,
    /// The digests of receipts the cell issued and retained.
    pub issued: &'a BTreeSet<Digest>,
    /// The ordinary receipt contract.
    pub inner: &'a R,
}

impl<R: norn_kernel::result_reconciliation::ReceiptValidator + ?Sized>
    norn_kernel::result_reconciliation::ReceiptValidator for IssuedOnly<'_, R>
{
    fn validate(
        &self,
        dispatched: &norn_kernel::result_reconciliation::DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), norn_kernel::result_reconciliation::InvalidReceipt> {
        if &dispatched.invocation.manifest != self.manifest && &receipt.manifest != self.manifest {
            return self.inner.validate(dispatched, receipt);
        }
        let issued = receipt
            .content_digest()
            .is_ok_and(|digest| self.issued.contains(&digest));
        if !issued || receipt.provenance.placement != PLACEMENT {
            return Err(norn_kernel::result_reconciliation::InvalidReceipt {
                reason: "decision.receipt_not_cell_issued".to_owned(),
            });
        }
        self.inner.validate(dispatched, receipt)
    }
}
