//! TRANSLATION-PRESERVES-1: `Valid_S and Admit => Valid_T(f(x)) and O_T(f(x)) = O_S(x)`.
//!
//! Each goal lowers to a counterexample query and a non-vacuity query in
//! `QF_UFLIA`. Decimals are integer coefficients at one common scale and unit,
//! and text is an uninterpreted identity. Queries omit `(check-sat)`, which
//! belongs to the solver driver. Anything else stays a visible Unknown reason.
use super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource, finite, transition};
use crate::{EffectiveCatalog, EffectiveKind, EffectiveObjectType, EffectiveProperty};
use norn_ir::verification::translation::TranslationStatus;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ConstraintClass, EntityRootV2, LawIR, LawModality, LawResponse,
    PropertyConstraints, PropertyType, UnitRef, ZenExpression,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// What every proof here assumes about the runtime it models.
pub const ASSUMPTIONS: [&str; 3] = [
    "decimal values are exact and unbounded: no overflow or rounding",
    "text is an identity compared only by equality",
    "stored records satisfy their integrity invariants",
];

/// The two queries of one goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queries {
    /// `sat` is a counterexample; `unsat` is preservation.
    pub preservation: String,
    /// `sat` is an admitted valid source; `unsat` makes the goal vacuous.
    pub non_vacuity: String,
}

/// Every translation goal of a freshly reconstructed challenge.
#[derive(Debug)]
pub struct Batch {
    /// Fresh checker-owned challenge.
    pub challenge: Challenge,
    /// Law artifact and queries or Unknown reason, in challenge order.
    pub goals: Vec<(String, Result<Queries, String>)>,
}

/// One solver answer; model text is opaque here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Satisfiable, with the solver's model.
    Sat(String),
    /// Exactly `unsat`.
    Unsat,
    /// No usable answer.
    Unknown(String),
}

/// A proof needs both answers; a refutation needs only its own model.
#[must_use]
pub fn classify(preservation: Answer, non_vacuity: Answer) -> TranslationStatus {
    match (preservation, non_vacuity) {
        (Answer::Sat(counterexample), _) => TranslationStatus::Refuted { counterexample },
        (Answer::Unsat, Answer::Sat(witness)) => TranslationStatus::ProvedUnderAssumptions {
            assumptions: ASSUMPTIONS.map(str::to_owned).to_vec(),
            witness,
        },
        (Answer::Unsat, Answer::Unsat) => TranslationStatus::Unsupported {
            reason: "vacuous: no admitted valid source exists".into(),
        },
        (Answer::Unknown(reason), _) | (Answer::Unsat, Answer::Unknown(reason)) => {
            TranslationStatus::Unsupported { reason }
        }
    }
}

impl ApprovedContract {
    /// TRANSLATION-PRESERVES-1: lower only goals of the reconstructed challenge.
    /// # Errors
    /// Returns the ordinary admission/challenge error, never a solver verdict.
    pub fn translation(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
    ) -> Result<Batch, ChallengeError> {
        let challenge = self.challenge(source, registry)?;
        let pool: Vec<_> = source
            .base
            .iter()
            .chain(&source.artifacts)
            .cloned()
            .collect();
        let catalog = EffectiveCatalog::from_artifacts_with_registry(&pool, registry);
        let laws = challenge.body().get("lawStatements");
        let goals = challenge
            .body()
            .get("obligations")
            .and_then(Value::as_array)
            .ok_or(ChallengeError::Closure)?
            .iter()
            .filter(|goal| {
                let kind = goal.get("goal").and_then(|goal| goal.get("kind"));
                kind.and_then(Value::as_str) == Some("translation_preserves")
            })
            .map(|goal| {
                let id = goal
                    .get("law")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| laws?.get(index)?.get("artifactId")?.as_str())
                    .ok_or(ChallengeError::Closure)?;
                Ok((id.to_owned(), lower(&pool, &catalog, id)))
            })
            .collect::<Result<_, ChallengeError>>()?;
        Ok(Batch { challenge, goals })
    }
}

type Lowered<T> = Result<T, String>;

fn demand(ok: bool, reason: &str) -> Lowered<()> {
    if ok { Ok(()) } else { Err(reason.into()) }
}

fn integer(value: i128) -> String {
    if value < 0 {
        format!("(- {})", value.unsigned_abs())
    } else {
        value.to_string()
    }
}

fn number(source: &str, symbols: &BTreeSet<String>) -> Lowered<String> {
    let value = transition::affine(source, symbols)?;
    let terms: Vec<_> = value
        .coefficients
        .iter()
        .map(|(symbol, factor)| format!("(* {} {symbol})", integer(*factor)))
        .collect();
    Ok(format!(
        "(+ {} 0 {})",
        integer(value.constant),
        terms.join(" ")
    ))
}

/// The field environment of one side of the translation.
#[derive(Default)]
struct Side {
    numbers: BTreeMap<String, String>,
    identities: BTreeMap<String, String>,
}

impl Side {
    fn scaled(&self, source: &str, scale: u32) -> Lowered<String> {
        demand(!source.contains("input."), "translation reads only self")?;
        finite::expression(source, &self.numbers, scale)
    }
    /// An identity is a whole-field read; everything else must be arithmetic.
    fn value(&self, source: &str, scale: u32, symbols: &BTreeSet<String>) -> Lowered<String> {
        let field = source.trim().strip_prefix("self.").unwrap_or_default();
        match self.identities.get(field) {
            Some(identity) => Ok(identity.clone()),
            None => number(&self.scaled(source, scale)?, symbols),
        }
    }
    /// A conjunction of comparisons; any other Boolean shape stays Unknown.
    fn predicate(
        &self,
        source: &str,
        scale: u32,
        symbols: &BTreeSet<String>,
    ) -> Lowered<Vec<String>> {
        self.scaled(source, scale)?
            .split(" and ")
            .map(|part| {
                let (operator, left, right) = ["==", "!=", "<=", ">=", "<", ">"]
                    .into_iter()
                    .find_map(|op| {
                        let (left, right) = part.split_once(&format!(" {op} "))?;
                        Some((op, left, right))
                    })
                    .ok_or("predicate is not a conjunction of comparisons")?;
                let (left, right) = (number(left, symbols)?, number(right, symbols)?);
                Ok(match operator {
                    "==" => format!("(= {left} {right})"),
                    "!=" => format!("(not (= {left} {right}))"),
                    _ => format!("({operator} {left} {right})"),
                })
            })
            .collect()
    }
}

/// `Some` scale and unit for a fixed decimal, `None` for text.
fn numeric(property: &EffectiveProperty) -> Lowered<Option<(u32, Option<UnitRef>)>> {
    demand(
        !property.computed
            && !property.nullable
            && property
                .constraints
                .iter()
                .all(PropertyConstraints::is_empty),
        "computed, nullable or constrained property is unsupported",
    )?;
    let EffectiveKind::Base(kind) = &property.kind else {
        return Err("unresolved property type".into());
    };
    if matches!(kind, PropertyType::Text { .. }) {
        return Ok(None);
    }
    let PropertyType::Decimal {
        scale,
        unit,
        rounding: None,
        currency_source: None,
        per_unit: None,
        ..
    } = kind
    else {
        return Err(format!(
            "property {} is outside the fragment",
            property.id.0
        ));
    };
    let scale = u32::try_from(scale.0).map_err(|_error| "decimal scale")?;
    Ok(Some((scale, unit.clone())))
}

/// Integrity invariants of a whole lineage; a missing owner refuses.
fn invariants(
    pool: &[ArtifactEnvelope],
    object: &EffectiveObjectType,
) -> Lowered<Vec<ZenExpression>> {
    if !object.has_invariants {
        return Ok(Vec::new());
    }
    demand(
        object.interfaces.is_empty(),
        "interface invariants are unsupported",
    )?;
    let mut found = Vec::new();
    let mut owners = 0_usize;
    for artifact in pool.iter().filter(|a| a.kind() == ArtifactKind::Entity) {
        let root: Option<EntityRootV2> = serde_json::to_value(artifact.payload.to_json_value())
            .ok()
            .and_then(|value| serde_json::from_value(value).ok());
        let Some(def) = root.map(|root| root.object_type) else {
            continue;
        };
        if object.lineage.contains(&def.name) {
            owners = owners.saturating_add(1);
            found.extend(
                def.invariants
                    .into_iter()
                    .filter(|i| i.classification == ConstraintClass::Integrity)
                    .map(|i| i.expression),
            );
        }
    }
    demand(owners == object.lineage.len(), "invariant owner unresolved")?;
    Ok(found)
}

/// The target side: each stored property holds its assignment over the source.
fn translated(
    before: &Side,
    target: &EffectiveObjectType,
    assignments: &BTreeMap<String, ZenExpression>,
    common: Option<&(u32, Option<UnitRef>)>,
    symbols: &BTreeSet<String>,
) -> Lowered<Side> {
    let scale = common.ok_or("source has no decimal property")?.0;
    let mut after = Side::default();
    for property in &target.properties {
        let expression = assignments
            .get(property.id.as_str())
            .ok_or("translation is not total over the target")?;
        let id = property.id.0.clone();
        if let Some(contract) = numeric(property)? {
            demand(
                common == Some(&contract),
                "mixed scales or units are unsupported",
            )?;
            let value = before.scaled(expression.as_str(), scale)?;
            number(&value, symbols)?;
            after.numbers.insert(id, format!("({value})"));
        } else {
            let field = expression.as_str().trim().strip_prefix("self.");
            let identity = field.and_then(|field| before.identities.get(field));
            let identity = identity.ok_or("text target needs a whole text field")?;
            after.identities.insert(id, identity.clone());
        }
    }
    Ok(after)
}

fn lower(pool: &[ArtifactEnvelope], catalog: &EffectiveCatalog, id: &str) -> Lowered<Queries> {
    let artifact = pool
        .iter()
        .find(|a| a.kind() == ArtifactKind::Law && a.artifact_id == id)
        .ok_or("governing law artifact absent")?;
    let law: LawIR = serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .map_err(|_error| "law payload")?;
    let LawResponse::TranslationPreserves {
        target_type,
        assignments,
        observation,
    } = &law.response
    else {
        return Err("not a translation law".into());
    };
    demand(
        matches!(law.modality, LawModality::Always {})
            && law.timing.is_none()
            && law.scope.object_types == [law.subject.as_str()]
            && law.scope.lifecycle_states.is_empty()
            && law.scope.commitments.is_empty(),
        "unsupported law scope or timing",
    )?;
    let object = |name: &str| {
        catalog
            .object_type_named(name)
            .and_then(Result::ok)
            .ok_or_else(|| format!("effective object {name} missing"))
    };
    let (source, target) = (object(&law.subject)?, object(target_type)?);
    demand(
        !target.has_links && target.lifecycles.is_empty() && target.owned_collections.is_empty(),
        "target links, lifecycles and collections are unsupported",
    )?;
    demand(
        assignments.len() == target.properties.len(),
        "translation is not total over the target",
    )?;
    let mut common = None;
    let mut before = Side::default();
    let mut lines = vec![
        "(set-logic QF_UFLIA)".to_owned(),
        "(declare-sort Id 0)".to_owned(),
    ];
    let mut symbols = BTreeSet::new();
    for (index, property) in source.properties.iter().enumerate() {
        let symbol = format!("s{index}");
        let side = if let Some(contract) = numeric(property)? {
            demand(
                common.get_or_insert_with(|| contract.clone()) == &contract,
                "mixed scales or units are unsupported",
            )?;
            lines.push(format!("(declare-const {symbol} Int)"));
            symbols.insert(symbol.clone());
            &mut before.numbers
        } else {
            lines.push(format!("(declare-const {symbol} Id)"));
            &mut before.identities
        };
        side.insert(property.id.0.clone(), symbol);
    }
    let (scale, _unit) = common.clone().ok_or("source has no decimal property")?;
    let after = translated(&before, target, assignments, common.as_ref(), &symbols)?;
    for invariant in invariants(pool, source)?.iter().chain(&law.condition) {
        for premise in before.predicate(invariant.as_str(), scale, &symbols)? {
            lines.push(format!("(assert {premise})"));
        }
    }
    let mut conclusions = Vec::new();
    for invariant in invariants(pool, target)? {
        conclusions.extend(after.predicate(invariant.as_str(), scale, &symbols)?);
    }
    let expected = before.value(observation.source.as_str(), scale, &symbols)?;
    let observed = after.value(observation.target.as_str(), scale, &symbols)?;
    demand(
        expected.starts_with('(') == observed.starts_with('('),
        "observation compares a number with an identity",
    )?;
    conclusions.push(format!("(= {observed} {expected})"));
    let non_vacuity = lines.join("\n");
    Ok(Queries {
        preservation: format!(
            "{non_vacuity}\n(assert (not (and true {})))",
            conclusions.join(" ")
        ),
        non_vacuity,
    })
}
