//! CONS-LINEAR-1 auto-lowering from compiled quantity ledger packs and
//! [`norn_ir::ActionQuantityEffect`] actions into exact-rational conservation systems.
//!
//! Evidence boundary: the per-writer weighted-balance decision in
//! [`super::decision::check_writer`] is proved by Aeneas/Lean
//! (`ConservationProof.lean`). This adapter is reviewed glue (structural
//! lowering, fail-closed `i128` -> `i64` exact-rational conversion, and action
//! template/parameter binding), not itself a machine-checked theorem of
//! declaration-to-runtime correspondence.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::quantity_ledger::{LedgerPackIr, MAX_LEDGER_SCALE};
use norn_ir::{ActionQuantityOperation, ActionRoot, UnitRef, UnitSystem};

use super::{
    BoundAction, Coordinate, Delta, Failure, InstalledActions, Rational, Writer, WriterSource,
    check,
};
use crate::quantity_ledger::{
    CompiledLedgerPack, CompiledLedgerTemplate, LedgerPackSeal, diagnostic_codes,
    ledger_pack_sha256, normalize_ledger_pack_for_conservation,
};

const MAX_COORDINATES: usize = 256;
const MAX_WRITERS: usize = 256;
const ZERO: Rational = Rational {
    numerator: 0,
    denominator: 1,
};
const ONE: Rational = Rational {
    numerator: 1,
    denominator: 1,
};

/// A lowered CONS-LINEAR-1 transition system ready for [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredLedgerSystem {
    /// Conserved balance coordinates, one per distinct `(account_id, unit_id, layer)`.
    pub coordinates: Vec<Coordinate>,
    /// Lowered affine writers, one per ledger template or bound quantity action.
    pub writers: Vec<Writer>,
    /// Unit-weight rational functional matching `coordinates`.
    pub certificate: Vec<Rational>,
}

/// Summary returned after [`check`] certifies template-level conservation for
/// every template in a ledger pack.
///
/// This records what was checked through the Aeneas-proved per-writer decision
/// core; it does not establish initial-state validity, non-vacuity, or store
/// atomicity. `pack_sha256` is populated when certifying from a [`LedgerPackSeal`]
/// or [`LedgerPackIr`], and `None` when certifying an unbound
/// [`CompiledLedgerPack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPackCertification {
    /// Certified ledger pack identifier.
    pub pack_id: String,
    /// Exact canonical pack SHA-256 when certified from a seal or raw IR,
    /// or `None` for an unbound [`CompiledLedgerPack`].
    pub pack_sha256: Option<String>,
    /// Number of compiled templates certified through [`check`].
    pub templates_checked: usize,
    /// Number of distinct `(account_id, unit_id, layer)` coordinates certified.
    pub coordinates_checked: usize,
    /// Number of distinct `(unit_id, layer)` partitions certified.
    pub partitions_checked: usize,
}

/// Summary returned after [`check`] certifies template-level conservation for
/// every quantity-effect action against the caller-supplied ledger packs.
///
/// This certifies that the bound template(s) are symbolically conserved
/// (`Reserve` holds negative legs of its pinned template; `PostReservation`
/// posts remaining legs of an open reservation from the pack's templates).
/// Callers must supply packs matching the cell's pinned `Define` seals.
/// `installed_pins_matched` reports how many checked actions matched an exact
/// revision in [`InstalledActions`]; it confers no extra proof authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantityActionsCertification {
    /// Number of actions with `quantity_effect: Some(_)` certified.
    pub actions_checked: usize,
    /// Total `(action, template)` writer checks certified through [`check`].
    pub templates_checked: usize,
    /// Number of checked quantity actions matching an [`InstalledActions`] pin.
    pub installed_pins_matched: usize,
}

fn nonempty(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn exact_i128_to_rational(value: i128, context: &str) -> Result<Rational, Failure> {
    Rational::normalize(value, 1).ok_or_else(|| {
        Failure::Unknown(format!(
            "{context}: exact integer {value} exceeds i64 rational representation"
        ))
    })
}

fn coordinate_id(account_id: &str, unit_id: &str, layer: &str) -> Result<String, Failure> {
    serde_json::to_string(&(account_id, unit_id, layer)).map_err(|_error| Failure::Malformed)
}

fn validate_pack_unit_precisions(pack: &CompiledLedgerPack) -> Result<(), Failure> {
    let mut precisions: BTreeMap<&str, u8> = BTreeMap::new();
    for template in pack.templates.values() {
        for leg in &template.legs {
            if leg.unit_precision > MAX_LEDGER_SCALE
                || !nonempty(&leg.unit_id)
                || *precisions
                    .entry(leg.unit_id.as_str())
                    .or_insert(leg.unit_precision)
                    != leg.unit_precision
            {
                return Err(Failure::Malformed);
            }
        }
    }
    Ok(())
}

fn validate_template_shape(
    expected_key: Option<&str>,
    template: &CompiledLedgerTemplate,
) -> Result<(), Failure> {
    if !nonempty(&template.template_id)
        || expected_key.is_some_and(|key| key != template.template_id)
        || template.legs.len() < 2
    {
        return Err(Failure::Malformed);
    }
    let mut leg_ids = BTreeSet::new();
    for leg in &template.legs {
        if !nonempty(&leg.leg_id)
            || !nonempty(&leg.account_id)
            || !nonempty(&leg.unit_id)
            || !nonempty(&leg.layer)
            || leg.unit_precision > MAX_LEDGER_SCALE
            || !leg_ids.insert(leg.leg_id.as_str())
        {
            return Err(Failure::Malformed);
        }
        if leg
            .coefficients_unscaled()
            .keys()
            .any(|param| template.parameter_scale(param).is_none())
        {
            return Err(Failure::Malformed);
        }
    }
    Ok(())
}

type CoordKey = (String, String, String);

fn build_coordinates(
    keys: &BTreeSet<CoordKey>,
    pack_id: &str,
    ledger_id: Option<&str>,
) -> Result<(Vec<Coordinate>, BTreeMap<CoordKey, usize>), Failure> {
    if keys.is_empty() {
        return Err(Failure::Malformed);
    }
    if keys.len() > MAX_COORDINATES {
        return Err(Failure::Unknown(format!(
            "ledger lowering: {} coordinates exceed limit {MAX_COORDINATES}",
            keys.len()
        )));
    }
    let mut coordinates = Vec::with_capacity(keys.len());
    let mut index = BTreeMap::new();
    for (pos, (account_id, unit_id, layer)) in keys.iter().enumerate() {
        let id = coordinate_id(account_id, unit_id, layer)?;
        let mut scope = BTreeMap::from([
            ("layer".to_owned(), layer.clone()),
            ("pack".to_owned(), pack_id.to_owned()),
        ]);
        if let Some(ledger_id) = ledger_id {
            scope.insert("ledger".to_owned(), ledger_id.to_owned());
        }
        coordinates.push(Coordinate {
            id,
            scope,
            // Opaque pack-local `unit_id` stored in `UnitRef` strictly as an
            // exact equality partition key; CONS-LINEAR-1 never converts units
            // or interprets `code` as a UCUM conversion expression.
            unit: UnitRef {
                system: UnitSystem::Ucum,
                code: unit_id.clone(),
            },
            weight: ONE,
            // Ledger accounts are statically fixed per template leg, never
            // dynamically keyed by runtime entity input fields, and multiple
            // legs on one (account, unit, layer) accumulate directly below.
            cell: None,
        });
        index.insert((account_id.clone(), unit_id.clone(), layer.clone()), pos);
    }
    Ok((coordinates, index))
}

fn lower_template_writer(
    writer_id: String,
    source: WriterSource,
    template: &CompiledLedgerTemplate,
    coordinates: &[Coordinate],
    coord_index: &BTreeMap<CoordKey, usize>,
) -> Result<Writer, Failure> {
    let param_order: Vec<&str> = template.parameter_ids().collect();
    let param_columns: BTreeMap<&str, usize> = param_order
        .iter()
        .enumerate()
        .map(|(col, &name)| (name, col))
        .collect();
    let constant_col = param_order.len();
    let columns = constant_col
        .checked_add(1)
        .ok_or_else(|| Failure::Unknown(format!("{writer_id}: parameter column limit exceeded")))?;

    let mut rows = vec![vec![ZERO; columns]; coordinates.len()];
    let mut assigned = BTreeSet::new();

    for leg in &template.legs {
        let key = (
            leg.account_id.clone(),
            leg.unit_id.clone(),
            leg.layer.clone(),
        );
        let &row_idx = coord_index.get(&key).ok_or(Failure::Malformed)?;
        let coord = coordinates.get(row_idx).ok_or(Failure::Malformed)?;
        assigned.insert(coord.id.clone());

        let row = rows.get_mut(row_idx).ok_or(Failure::Malformed)?;
        let context = format!("{writer_id}::{}", leg.leg_id);
        let constant_q = exact_i128_to_rational(leg.constant_unscaled(), &context)?;
        let constant_slot = row.get_mut(constant_col).ok_or(Failure::Malformed)?;
        *constant_slot = constant_slot.checked_add(constant_q).ok_or_else(|| {
            Failure::Unknown(format!(
                "{context}: constant accumulation overflowed i64 rational representation"
            ))
        })?;

        for (param_id, &coeff) in leg.coefficients_unscaled() {
            let &col_idx = param_columns
                .get(param_id.as_str())
                .ok_or(Failure::Malformed)?;
            let coeff_q = exact_i128_to_rational(coeff, &format!("{context}[{param_id}]"))?;
            let slot = row.get_mut(col_idx).ok_or(Failure::Malformed)?;
            *slot = slot.checked_add(coeff_q).ok_or_else(|| {
                Failure::Unknown(format!(
                    "{context}[{param_id}]: coefficient accumulation overflowed i64 rational representation"
                ))
            })?;
        }
    }

    Ok(Writer {
        id: writer_id,
        delta: Delta::Affine(rows),
        assigns: assigned.into_iter().collect(),
        source,
    })
}

/// Lowers every template in a [`CompiledLedgerPack`] into exact-rational
/// CONS-LINEAR-1 [`Coordinate`]s and [`Writer`]s.
///
/// Each distinct `(account_id, unit_id, layer)` becomes one unit-weight
/// coordinate scoped by `(pack, layer)` and unit `unit_id`. Every template
/// becomes one [`Writer`] with `Delta::Affine`, parameter columns in canonical
/// sorted order, and the affine constant in the final column. Multiple legs
/// targeting the same `(account_id, unit_id, layer)` accumulate exactly.
///
/// # Errors
///
/// Returns [`Failure::Malformed`] on empty/invalid identifiers, empty packs or
/// templates, duplicate leg ids, undeclared parameter references, or
/// inconsistent unit precisions. Returns [`Failure::Unknown`] when any exact
/// `i128` constant/coefficient or accumulation overflows `i64` rational bounds or
/// exceeds coordinate/writer limits.
pub fn lower_ledger_pack(pack: &CompiledLedgerPack) -> Result<LoweredLedgerSystem, Failure> {
    if !nonempty(&pack.pack_id) || pack.templates.is_empty() {
        return Err(Failure::Malformed);
    }
    if pack.templates.len() > MAX_WRITERS {
        return Err(Failure::Unknown(format!(
            "ledger lowering: {} templates exceed limit {MAX_WRITERS}",
            pack.templates.len()
        )));
    }
    validate_pack_unit_precisions(pack)?;

    let mut coord_keys = BTreeSet::new();
    for (key, template) in &pack.templates {
        validate_template_shape(Some(key.as_str()), template)?;
        for leg in &template.legs {
            coord_keys.insert((
                leg.account_id.clone(),
                leg.unit_id.clone(),
                leg.layer.clone(),
            ));
        }
    }

    let (coordinates, coord_index) = build_coordinates(&coord_keys, &pack.pack_id, None)?;
    let mut writers = Vec::with_capacity(pack.templates.len());
    for template in pack.templates.values() {
        writers.push(lower_template_writer(
            template.template_id.clone(),
            WriterSource::Unbound,
            template,
            &coordinates,
            &coord_index,
        )?);
    }
    let certificate = vec![ONE; coordinates.len()];
    Ok(LoweredLedgerSystem {
        coordinates,
        writers,
        certificate,
    })
}

fn partition_count(coordinates: &[Coordinate]) -> usize {
    coordinates
        .iter()
        .map(|coord| (&coord.scope, &coord.unit))
        .collect::<BTreeSet<_>>()
        .len()
}

/// Lowers `pack` and certifies template-level symbolic conservation of every
/// template and `(unit, layer)` partition through the Aeneas-proved [`check`]
/// decision core.
///
/// Prefer [`certify_ledger_pack_seal`] when a [`LedgerPackSeal`] is available
/// so the returned certification retains the exact canonical `pack_sha256`.
///
/// # Errors
///
/// Returns [`Failure`] on malformed pack structure, exact-rational overflow or
/// resource limit (`Failure::Unknown`), or any nonzero symbolic residual
/// (`Failure::Unbalanced`).
pub fn certify_ledger_pack(pack: &CompiledLedgerPack) -> Result<LedgerPackCertification, Failure> {
    let system = lower_ledger_pack(pack)?;
    check(
        &system.coordinates,
        &system.writers,
        &system.certificate,
        &InstalledActions::none(),
    )?;
    Ok(LedgerPackCertification {
        pack_id: pack.pack_id.clone(),
        pack_sha256: None,
        templates_checked: system.writers.len(),
        coordinates_checked: system.coordinates.len(),
        partitions_checked: partition_count(&system.coordinates),
    })
}

/// Certifies template-level symbolic conservation for a sealed
/// [`LedgerPackSeal`] through the Aeneas-proved [`check`] decision core,
/// binding `seal.pack_sha256()` into the returned certification.
///
/// # Errors
///
/// Returns [`Failure`] on malformed pack structure, exact-rational overflow or
/// resource limit (`Failure::Unknown`), or any nonzero symbolic residual
/// (`Failure::Unbalanced`).
pub fn certify_ledger_pack_seal(seal: &LedgerPackSeal) -> Result<LedgerPackCertification, Failure> {
    let mut cert = certify_ledger_pack(seal.compiled())?;
    cert.pack_sha256 = Some(seal.pack_sha256().to_owned());
    Ok(cert)
}

/// Structurally normalizes a raw [`LedgerPackIr`] without short-circuiting on
/// unbalanced templates, then certifies template-level conservation through the
/// Aeneas-proved [`check`] decision core, binding [`ledger_pack_sha256`].
///
/// Unbalanced constant or coefficient residuals surface directly from [`check`]
/// as [`Failure::Unbalanced`] algebraic witnesses.
///
/// # Errors
///
/// Returns [`Failure::Malformed`] on structural/identifier/scale errors,
/// [`Failure::Unknown`] on `i128` or `i64` representation/resource limits, and
/// [`Failure::Unbalanced`] on any nonzero weighted residual.
pub fn certify_ledger_pack_ir(pack: &LedgerPackIr) -> Result<LedgerPackCertification, Failure> {
    let compiled = normalize_ledger_pack_for_conservation(pack).map_err(|validation| {
        let is_limit = |code: &str| {
            matches!(
                code,
                diagnostic_codes::NORMALIZATION_OVERFLOW
                    | diagnostic_codes::BOUND_OVERFLOW
                    | diagnostic_codes::RESIDUAL_OVERFLOW
                    | diagnostic_codes::TOO_MANY_DIAGNOSTICS
            )
        };
        let is_balance = |code: &str| {
            matches!(
                code,
                diagnostic_codes::UNBALANCED_CONSTANT | diagnostic_codes::UNBALANCED_COEFFICIENT
            )
        };
        // Prioritize structural/malformed findings over limit findings on mixed
        // diagnostic sets; return Unknown only when every non-balance refusal
        // is an overflow or diagnostic-budget limit.
        let has_structural_error = validation
            .diagnostics
            .iter()
            .any(|d| !is_limit(d.code.as_str()) && !is_balance(d.code.as_str()));
        if has_structural_error {
            Failure::Malformed
        } else if let Some(limit) = validation
            .diagnostics
            .iter()
            .find(|d| is_limit(d.code.as_str()))
        {
            Failure::Unknown(format!("{}: {}", limit.code, limit.message))
        } else {
            Failure::Malformed
        }
    })?;
    let mut cert = certify_ledger_pack(&compiled)?;
    cert.pack_sha256 = Some(ledger_pack_sha256(pack));
    Ok(cert)
}

fn lower_single_template_for_action(
    action: &ActionRoot,
    pack: &CompiledLedgerPack,
    ledger_id: &str,
    expected_key: Option<&str>,
    template: &CompiledLedgerTemplate,
) -> Result<LoweredLedgerSystem, Failure> {
    validate_template_shape(expected_key, template)?;
    let mut coord_keys = BTreeSet::new();
    for leg in &template.legs {
        coord_keys.insert((
            leg.account_id.clone(),
            leg.unit_id.clone(),
            leg.layer.clone(),
        ));
    }
    let (coordinates, coord_index) =
        build_coordinates(&coord_keys, &pack.pack_id, Some(ledger_id))?;
    let writer = lower_template_writer(
        action.action_id.clone(),
        WriterSource::Action(BoundAction::of(action)),
        template,
        &coordinates,
        &coord_index,
    )?;
    let certificate = vec![ONE; coordinates.len()];
    Ok(LoweredLedgerSystem {
        coordinates,
        writers: vec![writer],
        certificate,
    })
}

/// Lowers one [`ActionRoot`] carrying an [`norn_ir::ActionQuantityEffect`] into
/// one or more [`LoweredLedgerSystem`]s bound to `action` via
/// [`BoundAction::of`].
///
/// - [`ActionQuantityOperation::Reserve`] validates exact parameter-name and
///   scale bindings against the pinned template and emits one lowered system.
/// - [`ActionQuantityOperation::PostReservation`] can commit a reservation
///   created from any template in `pack`, so it emits one lowered system per
///   template in `pack` (never folding distinct templates into one constant
///   column where opposing errors could cancel).
///
/// # Errors
///
/// Returns [`Failure::Malformed`] when the action has no quantity effect,
/// mismatches `ledger_id`, has invalid parameter/scale bindings, or the pack is
/// structurally invalid. Returns [`Failure::Unknown`] when the referenced
/// template is absent or exact-rational conversion/limits fail closed.
pub fn lower_quantity_action(
    action: &ActionRoot,
    pack: &CompiledLedgerPack,
    ledger_id: &str,
) -> Result<Vec<LoweredLedgerSystem>, Failure> {
    if !nonempty(&action.action_id)
        || !nonempty(ledger_id)
        || !nonempty(&pack.pack_id)
        || pack.templates.is_empty()
    {
        return Err(Failure::Malformed);
    }
    let Some(effect) = &action.quantity_effect else {
        return Err(Failure::Malformed);
    };
    if effect.ledger_id != ledger_id || !nonempty(&effect.balance_versions_field) {
        return Err(Failure::Malformed);
    }
    validate_pack_unit_precisions(pack)?;

    match &effect.operation {
        ActionQuantityOperation::Reserve {
            reservation_id_field,
            template_id,
            parameters,
            ttl_ms,
        } => {
            if !nonempty(reservation_id_field) || !nonempty(template_id) || ttl_ms.0 == 0 {
                return Err(Failure::Malformed);
            }
            let template = pack.templates.get(template_id).ok_or_else(|| {
                Failure::Unknown(format!(
                    "{}: unknown template `{template_id}` in ledger `{ledger_id}`",
                    action.action_id
                ))
            })?;
            if parameters.len() != template.parameter_count() {
                return Err(Failure::Malformed);
            }
            for (param_id, binding) in parameters {
                if !nonempty(&binding.field)
                    || template
                        .parameter_scale(param_id)
                        .is_none_or(|scale| u64::from(scale) != binding.scale.0)
                {
                    return Err(Failure::Malformed);
                }
            }
            Ok(vec![lower_single_template_for_action(
                action,
                pack,
                ledger_id,
                Some(template_id.as_str()),
                template,
            )?])
        }
        ActionQuantityOperation::PostReservation {
            reservation_id_field,
            posting_id_field,
        } => {
            if !nonempty(reservation_id_field) || !nonempty(posting_id_field) {
                return Err(Failure::Malformed);
            }
            if pack.templates.len() > MAX_WRITERS {
                return Err(Failure::Unknown(format!(
                    "{}: {} pack templates exceed limit {MAX_WRITERS}",
                    action.action_id,
                    pack.templates.len()
                )));
            }
            let mut systems = Vec::with_capacity(pack.templates.len());
            for (key, template) in &pack.templates {
                systems.push(lower_single_template_for_action(
                    action,
                    pack,
                    ledger_id,
                    Some(key.as_str()),
                    template,
                )?);
            }
            Ok(systems)
        }
    }
}

/// Lowers and certifies every [`ActionRoot`] carrying an
/// [`norn_ir::ActionQuantityEffect`] against its configured ledger in `ledgers`
/// through the Aeneas-proved [`check`] decision core.
///
/// Actions with `quantity_effect: None` are skipped. Each quantity action is
/// bound via [`BoundAction::of`] and checked against `installed`.
///
/// # Errors
///
/// Returns [`Failure::Malformed`] on duplicate `action_id`s or invalid action
/// bindings, [`Failure::Unknown`] on unknown ledgers/templates or exact-rational
/// overflow/resource limits, and [`Failure::Unbalanced`] on any unbalanced
/// template branch.
pub fn certify_quantity_actions(
    actions: &[ActionRoot],
    ledgers: &BTreeMap<String, CompiledLedgerPack>,
    installed: &InstalledActions,
) -> Result<QuantityActionsCertification, Failure> {
    let mut seen_ids = BTreeSet::new();
    let mut actions_checked = 0_usize;
    let mut templates_checked = 0_usize;
    let mut installed_pins_matched = 0_usize;

    for action in actions {
        if !nonempty(&action.action_id) || !seen_ids.insert(action.action_id.as_str()) {
            return Err(Failure::Malformed);
        }
        let Some(effect) = &action.quantity_effect else {
            continue;
        };
        if !nonempty(&effect.ledger_id) {
            return Err(Failure::Malformed);
        }
        let pack = ledgers.get(&effect.ledger_id).ok_or_else(|| {
            Failure::Unknown(format!(
                "{}: unknown ledger `{}`",
                action.action_id, effect.ledger_id
            ))
        })?;
        let systems = lower_quantity_action(action, pack, &effect.ledger_id)?;
        for system in &systems {
            check(
                &system.coordinates,
                &system.writers,
                &system.certificate,
                installed,
            )?;
            templates_checked = templates_checked.saturating_add(1);
        }
        if installed
            .guards(&WriterSource::Action(BoundAction::of(action)))
            .is_some()
        {
            installed_pins_matched = installed_pins_matched.saturating_add(1);
        }
        actions_checked = actions_checked.saturating_add(1);
    }

    Ok(QuantityActionsCertification {
        actions_checked,
        templates_checked,
        installed_pins_matched,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "exact conservation auto-lowering unit tests"
)]
mod tests {
    use super::*;
    use crate::quantity_ledger::compile_ledger_pack;
    use norn_ir::quantity_ledger::{
        LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerFundingPolicy, LedgerLayerIr, LedgerLegIr,
        LedgerOntologyArtifactPin, LedgerParameterIr, LedgerTemplateIr, LedgerUnitIr,
        LinearAmountIr, LinearTermIr,
    };
    use norn_ir::{ActionQuantityEffect, ActionQuantityParameter, WholeNumber};

    fn ontology_pin() -> LedgerOntologyArtifactPin {
        LedgerOntologyArtifactPin {
            artifact_id: "norn.ontology-pack.quantity-units".to_owned(),
            artifact_version: "0.1.0".to_owned(),
            artifact_sha256: "45be55780443fa2ece436ba8ab89107dd4ffcef5388beb3d332142c033ecf7d1"
                .to_owned(),
        }
    }

    fn unit(unit_id: &str, precision: u8) -> LedgerUnitIr {
        LedgerUnitIr {
            unit_id: unit_id.to_owned(),
            unit_term_id: format!("norn.measure.unit.{unit_id}"),
            quantity_kind_term_id: format!("norn.measure.kind.{unit_id}"),
            precision,
        }
    }

    fn account(account_id: &str, units: &[&str]) -> LedgerAccountIr {
        LedgerAccountIr {
            account_id: account_id.to_owned(),
            allowed_unit_ids: units.iter().map(|u| (*u).to_owned()).collect(),
            funding: LedgerFundingPolicy::Gross,
        }
    }

    fn param(id: &str, scale: u8) -> LedgerParameterIr {
        LedgerParameterIr {
            parameter_id: id.to_owned(),
            scale,
            min_unscaled: "-1000000".to_owned(),
            max_unscaled: "1000000".to_owned(),
        }
    }

    fn linear(result_scale: u8, constant: &str, terms: &[(&str, &str, u8)]) -> LinearAmountIr {
        LinearAmountIr {
            result_scale,
            constant_unscaled: constant.to_owned(),
            constant_scale: 0,
            terms: terms
                .iter()
                .map(|(parameter_id, coeff, scale)| LinearTermIr {
                    parameter_id: (*parameter_id).to_owned(),
                    coefficient_unscaled: (*coeff).to_owned(),
                    coefficient_scale: *scale,
                })
                .collect(),
        }
    }

    fn melt_pack(loss_output_coeff: &str, extra_constant: &str) -> LedgerPackIr {
        LedgerPackIr {
            ir_version: LEDGER_PACK_IR_VERSION.to_owned(),
            pack_id: "jewelry.melt".to_owned(),
            ontology_artifact: ontology_pin(),
            units: vec![unit("fine_g", 3), unit("USD", 2)],
            accounts: vec![
                account("melting", &["fine_g"]),
                account("vault", &["fine_g"]),
                account("process_loss", &["fine_g"]),
                account("wip_cost", &["USD"]),
                account("inventory_cost", &["USD"]),
            ],
            layers: vec![
                LedgerLayerIr {
                    layer_id: "posted".to_owned(),
                },
                LedgerLayerIr {
                    layer_id: "pending".to_owned(),
                },
            ],
            templates: vec![LedgerTemplateIr {
                template_id: "melt_post".to_owned(),
                parameters: vec![
                    param("input_g", 3),
                    param("output_g", 3),
                    param("cost_usd", 2),
                ],
                legs: vec![
                    LedgerLegIr {
                        leg_id: "consume_melt".to_owned(),
                        account_id: "melting".to_owned(),
                        unit_id: "fine_g".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(3, "0", &[("input_g", "-1", 0)]),
                    },
                    LedgerLegIr {
                        leg_id: "produce_vault".to_owned(),
                        account_id: "vault".to_owned(),
                        unit_id: "fine_g".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(3, "0", &[("output_g", "1", 0)]),
                    },
                    LedgerLegIr {
                        leg_id: "record_loss".to_owned(),
                        account_id: "process_loss".to_owned(),
                        unit_id: "fine_g".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(
                            3,
                            extra_constant,
                            &[("input_g", "1", 0), ("output_g", loss_output_coeff, 0)],
                        ),
                    },
                    LedgerLegIr {
                        leg_id: "cr_wip".to_owned(),
                        account_id: "wip_cost".to_owned(),
                        unit_id: "USD".to_owned(),
                        layer: "pending".to_owned(),
                        amount: linear(2, "0", &[("cost_usd", "-1", 0)]),
                    },
                    LedgerLegIr {
                        leg_id: "dr_inventory".to_owned(),
                        account_id: "inventory_cost".to_owned(),
                        unit_id: "USD".to_owned(),
                        layer: "pending".to_owned(),
                        amount: linear(2, "0", &[("cost_usd", "1", 0)]),
                    },
                ],
            }],
        }
    }

    fn base_action(action_id: &str, quantity_effect: Option<ActionQuantityEffect>) -> ActionRoot {
        let mut root: ActionRoot = serde_json::from_value(serde_json::json!({
            "actionId": action_id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": false,
            "ledgered": true,
            "inputSchema": {"type": "object"},
            "outputSchema": {"type": "object"},
        }))
        .unwrap();
        root.quantity_effect = quantity_effect;
        root
    }

    #[test]
    fn cons_ledger_certifies_balanced_multi_leg_multi_partition_pack() {
        let pack = melt_pack("-1", "0");
        let compiled = compile_ledger_pack(&pack).expect("valid pack compiles");
        let cert = compiled
            .certify_conservation()
            .expect("balanced pack certifies");
        assert_eq!(cert.pack_id, "jewelry.melt");
        assert_eq!(cert.pack_sha256, None);
        assert_eq!(cert.templates_checked, 1);
        assert_eq!(cert.coordinates_checked, 5);
        assert_eq!(cert.partitions_checked, 2);
        let ir_cert = certify_ledger_pack_ir(&pack).expect("ir pack certifies");
        assert_eq!(
            ir_cert.pack_sha256.as_deref(),
            Some(ledger_pack_sha256(&pack).as_str())
        );
        assert_eq!(ir_cert.templates_checked, 1);
        assert_eq!(ir_cert.coordinates_checked, 5);
        assert_eq!(ir_cert.partitions_checked, 2);
    }

    #[test]
    fn cons_ledger_refuses_unbalanced_constant_and_coefficient() {
        // Constant residual of +1_000 (1 * 10^3 at precision 3) on fine_g/posted.
        let bad_constant = melt_pack("-1", "1");
        let err = certify_ledger_pack_ir(&bad_constant).unwrap_err();
        assert!(
            matches!(
                err,
                Failure::Unbalanced {
                    ref writer,
                    column: 3,
                    residual,
                    ..
                } if writer == "melt_post" && residual == Rational::new(1_000, 1).unwrap()
            ),
            "expected unbalanced constant counterexample, got {err:?}"
        );

        // Coefficient residual for output_g (sorted index 2: cost_usd=0, input_g=1, output_g=2).
        let bad_coeff = melt_pack("0", "0");
        let err = certify_ledger_pack_ir(&bad_coeff).unwrap_err();
        assert!(
            matches!(
                err,
                Failure::Unbalanced {
                    ref writer,
                    column: 2,
                    residual,
                    ..
                } if writer == "melt_post" && residual == Rational::new(1, 1).unwrap()
            ),
            "expected unbalanced coefficient counterexample, got {err:?}"
        );
    }

    #[test]
    fn cons_ledger_keeps_unit_and_layer_partitions_disjoint() {
        // Cross-layer mismatch: cr_wip on pending, dr_inventory on posted -> must not cancel.
        let mut cross_layer = melt_pack("-1", "0");
        cross_layer.templates[0].legs[4].layer = "posted".to_owned();
        assert!(matches!(
            certify_ledger_pack_ir(&cross_layer),
            Err(Failure::Unbalanced { .. })
        ));

        // Cross-unit mismatch: two accounts admitting different units with opposite deltas.
        let mut cross_unit = melt_pack("-1", "0");
        cross_unit.accounts[3]
            .allowed_unit_ids
            .push("fine_g".to_owned());
        cross_unit.templates[0].legs[3].unit_id = "fine_g".to_owned();
        cross_unit.templates[0].legs[3].amount = linear(3, "0", &[("cost_usd", "-1", 0)]);
        assert!(matches!(
            certify_ledger_pack_ir(&cross_unit),
            Err(Failure::Unbalanced { .. })
        ));
    }

    #[test]
    fn cons_ledger_fails_closed_unknown_on_i128_to_i64_overflow() {
        let huge = i128::from(i64::MAX).checked_add(1).unwrap();
        let neg_huge = huge.checked_neg().unwrap();
        let pack = LedgerPackIr {
            ir_version: LEDGER_PACK_IR_VERSION.to_owned(),
            pack_id: "pack.overflow".to_owned(),
            ontology_artifact: ontology_pin(),
            units: vec![unit("units", 0)],
            accounts: vec![account("a", &["units"]), account("b", &["units"])],
            layers: vec![LedgerLayerIr {
                layer_id: "posted".to_owned(),
            }],
            templates: vec![LedgerTemplateIr {
                template_id: "huge_move".to_owned(),
                parameters: Vec::new(),
                legs: vec![
                    LedgerLegIr {
                        leg_id: "out".to_owned(),
                        account_id: "a".to_owned(),
                        unit_id: "units".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(0, &neg_huge.to_string(), &[]),
                    },
                    LedgerLegIr {
                        leg_id: "in".to_owned(),
                        account_id: "b".to_owned(),
                        unit_id: "units".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(0, &huge.to_string(), &[]),
                    },
                ],
            }],
        };
        // Fits i128 cleanly so compile_ledger_pack succeeds, but must fail closed as Unknown on i64 Rational.
        let compiled = compile_ledger_pack(&pack).expect("i128 pack compiles");
        assert!(matches!(
            certify_ledger_pack(&compiled),
            Err(Failure::Unknown(ref reason)) if reason.contains("i64 rational representation")
        ));
    }

    #[test]
    fn cons_ledger_certifies_reserve_and_post_reservation_actions() {
        let pack = compile_ledger_pack(&melt_pack("-1", "0")).unwrap();
        let ledgers = BTreeMap::from([("metal-ledger".to_owned(), pack)]);

        let reserve = base_action(
            "jewelry.reserve_melt",
            Some(ActionQuantityEffect {
                ledger_id: "metal-ledger".to_owned(),
                balance_versions_field: "balanceVersions".to_owned(),
                operation: ActionQuantityOperation::Reserve {
                    reservation_id_field: "reservationId".to_owned(),
                    template_id: "melt_post".to_owned(),
                    parameters: BTreeMap::from([
                        (
                            "input_g".to_owned(),
                            ActionQuantityParameter {
                                field: "inputGrams".to_owned(),
                                scale: WholeNumber(3),
                            },
                        ),
                        (
                            "output_g".to_owned(),
                            ActionQuantityParameter {
                                field: "outputGrams".to_owned(),
                                scale: WholeNumber(3),
                            },
                        ),
                        (
                            "cost_usd".to_owned(),
                            ActionQuantityParameter {
                                field: "costUsd".to_owned(),
                                scale: WholeNumber(2),
                            },
                        ),
                    ]),
                    ttl_ms: WholeNumber(60_000),
                },
            }),
        );
        let post = base_action(
            "jewelry.post_melt",
            Some(ActionQuantityEffect {
                ledger_id: "metal-ledger".to_owned(),
                balance_versions_field: "balanceVersions".to_owned(),
                operation: ActionQuantityOperation::PostReservation {
                    reservation_id_field: "reservationId".to_owned(),
                    posting_id_field: "postingId".to_owned(),
                },
            }),
        );
        let non_quantity = base_action("jewelry.note_only", None);

        let installed = InstalledActions::pin(&[reserve.clone(), post.clone()]).unwrap();
        let cert = certify_quantity_actions(
            &[reserve.clone(), post.clone(), non_quantity],
            &ledgers,
            &installed,
        )
        .expect("reserve + post_reservation certify");
        assert_eq!(cert.actions_checked, 2);
        assert_eq!(cert.templates_checked, 2);
        assert_eq!(cert.installed_pins_matched, 2);

        // Scale mismatch on Reserve parameter fails closed as Malformed.
        let mut bad_scale = reserve.clone();
        if let Some(ActionQuantityEffect {
            operation: ActionQuantityOperation::Reserve { parameters, .. },
            ..
        }) = &mut bad_scale.quantity_effect
        {
            parameters.get_mut("input_g").unwrap().scale = WholeNumber(2);
        }
        assert_eq!(
            certify_quantity_actions(&[bad_scale], &ledgers, &InstalledActions::none()),
            Err(Failure::Malformed)
        );

        // Unknown ledger fails closed as Unknown.
        assert!(matches!(
            certify_quantity_actions(&[reserve], &BTreeMap::new(), &InstalledActions::none()),
            Err(Failure::Unknown(ref reason)) if reason.contains("unknown ledger")
        ));

        // PostReservation against an unbalanced pack fails closed as Unbalanced.
        let unbalanced_compiled =
            normalize_ledger_pack_for_conservation(&melt_pack("0", "0")).unwrap();
        let bad_ledgers = BTreeMap::from([("metal-ledger".to_owned(), unbalanced_compiled)]);
        assert!(matches!(
            certify_quantity_actions(&[post], &bad_ledgers, &InstalledActions::none()),
            Err(Failure::Unbalanced { ref writer, .. }) if writer == "jewelry.post_melt"
        ));
    }

    #[test]
    fn cons_ledger_accumulates_multiple_legs_on_same_account() {
        let pack = LedgerPackIr {
            ir_version: LEDGER_PACK_IR_VERSION.to_owned(),
            pack_id: "pack.multi_leg_same_acct".to_owned(),
            ontology_artifact: ontology_pin(),
            units: vec![unit("USD", 2)],
            accounts: vec![account("cash", &["USD"]), account("revenue", &["USD"])],
            layers: vec![LedgerLayerIr {
                layer_id: "posted".to_owned(),
            }],
            templates: vec![LedgerTemplateIr {
                template_id: "split_cash".to_owned(),
                parameters: vec![param("amt", 2)],
                legs: vec![
                    LedgerLegIr {
                        leg_id: "cash_1".to_owned(),
                        account_id: "cash".to_owned(),
                        unit_id: "USD".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(2, "0", &[("amt", "1", 0)]),
                    },
                    LedgerLegIr {
                        leg_id: "cash_2".to_owned(),
                        account_id: "cash".to_owned(),
                        unit_id: "USD".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(2, "0", &[("amt", "2", 0)]),
                    },
                    LedgerLegIr {
                        leg_id: "rev".to_owned(),
                        account_id: "revenue".to_owned(),
                        unit_id: "USD".to_owned(),
                        layer: "posted".to_owned(),
                        amount: linear(2, "0", &[("amt", "-3", 0)]),
                    },
                ],
            }],
        };
        let compiled = compile_ledger_pack(&pack).unwrap();
        let cert = certify_ledger_pack(&compiled).expect("same-account legs accumulate cleanly");
        assert_eq!(cert.coordinates_checked, 2);
        assert_eq!(cert.partitions_checked, 1);
    }
}
