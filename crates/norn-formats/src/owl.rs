//! The OWL 2 reader: an ontology's RDF graph → one Norn interface per owned
//! class, with a loss report that accounts for every statement.
//!
//! # Input
//!
//! The reader reads canonical N-Triples ([`crate::ntriples`]), not RDF/XML
//! or Turtle. A pack pins its originals file by file in its
//! [`OntologySource`](norn_ir::OntologySource) admission; a pinned normaliser (the hash-locked rdflib
//! of `verification/ontology/tools/requirements.lock`, run by
//! `scripts/importers/owl_sources.py`) parses them and writes the union graph
//! as sorted N-Triples with canonical blank-node labels. The pack pins that
//! graph's SHA-256 too, so the reader refuses any bytes but the expected
//! graph, and the format family is "an RDF graph", whatever the originals'
//! serialization. The parser and normaliser are recorded in the import's
//! provenance.
//!
//! # Output
//!
//! - Layer `owl.vocabulary`: one interface per owned class (a class whose IRI
//!   is in one of the configured namespaces), named
//!   `<prefix as dotted segments>.<LocalName>` (`fibo-be-le-lp:LegalEntity` →
//!   `fibo.be.le.lp.LegalEntity`), with the closed-world reading of
//!   `owl_lower`.
//! - `SourceSchemaIR`: one dataset per owned class, carrying its IRI.
//! - The loss report: one entry per declaration, axiom, annotation predicate,
//!   individual and leftover statement (`owl_model::Item`). The
//!   functional-syntax rendering of an axiom is its stable identity. Nothing
//!   is dropped: everything the reader does not lower is `deferred` with a
//!   registered `format.owl.*` code naming the reason, and its home.

use std::collections::BTreeSet;

use norn_ir::source_schema::{
    DatasetKind, EvidenceRef, SourceAdapter, SourceDataset, SourceIdentity, SourceSchemaFormat,
    SourceSchemaIr,
};
use norn_ir::{ArtifactEnvelope, Diagnostic, DiagnosticSeverity, OntologySource};
use serde_json::json;

use crate::contract::{
    ArtifactLayer, ComponentPin, Disposition, Facet, ImportProvenance, InputPin, LossLedger,
    SchemaImport, SchemaImporter, SourceElement, sha256_hex,
};
use crate::owl_model::{Item, Model, Namespace, PropertyKind};
use crate::type_tables::TypeTable;

/// The adapter name of the OWL reader.
pub const OWL_READER_ADAPTER: &str = "norn.formats.owl";
/// The datatype table.
pub(crate) const DATATYPES: &str = include_str!("../tables/norn.formats.owl.datatypes.json");
/// The largest admitted graph, in bytes.
pub const MAX_GRAPH_BYTES: usize = 256 * 1024 * 1024;
/// The layer name of the lowered interfaces.
pub const VOCABULARY_LAYER: &str = "owl.vocabulary";

/// One owned namespace: an ontology namespace IRI and its preferred prefix
/// (`https://spec.edmcouncil.org/fibo/ontology/BE/LegalEntities/LegalPersons/`
/// and `fibo-be-le-lp`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwlNamespace {
    /// The namespace IRI every owned term starts with.
    pub iri: String,
    /// The prefix its ontology declares.
    pub prefix: String,
}

/// The normaliser that produced the graph, recorded in provenance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwlNormaliser {
    /// The parser (`rdflib`).
    pub name: String,
    /// Its pinned version.
    pub version: String,
}

/// The reader's typed configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct OwlConfig {
    /// Licence, release and per-file pins of the originals.
    pub admission: OntologySource,
    /// The caller-supplied observation time (no clock is read).
    pub observed_at: String,
    /// The SHA-256 of the normalised graph.
    pub graph_sha256: String,
    /// The normaliser that wrote the graph.
    pub normaliser: OwlNormaliser,
    /// The owned namespaces: only their classes and properties are lowered.
    pub namespaces: Vec<OwlNamespace>,
}

/// The normalised graph: its name (for provenance) and bytes.
#[derive(Debug, Clone, Copy)]
pub struct OwlGraph<'a> {
    /// A stable name for the graph (`graph.nt`).
    pub path: &'a str,
    /// Canonical N-Triples.
    pub bytes: &'a [u8],
}

/// The OWL 2 reader.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwlReader;

fn admit(graph: OwlGraph<'_>, config: &OwlConfig) -> Result<(), String> {
    let source = &config.admission;
    if !source.source_id.starts_with("source.")
        || source.release_id.trim().is_empty()
        || source.license.trim().is_empty()
        || !source.canonical_url.starts_with("https://")
        || source.files.as_ref().is_none_or(Vec::is_empty)
    {
        return Err(
            "source requires OntologySource identity, pinned release, licence, HTTPS provenance and per-file pins"
                .into(),
        );
    }
    if graph.bytes.len() > MAX_GRAPH_BYTES {
        return Err(format!("graph exceeds {MAX_GRAPH_BYTES} bytes"));
    }
    if sha256_hex(graph.bytes) != config.graph_sha256 {
        return Err(format!(
            "unadmitted bytes: {} is not the pinned graph",
            graph.path
        ));
    }
    if config.namespaces.is_empty() {
        return Err("no owned namespace".into());
    }
    let mut seen = BTreeSet::new();
    for namespace in &config.namespaces {
        let prefix_ok = namespace
            .prefix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
            && namespace
                .prefix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !namespace.iri.starts_with("https://") && !namespace.iri.starts_with("http://") {
            return Err(format!(
                "namespace {} is not an absolute IRI",
                namespace.iri
            ));
        }
        if !prefix_ok {
            return Err(format!(
                "prefix {:?} is not lowercase letters, digits and '-'",
                namespace.prefix
            ));
        }
        if !seen.insert(namespace.iri.as_str()) || !seen.insert(namespace.prefix.as_str()) {
            return Err(format!(
                "namespace {} or prefix {} repeats",
                namespace.iri, namespace.prefix
            ));
        }
    }
    Ok(())
}

fn model_of(graph: OwlGraph<'_>, config: &OwlConfig) -> Result<Model, String> {
    let triples = crate::ntriples::parse(graph.bytes)?;
    let namespaces = config
        .namespaces
        .iter()
        .map(|namespace| Namespace {
            iri: namespace.iri.clone(),
            prefix: namespace.prefix.clone(),
        })
        .collect();
    Model::build(triples, namespaces)
}

/// The family of the declaration an annotation belongs to.
fn family_of(model: &Model, iri: &str) -> &'static str {
    if model.classes.contains(iri) {
        if model.split(iri).is_some() {
            "owl.class"
        } else {
            "owl.external_class"
        }
    } else if let Some(kind) = model.properties.get(iri) {
        match kind {
            PropertyKind::Object => "owl.object_property",
            PropertyKind::Datatype => "owl.datatype_property",
            PropertyKind::Annotation => "owl.annotation_property",
        }
    } else if model.datatypes.contains(iri) {
        "owl.datatype"
    } else {
        "owl.ontology"
    }
}

/// The loss-report element of an item.
fn element_of(model: &Model, item: &Item) -> SourceElement {
    match item {
        Item::Class(iri) => SourceElement::dataset(iri),
        Item::ExternalClass(iri) => SourceElement::definition("owl.external_class", iri),
        Item::Ontology(iri) => SourceElement::definition("owl.ontology", iri),
        Item::Property(iri, kind) => SourceElement::definition(
            match kind {
                PropertyKind::Object => "owl.object_property",
                PropertyKind::Datatype => "owl.datatype_property",
                PropertyKind::Annotation => "owl.annotation_property",
            },
            iri,
        ),
        Item::Datatype(iri) => SourceElement::definition("owl.datatype", iri),
        Item::Individual(iri, _) => SourceElement::definition("owl.individual", iri),
        Item::Axiom(axiom) => SourceElement::definition("owl.axiom", &model.render_axiom(axiom)),
        Item::Annotation { subject, predicate } => {
            let facet = Facet::Key(model.compact(predicate));
            if model.classes.contains(subject) && model.split(subject).is_some() {
                SourceElement::dataset_facet(subject, facet)
            } else {
                SourceElement::definition_facet(family_of(model, subject), subject, facet)
            }
        }
        Item::AxiomAnnotation(rendering) => {
            SourceElement::definition("owl.axiom_annotation", rendering)
        }
        Item::Statement(rendering) => SourceElement::definition("owl.statement", rendering),
    }
}

fn rejected(detail: &str) -> Disposition {
    Disposition::Rejected {
        code: crate::codes::OWL_INVALID.code.into(),
        detail: detail.into(),
    }
}

impl SchemaImporter for OwlReader {
    type Source<'a> = OwlGraph<'a>;
    type Config = OwlConfig;

    fn adapter(&self) -> SourceAdapter {
        SourceAdapter {
            name: OWL_READER_ADAPTER.into(),
            version: "0.1.0".into(),
        }
    }

    fn inventory(&self, source: Self::Source<'_>, config: &Self::Config) -> Vec<SourceElement> {
        let model = admit(source, config).and_then(|()| model_of(source, config));
        match model {
            Ok(model) => model
                .items
                .iter()
                .map(|item| element_of(&model, item))
                .collect(),
            Err(_) => vec![SourceElement::definition("owl.graph", source.path)],
        }
    }

    fn import(&self, source: Self::Source<'_>, config: &Self::Config) -> SchemaImport {
        let mut ledger = LossLedger::new();
        let mut output = SchemaImport {
            provenance: ImportProvenance {
                adapter: self.adapter(),
                parser: Some(ComponentPin {
                    name: config.normaliser.name.clone(),
                    version: config.normaliser.version.clone(),
                }),
                evaluator: None,
                tables: Vec::new(),
                inputs: vec![InputPin::of(source.path, source.bytes)],
            },
            source_schema: None,
            layers: Vec::new(),
            diagnostics: Vec::new(),
            loss_report: LossLedger::new().finish(),
        };
        let result = admit(source, config)
            .and_then(|()| model_of(source, config))
            .and_then(|model| lower(&model, source, config, &mut output, &mut ledger));
        if let Err(error) = result {
            ledger.declare(SourceElement::definition("owl.graph", source.path));
            output.diagnostics.push(Diagnostic {
                code: crate::codes::OWL_INVALID.code.into(),
                severity: DiagnosticSeverity::Error,
                message: error.clone(),
                artifact_id: None,
                path: None,
                remediation: Some(crate::codes::OWL_INVALID.remediation.clone()),
            });
            output.source_schema = None;
            output.layers.clear();
            ledger.resolve_open(|_| Some(rejected(&error)));
        }
        output.loss_report = ledger.finish();
        output
    }
}

fn lower(
    model: &Model,
    source: OwlGraph<'_>,
    config: &OwlConfig,
    output: &mut SchemaImport,
    ledger: &mut LossLedger,
) -> Result<(), String> {
    for item in &model.items {
        ledger.declare(element_of(model, item));
    }
    let mut table = TypeTable::load(DATATYPES)?;
    output.provenance.tables = vec![table.pin().clone()];
    output.provenance.evaluator = Some(crate::type_tables::evaluator_pin());
    let lowered = crate::owl_lower::lower(model, &mut table)?;

    let mut artifacts = Vec::new();
    let mut datasets = Vec::new();
    for (iri, name) in &lowered.names {
        let Some(interface) = lowered.interfaces.get(iri) else {
            continue;
        };
        let (namespace, local) = name.rsplit_once('.').ok_or("unqualified interface name")?;
        let artifact: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": crate::owl_lower::artifact_id(name),
            "kind": "entity",
            "namespace": namespace,
            "name": local,
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": {},
            "dependencies": [],
            "provenance": { "source": "import", "sourcePath": iri },
            "payload": { "entityVersion": "2.0", "interface": interface },
        }))
        .map_err(|error| format!("{name}: {error}"))?;
        artifacts.push(artifact);
        datasets.push(SourceDataset {
            id: iri.clone(),
            native_name: model
                .labels
                .get(iri)
                .cloned()
                .unwrap_or_else(|| model.compact(iri)),
            namespace: model.split(iri).map(|(namespace, _)| namespace.iri.clone()),
            kind: DatasetKind::Document,
            description: model.definitions.get(iri).cloned(),
        });
    }
    for (index, item) in model.items.iter().enumerate() {
        let disposition = lowered
            .dispositions
            .get(&index)
            .cloned()
            .ok_or_else(|| format!("no disposition for {:?}", element_of(model, item)))?;
        ledger.resolve(&element_of(model, item), disposition);
    }
    let graph_hash = sha256_hex(source.bytes);
    output.source_schema = Some(SourceSchemaIr {
        format: SourceSchemaFormat::V1,
        source: SourceIdentity {
            system: "OWL".into(),
            instance: config.admission.source_id.clone(),
            version: Some(config.admission.release_id.clone()),
        },
        observed_at: config.observed_at.clone(),
        adapter: OwlReader.adapter(),
        raw_metadata: EvidenceRef {
            uri: format!("urn:norn:owl-graph:sha256:{graph_hash}"),
            sha256: graph_hash,
        },
        datasets,
        fields: Vec::new(),
        constraints: Vec::new(),
        profiles: Vec::new(),
        code_sets: Vec::new(),
        lineage: Vec::new(),
        extensions: None,
    });
    output.layers.push(ArtifactLayer {
        name: VOCABULARY_LAYER.into(),
        tenant: None,
        artifacts,
    });
    Ok(())
}
