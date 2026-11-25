//! PRINCIPAL-INPUT-1 (#1091): admission of a `decide` rule's declared
//! principal inputs.
//!
//! A decide rule reads the acting principal only through the inputs it
//! declares (`principalInputs`). Each becomes one typed column of the closed
//! `principal` record of its `inputSchema`; the host resolves the values and
//! a request never supplies them.
//!
//! - **T2** ([`validate`], one rule): the declarations are well formed and
//!   unique, the `principal` record has exactly one required column per
//!   declaration ([`codes::PRINCIPAL_INPUT_INVALID`]), no node writes into
//!   the reserved namespace, and every `principal.<name>` an expression
//!   reads is declared ([`codes::PRINCIPAL_INPUT_UNDECLARED`]). A rule with
//!   no declarations reads nothing under `principal`.
//! - **T2** ([`validate_catalog`], one policy): every `principalCatalog`
//!   entry of an `AccessIR` policy is well formed.
//! - **T3** ([`check_set`], the resolved set): the catalog is every entry the
//!   `AccessIR` policies of the set and the installed base declare, by name.
//!   Two different declarations of one name refuse
//!   ([`codes::PRINCIPAL_CATALOG_CONFLICT`]). Each input names an entry of
//!   the source it reads ([`codes::PRINCIPAL_INPUT_UNKNOWN`]) and its column
//!   has that entry's type ([`codes::PRINCIPAL_INPUT_TYPE`]).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::access::{
    AccessIr, PrincipalAttributeCatalog, PrincipalAttributeType, PrincipalCatalogEntry,
    PrincipalCatalogError, ScopeDimension,
};
use norn_ir::rule::{
    DECIDE_PRINCIPAL_MEMBER, HoldsRoleAt, PrincipalInputDeclaration, PrincipalInputKind,
};
use norn_ir::{ArtifactEnvelope, ArtifactKind, Diagnostic, JsonValue};

use crate::entity_artifacts::EntityArtifactSetContext;
use crate::envelope::invalid_diagnostic;
use crate::lifecycle_writes::finding;

/// Stable diagnostic codes of principal-input admission.
pub(crate) mod codes {
    /// A `principalInputs` declaration or its `principal` record is malformed.
    pub(crate) const PRINCIPAL_INPUT_INVALID: &str = "rule.validation.principal_input_invalid";
    /// An expression reads a `principal.*` name the rule does not declare.
    pub(crate) const PRINCIPAL_INPUT_UNDECLARED: &str =
        "rule.validation.principal_input_undeclared";
    /// An input names a role, role profile or attribute no catalog declares.
    pub(crate) const PRINCIPAL_INPUT_UNKNOWN: &str =
        "artifact_set.decision.principal_input_unknown";
    /// An input's column type is not its catalog entry's type.
    pub(crate) const PRINCIPAL_INPUT_TYPE: &str = "artifact_set.decision.principal_input_type";
    /// Two policies declare one catalog name differently.
    pub(crate) const PRINCIPAL_CATALOG_CONFLICT: &str =
        "artifact_set.policy.principal_catalog_conflict";
}

const POLICY_CODE: &str = "policy.validation.invalid_payload";
const INPUTS: &str = "payload.principalInputs";

macro_rules! decode {
    ($ty:ty, $value:expr) => {
        serde_json::to_value($value)
            .ok()
            .and_then(|value| serde_json::from_value::<$ty>(value).ok())
    };
}

struct Sink<'a> {
    diagnostics: &'a mut Vec<Diagnostic>,
    artifact_id: Option<&'a str>,
}

impl Sink<'_> {
    fn push(&mut self, code: &str, message: String, path: String) {
        self.diagnostics
            .push(invalid_diagnostic(code, message, path, self.artifact_id));
    }

    fn invalid(&mut self, message: impl Into<String>, path: impl Into<String>) {
        self.push(codes::PRINCIPAL_INPUT_INVALID, message.into(), path.into());
    }
}

fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn member<'v>(value: Option<&'v JsonValue>, name: &str) -> Option<&'v JsonValue> {
    value?.as_object()?.get(name)
}

/// T2: check one rule's `principalInputs`, its `principal` input record and
/// every `principal.*` read of its graph.
pub(crate) fn validate(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let mut sink = Sink {
        diagnostics,
        artifact_id,
    };
    let declared = match payload.get("principalInputs") {
        None => Vec::new(),
        Some(value) => {
            let Some(declared) = decode!(Vec<PrincipalInputDeclaration>, value) else {
                sink.invalid(
                    "principalInputs is a list of {name, kind}, where kind is holdsRole {role, at}, roleProfile {profile} or attribute {name}",
                    INPUTS,
                );
                return;
            };
            declared
        }
    };
    if !declared.is_empty()
        && payload.get("contract").and_then(JsonValue::as_string) != Some("decide")
    {
        sink.invalid(
            "principalInputs is only allowed with a RuleIR v2 decide graph",
            INPUTS,
        );
    }
    let mut names = BTreeSet::new();
    for (index, input) in declared.iter().enumerate() {
        check_declaration(input, index, &mut sink);
        if !names.insert(input.name.as_str()) {
            sink.invalid(
                format!("principal input {:?} is declared twice", input.name),
                format!("{INPUTS}[{index}].name"),
            );
        }
    }
    check_record(payload, &names, &mut sink);
    check_graph(payload, &names, &mut sink);
}

fn check_declaration(input: &PrincipalInputDeclaration, index: usize, sink: &mut Sink<'_>) {
    if !is_identifier(&input.name) {
        sink.invalid(
            format!(
                "principal input name {:?} is not an identifier ([A-Za-z_][A-Za-z0-9_]*)",
                input.name
            ),
            format!("{INPUTS}[{index}].name"),
        );
    }
    if input.kind.catalog_name().trim().is_empty() {
        sink.invalid(
            "a principal input names the catalog entry it reads",
            format!("{INPUTS}[{index}].kind"),
        );
    }
    if let PrincipalInputKind::HoldsRole(holds) = &input.kind
        && let HoldsRoleAt::Scope(dimensions) = &holds.at
    {
        let distinct: BTreeSet<ScopeDimension> = dimensions.iter().copied().collect();
        if distinct.len() != dimensions.len() || !distinct.contains(&ScopeDimension::Org) {
            sink.invalid(
                "holdsRole.at is \"resource\" or a list of distinct scope dimensions that includes org",
                format!("{INPUTS}[{index}].kind.holdsRole.at"),
            );
        }
    }
}

/// The `principal` input record has exactly one required column per
/// declaration, and exists exactly when the rule declares inputs.
fn check_record(
    payload: &BTreeMap<String, JsonValue>,
    names: &BTreeSet<&str>,
    sink: &mut Sink<'_>,
) {
    let path = format!("payload.inputSchema.properties.{DECIDE_PRINCIPAL_MEMBER}");
    let record = member(
        member(payload.get("inputSchema"), "properties"),
        DECIDE_PRINCIPAL_MEMBER,
    );
    let Some(record) = record else {
        if !names.is_empty() {
            sink.invalid(
                "declared principalInputs require the closed input record `principal`, with one required column per input",
                path,
            );
        }
        return;
    };
    if names.is_empty() {
        sink.invalid(
            "the input member `principal` is reserved for declared principalInputs",
            path,
        );
        return;
    }
    let columns: BTreeSet<&str> = member(Some(record), "properties")
        .and_then(JsonValue::as_object)
        .map_or_default(|properties| properties.keys().map(String::as_str).collect());
    let required: BTreeSet<&str> = member(Some(record), "required")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsonValue::as_string)
        .collect();
    for name in names.difference(&columns) {
        sink.invalid(
            format!("principal input {name:?} has no column in the `principal` record"),
            format!("{path}.properties"),
        );
    }
    for column in columns.difference(names) {
        sink.push(
            codes::PRINCIPAL_INPUT_UNDECLARED,
            format!("the `principal` record declares {column:?}, which is not a declared principal input"),
            format!("{path}.properties.{column}"),
        );
    }
    if let Some(optional) = columns.iter().find(|column| !required.contains(**column)) {
        sink.invalid(
            format!("principal input {optional:?} is always supplied: its column must be required"),
            format!("{path}.required"),
        );
    }
}

/// One string below a node's `content`: the member that holds it and the
/// member that holds that one (`field` of `outputs`, a cell of `rules`).
struct Site<'v> {
    holder: &'v str,
    parent: &'v str,
    text: &'v str,
}

fn strings<'v>(value: &'v JsonValue, holder: &'v str, parent: &'v str, out: &mut Vec<Site<'v>>) {
    match value {
        JsonValue::String(text) => out.push(Site {
            holder,
            parent,
            text,
        }),
        JsonValue::Array(items) => {
            for item in items {
                strings(item, holder, parent, out);
            }
        }
        JsonValue::Object(members) => {
            for (name, item) in members {
                strings(item, name, holder, out);
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => {}
    }
}

impl Site<'_> {
    /// A path the node writes: an expression's `key`, a table output's
    /// `field`, an `outputPath`.
    fn is_written_path(&self) -> bool {
        matches!(
            (self.parent, self.holder),
            ("expressions", "key") | ("outputs", "field") | (_, "outputPath")
        )
    }

    /// Identity and display members, and a sub-decision's artifact `key`:
    /// never evaluated. A decision-table row is keyed by column id, so only
    /// its `_id` and `_description` are metadata.
    fn is_metadata(&self) -> bool {
        if self.parent == "rules" {
            return matches!(self.holder, "_id" | "_description");
        }
        matches!(
            self.holder,
            "id" | "name" | "type" | "key" | "hitPolicy" | "executionMode"
        )
    }
}

/// No node writes under `principal`, and every `principal.<name>` an
/// expression reads is declared. Only text that names `principal` is parsed.
fn check_graph(payload: &BTreeMap<String, JsonValue>, names: &BTreeSet<&str>, sink: &mut Sink<'_>) {
    let nodes = member(payload.get("graph"), "nodes")
        .and_then(JsonValue::as_array)
        .unwrap_or_default();
    let reserved = format!("{DECIDE_PRINCIPAL_MEMBER}.");
    for (index, node) in nodes.iter().enumerate() {
        let path = format!("payload.graph.nodes[{index}].content");
        let mut found = Vec::new();
        if let Some(content) = member(Some(node), "content") {
            strings(content, "content", "node", &mut found);
        }
        for site in found {
            if !site.text.contains(DECIDE_PRINCIPAL_MEMBER) {
                continue;
            }
            if site.is_written_path() {
                if site.text == DECIDE_PRINCIPAL_MEMBER || site.text.starts_with(&reserved) {
                    sink.invalid(
                        format!(
                            "{:?} writes into the reserved `principal` namespace",
                            site.text
                        ),
                        path.clone(),
                    );
                }
            } else if !site.is_metadata() {
                check_read(site.text, names, &path, sink);
            }
        }
    }
}

fn check_read(source: &str, names: &BTreeSet<&str>, path: &str, sink: &mut Sink<'_>) {
    let Ok(reads) = norn_expr::expression_reads(source, DECIDE_PRINCIPAL_MEMBER) else {
        sink.push(
            codes::PRINCIPAL_INPUT_UNDECLARED,
            format!(
                "{source:?} names `principal` outside a standard expression; read a principal input in an expression, a switch condition, an input column's field or an output cell"
            ),
            path.to_owned(),
        );
        return;
    };
    if reads.whole_root {
        sink.push(
            codes::PRINCIPAL_INPUT_UNDECLARED,
            format!(
                "{source:?} reads the `principal` record as a whole or by a computed name; read each declared input by its name"
            ),
            path.to_owned(),
        );
    }
    for field in reads
        .fields
        .iter()
        .filter(|field| !names.contains(field.as_str()))
    {
        sink.push(
            codes::PRINCIPAL_INPUT_UNDECLARED,
            format!(
                "the decision reads principal.{field}, which it does not declare in principalInputs"
            ),
            path.to_owned(),
        );
    }
}

/// T2: every `principalCatalog` entry of an `AccessIR` policy is well formed
/// and declared once.
pub(crate) fn validate_catalog(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(entries) = payload
        .get("principalCatalog")
        .and_then(|value| decode!(Vec<PrincipalCatalogEntry>, value))
    else {
        return;
    };
    let mut catalog = PrincipalAttributeCatalog::default();
    for (index, entry) in entries.iter().enumerate() {
        let message = match catalog.declare(entry) {
            Ok(()) => continue,
            Err(PrincipalCatalogError::Conflict { name }) => {
                format!(
                    "principal catalog entry {name:?} is declared twice with different definitions"
                )
            }
            Err(
                PrincipalCatalogError::InvalidEntry { name }
                | PrincipalCatalogError::Unknown { name },
            ) => format!(
                "principal catalog entry {name:?} is invalid: a role is bool, a role profile is stringSet with at least one role, and a name is an identifier"
            ),
        };
        diagnostics.push(invalid_diagnostic(
            POLICY_CODE,
            message,
            format!("payload.principalCatalog[{index}]"),
            artifact_id,
        ));
    }
}

/// The catalog entries each `AccessIR` policy artifact declares.
fn declared_entries(artifact: &ArtifactEnvelope) -> Vec<PrincipalCatalogEntry> {
    if artifact.kind() != ArtifactKind::Policy {
        return Vec::new();
    }
    decode!(AccessIr, &artifact.payload.to_json_value())
        .map(|access| access.principal_catalog)
        .unwrap_or_default()
}

/// T3: build the set's one catalog and check every candidate decide rule's
/// inputs against it.
pub(crate) fn check_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<crate::Diagnostic> {
    let mut findings = Vec::new();
    let mut catalog = PrincipalAttributeCatalog::default();
    // The installed base is read-only and already admitted: its entries win.
    for entry in context.base.iter().flat_map(declared_entries) {
        let _already_reported = catalog.declare(&entry);
    }
    for artifact in artifacts {
        for (index, entry) in declared_entries(artifact).iter().enumerate() {
            if let Err(PrincipalCatalogError::Conflict { name }) = catalog.declare(entry) {
                findings.push(finding(
                    codes::PRINCIPAL_CATALOG_CONFLICT,
                    format!(
                        "principal catalog entry {name:?} is declared differently by another policy; a name has one type and one source"
                    ),
                    artifact,
                    &format!("/payload/principalCatalog/{index}"),
                ));
            }
        }
    }
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Rule)
    {
        let payload = artifact.payload.to_json_value();
        let Some(declared) = member(Some(&payload), "principalInputs")
            .and_then(|value| decode!(Vec<PrincipalInputDeclaration>, value))
        else {
            continue;
        };
        let columns = member(
            member(member(Some(&payload), "inputSchema"), "properties"),
            DECIDE_PRINCIPAL_MEMBER,
        );
        for (index, input) in declared.iter().enumerate() {
            findings.extend(check_input(artifact, input, index, columns, &catalog));
        }
    }
    findings
}

fn check_input(
    artifact: &ArtifactEnvelope,
    input: &PrincipalInputDeclaration,
    index: usize,
    record: Option<&JsonValue>,
    catalog: &PrincipalAttributeCatalog,
) -> Option<crate::Diagnostic> {
    let Ok(entry) = catalog.resolve(&input.kind) else {
        let source = match &input.kind {
            PrincipalInputKind::HoldsRole(_) => "role",
            PrincipalInputKind::RoleProfile(_) => "role profile",
            PrincipalInputKind::Attribute(_) => "scoped attribute",
        };
        return Some(finding(
            codes::PRINCIPAL_INPUT_UNKNOWN,
            format!(
                "principal input {:?} reads {source} {:?}, which no policy's principalCatalog declares",
                input.name,
                input.kind.catalog_name()
            ),
            artifact,
            &format!("/payload/principalInputs/{index}/kind"),
        ));
    };
    let column = member(member(record, "properties"), &input.name);
    let declared_type = column_type(column);
    (declared_type != Some(entry.value_type)).then(|| {
        finding(
            codes::PRINCIPAL_INPUT_TYPE,
            format!(
                "principal input {:?} is {}, but its column is declared {}",
                input.name,
                type_name(Some(entry.value_type)),
                type_name(declared_type)
            ),
            artifact,
            &format!(
                "/payload/inputSchema/properties/{DECIDE_PRINCIPAL_MEMBER}/properties/{}",
                input.name
            ),
        )
    })
}

/// The principal attribute type a column schema declares, if it is one.
fn column_type(column: Option<&JsonValue>) -> Option<PrincipalAttributeType> {
    let kind = member(column, "type")?.as_string()?;
    match kind {
        "boolean" => Some(PrincipalAttributeType::Bool),
        "integer" => Some(PrincipalAttributeType::Long),
        "string" => Some(PrincipalAttributeType::String),
        "array" => (member(member(column, "items"), "type")?.as_string()? == "string")
            .then_some(PrincipalAttributeType::StringSet),
        _ => None,
    }
}

const fn type_name(value_type: Option<PrincipalAttributeType>) -> &'static str {
    match value_type {
        Some(PrincipalAttributeType::Bool) => "a boolean",
        Some(PrincipalAttributeType::Long) => "an integer",
        Some(PrincipalAttributeType::String) => "a string",
        Some(PrincipalAttributeType::StringSet) => "a list of strings",
        None => "another type",
    }
}
