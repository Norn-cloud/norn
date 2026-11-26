//! PROOF-LIFECYCLE-2: exact frozen inputs for conditional chart relations, never evidence.
use super::super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::{EffectiveCatalog, EffectiveKind, EffectiveLifecycle, EffectiveObjectType};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ConstraintClass, EntityRoot, Invariant, LawIR, PropertyType,
    RuntimeValue,
};
use std::collections::{BTreeMap, BTreeSet};

/// Exact stored scalar values, keyed by declaration property ID.
pub type Row = BTreeMap<String, RuntimeValue>;
/// A complete finite profile, never a sample of a larger interval.
#[derive(Debug, Clone)]
pub struct Bounds {
    /// Selected effective concrete type.
    pub object_type: String,
    /// Selected lifecycle property; its enum is never clipped.
    pub property: String,
    /// Exact JSON scalar literals for every other stored field.
    pub frozen: BTreeMap<String, Vec<String>>,
    /// Reproducibility pin, never a sampling limit.
    pub seed: u64,
}
/// Declaration-bound inputs to a future relation consumer; no transition verdicts.
#[derive(Debug)]
pub struct Snapshot {
    /// Concrete subject, including inherited properties and guards.
    pub object_type: String,
    /// Complete effective chart in declaration order.
    pub machine: EffectiveLifecycle,
    /// Effective contracts, preserving scale, units and scalar kinds.
    pub fields: BTreeMap<String, PropertyType>,
    /// Every bounded valuation, including states a governing law may refuse.
    pub nodes: Vec<Row>,
    /// Applicable governing laws, retaining artifact identities and complete `LawIR`.
    pub laws: Vec<(String, LawIR)>,
    /// Effective integrity invariants (owned by the subject's lineage or implemented
    /// interfaces); quality invariants do not refuse native transitions.
    pub invariants: Vec<(String, Invariant)>,
    /// Subject-bound `ZenTypeEnv` for preparing and evaluating integrity
    /// invariants under the native exact profile (`evaluate_prepared`).
    pub invariant_env: Option<norn_expr::ZenTypeEnv>,
}
/// Fresh source identity and exact finite inputs, never checked evidence.
#[derive(Debug)]
pub struct Batch {
    /// Complete regenerated obligation inventory.
    pub challenge: Challenge,
    /// Exact bounds and declaration interpretation for subsequent pins.
    pub bounds_json: String,
    /// Unsupported input profiles remain visible Unknown reasons.
    pub snapshot: Result<Snapshot, String>,
}
fn require(ok: bool, reason: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(reason.into()) }
}
fn freeze(
    object: &EffectiveObjectType,
    bounds: &Bounds,
    laws: Vec<(String, LawIR)>,
    invariants: Vec<(String, Invariant)>,
    invariant_env: Option<norn_expr::ZenTypeEnv>,
) -> Result<Snapshot, String> {
    require(
        object.lifecycles.len() == 1,
        "requires one isolated effective lifecycle",
    )?;
    require(
        object.owned_collections.is_empty(),
        "owned child collections are outside the conditional profile",
    )?;
    let machine = object
        .lifecycle(&bounds.property)
        .ok_or("unknown lifecycle property")?;
    require(
        !machine.states.is_empty()
            && machine.states.len() <= 128
            && machine.lifecycle.transitions.len() <= 128,
        "chart exceeds finite profile",
    )?;
    require(object.properties.len() <= 16, "more than 16 stored fields")?;
    let state_bytes = machine
        .states
        .iter()
        .map(|state| state.len().saturating_mul(6))
        .max()
        .unwrap_or(0);
    let row_bytes = state_bytes
        .saturating_add(bounds.property.len().saturating_mul(6))
        .saturating_add(32);
    require(
        row_bytes.saturating_mul(machine.states.len()) <= 4_194_304,
        "finite snapshot byte budget exceeded",
    )?;
    let mut rows = vec![Row::new()];
    let mut fields = BTreeMap::new();
    for property in &object.properties {
        require(!property.computed, "computed scalar context is unsupported")?;
        let EffectiveKind::Base(kind) = &property.kind else {
            return Err("unresolved scalar context type".into());
        };
        require(
            matches!(
                kind,
                PropertyType::Text { .. }
                    | PropertyType::Integer { .. }
                    | PropertyType::Decimal { .. }
                    | PropertyType::Boolean {}
                    | PropertyType::Enum { .. }
                    | PropertyType::Date { .. }
                    | PropertyType::Timestamp { .. }
                    | PropertyType::Duration { .. }
            ),
            "unsupported frozen scalar kind",
        )?;
        fields.insert(property.id.as_str().to_owned(), kind.clone());
        if property.id.as_str() == bounds.property {
            require(
                !property.protected(),
                "rule-owned lifecycle property is outside profile",
            )?;
            for state in &machine.states {
                property
                    .check_runtime_value(&RuntimeValue::String(state.clone()))
                    .map_err(|error| format!("{error:?}"))?;
            }
            continue;
        }
        rows = extend_rows(
            rows,
            property,
            bounds
                .frozen
                .get(property.id.as_str())
                .ok_or("missing explicit frozen scalar domain")?,
            machine.states.len(),
            row_bytes,
        )?;
    }
    let mut nodes = Vec::new();
    for row in rows {
        for state in &machine.states {
            let mut next = row.clone();
            next.insert(bounds.property.clone(), RuntimeValue::String(state.clone()));
            nodes.push(next);
        }
    }
    Ok(Snapshot {
        object_type: object.name.as_str().to_owned(),
        machine: machine.clone(),
        fields,
        nodes,
        laws,
        invariants,
        invariant_env,
    })
}

/// Build the subject-bound `ZenTypeEnv` matching native `TypeCatalog` so
/// integrity invariants run through `evaluate_prepared` (exact Decimal profile).
fn build_invariant_env(
    pool: &[ArtifactEnvelope],
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
    object: &EffectiveObjectType,
) -> Result<norn_expr::ZenTypeEnv, String> {
    let mut property_types = Vec::new();
    let mut interfaces = Vec::new();
    let mut object_types = Vec::new();
    let mut extensions = Vec::new();
    for artifact in pool.iter().filter(|a| a.kind() == ArtifactKind::Entity) {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        match *root {
            EntityRoot::V2(root) => object_types.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2Extension(root) => extensions.push(root.object_type_extension),
            EntityRoot::V2PropertyTypes(root) => property_types.extend(root.property_types),
            EntityRoot::V1(_) | EntityRoot::V2Markings(_) => {}
        }
    }
    let definitions = norn_expr::EntityDefinitions {
        property_types: &property_types,
        object_types: &object_types,
        interfaces: &interfaces,
        extensions: &extensions,
    };
    norn_expr::ZenTypeEnv::from_artifact_set_for_checking(
        &definitions,
        registry,
        norn_expr::EnvOptions::default(),
    )
    .map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?
    .bind_self(&object.name)
    .map_err(|error| error.to_string())
}

/// Collect every integrity invariant declared by `object`'s lineage or
/// implemented interfaces (including transitive parent interfaces). Fails
/// closed if any owner in `lineage` or the interface closure cannot be
/// resolved in `pool`. Quality invariants do not refuse native writes.
fn integrity_invariants(
    pool: &[ArtifactEnvelope],
    object: &EffectiveObjectType,
) -> Result<Vec<(String, Invariant)>, String> {
    if !object.has_invariants {
        return Ok(Vec::new());
    }
    let mut by_type: BTreeMap<String, Vec<Invariant>> = BTreeMap::new();
    let mut by_interface: BTreeMap<String, (Vec<String>, Vec<Invariant>)> = BTreeMap::new();
    for artifact in pool.iter().filter(|a| a.kind() == ArtifactKind::Entity) {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        match *root {
            EntityRoot::V2(root) => {
                by_type.insert(root.object_type.name.0, root.object_type.invariants);
            }
            EntityRoot::V2Interface(root) => {
                by_interface.insert(
                    root.interface.name.0,
                    (
                        root.interface
                            .extends
                            .into_iter()
                            .map(|name| name.0)
                            .collect(),
                        root.interface.invariants,
                    ),
                );
            }
            EntityRoot::V1(_)
            | EntityRoot::V2PropertyTypes(_)
            | EntityRoot::V2Extension(_)
            | EntityRoot::V2Markings(_) => {}
        }
    }
    let mut out = Vec::new();
    for owner in &object.lineage {
        let invariants = by_type
            .get(owner.as_str())
            .ok_or_else(|| format!("unresolved invariant owner {}", owner.as_str()))?;
        for invariant in invariants {
            if invariant.classification == ConstraintClass::Integrity {
                out.push((owner.as_str().to_owned(), invariant.clone()));
            }
        }
    }
    let mut visited = BTreeSet::new();
    let mut pending: Vec<String> = object
        .interfaces
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    while let Some(owner) = pending.pop() {
        if !visited.insert(owner.clone()) {
            continue;
        }
        let (parents, invariants) = by_interface
            .get(&owner)
            .ok_or_else(|| format!("unresolved interface invariant owner {owner}"))?;
        for invariant in invariants {
            if invariant.classification == ConstraintClass::Integrity {
                out.push((owner.clone(), invariant.clone()));
            }
        }
        for parent in parents {
            if !visited.contains(parent) {
                pending.push(parent.clone());
            }
        }
    }
    Ok(out)
}

fn extend_rows(
    rows: Vec<Row>,
    property: &crate::EffectiveProperty,
    literals: &[String],
    states: usize,
    row_bytes: usize,
) -> Result<Vec<Row>, String> {
    let count = rows
        .len()
        .saturating_mul(literals.len())
        .saturating_mul(states);
    require(
        !literals.is_empty() && count <= 4096,
        "empty or excessive frozen context domain (4096 nodes)",
    )?;
    require(
        count.saturating_mul(row_bytes) <= 4_194_304,
        "finite snapshot byte budget exceeded",
    )?;
    let values = literals
        .iter()
        .map(|literal| RuntimeValue::from_json_text(literal).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    for value in &values {
        property
            .check_runtime_value(value)
            .map_err(|error| format!("{error:?}"))?;
        require(
            property.canonical_runtime_value(value) == *value,
            "frozen scalar requires storage materialization",
        )?;
    }
    let mut expanded = Vec::new();
    let mut bytes = 0_usize;
    for row in rows {
        for value in &values {
            let mut next = row.clone();
            next.insert(property.id.as_str().to_owned(), value.clone());
            let encoded = norn_canonical::serialize_json_verbatim(&next)
                .map_err(|error| error.to_string())?;
            bytes = bytes.saturating_add(encoded.len().saturating_add(row_bytes));
            require(
                bytes.saturating_mul(states) <= 4_194_304,
                "finite snapshot byte budget exceeded",
            )?;
            expanded.push(next);
        }
    }
    Ok(expanded)
}

impl ApprovedContract {
    /// Reconstruct admission and exact scalar inputs before any relation or solver runs.
    /// # Errors
    /// Admission/challenge or bounds-document resource refusal; no solver verdict.
    pub fn lifecycle_inputs(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
        bounds: &Bounds,
    ) -> Result<Batch, ChallengeError> {
        let challenge = self.challenge(source, registry)?;
        if bounds
            .object_type
            .len()
            .saturating_add(bounds.property.len())
            > 10_000
            || bounds.frozen.len() > 16
            || bounds.frozen.values().any(|values| values.len() > 4096)
        {
            return Err(ChallengeError::Limit);
        }
        let raw_bytes = bounds.frozen.iter().fold(0_usize, |total, (name, values)| {
            total.saturating_add(name.len()).saturating_add(
                values
                    .iter()
                    .map(String::len)
                    .fold(0_usize, usize::saturating_add),
            )
        });
        if raw_bytes > 10_000 {
            return Err(ChallengeError::Limit);
        }
        let domains = norn_canonical::canonical_json_exact_of(&bounds.frozen)
            .map_err(|_error| ChallengeError::LossyInput)?;
        let pool: Vec<_> = source
            .base
            .iter()
            .chain(&source.artifacts)
            .cloned()
            .collect();
        let catalog = EffectiveCatalog::from_artifacts_with_registry(&pool, registry);
        let laws = super::law_bindings(&pool)?;
        let object = catalog
            .object_type_named(&bounds.object_type)
            .and_then(Result::ok);
        let snapshot = object
            .ok_or_else(|| "unavailable effective subject".into())
            .and_then(|object| {
                if let Some(reason) = super::unsupported_binding(object, &laws) {
                    return Err(reason);
                }

                require(
                    bounds
                        .frozen
                        .keys()
                        .all(|name| name != &bounds.property && object.property(name).is_some()),
                    "extra frozen scalar domain",
                )?;
                require(
                    !object.keys.iter().any(|key| {
                        key.components
                            .iter()
                            .any(|member| member.as_str() == bounds.property)
                    }),
                    "lifecycle-dependent identity is outside profile",
                )?;
                let applicable = laws
                    .iter()
                    .filter(|(_, law)| {
                        object.lineage.iter().any(|ty| ty.as_str() == law.subject)
                            || object
                                .interfaces
                                .iter()
                                .any(|ty| ty.as_str() == law.subject)
                    })
                    .map(|(artifact, law)| (artifact.artifact_id.clone(), law.clone()))
                    .collect();
                let invariants = integrity_invariants(&pool, object)?;
                let invariant_env = if invariants.is_empty() {
                    None
                } else {
                    Some(build_invariant_env(&pool, registry, object)?)
                };
                freeze(object, bounds, applicable, invariants, invariant_env)
            });
        let document = serde_json::json!({"profile":"norn.lifecycle.frozen/2","objectType":bounds.object_type,"property":bounds.property,"frozenScalarsJson":String::from_utf8(domains).map_err(|_error|ChallengeError::LossyInput)?,"seed":bounds.seed,"maxModeledObjects":1,"maxNodes":4096,"maxSnapshotBytes":4_194_304,"maxFields":16,"maxStates":128,"maxTransitions":128,"maxFrozenLiteralBytes":10_000,"maxSelectorBytes":10_000,"maxPinnedBoundsBytes":65_536,"maxSteps":object.and_then(|object|object.lifecycle(&bounds.property)).map(|machine|machine.states.len()),"interpretation":snapshot.as_ref().map(|snapshot|&snapshot.fields).ok(),"frame":"one admitted subject; scalar context and external references frozen; pre-authorized declared transitions only"});
        let bytes = norn_canonical::canonical_json_exact_of(&document)
            .map_err(|_error| ChallengeError::LossyInput)?;
        if bytes.len() > 65_536 {
            return Err(ChallengeError::Limit);
        }
        let bounds_json = String::from_utf8(bytes).map_err(|_error| ChallengeError::LossyInput)?;
        Ok(Batch {
            challenge,
            bounds_json,
            snapshot,
        })
    }
}
