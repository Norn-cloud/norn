//! Pack projection: the part of a `SourceSchemaIR` that a `Norn.sourceSchema`
//! definition carries, rendered as a typed TypeScript module.
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

/// What the generated module declares.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleSpec {
    /// The module path, relative to the manifest.
    pub path: String,
    /// The exported constant's name.
    pub export: String,
    /// The `source_schema` artifact id.
    pub id: String,
    /// Human title.
    pub title: String,
    /// Artifact version.
    pub version: String,
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

    fn render(self, spec: &ModuleSpec) -> Result<String, String> {
        let schema = self.schema;
        let raw = &schema.raw_metadata;
        let mut source =
            json!({"system": schema.source.system, "instance": schema.source.instance});
        if let Some(version) = &schema.source.version
            && let Some(map) = source.as_object_mut()
        {
            map.insert("version".into(), Value::String(version.clone()));
        }
        let mut value = json!({
            "id": spec.id,
            "title": spec.title,
            "version": spec.version,
            "source": source,
            "observedAt": schema.observed_at,
            // A projection, not raw reader output: its admission is the pack's
            // pinned source, and `rawMetadata` pins the reader's full bundle.
            "adapter": {"name": PROJECTION_ADAPTER, "version": env!("CARGO_PKG_VERSION")},
            "rawMetadata": {"uri": raw.uri, "sha256": raw.sha256},
            "datasets": self.datasets,
            "fields": self.fields,
        });
        if !self.constraints.is_empty()
            && let Some(map) = value.as_object_mut()
        {
            map.insert("constraints".into(), Value::Array(self.constraints));
        }
        let body = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
        Ok(format!(
            "// Generated by `norn-format-read` from the hash-pinned source; do not edit.\n\
             import {{ Norn }} from \"@norn/authoring\"\n\n\
             export const {} = Norn.sourceSchema({body})\n",
            identifier(&spec.export)
        ))
    }
}

/// Render the `Norn.sourceSchema` module for the selected datasets.
///
/// # Errors
///
/// When the selection is empty, names a dataset the schema lacks, or a
/// flatten path does not resolve.
pub fn render_source_module(schema: &SourceSchemaIr, spec: &ModuleSpec) -> Result<String, String> {
    let mut projection = Projection::new(schema);
    projection.select(&spec.select)?;
    projection.project_fields();
    for flat in &spec.flatten {
        projection.flatten(flat)?;
    }
    projection.render(spec)
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
