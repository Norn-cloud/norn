//! K7/K5 receipt binding decisions, the pure core of
//! `receipt_binding::bind_receipt` (RECEIPT-AUTH-BIND-1, RECEIPT-CONSUME-1) and
//! of the commit pins an action re-checks (RUN-BIND-1).
//!
//! This file compiles standalone (core only): Charon extracts it with
//! `--start-from` and `verification/kernel-proof-v1/Binding` proves it. Store
//! resolution, receipt digests (`content_digest`, `result_identity`) and
//! summary decoding stay glue in `receipt_binding`.

/// The reconciled acceptance a command's state records.
pub(crate) struct AcceptedPins<D> {
    /// The accepted receipt's digest.
    pub(crate) receipt: D,
    /// The accepted result identity.
    pub(crate) result_identity: D,
    /// The generation it was accepted under.
    pub(crate) generation: u64,
}

/// A command-result delivery: the only delivery a receipt may consume.
pub(crate) struct ConsumedResult<C> {
    /// The command whose result is delivered.
    pub(crate) command: C,
    /// The generation it was performed under.
    pub(crate) generation: u64,
}

/// Everything `bind_receipt` compares: the requested reference, the
/// authenticated receipt, the persisted result record, and the command state.
pub(crate) struct BindingPins<D, C> {
    /// The digest the action's input names.
    pub(crate) requested: D,
    /// The receipt's content digest.
    pub(crate) receipt_digest: D,
    /// The receipt's result identity.
    pub(crate) receipt_result_identity: D,
    /// The receipt's command.
    pub(crate) receipt_command: C,
    /// The receipt's run generation.
    pub(crate) receipt_generation: u64,
    /// The receipt's attempt.
    pub(crate) receipt_attempt: u32,
    /// The receipt digest the result record pins.
    pub(crate) record_receipt: D,
    /// The result identity the result record pins.
    pub(crate) record_result_identity: D,
    /// The result record's command.
    pub(crate) record_command: C,
    /// The result record's generation.
    pub(crate) record_generation: u64,
    /// The result record's attempt.
    pub(crate) record_attempt: u32,
    /// The command state's current generation.
    pub(crate) state_generation: u64,
    /// Whether the command was cancelled.
    pub(crate) state_cancelled: bool,
    /// The delivery the acceptance consumes, when it is a command result.
    pub(crate) consumed: Option<ConsumedResult<C>>,
    /// The command state's accepted result, if any.
    pub(crate) accepted: Option<AcceptedPins<D>>,
}

/// RECEIPT-AUTH-BIND-1: the receipt binds only when the requested, receipt,
/// record and accepted digests agree, the result identities agree, command,
/// generation and attempt agree, the command is not cancelled, and the
/// delivery consumed is this command's result at this generation
/// (RECEIPT-CONSUME-1).
pub(crate) fn pins_bind<D: PartialEq, C: PartialEq>(pins: &BindingPins<D, C>) -> bool {
    let delivery_is_result = match &pins.consumed {
        Some(consumed) => {
            consumed.command == pins.record_command && consumed.generation == pins.record_generation
        }
        None => false,
    };
    let state_accepted = match &pins.accepted {
        Some(accepted) => {
            accepted.receipt == pins.receipt_digest
                && accepted.result_identity == pins.receipt_result_identity
                && accepted.generation == pins.record_generation
        }
        None => false,
    };
    pins.receipt_digest == pins.requested
        && pins.receipt_digest == pins.record_receipt
        && pins.receipt_result_identity == pins.record_result_identity
        && pins.record_command == pins.receipt_command
        && pins.record_generation == pins.receipt_generation
        && pins.record_attempt == pins.receipt_attempt
        && pins.state_generation == pins.record_generation
        && delivery_is_result
        && !pins.state_cancelled
        && state_accepted
}

/// The pins an action checks on a bound receipt before consuming it.
pub(crate) struct CommitPins<D, R> {
    /// The manifest the receipt executed.
    pub(crate) manifest: D,
    /// The manifest the action's acceptance declares.
    pub(crate) declared_manifest: D,
    /// The run the receipt was dispatched under.
    pub(crate) run: R,
    /// The run the action targets.
    pub(crate) target: R,
    /// The receipt's run generation.
    pub(crate) run_generation: u64,
    /// The result record's generation.
    pub(crate) record_generation: u64,
    /// The command state's generation.
    pub(crate) state_generation: u64,
}

/// Why a bound receipt does not bind the action's commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitPinRefusal {
    /// The receipt executed another manifest than the one declared.
    ManifestMismatch,
    /// The receipt belongs to another run or generation.
    RunMismatch,
}

/// RUN-BIND-1: a receipt binds exactly its commit. The manifest is checked
/// first, then the run, then the record's and state's generations.
pub(crate) fn commit_pins_bind<D: PartialEq, R: PartialEq>(
    pins: &CommitPins<D, R>,
) -> Result<(), CommitPinRefusal> {
    let manifest_bound = pins.manifest == pins.declared_manifest;
    if !manifest_bound {
        return Err(CommitPinRefusal::ManifestMismatch);
    }
    let run_bound = pins.run == pins.target;
    if !run_bound {
        return Err(CommitPinRefusal::RunMismatch);
    }
    let generation_bound = pins.record_generation == pins.run_generation
        && pins.state_generation == pins.run_generation;
    if !generation_bound {
        return Err(CommitPinRefusal::RunMismatch);
    }
    Ok(())
}
