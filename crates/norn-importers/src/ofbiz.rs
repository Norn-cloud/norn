//! Apache OFBiz entity-engine XML: entity models, entity groups and seed data
//! to `SourceSchemaIR`, with a loss report for every source element.
//!
//! Pure: source bytes and their pins in, typed output out; no IO, no clock.
//! The output is discovery evidence, not ontology. Datasets, fields, keys,
//! references and observed code sets go into `SourceSchemaIR`; what it has no
//! slot for (inverse relations, non-unique indexes, entity groups, status
//! lifecycles, file headers) goes into the `norn.importers.ofbiz/0.1.0`
//! extension. Lowering into `EntityIR` is a later mapping step, so every
//! lowered element is recorded as `format.source_schema_only`.
//!
//! OFBiz does not declare which status type or enumeration type a status or
//! enumeration field takes: its relations to `StatusItem` and `Enumeration`
//! carry no type, and a relation title names a role, not a type. Those
//! bindings are typed gaps that keep the relation, its title and its target.
//! Status types become lifecycle candidates (states and valid changes) of
//! their own, bound to no field.
//!
//! Anything the reader does not know (an element, an attribute where the
//! grammar is closed) stays unexplained, and the import refuses.

use std::collections::{BTreeMap, BTreeSet};

use norn_formats::codes;
use norn_formats::contract::{
    ComponentPin, Disposition, DropReason, Facet, ImportProvenance, InputPin, LossLedger,
    NornHome, SchemaImport, SchemaImporter, SourceElement, sha256_hex,
};
use norn_ir::source_schema::{
    CodeSet, CodeValue, ConstraintKind, ConstraintTarget, DatasetKind, EvidenceRef, LineageKind,
    NormalizedType, Origin, SourceAdapter, SourceConstraint, SourceDataset, SourceField,
    SourceIdentity, SourceLineage, SourceSchemaFormat, SourceSchemaIr, TypeCandidate,
};
use norn_ir::{Diagnostic, DiagnosticSeverity, WholeNumber};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::xml::{self, Element};

/// The adapter name recorded in `SourceSchemaIR`.
pub const ADAPTER: &str = "norn.importers.ofbiz-entity";
/// The adapter version.
pub const VERSION: &str = "0.1.0";
/// The extension slot for what `SourceSchemaIR` has no place for.
pub const EXTENSION: &str = "norn.importers.ofbiz/0.1.0";

const ROOT_ATTRIBUTE: &str = "entity_xml.root_attribute";
const HEADER: &str = "entity_xml.header";
const EXTEND: &str = "entity_xml.extend_entity";
const GROUP: &str = "entity_xml.entity_group";
const SEED: &str = "entity_xml.seed_row";
const UNKNOWN: &str = "entity_xml.unknown";

const HEADERS: &[&str] = &["title", "description", "copyright", "version", "default-resource-name"];
const SCHEMA_LOCATION: &[&str] = &["xmlns:xsi", "xsi:noNamespaceSchemaLocation"];
/// Entity attributes that only steer storage or caching.
const STORAGE: &[&str] = &["table-name", "never-cache", "sequence-bank-size", "no-auto-stamp"];
/// Entity attributes kept in provenance.
const KEPT: &[&str] = &["title", "dependent-on", "default-resource-name"];
/// The closed grammar of `relation`, `key-map`, `index` and `prim-key`.
const RELATION_ATTRIBUTES: &[&str] = &["type", "rel-entity-name", "title", "fk-name"];
const KEY_MAP_ATTRIBUTES: &[&str] = &["field-name", "rel-field-name"];
const INDEX_ATTRIBUTES: &[&str] = &["name", "unique"];
const STATUS: &str = "StatusItem";
const ENUMERATION: &str = "Enumeration";

/// What a pinned file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// An `entitymodel` file.
    Entity,
    /// An `entitygroup` file.
    Group,
    /// An `entity-engine-xml` seed-data file.
    Seed,
    /// The licence text.
    Licence,
    /// The notice text.
    Notice,
}

impl Role {
    const fn root(self) -> Option<&'static str> {
        match self {
            Self::Entity => Some("entitymodel"),
            Self::Group => Some("entitygroup"),
            Self::Seed => Some("entity-engine-xml"),
            Self::Licence | Self::Notice => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Group => "group",
            Self::Seed => "seed",
            Self::Licence => "licence",
            Self::Notice => "notice",
        }
    }
}

/// One pinned source file.
#[derive(Debug, Clone, Copy)]
pub struct OfbizFile<'a> {
    /// Path within the source repository.
    pub path: &'a str,
    /// What the file is.
    pub role: Role,
    /// The file's bytes.
    pub bytes: &'a [u8],
}

/// Where the files come from, and their pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfbizConfig {
    /// The source system's name.
    pub system: String,
    /// The repository (`apache/ofbiz-framework`).
    pub repository: String,
    /// The pinned commit.
    pub revision: String,
    /// The commit time, RFC 3339 with an offset.
    pub observed_at: String,
    /// The licence's SPDX id.
    pub licence: String,
    /// Path to the lower-case hex SHA-256 every file must have.
    pub pins: BTreeMap<String, String>,
}

/// The reader.
#[derive(Debug, Clone, Copy, Default)]
pub struct OfbizReader;

fn deferred(home: NornHome, code: &norn_formats::codes::Code, detail: impl Into<String>) -> Disposition {
    Disposition::Deferred {
        home,
        code: code.code.to_owned(),
        detail: detail.into(),
    }
}

fn only(detail: impl Into<String>) -> Disposition {
    deferred(NornHome::EntityIr, &codes::SOURCE_SCHEMA_ONLY, detail)
}

fn unresolved(detail: impl Into<String>) -> Disposition {
    deferred(NornHome::EntityIr, &codes::REFERENCE_UNRESOLVED, detail)
}

fn dropped(reason: DropReason, detail: impl Into<String>) -> Disposition {
    Disposition::Dropped {
        reason,
        detail: detail.into(),
    }
}

fn key(text: impl Into<String>) -> Facet {
    Facet::Key(text.into())
}

fn diagnostic(code: &norn_formats::codes::Code, message: impl Into<String>, path: Option<String>) -> Diagnostic {
    Diagnostic {
        code: code.code.to_owned(),
        severity: code.severity.clone(),
        message: message.into(),
        artifact_id: None,
        path,
        remediation: Some(code.remediation.clone()),
    }
}

// ----- inventory: the structure of the files, independent of meaning ---------

fn relation_key(path: &str, top: usize, index: usize) -> String {
    format!("relation:{path}#{top}.{index}")
}

/// Every element of an `entity` or `extend-entity`, attributed to `dataset`.
fn entity_inventory(path: &str, top: usize, element: &Element, dataset: &str, out: &mut Vec<SourceElement>) {
    let extend = element.name == "extend-entity";
    for (name, _) in &element.attributes {
        if name == "entity-name" {
            continue;
        }
        out.push(if extend {
            SourceElement::definition(EXTEND, &format!("{path}#{top}@{name}"))
        } else {
            SourceElement::dataset_facet(dataset, key(format!("attr:{name}")))
        });
    }
    let mut relations = 0_usize;
    for (index, child) in element.children.iter().enumerate() {
        match child.name.as_str() {
            "description" if !extend => out.push(SourceElement::dataset_facet(dataset, Facet::Description)),
            "field" => field_inventory(child, dataset, out),
            "prim-key" => out.push(SourceElement::dataset_facet(
                dataset,
                key(format!("prim-key:{}", child.get("field").unwrap_or_default())),
            )),
            "relation" => {
                let name = relation_key(path, top, relations);
                relations = relations.saturating_add(1);
                out.push(SourceElement::dataset_facet(dataset, key(name.clone())));
                closed(child, RELATION_ATTRIBUTES, &name, dataset, out);
                for part in &child.children {
                    match part.name.as_str() {
                        "key-map" => closed(part, KEY_MAP_ATTRIBUTES, &name, dataset, out),
                        "description" => {}
                        other => out.push(SourceElement::dataset_facet(dataset, key(format!("unknown:{name}/{other}")))),
                    }
                }
            }
            "index" => {
                let name = format!("index:{}", child.get("name").unwrap_or_default());
                out.push(SourceElement::dataset_facet(dataset, key(name.clone())));
                closed(child, INDEX_ATTRIBUTES, &name, dataset, out);
                for part in &child.children {
                    if part.name != "index-field" {
                        out.push(SourceElement::dataset_facet(dataset, key(format!("unknown:{name}/{}", part.name))));
                    }
                }
            }
            other => out.push(SourceElement::dataset_facet(
                dataset,
                key(format!("unknown:{other}@{path}#{top}.{index}")),
            )),
        }
    }
}

/// An unknown attribute of a closed-grammar element is its own element.
fn closed(element: &Element, allowed: &[&str], owner: &str, dataset: &str, out: &mut Vec<SourceElement>) {
    for (name, _) in &element.attributes {
        if !allowed.contains(&name.as_str()) {
            out.push(SourceElement::dataset_facet(
                dataset,
                key(format!("unknown:{owner}/{}@{name}", element.name)),
            ));
        }
    }
}

fn field_inventory(field: &Element, dataset: &str, out: &mut Vec<SourceElement>) {
    let name = field.get("name").unwrap_or_default();
    out.push(SourceElement::field(dataset, name));
    for (attribute, _) in &field.attributes {
        match attribute.as_str() {
            "name" => {}
            "type" => out.push(SourceElement::facet(dataset, name, Facet::Type)),
            "not-null" => out.push(SourceElement::facet(dataset, name, Facet::Required)),
            other => out.push(SourceElement::facet(dataset, name, key(format!("attr:{other}")))),
        }
    }
    for child in &field.children {
        out.push(if child.name == "description" {
            SourceElement::facet(dataset, name, Facet::Description)
        } else {
            SourceElement::facet(dataset, name, key(format!("unknown:{}", child.name)))
        });
    }
}

fn view_inventory(element: &Element, out: &mut Vec<SourceElement>) {
    let view = element.get("entity-name").unwrap_or_default();
    out.push(SourceElement::dataset(view));
    for (name, _) in &element.attributes {
        if name != "entity-name" {
            out.push(SourceElement::dataset_facet(view, key(format!("attr:{name}"))));
        }
    }
    for (index, child) in element.children.iter().enumerate() {
        out.push(match child.name.as_str() {
            "description" => SourceElement::dataset_facet(view, Facet::Description),
            "member-entity" => SourceElement::dataset_facet(
                view,
                key(format!("member:{}", child.get("entity-alias").unwrap_or_default())),
            ),
            "alias" | "alias-all" | "view-link" | "entity-condition" | "relation" => {
                SourceElement::dataset_facet(view, key(format!("{}#{index}", child.name)))
            }
            other => SourceElement::dataset_facet(view, key(format!("unknown:{other}#{index}"))),
        });
    }
}

/// The structural inventory of every parsed file.
fn inventory_of(files: &[(OfbizFile<'_>, Element)]) -> Vec<SourceElement> {
    let mut out = Vec::new();
    for (file, root) in files {
        let path = file.path;
        for (name, _) in &root.attributes {
            out.push(SourceElement::definition(ROOT_ATTRIBUTE, &format!("{path}@{name}")));
        }
        for (top, element) in root.children.iter().enumerate() {
            let tag = element.name.as_str();
            match (file.role, tag) {
                (Role::Entity, "entity") => {
                    let dataset = element.get("entity-name").unwrap_or_default();
                    out.push(SourceElement::dataset(dataset));
                    entity_inventory(path, top, element, dataset, &mut out);
                }
                (Role::Entity, "extend-entity") => {
                    out.push(SourceElement::definition(EXTEND, &format!("{path}#{top}")));
                    let dataset = element.get("entity-name").unwrap_or_default();
                    entity_inventory(path, top, element, dataset, &mut out);
                }
                (Role::Entity, "view-entity") => view_inventory(element, &mut out),
                (Role::Entity, header) if HEADERS.contains(&header) => {
                    out.push(SourceElement::definition(HEADER, &format!("{path}#{header}")));
                }
                (Role::Group, "entity-group") => {
                    out.push(SourceElement::definition(GROUP, &format!("{path}#{top}")));
                }
                (Role::Seed, _) => out.push(SourceElement::definition(SEED, &format!("{path}#{top}"))),
                _ => out.push(SourceElement::definition(UNKNOWN, &format!("{path}#{top}:{tag}"))),
            }
        }
    }
    out
}

// ----- types ------------------------------------------------------------------

/// The normalized type candidate of an OFBiz field type. Conventions rather
/// than exact equivalents (floats, Y/N indicators) are `inferred`.
fn type_candidate(native: &str) -> TypeCandidate {
    let (normalized, exact) = match native {
        "blob" | "byte-array" | "object" | "binary-very-long" => (NormalizedType::Binary, true),
        "comment" | "long-varchar" | "very-long" | "text-long" | "text-very-long" => (NormalizedType::Text, true),
        "currency-amount" | "currency-precise" | "fixed-point" | "number-decimal" => (NormalizedType::Decimal, true),
        "floating-point" | "number-float" => (NormalizedType::Decimal, false),
        "date" => (NormalizedType::Date, true),
        "date-time" => (NormalizedType::Datetime, true),
        "time" => (NormalizedType::Time, true),
        "indicator" | "text-indicator" => (NormalizedType::Boolean, false),
        "numeric" | "number-integer" => (NormalizedType::Integer, true),
        "credit-card-date" | "credit-card-number" | "description" | "email" | "id" | "id-long"
        | "id-vlong" | "name" | "short-varchar" | "url" | "value" | "very-short" | "text-short"
        | "text-medium" | "text-intermediate" => (NormalizedType::String, true),
        _ => (NormalizedType::Unknown, false),
    };
    TypeCandidate {
        normalized,
        origin: if exact { Origin::Declared } else { Origin::Inferred },
    }
}

// ----- the model ----------------------------------------------------------------

/// One definition part of an entity: the entity itself or an extension.
struct Part<'a> {
    path: &'a str,
    top: usize,
    element: &'a Element,
}

struct Relation<'a> {
    key: String,
    dataset: String,
    element: &'a Element,
}

struct Build<'a> {
    config: &'a OfbizConfig,
    ledger: LossLedger,
    diagnostics: Vec<Diagnostic>,
    datasets: Vec<SourceDataset>,
    fields: Vec<SourceField>,
    constraints: Vec<SourceConstraint>,
    code_sets: Vec<CodeSet>,
    lineage: Vec<SourceLineage>,
    /// Entity name to its parts, in discovery order.
    entities: BTreeMap<String, Vec<Part<'a>>>,
    order: Vec<String>,
    keys: BTreeMap<String, Vec<String>>,
    declared: BTreeMap<String, BTreeSet<String>>,
    relations: Vec<Relation<'a>>,
    /// Seed rows by tag, with their element identity.
    rows: BTreeMap<String, Vec<(String, &'a Element)>>,
    kept: BTreeMap<String, Map<String, Value>>,
    unresolved: Vec<Value>,
    gaps: Vec<Value>,
}

impl<'a> Build<'a> {
    fn dataset_kept(&mut self, dataset: &str) -> &mut Map<String, Value> {
        self.kept.entry(dataset.to_owned()).or_default()
    }

    fn push_kept(&mut self, dataset: &str, member: &str, value: Value) {
        let map = self.dataset_kept(dataset);
        let list = map.entry(member.to_owned()).or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(items) = list {
            items.push(value);
        }
    }

    fn collect(&mut self, files: &'a [(OfbizFile<'a>, Element)]) {
        for (file, root) in files {
            for name in root.attributes.iter().map(|(name, _)| name) {
                let element = SourceElement::definition(ROOT_ATTRIBUTE, &format!("{}@{name}", file.path));
                if SCHEMA_LOCATION.contains(&name.as_str()) {
                    self.ledger.resolve(&element, dropped(DropReason::SchemaPlumbing, "XML schema location"));
                }
            }
            for (top, element) in root.children.iter().enumerate() {
                match (file.role, element.name.as_str()) {
                    (Role::Entity, "entity") => {
                        let name = element.get("entity-name").unwrap_or_default().to_owned();
                        let part = Part { path: file.path, top, element };
                        if let Some(first) = self.entities.get_mut(&name).and_then(|parts| parts.first_mut()) {
                            // The later definition wins, as in OFBiz; the earlier one is a gap.
                            let earlier = std::mem::replace(first, part);
                            self.unresolved.push(json!({"kind": "duplicate-entity", "target": name, "file": earlier.path}));
                            let mut lost = Vec::new();
                            entity_inventory(earlier.path, earlier.top, earlier.element, &name, &mut lost);
                            for element in lost {
                                self.ledger.resolve(&element, unresolved(format!("{name} is defined again later; the later definition wins")));
                            }
                        } else {
                            self.order.push(name.clone());
                            self.entities.insert(name, vec![part]);
                        }
                    }
                    (Role::Seed, tag) => {
                        self.rows
                            .entry(tag.to_owned())
                            .or_default()
                            .push((format!("{}#{top}", file.path), element));
                    }
                    (Role::Entity, header) if HEADERS.contains(&header) => self.ledger.resolve(
                        &SourceElement::definition(HEADER, &format!("{}#{header}", file.path)),
                        only("file header, kept in provenance"),
                    ),
                    _ => {}
                }
            }
        }
        for (file, root) in files {
            for (top, element) in root.children.iter().enumerate() {
                if file.role == Role::Entity && element.name == "extend-entity" {
                    let name = element.get("entity-name").unwrap_or_default();
                    let identity = SourceElement::definition(EXTEND, &format!("{}#{top}", file.path));
                    if let Some(parts) = self.entities.get_mut(name) {
                        parts.push(Part { path: file.path, top, element });
                        self.ledger.resolve(&identity, only(format!("merged into {name}")));
                    } else {
                        self.unresolved.push(json!({"kind": "extend-entity", "target": name, "file": file.path}));
                        let mut lost = Vec::new();
                        entity_inventory(file.path, top, element, name, &mut lost);
                        for element in lost.iter().chain([&identity]) {
                            self.ledger.resolve(element, unresolved(format!("extends {name}, which no pinned file defines")));
                        }
                    }
                }
            }
        }
    }

    fn keys_of(&mut self) {
        for (name, parts) in &self.entities {
            let mut keys = Vec::new();
            for part in parts {
                for field in part.element.named("prim-key").filter_map(|key| key.get("field")) {
                    if !keys.iter().any(|known: &String| known == field) {
                        keys.push(field.to_owned());
                    }
                }
            }
            self.keys.insert(name.clone(), keys);
        }
    }

    fn entity(&mut self, name: &str) {
        let Some(parts) = self.entities.remove(name) else {
            return;
        };
        let Some(first) = parts.first() else {
            return;
        };
        let element = first.element;
        self.ledger.resolve(&SourceElement::dataset(name), only("SourceSchemaIR dataset"));
        let description = element.child_text("description").or_else(|| element.get("title").map(str::to_owned));
        if element.child_text("description").is_some() {
            self.ledger.resolve(&SourceElement::dataset_facet(name, Facet::Description), only("dataset description"));
        }
        let mut attributes = Map::new();
        for (attribute, value) in &element.attributes {
            let facet = SourceElement::dataset_facet(name, key(format!("attr:{attribute}")));
            match attribute.as_str() {
                "entity-name" => {}
                "package-name" => self.ledger.resolve(&facet, only("dataset namespace")),
                storage if STORAGE.contains(&storage) => self.ledger.resolve(
                    &facet,
                    dropped(DropReason::SourceBookkeeping, "a storage or cache setting of the entity engine"),
                ),
                kept if KEPT.contains(&kept) => {
                    attributes.insert(kept.to_owned(), Value::String(value.clone()));
                    self.ledger.resolve(&facet, only("kept in provenance"));
                }
                _ => {}
            }
        }
        let file = first.path.to_owned();
        let map = self.dataset_kept(name);
        map.insert("file".into(), Value::String(file));
        if !attributes.is_empty() {
            map.insert("attributes".into(), Value::Object(attributes));
        }
        self.datasets.push(SourceDataset {
            id: name.to_owned(),
            native_name: name.to_owned(),
            namespace: element.get("package-name").map(str::to_owned),
            kind: DatasetKind::Table,
            description,
        });
        let keys = self.keys.get(name).cloned().unwrap_or_default();
        let mut seen = BTreeSet::new();
        for part in &parts {
            for field in part.element.named("field") {
                self.field(name, part, field, &keys, &mut seen);
            }
        }
        self.declared.insert(name.to_owned(), seen);
        for part in &parts {
            self.part_keys(name, part);
        }
        for part in &parts {
            for (index, relation) in part.element.named("relation").enumerate() {
                self.relations.push(Relation {
                    key: relation_key(part.path, part.top, index),
                    dataset: name.to_owned(),
                    element: relation,
                });
            }
        }
        if !keys.is_empty() {
            self.constraints.push(SourceConstraint {
                id: format!("{name}:pk"),
                kind: ConstraintKind::PrimaryKey,
                dataset: name.to_owned(),
                fields: keys,
                target: None,
                expression: None,
                collection: None,
                origin: Origin::Declared,
                evidence: Vec::new(),
            });
        }
    }

    fn field(&mut self, dataset: &str, part: &Part<'_>, field: &Element, keys: &[String], seen: &mut BTreeSet<String>) {
        let name = field.get("name").unwrap_or_default();
        if !seen.insert(name.to_owned()) {
            self.unresolved.push(json!({"kind": "duplicate-field", "target": dataset, "field": name, "file": part.path}));
            let mut lost = Vec::new();
            field_inventory(field, dataset, &mut lost);
            for element in lost {
                self.ledger.resolve(&element, unresolved(format!("{dataset}.{name} is declared twice")));
            }
            return;
        }
        let native = field.get("type").unwrap_or("unspecified");
        let not_null = field.get("not-null");
        let required = if keys.iter().any(|key| key == name) || not_null == Some("true") {
            Some(true)
        } else if not_null == Some("false") {
            Some(false)
        } else {
            None
        };
        self.ledger.resolve(&SourceElement::field(dataset, name), only("SourceSchemaIR field"));
        self.ledger.resolve_declared(&SourceElement::facet(dataset, name, Facet::Type), only("native type and type candidate"));
        self.ledger.resolve_declared(&SourceElement::facet(dataset, name, Facet::Required), only("required"));
        self.ledger.resolve_declared(&SourceElement::facet(dataset, name, Facet::Description), only("field description"));
        let mut extra = Map::new();
        for (attribute, value) in &field.attributes {
            let facet = SourceElement::facet(dataset, name, key(format!("attr:{attribute}")));
            match attribute.as_str() {
                "col-name" => self.ledger.resolve(&facet, dropped(DropReason::SourceBookkeeping, "a physical column name")),
                "encrypt" => {
                    extra.insert(attribute.clone(), Value::String(value.clone()));
                    self.ledger.resolve(&facet, deferred(NornHome::AccessIr, &codes::SOURCE_DEFERRED, "encrypted at rest: a storage and access policy"));
                }
                "enable-audit-log" => {
                    extra.insert(attribute.clone(), Value::String(value.clone()));
                    self.ledger.resolve(&facet, deferred(NornHome::EntityIr, &codes::SOURCE_DEFERRED, "audit-log setting"));
                }
                _ => {}
            }
        }
        if !extra.is_empty() || part.element.name == "extend-entity" {
            let mut note = extra;
            note.insert("file".into(), Value::String(part.path.to_owned()));
            let map = self.dataset_kept(dataset);
            let fields = map.entry("fields").or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(fields) = fields {
                fields.insert(name.to_owned(), Value::Object(note));
            }
        }
        let ordinal = u64::try_from(seen.len().saturating_sub(1)).unwrap_or(u64::MAX);
        self.fields.push(SourceField {
            dataset: dataset.to_owned(),
            name: name.to_owned(),
            ordinal: WholeNumber(ordinal),
            native_type: native.to_owned(),
            type_candidate: type_candidate(native),
            required,
            markings: Vec::new(),
            description: field.child_text("description"),
        });
    }

    fn part_keys(&mut self, dataset: &str, part: &Part<'_>) {
        let declared = self.declared.get(dataset).cloned().unwrap_or_default();
        for field in part.element.named("prim-key").filter_map(|key| key.get("field")) {
            let element = SourceElement::dataset_facet(dataset, key(format!("prim-key:{field}")));
            if declared.contains(field) {
                self.ledger.resolve(&element, only("primary key"));
            } else {
                self.ledger.resolve(&element, unresolved(format!("primary key names undeclared field {field}")));
            }
        }
        for index in part.element.named("index") {
            let name = index.get("name").unwrap_or_default();
            let element = SourceElement::dataset_facet(dataset, key(format!("index:{name}")));
            let fields: Vec<String> = index.named("index-field").filter_map(|field| field.get("name")).map(str::to_owned).collect();
            if fields.is_empty() || !fields.iter().all(|field| declared.contains(field)) {
                self.ledger.resolve(&element, unresolved(format!("index {name} names undeclared fields")));
            } else if index.get("unique") == Some("true") {
                self.constraints.push(SourceConstraint {
                    id: format!("{dataset}:unique:{name}"),
                    kind: ConstraintKind::Unique,
                    dataset: dataset.to_owned(),
                    fields,
                    target: None,
                    expression: None,
                    collection: None,
                    origin: Origin::Declared,
                    evidence: Vec::new(),
                });
                self.ledger.resolve(&element, only("unique constraint"));
            } else {
                self.push_kept(dataset, "indexes", json!({"name": name, "fields": fields}));
                self.ledger.resolve(&element, only("non-unique index, kept in provenance"));
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "one pass decides every relation's disposition, constraint and kept record")]
    fn relations(&mut self) -> BTreeMap<(String, String), (String, String)> {
        let mut bound = BTreeMap::new();
        let relations = std::mem::take(&mut self.relations);
        let mut counters: BTreeMap<String, usize> = BTreeMap::new();
        for relation in relations {
            let element = SourceElement::dataset_facet(&relation.dataset, key(relation.key.clone()));
            let kind = relation.element.get("type").unwrap_or_default();
            let target = relation.element.get("rel-entity-name").unwrap_or_default();
            let title = relation.element.get("title");
            let maps: Vec<(String, Option<String>)> = relation
                .element
                .named("key-map")
                .map(|map| (map.get("field-name").unwrap_or_default().to_owned(), map.get("rel-field-name").map(str::to_owned)))
                .collect();
            let known = self.keys.contains_key(target);
            let (local, remote): (Vec<String>, Vec<String>) = if maps.is_empty() {
                let keys = self.keys.get(if kind == "many" { relation.dataset.as_str() } else { target }).cloned().unwrap_or_default();
                (keys.clone(), keys)
            } else {
                maps.iter().map(|(field, related)| (field.clone(), related.clone().unwrap_or_else(|| field.clone()))).unzip()
            };
            let mut record = Map::new();
            record.insert("type".into(), json!(kind));
            record.insert("target".into(), json!(target));
            if let Some(title) = title {
                record.insert("title".into(), json!(title));
            }
            record.insert("fields".into(), json!(local));
            record.insert("targetFields".into(), json!(remote));
            if let Some(description) = relation.element.child_text("description") {
                record.insert("description".into(), json!(description));
            }
            let declared_here = self.declared.get(&relation.dataset);
            let declared_there = self.declared.get(target);
            let fits = !local.is_empty()
                && declared_here.is_some_and(|fields| local.iter().all(|field| fields.contains(field)))
                && declared_there.is_some_and(|fields| remote.iter().all(|field| fields.contains(field)));
            let reference = match kind {
                "one" => Some(ConstraintKind::ForeignKey),
                "one-nofk" => Some(ConstraintKind::LogicalReference),
                _ => None,
            };
            if kind == "many" {
                self.push_kept(&relation.dataset, "inverseRelations", Value::Object(record));
                self.ledger.resolve(&element, only("inverse relation, kept in provenance"));
                continue;
            }
            let Some(reference) = reference else {
                record.insert("dataset".into(), json!(relation.dataset));
                record.insert("kind".into(), json!("relation-type"));
                self.unresolved.push(Value::Object(record));
                self.ledger.resolve(&element, unresolved(format!("relation type {kind:?} is not one, one-nofk or many")));
                continue;
            };
            if !known || !fits {
                record.insert("dataset".into(), json!(relation.dataset));
                record.insert("kind".into(), json!(if known { "relation-fields" } else { "relation" }));
                self.unresolved.push(Value::Object(record));
                self.ledger.resolve(&element, unresolved(format!("relation to {target} does not resolve to declared fields")));
                continue;
            }
            let counter = counters.entry(relation.dataset.clone()).or_default();
            let id = format!("{}:rel:{counter}", relation.dataset);
            *counter = counter.saturating_add(1);
            self.constraints.push(SourceConstraint {
                id: id.clone(),
                kind: reference,
                dataset: relation.dataset.clone(),
                fields: local.clone(),
                target: Some(ConstraintTarget { dataset: target.to_owned(), fields: remote.clone() }),
                expression: None,
                collection: None,
                origin: Origin::Declared,
                evidence: Vec::new(),
            });
            self.ledger.resolve(&element, only(format!("{} {id}", if reference == ConstraintKind::ForeignKey { "foreign key" } else { "logical reference" })));
            let mut kept = Map::new();
            for member in ["title", "description"] {
                if let Some(value) = record.get(member) {
                    kept.insert(member.into(), value.clone());
                }
            }
            if !kept.is_empty() {
                let map = self.dataset_kept(&relation.dataset);
                let relations = map.entry("relations").or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(relations) = relations {
                    relations.insert(id.clone(), Value::Object(kept));
                }
            }
            if let ([field], [target_field]) = (local.as_slice(), remote.as_slice()) {
                if target == STATUS || target == ENUMERATION {
                    let what = if target == STATUS { "status type" } else { "enumeration type" };
                    self.gaps.push(json!({
                        "kind": if target == STATUS { "status-type-undeclared" } else { "enumeration-type-undeclared" },
                        "dataset": relation.dataset, "field": field, "constraint": id,
                        "target": target, "targetField": target_field, "title": title,
                    }));
                    self.diagnostics.push(diagnostic(
                        &codes::REFERENCE_UNRESOLVED,
                        format!("{}.{field} references {target}, but the source does not declare which {what} it takes", relation.dataset),
                        Some(format!("{}.{field}", relation.dataset)),
                    ));
                } else {
                    bound.entry((relation.dataset.clone(), field.clone())).or_insert_with(|| (target.to_owned(), target_field.clone()));
                }
            }
        }
        bound
    }

    fn code_sets(&mut self, bound: &BTreeMap<(String, String), (String, String)>) -> BTreeSet<String> {
        let mut used = BTreeSet::new();
        for ((dataset, field), (tag, value_key)) in bound {
            let mut values = Vec::new();
            let mut seen = BTreeSet::new();
            for (_, row) in self.rows.get(tag).into_iter().flatten() {
                if let Some(value) = row.get(value_key)
                    && !value.is_empty()
                    && seen.insert(value.to_owned())
                {
                    values.push(CodeValue {
                        value: value.to_owned(),
                        label: row.get("description").filter(|label| !label.is_empty()).map(str::to_owned),
                    });
                }
            }
            if !values.is_empty() {
                used.insert(tag.clone());
                self.code_sets.push(CodeSet {
                    id: format!("{dataset}.{field}"),
                    dataset: dataset.clone(),
                    field: field.clone(),
                    origin: Origin::ObservedCandidate,
                    values,
                });
            }
        }
        used
    }

    /// Status types with their states and valid changes, bound to no field.
    fn lifecycles(&mut self) -> (Vec<Value>, BTreeSet<String>) {
        let mut broken = BTreeSet::new();
        let mut state_type: BTreeMap<String, String> = BTreeMap::new();
        let mut types: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        for (_, row) in self.rows.get("StatusType").into_iter().flatten() {
            let id = row.get("statusTypeId").unwrap_or_default().to_owned();
            let entry = types.entry(id.clone()).or_default();
            entry.insert("statusTypeId".into(), json!(id));
            for (from, to) in [("description", "description"), ("parentTypeId", "parentTypeId")] {
                if let Some(value) = row.get(from) {
                    entry.insert(to.into(), json!(value));
                }
            }
        }
        for (_, row) in self.rows.get(STATUS).into_iter().flatten() {
            let (Some(id), Some(kind)) = (row.get("statusId"), row.get("statusTypeId")) else {
                continue;
            };
            state_type.insert(id.to_owned(), kind.to_owned());
            let entry = types.entry(kind.to_owned()).or_default();
            entry.entry("statusTypeId").or_insert_with(|| json!(kind));
            let mut state = Map::new();
            for attribute in ["statusId", "statusCode", "sequenceId", "description"] {
                if let Some(value) = row.get(attribute) {
                    state.insert(attribute.into(), json!(value));
                }
            }
            if let Value::Array(states) = entry.entry("states").or_insert_with(|| Value::Array(Vec::new())) {
                states.push(Value::Object(state));
            }
        }
        for (identity, row) in self.rows.get("StatusValidChange").into_iter().flatten() {
            let (from, to) = (row.get("statusId").unwrap_or_default(), row.get("statusIdTo").unwrap_or_default());
            let Some(kind) = state_type.get(from) else {
                broken.insert(identity.clone());
                self.unresolved.push(json!({"kind": "status-change", "from": from, "to": to, "row": identity}));
                continue;
            };
            let mut change = Map::new();
            change.insert("from".into(), json!(from));
            change.insert("to".into(), json!(to));
            if state_type.get(to).is_none_or(|other| other != kind) {
                change.insert("toType".into(), json!(state_type.get(to)));
            }
            for (attribute, member) in [("transitionName", "name"), ("conditionExpression", "condition")] {
                if let Some(value) = row.get(attribute) {
                    change.insert(member.into(), json!(value));
                }
            }
            if let Some(entry) = types.get_mut(kind)
                && let Value::Array(changes) = entry.entry("transitions").or_insert_with(|| Value::Array(Vec::new()))
            {
                changes.push(Value::Object(change));
            }
        }
        (types.into_values().map(Value::Object).collect(), broken)
    }

    fn enumerations(&self) -> Value {
        let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (_, row) in self.rows.get(ENUMERATION).into_iter().flatten() {
            let mut value = Map::new();
            for attribute in ["enumId", "enumCode", "sequenceId", "description"] {
                if let Some(text) = row.get(attribute) {
                    value.insert(attribute.into(), json!(text));
                }
            }
            out.entry(row.get("enumTypeId").unwrap_or_default().to_owned()).or_default().push(Value::Object(value));
        }
        json!(out)
    }

    fn seed_rows(&mut self, used: &BTreeSet<String>, broken: &BTreeSet<String>) {
        let rows = std::mem::take(&mut self.rows);
        for (tag, list) in &rows {
            for (identity, _) in list {
                let element = SourceElement::definition(SEED, identity);
                let disposition = if broken.contains(identity) {
                    unresolved("a status change from a status no pinned file declares")
                } else if ["StatusType", STATUS, "StatusValidChange"].contains(&tag.as_str()) {
                    only("status lifecycle candidate, kept in provenance")
                } else if tag == ENUMERATION || tag == "EnumerationType" {
                    only("enumeration candidate, kept in provenance")
                } else if used.contains(tag) {
                    only("observed code-set value")
                } else {
                    deferred(NornHome::MappingIr, &codes::SOURCE_DEFERRED, format!("a {tag} data row, not schema"))
                };
                self.ledger.resolve(&element, disposition);
            }
        }
        self.rows = rows;
    }

    fn groups(&mut self, files: &'a [(OfbizFile<'a>, Element)]) -> Value {
        let mut groups = Map::new();
        for (file, root) in files.iter().filter(|(file, _)| file.role == Role::Group) {
            for (top, element) in root.children.iter().enumerate() {
                if element.name != "entity-group" {
                    continue;
                }
                let identity = SourceElement::definition(GROUP, &format!("{}#{top}", file.path));
                let entity = element.get("entity").unwrap_or_default();
                if self.declared.contains_key(entity) {
                    groups.insert(entity.to_owned(), json!(element.get("group")));
                    self.ledger.resolve(&identity, only("entity group, kept in provenance"));
                } else {
                    self.ledger.resolve(&identity, unresolved(format!("group entry for {entity}, which no pinned file defines")));
                }
            }
        }
        Value::Object(groups)
    }

    fn views(&mut self, files: &'a [(OfbizFile<'a>, Element)]) {
        let views: Vec<&Element> = files
            .iter()
            .filter(|(file, _)| file.role == Role::Entity)
            .flat_map(|(_, root)| root.named("view-entity"))
            .collect();
        let names: BTreeSet<&str> = views.iter().filter_map(|view| view.get("entity-name")).collect();
        for view in views {
            let name = view.get("entity-name").unwrap_or_default();
            if self.declared.contains_key(name) {
                self.unresolved.push(json!({"kind": "view-name", "target": name}));
                self.ledger.resolve(&SourceElement::dataset(name), unresolved("a view with the name of an entity"));
                continue;
            }
            self.ledger.resolve(&SourceElement::dataset(name), only("SourceSchemaIR view dataset"));
            for (attribute, _) in &view.attributes {
                let facet = SourceElement::dataset_facet(name, key(format!("attr:{attribute}")));
                match attribute.as_str() {
                    "package-name" => self.ledger.resolve(&facet, only("dataset namespace")),
                    "title" => self.ledger.resolve(&facet, only("dataset description")),
                    "never-cache" => self.ledger.resolve(&facet, dropped(DropReason::SourceBookkeeping, "a cache setting")),
                    _ => {}
                }
            }
            self.ledger.resolve_declared(&SourceElement::dataset_facet(name, Facet::Description), only("dataset description"));
            let mut inputs = Vec::new();
            let mut whole = true;
            for (index, child) in view.children.iter().enumerate() {
                match child.name.as_str() {
                    "member-entity" => {
                        let member = child.get("entity-name").unwrap_or_default();
                        let facet = SourceElement::dataset_facet(name, key(format!("member:{}", child.get("entity-alias").unwrap_or_default())));
                        if self.declared.contains_key(member) || names.contains(member) {
                            if !inputs.iter().any(|known| known == member) {
                                inputs.push(member.to_owned());
                            }
                            self.ledger.resolve(&facet, only("lineage input"));
                        } else {
                            whole = false;
                            self.ledger.resolve(&facet, unresolved(format!("member {member} is not a pinned entity or view")));
                        }
                    }
                    "alias" | "alias-all" | "view-link" | "entity-condition" | "relation" => self.ledger.resolve(
                        &SourceElement::dataset_facet(name, key(format!("{}#{index}", child.name))),
                        deferred(NornHome::MappingIr, &codes::SOURCE_DEFERRED, "view aliases, links and conditions stay in the pinned bytes"),
                    ),
                    _ => {}
                }
            }
            self.datasets.push(SourceDataset {
                id: name.to_owned(),
                native_name: name.to_owned(),
                namespace: view.get("package-name").map(str::to_owned),
                kind: DatasetKind::View,
                description: view.child_text("description").or_else(|| view.get("title").map(str::to_owned)),
            });
            if whole && !inputs.is_empty() {
                self.lineage.push(SourceLineage {
                    id: format!("{name}:view"),
                    kind: LineageKind::View,
                    inputs,
                    output: name.to_owned(),
                    evidence: Vec::new(),
                });
            } else {
                self.unresolved.push(json!({"kind": "view-member", "target": name}));
            }
        }
    }
}

fn admit(files: &[OfbizFile<'_>], config: &OfbizConfig) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for file in files {
        if !seen.insert(file.path) {
            return Err(format!("{} is supplied twice", file.path));
        }
        let pin = config.pins.get(file.path).ok_or_else(|| format!("{} has no pin", file.path))?;
        if sha256_hex(file.bytes) != *pin {
            return Err(format!("{} does not match its pinned SHA-256", file.path));
        }
    }
    if let Some(missing) = config.pins.keys().find(|path| !seen.contains(path.as_str())) {
        return Err(format!("pinned file {missing} was not supplied"));
    }
    if !files.iter().any(|file| file.role == Role::Entity) {
        return Err("no entity model file".into());
    }
    Ok(())
}

fn parse_all<'a>(files: &[OfbizFile<'a>]) -> Result<Vec<(OfbizFile<'a>, Element)>, String> {
    files
        .iter()
        .filter_map(|file| file.role.root().map(|root| (file, root)))
        .map(|(file, root)| {
            let element = xml::parse(file.bytes).map_err(|error| format!("{}: {error}", file.path))?;
            if element.name != root {
                return Err(format!("{}: root element is {}, not {root}", file.path, element.name));
            }
            Ok((*file, element))
        })
        .collect()
}

/// The copyright line of a notice file.
fn copyright(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .find(|line| line.contains("Copyright"))
        .map(|line| line.trim().to_owned())
}

impl OfbizReader {
    fn extension(build: &mut Build<'_>, files: &[OfbizFile<'_>], parsed: &[(OfbizFile<'_>, Element)]) -> Map<String, Value> {
        let mut listing = Vec::new();
        for file in files {
            let mut entry = Map::new();
            entry.insert("path".into(), json!(file.path));
            entry.insert("role".into(), json!(file.role.name()));
            entry.insert("sha256".into(), json!(sha256_hex(file.bytes)));
            if let Some((_, root)) = parsed.iter().find(|(parsed, _)| parsed.path == file.path) {
                let header: Map<String, Value> = root
                    .children
                    .iter()
                    .filter(|child| HEADERS.contains(&child.name.as_str()) && !child.text.is_empty())
                    .map(|child| (child.name.clone(), json!(child.text)))
                    .collect();
                if !header.is_empty() {
                    entry.insert("header".into(), Value::Object(header));
                }
            }
            listing.push(Value::Object(entry));
        }
        let notice = files.iter().find(|file| file.role == Role::Notice);
        let mut out = Map::new();
        out.insert("licence".into(), json!({
            "spdx": build.config.licence,
            "file": files.iter().find(|file| file.role == Role::Licence).map(|file| file.path),
        }));
        out.insert("notice".into(), json!({
            "file": notice.map(|file| file.path),
            "copyright": notice.and_then(|file| copyright(file.bytes)),
        }));
        out.insert("files".into(), Value::Array(listing));
        out.insert(
            "datasets".into(),
            Value::Object(std::mem::take(&mut build.kept).into_iter().map(|(name, map)| (name, Value::Object(map))).collect()),
        );
        out.insert("unresolved".into(), Value::Array(std::mem::take(&mut build.unresolved)));
        out.insert("codeSetGaps".into(), Value::Array(std::mem::take(&mut build.gaps)));
        out
    }
}

impl SchemaImporter for OfbizReader {
    type Source<'a> = &'a [OfbizFile<'a>];
    type Config = OfbizConfig;

    fn adapter(&self) -> SourceAdapter {
        SourceAdapter {
            name: ADAPTER.into(),
            version: VERSION.into(),
        }
    }

    fn inventory(&self, files: Self::Source<'_>, _config: &Self::Config) -> Vec<SourceElement> {
        parse_all(files).map(|parsed| inventory_of(&parsed)).unwrap_or_default()
    }

    #[allow(clippy::too_many_lines, reason = "the import's steps in order, each a call")]
    fn import(&self, files: Self::Source<'_>, config: &Self::Config) -> SchemaImport {
        let mut output = SchemaImport {
            provenance: ImportProvenance {
                adapter: self.adapter(),
                parser: Some(ComponentPin {
                    name: "quick-xml".into(),
                    version: "0.41.0".into(),
                }),
                evaluator: None,
                tables: Vec::new(),
                inputs: files.iter().map(|file| InputPin::of(file.path, file.bytes)).collect(),
            },
            source_schema: None,
            layers: Vec::new(),
            diagnostics: Vec::new(),
            loss_report: LossLedger::new().finish(),
        };
        let parsed = match admit(files, config).and_then(|()| parse_all(files)) {
            Ok(parsed) => parsed,
            Err(error) => {
                output.diagnostics.push(diagnostic(&codes::SOURCE_INVALID, error, None));
                return output;
            }
        };
        let mut build = Build {
            config,
            ledger: LossLedger::new(),
            diagnostics: Vec::new(),
            datasets: Vec::new(),
            fields: Vec::new(),
            constraints: Vec::new(),
            code_sets: Vec::new(),
            lineage: Vec::new(),
            entities: BTreeMap::new(),
            order: Vec::new(),
            keys: BTreeMap::new(),
            declared: BTreeMap::new(),
            relations: Vec::new(),
            rows: BTreeMap::new(),
            kept: BTreeMap::new(),
            unresolved: Vec::new(),
            gaps: Vec::new(),
        };
        for element in inventory_of(&parsed) {
            build.ledger.declare(element);
        }
        build.collect(&parsed);
        build.keys_of();
        for name in std::mem::take(&mut build.order) {
            build.entity(&name);
        }
        let bound = build.relations();
        let used = build.code_sets(&bound);
        let (lifecycles, broken) = build.lifecycles();
        let enumerations = build.enumerations();
        build.seed_rows(&used, &broken);
        let groups = build.groups(&parsed);
        build.views(&parsed);
        let mut extension = Self::extension(&mut build, files, &parsed);
        extension.insert("entityGroups".into(), groups);
        extension.insert("lifecycleCandidates".into(), Value::Array(lifecycles));
        extension.insert("enumerationCandidates".into(), enumerations);
        let listing: String = files
            .iter()
            .map(|file| format!("{}\t{}\n", file.path, sha256_hex(file.bytes)))
            .collect();
        let report = build.ledger.finish();
        let unexplained: Vec<String> = report
            .entries
            .iter()
            .filter(|entry| entry.disposition == Disposition::Unexplained)
            .map(|entry| format!("{:?}", entry.element))
            .collect();
        output.diagnostics = build.diagnostics;
        output.loss_report = report;
        if !unexplained.is_empty() {
            output.diagnostics.push(diagnostic(
                &codes::SOURCE_INVALID,
                format!("{} source elements are not understood: {}", unexplained.len(), unexplained.iter().take(5).cloned().collect::<Vec<_>>().join("; ")),
                None,
            ));
            return output;
        }
        output.source_schema = Some(SourceSchemaIr {
            format: SourceSchemaFormat::V1,
            source: SourceIdentity {
                system: config.system.clone(),
                instance: config.repository.clone(),
                version: Some(config.revision.clone()),
            },
            observed_at: config.observed_at.clone(),
            adapter: self.adapter(),
            raw_metadata: EvidenceRef {
                uri: format!("https://github.com/{}/tree/{}", config.repository, config.revision),
                sha256: sha256_hex(listing.as_bytes()),
            },
            datasets: build.datasets,
            fields: build.fields,
            constraints: build.constraints,
            profiles: Vec::new(),
            code_sets: build.code_sets,
            lineage: build.lineage,
            extensions: Some(std::iter::once((EXTENSION.to_owned(), Value::Object(extension))).collect()),
        });
        output
    }
}

/// Whether an import refused: an error diagnostic or no schema.
#[must_use]
pub fn refused(import: &SchemaImport) -> bool {
    import.source_schema.is_none()
        || import
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}
