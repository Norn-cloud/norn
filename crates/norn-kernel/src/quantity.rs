//! Pure bounded quantity admission. Hosts load only touched normalized heads
//! and apply this plan inside their existing commit transaction.
use std::collections::{BTreeMap, BTreeSet};

use norn_checker::quantity_ledger::{compile_ledger_pack, evaluate_ledger_template};
use norn_ir::quantity_ledger::{
    EvaluatedLedgerLeg, LedgerPackIr, QUANTITY_CELL_VERSION, QuantityOperation, QuantityRequest,
    QuantityResolution,
};
use serde::{Deserialize, Serialize};

/// Bound on one pinned vector and its balance reads.
pub const MAX_QUANTITY_LEGS: usize = 64;
/// Maximum definitions of each unit, layer, template, parameter and affine term.
pub const MAX_QUANTITY_PACK_MEMBERS: usize = 16;
/// Maximum UTF-8 bytes in any pack identifier, ontology pin or version string.
pub const MAX_QUANTITY_IDENTIFIER_BYTES: usize = 128;
/// Longest signed i128 decimal encoding, including its sign.
pub const MAX_QUANTITY_INTEGER_BYTES: usize = 40;

/// Typed refusal; every refusal leaves the whole cell transaction unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuantityError {
    /// Malformed, unsupported or unbounded input.
    #[error("invalid quantity contract")]
    InvalidContract,
    /// Exact arithmetic cannot be represented by i128 (including MIN negation).
    #[error("quantity arithmetic overflow")]
    Overflow,
    /// A balance partition revision changed.
    #[error("stale quantity revision: expected {expected}, current {current}")]
    StaleVersion {
        /// Requested revision.
        expected: u64,
        /// Stored revision.
        current: u64,
    },
    /// A nonnegative account would spend quantity held or absent.
    #[error("insufficient available quantity for {account}")]
    InsufficientAvailable {
        /// Pack-local account identity.
        account: String,
    },
    /// Trusted time reached the exclusive deadline.
    #[error("quantity reservation expired")]
    Expired,
    /// Expiry requested before the deadline.
    #[error("quantity reservation is not expired")]
    NotExpired,
    /// A missing or already resolved reservation.
    #[error("quantity reservation is missing or terminal")]
    Terminal,
    /// Transaction/reservation identity already exists.
    #[error("quantity identity already exists")]
    Duplicate,
    /// Missing original, repeated reversal, or reversal of a reversal.
    #[error("invalid quantity reversal")]
    InvalidReversal,
    /// Injected trusted time moved behind the last committed quantity turn.
    #[error("quantity clock regressed")]
    ClockRegression,
    /// Persisted state violates the pinned contract.
    #[error("corrupt quantity state")]
    Corrupt,
}

/// Normalized balance partition; accounts can identify lots, owners or transit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityKey {
    /// Pack-local account/lot identity.
    pub account: String,
    /// Opaque unit identifier.
    pub unit: String,
    /// Opaque conserved layer identifier.
    pub layer: String,
}

/// Exact signed posted balance and nonnegative outgoing holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityBalance {
    /// Independent CAS version of this partition.
    pub version: u64,
    /// Exact integer at the pinned unit precision, encoded as canonical text.
    pub posted: String,
    /// Exact outgoing holds, encoded as canonical text.
    pub held: String,
}

impl Default for QuantityBalance {
    fn default() -> Self {
        Self {
            version: 0,
            posted: "0".into(),
            held: "0".into(),
        }
    }
}

/// Persisted complete reservation vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityReservation {
    /// Exact ordered evaluated legs, never caller-supplied at resolution.
    pub legs: Vec<EvaluatedLedgerLeg>,
    /// Unresolved legs, retaining original identities and directions.
    pub remaining: Vec<EvaluatedLedgerLeg>,
    /// Exclusive deadline supplied at reserve.
    pub expires_at: u64,
    /// Open, posted, void or expired.
    pub status: String,
}

/// Append-only posting and its optional exact-negation link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityPosting {
    /// Exact ordered evaluated legs.
    pub legs: Vec<EvaluatedLedgerLeg>,
    /// Original transaction identity for a reversal.
    pub reverses: Option<String>,
}

/// Receipt-bound result of one quantity admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityResult {
    /// Ledger ordering revision, never a balance fence.
    pub version: u64,
    /// Resulting reservation projection, if changed.
    pub reservation: Option<(String, QuantityReservation)>,
    /// Immutable posting, if appended.
    pub posting: Option<(String, QuantityPosting)>,
    /// Balance projections actually changed by this commit.
    pub balance_keys: Vec<QuantityKey>,
}

/// Bounded current state read inside the host transaction.
#[derive(Debug, Clone, Default)]
pub struct QuantityState {
    /// Ledger ordering revision (zero means absent), never a caller fence.
    pub version: u64,
    /// Immutable checked pack.
    pub pack: Option<LedgerPackIr>,
    /// Explicit exceptions to the zero available floor.
    pub unrestricted_accounts: Vec<String>,
    /// Only heads touched by this operation.
    pub balances: BTreeMap<QuantityKey, QuantityBalance>,
    /// Named reservation, when resolving.
    pub reservation: Option<QuantityReservation>,
    /// Named original, when reversing.
    pub original: Option<QuantityPosting>,
    /// Whether the original already has a linked reversal.
    pub original_reversed: bool,
    /// Whether the new reservation or posting identity already exists.
    pub identity_exists: bool,
}

/// Mutations to apply atomically with facts, events, effects and the receipt.
#[derive(Debug, Clone)]
pub struct QuantityPlan {
    /// Next ledger ordering revision.
    pub version: u64,
    /// All changed heads (also append one history row per head).
    pub balances: BTreeMap<QuantityKey, QuantityBalance>,
    /// New, partially resolved or terminal reservation.
    pub reservation: Option<(String, QuantityReservation)>,
    /// Immutable posting to append.
    pub posting: Option<(String, QuantityPosting)>,
}

fn identity(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.trim() == id
}

/// Check resource bounds before invoking the shared pack checker.
///
/// # Errors
/// Unsupported or excessive definitions are refused.
pub fn check_quantity_pack_bounds(pack: &LedgerPackIr) -> Result<(), QuantityError> {
    // Cardinalities come first: do not traverse an oversized nested collection.
    if pack.units.is_empty()
        || pack.units.len() > MAX_QUANTITY_PACK_MEMBERS
        || pack.layers.len() > MAX_QUANTITY_PACK_MEMBERS
        || pack.templates.len() > MAX_QUANTITY_PACK_MEMBERS
        || pack
            .accounts
            .iter()
            .any(|a| a.allowed_unit_ids.len() > MAX_QUANTITY_PACK_MEMBERS)
        || pack.templates.iter().any(|t| {
            t.legs.len() > MAX_QUANTITY_LEGS
                || t.parameters.len() > MAX_QUANTITY_PACK_MEMBERS
                || t.legs
                    .iter()
                    .any(|l| l.amount.terms.len() > MAX_QUANTITY_PACK_MEMBERS)
        })
    {
        return Err(QuantityError::InvalidContract);
    }
    // Byte lengths are checked without cloning, formatting or parsing input.
    let oversized = |value: &str| value.len() > MAX_QUANTITY_IDENTIFIER_BYTES;
    let oversized_integer = |value: &str| value.len() > MAX_QUANTITY_INTEGER_BYTES;
    if oversized(&pack.pack_id)
        || oversized(&pack.ir_version)
        || oversized(&pack.ontology_artifact.artifact_id)
        || oversized(&pack.ontology_artifact.artifact_version)
        || oversized(&pack.ontology_artifact.artifact_sha256)
        || pack.units.iter().any(|u| {
            oversized(&u.unit_id)
                || oversized(&u.unit_term_id)
                || oversized(&u.quantity_kind_term_id)
        })
        || pack
            .accounts
            .iter()
            .any(|a| oversized(&a.account_id) || a.allowed_unit_ids.iter().any(|id| oversized(id)))
        || pack.layers.iter().any(|l| oversized(&l.layer_id))
        || pack.templates.iter().any(|t| {
            oversized(&t.template_id)
                || t.parameters.iter().any(|p| {
                    oversized(&p.parameter_id)
                        || oversized_integer(&p.min_unscaled)
                        || oversized_integer(&p.max_unscaled)
                })
                || t.legs.iter().any(|l| {
                    oversized(&l.leg_id)
                        || oversized(&l.account_id)
                        || oversized(&l.unit_id)
                        || oversized(&l.layer)
                        || oversized_integer(&l.amount.constant_unscaled)
                        || l.amount.terms.iter().any(|term| {
                            oversized(&term.parameter_id)
                                || oversized_integer(&term.coefficient_unscaled)
                        })
                })
        })
    {
        return Err(QuantityError::InvalidContract);
    }
    compile_ledger_pack(pack).map_err(|_diagnostics| QuantityError::InvalidContract)?;
    Ok(())
}

fn number(value: &str) -> Result<i128, QuantityError> {
    let parsed = value
        .parse::<i128>()
        .map_err(|_error| QuantityError::Overflow)?;
    if parsed.to_string() != value {
        return Err(QuantityError::InvalidContract);
    }
    if parsed == i128::MIN {
        return Err(QuantityError::Overflow);
    }
    Ok(parsed)
}

fn key(leg: &EvaluatedLedgerLeg) -> QuantityKey {
    QuantityKey {
        account: leg.account_id.clone(),
        unit: leg.unit_id.clone(),
        layer: leg.layer.clone(),
    }
}

fn open_reservation(state: &QuantityState) -> Result<&QuantityReservation, QuantityError> {
    state
        .reservation
        .as_ref()
        .filter(|reservation| reservation.status == "open")
        .ok_or(QuantityError::Terminal)
}

/// Check the shared envelope, identity and lifecycle order before discovering
/// dependencies or admitting a vector. Hosts must verify retained records first.
///
/// # Errors
/// Invalid envelopes, revision overflow, duplicate identities and missing or
/// terminal reservations refuse before subset selection.
pub fn preflight_quantity(
    request: &QuantityRequest,
    state: &QuantityState,
) -> Result<u64, QuantityError> {
    if request.version != QUANTITY_CELL_VERSION || !identity(&request.ledger_id) {
        return Err(QuantityError::InvalidContract);
    }
    let version = state
        .version
        .checked_add(1)
        .ok_or(QuantityError::Overflow)?;
    if !matches!(request.operation, QuantityOperation::Define { .. }) {
        if state.identity_exists {
            return Err(QuantityError::Duplicate);
        }
        if matches!(
            request.operation,
            QuantityOperation::PostReservation { .. }
                | QuantityOperation::Void { .. }
                | QuantityOperation::Expire { .. }
        ) {
            open_reservation(state)?;
        }
    }
    Ok(version)
}

/// Reconstruct the exact vector to be admitted, using the immutable pack or
/// stored reservation/original. This performs no time-dependent mutation.
///
/// # Errors
/// Bad templates, inputs, missing state and MIN/overflow refuse.
pub fn quantity_legs(
    request: &QuantityRequest,
    state: &QuantityState,
) -> Result<Vec<EvaluatedLedgerLeg>, QuantityError> {
    match &request.operation {
        QuantityOperation::Define { .. } => Ok(Vec::new()),
        QuantityOperation::Post {
            template_id,
            parameters,
            ..
        }
        | QuantityOperation::Reserve {
            template_id,
            parameters,
            ..
        } => {
            let pack = state.pack.as_ref().ok_or(QuantityError::InvalidContract)?;
            if parameters.len() > 16 {
                return Err(QuantityError::InvalidContract);
            }
            let compiled =
                compile_ledger_pack(pack).map_err(|_diagnostics| QuantityError::Corrupt)?;
            let evaluation = evaluate_ledger_template(&compiled, template_id, parameters).map_err(
                |diagnostics| {
                    if diagnostics.iter().any(|d| d.code.contains("overflow")) {
                        QuantityError::Overflow
                    } else {
                        QuantityError::InvalidContract
                    }
                },
            )?;
            Ok(evaluation.legs)
        }
        QuantityOperation::PostReservation { resolution, .. }
        | QuantityOperation::Void { resolution, .. } => {
            selected_legs(open_reservation(state)?, resolution)
        }
        QuantityOperation::Expire { .. } => Ok(open_reservation(state)?.remaining.clone()),
        QuantityOperation::Reverse { .. } => {
            let original = state
                .original
                .as_ref()
                .ok_or(QuantityError::InvalidReversal)?;
            if original.reverses.is_some() || state.original_reversed {
                return Err(QuantityError::InvalidReversal);
            }
            original
                .legs
                .iter()
                .map(|leg| {
                    let amount = number(&leg.amount_unscaled)?
                        .checked_neg()
                        .ok_or(QuantityError::Overflow)?;
                    let mut reversed = leg.clone();
                    reversed.amount_unscaled = amount.to_string();
                    Ok(reversed)
                })
                .collect()
        }
    }
}

fn selected_legs(
    reservation: &QuantityReservation,
    resolution: &QuantityResolution,
) -> Result<Vec<EvaluatedLedgerLeg>, QuantityError> {
    let mut residuals = BTreeMap::<(&str, &str), i128>::new();
    let mut ids = BTreeSet::new();
    for leg in &reservation.legs {
        if !ids.insert(&leg.leg_id) {
            return Err(QuantityError::InvalidContract);
        }
        let value = residuals.entry((&leg.unit_id, &leg.layer)).or_default();
        *value = value
            .checked_add(number(&leg.amount_unscaled)?)
            .ok_or(QuantityError::Overflow)?;
    }
    if residuals.values().any(|value| *value != 0) {
        return Err(QuantityError::InvalidContract);
    }
    let mut remaining_ids = BTreeSet::new();
    for leg in &reservation.remaining {
        let original = reservation
            .legs
            .iter()
            .find(|original| original.leg_id == leg.leg_id)
            .ok_or(QuantityError::Corrupt)?;
        let value = number(&leg.amount_unscaled)?;
        let total = number(&original.amount_unscaled)?;
        if !remaining_ids.insert(&leg.leg_id)
            || value.abs() > total.abs()
            || (value != 0 && value.signum() != total.signum())
            || leg.account_id != original.account_id
            || leg.unit_id != original.unit_id
            || leg.layer != original.layer
            || leg.unit_precision != original.unit_precision
        {
            return Err(QuantityError::Corrupt);
        }
    }
    match resolution {
        QuantityResolution::All => Ok(reservation.remaining.clone()),
        QuantityResolution::Subset { legs } => {
            if legs.is_empty() || legs.len() > MAX_QUANTITY_LEGS {
                return Err(QuantityError::InvalidContract);
            }
            let mut seen = BTreeSet::new();
            legs.iter()
                .map(|slice| {
                    if !seen.insert(&slice.leg_id) {
                        return Err(QuantityError::InvalidContract);
                    }
                    let mut leg = reservation
                        .remaining
                        .iter()
                        .find(|leg| leg.leg_id == slice.leg_id)
                        .cloned()
                        .ok_or(QuantityError::InvalidContract)?;
                    let selected = number(&slice.amount_unscaled)?;
                    let remaining = number(&leg.amount_unscaled)?;
                    if selected == 0
                        || selected.signum() != remaining.signum()
                        || selected.abs() > remaining.abs()
                    {
                        return Err(QuantityError::InvalidContract);
                    }
                    leg.amount_unscaled.clone_from(&slice.amount_unscaled);
                    Ok(leg)
                })
                .collect()
        }
    }
}

fn subtract_legs(
    reservation: &mut QuantityReservation,
    selected: &[EvaluatedLedgerLeg],
) -> Result<(), QuantityError> {
    for leg in &mut reservation.remaining {
        if let Some(slice) = selected.iter().find(|slice| slice.leg_id == leg.leg_id) {
            leg.amount_unscaled = number(&leg.amount_unscaled)?
                .checked_sub(number(&slice.amount_unscaled)?)
                .ok_or(QuantityError::Overflow)?
                .to_string();
        }
    }
    reservation
        .remaining
        .retain(|leg| leg.amount_unscaled != "0");
    Ok(())
}

// Funding remains per (account, unit, layer); other reservations' pending
// incoming quantities never finance this vector.
fn funding(
    legs: &[EvaluatedLedgerLeg],
    pack: &LedgerPackIr,
) -> Result<BTreeMap<QuantityKey, i128>, QuantityError> {
    let mut amounts = BTreeMap::<QuantityKey, (i128, i128)>::new();
    for leg in legs {
        let value = number(&leg.amount_unscaled)?;
        let (net, gross) = amounts.entry(key(leg)).or_default();
        *net = net.checked_add(value).ok_or(QuantityError::Overflow)?;
        if value < 0 {
            *gross = gross.checked_sub(value).ok_or(QuantityError::Overflow)?;
        }
    }
    amounts
        .into_iter()
        .map(|(key, (net, gross))| {
            let account = pack
                .accounts
                .iter()
                .find(|account| account.account_id == key.account)
                .ok_or(QuantityError::Corrupt)?;
            let required = match account.funding {
                norn_ir::quantity_ledger::LedgerFundingPolicy::Gross => gross,
                norn_ir::quantity_ledger::LedgerFundingPolicy::Net => {
                    net.checked_neg().ok_or(QuantityError::Overflow)?.max(0)
                }
            };
            Ok((key, required))
        })
        .collect()
}

fn balance_dependencies(
    legs: &[EvaluatedLedgerLeg],
    state: &QuantityState,
    operation: &QuantityOperation,
) -> Result<BTreeSet<QuantityKey>, QuantityError> {
    let pack = state.pack.as_ref().ok_or(QuantityError::Corrupt)?;
    let post = matches!(
        operation,
        QuantityOperation::Post { .. }
            | QuantityOperation::Reverse { .. }
            | QuantityOperation::PostReservation { .. }
    );
    let reserve = matches!(operation, QuantityOperation::Reserve { .. });
    let release = matches!(
        operation,
        QuantityOperation::PostReservation { .. }
            | QuantityOperation::Void { .. }
            | QuantityOperation::Expire { .. }
    );
    let required = funding(legs, pack)?;
    let mut deltas = BTreeMap::<QuantityKey, (i128, i128)>::new();
    if post {
        for leg in legs {
            let delta = &mut deltas.entry(key(leg)).or_default().0;
            *delta = delta
                .checked_add(number(&leg.amount_unscaled)?)
                .ok_or(QuantityError::Overflow)?;
        }
    }
    if reserve {
        for (key, held) in &required {
            deltas.entry(key.clone()).or_default().1 = *held;
        }
    }
    if release {
        let reservation = state.reservation.as_ref().ok_or(QuantityError::Terminal)?;
        let old = funding(&reservation.remaining, pack)?;
        let mut remainder = reservation.clone();
        subtract_legs(&mut remainder, legs)?;
        let new = funding(&remainder.remaining, pack)?;
        for (key, held) in old {
            deltas.entry(key.clone()).or_default().1 = new
                .get(&key)
                .copied()
                .unwrap_or(0)
                .checked_sub(held)
                .ok_or(QuantityError::Overflow)?;
        }
    }
    let mut keys: BTreeSet<_> = deltas
        .into_iter()
        .filter(|(_, (posted, held))| *posted != 0 || *held != 0)
        .map(|(key, _)| key)
        .collect();
    if post || reserve {
        keys.extend(
            required
                .into_iter()
                .filter(|(key, amount)| {
                    *amount > 0 && !state.unrestricted_accounts.contains(&key.account)
                })
                .map(|(key, _)| key),
        );
    }
    Ok(keys)
}

/// Exact balance dependencies: mutations and declared funding reads, excluding
/// zero-effect legs and pending incoming quantity that provides no funding.
///
/// # Errors
/// Invalid templates, subsets or arithmetic refuse.
pub fn quantity_balance_keys(
    request: &QuantityRequest,
    state: &QuantityState,
) -> Result<BTreeSet<QuantityKey>, QuantityError> {
    preflight_quantity(request, state)?;
    if matches!(request.operation, QuantityOperation::Define { .. }) {
        return Ok(BTreeSet::new());
    }
    balance_dependencies(&quantity_legs(request, state)?, state, &request.operation)
}

/// Admit a complete vector against version-fenced current heads and injected
/// trusted time. The declared account funding policy determines its outgoing
/// requirement, including the funded remainder after partial resolution.
/// At the deadline only expire may resolve.
///
/// # Errors
/// Typed arithmetic, CAS, conservation, availability and lifecycle refusals.
#[allow(
    clippy::too_many_lines,
    reason = "closed quantity operations share one atomic admission algorithm"
)]
pub fn prepare_quantity(
    request: &QuantityRequest,
    state: &QuantityState,
    now: u64,
) -> Result<QuantityPlan, QuantityError> {
    let version = preflight_quantity(request, state)?;
    let mut plan = QuantityPlan {
        version,
        balances: BTreeMap::new(),
        reservation: None,
        posting: None,
    };
    if let QuantityOperation::Define {
        account_limit,
        pack,
        unrestricted_accounts,
    } = &request.operation
    {
        if state.pack.is_some() || state.version != 0 {
            return Err(QuantityError::Duplicate);
        }
        if !request.balance_versions.is_empty() {
            return Err(QuantityError::InvalidContract);
        }
        if *account_limit == 0
            || u32::try_from(pack.accounts.len())
                .map_err(|_range| QuantityError::InvalidContract)?
                > *account_limit
        {
            return Err(QuantityError::InvalidContract);
        }
        check_quantity_pack_bounds(pack)?;
        let mut seen = BTreeSet::new();
        if unrestricted_accounts.len() > pack.accounts.len()
            || unrestricted_accounts.iter().any(|a| {
                !seen.insert(a)
                    || !pack
                        .accounts
                        .iter()
                        .any(|declared| &declared.account_id == a)
            })
        {
            return Err(QuantityError::InvalidContract);
        }
        return Ok(plan);
    }
    let legs = quantity_legs(request, state)?;
    if legs.is_empty() || legs.len() > MAX_QUANTITY_LEGS {
        return Err(QuantityError::InvalidContract);
    }
    let pack = state.pack.as_ref().ok_or(QuantityError::Corrupt)?;
    let mut residuals = BTreeMap::<(String, String), i128>::new();
    let mut changes = BTreeMap::<QuantityKey, (i128, i128)>::new();
    let mut seen = BTreeSet::new();
    for leg in &legs {
        if !seen.insert(&leg.leg_id)
            || !pack
                .units
                .iter()
                .any(|u| u.unit_id == leg.unit_id && u.precision == leg.unit_precision)
            || !pack.accounts.iter().any(|a| {
                a.account_id == leg.account_id && a.allowed_unit_ids.contains(&leg.unit_id)
            })
            || !pack.layers.iter().any(|l| l.layer_id == leg.layer)
        {
            return Err(QuantityError::Corrupt);
        }
        let amount = number(&leg.amount_unscaled)?;
        let residual = residuals
            .entry((leg.unit_id.clone(), leg.layer.clone()))
            .or_default();
        *residual = residual
            .checked_add(amount)
            .ok_or(QuantityError::Overflow)?;
        let (net, outgoing) = changes.entry(key(leg)).or_default();
        *net = net.checked_add(amount).ok_or(QuantityError::Overflow)?;
        if amount < 0 {
            *outgoing = outgoing
                .checked_add(amount.checked_neg().ok_or(QuantityError::Overflow)?)
                .ok_or(QuantityError::Overflow)?;
        }
    }
    if residuals.values().any(|r| *r != 0) {
        return Err(QuantityError::InvalidContract);
    }
    let (post, reserve_holds, release) = match &request.operation {
        QuantityOperation::Define { .. } => return Err(QuantityError::InvalidContract),
        QuantityOperation::Post { posting_id, .. } => {
            if !identity(posting_id) {
                return Err(QuantityError::InvalidContract);
            }
            plan.posting = Some((
                posting_id.clone(),
                QuantityPosting {
                    legs: legs.clone(),
                    reverses: None,
                },
            ));
            (true, false, false)
        }
        QuantityOperation::Reverse {
            original_id,
            posting_id,
        } => {
            if !identity(original_id) || !identity(posting_id) || original_id == posting_id {
                return Err(QuantityError::InvalidReversal);
            }
            plan.posting = Some((
                posting_id.clone(),
                QuantityPosting {
                    legs: legs.clone(),
                    reverses: Some(original_id.clone()),
                },
            ));
            (true, false, false)
        }
        QuantityOperation::Reserve {
            reservation_id,
            expires_at,
            ..
        } => {
            if !identity(reservation_id) {
                return Err(QuantityError::InvalidContract);
            }
            if *expires_at <= now {
                return Err(QuantityError::Expired);
            }
            plan.reservation = Some((
                reservation_id.clone(),
                QuantityReservation {
                    legs: legs.clone(),
                    remaining: legs.clone(),
                    expires_at: *expires_at,
                    status: "open".into(),
                },
            ));
            (false, true, false)
        }
        QuantityOperation::PostReservation { reservation_id, .. }
        | QuantityOperation::Void { reservation_id, .. }
        | QuantityOperation::Expire { reservation_id } => {
            let mut reservation = open_reservation(state)?.clone();
            let expired = now >= reservation.expires_at;
            let posting = match &request.operation {
                QuantityOperation::Expire { .. } => {
                    if !expired {
                        return Err(QuantityError::NotExpired);
                    }
                    reservation.status = "expired".into();
                    false
                }
                QuantityOperation::Void { .. } => {
                    if expired {
                        return Err(QuantityError::Expired);
                    }
                    reservation.status = "void".into();
                    false
                }
                QuantityOperation::PostReservation { posting_id, .. } => {
                    if expired {
                        return Err(QuantityError::Expired);
                    }
                    if !identity(posting_id) {
                        return Err(QuantityError::InvalidContract);
                    }
                    reservation.status = "posted".into();
                    plan.posting = Some((
                        posting_id.clone(),
                        QuantityPosting {
                            legs: legs.clone(),
                            reverses: None,
                        },
                    ));
                    true
                }
                QuantityOperation::Define { .. }
                | QuantityOperation::Post { .. }
                | QuantityOperation::Reserve { .. }
                | QuantityOperation::Reverse { .. } => return Err(QuantityError::InvalidContract),
            };
            subtract_legs(&mut reservation, &legs)?;
            if !reservation.remaining.is_empty() {
                reservation.status = "open".into();
            }
            plan.reservation = Some((reservation_id.clone(), reservation));
            (posting, false, true)
        }
    };
    let required = funding(&legs, pack)?;
    let old_holds = if release {
        funding(
            &state
                .reservation
                .as_ref()
                .ok_or(QuantityError::Corrupt)?
                .remaining,
            pack,
        )?
    } else {
        BTreeMap::new()
    };
    let new_holds = if release {
        funding(
            &plan
                .reservation
                .as_ref()
                .ok_or(QuantityError::Corrupt)?
                .1
                .remaining,
            pack,
        )?
    } else {
        BTreeMap::new()
    };
    // A subset can change the netted hold of keys outside its own outgoing
    // legs. Read and fence the full remainder and validate its new funding.
    for key in old_holds.keys().chain(new_holds.keys()) {
        changes.entry(key.clone()).or_default();
    }
    let dependencies = balance_dependencies(&legs, state, &request.operation)?;
    changes.retain(|key, _| dependencies.contains(key));
    if request.balance_versions.len() != changes.len() {
        return Err(QuantityError::InvalidContract);
    }
    let mut fences = BTreeSet::new();
    for fence in &request.balance_versions {
        let key = QuantityKey {
            account: fence.account.clone(),
            unit: fence.unit.clone(),
            layer: fence.layer.clone(),
        };
        if !changes.contains_key(&key) || !fences.insert(key.clone()) {
            return Err(QuantityError::InvalidContract);
        }
        let current = state
            .balances
            .get(&key)
            .map_or(0, |balance| balance.version);
        if current != fence.version {
            return Err(QuantityError::StaleVersion {
                expected: fence.version,
                current,
            });
        }
    }
    for (key, (net, _gross)) in changes {
        let outgoing = required.get(&key).copied().unwrap_or(0);
        let current = state.balances.get(&key).cloned().unwrap_or_default();
        let posted = number(&current.posted)?;
        let held = number(&current.held)?;
        if held < 0 {
            return Err(QuantityError::Corrupt);
        }
        let remaining_hold = if release {
            held.checked_sub(old_holds.get(&key).copied().unwrap_or(0))
                .ok_or(QuantityError::Overflow)?
        } else {
            held
        };
        if remaining_hold < 0 {
            return Err(QuantityError::Corrupt);
        }
        let available = posted
            .checked_sub(remaining_hold)
            .ok_or(QuantityError::Overflow)?;
        if (post || reserve_holds)
            && !state.unrestricted_accounts.contains(&key.account)
            && available < outgoing
        {
            return Err(QuantityError::InsufficientAvailable {
                account: key.account,
            });
        }
        let posted = if post {
            posted.checked_add(net).ok_or(QuantityError::Overflow)?
        } else {
            posted
        };
        let held = if release {
            remaining_hold
                .checked_add(new_holds.get(&key).copied().unwrap_or(0))
                .ok_or(QuantityError::Overflow)?
        } else if reserve_holds {
            remaining_hold
                .checked_add(outgoing)
                .ok_or(QuantityError::Overflow)?
        } else {
            remaining_hold
        };
        posted.checked_sub(held).ok_or(QuantityError::Overflow)?;
        if (post || reserve_holds || release)
            && !state.unrestricted_accounts.contains(&key.account)
            && posted < held
        {
            return Err(QuantityError::InsufficientAvailable {
                account: key.account,
            });
        }
        if posted == i128::MIN {
            return Err(QuantityError::Overflow);
        }
        if posted.to_string() == current.posted && held.to_string() == current.held {
            continue;
        }
        plan.balances.insert(
            key,
            QuantityBalance {
                version: current
                    .version
                    .checked_add(1)
                    .ok_or(QuantityError::Overflow)?,
                posted: posted.to_string(),
                held: held.to_string(),
            },
        );
    }
    Ok(plan)
}
