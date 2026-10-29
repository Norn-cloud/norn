//! `norn-authz`: `AccessIR`→Cedar compile/PDP.
//!
//! Ring 0 — pure, portable semantic core; see `AGENTS.md` for the allowed and
//! forbidden dependencies. `cedar-policy` is confined to this crate and is
//! only ever an authorization engine, never an expression evaluator.
//!
//! # Pipeline
//!
//! 1. **Schema** ([`AuthzSchema`], T5-AZ-1 #2576): the Norn base fragment
//!    ([`NORN_BASE_SCHEMA`]) composed with the fragment `norn-projections`
//!    projects from `EntityIR` v2 + `ActionIR` (T1-PROJ-3 #2530).
//! 2. **Compile + admit at T2** ([`compile`], T5-AZ-2 #2577): typed
//!    [`norn_ir::access::AccessIr`] lowers to Cedar `permit` policies, each
//!    carrying the mandatory structural scope conjunct, then the set is
//!    parsed and strictly validated against the schema. Any finding rejects
//!    the whole document with a stable [`AuthzCode`]. Only compiled policies
//!    are admitted; there is no entry point for hand-written Cedar.
//! 3. **Decide** ([`Pdp`], T5-AZ-3 #2578): the AND rule — capability grants
//!    ([`CapabilityGrant`], compiled to Cedar with `norn-engine`'s exact-match
//!    semantics) ∧ the Biscuit slot ([`Delegation`]; any token denies until
//!    Biscuit lands) ∧ the admitted [`ValidatedPolicySet`]. Any error
//!    denies. The kernel calls it through `norn_kernel::authz`.
//!
//! # Where the PDP runs
//!
//! In the kernel, on every call: the gateway may pre-check, but the kernel
//! repeats authorization (ruling 2026-08-27 §7, retained by 2026-09-25 §16).
//! The kernel is a `wasm32-unknown-unknown`, synchronous ring-0 crate, so this
//! crate and `cedar-policy` (default features off) must build for that target;
//! CI's `wasm32` job builds `norn-authz` for it on every change.

mod admit;
mod base;
mod diag;
mod entitlement;
mod grants;
mod lower;
mod pdp;
mod request;

/// The `AccessIR` input types of [`compile`], re-exported from `norn-ir`.
pub use norn_ir::access;

pub use admit::ValidatedPolicySet;
pub use base::{AuthzSchema, NORN_BASE_SCHEMA};
pub use diag::{AuthzCode, AuthzDiagnostic, AuthzError};
pub use entitlement::{
    EntitlementDenialReason, EntitlementEvaluation, EntitlementMatch, EntitlementRowEvaluation,
    EntitlementRowOutcome, EntitlementValidationError, validate_entitlement,
};
pub use grants::CAPABILITY_SCHEMA;
pub use lower::{
    ACTION_TYPE, ACTOR_TYPE, DECLASSIFY_ACTION_ID, MARKING_RESOURCE_TYPE, REVEAL_ACTION_ID,
    ROLE_TYPE, SCOPE_ATTRIBUTE, compile, compile_all, compile_all_projected,
    compile_all_projected_with_entitlements, infer_principal_attributes,
};
pub use pdp::{ConjunctOutcome, DenyReason, DeterminingPolicy, Pdp, PdpDecision, PdpExplanation};
pub use request::{
    AccessRequest, ActorKind, AttrValue, CapabilityGrant, CommitBinding, Delegation,
    EntitlementCatalog, EntitlementInput, EntityRef, PrincipalInput, ResourceInput, ScopeKey,
};

#[cfg(test)]
mod projected_tests;
#[cfg(test)]
mod tests;
