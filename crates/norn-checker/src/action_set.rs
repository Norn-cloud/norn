//! The `ActionIR` set check (T1-CHK-3, #2526; declared write set #2417): what
//! an action may write, checked over a resolved artifact set.
//!
//! [`check_action_set`] runs inside [`crate::check_artifact_set`] after
//! resolution. It reads every candidate `action` artifact that decodes into
//! its typed root ([`norn_ir::ActionRoot`]; one that does not has per-artifact
//! findings and is skipped) against the **effective model**: every object
//! type defined by an `entity` artifact of the candidates or the installed
//! base, `EntityIR` v2 as decoded and v1 lowered through
//! [`crate::entity_object_type`], with the effective (inherited, extended)
//! properties of v2 types from [`crate::EffectiveCatalog`].
//!
//! # Identities
//!
//! Artifact identities (`(artifactId, version)`, conflicting versions,
//! shadowing the base) are the resolver's ([`crate::artifact_set::codes`]),
//! and `EntityIR` v2 type names are the whole-model check's
//! (`object_type.validation.duplicate_name`). Two identities neither covers
//! are checked here:
//!
//! - an object type name defined twice where one definition is `EntityIR` v1
//!   ([`codes::DUPLICATE_OBJECT_TYPE`]): the v2 model does not see v1
//!   artifacts, and an object kind must name one type;
//! - an `actionId` used by two actions of the set or the base
//!   ([`codes::DUPLICATE_ACTION_ID`]): it is the action's command and MCP tool
//!   name.
//!
//! # Effect targets
//!
//! The closed effect program ([`norn_ir::ActionEffectProgram`]) has two
//! operations, `upsert_input_object` and `fire_transition` (T3-LIFE-3,
//! #2867). Each pins an `entity` artifact by exact id and version and names
//! the object kind it writes ([`norn_ir::ActionEffectProgram::target`]). Its
//! target must:
//!
//! - resolve to an `entity` artifact of the set or the base
//!   ([`codes::UNRESOLVED_EFFECT_ENTITY`]) the action's scope can see
//!   ([`crate::artifact_set::codes::INCOMPATIBLE_SCOPE`]);
//! - be the object type that artifact defines ([`codes::EFFECT_KIND_MISMATCH`]);
//! - be writable by a generic action ([`codes::TARGET_NOT_WRITABLE`]): the
//!   type's mutation authority is `generic_action` (a `domain_engine` type is
//!   mutated only by its domain engine's commands, the rule of
//!   [`norn_ir::entity_mutation_authority_admits`]), and, for an upsert, no
//!   property of the input schema, from which the upsert derives the object,
//!   is a computed (read-only) property of the effective type.
//!
//! # The declared write set
//!
//! Every `writes` entry must name an object type of the effective model
//! ([`codes::UNKNOWN_WRITE_KIND`]) that the action's scope can see and a
//! generic action may write. When the action has an effect program, the
//! write set and the program's targets must be **equal**:
//!
//! - every target is declared ([`codes::UNDECLARED_EFFECT_WRITE`]) — the #2417
//!   subset rule: prepare loads, and the single authority file locks, only
//!   the declared set, so an undeclared write would escape both;
//! - every declared write is a target ([`codes::WRITE_WITHOUT_EFFECT`]) — the
//!   effect program is the action's complete, closed effect whitelist, so a
//!   declared write it cannot perform only widens the load set and the MCP
//!   `destructiveHint`.
//!
//! An upsert may create or modify, so its targets are `(objectKind, created)`
//! and `(objectKind, modified)`; a transition only modifies, so its target is
//! `(objectKind, modified)` ([`norn_ir::ActionEffectProgram::changes`]). An
//! action without an effect program has no statically known targets; its
//! write set is its only declaration and is checked for existence, scope,
//! and writability only.
//!
//! The ruling's "the whole declared set routes to the single scoped authority
//! file" (#2417, ruling 2026-08-27 §6) holds by construction over this model:
//! every target is visible from the action's scope and has generic mutation
//! authority, so a write set cannot span scopes or authority owners.
//!
//! # Lifecycles
//!
//! A lifecycle property changes only through a declared transition (ruling
//! 2026-09-25 §2; [`crate::lifecycle_writes`]):
//!
//! - an input schema property that is a lifecycle property of the effective
//!   target type is [`crate::lifecycle_writes::codes::DIRECT_WRITE`], for
//!   both effects: an upsert would write it, and a transition decides the
//!   state itself, so such an input would only read as a status write;
//! - `fire_transition` names a lifecycle of the **effective** target type
//!   (declared, inherited, or narrowed by a subtype;
//!   [`crate::lifecycle_writes::codes::NOT_A_LIFECYCLE`]) and a transition
//!   id that effective lifecycle declares
//!   ([`crate::lifecycle_writes::codes::UNKNOWN_TRANSITION`]);
//! - a parent-and-rows action's row derivations may not target lifecycle
//!   properties on the effective child type, including inherited properties;
//!   lifecycle state changes remain the responsibility of transition effects;
//! - a target whose lifecycles cannot be resolved fails closed
//!   ([`crate::lifecycle_writes::codes::UNRESOLVED_LIFECYCLES`]).
//!
//! An object an upsert creates takes its property's default, which is an
//! initial state. Whether a transition may fire *now* depends on the
//! object's state: [`check_action_firing`] checks one call against it
//! (`lifecycle.validation.undeclared_write` when the state is not a source
//! of the transition); guards and the commit are the kernel's.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{
    EntityDefinitions, EnvOptions, Expression, FieldSource, Type, ZenTypeEnv, check_expression,
};
use norn_ir::{
    ActionEffectProgram, ActionEffectTarget, ActionRoot, ArtifactEnvelope, ArtifactPayload,
    DiagnosticRemediation, DiagnosticSeverity, EntityMutationAuthority, JsonObject, JsonValue,
    ObjectTypeName, OwnedCollectionAction, PropertyId, PropertyTypeName, SliceKey, TransitionId,
    UpsertWriteMode, WriteChange, ZenExpression,
};

use crate::artifact_set::scope_finding;
use crate::entity_artifacts::{Decoded, EntityArtifactSetContext, Layer, decode_v2};
use crate::entity_instance::{EffectiveCatalog, EffectiveLifecycle, EffectiveObjectType};
use crate::entity_lowering::{EntityForm, entity_object_type};
use crate::lifecycle_writes::{codes as codes_lifecycle, finding as lifecycle_finding};
use crate::lifecycles::{check_transition_firing, codes::UNDECLARED_WRITE};
use crate::property_types::error;
use crate::typed_roots::check_action_slice_preconditions;
use crate::{Diagnostic, DiagnosticCode, Span};

/// Stable `artifact_set.action.*` and `artifact_set.entity.*` codes.
pub mod codes {
    /// A quantity effect or its typed input bindings cannot be admitted.
    pub const QUANTITY_EFFECT: &str = "artifact_set.action.quantity_effect";
    /// A create-only outgoing link lacks its declared authorized target slice.
    pub const CREATE_LINK: &str = "artifact_set.action.create_link";
    /// An object type name is defined by two `entity` artifacts, at least one
    /// of them `EntityIR` v1.
    pub const DUPLICATE_OBJECT_TYPE: &str = "artifact_set.entity.duplicate_object_type";
    /// Two actions of the set or the base share an `actionId`.
    pub const DUPLICATE_ACTION_ID: &str = "artifact_set.action.duplicate_action_id";
    /// The effect program's pinned entity artifact is not in the set or the
    /// base at that version.
    pub const UNRESOLVED_EFFECT_ENTITY: &str = "artifact_set.action.unresolved_effect_entity";
    /// The pinned entity artifact does not define the effect's object kind.
    pub const EFFECT_KIND_MISMATCH: &str = "artifact_set.action.effect_kind_mismatch";
    /// A write set entry names no object type of the effective model.
    pub const UNKNOWN_WRITE_KIND: &str = "artifact_set.action.unknown_write_kind";
    /// An action slice names no object type of the effective model.
    pub const UNKNOWN_SLICE_KIND: &str = "artifact_set.action.unknown_slice_kind";
    /// An action slice reads an unknown key property or link.
    pub const INVALID_SLICE: &str = "artifact_set.action.invalid_slice";
    /// An effect target or declared write is not writable by a generic
    /// action: a domain-engine type, or a computed property.
    pub const TARGET_NOT_WRITABLE: &str = "artifact_set.action.target_not_writable";
    /// An effect target is not in the declared write set.
    pub const UNDECLARED_EFFECT_WRITE: &str = "artifact_set.action.undeclared_effect_write";
    /// A declared write is not a target of the action's effect program.
    pub const WRITE_WITHOUT_EFFECT: &str = "artifact_set.action.write_without_effect";
    /// An upsert's draft-only state is not an initial, writable state of its
    /// declared lifecycle.
    pub const INVALID_WRITE_MODE: &str = "artifact_set.action.invalid_write_mode";
    /// A generated key is not connected to a create-only allocator input, or
    /// the action can bypass allocation for an existing series key.
    pub const NAMING_SERIES_TARGET_MISMATCH: &str =
        "artifact_set.action.naming_series_target_mismatch";
}

/// Check every candidate action's effect targets and declared write set over
/// the resolved set of `artifacts` and the installed `base` (T3). See the
/// module docs.
#[must_use]
pub fn check_action_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let base = context.base;
    let actions: Vec<(&ArtifactEnvelope, ActionRoot)> = artifacts
        .iter()
        .filter_map(|artifact| decode_action(artifact).map(|root| (artifact, root)))
        .collect();
    let model = Model::new(artifacts, base, context.registry, !actions.is_empty());
    let mut findings = model.duplicate_findings();
    if actions.is_empty() {
        return findings;
    }

    let mut action_ids: BTreeSet<String> = base
        .iter()
        .filter_map(decode_action)
        .map(|root| root.action_id)
        .collect();
    for (artifact, root) in &actions {
        if !action_ids.insert(root.action_id.clone()) {
            findings.push(finding(
                codes::DUPLICATE_ACTION_ID,
                format!(
                    "actionId {:?} is already used by another action of the artifact set or the installed base; an action id names one command",
                    root.action_id
                ),
                artifact,
                "/payload/actionId",
            ));
        }
        findings.extend(check_action(artifact, root, &model, artifacts, base));
    }
    findings
}

/// Check one call of `action` on an existing object (T3-LIFE-3, #2867): the
/// object's effective type is `object_type` (its own type, so a subtype's
/// narrowed lifecycle applies) and its stored property values are `values`.
///
/// Only a `fire_transition` effect is checked; every other action has no
/// findings here. The object must be of the effect's object kind or a
/// subtype ([`codes::EFFECT_KIND_MISMATCH`]), the effect's lifecycle must be
/// a lifecycle of its type
/// ([`crate::lifecycle_writes::codes::NOT_A_LIFECYCLE`]), and the object's
/// current state must be a source state of the transition
/// ([`crate::check_transition_firing`]; `lifecycle.validation.undeclared_write`
/// otherwise, also when the object holds no state). This is the static
/// semantics of the refusal (an agent's `submit_invoice` on an order that is
/// not approved yet); guards, the commit, and the ledger are the kernel's.
/// Spans point into the action artifact.
#[must_use]
pub fn check_action_firing(
    action: &ActionRoot,
    object_type: &EffectiveObjectType,
    values: &BTreeMap<String, JsonValue>,
) -> Vec<Diagnostic> {
    let Some(ActionEffectProgram::FireTransition {
        object_kind,
        lifecycle,
        transition,
        ..
    }) = &action.effect_program
    else {
        return Vec::new();
    };
    let at = |member: &str| Span::pointer(format!("/payload/effectProgram/{member}"));
    let name = object_type.name.as_str();
    if !object_type
        .lineage
        .iter()
        .any(|ancestor| ancestor.as_str() == object_kind)
    {
        return vec![error(
            codes::EFFECT_KIND_MISMATCH,
            format!(
                "the transition writes {object_kind:?}, but the object is a {name:?}, which is not that type or a subtype"
            ),
            at("objectKind"),
        )];
    }
    let property = lifecycle.as_str();
    let Some(effective) = object_type.lifecycle(property) else {
        return vec![error(
            codes_lifecycle::NOT_A_LIFECYCLE,
            format!("{property:?} of {name:?} has no declared lifecycle to transition"),
            at("lifecycle"),
        )];
    };
    let Some(current) = values.get(property).and_then(JsonValue::as_string) else {
        return vec![error(
            UNDECLARED_WRITE,
            format!(
                "the object holds no {property} state, so firing {:?} cannot follow a declared transition",
                transition.as_str()
            ),
            at("transition"),
        )];
    };
    check_transition_firing(
        &effective.lifecycle,
        transition.as_str(),
        current,
        at("transition"),
    )
}

/// The findings of one action.
fn check_action(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let mut findings = action_slice_findings(artifact, root, model);
    findings.extend(quantity_effect_findings(artifact, root, model));
    findings.extend(create_link_findings(artifact, root, model));
    let mut targets: BTreeSet<(&str, WriteChange)> = BTreeSet::new();
    if let Some(program) = &root.effect_program {
        let kind = program.target().object_kind;
        targets.extend(program.changes().iter().map(|change| (kind, *change)));
        if matches!(
            program,
            ActionEffectProgram::UpsertInputObject { .. }
                | ActionEffectProgram::FireTransition { .. }
        ) && model.is_owned_child_target(kind)
        {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!(
                    "ordinary action effects cannot target owned child type {kind:?}; mutate owned rows through the parent's parent-and-rows effect"
                ),
                artifact,
                "/payload/effectProgram/objectKind",
            ));
        }
        if matches!(program, ActionEffectProgram::MutateParentWithRows { .. })
            && let Some(parent) = model.effective_object_type_named(kind)
        {
            for collection in &parent.owned_collections {
                targets.extend(
                    [
                        WriteChange::Created,
                        WriteChange::Modified,
                        WriteChange::Deleted,
                    ]
                    .iter()
                    .map(|change| (collection.child_type.as_str(), *change)),
                );
            }
        }
        findings.extend(effect_target_findings(
            artifact, root, program, model, artifacts, base,
        ));
    }

    for (index, write) in root.writes.iter().enumerate() {
        let kind = write.entity_kind.as_str();
        let pointer = format!("/payload/writes/{index}/entityKind");
        match model.object_type(kind) {
            None => findings.push(finding(
                codes::UNKNOWN_WRITE_KIND,
                format!(
                    "the write set names {kind:?}, which is not an object type of the artifact set or the installed base"
                ),
                artifact,
                &pointer,
            )),
            Some(defined) => {
                findings.extend(scope_finding(artifact, defined.artifact, &pointer));
                findings.extend(defined.authority_finding(artifact, kind, &pointer));
            }
        }
    }

    if root.effect_program.is_some() {
        let declared: BTreeSet<(&str, WriteChange)> = root
            .writes
            .iter()
            .map(|write| (write.entity_kind.as_str(), write.change))
            .collect();
        for (kind, change) in targets.difference(&declared) {
            findings.push(finding(
                codes::UNDECLARED_EFFECT_WRITE,
                format!(
                    "the effect program may write {kind:?} ({}), but the write set does not declare it",
                    change_name(*change)
                ),
                artifact,
                "/payload/effectProgram",
            ));
        }
        for (index, write) in root.writes.iter().enumerate() {
            if !targets.contains(&(write.entity_kind.as_str(), write.change)) {
                findings.push(finding(
                    codes::WRITE_WITHOUT_EFFECT,
                    format!(
                        "the write set declares {:?} ({}), which the effect program never writes",
                        write.entity_kind,
                        change_name(write.change)
                    ),
                    artifact,
                    &format!("/payload/writes/{index}"),
                ));
            }
        }
    }
    findings
}

fn action_slice_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    if !root.slice.is_empty() {
        let slice = action_slice_types(artifact, root, model);
        findings.extend(slice.findings);
        if slice.typed_slice {
            findings.extend(
                check_action_slice_preconditions(
                    root,
                    slice.records,
                    slice.fields,
                    Some(&artifact.artifact_id),
                )
                .into_iter()
                .map(Diagnostic::from),
            );
        }
    }
    findings
}

struct ActionSliceTypes {
    findings: Vec<Diagnostic>,
    records: BTreeMap<String, BTreeMap<String, Type>>,
    fields: BTreeMap<String, Type>,
    typed_slice: bool,
}

fn action_slice_types(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
) -> ActionSliceTypes {
    let mut findings = Vec::new();
    let mut records = BTreeMap::new();
    let mut slice_fields = BTreeMap::new();
    let mut typed_slice = true;
    let environment = match &model.expression_environment {
        Ok(environment) => environment,
        Err(errors) => {
            findings.push(finding(
                codes::INVALID_SLICE,
                format!("slice property types cannot be resolved: {errors:?}"),
                artifact,
                "/payload/slice",
            ));
            return ActionSliceTypes {
                findings,
                records,
                fields: slice_fields,
                typed_slice: false,
            };
        }
    };

    for (index, entry) in root.slice.iter().enumerate() {
        let path = format!("/payload/slice/{index}");
        let kind = entry.object_type.as_str();
        let Some(defined) = model.object_type(kind) else {
            findings.push(finding(
                codes::UNKNOWN_SLICE_KIND,
                format!(
                    "slice entry {:?} names {kind:?}, which is not an object type of the artifact set or the installed base",
                    entry.name
                ),
                artifact,
                &format!("{path}/objectType"),
            ));
            typed_slice = false;
            continue;
        };
        findings.extend(scope_finding(
            artifact,
            defined.artifact,
            &format!("{path}/objectType"),
        ));
        let Some(Ok(_)) = &defined.effective_object_type else {
            findings.push(finding(
                codes::INVALID_SLICE,
                format!("slice object type {kind:?} has no admitted effective definition"),
                artifact,
                &format!("{path}/objectType"),
            ));
            typed_slice = false;
            continue;
        };
        let Some(record) = environment.record(kind) else {
            findings.push(finding(
                codes::INVALID_SLICE,
                format!("slice object type {kind:?} has no resolved property record"),
                artifact,
                &format!("{path}/objectType"),
            ));
            typed_slice = false;
            continue;
        };
        // The host loads properties only. Related records must be separate,
        // bounded slice declarations; model links are never implicit reads.
        let fields = record
            .fields
            .iter()
            .filter(|(_, field)| matches!(field.source, FieldSource::Property { .. }))
            .map(|(name, field)| (name.clone(), field.ty.clone()))
            .collect();
        records.entry(kind.to_owned()).or_insert(fields);

        findings.extend(action_slice_key_findings(
            artifact,
            root,
            index,
            model,
            environment,
        ));

        let object = Type::Record(kind.to_owned());
        let object = if entry.is_list() {
            object.list()
        } else {
            object
        };
        let object = if entry.optional {
            object.optional()
        } else {
            object
        };
        slice_fields.insert(entry.name.clone(), object);
    }

    ActionSliceTypes {
        findings,
        records,
        fields: slice_fields,
        typed_slice,
    }
}

fn action_slice_key_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    index: usize,
    model: &Model<'_>,
    environment: &ZenTypeEnv,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let Some(entry) = root.slice.get(index) else {
        return findings;
    };
    let path = format!("/payload/slice/{index}");
    let kind = entry.object_type.as_str();
    if let SliceKey::Property { from, property } = &entry.key
        && from != "input"
    {
        let Some(source) = root
            .slice
            .iter()
            .take(index)
            .find(|prior| prior.name == *from)
        else {
            return findings;
        };
        match environment
            .record(source.object_type.as_str())
            .and_then(|record| record.fields.get(property.as_str()))
            .filter(|field| matches!(field.source, FieldSource::Property { .. }))
        {
            None => findings.push(finding(
                codes::INVALID_SLICE,
                format!(
                    "slice key property {:?} is not declared on source type {:?}",
                    property.as_str(),
                    source.object_type.as_str()
                ),
                artifact,
                &format!("{path}/key/property"),
            )),
            Some(field) if !is_action_string_key_type(&field.ty) => findings.push(finding(
                codes::INVALID_SLICE,
                format!(
                    "slice key property {:?} must be a string or enum",
                    property.as_str()
                ),
                artifact,
                &format!("{path}/key/property"),
            )),
            Some(field)
                if (source.optional || matches!(field.ty, Type::Optional(_)))
                    && !entry.optional =>
            {
                findings.push(finding(
                    codes::INVALID_SLICE,
                    format!(
                        "slice key property {:?} of entry {from:?} may be absent or null; mark the slice entry optional",
                        property.as_str(),
                    ),
                    artifact,
                    &format!("{path}/key/property"),
                ));
            }
            Some(_) => {}
        }
    }

    if let SliceKey::Link { from, link } = &entry.key
        && let Some(source) = root
            .slice
            .iter()
            .take(index)
            .find(|prior| prior.name == *from)
    {
        match model.link_target(source.object_type.as_str(), link) {
            None => findings.push(finding(
                codes::INVALID_SLICE,
                format!(
                    "slice entry {:?} reads undeclared link {link:?} of {:?}",
                    entry.name,
                    source.object_type.as_str()
                ),
                artifact,
                &format!("{path}/key"),
            )),
            Some(target) if target != kind => findings.push(finding(
                codes::INVALID_SLICE,
                format!(
                    "slice entry {:?} names {kind:?}, but link {link:?} targets {target:?}",
                    entry.name
                ),
                artifact,
                &format!("{path}/objectType"),
            )),
            Some(_) => {}
        }
    }
    findings
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn is_action_string_key_type(ty: &Type) -> bool {
    match ty {
        Type::Text { .. } | Type::Enum { .. } => true,
        Type::Optional(inner) => is_action_string_key_type(inner),
        _ => false,
    }
}

/// The findings of an action's effect program target: it resolves, is the
/// pinned artifact's object type, and is writable.
fn effect_target_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    program: &ActionEffectProgram,
    model: &Model<'_>,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let ActionEffectTarget {
        object_kind,
        entity_artifact_id,
        entity_artifact_version,
        ..
    } = program.target();
    let mut findings = Vec::new();
    let pinned = artifacts.iter().chain(base).find(|candidate| {
        candidate.artifact_id == entity_artifact_id && candidate.version == entity_artifact_version
    });
    match pinned {
        Some(pinned) if matches!(pinned.payload, ArtifactPayload::Entity(_)) => {
            findings.extend(scope_finding(
                artifact,
                pinned,
                "/payload/effectProgram/entityArtifactId",
            ));
            match model.defined_by(pinned) {
                Some(defined) if defined.name == object_kind => {
                    findings.extend(defined.writability(artifact, root, program, model));
                }
                defined => findings.push(finding(
                    codes::EFFECT_KIND_MISMATCH,
                    format!(
                        "the effect writes {object_kind:?}, but entity artifact {entity_artifact_id:?} version {entity_artifact_version:?} {}",
                        defined.map_or_else(
                            || "defines no object type".to_owned(),
                            |defined| format!("defines object type {:?}", defined.name)
                        )
                    ),
                    artifact,
                    "/payload/effectProgram/objectKind",
                )),
            }
        }
        other => findings.push(finding(
            codes::UNRESOLVED_EFFECT_ENTITY,
            format!(
                "the effect pins entity artifact {entity_artifact_id:?} version {entity_artifact_version:?}, which {}",
                if other.is_some() {
                    "is not an entity artifact"
                } else {
                    "is neither in the artifact set nor installed"
                }
            ),
            artifact,
            "/payload/effectProgram/entityArtifactVersion",
        )),
    }
    findings
}

/// One object type definition of the effective model.
pub(crate) struct Defined<'a> {
    name: String,
    pub(crate) artifact: &'a ArtifactEnvelope,
    form: EntityForm,
    authority: Option<EntityMutationAuthority>,
    /// Effective computed (read-only) property ids.
    computed: BTreeSet<String>,
    /// Whether the artifact is a candidate (not the base).
    candidate: bool,
    /// Effective lifecycles (T3-LIFE-2): empty for a v1 type (it declares
    /// none); `Err(reason)` for a v2 type the effective catalog cannot build,
    /// so a write of it fails closed
    /// ([`crate::lifecycle_writes::codes::UNRESOLVED_LIFECYCLES`]).
    pub(crate) lifecycles: Result<Vec<EffectiveLifecycle>, String>,
    /// Effective stored-property contracts; absent only when the model was
    /// built without effective views.
    pub(crate) effective_object_type: Option<Result<EffectiveObjectType, String>>,
    /// Effective forward links, including inherited and extension links:
    /// `(link id, target)`. Unavailable models have no admitted links.
    links: Vec<(String, String)>,
}

impl Defined<'_> {
    fn authority_finding(
        &self,
        action: &ArtifactEnvelope,
        kind: &str,
        pointer: &str,
    ) -> Option<Diagnostic> {
        let Some(EntityMutationAuthority::DomainEngine { domain }) = self.authority else {
            return None;
        };
        Some(finding(
            codes::TARGET_NOT_WRITABLE,
            format!(
                "{kind:?} is mutated only by the {domain:?} domain engine (mutationAuthority), not by a generic action"
            ),
            action,
            pointer,
        ))
    }

    /// The findings of writing `program`'s target, a type this definition
    /// defines: generic mutation authority, resolvable lifecycles, no
    /// lifecycle property in the input, and per effect, no computed input
    /// property (an upsert) or a declared transition of the effective
    /// lifecycle (a transition).
    #[allow(
        clippy::too_many_lines,
        reason = "effect writability evaluates one cohesive lifecycle and read-only contract"
    )]
    fn writability(
        &self,
        action: &ArtifactEnvelope,
        root: &ActionRoot,
        program: &ActionEffectProgram,
        model: &Model<'_>,
    ) -> Vec<Diagnostic> {
        let kind = program.target().object_kind;
        let mut findings: Vec<Diagnostic> = self
            .authority_finding(action, kind, "/payload/effectProgram/objectKind")
            .into_iter()
            .collect();
        let effect = match program {
            ActionEffectProgram::UpsertInputObject { .. } => "the upsert",
            ActionEffectProgram::FireTransition { .. } => "the transition",
            ActionEffectProgram::MutateParentWithRows { .. } => "the parent-and-rows mutation",
        };
        let lifecycles: Option<&[EffectiveLifecycle]> = match &self.lifecycles {
            Ok(lifecycles) => Some(lifecycles.as_slice()),
            Err(reason) => {
                findings.push(lifecycle_finding(
                    codes_lifecycle::UNRESOLVED_LIFECYCLES,
                    format!(
                        "{effect} writes {kind:?}, whose lifecycles cannot be resolved ({reason}); a status write cannot be checked, so it fails closed"
                    ),
                    action,
                    "/payload/effectProgram/objectKind",
                ));
                None
            }
        };
        let owned = |id: &str| {
            lifecycles
                .unwrap_or_default()
                .iter()
                .any(|lifecycle| lifecycle.lifecycle.property.as_str() == id)
        };
        if !matches!(program, ActionEffectProgram::MutateParentWithRows { .. })
            && let Some(JsonValue::Object(properties)) = root.input_schema.get("properties")
        {
            let remedy = match program {
                ActionEffectProgram::UpsertInputObject { .. } => {
                    "fire a declared transition instead (effect kind fire_transition), and omit it (a created object takes its default initial state)"
                }
                ActionEffectProgram::FireTransition { .. } => {
                    "the transition alone decides the state written, so omit it"
                }
                ActionEffectProgram::MutateParentWithRows { .. } => {
                    "the parent-and-rows effect controls its declared values, so omit lifecycle properties"
                }
            };
            findings.extend(properties.keys().filter(|id| owned(id.as_str())).map(|property| {
                lifecycle_finding(
                    codes_lifecycle::DIRECT_WRITE,
                    format!(
                        "{effect} takes input property {property:?}, which the lifecycle of {kind:?} owns; {remedy}"
                    ),
                    action,
                    &format!("/payload/inputSchema/properties/{property}"),
                )
            }));
            if matches!(program, ActionEffectProgram::UpsertInputObject { .. }) {
                for property in properties.keys().filter(|id| self.computed.contains(*id)) {
                    let mut diagnostic = finding(
                        codes::TARGET_NOT_WRITABLE,
                        format!(
                            "the upsert writes input property {property:?}, which is a computed (read-only) property of {kind:?}"
                        ),
                        action,
                        "",
                    );
                    diagnostic.span = Some(Span::from_segments([
                        "payload",
                        "inputSchema",
                        "properties",
                        property,
                    ]));
                    findings.push(diagnostic);
                }
            }
        }
        if let ActionEffectProgram::MutateParentWithRows {
            parent_values_field,
            row_ops_field,
            parent_version_field,
            parent_delete_field,
            collections,
            ..
        } = program
        {
            findings.extend(owned_rows_effect_findings(
                action,
                root,
                kind,
                self.effective_object_type
                    .as_ref()
                    .and_then(|view| view.as_ref().ok()),
                model,
                parent_values_field,
                row_ops_field,
                parent_version_field,
                parent_delete_field.as_deref(),
                collections,
            ));
        }
        findings.extend(self.naming_series_findings(action, root, program));
        if let (
            ActionEffectProgram::FireTransition {
                lifecycle,
                transition,
                ..
            },
            Some(lifecycles),
        ) = (program, lifecycles)
        {
            findings.extend(transition_findings(
                action, kind, lifecycles, lifecycle, transition,
            ));
        }
        if let (
            ActionEffectProgram::UpsertInputObject {
                write_mode: UpsertWriteMode::DraftOnly { lifecycle, state },
                ..
            },
            Some(lifecycles),
        ) = (program, lifecycles)
        {
            findings.extend(draft_write_mode_findings(
                action, kind, lifecycles, lifecycle, state,
            ));
        }
        findings
    }

    #[allow(
        clippy::too_many_lines,
        reason = "generated key, write mode, and required scope are checked as one action contract"
    )]
    fn naming_series_findings(
        &self,
        action: &ArtifactEnvelope,
        root: &ActionRoot,
        program: &ActionEffectProgram,
    ) -> Vec<Diagnostic> {
        let ActionEffectProgram::UpsertInputObject {
            object_id_field,
            write_mode,
            ..
        } = program
        else {
            return Vec::new();
        };
        let Some(Ok(effective)) = self.effective_object_type.as_ref() else {
            return Vec::new();
        };
        let declared_series_key = effective
            .keys
            .iter()
            .find(|key| key.naming_series.is_some());
        let series_key = declared_series_key.filter(|key| {
            key.components.len() == 1
                && key
                    .components
                    .first()
                    .is_some_and(|id| id.as_str() == object_id_field)
        });
        let field_required = input_schema_field_required(&root.input_schema, object_id_field);

        if declared_series_key.is_some() && series_key.is_none() {
            let message = match write_mode {
                UpsertWriteMode::Upsert => format!(
                    "upsert objectIdField {object_id_field:?} must target the generated naming-series key"
                ),
                UpsertWriteMode::CreateOnly => format!(
                    "create_only objectIdField {object_id_field:?} must target the generated naming-series key"
                ),
                UpsertWriteMode::DraftOnly { .. } => format!(
                    "draft_only objectIdField {object_id_field:?} must target the generated naming-series key"
                ),
            };
            return vec![finding(
                codes::NAMING_SERIES_TARGET_MISMATCH,
                message,
                action,
                "/payload/effectProgram/objectIdField",
            )];
        }

        if series_key.is_some()
            && !matches!(
                write_mode,
                UpsertWriteMode::CreateOnly | UpsertWriteMode::DraftOnly { .. }
            )
        {
            return vec![finding(
                codes::NAMING_SERIES_TARGET_MISMATCH,
                format!(
                    "series key {object_id_field:?} may be created only through create_only allocation; use draft_only for an existing object"
                ),
                action,
                "/payload/effectProgram/writeMode",
            )];
        }

        if !field_required {
            let Some(key) =
                series_key.filter(|_| matches!(write_mode, UpsertWriteMode::CreateOnly))
            else {
                return vec![finding(
                    codes::NAMING_SERIES_TARGET_MISMATCH,
                    format!(
                        "optional objectIdField {object_id_field:?} requires create_only and a matching naming-series key"
                    ),
                    action,
                    "/payload/effectProgram/objectIdField",
                )];
            };
            let Some(series) = key.naming_series.as_ref() else {
                return vec![finding(
                    codes::NAMING_SERIES_TARGET_MISMATCH,
                    "the selected key has no naming-series declaration".to_owned(),
                    action,
                    "/payload/effectProgram/objectIdField",
                )];
            };
            let mut findings = Vec::new();
            for (index, derivation) in root.derive.iter().enumerate() {
                if derivation.field == *object_id_field {
                    findings.push(finding(
                        codes::NAMING_SERIES_TARGET_MISMATCH,
                        format!("generated key {object_id_field:?} is allocated by the kernel and cannot be derived"),
                        action,
                        &format!("/payload/derive/{index}/field"),
                    ));
                }
            }
            for scope_field in &series.scope_fields {
                if !input_schema_field_required(&root.input_schema, scope_field.as_str())
                    || !input_schema_field_is_string(&root.input_schema, scope_field.as_str())
                {
                    findings.push(finding(
                        codes::NAMING_SERIES_TARGET_MISMATCH,
                        format!(
                            "generated key scope field {:?} must be a required string in the action input",
                            scope_field.as_str()
                        ),
                        action,
                        &format!(
                            "/payload/inputSchema/properties/{}",
                            scope_field.as_str()
                        ),
                    ));
                }
            }
            return findings;
        }

        if series_key.is_some() && matches!(write_mode, UpsertWriteMode::CreateOnly) {
            return vec![finding(
                codes::NAMING_SERIES_TARGET_MISMATCH,
                format!(
                    "create_only series key {object_id_field:?} must be optional so the kernel allocates it"
                ),
                action,
                "/payload/effectProgram/objectIdField",
            )];
        }
        Vec::new()
    }
}

fn input_schema_field_required(schema: &JsonObject, field: &str) -> bool {
    schema
        .get("required")
        .and_then(JsonValue::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|required| required.as_string() == Some(field))
        })
}

fn input_schema_field_is_string(schema: &JsonObject, field: &str) -> bool {
    schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .and_then(|properties| properties.get(field))
        .and_then(JsonValue::as_object)
        .and_then(|property| property.get("type"))
        .and_then(JsonValue::as_string)
        == Some("string")
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "owned row effect admission checks the complete trusted contract at once"
)]
fn owned_rows_effect_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    kind: &str,
    effective: Option<&EffectiveObjectType>,
    model: &Model<'_>,
    parent_values_field: &str,
    row_ops_field: &str,
    parent_version_field: &str,
    parent_delete_field: Option<&str>,
    configs: &[OwnedCollectionAction],
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let input_properties = root
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object);
    let required = root
        .input_schema
        .get("required")
        .and_then(JsonValue::as_array)
        .unwrap_or_default();
    if let Some(program) = &root.effect_program {
        let mut names = BTreeSet::new();
        for field in [
            Some(program.target().object_id_field),
            Some(parent_values_field),
            Some(row_ops_field),
            Some(parent_version_field),
            parent_delete_field,
        ]
        .into_iter()
        .flatten()
        {
            if !names.insert(field) {
                findings.push(finding(
                    codes::TARGET_NOT_WRITABLE,
                    format!("parent-and-rows input fields must be distinct; {field:?} is reused"),
                    artifact,
                    "/payload/effectProgram",
                ));
            }
        }
    }
    let typed_fields = [
        (parent_values_field, "object", true),
        (row_ops_field, "array", true),
        (parent_version_field, "string", false),
    ];
    let typed_fields = typed_fields
        .into_iter()
        .chain(parent_delete_field.map(|field| (field, "boolean", false)));
    for (field, expected_type, allow_union) in typed_fields {
        let schema = input_properties.and_then(|properties| properties.get(field));
        let type_ok = schema.is_some_and(|schema| {
            if allow_union {
                schema_includes_type(schema, expected_type)
            } else {
                schema
                    .as_object()
                    .and_then(|schema| schema.get("type"))
                    .and_then(JsonValue::as_string)
                    == Some(expected_type)
            }
        });
        let is_required = required
            .iter()
            .any(|value| value.as_string() == Some(field));
        if !type_ok || !is_required {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!("the parent-and-rows effect field {field:?} must be required and accept {expected_type:?} in the action input schema"),
                artifact,
                "/payload/effectProgram",
            ));
        }
    }
    if let Some(items) = input_properties
        .and_then(|properties| properties.get(row_ops_field))
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("items"))
        && !(items.as_object().is_some_and(|schema| schema.len() == 1)
            && schema_includes_type(items, "object")
            && items
                .as_object()
                .and_then(|schema| schema.get("type"))
                .is_some_and(|kind| matches!(kind, JsonValue::Array(_))))
    {
        findings.push(finding(
            codes::TARGET_NOT_WRITABLE,
            "row-operation items must use the unrestricted bounded JSON type union; the kernel validates canonical operations".to_owned(),
            artifact,
            "/payload/effectProgram/rowOpsField",
        ));
    }
    let Some(effective) = effective else {
        findings.push(finding(
            codes::TARGET_NOT_WRITABLE,
            format!("owned collections of {kind:?} cannot be resolved; the parent-and-rows effect fails closed"),
            artifact,
            "/payload/effectProgram/collections",
        ));
        return findings;
    };
    if !effective
        .properties
        .iter()
        .any(|property| !property.computed && effective.lifecycle(property.id.as_str()).is_none())
    {
        findings.push(finding(
            codes::TARGET_NOT_WRITABLE,
            "parent-and-rows effects require a stored non-lifecycle parent property for row-only CAS touches".to_owned(),
            artifact,
            "/payload/effectProgram",
        ));
    }
    let declared = effective
        .owned_collections
        .iter()
        .map(|collection| (collection.id.as_str(), collection))
        .collect::<BTreeMap<_, _>>();
    if declared.is_empty() {
        findings.push(finding(
            codes::TARGET_NOT_WRITABLE,
            format!(
                "the parent-and-rows effect target {kind:?} must declare at least one owned child collection"
            ),
            artifact,
            "/payload/effectProgram/collections",
        ));
    }
    let mut seen = BTreeSet::new();
    for config in configs {
        let id = config.id.as_str();
        if !seen.insert(id) {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!("owned collection {id:?} is configured more than once"),
                artifact,
                "/payload/effectProgram/collections",
            ));
            continue;
        }
        if !declared.contains_key(id) {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!("{kind:?} does not declare owned collection {id:?}"),
                artifact,
                "/payload/effectProgram/collections",
            ));
        } else if let Some(collection) = declared.get(id)
            && let Some(child) = model.object_type(collection.child_type.as_str())
        {
            let pinned = artifact.dependencies.iter().any(|dependency| {
                dependency.kind == child.artifact.kind()
                    && dependency.artifact_id == child.artifact.artifact_id
                    && dependency.version.as_deref() == Some(child.artifact.version.as_str())
                    && dependency.required
            });
            if !pinned {
                findings.push(finding(
                    codes::UNRESOLVED_EFFECT_ENTITY,
                    format!("owned collection {id:?} child type {:?} must be pinned by an exact required action dependency", collection.child_type.as_str()),
                    artifact,
                    "/payload/effectProgram/collections",
                ));
            }
        }
        for property in config.derive.keys() {
            let reserved = declared.get(id).is_some_and(|collection| {
                property == &collection.row_id_property || property == &collection.ordering_key
            });
            let child_effective = declared.get(id).and_then(|collection| {
                model.effective_object_type_named(collection.child_type.as_str())
            });
            let computed = child_effective
                .and_then(|child| child.property(property.as_str()))
                .is_none_or(|property| property.computed);
            let lifecycle =
                child_effective.is_some_and(|child| child.lifecycle(property.as_str()).is_some());
            if lifecycle {
                findings.push(lifecycle_finding(
                    codes_lifecycle::DIRECT_WRITE,
                    format!(
                        "row derivation for {id:?} targets lifecycle property {:?}; lifecycle states may change only through declared transition effects",
                        property.as_str()
                    ),
                    artifact,
                    "/payload/effectProgram/collections",
                ));
            }
            let generated = child_effective.is_some_and(|child| {
                child
                    .keys
                    .iter()
                    .any(|key| key.naming_series.is_some() && key.components.contains(property))
            });
            if reserved || computed || generated {
                findings.push(finding(
                    codes::TARGET_NOT_WRITABLE,
                    format!(
                        "row derivation for {id:?} may not write reserved, generated naming-series, computed, or unknown property {:?}",
                        property.as_str()
                    ),
                    artifact,
                    "/payload/effectProgram/collections",
                ));
            }
        }
        if let Some(collection) = declared.get(id) {
            findings.extend(owned_row_expression_findings(
                artifact, root, model, collection, config,
            ));
        }
    }
    for id in declared.keys().filter(|id| !seen.contains(**id)) {
        findings.push(finding(
            codes::TARGET_NOT_WRITABLE,
            format!("the effect must declare trusted behavior for owned collection {id:?}"),
            artifact,
            "/payload/effectProgram/collections",
        ));
    }
    let parent_values_properties = input_properties
        .and_then(|properties| properties.get(parent_values_field))
        .and_then(JsonValue::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(JsonValue::as_object);
    for name in parent_values_properties
        .into_iter()
        .flat_map(|fields| fields.keys())
    {
        if name.starts_with("_norn") || effective.property(name).is_none() {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!("parent values may not write unknown or reserved property {name:?}"),
                artifact,
                "/payload/effectProgram/parentValuesField",
            ));
        }
    }
    for property in &effective.properties {
        if !parent_values_properties.is_some_and(|fields| fields.contains_key(property.id.as_str()))
        {
            continue;
        }
        if property.computed {
            findings.push(finding(
                codes::TARGET_NOT_WRITABLE,
                format!(
                    "parent values may not write computed property {:?}",
                    property.id.as_str()
                ),
                artifact,
                "/payload/effectProgram/parentValuesField",
            ));
        }
        if effective.lifecycle(property.id.as_str()).is_some() {
            findings.push(lifecycle_finding(
                codes_lifecycle::DIRECT_WRITE,
                format!(
                    "parent values may not write lifecycle property {:?}; lifecycle states may change only through declared transition effects",
                    property.id.as_str()
                ),
                artifact,
                "/payload/effectProgram/parentValuesField",
            ));
        }
    }
    findings
}

fn schema_includes_type(schema: &JsonValue, expected: &str) -> bool {
    match schema.as_object().and_then(|schema| schema.get("type")) {
        Some(JsonValue::String(actual)) => actual == expected,
        Some(JsonValue::Array(types)) => {
            if !matches!(expected, "object" | "array") || types.len() != 6 {
                return false;
            }
            let names = types
                .iter()
                .map(JsonValue::as_string)
                .collect::<Option<BTreeSet<_>>>();
            names.is_some_and(|names| {
                names.len() == 6
                    && names
                        == ["null", "boolean", "number", "string", "array", "object"]
                            .into_iter()
                            .collect()
                    && names.contains(expected)
            })
        }
        _ => false,
    }
}

fn owned_row_expression_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
    collection: &norn_ir::OwnedChildCollection,
    config: &OwnedCollectionAction,
) -> Vec<Diagnostic> {
    if config.preconditions.is_empty() && config.derive.is_empty() {
        return Vec::new();
    }
    let Ok(base_env) = model.expression_environment.clone() else {
        return vec![owned_row_expression_invalid(
            artifact,
            "owned-row expressions cannot be checked because the entity ZEN environment is unresolved"
                .to_owned(),
        )];
    };
    let Some(input_fields) = action_expression_fields(root) else {
        return vec![owned_row_expression_invalid(
            artifact,
            "owned-row expressions cannot be checked because the action input schema is unresolved"
                .to_owned(),
        )];
    };
    let excluded_derive_targets = config
        .derive
        .keys()
        .map(|property| property.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let bind_row_scope = |excluded_fields: &BTreeSet<String>| {
        base_env
            .clone()
            .bind_self_to_stored_fields(&collection.child_type, excluded_fields)
            .and_then(|env| env.bind_input(input_fields.clone()))
    };
    let row_env = match bind_row_scope(&BTreeSet::new()) {
        Ok(env) => env,
        Err(error) => {
            return vec![owned_row_expression_invalid(
                artifact,
                format!("owned-row expression scope is invalid: {error}"),
            )];
        }
    };
    let derive_env = match bind_row_scope(&excluded_derive_targets) {
        Ok(env) => env,
        Err(error) => {
            return vec![owned_row_expression_invalid(
                artifact,
                format!("owned-row derivation scope is invalid: {error}"),
            )];
        }
    };
    let mut findings = owned_row_precondition_findings(artifact, &config.preconditions, &row_env);
    findings.extend(owned_row_derivation_findings(
        artifact,
        &config.derive,
        &collection.child_type,
        &base_env,
        &derive_env,
    ));
    findings
}

fn owned_row_expression_invalid(artifact: &ArtifactEnvelope, message: String) -> Diagnostic {
    finding(
        codes::TARGET_NOT_WRITABLE,
        message,
        artifact,
        "/payload/effectProgram/collections",
    )
}

fn owned_row_precondition_findings(
    artifact: &ArtifactEnvelope,
    preconditions: &[ZenExpression],
    row_env: &ZenTypeEnv,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for precondition in preconditions {
        match Expression::compile(precondition.as_str()) {
            Ok(expression) => {
                if let Err(errors) = check_expression(&expression, row_env, &Type::Bool) {
                    findings.extend(errors.into_iter().map(|error| {
                        owned_row_expression_invalid(
                            artifact,
                            format!(
                                "row precondition {:?} is invalid: {} ({})",
                                precondition.as_str(),
                                error.message,
                                error.code
                            ),
                        )
                    }));
                }
            }
            Err(error) => findings.push(owned_row_expression_invalid(
                artifact,
                format!(
                    "row precondition {:?} does not compile: {error}",
                    precondition.as_str()
                ),
            )),
        }
    }
    findings
}

fn owned_row_derivation_findings(
    artifact: &ArtifactEnvelope,
    derivations: &BTreeMap<PropertyId, ZenExpression>,
    child_type: &ObjectTypeName,
    base_env: &ZenTypeEnv,
    derive_env: &ZenTypeEnv,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (property, source) in derivations {
        let expected = base_env
            .record(child_type.as_str())
            .and_then(|record| record.fields.get(property.as_str()))
            .map(|field| field.ty.clone());
        let Some(expected) = expected else {
            findings.push(owned_row_expression_invalid(
                artifact,
                format!(
                    "row derivation target {:?} is not in the typed child scope",
                    property.as_str()
                ),
            ));
            continue;
        };
        match Expression::compile(source.as_str()) {
            Ok(expression) => {
                if let Err(errors) = check_expression(&expression, derive_env, &expected) {
                    findings.extend(errors.into_iter().map(|error| {
                        owned_row_expression_invalid(
                            artifact,
                            format!(
                                "row derivation for {:?} is invalid: {} ({})",
                                property.as_str(),
                                error.message,
                                error.code
                            ),
                        )
                    }));
                }
            }
            Err(error) => findings.push(owned_row_expression_invalid(
                artifact,
                format!(
                    "row derivation for {:?} does not compile: {error}",
                    property.as_str()
                ),
            )),
        }
    }
    findings
}

fn action_expression_fields(root: &ActionRoot) -> Option<BTreeMap<String, Type>> {
    let properties = root.input_schema.get("properties")?.as_object()?;
    let required: BTreeSet<String> = match root.input_schema.get("required") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(JsonValue::as_string)
            .map(|value| value.map(str::to_owned))
            .collect::<Option<_>>()?,
        Some(_) => return None,
        None => BTreeSet::new(),
    };
    properties
        .iter()
        .map(|(name, schema)| {
            let mut ty = crate::typed_roots::action_schema_type(schema)?;
            if !required.contains(name) {
                ty = ty.optional();
            }
            Some((name.clone(), ty))
        })
        .collect()
}

fn draft_write_mode_findings(
    action: &ArtifactEnvelope,
    kind: &str,
    lifecycles: &[EffectiveLifecycle],
    lifecycle: &PropertyId,
    state: &str,
) -> Vec<Diagnostic> {
    let property = lifecycle.as_str();
    let pointer = "/payload/effectProgram/writeMode";
    let Some(effective) = lifecycles
        .iter()
        .find(|effective| effective.lifecycle.property == *lifecycle)
    else {
        return vec![lifecycle_finding(
            codes_lifecycle::NOT_A_LIFECYCLE,
            format!("{property:?} of {kind:?} has no declared lifecycle for draft-only writes"),
            action,
            pointer,
        )];
    };
    let mut findings = Vec::new();
    if !effective
        .lifecycle
        .initial
        .iter()
        .any(|initial| initial == state)
    {
        findings.push(finding(
            codes::INVALID_WRITE_MODE,
            format!(
                "draft-only state {state:?} must be an initial state of the {property:?} lifecycle"
            ),
            action,
            pointer,
        ));
    }
    if effective
        .lifecycle
        .immutable
        .iter()
        .any(|immutable| immutable == state)
    {
        findings.push(finding(
            codes::INVALID_WRITE_MODE,
            format!(
                "draft-only state {state:?} cannot also be immutable in the {property:?} lifecycle"
            ),
            action,
            pointer,
        ));
    }
    findings
}

/// A transition effect names a lifecycle of the effective target type
/// (inherited or narrowed) and a transition id that effective lifecycle
/// declares.
fn transition_findings(
    action: &ArtifactEnvelope,
    kind: &str,
    lifecycles: &[EffectiveLifecycle],
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> Option<Diagnostic> {
    let property = lifecycle.as_str();
    let Some(effective) = lifecycles
        .iter()
        .find(|effective| effective.lifecycle.property == *lifecycle)
    else {
        return Some(lifecycle_finding(
            codes_lifecycle::NOT_A_LIFECYCLE,
            format!("{property:?} of {kind:?} has no declared lifecycle to transition"),
            action,
            "/payload/effectProgram/lifecycle",
        ));
    };
    let declared = effective
        .lifecycle
        .transitions
        .iter()
        .any(|declared| declared.id == *transition);
    (!declared).then(|| {
        lifecycle_finding(
            codes_lifecycle::UNKNOWN_TRANSITION,
            format!(
                "{:?} is not a declared transition of the {property:?} lifecycle of {kind:?} (inherited and narrowed)",
                transition.as_str()
            ),
            action,
            "/payload/effectProgram/transition",
        )
    })
}

fn action_model_environment(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    let mut layer = Layer::default();
    let mut object_types = Vec::new();
    for (index, artifact) in base.iter().chain(artifacts).enumerate() {
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
        if let ArtifactPayload::Entity(payload) = &artifact.payload
            && let Ok(Some(object)) = entity_object_type(payload)
        {
            object_types.push(object.root.object_type);
        }
    }
    let property_types: Vec<_> = layer.catalog.iter().map(|(_, def)| def.clone()).collect();
    let definitions = EntityDefinitions {
        property_types: &property_types,
        interfaces: &layer.interfaces,
        object_types: &object_types,
        extensions: &layer.extensions,
    };
    ZenTypeEnv::from_artifact_set_for_checking(&definitions, registry, EnvOptions::default())
}

/// The object types of the set and the base, by name, in definition order
/// (base first).
pub(crate) struct Model<'a> {
    by_name: BTreeMap<String, Vec<Defined<'a>>>,
    expression_environment: Result<ZenTypeEnv, Vec<norn_expr::EnvError>>,
}

impl<'a> Model<'a> {
    pub(crate) fn new(
        artifacts: &'a [ArtifactEnvelope],
        base: &'a [ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
        effective: bool,
    ) -> Self {
        // The effective view is only needed to check writes.
        let catalog = effective.then(|| {
            let combined: Vec<ArtifactEnvelope> = base.iter().chain(artifacts).cloned().collect();
            EffectiveCatalog::from_artifacts_with_registry(&combined, registry)
        });
        let mut by_name: BTreeMap<String, Vec<Defined<'a>>> = BTreeMap::new();
        let pool = base
            .iter()
            .map(|artifact| (artifact, false))
            .chain(artifacts.iter().map(|artifact| (artifact, true)));
        for (artifact, candidate) in pool {
            let ArtifactPayload::Entity(payload) = &artifact.payload else {
                continue;
            };
            let Ok(Some(object_type)) = entity_object_type(payload) else {
                continue;
            };
            let declared = || {
                object_type
                    .root
                    .object_type
                    .properties
                    .iter()
                    .filter(|property| property.is_read_only())
                    .map(|property| property.id.0.clone())
                    .collect()
            };
            let effective = catalog
                .as_ref()
                .and_then(|catalog| catalog.object_type(&artifact.artifact_id));
            let (computed, lifecycles, effective_object_type) = match effective {
                Some(Ok(effective)) => (
                    effective
                        .properties
                        .iter()
                        .filter(|property| property.computed)
                        .map(|property| property.id.0.clone())
                        .collect(),
                    Ok(effective.lifecycles.clone()),
                    Some(Ok(effective.clone())),
                ),
                // A v1 type is a flattened read-side view and declares no
                // lifecycle. Preserve its standalone stored-property
                // contracts when the catalog cannot supply that view.
                _ if object_type.form == EntityForm::V1 => (
                    declared(),
                    Ok(Vec::new()),
                    Some(
                        EffectiveObjectType::standalone(&object_type.root.object_type)
                            .map_err(|unsupported| unsupported.feature),
                    ),
                ),
                Some(Err(blocking)) => {
                    let reason = format!(
                        "{} ({})",
                        blocking.code.as_str(),
                        blocking.artifact_id.as_deref().unwrap_or("the set")
                    );
                    (declared(), Err(reason.clone()), Some(Err(reason)))
                }
                None => {
                    let reason = "the effective catalog does not define it".to_owned();
                    (declared(), Err(reason.clone()), Some(Err(reason)))
                }
            };
            let name = object_type.root.object_type.name.0.clone();
            let links = catalog
                .as_ref()
                .and_then(|catalog| catalog.links_for_object_type(&name))
                .unwrap_or_default()
                .iter()
                .map(|link| (link.id.as_str().to_owned(), link.target.as_str().to_owned()))
                .collect();
            by_name.entry(name.clone()).or_default().push(Defined {
                name,
                artifact,
                form: object_type.form,
                authority: object_type.root.mutation_authority,
                computed,
                candidate,
                lifecycles,
                effective_object_type,
                links,
            });
        }
        let expression_environment = action_model_environment(artifacts, base, registry);
        Self {
            by_name,
            expression_environment,
        }
    }

    /// The (first) definition of `name`.
    pub(crate) fn object_type(&self, name: &str) -> Option<&Defined<'a>> {
        self.by_name.get(name).and_then(|defined| defined.first())
    }

    fn effective_object_type_named(&self, name: &str) -> Option<&EffectiveObjectType> {
        self.by_name
            .get(name)?
            .iter()
            .find_map(|defined| defined.effective_object_type.as_ref()?.as_ref().ok())
    }

    /// Whether an effect target is an owned child type or its subtype, using
    /// the same effective ownership and lineage model as the runtime catalog.
    fn is_owned_child_target(&self, target_name: &str) -> bool {
        let Some(target) = self.effective_object_type_named(target_name) else {
            return false;
        };
        self.by_name.values().flatten().any(|defined| {
            defined
                .effective_object_type
                .as_ref()
                .and_then(|effective| effective.as_ref().ok())
                .is_some_and(|parent| {
                    parent.owned_collections.iter().any(|collection| {
                        target
                            .lineage
                            .iter()
                            .any(|ancestor| ancestor == &collection.child_type)
                    })
                })
        })
    }

    /// The effective forward target of `link` on `name`, including inherited
    /// narrowing and extension-added links. Unavailable models fail closed.
    pub(crate) fn link_target(&self, name: &str, link: &str) -> Option<&str> {
        self.object_type(name)?
            .links
            .iter()
            .find(|(id, _)| id == link)
            .map(|(_, target)| target.as_str())
    }

    /// The object type `artifact` defines.
    fn defined_by(&self, artifact: &ArtifactEnvelope) -> Option<&Defined<'a>> {
        self.by_name.values().flatten().find(|defined| {
            defined.artifact.artifact_id == artifact.artifact_id
                && defined.artifact.version == artifact.version
        })
    }

    /// A candidate definition of a name already defined, where one of the two
    /// is v1 (v2 pairs are the whole-model check's).
    fn duplicate_findings(&self) -> Vec<Diagnostic> {
        let mut findings = Vec::new();
        for (name, defined) in &self.by_name {
            let Some(first) = defined.first() else {
                continue;
            };
            for later in defined.iter().skip(1).filter(|later| later.candidate) {
                if first.form == EntityForm::V2 && later.form == EntityForm::V2 {
                    continue;
                }
                let pointer = match later.form {
                    EntityForm::V1 => "/payload/objectKind",
                    EntityForm::V2 => "/payload/objectType/name",
                };
                findings.push(finding(
                    codes::DUPLICATE_OBJECT_TYPE,
                    format!(
                        "object type {name:?} is already defined by entity artifact {:?}; an object kind names one type",
                        first.artifact.artifact_id
                    ),
                    later.artifact,
                    pointer,
                ));
            }
        }
        findings
    }
}

/// The typed root of an `action` artifact, when its payload decodes.
fn decode_action(artifact: &ArtifactEnvelope) -> Option<ActionRoot> {
    let ArtifactPayload::Action(payload) = &artifact.payload else {
        return None;
    };
    serde_json::to_value(payload.to_json_value())
        .and_then(serde_json::from_value)
        .ok()
}

const fn change_name(change: WriteChange) -> &'static str {
    match change {
        WriteChange::Created => "created",
        WriteChange::Modified => "modified",
        WriteChange::Deleted => "deleted",
    }
}

fn finding(code: &str, message: String, artifact: &ArtifactEnvelope, pointer: &str) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "validate the complete closed quantity effect and its input bindings together"
)]
fn quantity_effect_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    use norn_ir::ActionQuantityOperation;
    let Some(effect) = &root.quantity_effect else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    let identity = |s: &str| {
        !s.is_empty() && s.len() <= 128 && !s.chars().any(|c| c.is_whitespace() || c.is_control())
    };
    let supported = matches!(
        root.effect_program,
        Some(
            ActionEffectProgram::FireTransition { .. }
                | ActionEffectProgram::UpsertInputObject { .. }
        )
    );
    if !supported
        || !root.ledgered
        || !root.scope_required.iter().any(|s| s == "orgId")
        || !root.scope_required.iter().any(|s| s == "environmentId")
        || !identity(&effect.ledger_id)
    {
        errors.push("quantity effects require a ledgered scoped ordinary transition/upsert and a fixed ledger identity".to_owned());
    }
    let props = root
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object);
    if !input_schema_field_required(&root.input_schema, &effect.balance_versions_field)
        || props
            .and_then(|p| p.get(&effect.balance_versions_field))
            .and_then(JsonValue::as_object)
            .and_then(|v| v.get("type"))
            .and_then(JsonValue::as_string)
            != Some("array")
        || root
            .derive
            .iter()
            .any(|d| d.field == effect.balance_versions_field)
    {
        errors.push("balanceVersionsField must name a required caller-supplied array".to_owned());
    }
    let mut strings = Vec::new();
    match &effect.operation {
        ActionQuantityOperation::Reserve {
            reservation_id_field,
            template_id,
            parameters,
            ttl_ms,
        } => {
            strings.push(reservation_id_field.as_str());
            if !identity(template_id)
                || ttl_ms.0 == 0
                || parameters.len() > 16
                || parameters
                    .iter()
                    .any(|(id, p)| !identity(id) || p.scale.0 > 28)
            {
                errors.push("reserve needs a fixed template, a positive deadline and bounded exact parameter bindings".to_owned());
            }
            strings.extend(parameters.values().map(|p| p.field.as_str()));
        }
        ActionQuantityOperation::PostReservation {
            reservation_id_field,
            posting_id_field,
        } => {
            strings.extend([reservation_id_field.as_str(), posting_id_field.as_str()]);
        }
    }
    for field in strings {
        if field == effect.balance_versions_field
            || !input_schema_field_is_string(&root.input_schema, field)
            || (!input_schema_field_required(&root.input_schema, field)
                && !root
                    .derive
                    .iter()
                    .any(|d| d.field == field && !d.input_required))
        {
            errors.push(format!(
                "quantity binding {field:?} must name a required or derived string input"
            ));
        }
    }
    if let Some(program) = &root.effect_program {
        let target = program.target();
        if target.object_id_field == effect.balance_versions_field {
            errors.push("quantity fences cannot replace the business object key".to_owned());
        }
        if let Some(effective) = model.effective_object_type_named(target.object_kind)
            && (effective
                .properties
                .iter()
                .any(|p| p.id.as_str() == effect.balance_versions_field)
                || effective.keys.iter().any(|k| k.naming_series.is_some()))
        {
            errors.push("quantity fence transport cannot be a business property or accompany a generated key".to_owned());
        }
    }
    errors
        .into_iter()
        .map(|message| {
            finding(
                codes::QUANTITY_EFFECT,
                message,
                artifact,
                "/payload/quantityEffect",
            )
        })
        .collect()
}

fn create_link_findings(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for binding in &root.create_links {
        let source = root
            .effect_program
            .as_ref()
            .filter(|p| {
                matches!(
                    p,
                    ActionEffectProgram::UpsertInputObject {
                        write_mode: UpsertWriteMode::CreateOnly,
                        ..
                    }
                )
            })
            .and_then(|p| model.object_type(p.target().object_kind));
        let declared = source.and_then(|s| {
            s.links
                .iter()
                .find(|(id, _)| id == &binding.link)
                .map(|(_, target)| target)
        });
        let in_slice = root.slice.iter().any(|entry| entry.object_type.as_str() == binding.target_type && !entry.optional && matches!(&entry.key, SliceKey::Property { from, property } if from == "input" && property.as_str() == binding.target_id_field));
        if root.create_links.len() > 16
            || !seen.insert(&binding.link)
            || !in_slice
            || !input_schema_field_is_string(&root.input_schema, &binding.target_id_field)
            || declared.is_none_or(|target| target != &binding.target_type)
        {
            findings.push(finding(codes::CREATE_LINK, "createLinks requires a create-only business effect, a unique declared link and a required authorized input-key slice of its exact target type".to_owned(), artifact, "/payload/createLinks"));
        }
    }
    findings
}
