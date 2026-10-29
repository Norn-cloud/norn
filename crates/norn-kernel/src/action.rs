//! The ring-0 action transition (T2-KER-2a).
//!
//! Pure authority checks extracted from `norn-engine`'s action command
//! validation and its versioned/authorized commit boundary. The engine is now
//! the shell: it reads the relevant slice of its store, calls these functions
//! (`decide`), and applies an accepted decision to storage (`commit`).
//!
//! Authority order (ruling 2026-09-25 §5; 2026-09-09 §4) as the engine has
//! always applied it:
//!
//! 1. **Scope fail-closed** — [`check_action_command`] refuses a command
//!    without an organization scope. Scope *activation* stays a storage fact
//!    checked by the shell's commit.
//! 2. **Authorization** — [`decide_authorized`] fixes where the shell's
//!    authorization verdict is taken (after transition-identity binding and
//!    logical-time monotonicity, before replay or commit). The verdict itself
//!    is supplied by the shell through [`AuthorizedActionShell`]; this crate
//!    never evaluates policy (the PDP stays outside ring 0).
//! 3. **Owner-version CAS** and 4. **idempotency** — [`VersionedAction`], a
//!    [`Decider`] over an [`ActionAuthorityState`] slice.
//! 5. **Ledger + receipt** — an accepted [`ActionTransitionCommitted`] event
//!    carries the resulting owner versions; the shell builds the ledger rows
//!    and receipt digests from it.
//!
//! An exact retry of a committed transition is a replay: it decides no events
//! and the shell returns the recorded receipt.
//!
//! Digests are computed by the shell and enter as opaque strings: this crate
//! does not hash.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;

use norn_ir::identifier_limits::{MAX_ACTION_ID_BYTES, MAX_IDEMPOTENCY_KEY_BYTES};
use serde::{Deserialize, Serialize};

use crate::decider::{Decider, Decision, DecisionOf};
use crate::observations::Observations;

/// Maximum UTF-8 byte length of one action idempotency key.
pub const ACTION_IDEMPOTENCY_KEY_MAX_BYTES: usize = MAX_IDEMPOTENCY_KEY_BYTES;

/// The fields of an action command the kernel admits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionCommandShape<'a> {
    /// The command's organization scope, if any.
    pub org_id: Option<&'a str>,
    /// The action identifier.
    pub action_id: &'a str,
    /// The pinned action version, if any.
    pub action_version: Option<&'a str>,
    /// The caller's idempotency key.
    pub idempotency_key: &'a str,
}

/// Why an action command is not admissible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ActionCommandRejection {
    /// No non-blank organization scope: scope never widens to a wildcard.
    #[error("action command has no organization scope")]
    MissingOrgScope,
    /// A blank action id, a present-but-blank action version, a blank
    /// idempotency key, or a blank admitted action artifact id.
    #[error("action command has a blank identity field")]
    BlankField,
    /// The action identifier exceeds the kernel limit.
    #[error("action id is {actual_bytes} bytes; the limit is {max_bytes} bytes")]
    ActionIdTooLong {
        /// The identifier's UTF-8 byte length.
        actual_bytes: usize,
        /// The permitted UTF-8 byte length.
        max_bytes: usize,
    },
    /// The idempotency key's UTF-8 representation exceeds the kernel limit.
    #[error("action idempotency key is {actual_bytes} bytes; the limit is {max_bytes} bytes")]
    IdempotencyKeyTooLong {
        /// The key's UTF-8 byte length.
        actual_bytes: usize,
        /// The permitted UTF-8 byte length.
        max_bytes: usize,
    },
}

/// Admits an action command: scope first, then its identity fields.
///
/// # Errors
///
/// Returns the first failed check, in the order scope, action-id byte length,
/// idempotency-key byte length, then non-blank action id, version and key.
pub fn check_action_command(
    command: &ActionCommandShape<'_>,
) -> Result<(), ActionCommandRejection> {
    if command.org_id.is_none_or(|org| org.trim().is_empty()) {
        return Err(ActionCommandRejection::MissingOrgScope);
    }
    if command.action_id.len() > MAX_ACTION_ID_BYTES {
        return Err(ActionCommandRejection::ActionIdTooLong {
            actual_bytes: command.action_id.len(),
            max_bytes: MAX_ACTION_ID_BYTES,
        });
    }
    if command.idempotency_key.len() > ACTION_IDEMPOTENCY_KEY_MAX_BYTES {
        return Err(ActionCommandRejection::IdempotencyKeyTooLong {
            actual_bytes: command.idempotency_key.len(),
            max_bytes: ACTION_IDEMPOTENCY_KEY_MAX_BYTES,
        });
    }
    if command.action_id.trim().is_empty()
        || command
            .action_version
            .is_some_and(|version| version.trim().is_empty())
        || command.idempotency_key.trim().is_empty()
    {
        return Err(ActionCommandRejection::BlankField);
    }
    Ok(())
}

/// An optimistic-concurrency expectation on one authority owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerExpectation<O> {
    /// The owner.
    pub owner: O,
    /// The owner version the transition read.
    pub expected_version: u64,
}

/// An owner's version after a committed transition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultingVersion<O> {
    /// The owner.
    pub owner: O,
    /// Its version after the commit.
    pub version: u64,
}

/// The semantic identity a transition id was committed with.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionBinding {
    /// Digest of the complete semantic command.
    pub input_sha256: String,
    /// Digest of the complete effect. Empty for a legacy receipt, which never
    /// authorizes a replay.
    pub effect_sha256: String,
}

impl TransitionBinding {
    /// Whether `request` is an exact retry of this binding.
    #[must_use]
    pub fn admits_replay_of(&self, request: &Self) -> bool {
        self.input_sha256 == request.input_sha256
            && !self.effect_sha256.is_empty()
            && self.effect_sha256 == request.effect_sha256
    }
}

/// The authority slice of one scope a versioned transition decides against.
///
/// The shell fills it from storage for the owners, transition id, and
/// idempotency key one request names; [`VersionedAction::evolve`] keeps it
/// current. An owner absent from `owner_versions` is at version `0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionAuthorityState<O: Ord> {
    /// Current version of each authority owner.
    pub owner_versions: BTreeMap<O, u64>,
    /// Committed versioned-action transitions by transition id.
    pub transitions: BTreeMap<String, TransitionBinding>,
    /// Transition ids already used by another authority protocol.
    pub foreign_transitions: BTreeSet<String>,
    /// Idempotency keys already bound to a committed action.
    pub idempotency_keys: BTreeSet<String>,
}

impl<O: Ord> Default for ActionAuthorityState<O> {
    fn default() -> Self {
        Self {
            owner_versions: BTreeMap::new(),
            transitions: BTreeMap::new(),
            foreign_transitions: BTreeSet::new(),
            idempotency_keys: BTreeSet::new(),
        }
    }
}

impl<O: Ord> ActionAuthorityState<O> {
    /// The current version of `owner`.
    #[must_use]
    pub fn owner_version(&self, owner: &O) -> u64 {
        self.owner_versions.get(owner).copied().unwrap_or(0)
    }
}

/// A versioned action transition request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionedActionRequest<O: Ord> {
    /// Durable transition identity.
    pub transition_id: String,
    /// Digests binding the semantic command and effect.
    pub binding: TransitionBinding,
    /// Expected owner versions: every mutated owner plus optional guards.
    pub preconditions: Vec<OwnerExpectation<O>>,
    /// Owners the effect mutates.
    pub mutation_owners: BTreeSet<O>,
    /// The action's idempotency key.
    pub idempotency_key: String,
}

/// The ledger intent of an accepted versioned action transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTransitionCommitted<O> {
    /// Durable transition identity.
    pub transition_id: String,
    /// Digests the transition is bound to.
    pub binding: TransitionBinding,
    /// The idempotency key now bound to this action.
    pub idempotency_key: String,
    /// Mutated owners and their new versions, in owner order.
    pub resulting_versions: Vec<ResultingVersion<O>>,
}

/// Why a versioned action transition is refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ActionRejection {
    /// Blank transition id, input digest, or (in a request) effect digest.
    #[error("transition id and digests must be non-empty")]
    InvalidTransitionIdentity,
    /// The transition id belongs to another authority protocol.
    #[error("transition '{transition_id}' is already used by a different authority protocol")]
    TransitionUsedByOtherProtocol {
        /// The transition id.
        transition_id: String,
    },
    /// The transition id was committed with different input or effects.
    #[error(
        "transition '{transition_id}' was already committed with different semantic input or effects"
    )]
    TransitionBindingMismatch {
        /// The transition id.
        transition_id: String,
    },
    /// An owner appears twice in the preconditions.
    #[error("an authority owner may appear only once in preconditions")]
    DuplicatePrecondition,
    /// A mutated owner has no expected version.
    #[error("every mutated authority owner requires an expected version")]
    MissingPrecondition,
    /// Compare-and-swap failed on an owner version.
    #[error("authority owner version conflict: expected {expected}, current {current}")]
    OwnerVersionConflict {
        /// The version the transition read.
        expected: u64,
        /// The version now current.
        current: u64,
    },
    /// The idempotency key is bound to an action without this transition.
    #[error("action idempotency identity exists without the matching versioned transition receipt")]
    IdempotencyWithoutReceipt,
    /// An owner version would overflow.
    ///
    /// Lowest precedence: every other check here passed. A host that runs its
    /// own commit validation after `decide` (as `norn-engine` does) reports
    /// that validation's failures ahead of this one.
    #[error("authority owner version overflow")]
    OwnerVersionOverflow,
}

/// Checks a transition's durable identity.
///
/// # Errors
///
/// [`ActionRejection::InvalidTransitionIdentity`] when either part is blank.
pub fn check_transition_identity(
    transition_id: &str,
    input_sha256: &str,
) -> Result<(), ActionRejection> {
    if transition_id.trim().is_empty() || input_sha256.trim().is_empty() {
        Err(ActionRejection::InvalidTransitionIdentity)
    } else {
        Ok(())
    }
}

/// What a versioned action transition decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionedOutcome<O> {
    /// An exact retry: return the recorded receipt, change nothing.
    Replay,
    /// A new transition: record this ledger intent.
    Commit(ActionTransitionCommitted<O>),
}

/// Decides one versioned action transition against `state`.
///
/// Order: identity (including a non-empty effect digest); transition-id
/// protocol ownership; exact replay; duplicate preconditions; every mutated
/// owner has a precondition; CAS on every precondition in owner order;
/// idempotency key unbound; resulting versions.
///
/// # Errors
///
/// The first failed check as an [`ActionRejection`].
pub fn decide_versioned<O: Ord + Clone>(
    state: &ActionAuthorityState<O>,
    request: &VersionedActionRequest<O>,
) -> Result<VersionedOutcome<O>, ActionRejection> {
    check_transition_identity(&request.transition_id, &request.binding.input_sha256)?;
    // A new commit must bind its effect: an empty effect digest is only ever
    // tolerated in recorded legacy state, where it refuses replay.
    if request.binding.effect_sha256.trim().is_empty() {
        return Err(ActionRejection::InvalidTransitionIdentity);
    }
    if state.foreign_transitions.contains(&request.transition_id) {
        return Err(ActionRejection::TransitionUsedByOtherProtocol {
            transition_id: request.transition_id.clone(),
        });
    }
    if let Some(existing) = state.transitions.get(&request.transition_id) {
        return if existing.admits_replay_of(&request.binding) {
            Ok(VersionedOutcome::Replay)
        } else {
            Err(ActionRejection::TransitionBindingMismatch {
                transition_id: request.transition_id.clone(),
            })
        };
    }

    let mut expectations = BTreeMap::new();
    for expectation in &request.preconditions {
        if expectations
            .insert(&expectation.owner, expectation.expected_version)
            .is_some()
        {
            return Err(ActionRejection::DuplicatePrecondition);
        }
    }
    if request
        .mutation_owners
        .iter()
        .any(|owner| !expectations.contains_key(owner))
    {
        return Err(ActionRejection::MissingPrecondition);
    }
    for (owner, expected) in &expectations {
        let current = state.owner_version(owner);
        if current != *expected {
            return Err(ActionRejection::OwnerVersionConflict {
                expected: *expected,
                current,
            });
        }
    }

    if state.idempotency_keys.contains(&request.idempotency_key) {
        return Err(ActionRejection::IdempotencyWithoutReceipt);
    }

    let resulting_versions = request
        .mutation_owners
        .iter()
        .map(|owner| {
            state
                .owner_version(owner)
                .checked_add(1)
                .map(|version| ResultingVersion {
                    owner: owner.clone(),
                    version,
                })
                .ok_or(ActionRejection::OwnerVersionOverflow)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(VersionedOutcome::Commit(ActionTransitionCommitted {
        transition_id: request.transition_id.clone(),
        binding: request.binding.clone(),
        idempotency_key: request.idempotency_key.clone(),
        resulting_versions,
    }))
}

/// Applies a committed transition to the authority slice.
#[must_use]
pub fn evolve_versioned<O: Ord + Clone>(
    state: &ActionAuthorityState<O>,
    event: &ActionTransitionCommitted<O>,
) -> ActionAuthorityState<O> {
    let mut next = state.clone();
    for resulting in &event.resulting_versions {
        next.owner_versions
            .insert(resulting.owner.clone(), resulting.version);
    }
    next.transitions
        .insert(event.transition_id.clone(), event.binding.clone());
    next.idempotency_keys.insert(event.idempotency_key.clone());
    next
}

/// The versioned action transition as a [`Decider`].
///
/// `decide` is [`decide_versioned`]: a replay decides no events, a commit
/// decides one [`ActionTransitionCommitted`]. No effects: external commands
/// are T2-KER-2b.
pub struct VersionedAction<O>(PhantomData<fn() -> O>);

impl<O> VersionedAction<O> {
    /// The decider.
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<O> Default for VersionedAction<O> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O> Clone for VersionedAction<O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<O> Copy for VersionedAction<O> {}

impl<O> fmt::Debug for VersionedAction<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VersionedAction")
    }
}

impl<O: Ord + Clone> Decider for VersionedAction<O> {
    type State = ActionAuthorityState<O>;
    type Input = VersionedActionRequest<O>;
    type Event = ActionTransitionCommitted<O>;
    type Command = Infallible;
    type Rejection = ActionRejection;

    fn initial_state(&self) -> Self::State {
        ActionAuthorityState::default()
    }

    fn decide(
        &self,
        state: &Self::State,
        input: &Self::Input,
        _observations: &Observations,
    ) -> Result<DecisionOf<Self>, Self::Rejection> {
        Ok(match decide_versioned(state, input)? {
            VersionedOutcome::Replay => Decision::none(),
            VersionedOutcome::Commit(event) => Decision::events(vec![event]),
        })
    }

    fn evolve(&self, state: &Self::State, event: &Self::Event) -> Self::State {
        evolve_versioned(state, event)
    }
}

/// The identity an authorized transition id was committed with.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedBinding {
    /// Digest of the complete authorized command.
    pub authorization_input_sha256: String,
    /// The underlying versioned transition binding.
    pub transition: TransitionBinding,
}

impl AuthorizedBinding {
    /// Whether `request` is an exact retry of this binding.
    ///
    /// Unlike [`TransitionBinding::admits_replay_of`], an empty recorded
    /// effect digest is compared as-is: strong receipts always carry one.
    #[must_use]
    pub fn matches(&self, request: &Self) -> bool {
        self.authorization_input_sha256 == request.authorization_input_sha256
            && self.transition.input_sha256 == request.transition.input_sha256
            && self.transition.effect_sha256 == request.transition.effect_sha256
    }
}

/// What the kernel knows of one scope when deciding an authorized transition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedActionState {
    /// The strong receipt already recorded for this transition id, if any.
    pub existing: Option<AuthorizedBinding>,
    /// Whether the authorization-authority protocol uses this transition id.
    pub used_by_authority_protocol: bool,
    /// Whether a legacy (unauthorized) versioned receipt uses this id.
    pub used_by_versioned_protocol: bool,
    /// Logical time of the scope's last authorized commit.
    pub last_commit_logical_time: Option<u64>,
}

/// An authorized action transition request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedActionRequest {
    /// Durable transition identity.
    pub transition_id: String,
    /// Digests binding the authorized command.
    pub binding: AuthorizedBinding,
    /// Trusted logical time injected by the sequencer.
    pub commit_logical_time: u64,
}

/// The checks only the shell can perform, taken at kernel-chosen points.
///
/// Authorization is supplied, not evaluated, by ring 0: the policy decision
/// point lives in the shell (today `norn-engine`'s commit authorization).
pub trait AuthorizedActionShell {
    /// The shell's error, passed through unchanged.
    type Error;
    /// Whatever the shell's authorization produces (its receipt evidence).
    type Authorized;

    /// Verifies the integrity of the recorded receipt before it is replayed.
    ///
    /// # Errors
    ///
    /// The shell's integrity failure.
    fn verify_recorded_receipt(&mut self) -> Result<(), Self::Error>;

    /// Evaluates authorization for the request under current authority.
    ///
    /// # Errors
    ///
    /// The shell's denial or failure.
    fn authorize(&mut self) -> Result<Self::Authorized, Self::Error>;
}

/// Why an authorized action transition is refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub enum AuthorizedRejection<X> {
    /// The transition id was committed with a different authorized command.
    #[error(
        "authorized transition '{transition_id}' was already committed with different semantic input, authorization, or effects"
    )]
    BindingMismatch {
        /// The transition id.
        transition_id: String,
    },
    /// The transition id belongs to the authorization-authority protocol.
    #[error("transition '{transition_id}' is already used by the authorization authority protocol")]
    UsedByAuthorityProtocol {
        /// The transition id.
        transition_id: String,
    },
    /// The transition id has only a legacy versioned receipt.
    #[error("a legacy versioned action receipt cannot authorize a strong commit replay")]
    LegacyVersionedReceipt,
    /// The logical time moved backwards for the scope.
    #[error("commit_authorization.logical_time_rollback")]
    LogicalTimeRollback,
    /// A shell check failed.
    #[error("shell check failed")]
    Shell(X),
}

/// What an authorized action transition decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizedOutcome<A> {
    /// An exact, re-authorized retry: return the recorded receipt unchanged.
    Replay,
    /// A new transition, authorized; proceed to the versioned transition.
    Proceed(A),
}

/// Decides the authorization stage of one authorized action transition.
///
/// Order: exact binding of a recorded receipt (then its integrity, via the
/// shell); protocol ownership of the transition id; logical-time
/// monotonicity; authorization (via the shell). Idempotency never grants
/// continuing access: a replay is returned only after authorization is
/// re-established.
///
/// # Errors
///
/// The first failed check as an [`AuthorizedRejection`].
pub fn decide_authorized<S: AuthorizedActionShell>(
    state: &AuthorizedActionState,
    request: &AuthorizedActionRequest,
    shell: &mut S,
) -> Result<AuthorizedOutcome<S::Authorized>, AuthorizedRejection<S::Error>> {
    let replay = if let Some(existing) = &state.existing {
        if !existing.matches(&request.binding) {
            return Err(AuthorizedRejection::BindingMismatch {
                transition_id: request.transition_id.clone(),
            });
        }
        shell
            .verify_recorded_receipt()
            .map_err(AuthorizedRejection::Shell)?;
        true
    } else {
        if state.used_by_authority_protocol {
            return Err(AuthorizedRejection::UsedByAuthorityProtocol {
                transition_id: request.transition_id.clone(),
            });
        }
        if state.used_by_versioned_protocol {
            return Err(AuthorizedRejection::LegacyVersionedReceipt);
        }
        false
    };

    if state
        .last_commit_logical_time
        .is_some_and(|last| request.commit_logical_time < last)
    {
        return Err(AuthorizedRejection::LogicalTimeRollback);
    }

    let authorized = shell.authorize().map_err(AuthorizedRejection::Shell)?;
    Ok(if replay {
        AuthorizedOutcome::Replay
    } else {
        AuthorizedOutcome::Proceed(authorized)
    })
}
