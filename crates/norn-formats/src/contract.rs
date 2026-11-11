//! The shared schema-importer contract (#2733): one pure trait every schema
//! importer implements, its typed output, and the typed [`LossReport`] that
//! accounts for every source element.
//!
//! The typed spine is `source bytes → SourceSchemaIR → EntityIR`
//! technical-view types. There is no `LinkML` step: `LinkML` has no maintained
//! Rust implementation, and `SourceSchemaIR` plus `EntityIR` already carry the
//! structure an intermediate would.
//!
//! # Contract
//!
//! A [`SchemaImporter`] is a pure function from borrowed source bytes and a
//! typed configuration to a [`SchemaImport`]: no network, file system, clock,
//! or randomness. Any observation time is a configuration input, so two runs
//! on the same input are byte-identical ([`SchemaImport::canonical_json`]).
//!
//! # Loss accounting
//!
//! Every importer first inventories its parsed source into a [`LossLedger`]:
//! every dataset, field, field facet (required, unique, precision, options,
//! default, cardinality, arguments, …), and every other definition or
//! metadata key, *before* lowering anything. Lowering then resolves each
//! element to exactly one [`Disposition`]. An element nobody resolves becomes
//! [`Disposition::Unexplained`], which the conformance harness rejects, so an
//! element cannot fall out of the count.
//!
//! - [`Disposition::Mapped`] / [`Disposition::Extension`]: lowered into a registry
//!   type, tenant subtype, or [`norn_ir::ObjectTypeExtension`] member, optionally
//!   with a [`Degradation`] (a weaker but lossless-in-bytes representation,
//!   such as `json-ext`) and the [`TypeDecision`] table row that chose it.
//! - [`Disposition::Deferred`]: Norn has a home for it ([`NornHome`]), but
//!   this importer does not lower it yet; a registered gap code says why.
//! - [`Disposition::Rejected`]: the source element is invalid; an `error`
//!   diagnostic with the same code was emitted.
//! - [`Disposition::Dropped`]: only for information Norn genuinely has no
//!   home for, with a closed [`DropReason`].

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::source_schema::{SourceAdapter, SourceSchemaIr};
use norn_ir::{ArtifactEnvelope, Diagnostic, ExtensionNamespace, SUPPORTED_IR_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codes;

/// A pure schema importer: source bytes and configuration in, typed output
/// out. Implementations perform no IO and read no clock.
pub trait SchemaImporter {
    /// The borrowed raw source inputs (each one byte slice).
    type Source<'a>: Copy;
    /// The typed, caller-attested configuration.
    type Config;

    /// The adapter identity recorded in `SourceSchemaIR` and provenance.
    fn adapter(&self) -> SourceAdapter;

    /// Inventory raw source before lowering. The conformance harness compares
    /// this independent run with the final report, including non-emitted metadata.
    fn inventory(&self, source: Self::Source<'_>, config: &Self::Config) -> Vec<SourceElement>;

    /// Import `source` under `config`.
    fn import(&self, source: Self::Source<'_>, config: &Self::Config) -> SchemaImport;
}

/// The output of one [`SchemaImporter::import`] run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaImport {
    /// Pins of everything that determined the output.
    pub provenance: ImportProvenance,
    /// The discovered source schema. `None` when input or configuration
    /// validation failed (see `diagnostics`).
    pub source_schema: Option<SourceSchemaIr>,
    /// Artifact layers in dependency order: each layer is checked with every
    /// earlier layer as its base.
    pub layers: Vec<ArtifactLayer>,
    /// Importer findings, in emission order.
    pub diagnostics: Vec<Diagnostic>,
    /// Accounting of every inventoried source element.
    pub loss_report: LossReport,
}

/// One checkable layer of artifacts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactLayer {
    /// Stable layer name (`shopify.admin`, `frappe.entities`).
    pub name: String,
    /// `Some` when the layer is a tenant layer (extensions); `None` for a
    /// registry/source layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<LayerTenant>,
    /// The layer's artifacts, in the importer's documented order.
    pub artifacts: Vec<ArtifactEnvelope>,
}

/// The tenant identity of a tenant layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerTenant {
    /// Every artifact's `scope.tenantId`.
    pub tenant_id: String,
    /// The extension namespace every added member is prefixed with.
    pub namespace: ExtensionNamespace,
}

/// Everything that determined an import's output, pinned together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProvenance {
    /// The importer.
    pub adapter: SourceAdapter,
    /// The source-format parser, when the importer uses one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parser: Option<ComponentPin>,
    /// The decision evaluator, when type tables were evaluated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<ComponentPin>,
    /// The type-table artifacts evaluated, in evaluation-registration order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<TablePin>,
    /// SHA-256 of each raw input, computed by the importer.
    pub inputs: Vec<InputPin>,
}

/// A named, versioned component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentPin {
    /// Component name (`apollo-parser`, `zen-engine`).
    pub name: String,
    /// Exact version or tag.
    pub version: String,
}

/// A type-table artifact, pinned by content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePin {
    /// The rule artifact id.
    pub artifact_id: String,
    /// The artifact version.
    pub version: String,
    /// Lower-case hex SHA-256 of the artifact's bytes as shipped.
    pub sha256: String,
}

/// One raw input, pinned by content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputPin {
    /// Input name (`admin_sdl`, `doctypes`).
    pub name: String,
    /// Lower-case hex SHA-256 of the bytes.
    pub sha256: String,
    /// Length in bytes.
    pub bytes: u64,
}

impl InputPin {
    /// Pin `bytes` under `name`.
    #[must_use]
    pub fn of(name: &str, bytes: &[u8]) -> Self {
        Self {
            name: name.to_owned(),
            sha256: sha256_hex(bytes),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        }
    }
}

/// Lower-case hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible"
        )]
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ----- the loss report -------------------------------------------------------

/// One inventoried source element.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SourceElement {
    /// A dataset (a `SourceSchemaIR` dataset: a `DocType`, a GraphQL object
    /// type, a metaobject definition).
    Dataset {
        /// The `SourceSchemaIR` dataset id.
        dataset: String,
    },
    /// A field of a dataset.
    Field {
        /// The dataset id.
        dataset: String,
        /// The source-native field name.
        field: String,
    },
    /// One facet (constraint, behaviour, or metadata key) of a field.
    Facet {
        /// The dataset id.
        dataset: String,
        /// The source-native field name.
        field: String,
        /// Which facet.
        facet: Facet,
    },
    /// One metadata key of a dataset itself (`autoname`, `module`).
    DatasetFacet {
        /// The dataset id.
        dataset: String,
        /// Which facet.
        facet: Facet,
    },
    /// Any other source definition (a GraphQL enum or interface, a Frappe
    /// Workflow, a Shopify metafield definition).
    Definition {
        /// A stable family token (`graphql.enum`, `frappe.workflow`,
        /// `shopify.metafield_definition`).
        family: String,
        /// The definition's identity within the family.
        name: String,
    },
    /// One facet of a [`SourceElement::Definition`].
    DefinitionFacet {
        /// The definition family.
        family: String,
        /// The definition's identity.
        name: String,
        /// Which facet.
        facet: Facet,
    },
}

impl SourceElement {
    /// A dataset element.
    #[must_use]
    pub fn dataset(dataset: &str) -> Self {
        Self::Dataset {
            dataset: dataset.to_owned(),
        }
    }

    /// A field element.
    #[must_use]
    pub fn field(dataset: &str, field: &str) -> Self {
        Self::Field {
            dataset: dataset.to_owned(),
            field: field.to_owned(),
        }
    }

    /// A field-facet element.
    #[must_use]
    pub fn facet(dataset: &str, field: &str, facet: Facet) -> Self {
        Self::Facet {
            dataset: dataset.to_owned(),
            field: field.to_owned(),
            facet,
        }
    }

    /// A dataset-facet element.
    #[must_use]
    pub fn dataset_facet(dataset: &str, facet: Facet) -> Self {
        Self::DatasetFacet {
            dataset: dataset.to_owned(),
            facet,
        }
    }

    /// A definition element.
    #[must_use]
    pub fn definition(family: &str, name: &str) -> Self {
        Self::Definition {
            family: family.to_owned(),
            name: name.to_owned(),
        }
    }

    /// A definition-facet element.
    #[must_use]
    pub fn definition_facet(family: &str, name: &str, facet: Facet) -> Self {
        Self::DefinitionFacet {
            family: family.to_owned(),
            name: name.to_owned(),
            facet,
        }
    }
}

/// A facet of a field, dataset, or definition: one constraint, behaviour, or
/// metadata key the source declares.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "facet", content = "key", rename_all = "snake_case")]
pub enum Facet {
    /// The value type.
    Type,
    /// Required / non-null (absent and `null` are both "no value").
    Required,
    /// Uniqueness.
    Unique,
    /// Decimal precision or scale.
    Precision,
    /// A unit of measure.
    Unit,
    /// Single vs list cardinality.
    Cardinality,
    /// Declared options: enum values or a reference target.
    Options,
    /// A declared default value.
    Default,
    /// Label or description text.
    Description,
    /// Deprecation status.
    Deprecation,
    /// A request argument of a field (its name).
    Argument(String),
    /// A declared validation rule (its name).
    Validation(String),
    /// Any other metadata key, by its source-native name.
    Key(String),
}

/// What became of one source element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "disposition",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Disposition {
    /// Lowered into an object type: a registry type or a tenant subtype.
    Mapped {
        /// Where it went.
        target: Target,
        /// A weaker representation, when one was chosen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        degraded: Option<Degradation>,
        /// The type-table row that chose the representation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<TypeDecision>,
    },
    /// Lowered into an `ObjectTypeExtension` of a tenant layer.
    Extension {
        /// Where it went.
        target: Target,
        /// A weaker representation, when one was chosen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        degraded: Option<Degradation>,
        /// The type-table row that chose the representation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<TypeDecision>,
    },
    /// Norn has a home for it, but this importer does not lower it.
    Deferred {
        /// The Norn IR that would hold it.
        home: NornHome,
        /// A registered importer code naming the gap.
        code: String,
        /// Human-readable detail.
        detail: String,
    },
    /// The source element is invalid; an `error` diagnostic with `code` was
    /// emitted.
    Rejected {
        /// The registered diagnostic code.
        code: String,
        /// Human-readable detail.
        detail: String,
    },
    /// Norn has no home for this information.
    Dropped {
        /// Why there is no home.
        reason: DropReason,
        /// Human-readable detail.
        detail: String,
    },
    /// Nobody resolved the element: an importer bug the harness rejects.
    Unexplained,
}

/// Where a lowered element went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    /// The artifact id.
    pub artifact_id: String,
    /// The property, link, or key name within it; `None` for the artifact
    /// itself (a dataset, or a facet recorded in its provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

impl Target {
    /// The artifact itself.
    #[must_use]
    pub fn artifact(artifact_id: &str) -> Self {
        Self {
            artifact_id: artifact_id.to_owned(),
            member: None,
        }
    }

    /// One member of the artifact.
    #[must_use]
    pub fn member(artifact_id: &str, member: &str) -> Self {
        Self {
            artifact_id: artifact_id.to_owned(),
            member: Some(member.to_owned()),
        }
    }
}

/// A weaker representation chosen for a mapped element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Degradation {
    /// A registered importer code.
    pub code: String,
    /// Human-readable detail.
    pub detail: String,
}

/// The decision-table row that chose a representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDecision {
    /// The rule artifact id of the table.
    pub table: String,
    /// The matched row's `_id`.
    pub row: String,
}

/// The Norn IR that would hold a deferred element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NornHome {
    /// `EntityIR` (a constraint, status, or member feature).
    EntityIr,
    /// `AccessIR` (roles, permissions).
    AccessIr,
    /// `RuleIR` (validations, conditional behaviour, guards).
    RuleIr,
    /// `WidgetIR` (layout, presentation; see `frappe_ui`).
    WidgetIr,
    /// `MappingIR` (source-record identity and value transforms).
    MappingIr,
    /// `ActionIR` (write operations).
    ActionIr,
}

/// Why Norn has no home for a dropped element. Closed: the conformance
/// harness rejects any other value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// Bookkeeping of the source's own metadata store (row ids, row order,
    /// `doctype: DocField`, modification stamps); not a fact about the data.
    SourceBookkeeping,
    /// Transport framing of the source API (pagination connections, edges,
    /// cursors, page info, response envelopes).
    TransportFraming,
    /// A per-request query parameter of the source API (sort keys, filters,
    /// truncation); it selects or shapes a response, it is not data.
    RequestParameter,
    /// Introspection or API-plumbing metadata of the source schema itself
    /// (root operation types, the `Node` interface's fetch-by-id contract).
    SchemaPlumbing,
    /// Credential material (a Frappe `Password` field). Secret values never
    /// enter the ontology; the secrets store owns them.
    SecretMaterial,
}

impl DropReason {
    /// Every allowed drop reason.
    pub const ALL: [Self; 5] = [
        Self::SourceBookkeeping,
        Self::TransportFraming,
        Self::RequestParameter,
        Self::SchemaPlumbing,
        Self::SecretMaterial,
    ];
}

/// One accounted element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LossEntry {
    /// The element.
    pub element: SourceElement,
    /// What became of it.
    #[serde(flatten)]
    pub disposition: Disposition,
}

/// The accounting of every inventoried source element, sorted by element
/// (unique). Built only through a [`LossLedger`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LossReport {
    /// One entry per element, in element order.
    pub entries: Vec<LossEntry>,
}

/// Counts per disposition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LossSummary {
    /// Inventoried elements.
    pub total: usize,
    /// Mapped without degradation.
    pub mapped: usize,
    /// Lowered into an extension without degradation.
    pub extension: usize,
    /// Mapped or extension with a degradation.
    pub degraded: usize,
    /// Deferred to another Norn IR.
    pub deferred: usize,
    /// Invalid in the source.
    pub rejected: usize,
    /// No Norn home.
    pub dropped: usize,
    /// Unresolved (must be zero).
    pub unexplained: usize,
}

impl LossReport {
    /// Counts per disposition.
    #[must_use]
    pub fn summary(&self) -> LossSummary {
        let mut summary = LossSummary {
            total: self.entries.len(),
            ..LossSummary::default()
        };
        for entry in &self.entries {
            let slot = match &entry.disposition {
                Disposition::Mapped { degraded: None, .. } => &mut summary.mapped,
                Disposition::Extension { degraded: None, .. } => &mut summary.extension,
                Disposition::Mapped {
                    degraded: Some(_), ..
                }
                | Disposition::Extension {
                    degraded: Some(_), ..
                } => &mut summary.degraded,
                Disposition::Deferred { .. } => &mut summary.deferred,
                Disposition::Rejected { .. } => &mut summary.rejected,
                Disposition::Dropped { .. } => &mut summary.dropped,
                Disposition::Unexplained => &mut summary.unexplained,
            };
            *slot = slot.saturating_add(1);
        }
        summary
    }

    /// The entry for `element`.
    #[must_use]
    pub fn entry(&self, element: &SourceElement) -> Option<&LossEntry> {
        self.entries
            .binary_search_by(|entry| entry.element.cmp(element))
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    /// Find missing or unexpected entries against an independently collected
    /// raw-source inventory, rather than a reduced `SourceSchemaIR`.
    #[must_use]
    pub fn validate_inventory(&self, inventory: &[SourceElement]) -> Vec<String> {
        let expected: BTreeSet<_> = inventory.iter().collect();
        let actual: BTreeSet<_> = self.entries.iter().map(|entry| &entry.element).collect();
        expected
            .difference(&actual)
            .map(|element| format!("raw source {element:?} has no loss entry"))
            .chain(
                actual
                    .difference(&expected)
                    .map(|element| format!("loss entry {element:?} is outside raw inventory")),
            )
            .collect()
    }

    /// Structural findings: elements without a resolution, unregistered
    /// codes, targets that name no emitted artifact or member, extension
    /// dispositions that do not point at an extension (and vice versa), and
    /// `SourceSchemaIR` datasets or fields that have no entry.
    #[must_use]
    pub fn validate(
        &self,
        layers: &[ArtifactLayer],
        source_schema: Option<&SourceSchemaIr>,
    ) -> Vec<String> {
        let mut findings = Vec::new();
        let registered: BTreeSet<&str> = codes::ALL.iter().map(|code| code.code).collect();
        let members = emitted_members(layers);

        for pair in self.entries.windows(2) {
            if let [left, right] = pair
                && left.element >= right.element
            {
                findings.push(format!(
                    "entries are not strictly sorted at {:?}",
                    right.element
                ));
            }
        }
        for entry in &self.entries {
            let element = &entry.element;
            match &entry.disposition {
                Disposition::Unexplained => {
                    findings.push(format!("unexplained element {element:?}"));
                }
                Disposition::Mapped {
                    target,
                    degraded,
                    decision,
                }
                | Disposition::Extension {
                    target,
                    degraded,
                    decision,
                } => {
                    let extension = matches!(entry.disposition, Disposition::Extension { .. });
                    match members.get(&target.artifact_id) {
                        None => findings.push(format!(
                            "{element:?} targets unknown artifact {:?}",
                            target.artifact_id
                        )),
                        Some(emitted) => {
                            if emitted.extension != extension {
                                findings.push(format!(
                                    "{element:?} disposition does not match artifact {:?} (extension: {})",
                                    target.artifact_id, emitted.extension
                                ));
                            }
                            if let Some(member) = &target.member
                                && !emitted.members.contains(member)
                            {
                                findings.push(format!(
                                    "{element:?} targets unknown member {member:?} of {:?}",
                                    target.artifact_id
                                ));
                            }
                        }
                    }
                    if let Some(degraded) = degraded {
                        check_code(&registered, &degraded.code, element, &mut findings);
                    }
                    if let Some(decision) = decision
                        && (decision.table.is_empty() || decision.row.is_empty())
                    {
                        findings.push(format!("{element:?} has an incomplete type decision"));
                    }
                }
                Disposition::Deferred { code, detail, .. }
                | Disposition::Rejected { code, detail } => {
                    check_code(&registered, code, element, &mut findings);
                    if detail.trim().is_empty() {
                        findings.push(format!("{element:?} has no detail"));
                    }
                }
                Disposition::Dropped { reason, detail } => {
                    if !DropReason::ALL.contains(reason) {
                        findings.push(format!("{element:?} drop reason {reason:?} not allowed"));
                    }
                    if detail.trim().is_empty() {
                        findings.push(format!("{element:?} has no detail"));
                    }
                }
            }
        }
        if let Some(schema) = source_schema {
            for dataset in &schema.datasets {
                let element = SourceElement::dataset(&dataset.id);
                if self.entry(&element).is_none() {
                    findings.push(format!("source schema {element:?} has no loss entry"));
                }
            }
            for field in &schema.fields {
                let element = SourceElement::field(&field.dataset, &field.name);
                if self.entry(&element).is_none() {
                    findings.push(format!("source schema {element:?} has no loss entry"));
                }
            }
        }
        findings
    }
}

fn check_code(
    registered: &BTreeSet<&str>,
    code: &str,
    element: &SourceElement,
    findings: &mut Vec<String>,
) {
    if !registered.contains(code) {
        findings.push(format!("{element:?} uses unregistered code {code:?}"));
    }
}

struct EmittedArtifact {
    extension: bool,
    members: BTreeSet<String>,
}

/// Every emitted artifact id with its member names: property and link ids,
/// key names, and owned-collection ids.
fn emitted_members(layers: &[ArtifactLayer]) -> BTreeMap<String, EmittedArtifact> {
    let mut out = BTreeMap::new();
    for artifact in layers.iter().flat_map(|layer| &layer.artifacts) {
        let Ok(value) = serde_json::to_value(artifact) else {
            continue;
        };
        let payload = value.get("payload");
        let extension_definition = payload.and_then(|payload| payload.get("objectTypeExtension"));
        let extension = extension_definition.is_some();
        let definition =
            extension_definition.or_else(|| payload.and_then(|payload| payload.get("objectType")));
        let list = |name: &str| {
            definition
                .and_then(|definition| definition.get(name))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        };
        let mut members = BTreeSet::new();
        for name in ["properties", "links", "ownedCollections"] {
            for member in list(name) {
                if let Some(id) = member.get("id").and_then(Value::as_str) {
                    members.insert(id.to_owned());
                }
            }
        }
        for key in list("identity") {
            if let Some(name) = key.get("name").and_then(Value::as_str) {
                members.insert(format!("key:{name}"));
            }
        }
        out.insert(
            artifact.artifact_id.clone(),
            EmittedArtifact { extension, members },
        );
    }
    out
}

/// Builds a [`LossReport`]: inventory first ([`Self::declare`]), then resolve
/// each element once ([`Self::resolve`]).
#[derive(Debug, Clone, Default)]
pub struct LossLedger {
    entries: BTreeMap<SourceElement, Option<Disposition>>,
    late: Vec<SourceElement>,
    conflicts: Vec<SourceElement>,
}

impl LossLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inventory `element` (idempotent).
    pub fn declare(&mut self, element: SourceElement) {
        self.entries.entry(element).or_insert(None);
    }

    /// Whether `element` was inventoried.
    #[must_use]
    pub fn contains(&self, element: &SourceElement) -> bool {
        self.entries.contains_key(element)
    }

    /// Whether `element` is inventoried and still unresolved.
    #[must_use]
    pub fn is_open(&self, element: &SourceElement) -> bool {
        matches!(self.entries.get(element), Some(None))
    }

    /// Resolve `element`. The first resolution wins; resolving an element
    /// that was not inventoried, or resolving one twice with a different
    /// disposition, is recorded and fails [`Self::finish`]'s report.
    pub fn resolve(&mut self, element: &SourceElement, disposition: Disposition) {
        match self.entries.get_mut(element) {
            None => self.late.push(element.clone()),
            Some(slot @ None) => *slot = Some(disposition),
            Some(Some(existing)) => {
                if *existing != disposition {
                    self.conflicts.push(element.clone());
                }
            }
        }
    }

    /// [`Self::resolve`] if `element` was inventoried; otherwise nothing (for
    /// facets the source may or may not declare).
    pub fn resolve_declared(&mut self, element: &SourceElement, disposition: Disposition) {
        if self.entries.contains_key(element) {
            self.resolve(element, disposition);
        }
    }

    /// Resolve every still-open element for which `decide` returns `Some`.
    pub fn resolve_open(&mut self, mut decide: impl FnMut(&SourceElement) -> Option<Disposition>) {
        for (element, slot) in &mut self.entries {
            if slot.is_none() {
                *slot = decide(element);
            }
        }
    }

    /// The report. An element resolved without being inventoried, or
    /// resolved twice differently, becomes [`Disposition::Unexplained`].
    #[must_use]
    pub fn finish(mut self) -> LossReport {
        for element in self.late.drain(..).chain(self.conflicts.drain(..)) {
            self.entries.insert(element, None);
        }
        LossReport {
            entries: self
                .entries
                .into_iter()
                .map(|(element, disposition)| LossEntry {
                    element,
                    disposition: disposition.unwrap_or(Disposition::Unexplained),
                })
                .collect(),
        }
    }
}

// ----- canonical output -------------------------------------------------------

impl SchemaImport {
    /// The canonical JSON form: every object's keys sorted, arrays in output
    /// order (never re-sorted), pretty-printed with a trailing newline. Two
    /// runs on the same input produce identical bytes.
    ///
    /// # Errors
    ///
    /// When an artifact does not serialize.
    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(&sorted_keys(serde_json::to_value(self)?))?;
        text.push('\n');
        Ok(text)
    }

    /// The `SourceSchemaIR` as an artifact envelope (`<prefix>.source_schema`).
    ///
    /// # Errors
    ///
    /// When the schema does not form a valid envelope.
    pub fn source_schema_envelope(&self, prefix: &str) -> Result<Option<ArtifactEnvelope>, String> {
        self.source_schema
            .as_ref()
            .map(|schema| source_schema_envelope(schema, prefix, &self.provenance.adapter.version))
            .transpose()
    }
}

/// Wrap a `SourceSchemaIR` as a `source_schema` artifact envelope.
///
/// # Errors
///
/// When the schema does not form a valid envelope.
pub fn source_schema_envelope(
    schema: &SourceSchemaIr,
    prefix: &str,
    version: &str,
) -> Result<ArtifactEnvelope, String> {
    let payload = serde_json::to_value(schema).map_err(|error| error.to_string())?;
    let envelope = serde_json::json!({
        "irVersion": SUPPORTED_IR_VERSION,
        "artifactId": format!("{prefix}.source_schema"),
        "kind": "source_schema",
        "namespace": prefix,
        "name": "source_schema",
        "version": version,
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "import", "sourcePath": format!("{prefix}.source_schema") },
        "payload": payload,
    });
    serde_json::from_value(envelope).map_err(|error| error.to_string())
}

/// Rebuild every object with its keys in sorted order (independent of
/// `serde_json`'s `preserve_order` feature).
#[must_use]
pub fn sorted_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sorted_keys(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted_keys).collect()),
        other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => other,
    }
}
