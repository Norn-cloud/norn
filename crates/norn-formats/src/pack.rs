//! Pack projection: the part of a `SourceSchemaIR` a pack maps from, and the
//! check that a pack's hand-written source contract claims no more than it.
//!
//! A pack never commits schema payload derived from third-party originals.
//! It commits an independently authored contract (a `Norn.sourceSchema` of
//! only the fields its mapping reads); [`check_contract`] verifies, against
//! the projection of the hash-pinned originals, that every contract dataset
//! and field exists upstream with the same native and candidate type, and that
//! a field the contract requires is required upstream.
//!
//! A format reader keeps the whole source graph; a pack maps from a small,
//! named subset of it. This projection is format-independent: it selects
//! datasets by native name, gives every dataset and field an identifier-safe
//! name (mapping expressions read `input.<field>`), keeps each native type,
//! and declares a `parent_child` constraint wherever a field's native type is
//! another selected dataset, so a nested map can read the child collection.
//! Everything else stays in the reader's full output and its loss report.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::source_schema::{NormalizedType, SourceSchemaIr};
use serde_json::{Value, json};

/// The adapter a pack projection declares (the reader's own name is kept in
/// `rawMetadata`, which pins the reader's whole source bundle).
pub const PROJECTION_ADAPTER: &str = "norn.formats.pack";

/// What a pack projects from its originals.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectionSpec {
    /// The pack's compiled source contract, relative to the manifest.
    pub contract: String,
    /// The datasets to carry, by native name; never empty.
    pub select: Vec<String>,
    /// Nested values a selected dataset carries as its own fields.
    #[serde(default)]
    pub flatten: Vec<Flatten>,
}

/// One nested value read into a selected dataset as a field. Each step names
/// a field (by local name) of the previous step's type, starting at
/// `dataset`; the generated field takes the last step's native type. A record
/// reader reads the first occurrence of each step, so the pack author asserts
/// that every step is single-valued for the documents the pack admits.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Flatten {
    /// The selected dataset, by native name.
    pub dataset: String,
    /// Field local names, outermost first; never empty.
    pub path: Vec<String>,
    /// The generated field's name.
    pub name: String,
}

/// The TypeScript type-candidate name for a normalized type.
const fn candidate(normalized: NormalizedType) -> &'static str {
    match normalized {
        NormalizedType::String => "string",
        NormalizedType::Text => "text",
        NormalizedType::Integer => "integer",
        NormalizedType::Decimal => "decimal",
        NormalizedType::Boolean => "boolean",
        NormalizedType::Date => "date",
        NormalizedType::Datetime => "datetime",
        NormalizedType::Time => "time",
        NormalizedType::Duration => "duration",
        NormalizedType::Json => "json",
        NormalizedType::Binary => "binary",
        NormalizedType::Reference => "reference",
        NormalizedType::Unknown => "unknown",
    }
}

/// The local part of a Clark name (`{ns}local@node` → `local`).
fn local(name: &str) -> &str {
    let name = name.split_once('@').map_or(name, |(head, _)| head);
    name.rsplit_once('}').map_or(name, |(_, tail)| tail)
}

/// An identifier-safe rendering: ASCII letters, digits and `_`, never empty,
/// never starting with a digit.
fn identifier(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// A name unique within `taken`, suffixed `_2`, `_3`, … on collision.
fn unique(base: &str, taken: &mut BTreeSet<String>) -> String {
    let mut candidate = base.to_owned();
    let mut n = 2_u32;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{base}_{n}");
        n = n.saturating_add(1);
    }
    candidate
}

/// The projection being built: selected datasets, their fields and the
/// nesting constraints between them.
struct Projection<'s> {
    schema: &'s SourceSchemaIr,
    /// Native name → source dataset id, over every dataset of the schema.
    by_native: BTreeMap<&'s str, &'s str>,
    /// Selected source dataset id → generated dataset id.
    ids: BTreeMap<&'s str, String>,
    datasets: Vec<Value>,
    fields: Vec<Value>,
    constraints: Vec<Value>,
    field_names: BTreeMap<&'s str, BTreeSet<String>>,
    ordinals: BTreeMap<&'s str, u64>,
}

impl<'s> Projection<'s> {
    fn new(schema: &'s SourceSchemaIr) -> Self {
        Self {
            schema,
            by_native: schema
                .datasets
                .iter()
                .map(|d| (d.native_name.as_str(), d.id.as_str()))
                .collect(),
            ids: BTreeMap::new(),
            datasets: Vec::new(),
            fields: Vec::new(),
            constraints: Vec::new(),
            field_names: BTreeMap::new(),
            ordinals: BTreeMap::new(),
        }
    }

    fn source_id(&self, native: &str, purpose: &str) -> Result<&'s str, String> {
        self.by_native
            .get(native)
            .copied()
            .ok_or_else(|| format!("{purpose} names unknown dataset {native}"))
    }

    fn next_ordinal(&mut self, dataset: &'s str) -> u64 {
        let ordinal = self.ordinals.entry(dataset).or_insert(0);
        let current = *ordinal;
        *ordinal = ordinal.saturating_add(1);
        current
    }

    fn select(&mut self, select: &[String]) -> Result<(), String> {
        if select.is_empty() {
            return Err("select names no dataset".into());
        }
        let mut names = BTreeSet::new();
        for native in select {
            let source_id = self.source_id(native, "select")?;
            let id = unique(&identifier(local(native)), &mut names);
            self.datasets
                .push(json!({"id": id, "nativeName": native, "kind": "document"}));
            self.ids.insert(source_id, id);
        }
        Ok(())
    }

    /// Every field of the selected datasets, in source order.
    fn project_fields(&mut self) {
        let schema = self.schema;
        let mut source_fields: Vec<_> = schema
            .fields
            .iter()
            .filter(|f| self.ids.contains_key(f.dataset.as_str()))
            .collect();
        source_fields.sort_by_key(|f| (f.dataset.as_str(), f.ordinal.0));
        for field in source_fields {
            let Some(dataset) = self.ids.get(field.dataset.as_str()).cloned() else {
                continue;
            };
            let name = unique(
                &identifier(local(&field.name)),
                self.field_names.entry(field.dataset.as_str()).or_default(),
            );
            let ordinal = self.next_ordinal(field.dataset.as_str());
            let mut row = json!({
                "dataset": dataset,
                "name": name,
                "ordinal": ordinal,
                "nativeType": field.native_type,
                "typeCandidate": {"type": candidate(field.type_candidate.normalized), "origin": "inferred"},
            });
            if let Some(required) = field.required
                && let Some(map) = row.as_object_mut()
            {
                map.insert("required".into(), Value::Bool(required));
            }
            let child = self
                .by_native
                .get(field.native_type.as_str())
                .and_then(|id| self.ids.get(id));
            if let Some(child) = child {
                self.constraints.push(json!({
                    "id": format!("{dataset}_{name}"),
                    "kind": "parent_child",
                    "dataset": child,
                    "fields": [],
                    "target": {"dataset": dataset, "fields": []},
                    "collection": name,
                    "origin": "declared",
                }));
            }
            self.fields.push(row);
        }
    }

    fn flatten(&mut self, flat: &Flatten) -> Result<(), String> {
        let source_id = self.source_id(&flat.dataset, "flatten")?;
        let dataset = self
            .ids
            .get(source_id)
            .cloned()
            .ok_or_else(|| format!("flatten dataset {} is not selected", flat.dataset))?;
        let (last, init) = flat
            .path
            .split_last()
            .ok_or_else(|| format!("flatten {} has an empty path", flat.name))?;
        let mut current = source_id;
        let mut required = true;
        for step in init {
            let field = self.step(current, step, flat)?;
            required &= field.required == Some(true);
            current = self.source_id(&field.native_type, "flatten step")?;
        }
        let field = self.step(current, last, flat)?;
        required &= field.required == Some(true);
        let names = self.field_names.entry(source_id).or_default();
        if identifier(&flat.name) != flat.name || !names.insert(flat.name.clone()) {
            return Err(format!(
                "flatten name {} is taken or not an identifier",
                flat.name
            ));
        }
        let ordinal = self.next_ordinal(source_id);
        self.fields.push(json!({
            "dataset": dataset,
            "name": flat.name,
            "ordinal": ordinal,
            "nativeType": field.native_type,
            "typeCandidate": {"type": candidate(field.type_candidate.normalized), "origin": "inferred"},
            "required": required,
        }));
        Ok(())
    }

    /// The first field of `dataset` whose local name is `step`.
    fn step(
        &self,
        dataset: &str,
        step: &str,
        flat: &Flatten,
    ) -> Result<&'s norn_ir::source_schema::SourceField, String> {
        self.schema
            .fields
            .iter()
            .filter(|f| f.dataset == dataset && local(&f.name) == step)
            .min_by_key(|f| f.ordinal.0)
            .ok_or_else(|| format!("flatten {}: no field {step}", flat.name))
    }

    fn into_value(self) -> Value {
        let schema = self.schema;
        let raw = &schema.raw_metadata;
        let mut source =
            json!({"system": schema.source.system, "instance": schema.source.instance});
        if let Some(version) = &schema.source.version
            && let Some(map) = source.as_object_mut()
        {
            map.insert("version".into(), Value::String(version.clone()));
        }
        json!({
            "source": source,
            "observedAt": schema.observed_at,
            // A projection, not raw reader output: `rawMetadata` pins the
            // reader's whole source bundle.
            "adapter": {"name": PROJECTION_ADAPTER, "version": env!("CARGO_PKG_VERSION")},
            "rawMetadata": {"uri": raw.uri, "sha256": raw.sha256},
            "datasets": self.datasets,
            "fields": self.fields,
            "constraints": self.constraints,
        })
    }
}

/// The projection of `schema`: selected datasets, their fields (plus flatten
/// paths) and the `parent_child` constraints between them, shaped like a
/// `source_schema` payload.
///
/// # Errors
///
/// When the selection is empty, names a dataset the schema lacks, or a
/// flatten path does not resolve.
pub fn project(schema: &SourceSchemaIr, spec: &ProjectionSpec) -> Result<Value, String> {
    let mut projection = Projection::new(schema);
    projection.select(&spec.select)?;
    projection.project_fields();
    for flat in &spec.flatten {
        projection.flatten(flat)?;
    }
    Ok(projection.into_value())
}

/// A reviewable TypeScript module of the whole projection (never committed).
///
/// # Errors
///
/// When the projection does not serialize.
pub fn render_review_module(projection: &Value) -> Result<String, String> {
    let body = serde_json::to_string_pretty(projection).map_err(|error| error.to_string())?;
    Ok(format!(
        "// Review output of `norn-format-read --emit`: upstream-derived; never commit.\n{body}\n"
    ))
}

fn by_key<'v>(
    list: Option<&'v Value>,
    key: &dyn Fn(&Value) -> Option<String>,
) -> BTreeMap<String, &'v Value> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| key(item).map(|k| (k, item)))
        .collect()
}

fn text<'v>(value: &'v Value, pointer: &str) -> Option<&'v str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// Every way the contract `payload` (a compiled `source_schema` payload)
/// claims more than the `projection` of the pinned originals. Empty when the
/// contract is a faithful subset.
#[must_use]
pub fn check_contract(contract: &Value, projection: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if text(contract, "/rawMetadata/sha256") != text(projection, "/rawMetadata/sha256") {
        problems.push("contract rawMetadata does not pin the originals' bundle".to_owned());
    }
    let dataset_key = |d: &Value| text(d, "/id").map(str::to_owned);
    let upstream_datasets = by_key(projection.get("datasets"), &dataset_key);
    for (id, dataset) in by_key(contract.get("datasets"), &dataset_key) {
        match upstream_datasets.get(&id) {
            None => problems.push(format!("dataset {id} is not selected upstream")),
            Some(up) if text(up, "/nativeName") != text(dataset, "/nativeName") => {
                problems.push(format!("dataset {id} names a different upstream type"));
            }
            Some(_) => {}
        }
    }
    let field_key = |f: &Value| Some(format!("{}.{}", text(f, "/dataset")?, text(f, "/name")?));
    let upstream_fields = by_key(projection.get("fields"), &field_key);
    for (id, field) in by_key(contract.get("fields"), &field_key) {
        let Some(up) = upstream_fields.get(&id) else {
            problems.push(format!("field {id} does not exist upstream"));
            continue;
        };
        if text(up, "/nativeType") != text(field, "/nativeType") {
            problems.push(format!("field {id} has a different native type upstream"));
        }
        if text(up, "/typeCandidate/type") != text(field, "/typeCandidate/type") {
            problems.push(format!(
                "field {id} claims type {:?}; the reader's candidate is {:?}",
                text(field, "/typeCandidate/type"),
                text(up, "/typeCandidate/type")
            ));
        }
        let required = |v: &Value| v.get("required").and_then(Value::as_bool) == Some(true);
        if required(field) && !required(up) {
            problems.push(format!("field {id} is required but optional upstream"));
        }
    }
    let nest_key = |c: &Value| {
        (text(c, "/kind") == Some("parent_child")).then(|| {
            format!(
                "{}<{}.{}",
                text(c, "/dataset").unwrap_or_default(),
                text(c, "/target/dataset").unwrap_or_default(),
                text(c, "/collection").unwrap_or_default()
            )
        })
    };
    let upstream_nests = by_key(projection.get("constraints"), &nest_key);
    for id in by_key(contract.get("constraints"), &nest_key).keys() {
        if !upstream_nests.contains_key(id) {
            problems.push(format!("nesting {id} does not exist upstream"));
        }
    }
    problems
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::{identifier, local, unique};
    use std::collections::BTreeSet;

    #[test]
    fn names_are_identifier_safe_and_unique() {
        assert_eq!(local("{urn:x}Line@doc.xsd#4"), "Line");
        assert_eq!(identifier("cac:Party-Name"), "cac_Party_Name");
        assert_eq!(identifier("9lives"), "_9lives");
        let mut taken = BTreeSet::new();
        assert_eq!(unique("ID", &mut taken), "ID");
        assert_eq!(unique("ID", &mut taken), "ID_2");
    }
}
