//! `MappingIR` compilation and application (T8-MAP-1b, #2601).
//!
//! [`super::compile_mapping_artifact`] lowers an admitted mapping to ordinary runtime
//! primitives (AGENTS.md: connectors compile to actions, object patches,
//! and ledger events):
//!
//! - per map, one **observe action** (`<mapping id>.<map id>.observe`): an
//!   `upsert_input_object` effect over a closed input schema with the
//!   object key ([`OBJECT_ID_FIELD`]) and every mapped property, writing the
//!   target object type (created or modified);
//! - per lifecycle rule, **transition action artifacts** for every declared
//!   transition, so downstream consumers can use the target type's declared
//!   actions; applying the mapping emits an observed target-state snapshot,
//!   not a transition invocation;
//! - per map, its [`MappingCoverage`]: every target member it maps (with
//!   the source fields the rule reads) or declares a gap for, and the source
//!   fields it ignores. The ontology lint consumes it; the artifact-set
//!   check refuses a mapping that leaves a required member uncovered.
//!
//! Integer properties travel as JSON numbers; decimals as exact decimal
//! strings (OPEN-DECISIONS D37, the encoding of decimal defaults); text,
//! dates, timestamps, durations, and enums as strings.
//!
//! [`CompiledMapping::apply`] runs the compiled mapping over source
//! documents: each record is checked against its dataset's `input` record
//! type, an optional map guard runs before its subject and source identity,
//! and every matched rule is evaluated under a metered budget. A matched
//! record yields one [`MappedRecord`]: the observe invocation, lifecycle
//! snapshots, link patches, and [`InstanceProvenance`] per produced member.
//! Application is pure: the caller supplies the documents and the recording
//! time.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use norn_expr::{EvaluationBudget, Expression, FieldSource, Type, Value, expression_reads};
use norn_ir::entitlement::{
    Entitlement, EntitlementCondition, EntitlementGrantee, EntitlementSource, EntitlementStatus,
    ObjectRef,
};
use norn_ir::mapping::{
    ActionInvocation, EntitlementConditionMap, EntitlementCoverageStatus, EntitlementFieldPath,
    EntitlementGranteeKind, EntitlementJoinCardinality, EntitlementJoinMode, EntitlementMap,
    IdentityBinding, LinkPatch, MappedDelete, MappedRecord, MappingIr, NestedSource, ObjectMap,
    ObservedLifecycleSnapshot,
};
use norn_ir::provenance::{InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp};
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::{
    ActionRoot, ArtifactDependency, ArtifactEnvelope, ArtifactKind, ArtifactPayload, Decimal,
    IrVersion01, LinkId, MarkingId, ObjectTypeName, PropertyId, ResourceLimitProfile,
    RuntimeObject, RuntimeValue, SUPPORTED_IR_VERSION,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{MappingModel, check_mapping, codes, input_fields, mapping_key};
use crate::lint::coverage::{
    IgnoredSourceField, MappingCoverage, MemberCoverage, MemberRef, MemberStatus, SourceRef,
};
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// The input field of a compiled action naming the object it writes.
pub const OBJECT_ID_FIELD: &str = "objectId";

/// `input.<field>` and `input['<field>']` reads in a rule.
static INPUT_READ: LazyLock<Option<regex_lite::Regex>> = LazyLock::new(|| {
    regex_lite::Regex::new(
        r#"\binput\s*(?:\.\s*([A-Za-z_][A-Za-z0-9_]*)|\[\s*(?:'([^']*)'|"([^"]*)")\s*\])"#,
    )
    .ok()
});

/// One source document: a record of `dataset` as the source returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDocument {
    /// The `SourceSchemaIR` dataset id.
    pub dataset: String,
    /// The record (a JSON object; nested child rows stay in their parent
    /// field).
    pub record: norn_canonical::JsonValue,
    /// When the source last changed the record, if it says.
    pub observed_at: Option<UtcTimestamp>,
}

impl SourceDocument {
    /// Parse a source document without rounding decimal JSON numbers.
    ///
    /// # Errors
    ///
    /// Invalid JSON, duplicate normalized keys, or numbers outside the
    /// canonical parser's bounds. Expression representability is checked
    /// fallibly by [`CompiledMapping::apply`].
    pub fn from_json(
        dataset: impl Into<String>,
        record: &str,
        observed_at: Option<UtcTimestamp>,
    ) -> Result<Self, norn_canonical::CanonicalJsonError> {
        Ok(Self {
            dataset: dataset.into(),
            record: norn_canonical::parse(record)?,
            observed_at,
        })
    }
}

/// A checked mapping lowered to actions, coverage, and an applicator. See
/// the module docs.
#[derive(Debug, Clone)]
pub struct CompiledMapping {
    key: String,
    artifact_id: String,
    source_system: String,
    source_instance: String,
    actions: Vec<ActionRoot>,
    action_artifacts: Vec<ArtifactEnvelope>,
    coverage: Vec<MappingCoverage>,
    entitlement_coverage: Vec<EntitlementCoverageReport>,
    maps: Vec<CompiledMap>,
    entitlement_maps: Vec<CompiledEntitlementMap>,
}

/// Generic coverage of one permission mapping family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementCoverageReport {
    /// The declarative entitlement map id.
    pub map: String,
    /// The mapped business target type.
    pub target_type: ObjectTypeName,
    /// Exact, narrowed, or missing coverage category.
    pub status: EntitlementCoverageStatus,
    /// Why this coverage level applies.
    pub reason: String,
}

/// Results of applying a complete entitlement snapshot, including explicit
/// deletes for keys previously owned by this mapping but absent now.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedSnapshot {
    /// Business and entitlement records emitted by the snapshot.
    pub records: Vec<MappedRecord>,
    /// Prior entitlement keys removed by authoritative snapshot reconciliation.
    pub deletes: Vec<MappedDelete>,
}

#[derive(Debug, Clone)]
struct CompiledMap {
    id: String,
    dataset: String,
    nested_in: Option<NestedSource>,
    object_type: ObjectTypeName,
    input_types: BTreeMap<String, Type>,
    when: Option<Expression>,
    subject: Expression,
    source_record: Expression,
    properties: Vec<CompiledProperty>,
    identity_metadata: PropertyProvenance,
    property_metadata: BTreeMap<String, PropertyProvenance>,
    links: Vec<CompiledLink>,
    identity_bindings: BTreeMap<String, IdentityBinding>,
    lifecycles: Vec<CompiledLifecycleRule>,
    observe_action: String,
}

#[derive(Debug, Clone, Default)]
struct PropertyProvenance {
    source_fields: Vec<String>,
    markings: Vec<MarkingId>,
}

#[derive(Debug, Clone)]
struct CompiledProperty {
    property: PropertyId,
    expression: Expression,
    target_type: Type,
    nested: Option<CompiledNestedProperty>,
}

#[derive(Debug, Clone)]
struct CompiledNestedProperty {
    dataset: String,
    field: String,
    input_types: BTreeMap<String, Type>,
    predicate: Expression,
}

#[derive(Debug, Clone)]
struct CompiledLifecycleRule {
    property: PropertyId,
    expression: Expression,
    /// Present for legacy transition expressions; maps transition ids to
    /// the destination states they observe.
    transition_destinations: Option<BTreeMap<String, String>>,
    /// The terminal state a source delete means (#3013).
    on_delete: Option<String>,
}

#[derive(Debug, Clone)]
struct CompiledLink {
    link: LinkId,
    expression: Expression,
    target_type: ObjectTypeName,
    identity_binding: bool,
    required: bool,
    reviewed_references: Vec<norn_ir::mapping_evidence::ReviewedReference>,
    chunks: Vec<Expression>,
}

enum LinkTargetResolution {
    Clear,
    Target(String),
}

#[derive(Debug, Clone)]
struct CompiledEntitlementMap {
    id: String,
    source_alias: String,
    source_dataset: String,
    joins: Vec<CompiledEntitlementJoin>,
    required_snapshot: std::collections::BTreeSet<String>,
    target_type: ObjectTypeName,
    grantee_kind: EntitlementGranteeKind,
    grantee_type: ObjectTypeName,
    grantee_key: EntitlementFieldPath,
    eligibility: Vec<Expression>,
    actions: Vec<(String, Expression)>,
    scope_attributes: BTreeMap<String, Expression>,
    condition: EntitlementConditionMap,
    source_record: String,
    source_version: String,
    identity_bindings: BTreeMap<String, IdentityBinding>,
    alias_types: BTreeMap<String, BTreeMap<String, Type>>,
    observe_action: String,
    status: EntitlementStatus,
}

#[derive(Debug, Clone)]
struct CompiledEntitlementJoin {
    alias: String,
    dataset: String,
    mode: EntitlementJoinMode,
    cardinality: EntitlementJoinCardinality,
    on: Vec<(EntitlementFieldPath, String)>,
    nullable_fields: BTreeSet<String>,
    record_id: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EntitlementJoinIndexSpec {
    dataset: String,
    fields: Vec<String>,
    nullable_fields: BTreeSet<String>,
}

#[derive(Debug)]
struct IndexedEntitlementDocuments<'a> {
    by_dataset: BTreeMap<&'a str, Vec<&'a SourceDocument>>,
    by_join_key: BTreeMap<EntitlementJoinIndexSpec, BTreeMap<Vec<String>, Vec<&'a SourceDocument>>>,
}

impl<'a> IndexedEntitlementDocuments<'a> {
    fn join_candidates(
        &self,
        spec: &EntitlementJoinIndexSpec,
        join: &CompiledEntitlementJoin,
        row: &BTreeMap<String, &'a SourceDocument>,
    ) -> Option<&[&'a SourceDocument]> {
        let key = join
            .on
            .iter()
            .map(|(left, _)| row_text(row, left).ok())
            .collect::<Option<Vec<_>>>()?;
        self.by_join_key.get(spec)?.get(&key).map(Vec::as_slice)
    }
}

// A permission snapshot runs as one synchronous checker operation. Keep
// join comparisons, join-key index entries, and materialized rows bounded
// across every entitlement map, rather than giving each map a fresh allowance.
const MAX_ENTITLEMENT_JOIN_COMPARISONS: usize = 100_000;
const MAX_ENTITLEMENT_INDEXED_DOCUMENTS: usize = 100_000;
const MAX_ENTITLEMENT_JOIN_INDEX_ENTRIES: usize = 100_000;
const MAX_ENTITLEMENT_INTERMEDIATE_ROWS: usize = 4_096;
pub(super) const MAX_ENTITLEMENT_EVIDENCE_VALUE_BYTES: usize = 1_024;

#[derive(Debug, Clone, Copy)]
enum EntitlementJoinBudgetExceeded {
    Comparisons,
    IndexedDocuments,
    JoinIndexEntries,
    IntermediateRows,
}

#[derive(Debug, Default)]
struct EntitlementJoinBudget {
    comparisons: usize,
    index_entries: usize,
    intermediate_rows: usize,
}

impl EntitlementJoinBudget {
    fn comparison(&mut self) -> Result<(), EntitlementJoinBudgetExceeded> {
        if self.comparisons >= MAX_ENTITLEMENT_JOIN_COMPARISONS {
            return Err(EntitlementJoinBudgetExceeded::Comparisons);
        }
        self.comparisons = self.comparisons.saturating_add(1);
        Ok(())
    }

    fn index_entry(&mut self) -> Result<(), EntitlementJoinBudgetExceeded> {
        if self.index_entries >= MAX_ENTITLEMENT_JOIN_INDEX_ENTRIES {
            return Err(EntitlementJoinBudgetExceeded::JoinIndexEntries);
        }
        self.index_entries = self.index_entries.saturating_add(1);
        Ok(())
    }

    fn intermediate_row(&mut self) -> Result<(), EntitlementJoinBudgetExceeded> {
        if self.intermediate_rows >= MAX_ENTITLEMENT_INTERMEDIATE_ROWS {
            return Err(EntitlementJoinBudgetExceeded::IntermediateRows);
        }
        self.intermediate_rows = self.intermediate_rows.saturating_add(1);
        Ok(())
    }
}

impl EntitlementJoinBudgetExceeded {
    fn diagnostic_message(self) -> String {
        match self {
            Self::Comparisons => format!(
                "complete permission snapshot exceeded the shared entitlement join comparison budget ({MAX_ENTITLEMENT_JOIN_COMPARISONS})"
            ),
            Self::IndexedDocuments => format!(
                "complete permission snapshot exceeded the entitlement input document budget ({MAX_ENTITLEMENT_INDEXED_DOCUMENTS})"
            ),
            Self::JoinIndexEntries => format!(
                "complete permission snapshot exceeded the shared entitlement join-key index entry budget ({MAX_ENTITLEMENT_JOIN_INDEX_ENTRIES})"
            ),
            Self::IntermediateRows => format!(
                "complete permission snapshot exceeded the per-map entitlement intermediate-row budget ({MAX_ENTITLEMENT_INTERMEDIATE_ROWS})"
            ),
        }
    }
}

fn entitlement_join_index_spec(join: &CompiledEntitlementJoin) -> EntitlementJoinIndexSpec {
    EntitlementJoinIndexSpec {
        dataset: join.dataset.clone(),
        fields: join.on.iter().map(|(_, right)| right.clone()).collect(),
        nullable_fields: join.nullable_fields.clone(),
    }
}

fn index_entitlement_documents<'a>(
    documents: &'a [SourceDocument],
    maps: &[CompiledEntitlementMap],
    budget: &mut EntitlementJoinBudget,
) -> Result<IndexedEntitlementDocuments<'a>, Box<Diagnostic>> {
    let failure = |message| {
        Box::new(error(
            codes::EVALUATION_FAILED,
            message,
            Span::pointer("/documents"),
        ))
    };
    if documents.len() > MAX_ENTITLEMENT_INDEXED_DOCUMENTS {
        return Err(failure(
            EntitlementJoinBudgetExceeded::IndexedDocuments.diagnostic_message(),
        ));
    }
    let mut by_dataset: BTreeMap<&str, Vec<&SourceDocument>> = BTreeMap::new();
    for document in documents {
        by_dataset
            .entry(document.dataset.as_str())
            .or_default()
            .push(document);
    }
    let specs: BTreeSet<_> = maps
        .iter()
        .filter(|map| !map.observe_action.is_empty())
        .flat_map(|map| map.joins.iter().map(entitlement_join_index_spec))
        .collect();
    let mut by_join_key = BTreeMap::new();
    for spec in specs {
        let mut indexed = BTreeMap::<Vec<String>, Vec<&SourceDocument>>::new();
        if let Some(dataset_documents) = by_dataset.get(spec.dataset.as_str()) {
            for &document in dataset_documents {
                let Some(key) = entitlement_index_key(document, &spec).map_err(failure)? else {
                    continue;
                };
                budget
                    .index_entry()
                    .map_err(|exceeded| failure(exceeded.diagnostic_message()))?;
                indexed.entry(key).or_default().push(document);
            }
        }
        by_join_key.insert(spec, indexed);
    }
    Ok(IndexedEntitlementDocuments {
        by_dataset,
        by_join_key,
    })
}

fn entitlement_index_key(
    document: &SourceDocument,
    spec: &EntitlementJoinIndexSpec,
) -> Result<Option<Vec<String>>, String> {
    let mut key = Vec::with_capacity(spec.fields.len());
    let mut nullable_absence = false;
    for field in &spec.fields {
        if let Some(value) = record_text(&document.record, field) {
            key.push(value);
            continue;
        }
        let absent = match &document.record {
            norn_canonical::JsonValue::Object(fields) => {
                matches!(
                    fields.get(field),
                    None | Some(norn_canonical::JsonValue::Null)
                )
            }
            norn_canonical::JsonValue::Null
            | norn_canonical::JsonValue::Bool(_)
            | norn_canonical::JsonValue::Number(_)
            | norn_canonical::JsonValue::String(_)
            | norn_canonical::JsonValue::Array(_) => false,
        };
        if absent && spec.nullable_fields.contains(field) {
            nullable_absence = true;
            continue;
        }
        return Err(format!(
            "source dataset {:?} has a malformed text entitlement join key {field:?}",
            spec.dataset
        ));
    }
    Ok((!nullable_absence).then_some(key))
}

/// Compile the `mapping` artifact `artifact` (whose payload is `mapping`)
/// against its bound `source` schema and the target `model`.
///
/// # Errors
///
/// Every finding of [`check_mapping`] (spans point into the payload), or a
/// [`codes::UNENCODABLE_MEMBER`] for a mapped property whose type has no
/// action-input encoding.
pub(super) fn compile_mapping(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    source: &SourceSchemaIr,
    model: &MappingModel,
) -> Result<CompiledMapping, Vec<Diagnostic>> {
    let findings = check_mapping(mapping, source, model);
    if !findings.is_empty() {
        return Err(findings);
    }
    let mut out = CompiledMapping {
        key: mapping_key(artifact),
        artifact_id: artifact.artifact_id.clone(),
        source_system: source.source.system.clone(),
        source_instance: source.source.instance.clone(),
        actions: Vec::new(),
        action_artifacts: Vec::new(),
        coverage: Vec::new(),
        entitlement_coverage: Vec::new(),
        maps: Vec::new(),
        entitlement_maps: Vec::new(),
    };
    let mut findings = Vec::new();
    for (index, map) in mapping.maps.iter().enumerate() {
        match compile_map(artifact, mapping, map, index, source, model) {
            Ok((compiled, actions, coverage)) => {
                out.maps.push(compiled);
                out.actions.extend(actions);
                out.coverage.push(coverage);
            }
            Err(finding) => findings.push(*finding),
        }
    }
    for (index, map) in mapping.entitlements.iter().enumerate() {
        match compile_entitlement_map(artifact, mapping, map, index, source, model) {
            Ok((compiled, action, coverage)) => {
                out.entitlement_maps.push(compiled);
                if let Some(action) = action {
                    out.actions.push(action);
                }
                out.entitlement_coverage.push(coverage);
            }
            Err(finding) => findings.push(*finding),
        }
    }
    if !findings.is_empty() {
        return Err(findings);
    }
    for root in &out.actions {
        let payload = serde_json::to_value(root)
            .and_then(serde_json::from_value)
            .map_err(|failure| {
                vec![error(
                    codes::UNENCODABLE_MEMBER,
                    failure.to_string(),
                    Span::pointer(""),
                )]
            })?;
        let dependencies = root
            .effect_program
            .as_ref()
            .map(|program| {
                let target = program.target();
                vec![ArtifactDependency {
                    artifact_id: target.entity_artifact_id.to_owned(),
                    kind: ArtifactKind::Entity,
                    version: Some(target.entity_artifact_version.to_owned()),
                    required: true,
                }]
            })
            .unwrap_or_default();
        out.action_artifacts.push(ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_owned(),
            artifact_id: root.action_id.clone(),
            namespace: artifact.namespace.clone(),
            name: root.action_id.clone(),
            version: artifact.version.clone(),
            lifecycle: artifact.lifecycle.clone(),
            scope: artifact.scope.clone(),
            dependencies,
            provenance: artifact.provenance.clone(),
            payload: ArtifactPayload::Action(payload),
        });
    }
    Ok(out)
}

#[allow(
    clippy::too_many_lines,
    reason = "compile one entitlement map as a single fail-fast typed lowering pass"
)]
fn compile_entitlement_map(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    map: &EntitlementMap,
    index: usize,
    source: &SourceSchemaIr,
    model: &MappingModel,
) -> Result<
    (
        CompiledEntitlementMap,
        Option<ActionRoot>,
        EntitlementCoverageReport,
    ),
    Box<Diagnostic>,
> {
    let at = || Span::from_segments(["entitlements", index.to_string().as_str()]);
    let unresolvable = || {
        Box::new(error(
            codes::INVALID_ENTITLEMENT_MAP,
            format!("entitlement map {:?} cannot be compiled", map.id),
            at(),
        ))
    };
    let target_type = model
        .object_type_of("core.Entitlement.entity")
        .ok_or_else(unresolvable)?
        .clone();
    let (target_entity_id, target_entity_version) = model
        .entity_artifact(target_type.as_str())
        .ok_or_else(unresolvable)?;
    let mut alias_types = BTreeMap::new();
    alias_types.insert(
        map.source_alias.clone(),
        input_fields(source, &map.source.dataset).ok_or_else(unresolvable)?,
    );
    let mut joins = Vec::new();
    for join in &map.joins {
        let on = join
            .on
            .iter()
            .map(|key| (key.left.clone(), key.right.clone()))
            .collect();
        joins.push(CompiledEntitlementJoin {
            alias: join.alias.clone(),
            dataset: join.dataset.clone(),
            mode: join.mode,
            cardinality: join.cardinality,
            on,
            nullable_fields: join
                .on
                .iter()
                .filter(|key| {
                    source
                        .field(&join.dataset, &key.right)
                        .is_some_and(|field| field.required != Some(true))
                })
                .map(|key| key.right.clone())
                .collect(),
            record_id: join.record_id.clone(),
            version: join.version.clone(),
        });
        if join.mode == EntitlementJoinMode::Inner {
            alias_types.insert(
                join.alias.clone(),
                input_fields(source, &join.dataset).ok_or_else(unresolvable)?,
            );
        }
    }
    let compile = |expression: &norn_ir::ZenExpression| {
        Expression::compile(expression.as_str()).map_err(|failure| {
            Box::new(error(
                codes::INVALID_EXPRESSION,
                format!("entitlement expression does not compile: {failure}"),
                at(),
            ))
        })
    };
    let eligibility = map
        .eligibility
        .iter()
        .map(compile)
        .collect::<Result<Vec<_>, _>>()?;
    let actions = map
        .actions
        .iter()
        .map(|action| Ok((action.action.clone(), compile(&action.when)?)))
        .collect::<Result<Vec<_>, Box<Diagnostic>>>()?;
    let scope_attributes = map
        .scope_attributes
        .iter()
        .map(|(attribute, expression)| Ok((attribute.clone(), compile(expression)?)))
        .collect::<Result<BTreeMap<_, _>, Box<Diagnostic>>>()?;
    let action_id = format!("{}.{}.observe", artifact.artifact_id, map.id);
    // A Missing rule is a coverage declaration only. It emits no entitlement
    // record and therefore must not add an unused observe action to the
    // admitted action catalog.
    let action = if map.coverage.status == EntitlementCoverageStatus::Missing {
        None
    } else {
        Some(
            entitlement_observe_action_root(
                &action_id,
                &target_type,
                (target_entity_id, target_entity_version),
            )
            .map_err(|reason| {
                Box::new(error(
                    codes::UNENCODABLE_MEMBER,
                    format!("entitlement observe action does not encode: {reason}"),
                    at(),
                ))
            })?,
        )
    };
    let coverage = EntitlementCoverageReport {
        map: map.id.clone(),
        target_type: map.target_type.clone(),
        status: map.coverage.status,
        reason: map.coverage.reason.clone(),
    };
    let compiled = CompiledEntitlementMap {
        id: map.id.clone(),
        source_alias: map.source_alias.clone(),
        source_dataset: map.source.dataset.clone(),
        joins,
        required_snapshot: map.required_snapshot.iter().cloned().collect(),
        target_type: map.target_type.clone(),
        grantee_kind: map.grantee.kind,
        grantee_type: map.grantee.object_type.clone(),
        grantee_key: map.grantee.key.clone(),
        eligibility,
        actions,
        scope_attributes,
        condition: map.condition.clone(),
        source_record: map.source_record.clone(),
        source_version: map.source_version.clone(),
        identity_bindings: mapping
            .identity_bindings
            .iter()
            .map(|binding| (binding.source_key.clone(), binding.clone()))
            .collect(),
        alias_types,
        observe_action: if action.is_some() {
            action_id
        } else {
            String::new()
        },
        status: map.status,
    };
    Ok((compiled, action, coverage))
}

fn entitlement_observe_action_root(
    action_id: &str,
    object_type: &ObjectTypeName,
    entity: (&str, &str),
) -> Result<ActionRoot, String> {
    // Match observe_action_root: Core json-ext properties use the admitted
    // bounded JSON type union, and enums use string inputs. The Core entity
    // and typed entitlement boundary retain the value/variant constraints.
    let root = json!({
        "actionId": action_id,
        "title": "Import Norn entitlement",
        "description": "Upsert one canonical entitlement row observed in a complete source snapshot (MappingIR).",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "ledgerCategory": "observe",
        "inputSchema": {
            "type": "object",
            "properties": {
                OBJECT_ID_FIELD: { "type": "string" },
                "entitlement_version": { "type": "string" },
                "id": { "type": "string" },
                "grantee": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                "target_type": { "type": "string" },
                "actions": { "type": "array", "items": { "type": "string" } },
                "scope_attributes": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                "condition": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                "source": {
                    "type": "object",
                    "properties": {
                        "system": { "type": "string" },
                        "recordId": { "type": "string" },
                        "version": { "type": "string" }
                    },
                    "required": ["system", "recordId", "version"],
                    "additionalProperties": false
                },
                "status": { "type": "string" }
            },
            "required": [
                OBJECT_ID_FIELD, "entitlement_version", "id", "grantee", "target_type",
                "actions", "scope_attributes", "condition", "source", "status"
            ],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": OBJECT_ID_FIELD,
            "objectKind": object_type.as_str(),
            "entityArtifactId": entity.0,
            "entityArtifactVersion": entity.1
        },
        "writes": [
            { "entityKind": object_type.as_str(), "change": "created" },
            { "entityKind": object_type.as_str(), "change": "modified" }
        ]
    });
    serde_json::from_value(root).map_err(|decode| decode.to_string())
}

type CompiledParts = (CompiledMap, Vec<ActionRoot>, MappingCoverage);

#[allow(
    clippy::too_many_lines,
    reason = "compile one object map and its action roots as a single fail-fast lowering pass"
)]
fn compile_map(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    map: &ObjectMap,
    index: usize,
    source: &SourceSchemaIr,
    model: &MappingModel,
) -> Result<CompiledParts, Box<Diagnostic>> {
    let at = || Span::from_segments(["maps", index.to_string().as_str()]);
    let unresolved = || {
        Box::new(error(
            codes::UNKNOWN_OBJECT_TYPE,
            format!("map {:?} has no resolvable target", map.id),
            at(),
        ))
    };
    let object_type = model
        .object_type_of(&map.target.entity)
        .ok_or_else(unresolved)?
        .clone();
    let record = model
        .env()
        .record(object_type.as_str())
        .ok_or_else(unresolved)?;
    let (entity_id, entity_version) = model
        .entity_artifact(object_type.as_str())
        .ok_or_else(unresolved)?;
    let input_types = input_fields(source, &map.source.dataset).ok_or_else(unresolved)?;
    let compile = |expression: &norn_ir::ZenExpression| {
        Expression::compile(expression.as_str()).map_err(|failure| {
            Box::new(error(
                codes::INVALID_EXPRESSION,
                format!("mapping expression does not compile: {failure}"),
                at(),
            ))
        })
    };
    let when = map.when.as_ref().map(compile).transpose()?;
    let properties = compile_properties(map, record, source, &compile, &unresolved)?;
    let links = compile_links(map, mapping, model, &object_type, &compile, &unresolved)?;
    let lifecycles = compile_lifecycle_rules(map, model, &object_type, &compile, &at())?;
    let mut identity_expressions = vec![map.subject.as_str(), map.source_record.as_str()];
    identity_expressions.extend(map.when.as_ref().map(norn_ir::ZenExpression::as_str));
    let identity_reads = expression_reads_union(&identity_expressions, "input");
    let mut identity_metadata = source_provenance(&identity_reads, source, &map.source.dataset);
    if let Some(parent) = &map.source.nested_in
        && let Some(collection) = source.field(&parent.dataset, &parent.field)
    {
        identity_metadata.markings = identity_metadata
            .markings
            .into_iter()
            .chain(collection.markings.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    let mut property_metadata = BTreeMap::new();
    for entry in &properties {
        property_metadata.insert(
            entry.property.as_str().to_owned(),
            compiled_property_provenance(
                entry,
                source,
                &map.source.dataset,
                &identity_reads,
                model,
                &object_type,
            ),
        );
    }
    for lifecycle in &lifecycles {
        property_metadata.insert(
            lifecycle.property.as_str().to_owned(),
            property_provenance(
                lifecycle.expression.source(),
                source,
                &map.source.dataset,
                &identity_reads,
                model,
                &object_type,
                lifecycle.property.as_str(),
            ),
        );
    }
    for link in &links {
        let mut metadata = PropertyProvenance::default();
        for expression in std::iter::once(&link.expression).chain(&link.chunks) {
            let chunk = property_provenance(
                expression.source(),
                source,
                &map.source.dataset,
                &identity_reads,
                model,
                &object_type,
                link.link.as_str(),
            );
            metadata.source_fields.extend(chunk.source_fields);
            metadata.markings.extend(chunk.markings);
        }
        metadata.source_fields.sort();
        metadata.source_fields.dedup();
        property_metadata.insert(link.link.as_str().to_owned(), metadata);
    }
    for metadata in property_metadata.values_mut() {
        metadata.markings = metadata
            .markings
            .iter()
            .chain(&identity_metadata.markings)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    let prefix = format!("{}.{}", artifact.artifact_id, map.id);
    let observe_action = format!("{prefix}.observe");
    let mut actions = vec![
        observe_action_root(
            &observe_action,
            &object_type,
            (entity_id, entity_version),
            &properties,
        )
        .map_err(|property| {
            Box::new(error(
                codes::UNENCODABLE_MEMBER,
                format!(
                    "property {property} of {} has no action-input encoding",
                    object_type.as_str()
                ),
                at(),
            ))
        })?,
    ];
    let transition_roots = lifecycle_actions(
        &prefix,
        &object_type,
        (entity_id, entity_version),
        &lifecycles,
        model,
        &at(),
    )?;
    actions.extend(transition_roots);
    let coverage = map_coverage(artifact, mapping, map, source, &object_type, record);
    let compiled = CompiledMap {
        id: map.id.clone(),
        dataset: map.source.dataset.clone(),
        nested_in: map.source.nested_in.clone(),
        object_type,
        input_types,
        when,
        subject: compile(&map.subject)?,
        source_record: compile(&map.source_record)?,
        properties,
        identity_metadata,
        property_metadata,
        links,
        identity_bindings: mapping
            .identity_bindings
            .iter()
            .map(|binding| (binding.source_key.clone(), binding.clone()))
            .collect(),
        lifecycles,
        observe_action,
    };
    Ok((compiled, actions, coverage))
}

fn compile_properties<C, U>(
    map: &ObjectMap,
    record: &norn_expr::RecordType,
    source: &SourceSchemaIr,
    compile: &C,
    unresolved: &U,
) -> Result<Vec<CompiledProperty>, Box<Diagnostic>>
where
    C: Fn(&norn_ir::ZenExpression) -> Result<Expression, Box<Diagnostic>>,
    U: Fn() -> Box<Diagnostic>,
{
    map.properties
        .iter()
        .map(|entry| {
            let target_type = record
                .fields
                .get(entry.property.as_str())
                .map(|field| field.ty.clone())
                .ok_or_else(unresolved)?;
            let nested = entry
                .nested
                .as_ref()
                .map(|nested| {
                    let input_types =
                        input_fields(source, &nested.dataset).ok_or_else(unresolved)?;
                    Ok::<_, Box<Diagnostic>>(CompiledNestedProperty {
                        dataset: nested.dataset.clone(),
                        field: nested.field.clone(),
                        input_types,
                        predicate: compile(&nested.predicate)?,
                    })
                })
                .transpose()?;
            Ok(CompiledProperty {
                property: entry.property.clone(),
                expression: compile(&entry.value)?,
                target_type,
                nested,
            })
        })
        .collect()
}

fn compile_links<C, U>(
    map: &ObjectMap,
    mapping: &MappingIr,
    model: &MappingModel,
    object_type: &ObjectTypeName,
    compile: &C,
    unresolved: &U,
) -> Result<Vec<CompiledLink>, Box<Diagnostic>>
where
    C: Fn(&norn_ir::ZenExpression) -> Result<Expression, Box<Diagnostic>>,
    U: Fn() -> Box<Diagnostic>,
{
    let evidence = super::evidence::read_evidence(mapping).map_err(|reason| {
        Box::new(error(
            codes::INVALID_DOCUMENT,
            format!("invalid canonical reference evidence: {reason}"),
            Span::pointer("/extensions"),
        ))
    })?;
    let entries = map
        .links
        .iter()
        .map(|entry| {
            let target_type = if let Some(target_map) = &entry.target_map {
                mapping
                    .map(target_map)
                    .and_then(|joined| model.object_type_of(&joined.target.entity))
                    .ok_or_else(unresolved)?
                    .clone()
            } else {
                ObjectTypeName("party.Person".to_owned())
            };
            let reviewed_references: Vec<_> = evidence.as_ref().map_or_else(Vec::new, |evidence| {
                evidence
                    .reviewed_references
                    .iter()
                    .filter(|reference| {
                        reference.dataset == map.source.dataset
                            && reference.link == entry.link.as_str()
                    })
                    .cloned()
                    .collect()
            });
            if reviewed_references
                .iter()
                .any(|reference| reference.target_type != target_type.as_str())
            {
                return Err(unresolved());
            }
            Ok(CompiledLink {
                link: entry.link.clone(),
                expression: compile(&entry.key)?,
                target_type,
                identity_binding: entry.identity_binding,
                reviewed_references,
                chunks: Vec::new(),
                required: model
                    .env()
                    .record(object_type.as_str())
                    .and_then(|target| target.fields.get(entry.link.as_str()))
                    .is_some_and(|field| {
                        matches!(field.source, FieldSource::Link { required: true })
                    }),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut links: Vec<CompiledLink> = Vec::new();
    for entry in entries {
        if let Some(link) = links.iter_mut().find(|link| link.link == entry.link) {
            // Full admission proves these nullable chunks are complete, disjoint
            // and target the same relationship before compilation reaches here.
            link.chunks.push(entry.expression);
        } else {
            links.push(entry);
        }
    }
    Ok(links)
}

fn compile_lifecycle_rules(
    map: &ObjectMap,
    model: &MappingModel,
    object_type: &ObjectTypeName,
    compile: &impl Fn(&norn_ir::ZenExpression) -> Result<Expression, Box<Diagnostic>>,
    at: &Span,
) -> Result<Vec<CompiledLifecycleRule>, Box<Diagnostic>> {
    let mut lifecycles = Vec::new();
    for entry in &map.lifecycles {
        let (expression, transition_destinations) = match (&entry.state, &entry.transition) {
            (Some(state), None) => (compile(state)?, None),
            (None, Some(transition)) => {
                let destinations = model
                    .transitions(object_type.as_str(), entry.property.as_str())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|id| {
                        model
                            .transition_destination(
                                object_type.as_str(),
                                entry.property.as_str(),
                                id,
                            )
                            .map(|state| (id.clone(), state.to_owned()))
                    })
                    .collect();
                (compile(transition)?, Some(destinations))
            }
            _ => {
                return Err(Box::new(error(
                    codes::LIFECYCLE_RULE_SHAPE,
                    format!(
                        "lifecycle {} must specify exactly one of state or transition",
                        entry.property.as_str()
                    ),
                    at.clone(),
                )));
            }
        };
        lifecycles.push(CompiledLifecycleRule {
            property: entry.property.clone(),
            expression,
            transition_destinations,
            on_delete: entry.on_delete.clone(),
        });
    }
    Ok(lifecycles)
}

fn lifecycle_actions(
    prefix: &str,
    object_type: &ObjectTypeName,
    entity: (&str, &str),
    lifecycles: &[CompiledLifecycleRule],
    model: &MappingModel,
    at: &Span,
) -> Result<Vec<ActionRoot>, Box<Diagnostic>> {
    let mut actions = Vec::new();
    for lifecycle in lifecycles {
        let property = &lifecycle.property;
        for transition in model
            .transitions(object_type.as_str(), property.as_str())
            .unwrap_or_default()
        {
            let action_id = format!("{prefix}.{}.{transition}", property.as_str());
            let root = transition_action_root(
                &action_id,
                object_type,
                entity,
                property.as_str(),
                transition,
            )
            .map_err(|reason| {
                Box::new(error(
                    codes::UNENCODABLE_MEMBER,
                    format!("transition action {action_id} does not encode: {reason}"),
                    at.clone(),
                ))
            })?;
            actions.push(root);
        }
    }
    Ok(actions)
}

/// The JSON Schema `type` of a property's action-input encoding, or `None`
/// when it has none.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn input_schema_type(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Optional(inner) => input_schema_type(inner),
        Type::Text { .. }
        | Type::Date
        | Type::Timestamp { .. }
        | Type::Duration
        | Type::Enum { .. }
        | Type::Decimal { .. }
        | Type::Number => Some("string"),
        Type::Integer => Some("integer"),
        Type::Bool => Some("boolean"),
        _ => None,
    }
}

fn observe_action_root(
    action_id: &str,
    object_type: &ObjectTypeName,
    entity: (&str, &str),
    properties: &[CompiledProperty],
) -> Result<ActionRoot, String> {
    let mut schema = serde_json::Map::new();
    schema.insert(OBJECT_ID_FIELD.to_owned(), json!({ "type": "string" }));
    let mut required = vec![OBJECT_ID_FIELD.to_owned()];
    for entry in properties {
        let property = &entry.property;
        let ty = &entry.target_type;
        if property.as_str() == OBJECT_ID_FIELD {
            return Err(property.as_str().to_owned());
        }
        let input_schema = if matches!(ty, Type::Json)
            || matches!(ty, Type::Optional(inner) if matches!(**inner, Type::Json))
        {
            // `Type::Json` is an opaque but bounded `RuntimeValue`. Encode its
            // complete value domain using JSON Schema's standard `type` union
            // rather than `{}`, which the closed `ActionIR` dialect rejects.
            json!({ "type": ["null", "boolean", "number", "string", "array", "object"] })
        } else {
            let kind = input_schema_type(ty).ok_or_else(|| property.as_str().to_owned())?;
            if matches!(ty, Type::Optional(_)) {
                json!({ "type": [kind, "null"] })
            } else {
                json!({ "type": kind })
            }
        };
        schema.insert(property.as_str().to_owned(), input_schema);
        if !matches!(ty, Type::Optional(_)) {
            required.push(property.as_str().to_owned());
        }
    }
    let root = json!({
        "actionId": action_id,
        "title": format!("Observe {}", object_type.as_str()),
        "description": format!("Upsert one {} observed in a source record (MappingIR).", object_type.as_str()),
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "ledgerCategory": "observe",
        "inputSchema": {
            "type": "object",
            "properties": schema,
            "required": required,
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": OBJECT_ID_FIELD,
            "objectKind": object_type.as_str(),
            "entityArtifactId": entity.0,
            "entityArtifactVersion": entity.1
        },
        "writes": [
            { "entityKind": object_type.as_str(), "change": "created" },
            { "entityKind": object_type.as_str(), "change": "modified" }
        ]
    });
    serde_json::from_value(root).map_err(|decode| decode.to_string())
}

fn transition_action_root(
    action_id: &str,
    object_type: &ObjectTypeName,
    entity: (&str, &str),
    property: &str,
    transition: &str,
) -> Result<ActionRoot, String> {
    let root = json!({
        "actionId": action_id,
        "title": format!("Observe {transition} of {}", object_type.as_str()),
        "description": format!(
            "Fire the {property} lifecycle's {transition} transition a source record evidences (MappingIR observed transition)."
        ),
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "ledgerCategory": "observe",
        "inputSchema": {
            "type": "object",
            "properties": { OBJECT_ID_FIELD: { "type": "string" } },
            "required": [OBJECT_ID_FIELD],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "effectProgram": {
            "kind": "fire_transition",
            "objectIdField": OBJECT_ID_FIELD,
            "objectKind": object_type.as_str(),
            "entityArtifactId": entity.0,
            "entityArtifactVersion": entity.1,
            "lifecycle": property,
            "transition": transition
        },
        "writes": [{ "entityKind": object_type.as_str(), "change": "modified" }]
    });
    serde_json::from_value(root).map_err(|decode| decode.to_string())
}

/// The source fields an expression reads (`input.x`, `input['x']`), in
/// first-read order.
fn read_fields(expression: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(grammar) = INPUT_READ.as_ref() else {
        return out;
    };
    for captures in grammar.captures_iter(expression) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        if inside_string_literal(expression, whole.start()) {
            continue;
        }
        let field = (1..=3)
            .find_map(|group| captures.get(group))
            .map(|found| found.as_str().to_owned());
        if let Some(field) = field
            && !out.contains(&field)
        {
            out.push(field);
        }
    }
    out
}

fn expression_reads_union(expressions: &[&str], root: &str) -> norn_expr::ExpressionReads {
    let mut union = norn_expr::ExpressionReads::default();
    for expression in expressions {
        let reads =
            expression_reads(expression, root).unwrap_or_else(|_| norn_expr::ExpressionReads {
                whole_root: true,
                ..Default::default()
            });
        union.fields.extend(reads.fields);
        union.whole_root |= reads.whole_root;
    }
    union
}

fn source_provenance(
    reads: &norn_expr::ExpressionReads,
    schema: &SourceSchemaIr,
    dataset: &str,
) -> PropertyProvenance {
    let mut source_fields = Vec::new();
    let mut markings = BTreeSet::new();
    for field in schema.fields_of(dataset) {
        if reads.whole_root || reads.fields.contains(&field.name) {
            source_fields.push(field.name.clone());
            markings.extend(field.markings.iter().cloned());
        }
    }
    PropertyProvenance {
        source_fields,
        markings: markings.into_iter().collect(),
    }
}

fn property_provenance(
    source: &str,
    schema: &SourceSchemaIr,
    dataset: &str,
    identity_reads: &norn_expr::ExpressionReads,
    model: &super::MappingModel,
    object_type: &ObjectTypeName,
    property: &str,
) -> PropertyProvenance {
    let mut reads = identity_reads.clone();
    let member_reads = expression_reads_union(&[source], "input");
    reads.fields.extend(member_reads.fields);
    reads.whole_root |= member_reads.whole_root;
    let mut metadata = source_provenance(&reads, schema, dataset);
    if let Some(target) = model.property_markings(object_type.as_str(), property) {
        let mut markings: BTreeSet<MarkingId> = metadata.markings.into_iter().collect();
        markings.extend(target.iter().cloned());
        metadata.markings = markings.into_iter().collect();
    }
    metadata
}

fn compiled_property_provenance(
    entry: &CompiledProperty,
    schema: &SourceSchemaIr,
    dataset: &str,
    identity_reads: &norn_expr::ExpressionReads,
    model: &super::MappingModel,
    object_type: &ObjectTypeName,
) -> PropertyProvenance {
    let Some(nested) = &entry.nested else {
        return property_provenance(
            entry.expression.source(),
            schema,
            dataset,
            identity_reads,
            model,
            object_type,
            entry.property.as_str(),
        );
    };
    let mut parent_reads = identity_reads.clone();
    parent_reads.fields.insert(nested.field.clone());
    let mut metadata = property_provenance(
        "null",
        schema,
        dataset,
        &parent_reads,
        model,
        object_type,
        entry.property.as_str(),
    );
    let child_reads = expression_reads_union(
        &[nested.predicate.source(), entry.expression.source()],
        "input",
    );
    let child_metadata = source_provenance(&child_reads, schema, &nested.dataset);
    metadata.source_fields.extend(
        child_metadata
            .source_fields
            .into_iter()
            .map(|field| format!("{}.{}", nested.field, field)),
    );
    metadata.markings = metadata
        .markings
        .into_iter()
        .chain(child_metadata.markings)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    metadata
}

fn apply_property_metadata(provenance: &mut InstanceProvenance, map: &CompiledMap, property: &str) {
    if let Some(metadata) = map.property_metadata.get(property) {
        provenance.source_fields.clone_from(&metadata.source_fields);
        provenance.markings.clone_from(&metadata.markings);
    }
}

fn apply_identity_metadata(provenance: &mut InstanceProvenance, map: &CompiledMap) {
    provenance
        .source_fields
        .clone_from(&map.identity_metadata.source_fields);
    provenance
        .markings
        .clone_from(&map.identity_metadata.markings);
}

/// Whether `byte_index` lies inside a quoted string literal. A literal such
/// as `'input.example'` is not a source-field read, while `input['example']`
/// starts outside the quoted field name and remains discoverable.
fn inside_string_literal(expression: &str, byte_index: usize) -> bool {
    let mut delimiter = None;
    let mut escaped = false;
    for (index, character) in expression.char_indices() {
        if index >= byte_index {
            break;
        }
        match delimiter {
            Some(_) if escaped => escaped = false,
            Some(_) if character == '\\' => escaped = true,
            Some(active) if character == active => delimiter = None,
            None if character == '\'' || character == '"' => delimiter = Some(character),
            Some(_) | None => {}
        }
    }
    delimiter.is_some()
}

fn map_coverage(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    map: &ObjectMap,
    source: &SourceSchemaIr,
    object_type: &ObjectTypeName,
    record: &norn_expr::RecordType,
) -> MappingCoverage {
    let mut fields: Vec<_> = source.fields_of(&map.source.dataset).collect();
    fields.sort_by_key(|field| field.ordinal);
    let shared_fields = shared_source_fields(map);
    let mut members = mapped_member_coverages(map, &shared_fields);
    add_gap_coverages(map, record, &mut members);
    MappingCoverage {
        mapping_artifact_id: artifact.artifact_id.clone(),
        mapping_version: artifact.version.clone(),
        source: SourceRef {
            schema_id: mapping.source.schema_id.clone(),
            entity: map.source.dataset.clone(),
            fields: fields.iter().map(|field| field.name.clone()).collect(),
        },
        target: object_type.clone(),
        members,
        ignored_source_fields: map
            .ignored_fields
            .iter()
            .map(|ignored| IgnoredSourceField {
                field: ignored.field.clone(),
                reason: ignored.reason.clone(),
            })
            .collect(),
    }
}

fn shared_source_fields(map: &ObjectMap) -> Vec<String> {
    let mut shared_fields = read_fields(map.subject.as_str());
    let shared_reads = read_fields(map.source_record.as_str()).into_iter().chain(
        map.when
            .as_ref()
            .into_iter()
            .flat_map(|expression| read_fields(expression.as_str())),
    );
    for field in shared_reads {
        if !shared_fields.contains(&field) {
            shared_fields.push(field);
        }
    }
    shared_fields
}

fn mapped_member_coverages(map: &ObjectMap, shared_fields: &[String]) -> Vec<MemberCoverage> {
    map.properties
        .iter()
        .map(|entry| {
            mapped_member_coverage(
                MemberRef::Property(entry.property.clone()),
                &entry.value,
                entry.nested.as_ref(),
                shared_fields,
            )
        })
        .chain(map.links.iter().map(|entry| {
            mapped_member_coverage(
                MemberRef::Link(entry.link.clone()),
                &entry.key,
                None,
                shared_fields,
            )
        }))
        .chain(map.lifecycles.iter().filter_map(|entry| {
            entry
                .state
                .as_ref()
                .or(entry.transition.as_ref())
                .map(|expression| {
                    mapped_member_coverage(
                        MemberRef::Property(entry.property.clone()),
                        expression,
                        None,
                        shared_fields,
                    )
                })
        }))
        .collect()
}

fn mapped_member_coverage(
    member: MemberRef,
    expression: &norn_ir::ZenExpression,
    nested: Option<&norn_ir::mapping::NestedPropertySource>,
    shared_fields: &[String],
) -> MemberCoverage {
    let mut source_fields = shared_fields.to_vec();
    if let Some(nested) = nested {
        if !source_fields.contains(&nested.field) {
            source_fields.push(nested.field.clone());
        }
        for field in read_fields(nested.predicate.as_str())
            .into_iter()
            .chain(read_fields(expression.as_str()))
        {
            let qualified = format!("{}.{}", nested.field, field);
            if !source_fields.contains(&qualified) {
                source_fields.push(qualified);
            }
        }
    } else {
        for field in read_fields(expression.as_str()) {
            if !source_fields.contains(&field) {
                source_fields.push(field);
            }
        }
    }
    MemberCoverage {
        member,
        status: MemberStatus::Mapped { source_fields },
    }
}

fn add_gap_coverages(
    map: &ObjectMap,
    record: &norn_expr::RecordType,
    members: &mut Vec<MemberCoverage>,
) {
    for gap in &map.gaps {
        let is_link = record
            .fields
            .get(gap.member.as_str())
            .is_some_and(|field| matches!(field.source, FieldSource::Link { .. }));
        let member = if is_link {
            MemberRef::Link(LinkId(gap.member.clone()))
        } else {
            MemberRef::Property(PropertyId(gap.member.clone()))
        };
        let reason = serde_json::to_value(gap.reason)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let reason = match &gap.note {
            Some(note) => format!("{reason}: {note}"),
            None => reason,
        };
        members.push(MemberCoverage {
            member,
            status: MemberStatus::Gap { reason },
        });
    }
}

impl CompiledMapping {
    /// The mapping key `<artifactId>@<version>`: every fact's
    /// `mappingVersion`.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The source system the mapping's source schema names
    /// (`source.system`): the system of every fact's source record.
    #[must_use]
    pub fn source_system(&self) -> &str {
        &self.source_system
    }

    /// The compiled action roots: per map the observe action, then artifacts
    /// for its declared lifecycle transitions.
    #[must_use]
    pub fn actions(&self) -> &[ActionRoot] {
        &self.actions
    }

    /// The coverage of every map, in map order.
    #[must_use]
    pub fn coverage(&self) -> &[MappingCoverage] {
        &self.coverage
    }

    /// The exact, narrowed, or missing coverage of the declared permission
    /// map families.
    #[must_use]
    pub fn entitlement_coverage(&self) -> &[EntitlementCoverageReport] {
        &self.entitlement_coverage
    }

    /// Return the declarative entitlement map id that owns `key`, if its
    /// stable mapping and source-instance namespace matches this compilation.
    ///
    /// The check intentionally recognizes well-formed keys from retired map
    /// ids too, so a complete snapshot can reconcile rows after a map rename.
    /// It does not depend on this mapping artifact's version.
    #[must_use]
    pub fn owned_entitlement_map_id(&self, key: &str) -> Option<String> {
        let mapping_prefix = format!("{}:", self.artifact_id);
        let tail = key.strip_prefix(&mapping_prefix)?;
        let (map_id, rest) = tail.split_once(':')?;
        if !super::member_id(map_id) {
            return None;
        }
        let (source_digest, source_record) = rest.split_once(':')?;
        if source_record.is_empty() {
            return None;
        }
        let source_namespace = format!("{}\0{}", self.source_system, self.source_instance);
        let expected_digest = format!("{:x}", Sha256::digest(source_namespace.as_bytes()));
        if source_digest != expected_digest {
            return None;
        }
        Some(map_id.to_owned())
    }

    /// The compiled actions as `action` artifacts with the mapping
    /// artifact's namespace, version, lifecycle, scope, and provenance.
    #[must_use]
    pub fn action_artifacts(&self) -> &[ArtifactEnvelope] {
        &self.action_artifacts
    }

    /// Apply the mapping to `documents`, recorded at `recorded_at`. Maps run
    /// in order; within a map, documents and nested rows in input order.
    /// False map guards skip records before subject, source identity, or
    /// property rules run.
    ///
    /// # Errors
    ///
    /// Every record that does not conform to its dataset's `input` type
    /// ([`codes::INVALID_RECORD`]), whose rules fail to evaluate or yield a
    /// value of the wrong shape ([`codes::EVALUATION_FAILED`]), or whose key
    /// or source identity is not a non-empty string
    /// ([`codes::INVALID_KEY_VALUE`]). A required identity link without a
    /// source key or explicit binding yields
    /// [`codes::INVALID_IDENTITY_BINDING`]. Spans point at `/documents/<index>`.
    pub fn apply(
        &self,
        documents: &[SourceDocument],
        recorded_at: &UtcTimestamp,
    ) -> Result<Vec<MappedRecord>, Vec<Diagnostic>> {
        let mut records = Vec::new();
        let mut findings = Vec::new();
        for map in &self.maps {
            for (index, document) in documents.iter().enumerate() {
                for row in rows(map, document) {
                    let at = Span::from_segments(["documents", index.to_string().as_str()]);
                    match self.apply_row(map, row, document, recorded_at) {
                        Ok(Some(record)) => records.push(record),
                        Ok(None) => {}
                        Err((code, message)) => {
                            findings.push(error(code, format!("map {:?}: {message}", map.id), at));
                        }
                    }
                }
            }
        }
        if findings.is_empty() {
            Ok(records)
        } else {
            Err(findings)
        }
    }

    /// Apply a complete source snapshot and reconcile entitlement keys owned
    /// by this mapping. Every permission source and join dataset must appear
    /// in `complete_datasets`; otherwise the operation fails closed. The
    /// caller supplies only previously owned entitlement keys, so unrelated
    /// objects are never removed.
    ///
    /// Delta/page polling through [`CompiledMapping::apply`] emits no
    /// authorization candidates. It also cannot revoke a previously imported
    /// entitlement when a source row changes or disappears, so it must not
    /// maintain enforced imports. Use this operation only after receiving all
    /// datasets in the mapping's declared complete snapshot profile.
    ///
    /// # Errors
    ///
    /// Returns diagnostics when a required dataset is not declared complete,
    /// a prior key does not match a map namespace owned by this mapping and
    /// source instance, or any business/entitlement row cannot be safely
    /// evaluated. Missing optional provenance and invalid entitlement keys
    /// omit that row candidate so the successful reconciliation deletes any
    /// previously owned key. Input documents and join comparisons have shared
    /// bounds; materialized join rows have a per-map bound. If one is exceeded, no partial result
    /// is returned. Well-formed keys for retired map ids are accepted so the
    /// operation can delete rows left by a map removal or rename.
    #[allow(
        clippy::too_many_lines,
        reason = "snapshot reconciliation stages validation, rows, and deletes atomically"
    )]
    pub fn apply_complete_snapshot(
        &self,
        documents: &[SourceDocument],
        complete_datasets: &std::collections::BTreeSet<String>,
        previously_owned_entitlements: &BTreeMap<String, std::collections::BTreeSet<String>>,
        recorded_at: &UtcTimestamp,
    ) -> Result<MappedSnapshot, Vec<Diagnostic>> {
        let mut findings = Vec::new();
        for map in &self.entitlement_maps {
            if map.observe_action.is_empty() {
                continue;
            }
            if !map.required_snapshot.is_subset(complete_datasets) {
                let missing: Vec<_> = map
                    .required_snapshot
                    .difference(complete_datasets)
                    .cloned()
                    .collect();
                findings.push(error(
                    codes::INCOMPLETE_ENTITLEMENT_SNAPSHOT,
                    format!(
                        "entitlement map {:?} needs complete datasets {missing:?}",
                        map.id
                    ),
                    Span::pointer("/entitlements"),
                ));
            }
        }
        if !findings.is_empty() {
            return Err(findings);
        }
        // Validate ownership from the key itself. This recognizes retired map
        // ids under the same stable mapping/source namespace, allowing map
        // removal or renames to revoke prior rows without trusting a caller's
        // map label as proof of ownership.
        for (map_id, keys) in previously_owned_entitlements {
            for key in keys {
                if self.owned_entitlement_map_id(key).as_deref() != Some(map_id.as_str()) {
                    findings.push(error(
                        codes::INVALID_ENTITLEMENT_MAP,
                        format!(
                            "snapshot key {key:?} is outside mapping-owned prefix for map {map_id:?}"
                        ),
                        Span::pointer("/entitlements"),
                    ));
                }
            }
        }
        if !findings.is_empty() {
            return Err(findings);
        }
        let mut entitlement_records = Vec::new();
        let mut current_by_map: BTreeMap<String, std::collections::BTreeSet<String>> =
            BTreeMap::new();
        let mut failures = Vec::new();
        let mut join_budget = EntitlementJoinBudget::default();
        let document_index = match index_entitlement_documents(
            documents,
            &self.entitlement_maps,
            &mut join_budget,
        ) {
            Ok(index) => index,
            Err(finding) => return Err(vec![*finding]),
        };
        for map in &self.entitlement_maps {
            if map.observe_action.is_empty() {
                continue;
            }
            // Intermediate rows are released after each map; comparisons and
            // index allocations remain bounded across the whole snapshot.
            join_budget.intermediate_rows = 0;
            let rows = match entitlement_rows(map, &document_index, &mut join_budget) {
                Ok(rows) => rows,
                Err(exceeded) => {
                    failures.push(error(
                        codes::EVALUATION_FAILED,
                        format!(
                            "entitlement map {:?}: {}",
                            map.id,
                            exceeded.diagnostic_message()
                        ),
                        Span::pointer("/documents"),
                    ));
                    break;
                }
            };
            for row in rows {
                match self.apply_entitlement_row(map, &row, recorded_at) {
                    Ok(Some(record)) => {
                        if current_by_map
                            .entry(map.id.clone())
                            .or_default()
                            .insert(record.key.clone())
                        {
                            entitlement_records.push(record);
                        } else {
                            failures.push(error(
                                codes::INVALID_ENTITLEMENT_MAP,
                                format!(
                                    "entitlement map {:?} generated duplicate identity {:?}",
                                    map.id, record.key
                                ),
                                Span::pointer("/documents"),
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(message) => failures.push(error(
                        codes::EVALUATION_FAILED,
                        format!("entitlement map {:?}: {message}", map.id),
                        Span::pointer("/documents"),
                    )),
                }
            }
        }
        if !failures.is_empty() {
            return Err(failures);
        }
        // Do not allocate ordinary observe upserts until all permission joins
        // have passed the shared budget and produced a complete result.
        let mut records = self.apply(documents, recorded_at)?;
        records.extend(entitlement_records);
        let mut deletes = Vec::new();
        for (map_id, prior_keys) in previously_owned_entitlements {
            let current = current_by_map.get(map_id);
            for key in prior_keys
                .iter()
                .filter(|key| !current.is_some_and(|current| current.contains(*key)))
            {
                deletes.push(MappedDelete {
                    map: map_id.clone(),
                    object_type: ObjectTypeName("core.Entitlement".to_owned()),
                    key: key.clone(),
                    lifecycle_snapshots: Vec::new(),
                    provenance: InstanceProvenance {
                        source: SourceRecordRef::new(
                            self.source_system.clone(),
                            format!("complete-snapshot/{map_id}"),
                        ),
                        mapping_version: MappingVersion(self.key.clone()),
                        transform: Some(format!("{map_id}.reconcile")),
                        observed_at: None,
                        recorded_at: recorded_at.clone(),
                        source_fields: Vec::new(),
                        markings: Vec::new(),
                        upstream: Vec::new(),
                    },
                });
            }
        }
        Ok(MappedSnapshot { records, deletes })
    }

    /// Permission rows require complete relational context. A source delta
    /// for a dataset used by a permission map therefore fails closed and
    /// directs callers to reconcile the next complete snapshot.
    fn entitlement_row_delete_is_unsafe(&self, dataset: &str) -> bool {
        self.entitlement_maps.iter().any(|map| {
            map.source_dataset == dataset || map.joins.iter().any(|join| join.dataset == dataset)
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "construct one entitlement only after all typed row values validate together"
    )]
    fn apply_entitlement_row(
        &self,
        map: &CompiledEntitlementMap,
        row: &BTreeMap<String, &SourceDocument>,
        recorded_at: &UtcTimestamp,
    ) -> Result<Option<MappedRecord>, String> {
        let context = entitlement_context(map, row)?;
        for expression in &map.eligibility {
            if !evaluate_bool(expression, &context)? {
                return Ok(None);
            }
        }
        // A missing/null source identity is an unresolvable candidate, not a
        // reason to abort a complete reconciliation and preserve an old
        // grant. Well-typed but unmapped external principals likewise produce
        // no candidate below.
        let Ok(raw_grantee_key) = row_text(row, &map.grantee_key) else {
            return Ok(None);
        };
        if !super::exact_entitlement_value(&raw_grantee_key) {
            return Ok(None);
        }
        let grantee = match map.grantee_kind {
            EntitlementGranteeKind::Role => EntitlementGrantee::Role {
                object_type: map.grantee_type.clone(),
                key: raw_grantee_key,
            },
            EntitlementGranteeKind::Group => EntitlementGrantee::Group {
                object_type: map.grantee_type.clone(),
                key: raw_grantee_key,
            },
            EntitlementGranteeKind::Principal => {
                let Some(binding) = map.identity_bindings.get(&raw_grantee_key) else {
                    return Ok(None);
                };
                EntitlementGrantee::Principal {
                    object_type: binding.object_type.clone(),
                    key: binding.key.clone(),
                }
            }
        };
        let key_prefix = entitlement_key_prefix(
            &self.artifact_id,
            &self.source_system,
            &self.source_instance,
            &map.id,
        );
        let source_reference_prefix_bytes = map.source_dataset.len().saturating_add(1);
        let source_record_overhead = key_prefix.len().max(source_reference_prefix_bytes);
        let max_source_record_bytes =
            MAX_ENTITLEMENT_EVIDENCE_VALUE_BYTES.saturating_sub(source_record_overhead);
        let Some(source_record) =
            entitlement_source_identity(map, row, false, max_source_record_bytes)?
        else {
            return Ok(None);
        };
        let Some(source_version) =
            entitlement_source_identity(map, row, true, MAX_ENTITLEMENT_EVIDENCE_VALUE_BYTES)?
        else {
            return Ok(None);
        };
        let mut actions = Vec::new();
        for (action, condition) in &map.actions {
            if evaluate_bool(condition, &context)? && !actions.contains(action) {
                actions.push(action.clone());
            }
        }
        actions.sort();
        if actions.is_empty() {
            return Ok(None);
        }
        let mut scope_attributes = BTreeMap::new();
        for (attribute, expression) in &map.scope_attributes {
            let value = text(expression, &context).map_err(|(_, message)| message)?;
            if !super::exact_entitlement_value(&value) {
                return Ok(None);
            }
            scope_attributes.insert(attribute.clone(), value);
        }
        let condition = match &map.condition {
            EntitlementConditionMap::None => EntitlementCondition::None,
            EntitlementConditionMap::OwnerOnly { link } => {
                EntitlementCondition::OwnerOnly { link: link.clone() }
            }
            EntitlementConditionMap::LinkedRecord {
                link,
                target_type,
                target_key,
            } => {
                let Ok(key) = row_text(row, target_key) else {
                    return Ok(None);
                };
                if !super::exact_entitlement_value(&key) {
                    return Ok(None);
                }
                EntitlementCondition::LinkedRecord {
                    link: link.clone(),
                    target: ObjectRef {
                        object_type: target_type.clone(),
                        key,
                    },
                }
            }
        };
        let id = format!("{key_prefix}{source_record}");
        let entitlement = Entitlement {
            entitlement_version: IrVersion01::V0_1,
            id: id.clone(),
            grantee,
            target_type: map.target_type.clone(),
            actions,
            scope_attributes,
            condition,
            source: EntitlementSource {
                system: self.source_system.clone(),
                record_id: source_record.clone(),
                version: source_version,
            },
            status: map.status,
        };
        let mut json = serde_json::to_value(&entitlement).map_err(|failure| failure.to_string())?;
        if let serde_json::Value::Object(object) = &mut json {
            object
                .entry("scope_attributes")
                .or_insert_with(|| serde_json::json!({}));
        }
        let RuntimeValue::Object(mut input) =
            RuntimeValue::from_json(json).map_err(|failure| failure.to_string())?
        else {
            return Err("the canonical entitlement did not encode as an object".to_owned());
        };
        input.insert(OBJECT_ID_FIELD.to_owned(), RuntimeValue::String(id.clone()));
        let observed_at = row
            .get(&map.source_alias)
            .and_then(|document| document.observed_at.clone());
        let source_ref = SourceRecordRef::new(
            self.source_system.clone(),
            format!("{}/{}", map.source_dataset, source_record),
        );
        let provenance = |member: &str| InstanceProvenance {
            source: source_ref.clone(),
            mapping_version: MappingVersion(self.key.clone()),
            transform: Some(format!("{}.{}", map.id, member)),
            observed_at: observed_at.clone(),
            recorded_at: recorded_at.clone(),
            source_fields: Vec::new(),
            markings: Vec::new(),
            upstream: Vec::new(),
        };
        let property_provenance = [
            "entitlement_version",
            "id",
            "grantee",
            "target_type",
            "actions",
            "scope_attributes",
            "condition",
            "source",
            "status",
        ]
        .into_iter()
        .map(|property| (PropertyId(property.to_owned()), provenance(property)))
        .collect();
        Ok(Some(MappedRecord {
            map: map.id.clone(),
            object_type: ObjectTypeName("core.Entitlement".to_owned()),
            key: id,
            invocations: vec![ActionInvocation {
                action_id: map.observe_action.clone(),
                input,
            }],
            lifecycle_snapshots: Vec::new(),
            links: Vec::new(),
            cleared_links: Vec::new(),
            property_provenance,
            provenance: provenance("id"),
        }))
    }

    /// What a source delete of `document` means (#3013): for every top-level
    /// map of its dataset, the object it removes (key and source record from
    /// the map's subject and source-record rules) and the `onDelete` states of
    /// its lifecycles. A delete payload may be partial (Shopify sends only the
    /// id), so fields that are absent or do not conform read as `null`; only
    /// the key rules must evaluate. A guarded map requires a complete source
    /// record (every declared input field present and conforming) because a
    /// partial delete cannot safely select a conditional branch.
    ///
    /// # Errors
    ///
    /// A map whose subject or source-record rule does not yield a non-empty
    /// string ([`codes::INVALID_KEY_VALUE`], [`codes::EVALUATION_FAILED`]),
    /// or a record that is not an object ([`codes::INVALID_RECORD`]).
    pub fn apply_delete(
        &self,
        document: &SourceDocument,
        recorded_at: &UtcTimestamp,
    ) -> Result<Vec<MappedDelete>, Vec<Diagnostic>> {
        if self.entitlement_row_delete_is_unsafe(&document.dataset) {
            return Err(vec![error(
                codes::INCOMPLETE_ENTITLEMENT_SNAPSHOT,
                format!(
                    "deleting from {:?} can invalidate joined entitlements; reconcile a complete snapshot to emit mapping-owned deletes",
                    document.dataset
                ),
                Span::pointer("/documents/0"),
            )]);
        }
        let mut deletes = Vec::new();
        let mut findings = Vec::new();
        let at = Span::from_segments(["documents", "0"]);
        for map in &self.maps {
            if map.nested_in.is_some() || document.dataset != map.dataset {
                continue;
            }
            match self.delete_row(map, document, recorded_at) {
                Ok(Some(delete)) => deletes.push(delete),
                Ok(None) => {}
                Err((code, message)) => {
                    findings.push(error(
                        code,
                        format!("map {:?}: {message}", map.id),
                        at.clone(),
                    ));
                }
            }
        }
        if findings.is_empty() {
            Ok(deletes)
        } else {
            Err(findings)
        }
    }

    /// The source-owned retirement template for an output actually emitted
    /// by this mapping. Guards have already matched and are not re-evaluated
    /// against a later document where this conditional output may disappear.
    ///
    /// # Errors
    /// Refuses a record from another mapping version or an unknown map.
    pub fn retirement_for_record(
        &self,
        record: &MappedRecord,
        recorded_at: &UtcTimestamp,
    ) -> Result<MappedDelete, String> {
        if record.provenance.mapping_version.as_str() != self.key() {
            return Err("retirement record belongs to another mapping version".to_owned());
        }
        let map = self
            .maps
            .iter()
            .find(|map| map.id == record.map)
            .ok_or_else(|| "retirement record names an unknown map".to_owned())?;
        let provenance = |member: &str| {
            let mut provenance = record.provenance.clone();
            provenance.recorded_at = recorded_at.clone();
            provenance.transform = Some(format!("{}.{}", map.id, member));
            provenance
        };
        Ok(MappedDelete {
            map: record.map.clone(),
            object_type: record.object_type.clone(),
            key: record.key.clone(),
            lifecycle_snapshots: map
                .lifecycles
                .iter()
                .filter_map(|lifecycle| {
                    lifecycle
                        .on_delete
                        .as_ref()
                        .map(|state| ObservedLifecycleSnapshot {
                            property: lifecycle.property.clone(),
                            state: state.clone(),
                            provenance: provenance(lifecycle.property.as_str()),
                        })
                })
                .collect(),
            provenance: provenance("delete"),
        })
    }

    fn delete_row(
        &self,
        map: &CompiledMap,
        document: &SourceDocument,
        recorded_at: &UtcTimestamp,
    ) -> Result<Option<MappedDelete>, (&'static str, String)> {
        let norn_canonical::JsonValue::Object(row) = &document.record else {
            return Err((
                codes::INVALID_RECORD,
                "the record is not an object".to_owned(),
            ));
        };
        let mut input = BTreeMap::new();
        for (field, ty) in &map.input_types {
            let raw = row.get(field);
            let value = if map.when.is_some() {
                if raw.is_none() {
                    return Err((
                        codes::INVALID_RECORD,
                        format!(
                            "partial delete is missing input field {field:?}; guarded maps require a complete record"
                        ),
                    ));
                }
                conform(raw, ty).map_err(|reason| {
                    (
                        codes::INVALID_RECORD,
                        format!("delete field {field:?} {reason}"),
                    )
                })?
            } else {
                conform(raw, ty).unwrap_or(Value::Null)
            };
            input.insert(field.clone(), value);
        }
        let context = Value::object([("input", Value::Object(input))]);
        if !map_matches(map, &context)? {
            return Ok(None);
        }
        let key = text(&map.subject, &context)?;
        let source_record = text(&map.source_record, &context)?;
        let provenance =
            |member: &str| self.provenance(map, &source_record, member, document, recorded_at);
        let lifecycle_snapshots = map
            .lifecycles
            .iter()
            .filter_map(|lifecycle| {
                lifecycle.on_delete.as_ref().map(|state| {
                    let mut recorded = provenance(lifecycle.property.as_str());
                    apply_property_metadata(&mut recorded, map, lifecycle.property.as_str());
                    ObservedLifecycleSnapshot {
                        property: lifecycle.property.clone(),
                        state: state.clone(),
                        provenance: recorded,
                    }
                })
            })
            .collect();
        let mut delete_provenance = provenance("delete");
        apply_identity_metadata(&mut delete_provenance, map);
        Ok(Some(MappedDelete {
            map: map.id.clone(),
            object_type: map.object_type.clone(),
            key,
            lifecycle_snapshots,
            provenance: delete_provenance,
        }))
    }

    fn provenance(
        &self,
        map: &CompiledMap,
        source_record: &str,
        member: &str,
        document: &SourceDocument,
        recorded_at: &UtcTimestamp,
    ) -> InstanceProvenance {
        InstanceProvenance {
            source: SourceRecordRef::new(
                self.source_system.clone(),
                format!("{}/{source_record}", map.dataset),
            ),
            mapping_version: MappingVersion(self.key.clone()),
            transform: Some(format!("{}.{member}", map.id)),
            observed_at: document.observed_at.clone(),
            recorded_at: recorded_at.clone(),
            source_fields: Vec::new(),
            markings: Vec::new(),
            upstream: Vec::new(),
        }
    }

    fn apply_row(
        &self,
        map: &CompiledMap,
        row: Option<&norn_canonical::JsonValue>,
        document: &SourceDocument,
        recorded_at: &UtcTimestamp,
    ) -> Result<Option<MappedRecord>, (&'static str, String)> {
        let Some(norn_canonical::JsonValue::Object(row)) = row else {
            return Err((
                codes::INVALID_RECORD,
                "the record is not an object".to_owned(),
            ));
        };
        let mut input = BTreeMap::new();
        for (field, ty) in &map.input_types {
            let value = conform(row.get(field), ty)
                .map_err(|reason| (codes::INVALID_RECORD, format!("field {field:?} {reason}")))?;
            input.insert(field.clone(), value);
        }
        let context = Value::object([("input", Value::Object(input))]);
        if !map_matches(map, &context)? {
            return Ok(None);
        }
        let key = text(&map.subject, &context)?;
        let source_record = text(&map.source_record, &context)?;
        let provenance =
            |member: &str| self.provenance(map, &source_record, member, document, recorded_at);
        let mut property_provenance = BTreeMap::new();
        let mut values = RuntimeObject::new();
        values.insert(
            OBJECT_ID_FIELD.to_owned(),
            RuntimeValue::String(key.clone()),
        );
        for entry in &map.properties {
            let value = match &entry.nested {
                Some(nested) => select_nested_value(
                    nested,
                    row,
                    &entry.property,
                    &entry.expression,
                    &entry.target_type,
                )?,
                None => evaluate(&entry.expression, &context)?,
            };
            if let Some(encoded) = encode(&value, &entry.target_type).map_err(|reason| {
                (
                    codes::EVALUATION_FAILED,
                    format!("property {}: {reason}", entry.property.as_str()),
                )
            })? {
                values.insert(entry.property.as_str().to_owned(), encoded);
                let mut recorded = provenance(entry.property.as_str());
                apply_property_metadata(&mut recorded, map, entry.property.as_str());
                property_provenance.insert(entry.property.clone(), recorded);
            }
        }
        let invocations = vec![ActionInvocation {
            action_id: map.observe_action.clone(),
            input: values,
        }];
        let mut lifecycle_snapshots = evaluate_lifecycle_snapshots(map, &context, &provenance)?;
        for snapshot in &mut lifecycle_snapshots {
            apply_property_metadata(&mut snapshot.provenance, map, snapshot.property.as_str());
            property_provenance.insert(snapshot.property.clone(), snapshot.provenance.clone());
        }
        let mut links = Vec::new();
        let mut cleared_links = Vec::new();
        for link in &map.links {
            match resolve_link_target(link, map, &context)? {
                LinkTargetResolution::Clear => cleared_links.push(link.link.clone()),
                LinkTargetResolution::Target(target_key) => {
                    links.push(LinkPatch {
                        object_type: map.object_type.clone(),
                        key: key.clone(),
                        link: link.link.clone(),
                        target_type: link.target_type.clone(),
                        target_key,
                        provenance: provenance(link.link.as_str()),
                    });
                }
            }
        }
        let mut record_provenance = provenance("subject");
        apply_identity_metadata(&mut record_provenance, map);
        Ok(Some(MappedRecord {
            map: map.id.clone(),
            object_type: map.object_type.clone(),
            key,
            invocations,
            lifecycle_snapshots,
            links,
            cleared_links,
            property_provenance,
            provenance: record_provenance,
        }))
    }
}

fn resolve_link_target(
    link: &CompiledLink,
    map: &CompiledMap,
    context: &Value,
) -> Result<LinkTargetResolution, (&'static str, String)> {
    if map
        .property_metadata
        .get(link.link.as_str())
        .is_some_and(|metadata| !metadata.markings.is_empty())
    {
        return Err((
            codes::EVALUATION_FAILED,
            format!(
                "classified association rule {} is unsupported until durable edge classification is available",
                link.link.as_str()
            ),
        ));
    }
    let mut evaluated = evaluate(&link.expression, context)?;
    for chunk in &link.chunks {
        let value = evaluate(chunk, context)?;
        if value != Value::Null {
            if evaluated != Value::Null {
                return Err((
                    codes::INVALID_IDENTITY_BINDING,
                    "reviewed reference chunks overlap".into(),
                ));
            }
            evaluated = value;
        }
    }
    verify_reviewed_reference(link, context, &evaluated)?;
    let target_key = match evaluated {
        Value::Null if link.required && link.identity_binding => {
            return Err((
                codes::INVALID_IDENTITY_BINDING,
                format!(
                    "required identity link {} has no source key",
                    link.link.as_str()
                ),
            ));
        }
        Value::Null if link.required => {
            return Err((
                codes::INVALID_REQUIRED_LINK,
                format!("required link {} has no source key", link.link.as_str()),
            ));
        }
        Value::Null => return Ok(LinkTargetResolution::Clear),
        Value::String(target_key) if !super::exact_entitlement_value(&target_key) => {
            if link.identity_binding && link.required {
                return Err((
                    codes::INVALID_IDENTITY_BINDING,
                    format!(
                        "required identity link {} has invalid source key {target_key:?}",
                        link.link.as_str()
                    ),
                ));
            }
            if link.required {
                return Err((
                    codes::INVALID_REQUIRED_LINK,
                    format!(
                        "required link {} has invalid source key {target_key:?}",
                        link.link.as_str()
                    ),
                ));
            }
            return Ok(LinkTargetResolution::Clear);
        }
        Value::String(target_key) => target_key,
        other @ (Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
            return Err((
                codes::EVALUATION_FAILED,
                format!("link rule {} yielded {other:?}", link.link.as_str()),
            ));
        }
    };
    if !link.identity_binding {
        return Ok(LinkTargetResolution::Target(target_key));
    }
    if let Some(binding) = map.identity_bindings.get(&target_key) {
        return Ok(LinkTargetResolution::Target(binding.key.clone()));
    }
    if link.required {
        return Err((
            codes::INVALID_IDENTITY_BINDING,
            format!(
                "required identity link {} has no binding for source key {target_key:?}",
                link.link.as_str()
            ),
        ));
    }
    Ok(LinkTargetResolution::Clear)
}

fn verify_reviewed_reference(
    link: &CompiledLink,
    context: &Value,
    evaluated: &Value,
) -> Result<(), (&'static str, String)> {
    if link.reviewed_references.is_empty() {
        return Ok(());
    }
    let Value::Object(root) = context else {
        return Err((
            codes::INVALID_IDENTITY_BINDING,
            "reference input is unavailable".into(),
        ));
    };
    let Some(Value::Object(input)) = root.get("input") else {
        return Err((
            codes::INVALID_IDENTITY_BINDING,
            "reference input is unavailable".into(),
        ));
    };
    let mut applicable = false;
    let mut expected = None;
    for reference in &link.reviewed_references {
        if reference.target_type != link.target_type.as_str() {
            return Err((
                codes::INVALID_IDENTITY_BINDING,
                "reviewed reference target type differs from LinkMap".into(),
            ));
        }
        if let Some(selector) = &reference.selector_field
            && input.get(selector)
                != reference
                    .selector_value
                    .as_ref()
                    .map(|v| Value::String(v.clone()))
                    .as_ref()
        {
            continue;
        }
        match input.get(&reference.field) {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) => {
                if !value.is_empty() {
                    applicable = true;
                    if value == &reference.external_key {
                        expected = Some(reference.canonical_key.as_str());
                    }
                }
            }
            Some(Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
                return Err((
                    codes::INVALID_IDENTITY_BINDING,
                    "canonical reference key must be text".into(),
                ));
            }
        }
    }
    let correct = match (expected, evaluated) {
        (Some(expected), Value::String(actual)) => expected == actual,
        (None, Value::Null) => !applicable,
        (
            Some(_),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_),
        )
        | (
            None,
            Value::String(_)
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_),
        ) => false,
    };
    if correct {
        Ok(())
    } else {
        Err((
            codes::INVALID_IDENTITY_BINDING,
            format!(
                "canonical reference {} has no matching reviewed exact key, or its transform differs from the reviewed binding",
                link.link.as_str()
            ),
        ))
    }
}

fn entitlement_rows<'a>(
    map: &CompiledEntitlementMap,
    documents: &IndexedEntitlementDocuments<'a>,
    budget: &mut EntitlementJoinBudget,
) -> Result<Vec<BTreeMap<String, &'a SourceDocument>>, EntitlementJoinBudgetExceeded> {
    let mut rows = Vec::new();
    if let Some(source_documents) = documents.by_dataset.get(map.source_dataset.as_str()) {
        for document in source_documents {
            budget.intermediate_row()?;
            rows.push(BTreeMap::from([(map.source_alias.clone(), *document)]));
        }
    }
    for join in &map.joins {
        let spec = entitlement_join_index_spec(join);
        if join.mode == EntitlementJoinMode::Inner {
            let mut expanded = Vec::new();
            for row in rows {
                let candidates = documents
                    .join_candidates(&spec, join, &row)
                    .unwrap_or_default();
                if join.cardinality == EntitlementJoinCardinality::AtMostOne {
                    let mut matching = None;
                    let mut ambiguous = false;
                    for &candidate in candidates {
                        budget.comparison()?;
                        if matching.is_some() {
                            ambiguous = true;
                            break;
                        }
                        matching = Some(candidate);
                    }
                    if !ambiguous && let Some(candidate) = matching {
                        budget.intermediate_row()?;
                        let mut joined = row;
                        joined.insert(join.alias.clone(), candidate);
                        expanded.push(joined);
                    }
                } else {
                    for &candidate in candidates {
                        budget.comparison()?;
                        budget.intermediate_row()?;
                        let mut joined = row.clone();
                        joined.insert(join.alias.clone(), candidate);
                        expanded.push(joined);
                    }
                }
            }
            rows = expanded;
        } else {
            let mut retained = Vec::new();
            for row in rows {
                let candidates = documents
                    .join_candidates(&spec, join, &row)
                    .unwrap_or_default();
                if !candidates.is_empty() {
                    budget.comparison()?;
                }
                if candidates.is_empty() {
                    budget.intermediate_row()?;
                    retained.push(row);
                }
            }
            rows = retained;
        }
    }
    Ok(rows)
}

fn entitlement_key_prefix(
    artifact_id: &str,
    source_system: &str,
    source_instance: &str,
    map_id: &str,
) -> String {
    let source_namespace = format!("{source_system}\0{source_instance}");
    let source_digest = format!("{:x}", Sha256::digest(source_namespace.as_bytes()));
    format!("{artifact_id}:{map_id}:{source_digest}:")
}

fn record_text(record: &norn_canonical::JsonValue, field: &str) -> Option<String> {
    let norn_canonical::JsonValue::Object(fields) = record else {
        return None;
    };
    match fields.get(field)? {
        norn_canonical::JsonValue::String(value) => Some(value.clone()),
        norn_canonical::JsonValue::Null
        | norn_canonical::JsonValue::Bool(_)
        | norn_canonical::JsonValue::Number(_)
        | norn_canonical::JsonValue::Array(_)
        | norn_canonical::JsonValue::Object(_) => None,
    }
}

fn row_text(
    row: &BTreeMap<String, &SourceDocument>,
    path: &EntitlementFieldPath,
) -> Result<String, String> {
    let document = row
        .get(&path.alias)
        .ok_or_else(|| format!("joined alias {:?} is unavailable", path.alias))?;
    record_text(&document.record, &path.field)
        .ok_or_else(|| format!("{}.{} is missing or is not text", path.alias, path.field))
}

fn entitlement_context(
    map: &CompiledEntitlementMap,
    row: &BTreeMap<String, &SourceDocument>,
) -> Result<Value, String> {
    let mut input = BTreeMap::new();
    for (alias, fields) in &map.alias_types {
        let document = row
            .get(alias)
            .ok_or_else(|| format!("joined alias {alias:?} is unavailable"))?;
        let norn_canonical::JsonValue::Object(values) = &document.record else {
            return Err(format!("joined alias {alias:?} is not a record object"));
        };
        for (field, ty) in fields {
            let value = conform(values.get(field), ty)
                .map_err(|reason| format!("field {alias}.{field} {reason}"))?;
            input.insert(format!("{alias}_{field}"), value);
        }
    }
    Ok(Value::object([("input", Value::Object(input))]))
}

fn evaluate_bool(expression: &Expression, context: &Value) -> Result<bool, String> {
    match evaluate(expression, context).map_err(|(_, message)| message)? {
        Value::Bool(value) => Ok(value),
        value @ (Value::Null
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => Err(format!("predicate yielded {value:?}, not a boolean")),
    }
}

fn entitlement_source_identity(
    map: &CompiledEntitlementMap,
    row: &BTreeMap<String, &SourceDocument>,
    version: bool,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    let base_field = if version {
        &map.source_version
    } else {
        &map.source_record
    };
    let Ok(base) = row_text(
        row,
        &EntitlementFieldPath {
            alias: map.source_alias.clone(),
            field: base_field.clone(),
        },
    ) else {
        return Ok(None);
    };
    if base.is_empty() {
        return Ok(None);
    }
    let mut parts = vec![(map.source_dataset.clone(), base.clone())];
    for join in map
        .joins
        .iter()
        .filter(|join| join.mode == EntitlementJoinMode::Inner)
    {
        let field = if version {
            join.version.as_ref()
        } else {
            join.record_id.as_ref()
        }
        .ok_or_else(|| format!("inner join {:?} lacks provenance fields", join.alias))?;
        let Ok(value) = row_text(
            row,
            &EntitlementFieldPath {
                alias: join.alias.clone(),
                field: field.clone(),
            },
        ) else {
            return Ok(None);
        };
        if value.is_empty() {
            return Ok(None);
        }
        parts.push((join.dataset.clone(), value));
    }
    let canonical_parts =
        norn_ir::canonical::canonical_json_of(&parts).map_err(|failure| failure.to_string())?;
    let digest = format!("{:x}", Sha256::digest(canonical_parts));
    let suffix = format!("~{digest}");
    let visible = format!("{}={base}", map.source_dataset);
    let visible_budget = max_bytes.checked_sub(suffix.len());
    let Some(visible_budget) = visible_budget else {
        return Ok(None);
    };
    let visible = utf8_prefix(&visible, visible_budget);
    // Keep normal source identities readable and byte-for-byte stable. Long
    // tokens retain a UTF-8-safe visible prefix, while the digest always
    // commits to the complete primary/join tuple.
    Ok(Some(format!("{visible}{suffix}")))
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.get(..end).unwrap_or_default()
}

/// Evaluate a map guard before its subject and source identity using the
/// complete, source-typed input context.
fn map_matches(map: &CompiledMap, context: &Value) -> Result<bool, (&'static str, String)> {
    let Some(when) = &map.when else {
        return Ok(true);
    };
    match evaluate(when, context)? {
        Value::Bool(matches) => Ok(matches),
        other @ (Value::Null
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => Err((
            codes::EVALUATION_FAILED,
            format!("map guard yielded {other:?}, not a boolean"),
        )),
    }
}

/// Select and evaluate the one child row that supplies a parent property.
fn select_nested_value(
    nested: &CompiledNestedProperty,
    parent: &BTreeMap<String, norn_canonical::JsonValue>,
    property: &PropertyId,
    expression: &Expression,
    target_type: &Type,
) -> Result<Value, (&'static str, String)> {
    select_nested_value_with_budget(
        nested,
        parent,
        property,
        expression,
        target_type,
        EvaluationBudget::default().units(),
    )
}

fn select_nested_value_with_budget(
    nested: &CompiledNestedProperty,
    parent: &BTreeMap<String, norn_canonical::JsonValue>,
    property: &PropertyId,
    expression: &Expression,
    target_type: &Type,
    total_units: u64,
) -> Result<Value, (&'static str, String)> {
    use norn_canonical::JsonValue as Json;

    let rows = match parent.get(&nested.field) {
        None | Some(Json::Null) => &[][..],
        Some(Json::Array(rows)) => rows.as_slice(),
        Some(value) => {
            return Err((
                codes::INVALID_RECORD,
                format!(
                    "nested collection {:?} is not an array: {value:?}",
                    nested.field
                ),
            ));
        }
    };
    let max_rows = usize::try_from(ResourceLimitProfile::norn_limits_v1().slice.max_records)
        .map_err(|_failure| {
            (
                codes::INVALID_RECORD,
                "the configured nested-row limit does not fit the host size".to_owned(),
            )
        })?;
    if rows.len() > max_rows {
        return Err((
            codes::INVALID_RECORD,
            format!(
                "nested collection {:?} has {} rows; the slice limit is {max_rows}",
                nested.field,
                rows.len()
            ),
        ));
    }

    let mut remaining_units = total_units;
    let mut selected_context = None;
    for (index, child) in rows.iter().enumerate() {
        let Json::Object(child) = child else {
            return Err((
                codes::INVALID_RECORD,
                format!(
                    "nested dataset {:?} row {index} is not an object",
                    nested.dataset
                ),
            ));
        };
        let mut input = BTreeMap::new();
        for (field, ty) in &nested.input_types {
            let value = conform(child.get(field), ty).map_err(|reason| {
                (
                    codes::INVALID_RECORD,
                    format!(
                        "nested dataset {:?} row {index} field {field:?} {reason}",
                        nested.dataset
                    ),
                )
            })?;
            input.insert(field.clone(), value);
        }
        let context = Value::object([("input", Value::Object(input))]);
        let predicate = evaluate_with_remaining(&nested.predicate, &context, &mut remaining_units)?;
        let matches = nested_predicate_value(&predicate, property)?;
        if matches {
            if selected_context.is_some() {
                return Err((
                    codes::EVALUATION_FAILED,
                    format!(
                        "nested property {} matched multiple rows in {:?}.{}",
                        property.as_str(),
                        nested.dataset,
                        nested.field
                    ),
                ));
            }
            selected_context = Some(context);
        }
    }

    let Some(context) = selected_context else {
        return if matches!(target_type, Type::Optional(_)) {
            Ok(Value::Null)
        } else {
            Err((
                codes::EVALUATION_FAILED,
                format!(
                    "nested property {} has no matching child row for a non-nullable target",
                    property.as_str()
                ),
            ))
        };
    };
    evaluate_with_remaining(expression, &context, &mut remaining_units)
}

fn nested_predicate_value(
    value: &Value,
    property: &PropertyId,
) -> Result<bool, (&'static str, String)> {
    match value {
        Value::Bool(matches) => Ok(*matches),
        other @ (Value::Null
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => Err((
            codes::EVALUATION_FAILED,
            format!(
                "nested predicate for {} yielded {other:?}, not a boolean",
                property.as_str()
            ),
        )),
    }
}

fn evaluate_with_remaining(
    expression: &Expression,
    context: &Value,
    remaining_units: &mut u64,
) -> Result<Value, (&'static str, String)> {
    let attempt = expression.evaluate_metered(context, EvaluationBudget::new(*remaining_units));
    *remaining_units = (*remaining_units).saturating_sub(attempt.units_used);
    attempt
        .result
        .map(norn_expr::Evaluation::into_value)
        .map_err(|failure| {
            (
                codes::EVALUATION_FAILED,
                format!("{:?}: {failure}", expression.source()),
            )
        })
}

fn evaluate_lifecycle_snapshots(
    map: &CompiledMap,
    context: &Value,
    provenance: &impl Fn(&str) -> InstanceProvenance,
) -> Result<Vec<ObservedLifecycleSnapshot>, (&'static str, String)> {
    let mut lifecycle_snapshots = Vec::new();
    for lifecycle in &map.lifecycles {
        match evaluate(&lifecycle.expression, context)? {
            Value::Null => {}
            Value::String(observed) => {
                let state = if let Some(destinations) = &lifecycle.transition_destinations {
                    destinations.get(&observed).cloned().ok_or_else(|| {
                        (
                            codes::EVALUATION_FAILED,
                            format!(
                                "{observed:?} is not a transition of {}",
                                lifecycle.property.as_str()
                            ),
                        )
                    })?
                } else {
                    observed
                };
                let lifecycle_provenance = provenance(lifecycle.property.as_str());
                lifecycle_snapshots.push(ObservedLifecycleSnapshot {
                    property: lifecycle.property.clone(),
                    state,
                    provenance: lifecycle_provenance,
                });
            }
            other @ (Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
                return Err((
                    codes::EVALUATION_FAILED,
                    format!(
                        "lifecycle rule {} yielded {other:?}",
                        lifecycle.property.as_str()
                    ),
                ));
            }
        }
    }
    Ok(lifecycle_snapshots)
}

/// The rows a map reads from one document: the document itself, or the
/// elements of its nested child collection (a missing collection has none;
/// a non-array one is one malformed row).
fn rows<'d>(
    map: &CompiledMap,
    document: &'d SourceDocument,
) -> Vec<Option<&'d norn_canonical::JsonValue>> {
    match &map.nested_in {
        None if document.dataset == map.dataset => vec![Some(&document.record)],
        Some(nested) if document.dataset == nested.dataset => {
            let norn_canonical::JsonValue::Object(record) = &document.record else {
                return vec![None];
            };
            match record.get(&nested.field) {
                None | Some(norn_canonical::JsonValue::Null) => Vec::new(),
                Some(norn_canonical::JsonValue::Array(items)) => items.iter().map(Some).collect(),
                Some(_) => vec![None],
            }
        }
        _ => Vec::new(),
    }
}

fn evaluate(expression: &Expression, context: &Value) -> Result<Value, (&'static str, String)> {
    expression
        .evaluate(context, EvaluationBudget::default())
        .map(norn_expr::Evaluation::into_value)
        .map_err(|failure| {
            (
                codes::EVALUATION_FAILED,
                format!("{:?}: {failure}", expression.source()),
            )
        })
}

fn text(expression: &Expression, context: &Value) -> Result<String, (&'static str, String)> {
    match evaluate(expression, context)? {
        Value::String(text) if !text.is_empty() => Ok(text),
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => Err((
            codes::INVALID_KEY_VALUE,
            format!(
                "{:?} yielded {other:?}, not a non-empty string",
                expression.source()
            ),
        )),
    }
}

/// Check one raw field value against its `input` type and convert it.
fn conform(raw: Option<&norn_canonical::JsonValue>, ty: &Type) -> Result<Value, String> {
    use norn_canonical::JsonValue as Json;
    match (raw, ty) {
        (None | Some(Json::Null), Type::Optional(_)) => Ok(Value::Null),
        // Frappe returns `""` for an unset `Select` or `Date`.
        (Some(Json::String(empty)), Type::Optional(inner))
            if empty.is_empty() && matches!(**inner, Type::Enum { .. } | Type::Date) =>
        {
            Ok(Value::Null)
        }
        (raw, Type::Optional(inner)) => conform(raw, inner),
        (None | Some(Json::Null), _) => Err("is required".to_owned()),
        (Some(Json::String(text)), Type::Enum { values }) => {
            if values.contains(text) {
                Ok(Value::String(text.clone()))
            } else {
                Err(format!("value {text:?} is not one of {values:?}"))
            }
        }
        (
            Some(Json::String(text)),
            Type::Text { .. } | Type::Date | Type::Timestamp { .. } | Type::Duration,
        ) => Ok(Value::String(text.clone())),
        (Some(Json::Number(number)), Type::Integer) if number.parse::<i64>().is_ok() => {
            number_value(number)
        }
        (Some(Json::Number(number)), Type::Number | Type::Decimal { .. }) => number_value(number),
        (Some(Json::Bool(flag)), Type::Bool) => Ok(Value::Bool(*flag)),
        (Some(value), Type::Json) => any_value(value),
        (Some(value), ty) => Err(format!("{value:?} does not conform to {ty:?}")),
    }
}

fn number_value(number: &str) -> Result<Value, String> {
    number
        .parse::<norn_expr::Number>()
        .map(Value::Number)
        .map_err(|failure| failure.to_string())
}

fn any_value(value: &norn_canonical::JsonValue) -> Result<Value, String> {
    use norn_canonical::JsonValue as Json;
    Ok(match value {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Number(number) => number_value(number)?,
        Json::String(text) => Value::String(text.clone()),
        Json::Array(items) => Value::Array(items.iter().map(any_value).collect::<Result<_, _>>()?),
        Json::Object(members) => Value::Object(
            members
                .iter()
                .map(|(key, member)| any_value(member).map(|member| (key.clone(), member)))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// Encode an evaluated property value for the action input, exactly
/// (#2770, #3014). Nullable properties retain explicit null so an observed
/// removal clears a previously stored value. Numbers keep their decimal digits: an integer property takes
/// an `i64`, a decimal one the exact evaluated decimal.
fn encode(value: &Value, ty: &Type) -> Result<Option<RuntimeValue>, String> {
    match (value, ty) {
        (Value::Null, Type::Optional(_)) => Ok(Some(RuntimeValue::Null)),
        (value, Type::Optional(inner)) => encode(value, inner),
        (
            Value::String(text),
            Type::Text { .. }
            | Type::Date
            | Type::Timestamp { .. }
            | Type::Duration
            | Type::Enum { .. },
        ) => Ok(Some(RuntimeValue::String(text.clone()))),
        (Value::Number(number), Type::Integer) => {
            let integer = number
                .as_str()
                .parse::<i64>()
                .map_err(|failure| format!("{} is not an integer: {failure}", number.as_str()))?;
            Ok(Some(RuntimeValue::Number(Decimal::from(integer))))
        }
        (Value::Number(number), Type::Decimal { .. } | Type::Number) => number
            .as_str()
            .parse::<Decimal>()
            .map(|decimal| Some(RuntimeValue::Number(decimal)))
            .map_err(|failure| format!("{} is not an exact decimal: {failure}", number.as_str())),
        (Value::Bool(flag), Type::Bool) => Ok(Some(RuntimeValue::Bool(*flag))),
        (value, Type::Json) => encode_json(value).map(Some),
        (value, ty) => Err(format!("{value:?} is not a {ty:?}")),
    }
}

fn encode_json(value: &Value) -> Result<RuntimeValue, String> {
    Ok(match value {
        Value::Null => RuntimeValue::Null,
        Value::Bool(flag) => RuntimeValue::Bool(*flag),
        Value::String(text) => RuntimeValue::String(text.clone()),
        Value::Number(number) => RuntimeValue::Number(
            number
                .as_str()
                .parse::<Decimal>()
                .map_err(|error| error.to_string())?,
        ),
        Value::Array(items) => {
            RuntimeValue::Array(items.iter().map(encode_json).collect::<Result<_, _>>()?)
        }
        Value::Object(fields) => RuntimeValue::Object(
            fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), encode_json(value)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

#[cfg(test)]
mod nested_budget_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "focused unit tests"
    )]

    use super::*;

    #[test]
    fn whole_child_input_reads_inherit_every_child_field_and_no_parent_fields() {
        let mut schema: SourceSchemaIr = serde_json::from_str(include_str!(
            "../../tests/fixtures/mapping/toy-variant.source-schema.json"
        ))
        .expect("the Fashion source schema decodes");
        schema
            .fields
            .iter_mut()
            .find(|field| field.dataset == "Item Variant Attribute" && field.name == "parent")
            .expect("the child parent field exists")
            .markings
            .push(MarkingId("privacy.child_parent".to_owned()));
        let reads = expression_reads_union(&["input"], "input");
        assert!(reads.whole_root);
        let child = source_provenance(&reads, &schema, "Item Variant Attribute");
        assert_eq!(
            child.source_fields,
            ["name", "parent", "attribute", "attribute_value"]
        );
        assert_eq!(
            child.markings,
            [MarkingId("privacy.child_parent".to_owned())]
        );
        let parent = source_provenance(&reads, &schema, "Item");
        assert_eq!(
            parent.source_fields,
            ["name", "item_code", "item_name", "variant_of", "attributes"]
        );
        assert!(parent.markings.is_empty());
    }

    #[test]
    fn nested_rows_and_selected_value_share_one_evaluation_budget() {
        use norn_canonical::JsonValue as Json;

        let predicate = Expression::compile("input.attribute == 'Size'")
            .expect("the simple predicate compiles");
        let value = Expression::compile("input.attribute_value").expect("the child value compiles");
        let nested = CompiledNestedProperty {
            dataset: "Variant Attribute".to_owned(),
            field: "attributes".to_owned(),
            input_types: BTreeMap::from([
                ("attribute".to_owned(), Type::Text { format: None }),
                ("attribute_value".to_owned(), Type::Text { format: None }),
            ]),
            predicate,
        };
        let context = Value::object([(
            "input",
            Value::object([
                ("attribute", Value::String("Size".to_owned())),
                ("attribute_value", Value::String("M".to_owned())),
            ]),
        )]);
        let predicate_units = nested
            .predicate
            .evaluate_metered(&context, EvaluationBudget::default())
            .units_used;
        let value_units = value
            .evaluate_metered(&context, EvaluationBudget::default())
            .units_used;
        assert!(predicate_units > 0);
        assert!(value_units > 0);

        let one_row =
            norn_canonical::parse(r#"{"attributes":[{"attribute":"Size","attribute_value":"M"}]}"#)
                .expect("the one-row parent parses");
        let Json::Object(one_row) = one_row else {
            panic!("the parent is an object");
        };
        let property = PropertyId("size".to_owned());
        let combined_budget = predicate_units
            .saturating_add(value_units)
            .saturating_sub(1);
        let failure = select_nested_value_with_budget(
            &nested,
            &one_row,
            &property,
            &value,
            &Type::Text { format: None },
            combined_budget,
        )
        .expect_err("predicate and selected value exceed one shared budget");
        assert_eq!(failure.0, codes::EVALUATION_FAILED);
        assert!(failure.1.contains("budget exhausted"), "{}", failure.1);

        let two_rows = norn_canonical::parse(
            r#"{"attributes":[{"attribute":"Size","attribute_value":"M"},{"attribute":"Size","attribute_value":"L"}]}"#,
        )
        .expect("the two-row parent parses");
        let Json::Object(two_rows) = two_rows else {
            panic!("the parent is an object");
        };
        let failure = select_nested_value_with_budget(
            &nested,
            &two_rows,
            &property,
            &value,
            &Type::Text { format: None },
            predicate_units,
        )
        .expect_err("the second predicate cannot restart the consumed budget");
        assert_eq!(failure.0, codes::EVALUATION_FAILED);
        assert!(failure.1.contains("budget exhausted"), "{}", failure.1);
    }
}
