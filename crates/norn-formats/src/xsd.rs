//! Pure, offline XML Schema ingestion. XML syntax is owned by quick-xml;
//! schema particles and facets remain source evidence until explicitly lowered.
use crate::contract::{
    ArtifactLayer, ComponentPin, Disposition, ImportProvenance, InputPin, LossLedger, NornHome,
    SchemaImport, SchemaImporter, SourceElement, Target, sha256_hex,
};
use norn_ir::source_schema::{
    DatasetKind, EvidenceRef, NormalizedType, Origin, SourceAdapter, SourceDataset, SourceField,
    SourceIdentity, SourceSchemaFormat, SourceSchemaIr, TypeCandidate,
};
use norn_ir::{ArtifactEnvelope, Diagnostic, DiagnosticSeverity, OntologySource};
use quick_xml::{Reader, events::Event};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const XS: &str = "http://www.w3.org/2001/XMLSchema";
/// The quick-xml version recorded as this importer's parser in every import's provenance; a
/// test holds it equal to the crate's exact `quick-xml = "=…"` requirement.
const QUICK_XML_VERSION: &str = "0.42.0";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
/// Largest admitted schema file.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Largest admitted bundle of schema files.
pub const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;
/// Deepest admitted element nesting.
pub const MAX_DEPTH: usize = 256;
/// Most elements admitted in one file.
pub const MAX_NODES: usize = 250_000;
/// Most namespace declarations on one element.
pub const MAX_DECLARATIONS_PER_ELEMENT: usize = 64;
/// Most namespace bindings in scope at once.
pub const MAX_IN_SCOPE: usize = 128;
/// Most elements in one file that declare namespaces (each opens a scope).
pub const MAX_SCOPES: usize = 4096;
/// Most bytes of namespace prefixes and URIs declared in one file.
pub const MAX_BINDING_BYTES: usize = 1024 * 1024;
/// Longest admitted namespace URI (a declaration or a `targetNamespace`).
/// Every expanded name copies its URI, so this bounds each copy.
pub const MAX_URI_BYTES: usize = 512;
/// Most bytes of expanded element and attribute names built for one file.
pub const MAX_EXPANDED_BYTES: usize = 64 * 1024 * 1024;

/// The namespace bindings in scope at an element: the element's own
/// declarations, linked to the scope it inherits. Nothing is copied: an
/// element that declares nothing shares its parent's scope, and one that does
/// stores only its own declarations. Memory is bounded by the declared bytes
/// (`MAX_BINDING_BYTES`), not by nodes times bindings.
#[derive(Debug, Default)]
pub struct Scope {
    own: Vec<(Rc<str>, Rc<str>)>,
    parent: Option<Rc<Scope>>,
    /// How many declarations the chain holds here, shadowed ones included.
    bound: usize,
}

impl Scope {
    /// The URI bound to `prefix` (`""` is the default namespace), searching
    /// this element's declarations, then each enclosing scope.
    #[must_use]
    pub fn lookup(&self, prefix: &str) -> Option<&str> {
        let mut scope = Some(self);
        while let Some(current) = scope {
            if let Some((_, uri)) = current.own.iter().rev().find(|(p, _)| &**p == prefix) {
                return Some(uri);
            }
            scope = current.parent.as_deref();
        }
        None
    }
}

/// The scope every document starts with: only the reserved `xml` prefix.
fn root_scope() -> Rc<Scope> {
    Rc::new(Scope {
        own: vec![(Rc::from("xml"), Rc::from(XML_NS))],
        parent: None,
        bound: 1,
    })
}

/// One caller-supplied schema; imports are never fetched implicitly.
#[derive(Clone, Copy)]
pub struct XsdFile<'a> {
    /// Identity relative to the admitted source's file set.
    pub path: &'a str,
    /// Original, unmodified XML bytes.
    pub bytes: &'a [u8],
}
/// Source declarations and observation are explicit inputs, never read from a
/// clock. Output is a proposal: `norn-checker` applies the canonical
/// `OntologySource` policy before the source schema can be admitted.
pub struct XsdConfig {
    /// Licence, immutable release and file hashes submitted for checker admission.
    pub admission: OntologySource,
    /// RFC3339 timestamp supplied by the caller.
    pub observed_at: String,
}
/// The offline XSD importer.
pub struct XsdReader;

/// A syntax node. Attribute values retain their original lexical precision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XsdNode {
    /// Globally unique identity within the supplied file set.
    pub id: String,
    /// Expanded element name, in Clark notation.
    pub name: String,
    /// Expanded attribute names and decoded lexical values.
    pub attributes: BTreeMap<String, String>,
    /// The namespace bindings this element itself declares (`""` is the
    /// default namespace). The in-scope set is the chain through `parent`.
    pub namespaces: BTreeMap<String, String>,
    /// The bindings in scope here, shared with the enclosing elements.
    #[serde(skip)]
    pub scope: Rc<Scope>,
    /// Parent node identity; absence denotes the document element.
    pub parent: Option<String>,
    /// Original text content, including documentation.
    pub text: String,
}
impl XsdNode {
    pub(crate) fn attr(&self, key: &str) -> &str {
        self.attributes.get(key).map_or("", String::as_str)
    }
    pub(crate) fn is(&self, local: &str) -> bool {
        self.name == format!("{{{XS}}}{local}")
    }
    pub(crate) fn qname(&self, value: &str) -> Result<String, String> {
        let value = value.trim_matches([' ', '\t', '\r', '\n']);
        if value.starts_with(':') {
            return Err(format!("invalid QName {value}"));
        }
        let (prefix, local) = value.split_once(':').unwrap_or(("", value));
        if local.is_empty() || local.contains(':') {
            return Err(format!("invalid QName {value}"));
        }
        let ns = self.scope.lookup(prefix);
        if !prefix.is_empty() && ns.is_none_or(str::is_empty) {
            return Err(format!("unbound QName {value}"));
        }
        Ok(format!("{{{}}}{local}", ns.unwrap_or("")))
    }
}
/// The Clark name of an element or attribute. `budget` is the file's
/// running total of expanded bytes, charged before the string is built.
fn expanded(
    name: &str,
    scope: &Scope,
    attribute: bool,
    budget: &mut usize,
) -> Result<String, String> {
    let (prefix, local) = name.split_once(':').unwrap_or(("", name));
    let uri = if attribute && prefix.is_empty() {
        None
    } else {
        let uri = scope.lookup(prefix);
        if !prefix.is_empty() && uri.is_none_or(str::is_empty) {
            return Err(format!("unbound prefix {prefix}"));
        }
        Some(uri.unwrap_or(""))
    };
    *budget = budget
        .saturating_add(name.len())
        .saturating_add(uri.map_or(0, str::len));
    if *budget > MAX_EXPANDED_BYTES {
        return Err("XML expanded-name limit".into());
    }
    Ok(uri.map_or_else(|| name.to_owned(), |uri| format!("{{{uri}}}{local}")))
}
/// Running totals that bound one file's namespace declarations.
#[derive(Default)]
struct Bindings {
    scopes: usize,
    bytes: usize,
    expanded: usize,
}

fn parse_node(
    start: &quick_xml::events::BytesStart<'_>,
    nodes: &[XsdNode],
    stack: &[usize],
    path: &str,
    bindings: &mut Bindings,
) -> Result<XsdNode, String> {
    let parent = stack.last().and_then(|i| nodes.get(*i));
    let mut attrs = Vec::new();
    let mut declarations = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| e.to_string())?;
        // quick-xml 0.42 stores names and values as UTF-8 text: no decoding step.
        let key = attribute.key.as_ref().to_owned();
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| e.to_string())?
            .into_owned();
        let prefix = if key == "xmlns" {
            Some(String::new())
        } else {
            key.strip_prefix("xmlns:").map(str::to_owned)
        };
        let Some(prefix) = prefix else {
            attrs.push((key, value));
            continue;
        };
        // Reserved-namespace rules govern namespace declarations only; an
        // ordinary attribute such as `xs:import/@namespace` may name them.
        if (prefix == "xml" && value != XML_NS)
            || prefix == "xmlns"
            || (prefix != "xml" && value == XML_NS)
            || value == "http://www.w3.org/2000/xmlns/"
        {
            return Err("invalid reserved namespace binding".into());
        }
        bindings.bytes = bindings
            .bytes
            .saturating_add(prefix.len())
            .saturating_add(value.len());
        let too_long = value.len() > MAX_URI_BYTES;
        declarations.push((prefix, value));
        if too_long
            || declarations.len() > MAX_DECLARATIONS_PER_ELEMENT
            || bindings.bytes > MAX_BINDING_BYTES
        {
            return Err("namespace binding limit".into());
        }
    }
    let inherited = parent.map_or_else(root_scope, |n| Rc::clone(&n.scope));
    let scope = if declarations.is_empty() {
        inherited
    } else {
        bindings.scopes = bindings.scopes.saturating_add(1);
        if bindings.scopes > MAX_SCOPES {
            return Err("namespace binding limit".into());
        }
        // Redeclarations count too: the chain a lookup walks then holds at
        // most `MAX_IN_SCOPE` entries, however deeply prefixes are shadowed.
        let bound = inherited.bound.saturating_add(declarations.len());
        if bound > MAX_IN_SCOPE {
            return Err("namespace binding limit".into());
        }
        Rc::new(Scope {
            own: declarations
                .iter()
                .map(|(prefix, uri)| (Rc::from(prefix.as_str()), Rc::from(uri.as_str())))
                .collect(),
            parent: Some(inherited),
            bound,
        })
    };
    let name = expanded(start.name().as_ref(), &scope, false, &mut bindings.expanded)?;
    let mut attributes = BTreeMap::new();
    for (key, value) in attrs {
        let key = expanded(&key, &scope, true, &mut bindings.expanded)?;
        if attributes.insert(key, value).is_some() {
            return Err("duplicate expanded attribute".into());
        }
    }
    let index = nodes.len();
    Ok(XsdNode {
        id: format!("{path}#{index}"),
        name,
        attributes,
        namespaces: declarations.into_iter().collect(),
        scope,
        parent: parent.map(|n| n.id.clone()),
        text: String::new(),
    })
}

fn parse(file: XsdFile<'_>) -> Result<Vec<XsdNode>, String> {
    if file.bytes.len() > MAX_FILE_BYTES {
        return Err("schema exceeds 16 MiB".into());
    }
    std::str::from_utf8(file.bytes).map_err(|error| error.to_string())?;
    let mut reader = Reader::from_reader(file.bytes);
    let mut nodes: Vec<XsdNode> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut root_seen = false;
    let mut bindings = Bindings::default();
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                if stack.is_empty() {
                    if root_seen {
                        return Err("multiple XML roots".into());
                    }
                    root_seen = true;
                }
                if stack.len() >= MAX_DEPTH {
                    return Err("XML depth limit".into());
                }
                if nodes.len() >= MAX_NODES {
                    return Err("XML node limit".into());
                }
                let index = nodes.len();
                let node = parse_node(&start, &nodes, &stack, file.path, &mut bindings)?;
                nodes.push(node);
                if !empty {
                    stack.push(index);
                }
            }
            Event::End(_) => {
                stack.pop().ok_or("unexpected close")?;
            }
            Event::Text(text) => {
                let value = text.xml_content(quick_xml::XmlVersion::Implicit1_0);
                if let Some(node) = stack.last().and_then(|i| nodes.get_mut(*i)) {
                    node.text.push_str(&value);
                } else if !value.trim().is_empty() {
                    return Err("text outside schema".into());
                }
            }
            Event::CData(text) => {
                if let Some(node) = stack.last().and_then(|i| nodes.get_mut(*i)) {
                    // The section's text as written, as 0.41's `decode()` returned it.
                    node.text.push_str(&text);
                }
            }
            Event::Decl(decl) => {
                if decl.version().map_err(|e| e.to_string())? != "1.0" {
                    return Err("only XML 1.0 is supported".into());
                }
            }
            Event::DocType(_) => return Err("DTD is unsupported".into()),
            Event::GeneralRef(reference) => {
                let name: &str = reference.as_ref();
                let encoded = format!("&{name};");
                let decoded = quick_xml::escape::unescape(&encoded).map_err(|e| e.to_string())?;
                if let Some(node) = stack.last().and_then(|i| nodes.get_mut(*i)) {
                    node.text.push_str(&decoded);
                } else {
                    return Err("reference outside schema".into());
                }
            }
            Event::Eof => break,
            Event::Comment(_) | Event::PI(_) => {}
        }
    }
    if !stack.is_empty() || !nodes.first().is_some_and(|n| n.is("schema")) {
        return Err("not an XML Schema document".into());
    }
    Ok(nodes)
}
pub(crate) fn semantic_nodes(nodes: &[XsdNode]) -> Vec<&XsdNode> {
    let mut opaque = std::collections::BTreeSet::new();
    nodes
        .iter()
        .filter(|node| {
            let skip = node.is("annotation")
                || !node.name.starts_with(&format!("{{{XS}}}"))
                || node
                    .parent
                    .as_deref()
                    .is_some_and(|parent| opaque.contains(parent));
            if skip {
                opaque.insert(node.id.as_str());
            }
            !skip
        })
        .collect()
}

/// The parse of each file, in input order; computed once per import.
type Parsed = Vec<Result<Vec<XsdNode>, String>>;

/// Every source element, inventoried before lowering. Unadmitted bytes are
/// never parsed: their inventory is the file alone.
fn inventory_of(files: &[XsdFile<'_>], parsed: Option<&Parsed>) -> Vec<SourceElement> {
    let mut elements = Vec::new();
    for (i, file) in files.iter().enumerate() {
        elements.push(SourceElement::definition("xsd.file", file.path));
        let Some(Ok(nodes)) = parsed.and_then(|p| p.get(i)) else {
            continue;
        };
        for node in nodes {
            elements.push(SourceElement::definition("xsd.node", &node.id));
            if !node.text.is_empty() {
                elements.push(SourceElement::definition("xsd.text", &node.id));
            }
            for prefix in node.namespaces.keys() {
                elements.push(SourceElement::definition(
                    "xsd.namespace",
                    &format!("{}@{prefix}", node.id),
                ));
            }
            for key in node.attributes.keys() {
                elements.push(SourceElement::definition(
                    "xsd.attribute",
                    &format!("{}@{key}", node.id),
                ));
            }
        }
        let mut datasets = Vec::new();
        let mut names = BTreeSet::new();
        let mut repeating = BTreeSet::new();
        if discover_file(nodes, &mut datasets, &mut names, &mut repeating).is_ok() {
            for (id, _, fields) in datasets {
                elements.push(SourceElement::dataset(&id));
                for (field, _) in fields {
                    elements.push(SourceElement::field(&id, &field.name));
                }
            }
        }
    }
    elements
}
fn rejected(detail: &str) -> Disposition {
    Disposition::Rejected {
        code: crate::codes::XSD_INVALID.code.into(),
        detail: detail.into(),
    }
}
fn deferred(detail: &str) -> Disposition {
    Disposition::Deferred {
        home: NornHome::EntityIr,
        code: crate::codes::XSD_DEFERRED.code.into(),
        detail: detail.into(),
    }
}
impl SchemaImporter for XsdReader {
    type Source<'a> = &'a [XsdFile<'a>];
    type Config = XsdConfig;
    fn adapter(&self) -> SourceAdapter {
        SourceAdapter {
            name: norn_ir::source_schema::XSD_READER_ADAPTER.into(),
            version: "0.1.0".into(),
        }
    }
    fn inventory(&self, source: Self::Source<'_>, config: &Self::Config) -> Vec<SourceElement> {
        let parsed = admit(source, config)
            .is_ok()
            .then(|| source.iter().map(|file| parse(*file)).collect::<Parsed>());
        inventory_of(source, parsed.as_ref())
    }
    fn import(&self, files: Self::Source<'_>, config: &Self::Config) -> SchemaImport {
        let mut ledger = LossLedger::new();
        let mut output = SchemaImport {
            provenance: ImportProvenance {
                adapter: self.adapter(),
                parser: Some(ComponentPin {
                    name: "quick-xml".into(),
                    version: QUICK_XML_VERSION.into(),
                }),
                evaluator: None,
                tables: Vec::new(),
                inputs: files
                    .iter()
                    .map(|file| InputPin::of(file.path, file.bytes))
                    .collect(),
            },
            source_schema: None,
            layers: Vec::new(),
            diagnostics: Vec::new(),
            loss_report: LossLedger::new().finish(),
        };
        // Pins and sizes first: unadmitted bytes never reach the XML parser.
        let result = if let Err(error) = admit(files, config) {
            for element in inventory_of(files, None) {
                ledger.declare(element);
            }
            Err(error)
        } else {
            let parsed: Parsed = files.iter().map(|file| parse(*file)).collect();
            for element in inventory_of(files, Some(&parsed)) {
                ledger.declare(element);
            }
            lower(files, config, parsed, &mut output, &mut ledger)
        };
        match result {
            Ok(()) => ledger.resolve_open(|_| {
                Some(deferred(
                    "Preserved in source graph; not an executable XML constraint or reversible mapping",
                ))
            }),
            Err(error) => {
                output.diagnostics.push(Diagnostic {
                    code: crate::codes::XSD_INVALID.code.into(),
                    severity: DiagnosticSeverity::Error,
                    message: error.clone(),
                    artifact_id: None,
                    path: None,
                    remediation: Some(crate::codes::XSD_INVALID.remediation.clone()),
                });
                output.source_schema = None;
                output.layers.clear();
                ledger.resolve_open(|_| Some(rejected(&error)));
            }
        }
        output.loss_report = ledger.finish();
        output
    }
}
struct Lowered {
    datasets: Vec<SourceDataset>,
    fields: Vec<SourceField>,
    artifacts: Vec<ArtifactEnvelope>,
}
fn lower(
    files: &[XsdFile<'_>],
    config: &XsdConfig,
    parsed: Parsed,
    output: &mut SchemaImport,
    ledger: &mut LossLedger,
) -> Result<(), String> {
    let (nodes, datasets, repeating) = discover(parsed)?;
    let profile = crate::xsd_profile::describe(&nodes)?;
    let documents = files
        .iter()
        .map(|file| {
            std::str::from_utf8(file.bytes)
                .map(|xml| json!({"path":file.path,"xml":xml}))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bundle =
        norn_ir::canonical::canonical_json_of(&documents).map_err(|error| error.to_string())?;
    let bundle_hash = sha256_hex(&bundle);
    let mut scalar_table = crate::type_tables::TypeTable::load(crate::xsd_projection::SCALARS)?;
    output.provenance.tables = vec![scalar_table.pin().clone()];
    output.provenance.evaluator = Some(crate::type_tables::evaluator_pin());
    let lowered = lower_datasets(datasets, &nodes, &mut scalar_table, ledger)?;
    let mut slots = serde_json::Map::new();
    slots.insert(
        norn_ir::source_schema::XSD_READER_EXTENSION.to_owned(),
        json!({
            "profile": profile, "nodes": nodes, "admission": config.admission,
            "documents": documents, "repeatingFields": repeating,
        }),
    );
    let extensions = serde_json::from_value(serde_json::Value::Object(slots))
        .map_err(|error| error.to_string())?;
    output.source_schema = Some(SourceSchemaIr {
        format: SourceSchemaFormat::V1,
        source: SourceIdentity {
            system: "XSD".into(),
            instance: config.admission.source_id.clone(),
            version: Some(config.admission.release_id.clone()),
        },
        observed_at: config.observed_at.clone(),
        adapter: XsdReader.adapter(),
        raw_metadata: EvidenceRef {
            uri: format!("urn:norn:xsd-bundle:sha256:{bundle_hash}"),
            sha256: bundle_hash,
        },
        datasets: lowered.datasets,
        fields: lowered.fields,
        constraints: Vec::new(),
        profiles: Vec::new(),
        code_sets: Vec::new(),
        lineage: Vec::new(),
        extensions: Some(extensions),
    });
    output.layers.push(ArtifactLayer {
        name: "xsd.technical".into(),
        tenant: None,
        artifacts: lowered.artifacts,
    });
    Ok(())
}
fn lower_datasets(
    datasets: Vec<DiscoveredDataset>,
    nodes: &[XsdNode],
    scalar_table: &mut crate::type_tables::TypeTable,
    ledger: &mut LossLedger,
) -> Result<Lowered, String> {
    let mut lowered = Lowered {
        datasets: Vec::new(),
        fields: Vec::new(),
        artifacts: Vec::new(),
    };
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    for (index, (id, name, fields)) in datasets.into_iter().enumerate() {
        let mut properties = Vec::new();
        let mut decisions = Vec::new();
        for (i, (field, node)) in fields.iter().enumerate() {
            let node = by_id.get(node.as_str()).ok_or("missing field node")?;
            let (property, decision) =
                crate::xsd_projection::property(i, field, node, &by_id, scalar_table)?;
            properties.push(property);
            decisions.push(decision);
        }
        let artifact_id = format!("xsd.Type{index}.entity");
        let artifact:ArtifactEnvelope=serde_json::from_value(json!({"irVersion":"0.1","artifactId":artifact_id,"kind":"entity","namespace":"xsd","name":format!("Type{index}"),"version":"0.1.0","lifecycle":"draft","scope":{},"dependencies":[],"provenance":{"source":"import","sourcePath":id},"payload":{"entityVersion":"2.0","objectType":{"name":format!("xsd.Type{index}"),"description":name,"properties":properties},"extensions":{"norn.formats.xsd/0.1.0":{"sourceDataset":id}}}})).map_err(|error|error.to_string())?;
        ledger.resolve(
            &SourceElement::dataset(&id),
            Disposition::Mapped {
                target: Target::artifact(&artifact_id),
                degraded: None,
                decision: None,
            },
        );
        for (i, ((field, _), decision)) in fields.iter().zip(decisions).enumerate() {
            ledger.resolve(
                &SourceElement::field(&id, &field.name),
                Disposition::Mapped {
                    target: Target::member(&artifact_id, &format!("f{i}")),
                    degraded: Some(crate::contract::Degradation {
                        code: crate::codes::XSD_DEFERRED.code.into(),
                        detail:
                            "Native particles, facets, nil and presence are preserved, not enforced"
                                .into(),
                    }),
                    decision: Some(decision),
                },
            );
        }
        lowered.artifacts.push(artifact);
        lowered.datasets.push(SourceDataset {
            id,
            native_name: name,
            namespace: None,
            kind: DatasetKind::Document,
            description: None,
        });
        lowered
            .fields
            .extend(fields.into_iter().map(|(field, _)| field));
    }
    Ok(lowered)
}

pub(crate) type DiscoveredDataset = (String, String, Vec<(SourceField, String)>);

/// Every file's nodes, its datasets, and the field names that can repeat.
type Discovery = (Vec<XsdNode>, Vec<DiscoveredDataset>, BTreeSet<String>);

/// Datasets and fields across every file, with the fields that can repeat.
fn discover(parsed: Parsed) -> Result<Discovery, String> {
    let mut datasets: Vec<DiscoveredDataset> = Vec::new();
    let mut names = BTreeSet::new();
    let mut repeating = BTreeSet::new();
    let files = parsed.into_iter().collect::<Result<Vec<_>, _>>()?;
    for nodes in &files {
        discover_file(nodes, &mut datasets, &mut names, &mut repeating)?;
    }
    let all_nodes: Vec<XsdNode> = files.into_iter().flatten().collect();
    crate::xsd_projection::resolve_fields(&all_nodes, &mut datasets, &repeating)?;
    Ok((all_nodes, datasets, repeating))
}

/// One file's complex types as datasets and their elements and attributes as
/// fields; records each field that can occur more than once.
fn discover_file(
    nodes: &[XsdNode],
    datasets: &mut Vec<DiscoveredDataset>,
    names: &mut BTreeSet<String>,
    repeating: &mut BTreeSet<String>,
) -> Result<(), String> {
    let node_index: BTreeMap<&str, &XsdNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let root = nodes.first().ok_or("empty schema")?;
    let ns = root.attr("targetNamespace");
    if ns.len() > MAX_URI_BYTES {
        return Err("namespace binding limit".into());
    }
    let mut owners: BTreeMap<&str, Option<usize>> = BTreeMap::new();
    for node in semantic_nodes(nodes) {
        for key in ["ref", "type", "base", "itemType", "substitutionGroup"] {
            if node.name.starts_with(&format!("{{{XS}}}")) && !node.attr(key).is_empty() {
                node.qname(node.attr(key))?;
            }
        }
        let inherited = node
            .parent
            .as_deref()
            .and_then(|p| owners.get(p))
            .copied()
            .flatten();
        let owner = if node.is("complexType") {
            if node.attr("name").is_empty() {
                None
            } else {
                let name = format!("{{{ns}}}{}", node.attr("name"));
                if !names.insert(name.clone()) {
                    return Err(format!("duplicate type {name}"));
                }
                let index = datasets.len();
                datasets.push((name.clone(), name, Vec::new()));
                Some(index)
            }
        } else {
            inherited
        };
        owners.insert(&node.id, owner);
        if !node.is("element") && !node.is("attribute") {
            continue;
        }
        let Some(index) = inherited else {
            continue;
        };
        let Some((name, _, fields)) = datasets.get_mut(index) else {
            continue;
        };
        let field_name = if child_name(node).is_empty() {
            return Err("unnamed field".into());
        } else if node.attr("ref").is_empty() {
            let qualified = node.attr("form") == "qualified"
                || (node.attr("form").is_empty()
                    && root.attr(if node.is("attribute") {
                        "attributeFormDefault"
                    } else {
                        "elementFormDefault"
                    }) == "qualified");
            format!(
                "{{{}}}{}",
                if qualified { ns } else { "" },
                node.attr("name")
            )
        } else {
            node.qname(node.attr("ref"))?
        };
        let field_name = format!("{field_name}@{}", node.id);
        if repeats(node, &node_index)? {
            repeating.insert(field_name.clone());
        }
        let field = SourceField {
            dataset: name.clone(),
            name: field_name,
            ordinal: norn_ir::WholeNumber(
                u64::try_from(fields.len()).map_err(|error| error.to_string())?,
            ),
            native_type: if node.attr("type").is_empty() {
                "xsd:unresolved".into()
            } else {
                node.qname(node.attr("type"))?
            },
            type_candidate: TypeCandidate {
                normalized: NormalizedType::Json,
                origin: Origin::Inferred,
            },
            required: Some(required(node, &node_index)?),
            markings: Vec::new(),
            description: Some(format!(
                "Source node {}; presence, nil, order and occurrence bounds remain distinct in the source graph",
                node.id
            )),
        };
        fields.push((field, node.id.clone()));
    }
    Ok(())
}
fn admit(files: &[XsdFile<'_>], config: &XsdConfig) -> Result<(), String> {
    let source = &config.admission;
    if files.is_empty()
        || !source.source_id.starts_with("source.")
        || source.release_id.trim().is_empty()
        || source.license.trim().is_empty()
        || !source.canonical_url.starts_with("https://")
    {
        return Err(
            "source requires OntologySource identity, pinned release, licence and HTTPS provenance"
                .into(),
        );
    }
    let mut total = 0_usize;
    for file in files {
        if file.bytes.len() > MAX_FILE_BYTES {
            return Err(format!("schema exceeds 16 MiB: {}", file.path));
        }
        total = total.saturating_add(file.bytes.len());
    }
    if total > MAX_BUNDLE_BYTES {
        return Err("schema bundle exceeds 64 MiB".into());
    }
    let mut paths = BTreeSet::new();
    for file in files {
        if !paths.insert(file.path) {
            return Err("duplicate input path".into());
        }
        if !source.files.as_ref().is_some_and(|pins| {
            pins.iter()
                .any(|p| p.path == file.path && p.sha256 == sha256_hex(file.bytes))
        }) {
            return Err(format!("unadmitted bytes: {}", file.path));
        }
    }
    Ok(())
}

fn child_name(node: &XsdNode) -> &str {
    if node.attr("ref").is_empty() {
        node.attr("name")
    } else {
        node.attr("ref")
    }
}

/// Whether every valid instance carries a non-null value: an attribute with
/// `use="required"`, or a non-nillable element whose own and enclosing
/// particles all have `minOccurs` ≥ 1 and that sits in no `choice`.
fn required(node: &XsdNode, index: &BTreeMap<&str, &XsdNode>) -> Result<bool, String> {
    if node.is("attribute") {
        return Ok(node.attr("use") == "required");
    }
    // `required` means non-null: a nillable element may carry a nil value.
    if matches!(node.attr("nillable").trim(), "true" | "1") {
        return Ok(false);
    }
    let mut current = Some(node);
    while let Some(particle) = current {
        if particle.is("complexType") {
            return Ok(true);
        }
        if particle.is("choice") || crate::xsd_profile::occurrences(particle)?.0 == 0 {
            return Ok(false);
        }
        current = particle
            .parent
            .as_deref()
            .and_then(|id| index.get(id).copied());
    }
    Ok(true)
}

/// Whether the field can occur more than once in one record: its own or an
/// enclosing particle allows more than one occurrence.
fn repeats(node: &XsdNode, index: &BTreeMap<&str, &XsdNode>) -> Result<bool, String> {
    if node.is("attribute") {
        return Ok(false);
    }
    let mut current = Some(node);
    while let Some(particle) = current {
        if particle.is("complexType") {
            return Ok(false);
        }
        if crate::xsd_profile::occurrences(particle)?.1 != Some(1) {
            return Ok(true);
        }
        current = particle
            .parent
            .as_deref()
            .and_then(|id| index.get(id).copied());
    }
    Ok(false)
}

/// The fields of a reader-produced source schema that can repeat, by
/// `(dataset, field)`; empty for schemas this reader did not produce.
#[must_use]
pub fn repeating_fields(schema: &SourceSchemaIr) -> BTreeSet<(String, String)> {
    let names: BTreeSet<&str> = schema
        .extensions
        .as_ref()
        .and_then(|slots| slots.get(norn_ir::source_schema::XSD_READER_EXTENSION))
        .and_then(norn_ir::JsonValue::as_object)
        .and_then(|slot| slot.get("repeatingFields"))
        .and_then(norn_ir::JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|name| match name {
            norn_ir::JsonValue::String(name) => Some(name.as_str()),
            norn_ir::JsonValue::Null
            | norn_ir::JsonValue::Bool(_)
            | norn_ir::JsonValue::Number(_)
            | norn_ir::JsonValue::Array(_)
            | norn_ir::JsonValue::Object(_) => None,
        })
        .collect();
    schema
        .fields
        .iter()
        .filter(|field| names.contains(field.name.as_str()))
        .map(|field| (field.dataset.clone(), field.name.clone()))
        .collect()
}

/// Pins how XML text and attribute values decode, across quick-xml upgrades (#1266): the same
/// block passes on 0.41 and 0.42.
#[cfg(test)]
mod decoding_tests {
    use super::{XsdFile, XsdNode, parse};

    fn nodes(xml: &str) -> Vec<XsdNode> {
        parse(XsdFile {
            path: "https://example.test/decoding.xsd",
            bytes: xml.as_bytes(),
        })
        .expect("parses")
    }

    fn find<'a>(nodes: &'a [XsdNode], local: &str) -> &'a XsdNode {
        nodes
            .iter()
            .find(|node| node.is(local))
            .expect("node present")
    }

    const OPEN: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">"#;

    #[test]
    fn attribute_values_resolve_references_and_normalize_whitespace() {
        // Entity and character references resolve; a literal tab or newline becomes a
        // space (attribute-value normalization), a referenced tab stays a tab.
        let xml = format!(
            "{OPEN}<xs:simpleType name=\"S\"><xs:restriction base=\"xs:string\">\
             <xs:enumeration value=\"a&amp;b&lt;c&#x41;&#9;d\te\nf\r\ng\"/>\
             </xs:restriction></xs:simpleType></xs:schema>"
        );
        assert_eq!(
            find(&nodes(&xml), "enumeration").attr("value"),
            "a&b<cA\td e f g"
        );
    }

    /// CDATA keeps `\r\n` as written: behaviour inherited from quick-xml 0.41 and kept by this
    /// port. It diverges from XML 1.0 §2.11, which normalizes line ends in the whole document
    /// before parsing (follow-up: #1285).
    #[test]
    fn text_resolves_references_normalizes_line_ends_and_keeps_the_inherited_raw_cdata() {
        let xml = format!(
            "{OPEN}<xs:annotation><xs:documentation>x &amp; y &#x42;&lt;\r\nline\
             <![CDATA[ <raw>&amp;\r\n ]]>z</xs:documentation></xs:annotation></xs:schema>"
        );
        assert_eq!(
            find(&nodes(&xml), "documentation").text,
            "x & y B<\nline <raw>&amp;\r\n z"
        );
    }

    /// Every import's provenance names the quick-xml that parsed it: the pin equals the
    /// crate's exact requirement, so a version bump cannot leave the provenance behind.
    #[test]
    fn the_provenance_parser_pin_is_the_required_quick_xml() {
        let manifest = include_str!("../Cargo.toml");
        let required: Vec<&str> = manifest
            .lines()
            .filter_map(|line| line.trim().strip_prefix("quick-xml = \"="))
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert_eq!(required, [super::QUICK_XML_VERSION]);
    }

    #[test]
    fn only_xml_1_0_declarations_are_admitted() {
        let body = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"/>";
        assert!(
            parse(XsdFile {
                path: "a.xsd",
                bytes: format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{body}").as_bytes(),
            })
            .is_ok()
        );
        let refused = parse(XsdFile {
            path: "a.xsd",
            bytes: format!("<?xml version=\"1.1\"?>{body}").as_bytes(),
        })
        .expect_err("XML 1.1 is refused");
        assert_eq!(refused, "only XML 1.0 is supported");
    }
}
