//! One-way interop exports of lineage (T8-STD-2, #2751; ruling 2026-09-25
//! §26: PROV-O provenance, `OpenLineage` lineage, DCAT technical-view catalog,
//! and SKOS synonyms).
//!
//! Every export reads its source by shared reference and returns new JSON.
//! An export never writes back or becomes an import authority. Every call
//! requires an [`ExportScope`] made from the globally unique, stable identity
//! of the source tenant cell. Scope-qualified identifiers are opaque and
//! deterministic, so equal ledger-local ids in separate cells remain distinct.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use norn_ir::canonical::{CanonicalJsonError, HashProfile, canonical_json_of, domain_hash_hex};
use norn_ir::{JsonValue, LineageObject, LinkId, ObjectTypeDef, TechnicalType, object_stable_id};

use crate::lineage::{
    CausalChain, LineageEdge, LineageEdgeKind, LineageNode, LineageNodeKind, project_views,
};

/// A validated, opaque identity for the tenant cell whose artifacts are
/// being exported.
///
/// The caller must provide a globally unique, stable cell identity. The
/// constructor rejects empty, whitespace-containing, and control-containing ids;
/// it hashes the exact accepted value and does not retain or export it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportScope {
    cell_digest: String,
}

impl ExportScope {
    /// Validate and hash a globally unique stable cell id.
    ///
    /// The value is used exactly as supplied, without trimming or
    /// normalization. Callers are responsible for supplying an identity that
    /// is globally unique and stable for the lifetime of that cell.
    ///
    /// # Errors
    ///
    /// Returns [`InteropError::EmptyCellId`],
    /// [`InteropError::WhitespaceCellId`],
    /// [`InteropError::ControlCharacterInCellId`], or a canonicalization
    /// error.
    pub fn new(cell_id: &str) -> Result<Self, InteropError> {
        if cell_id.is_empty() {
            return Err(InteropError::EmptyCellId);
        }
        if cell_id.chars().any(char::is_whitespace) {
            return Err(InteropError::WhitespaceCellId);
        }
        if cell_id.chars().any(char::is_control) {
            return Err(InteropError::ControlCharacterInCellId);
        }

        let cell_digest =
            domain_hash_hex(b"norn.projections.export-scope.v1\0", cell_id.as_bytes())?;
        Ok(Self { cell_digest })
    }

    /// Return the compact JSON-LD IRI for a stable Norn identifier in this
    /// cell. The cell id and resource id are canonically hashed, so arbitrary
    /// delimiters and URI characters never enter the IRI.
    ///
    /// # Errors
    ///
    /// Returns an error only if canonical JSON encoding fails.
    pub fn resource_id(&self, stable_id: &str) -> Result<String, InteropError> {
        let resource_digest = domain_hash_hex(
            b"norn.projections.resource-id.v1\0",
            &(self.cell_digest.as_bytes(), stable_id.as_bytes()),
        )?;
        Ok(format!("cell:{resource_digest}"))
    }

    fn cell_prefix_iri(&self) -> String {
        format!("urn:norn:cell:{}:", self.cell_digest)
    }

    fn norn_namespace(&self) -> String {
        format!("urn:norn:cell:{}:norn", self.cell_digest)
    }

    fn source_namespace(&self, system: &str) -> Result<String, InteropError> {
        let source_digest = domain_hash_hex(
            b"norn.projections.source-namespace.v1\0",
            &(self.cell_digest.as_bytes(), system.as_bytes()),
        )?;
        Ok(format!(
            "urn:norn:cell:{}:source:{source_digest}",
            self.cell_digest
        ))
    }

    /// Scope a stable ledger id for the `OpenLineage` facet while preserving
    /// its established `commit:` or `decision:` kind prefix.
    fn lineage_facet_id(&self, stable_id: &str) -> String {
        match stable_id.split_once(':') {
            Some((kind, local_id)) => {
                format!("{kind}:{}:{local_id}", self.cell_digest)
            }
            None => format!("norn:{}:{stable_id}", self.cell_digest),
        }
    }
}

/// Errors returned while validating scope or producing an interop export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteropError {
    /// The cell identity was empty.
    EmptyCellId,
    /// The cell identity contained only whitespace.
    WhitespaceCellId,
    /// The cell identity contained a control character.
    ControlCharacterInCellId,
    /// Canonical JSON encoding failed while constructing a stable digest.
    Canonicalization(CanonicalJsonError),
}

impl fmt::Display for InteropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCellId => formatter.write_str("export cell id must not be empty"),
            Self::WhitespaceCellId => {
                formatter.write_str("export cell id must not contain whitespace")
            }
            Self::ControlCharacterInCellId => {
                formatter.write_str("export cell id must not contain control characters")
            }
            Self::Canonicalization(error) => {
                write!(formatter, "canonical export identity: {error}")
            }
        }
    }
}

impl std::error::Error for InteropError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Canonicalization(error) => Some(error),
            Self::EmptyCellId | Self::WhitespaceCellId | Self::ControlCharacterInCellId => None,
        }
    }
}

impl From<CanonicalJsonError> for InteropError {
    fn from(error: CanonicalJsonError) -> Self {
        Self::Canonicalization(error)
    }
}

/// The `producer` URI of every export.
pub const NORN_PRODUCER: &str =
    "https://github.com/Norn-cloud/norn-platform/tree/main/crates/norn-projections";

/// The `OpenLineage` spec version the events conform to.
pub const OPENLINEAGE_SPEC_VERSION: &str = "2-0-2";

/// The `schemaURL` of an exported `RunEvent`.
pub const OPENLINEAGE_RUN_EVENT_SCHEMA_URL: &str =
    "https://openlineage.io/spec/2-0-2/OpenLineage.json#/$defs/RunEvent";

/// The `_schemaURL` of the `nornCommit` run facet (its JSON Schema is
/// `crates/norn-projections/schemas/openlineage-norn-facets.json`).
pub const NORN_COMMIT_FACET_SCHEMA_URL: &str = "https://raw.githubusercontent.com/Norn-cloud/norn-platform/main/crates/norn-projections/schemas/openlineage-norn-facets.json#/$defs/NornCommitRunFacet";

/// The logical Norn namespace key, also used when a source record lacks a
/// system key. Emitted `OpenLineage` namespaces are cell-scoped URNs.
pub const NORN_NAMESPACE: &str = "norn";

/// The PROV-JSONLD context every PROV document references.
pub const PROV_JSONLD_CONTEXT_URL: &str = "https://openprovenance.org/prov-jsonld/context.jsonld";

/// The global IRI the `norn:` prefix expands to for vocabulary terms.
pub const NORN_PREFIX_IRI: &str = "urn:norn:";

fn string(value: impl Into<String>) -> JsonValue {
    JsonValue::String(value.into())
}

fn object<const N: usize>(members: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

/// The node at the end of `edge`.
fn target<'c>(chain: &'c CausalChain, edge: &LineageEdge) -> Option<&'c LineageNode> {
    chain.node(&edge.to)
}

/// The targets of `id`'s edges of kind `kind`, in discovery order.
fn targets<'c>(
    chain: &'c CausalChain,
    id: &'c str,
    kind: LineageEdgeKind,
) -> impl Iterator<Item = &'c LineageNode> + 'c {
    chain
        .causes(id)
        .filter(move |edge| edge.kind == kind)
        .filter_map(move |edge| target(chain, edge))
}

/// A deterministic UUID (version 8, RFC 9562 §5.8) for `stable_id` within
/// `scope`.
///
/// # Errors
///
/// Returns an error only if canonical JSON encoding fails.
pub fn stable_uuid(scope: &ExportScope, stable_id: &str) -> Result<String, InteropError> {
    let input = canonical_json_of(&(
        "norn.openlineage.run.v2",
        scope.cell_digest.as_bytes(),
        stable_id.as_bytes(),
    ))?;
    let digest = HashProfile::Norn1.digest_canonical_bytes(&input);
    let mut out = String::with_capacity(36);
    for (index, byte) in digest.iter().take(16).enumerate() {
        let byte = match index {
            6 => (byte & 0x0f) | 0x80,
            8 => (byte & 0x3f) | 0x80,
            _ => *byte,
        };
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        // Writing to a `String` cannot fail.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// One `OpenLineage` dataset reference.
fn dataset(namespace: &str, name: &str) -> JsonValue {
    object([("namespace", string(namespace)), ("name", string(name))])
}

fn push_unique(datasets: &mut Vec<(String, String)>, namespace: &str, name: &str) {
    if !datasets
        .iter()
        .any(|(seen_namespace, seen_name)| seen_namespace == namespace && seen_name == name)
    {
        datasets.push((namespace.to_owned(), name.to_owned()));
    }
}

/// The source tables observation `observation` was mapped from: mapping →
/// `derived_from` columns → `table`, namespaced by both the source system and
/// the cell scope.
fn source_tables(
    scope: &ExportScope,
    chain: &CausalChain,
    observation: &str,
    datasets: &mut Vec<(String, String)>,
) -> Result<(), InteropError> {
    let system = targets(chain, observation, LineageEdgeKind::ObservedFrom)
        .flat_map(|record| targets(chain, &record.id, LineageEdgeKind::RecordOf))
        .find_map(|source| source.key.clone())
        .unwrap_or_else(|| NORN_NAMESPACE.to_owned());
    let namespace = scope.source_namespace(&system)?;
    let table_link = LineageEdgeKind::Link(LinkId("table".to_owned()));
    for mapping in targets(chain, observation, LineageEdgeKind::MappedBy) {
        for column in targets(chain, &mapping.id, LineageEdgeKind::DerivedFrom) {
            for table in targets(chain, &column.id, table_link.clone()) {
                if table.kind == LineageNodeKind::Technical(TechnicalType::Table)
                    && let Some(key) = &table.key
                {
                    push_unique(datasets, &namespace, key);
                }
            }
        }
    }
    Ok(())
}

fn run_event(
    scope: &ExportScope,
    chain: &CausalChain,
    commit: &LineageNode,
) -> Result<Option<JsonValue>, InteropError> {
    let Some(event_time) = commit.recorded_at.as_ref() else {
        return Ok(None);
    };
    let norn_namespace = scope.norn_namespace();
    let decision = targets(chain, &commit.id, LineageEdgeKind::DecidedBy).next();
    let observations: Vec<&LineageNode> = targets(chain, &commit.id, LineageEdgeKind::Consumed)
        .filter(|node| node.kind == LineageNodeKind::Observation)
        .collect();
    let job = decision
        .and_then(|node| node.key.clone())
        .or_else(|| {
            observations
                .iter()
                .flat_map(|observation| targets(chain, &observation.id, LineageEdgeKind::MappedBy))
                .find_map(|mapping| mapping.key.clone())
        })
        .unwrap_or_else(|| "norn.commit".to_owned());

    let mut inputs = Vec::new();
    for fact in targets(chain, &commit.id, LineageEdgeKind::Read) {
        if let Some(object_type) = &fact.object_type {
            push_unique(&mut inputs, &norn_namespace, object_type.as_str());
        }
    }
    for observation in &observations {
        source_tables(scope, chain, &observation.id, &mut inputs)?;
    }
    let mut outputs = Vec::new();
    for edge in chain
        .edges
        .iter()
        .filter(|edge| edge.kind == LineageEdgeKind::WrittenBy && edge.to == commit.id)
    {
        if let Some(object_type) = chain
            .node(&edge.from)
            .and_then(|fact| fact.object_type.as_ref())
        {
            push_unique(&mut outputs, &norn_namespace, object_type.as_str());
        }
    }

    let mut facet: BTreeMap<String, JsonValue> = BTreeMap::from([
        ("_producer".to_owned(), string(NORN_PRODUCER)),
        (
            "_schemaURL".to_owned(),
            string(NORN_COMMIT_FACET_SCHEMA_URL),
        ),
        (
            "commit".to_owned(),
            string(scope.lineage_facet_id(&commit.id)),
        ),
    ]);
    if let Some(decision) = decision {
        facet.insert(
            "decision".to_owned(),
            string(scope.lineage_facet_id(&decision.id)),
        );
    }
    let datasets = |list: Vec<(String, String)>| {
        JsonValue::Array(
            list.iter()
                .map(|(namespace, name)| dataset(namespace, name))
                .collect(),
        )
    };
    let run_id = stable_uuid(scope, &commit.id)?;
    Ok(Some(object([
        ("eventType", string("COMPLETE")),
        ("eventTime", string(event_time.as_str())),
        ("producer", string(NORN_PRODUCER)),
        ("schemaURL", string(OPENLINEAGE_RUN_EVENT_SCHEMA_URL)),
        (
            "run",
            object([
                ("runId", string(run_id)),
                ("facets", object([("nornCommit", JsonValue::Object(facet))])),
            ]),
        ),
        (
            "job",
            object([("namespace", string(norn_namespace)), ("name", string(job))]),
        ),
        ("inputs", datasets(inputs)),
        ("outputs", datasets(outputs)),
    ])))
}

/// One `OpenLineage` `RunEvent` (`eventType` `COMPLETE`) per commit of
/// `chain`, in the chain's discovery order. Job and Norn dataset namespaces,
/// source namespaces, facet ids, and run UUIDs are all scoped to `scope`.
///
/// # Errors
///
/// Returns an error only if canonical JSON encoding fails while deriving a
/// scope-qualified identity.
pub fn openlineage_run_events(
    scope: &ExportScope,
    chain: &CausalChain,
) -> Result<Vec<JsonValue>, InteropError> {
    let mut events = Vec::new();
    for commit in chain
        .nodes
        .iter()
        .filter(|node| node.kind == LineageNodeKind::Commit)
    {
        if let Some(event) = run_event(scope, chain, commit)? {
            events.push(event);
        }
    }
    Ok(events)
}

/// One stable id as a `cell:` compact IRI.
fn qualified(scope: &ExportScope, id: &str) -> Result<String, InteropError> {
    scope.resource_id(id)
}

fn values(items: &[&str]) -> JsonValue {
    JsonValue::Array(items.iter().map(|item| string(*item)).collect())
}

fn label(text: &str) -> JsonValue {
    JsonValue::Array(vec![object([("@value", string(text))])])
}

fn node_kind_type(kind: LineageNodeKind) -> String {
    match kind {
        LineageNodeKind::Fact => "norn:Fact".to_owned(),
        LineageNodeKind::Commit => "norn:Commit".to_owned(),
        LineageNodeKind::Decision => "norn:Decision".to_owned(),
        LineageNodeKind::Observation => "norn:Observation".to_owned(),
        LineageNodeKind::Trigger => "norn:Trigger".to_owned(),
        LineageNodeKind::SourceRecord => "norn:SourceRecord".to_owned(),
        LineageNodeKind::Technical(technical) => format!("norn:{}", technical.type_name()),
        LineageNodeKind::Object => "norn:Object".to_owned(),
    }
}

fn prov_node(scope: &ExportScope, node: &LineageNode) -> Result<JsonValue, InteropError> {
    let kind_type = node_kind_type(node.kind);
    let mut members: BTreeMap<String, JsonValue> = BTreeMap::from([
        ("@id".to_owned(), string(qualified(scope, &node.id)?)),
        ("label".to_owned(), label(&node.label)),
    ]);
    let statement = match node.kind {
        LineageNodeKind::Commit => {
            if let Some(at) = &node.recorded_at {
                members.insert("startTime".to_owned(), string(at.as_str()));
                members.insert("endTime".to_owned(), string(at.as_str()));
            }
            members.insert("type".to_owned(), values(&[kind_type.as_str()]));
            "Activity"
        }
        LineageNodeKind::Technical(TechnicalType::Owner) => {
            members.insert(
                "type".to_owned(),
                values(&["prov:Person", kind_type.as_str()]),
            );
            "Agent"
        }
        LineageNodeKind::Decision => {
            members.insert(
                "type".to_owned(),
                values(&["prov:Plan", kind_type.as_str()]),
            );
            "Entity"
        }
        LineageNodeKind::Fact
        | LineageNodeKind::Observation
        | LineageNodeKind::Trigger
        | LineageNodeKind::SourceRecord
        | LineageNodeKind::Technical(_)
        | LineageNodeKind::Object => {
            members.insert("type".to_owned(), values(&[kind_type.as_str()]));
            "Entity"
        }
    };
    members.insert("@type".to_owned(), string(statement));
    Ok(JsonValue::Object(members))
}

fn prov_relation(
    scope: &ExportScope,
    chain: &CausalChain,
    edge: &LineageEdge,
) -> Result<JsonValue, InteropError> {
    let from = string(qualified(scope, &edge.from)?);
    let to = string(qualified(scope, &edge.to)?);
    let edge_type = format!("norn:{}", edge.kind.name());
    let statement = match &edge.kind {
        LineageEdgeKind::WrittenBy | LineageEdgeKind::ScheduledBy => {
            let mut members = BTreeMap::from([
                ("@type".to_owned(), string("Generation")),
                ("entity".to_owned(), from),
                ("activity".to_owned(), to),
                ("type".to_owned(), values(&[edge_type.as_str()])),
            ]);
            if let Some(at) = chain
                .node(&edge.to)
                .and_then(|node| node.recorded_at.as_ref())
            {
                members.insert("time".to_owned(), string(at.as_str()));
            }
            JsonValue::Object(members)
        }
        LineageEdgeKind::Read | LineageEdgeKind::Consumed => object([
            ("@type", string("Usage")),
            ("activity", from),
            ("entity", to),
            ("role", values(&[edge_type.as_str()])),
        ]),
        LineageEdgeKind::DecidedBy => object([
            ("@type", string("Association")),
            ("activity", from),
            ("plan", to),
        ]),
        LineageEdgeKind::ObservedFrom | LineageEdgeKind::DerivedFrom => object([
            ("@type", string("Derivation")),
            ("generatedEntity", from),
            ("usedEntity", to),
            ("type", values(&[edge_type.as_str()])),
        ]),
        LineageEdgeKind::Link(_)
            if chain.node(&edge.to).map(|node| node.kind)
                == Some(LineageNodeKind::Technical(TechnicalType::Owner)) =>
        {
            object([
                ("@type", string("Attribution")),
                ("entity", from),
                ("agent", to),
                ("type", values(&[edge_type.as_str()])),
            ])
        }
        LineageEdgeKind::MappedBy
        | LineageEdgeKind::BackedBy
        | LineageEdgeKind::RecordOf
        | LineageEdgeKind::Link(_) => {
            let mut members = BTreeMap::from([
                ("@type".to_owned(), string("Influence")),
                ("influencee".to_owned(), from),
                ("influencer".to_owned(), to),
                ("type".to_owned(), values(&[edge_type.as_str()])),
            ]);
            if let Some(transform) = &edge.detail {
                members.insert("norn:transform".to_owned(), label(transform));
            }
            JsonValue::Object(members)
        }
    };
    Ok(statement)
}

/// The PROV-JSONLD document of `chain`: every node, then every edge, in the
/// chain's discovery order. Resource ids use the cell-scoped `cell:` prefix;
/// PROV and Norn vocabulary terms remain global.
///
/// # Errors
///
/// Returns an error only if canonical JSON encoding fails while deriving a
/// scope-qualified identity.
pub fn prov_document(scope: &ExportScope, chain: &CausalChain) -> Result<JsonValue, InteropError> {
    let mut graph = Vec::new();
    for node in &chain.nodes {
        graph.push(prov_node(scope, node)?);
    }
    for edge in &chain.edges {
        graph.push(prov_relation(scope, chain, edge)?);
    }
    Ok(object([
        (
            "@context",
            JsonValue::Array(vec![
                string(PROV_JSONLD_CONTEXT_URL),
                object([("norn", string(NORN_PREFIX_IRI))]),
                object([("cell", string(scope.cell_prefix_iri()))]),
            ]),
        ),
        ("@graph", JsonValue::Array(graph)),
    ]))
}

/// The namespace IRI of the DCAT vocabulary.
pub const DCAT_IRI: &str = "http://www.w3.org/ns/dcat#";

/// The namespace IRI of the DCMI Metadata Terms.
pub const DCTERMS_IRI: &str = "http://purl.org/dc/terms/";

/// The namespace IRI of the SKOS vocabulary.
pub const SKOS_IRI: &str = "http://www.w3.org/2004/02/skos/core#";

fn reference(scope: &ExportScope, id: &str) -> Result<JsonValue, InteropError> {
    Ok(object([("@id", string(qualified(scope, id)?))]))
}

fn global_reference(id: &str) -> JsonValue {
    object([("@id", string(format!("norn:{id}")))])
}

/// The first target of `object`'s link `link`, as a stable object id.
fn link_target(object: &LineageObject, link: &str) -> Option<String> {
    object
        .links
        .iter()
        .find(|candidate| candidate.link.as_str() == link)
        .map(|found| object_stable_id(&found.target_type, &found.target_key))
}

/// The cell-scoped DCAT 3 technical-view catalog. It contains one `dcat:Dataset`
/// per `technical.Dataset` and `technical.Table` object, sorted by stable id.
///
/// # Errors
///
/// Returns an error only if canonical JSON encoding fails while deriving a
/// scope-qualified identity.
pub fn dcat_catalog(
    scope: &ExportScope,
    objects: &[LineageObject],
) -> Result<JsonValue, InteropError> {
    let mut datasets: BTreeMap<String, JsonValue> = BTreeMap::new();
    for item in objects {
        let kind = TechnicalType::of(&item.object_type);
        if !matches!(kind, Some(TechnicalType::Dataset | TechnicalType::Table)) {
            continue;
        }
        let stable_id = item.stable_id();
        let id = qualified(scope, &stable_id)?;
        let mut members: BTreeMap<String, JsonValue> = BTreeMap::from([
            ("@id".to_owned(), string(id.clone())),
            ("@type".to_owned(), string("dcat:Dataset")),
            ("dcterms:identifier".to_owned(), string(item.key.as_str())),
            ("dcterms:title".to_owned(), string(item.key.as_str())),
            (
                "dcterms:type".to_owned(),
                global_reference(&format!("type:{}", item.object_type.as_str())),
            ),
        ]);
        for (link, member) in [
            ("dataset", "dcterms:isPartOf"),
            ("source", "dcterms:source"),
            ("owner", "dcterms:rightsHolder"),
            ("domain", "dcat:theme"),
        ] {
            if let Some(target) = link_target(item, link) {
                members.insert(member.to_owned(), reference(scope, &target)?);
            }
        }
        datasets.insert(stable_id, JsonValue::Object(members));
    }
    Ok(object([
        (
            "@context",
            object([
                ("dcat", string(DCAT_IRI)),
                ("dcterms", string(DCTERMS_IRI)),
                ("norn", string(NORN_PREFIX_IRI)),
                ("cell", string(scope.cell_prefix_iri())),
            ]),
        ),
        ("@id", string(qualified(scope, "catalog:technical-view")?)),
        ("@type", string("dcat:Catalog")),
        ("dcterms:title", string("Norn technical view")),
        (
            "dcat:dataset",
            JsonValue::Array(datasets.into_values().collect()),
        ),
    ]))
}

/// `SalesOrder` → `Sales Order`.
fn words(local_name: &str) -> String {
    let mut out = String::with_capacity(local_name.len());
    let mut previous_lower = false;
    for character in local_name.chars() {
        if character.is_uppercase() && previous_lower {
            out.push(' ');
        }
        previous_lower = character.is_lowercase() || character.is_ascii_digit();
        out.push(character);
    }
    out
}

/// The cell-scoped SKOS concept scheme of `defs`: one concept per business
/// type with its labels, definition, broader type, and related types.
///
/// # Errors
///
/// Returns an error only if canonical JSON encoding fails while deriving a
/// scope-qualified identity.
pub fn skos_concept_scheme(
    scope: &ExportScope,
    defs: &[ObjectTypeDef],
) -> Result<JsonValue, InteropError> {
    let views = project_views(defs);
    let mut schemes: Vec<String> = Vec::new();
    let mut concepts = Vec::new();
    for business in &views.business.types {
        let name = &business.name;
        let scheme = format!("scheme:{}", name.domain().unwrap_or(name.as_str()));
        if !schemes.contains(&scheme) {
            schemes.push(scheme.clone());
        }
        let local = name.local_name();
        let label = words(local);
        let mut members: BTreeMap<String, JsonValue> = BTreeMap::from([
            (
                "@id".to_owned(),
                string(qualified(scope, &format!("type:{}", name.as_str()))?),
            ),
            ("@type".to_owned(), string("skos:Concept")),
            ("skos:inScheme".to_owned(), reference(scope, &scheme)?),
            ("skos:prefLabel".to_owned(), string(label.as_str())),
            ("skos:notation".to_owned(), string(name.as_str())),
        ]);
        if label != local {
            members.insert("skos:altLabel".to_owned(), string(local));
        }
        if let Some(def) = defs.iter().find(|def| def.name == *name) {
            if let Some(description) = &def.description {
                members.insert("skos:definition".to_owned(), string(description.as_str()));
            }
            if let Some(parent) = &def.extends {
                members.insert(
                    "skos:broader".to_owned(),
                    reference(scope, &format!("type:{}", parent.as_str()))?,
                );
            }
        }
        let mut related: Vec<String> = Vec::new();
        for link in &business.links {
            let target = format!("type:{}", link.target.as_str());
            if !related.contains(&target) {
                related.push(target);
            }
        }
        if !related.is_empty() {
            let mut references = Vec::with_capacity(related.len());
            for target in &related {
                references.push(reference(scope, target)?);
            }
            members.insert("skos:related".to_owned(), JsonValue::Array(references));
        }
        concepts.push(JsonValue::Object(members));
    }
    let mut graph = Vec::new();
    for scheme in &schemes {
        graph.push(object([
            ("@id", string(qualified(scope, scheme)?)),
            ("@type", string("skos:ConceptScheme")),
            (
                "skos:prefLabel",
                string(scheme.trim_start_matches("scheme:")),
            ),
        ]));
    }
    graph.extend(concepts);
    Ok(object([
        (
            "@context",
            object([
                ("skos", string(SKOS_IRI)),
                ("cell", string(scope.cell_prefix_iri())),
            ]),
        ),
        ("@graph", JsonValue::Array(graph)),
    ]))
}
