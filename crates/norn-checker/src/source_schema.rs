//! `SourceSchemaIR` checks (T8-SRC-1, #2596): a discovery adapter's output
//! is well-formed before it becomes evidence.
//!
//! [`decode_source_schema`] decodes a document from its wire JSON (a decode
//! failure is [`codes::INVALID_DOCUMENT`]) and runs [`check_source_schema`],
//! which reports every finding located by an RFC 6901 pointer into the
//! document. The rules are those of `norn_ir::source_schema`:
//!
//! - ids are non-blank and unique per list; every dataset and field a list
//!   names is declared;
//! - field names and ordinals are unique within their dataset;
//! - an inference is never a declaration: a constraint is `declared` or
//!   `inferred`, a code set `declared` or `observed_candidate`, a type
//!   candidate `declared` or `inferred` ([`codes::PROMOTED_INFERENCE`]);
//! - a reference constraint (`foreign_key`, `logical_reference`,
//!   `parent_child`) has a target with as many fields as it constrains, a
//!   `check` has an expression, and nothing else has either;
//! - a profile's counts never exceed the rows it saw, field measures appear
//!   only on field profiles, and a distinct count states its precision;
//! - samples are bounded by their policy and by
//!   [`norn_ir::source_schema::MAX_SAMPLE_VALUES`], and a `withheld` policy
//!   keeps none;
//! - timestamps are RFC 3339 with an explicit offset and evidence hashes are
//!   lower-case hex SHA-256.

use std::collections::BTreeSet;

use norn_ir::TimezonePolicy;
use norn_ir::source_schema::{
    CodeSet, ConstraintKind, EvidenceRef, MAX_SAMPLE_VALUES, Origin, Redaction, SourceConstraint,
    SourceProfile, SourceSchemaIr,
};

use crate::object_types::valid_timestamp;
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// Stable `source_schema.validation.*` codes.
pub mod codes {
    /// The document does not decode as a `SourceSchemaIR` document.
    pub const INVALID_DOCUMENT: &str = "source_schema.validation.invalid_document";
    /// A source namespace component contains a control character.
    pub const INVALID_SOURCE_IDENTITY: &str = "source_schema.validation.invalid_source_identity";
    /// A required name or id is blank.
    pub const BLANK_VALUE: &str = "source_schema.validation.blank_value";
    /// Two entries of one list share an id.
    pub const DUPLICATE_ID: &str = "source_schema.validation.duplicate_id";
    /// A dataset id names no declared dataset.
    pub const UNKNOWN_DATASET: &str = "source_schema.validation.unknown_dataset";
    /// Two fields of one dataset share a name or an ordinal.
    pub const DUPLICATE_FIELD: &str = "source_schema.validation.duplicate_field";
    /// A field name names no field of its dataset.
    pub const UNKNOWN_FIELD: &str = "source_schema.validation.unknown_field";
    /// An origin the list does not admit: an inference recorded as a
    /// declaration's sibling kind (a constraint `observed_candidate`, a code
    /// set or type candidate of the wrong origin).
    pub const PROMOTED_INFERENCE: &str = "source_schema.validation.promoted_inference";
    /// A constraint's target or expression does not fit its kind.
    pub const CONSTRAINT_SHAPE: &str = "source_schema.validation.constraint_shape";
    /// A profile's measures are inconsistent.
    pub const PROFILE_COUNTS: &str = "source_schema.validation.profile_counts";
    /// A sample set exceeds its bound or keeps withheld values.
    pub const SAMPLE_BOUND: &str = "source_schema.validation.sample_bound";
    /// A code set is empty or repeats a value.
    pub const DUPLICATE_CODE: &str = "source_schema.validation.duplicate_code";
    /// An evidence reference is malformed.
    pub const INVALID_EVIDENCE: &str = "source_schema.validation.invalid_evidence";
    /// A timestamp is not RFC 3339 with an explicit offset.
    pub const INVALID_TIMESTAMP: &str = "source_schema.validation.invalid_timestamp";
}

/// Decode a `SourceSchemaIR` document from its wire JSON and check it.
///
/// # Errors
///
/// [`codes::INVALID_DOCUMENT`] when the JSON does not decode (closed members,
/// types, and enums are the decoder's), or every finding of
/// [`check_source_schema`].
pub fn decode_source_schema(value: &serde_json::Value) -> Result<SourceSchemaIr, Vec<Diagnostic>> {
    let document: SourceSchemaIr = serde_json::from_value(value.clone()).map_err(|decode| {
        vec![error(
            codes::INVALID_DOCUMENT,
            format!("the document is not a SourceSchemaIR document: {decode}"),
            Span::pointer(""),
        )]
    })?;
    let findings = check_source_schema(&document);
    if findings.is_empty() {
        Ok(document)
    } else {
        Err(findings)
    }
}

/// Check a decoded `SourceSchemaIR` document. Empty when it is valid. See the
/// module docs.
#[must_use]
pub fn check_source_schema(document: &SourceSchemaIr) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_envelope(document, &mut out);
    let datasets = check_datasets(document, &mut out);
    check_fields(document, &datasets, &mut out);
    let mut constraint_ids = BTreeSet::new();
    for (index, constraint) in document.constraints.iter().enumerate() {
        check_id(
            &constraint.id,
            "constraints",
            index,
            &mut constraint_ids,
            &mut out,
        );
        check_constraint(document, constraint, index, &datasets, &mut out);
    }
    for (index, profile) in document.profiles.iter().enumerate() {
        check_profile(document, profile, index, &datasets, &mut out);
    }
    let mut code_set_ids = BTreeSet::new();
    for (index, code_set) in document.code_sets.iter().enumerate() {
        check_id(&code_set.id, "codeSets", index, &mut code_set_ids, &mut out);
        check_code_set(document, code_set, index, &datasets, &mut out);
    }
    let mut lineage_ids = BTreeSet::new();
    for (index, edge) in document.lineage.iter().enumerate() {
        check_id(&edge.id, "lineage", index, &mut lineage_ids, &mut out);
        let at = |member: &str| member_at("lineage", index, member);
        if edge.inputs.is_empty() {
            out.push(error(
                codes::CONSTRAINT_SHAPE,
                format!("lineage {:?} names no input dataset", edge.id),
                at("inputs"),
            ));
        }
        for (input, dataset) in edge.inputs.iter().enumerate() {
            known_dataset(
                dataset,
                &datasets,
                Span::from_segments(["lineage", &index.to_string(), "inputs", &input.to_string()]),
                &mut out,
            );
        }
        known_dataset(&edge.output, &datasets, at("output"), &mut out);
        check_evidence_list(&edge.evidence, "lineage", index, &mut out);
    }
    out
}

fn member_at(list: &str, index: usize, member: &str) -> Span {
    Span::from_segments([list, &index.to_string(), member])
}

fn check_envelope(document: &SourceSchemaIr, out: &mut Vec<Diagnostic>) {
    check_source_identity(document, out);
    for (value, pointer) in [
        (&document.source.system, ["source", "system"]),
        (&document.source.instance, ["source", "instance"]),
        (&document.adapter.name, ["adapter", "name"]),
        (&document.adapter.version, ["adapter", "version"]),
    ] {
        blank(value, Span::from_segments(pointer), out);
    }
    timestamp(&document.observed_at, Span::pointer("/observedAt"), out);
    check_evidence(&document.raw_metadata, Span::pointer("/rawMetadata"), out);
}

/// Namespace components must never contain the delimiter used by mapping ownership.
pub(crate) fn source_identity_has_control_chars(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// Also used by direct mapping admission, which need not check the schema envelope.
pub(crate) fn check_source_identity(document: &SourceSchemaIr, out: &mut Vec<Diagnostic>) {
    for (value, member) in [
        (&document.source.system, "system"),
        (&document.source.instance, "instance"),
    ] {
        if source_identity_has_control_chars(value) {
            out.push(error(
                codes::INVALID_SOURCE_IDENTITY,
                "source identity components must not contain control characters".to_owned(),
                Span::from_segments(["source", member]),
            ));
        }
    }
}

fn check_datasets(document: &SourceSchemaIr, out: &mut Vec<Diagnostic>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for (index, dataset) in document.datasets.iter().enumerate() {
        check_id(&dataset.id, "datasets", index, &mut ids, out);
        blank(
            &dataset.native_name,
            member_at("datasets", index, "nativeName"),
            out,
        );
    }
    ids
}

fn check_fields(document: &SourceSchemaIr, datasets: &BTreeSet<String>, out: &mut Vec<Diagnostic>) {
    let mut names = BTreeSet::new();
    let mut ordinals = BTreeSet::new();
    for (index, field) in document.fields.iter().enumerate() {
        let at = |member: &str| member_at("fields", index, member);
        known_dataset(&field.dataset, datasets, at("dataset"), out);
        if blank(&field.name, at("name"), out) {
            continue;
        }
        if !names.insert((field.dataset.as_str(), field.name.as_str())) {
            out.push(error(
                codes::DUPLICATE_FIELD,
                format!(
                    "dataset {:?} declares field {:?} twice",
                    field.dataset, field.name
                ),
                at("name"),
            ));
        }
        if !ordinals.insert((field.dataset.as_str(), field.ordinal)) {
            out.push(error(
                codes::DUPLICATE_FIELD,
                format!(
                    "dataset {:?} declares ordinal {} twice",
                    field.dataset, field.ordinal.0
                ),
                at("ordinal"),
            ));
        }
        blank(&field.native_type, at("nativeType"), out);
        if field.type_candidate.origin == Origin::ObservedCandidate {
            out.push(error(
                codes::PROMOTED_INFERENCE,
                format!(
                    "the type candidate of field {:?} is observed_candidate; a type is declared or inferred",
                    field.name
                ),
                Span::from_segments(["fields", &index.to_string(), "typeCandidate", "origin"]),
            ));
        }
    }
}

fn check_constraint(
    document: &SourceSchemaIr,
    constraint: &SourceConstraint,
    index: usize,
    datasets: &BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    let at = |member: &str| member_at("constraints", index, member);
    if constraint.origin == Origin::ObservedCandidate {
        out.push(error(
            codes::PROMOTED_INFERENCE,
            format!(
                "constraint {:?} is observed_candidate; a constraint is declared or inferred",
                constraint.id
            ),
            at("origin"),
        ));
    }
    if known_dataset(&constraint.dataset, datasets, at("dataset"), out) {
        known_fields(
            document,
            &constraint.dataset,
            &constraint.fields,
            &["constraints", &index.to_string(), "fields"],
            out,
        );
    }
    if constraint.fields.is_empty() {
        out.push(error(
            codes::CONSTRAINT_SHAPE,
            format!("constraint {:?} constrains no field", constraint.id),
            at("fields"),
        ));
    }
    check_constraint_target(document, constraint, index, datasets, out);
    if let Some(collection) = &constraint.collection {
        let parent = constraint
            .target
            .as_ref()
            .filter(|_| constraint.kind == ConstraintKind::ParentChild);
        match parent {
            None => out.push(error(
                codes::CONSTRAINT_SHAPE,
                format!(
                    "constraint {:?}: only a parent_child constraint names a collection",
                    constraint.id
                ),
                at("collection"),
            )),
            Some(target) if document.field(&target.dataset, collection).is_none() => {
                out.push(unknown_field(&target.dataset, collection, at("collection")));
            }
            Some(_) => {}
        }
    }
    let check = constraint.kind == ConstraintKind::Check;
    if check != constraint.expression.is_some() {
        out.push(error(
            codes::CONSTRAINT_SHAPE,
            format!(
                "constraint {:?}: a check (and only a check) carries an expression",
                constraint.id
            ),
            at("expression"),
        ));
    }
    check_evidence_list(&constraint.evidence, "constraints", index, out);
}

fn check_constraint_target(
    document: &SourceSchemaIr,
    constraint: &SourceConstraint,
    index: usize,
    datasets: &BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    let at = |member: &str| member_at("constraints", index, member);
    let reference = matches!(
        constraint.kind,
        ConstraintKind::ForeignKey | ConstraintKind::LogicalReference | ConstraintKind::ParentChild
    );
    match (&constraint.target, reference) {
        (Some(target), true) => {
            if known_dataset(
                &target.dataset,
                datasets,
                Span::from_segments(["constraints", &index.to_string(), "target", "dataset"]),
                out,
            ) {
                known_fields(
                    document,
                    &target.dataset,
                    &target.fields,
                    &["constraints", &index.to_string(), "target", "fields"],
                    out,
                );
            }
            if target.fields.len() != constraint.fields.len() {
                out.push(error(
                    codes::CONSTRAINT_SHAPE,
                    format!(
                        "constraint {:?} constrains {} fields but references {}",
                        constraint.id,
                        constraint.fields.len(),
                        target.fields.len()
                    ),
                    at("target"),
                ));
            }
        }
        (None, true) => out.push(error(
            codes::CONSTRAINT_SHAPE,
            format!(
                "constraint {:?} is a reference and needs a target",
                constraint.id
            ),
            at("target"),
        )),
        (Some(_), false) => out.push(error(
            codes::CONSTRAINT_SHAPE,
            format!(
                "constraint {:?} is not a reference and has no target",
                constraint.id
            ),
            at("target"),
        )),
        (None, false) => {}
    }
}

fn check_profile(
    document: &SourceSchemaIr,
    profile: &SourceProfile,
    index: usize,
    datasets: &BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    let at = |member: &str| member_at("profiles", index, member);
    if known_dataset(&profile.dataset, datasets, at("dataset"), out)
        && let Some(field) = &profile.field
        && document.field(&profile.dataset, field).is_none()
    {
        out.push(unknown_field(&profile.dataset, field, at("field")));
    }
    timestamp(&profile.as_of, at("asOf"), out);
    let rows = profile.rows_seen.0;
    for (member, count) in [
        ("nullCount", profile.null_count),
        ("distinctCount", profile.distinct_count),
    ] {
        let Some(count) = count else { continue };
        if profile.field.is_none() {
            out.push(error(
                codes::PROFILE_COUNTS,
                format!(
                    "{member} is a field measure, but the profile measures dataset {:?}",
                    profile.dataset
                ),
                at(member),
            ));
        } else if count.0 > rows {
            out.push(error(
                codes::PROFILE_COUNTS,
                format!("{member} {} exceeds the {rows} rows seen", count.0),
                at(member),
            ));
        }
    }
    if profile.distinct_count.is_some() != profile.distinct_precision.is_some() {
        out.push(error(
            codes::PROFILE_COUNTS,
            "distinctPrecision is present exactly when distinctCount is".to_owned(),
            at("distinctPrecision"),
        ));
    }
    if let Some(samples) = &profile.samples {
        let max = samples.policy.max_values.0;
        let kept = u64::try_from(samples.values.len()).unwrap_or(u64::MAX);
        let message = if profile.field.is_none() {
            Some("samples are a field measure, but the profile measures a dataset".to_owned())
        } else if max > MAX_SAMPLE_VALUES {
            Some(format!(
                "the sample policy keeps {max} values; at most {MAX_SAMPLE_VALUES} are admitted"
            ))
        } else if kept > max {
            Some(format!("{kept} sample values exceed the policy's {max}"))
        } else if samples.policy.redaction == Redaction::Withheld && kept > 0 {
            Some("a withheld sample policy keeps no values".to_owned())
        } else {
            None
        };
        if let Some(message) = message {
            out.push(error(codes::SAMPLE_BOUND, message, at("samples")));
        }
    }
    check_evidence_list(&profile.evidence, "profiles", index, out);
}

fn check_code_set(
    document: &SourceSchemaIr,
    code_set: &CodeSet,
    index: usize,
    datasets: &BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    let at = |member: &str| member_at("codeSets", index, member);
    if code_set.origin == Origin::Inferred {
        out.push(error(
            codes::PROMOTED_INFERENCE,
            format!(
                "code set {:?} is inferred; a code set is declared or an observed_candidate",
                code_set.id
            ),
            at("origin"),
        ));
    }
    if known_dataset(&code_set.dataset, datasets, at("dataset"), out)
        && document.field(&code_set.dataset, &code_set.field).is_none()
    {
        out.push(unknown_field(
            &code_set.dataset,
            &code_set.field,
            at("field"),
        ));
    }
    if code_set.values.is_empty() {
        out.push(error(
            codes::DUPLICATE_CODE,
            format!("code set {:?} lists no value", code_set.id),
            at("values"),
        ));
    }
    let mut seen = BTreeSet::new();
    for (position, value) in code_set.values.iter().enumerate() {
        if !seen.insert(value.value.as_str()) {
            out.push(error(
                codes::DUPLICATE_CODE,
                format!("code set {:?} lists {:?} twice", code_set.id, value.value),
                Span::from_segments([
                    "codeSets",
                    &index.to_string(),
                    "values",
                    &position.to_string(),
                    "value",
                ]),
            ));
        }
    }
}

/// Report a blank value; returns whether it was blank.
fn blank(value: &str, at: Span, out: &mut Vec<Diagnostic>) -> bool {
    let is_blank = value.trim().is_empty();
    if is_blank {
        out.push(error(
            codes::BLANK_VALUE,
            "the value must not be blank".to_owned(),
            at,
        ));
    }
    is_blank
}

fn check_id(
    id: &str,
    list: &str,
    index: usize,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<Diagnostic>,
) {
    let at = member_at(list, index, "id");
    if blank(id, at.clone(), out) {
        return;
    }
    if !seen.insert(id.to_owned()) {
        out.push(error(
            codes::DUPLICATE_ID,
            format!("{list} declares id {id:?} twice"),
            at,
        ));
    }
}

/// Report an undeclared dataset; returns whether it is declared.
fn known_dataset(
    dataset: &str,
    datasets: &BTreeSet<String>,
    at: Span,
    out: &mut Vec<Diagnostic>,
) -> bool {
    let known = datasets.contains(dataset);
    if !known {
        out.push(error(
            codes::UNKNOWN_DATASET,
            format!("dataset {dataset:?} is not declared"),
            at,
        ));
    }
    known
}

fn known_fields(
    document: &SourceSchemaIr,
    dataset: &str,
    fields: &[String],
    pointer: &[&str],
    out: &mut Vec<Diagnostic>,
) {
    for (position, field) in fields.iter().enumerate() {
        if document.field(dataset, field).is_none() {
            let position = position.to_string();
            let segments = pointer.iter().copied().chain([position.as_str()]);
            out.push(unknown_field(dataset, field, Span::from_segments(segments)));
        }
    }
}

fn unknown_field(dataset: &str, field: &str, at: Span) -> Diagnostic {
    error(
        codes::UNKNOWN_FIELD,
        format!("dataset {dataset:?} has no field {field:?}"),
        at,
    )
}

fn timestamp(value: &str, at: Span, out: &mut Vec<Diagnostic>) {
    if !valid_timestamp(value, TimezonePolicy::Offset) {
        out.push(error(
            codes::INVALID_TIMESTAMP,
            format!("{value:?} is not an RFC 3339 timestamp with an explicit offset"),
            at,
        ));
    }
}

fn check_evidence(evidence: &EvidenceRef, at: Span, out: &mut Vec<Diagnostic>) {
    let hex = evidence.sha256.len() == 64
        && evidence
            .sha256
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'));
    if evidence.uri.trim().is_empty() || !hex {
        out.push(error(
            codes::INVALID_EVIDENCE,
            format!(
                "evidence {:?} needs a non-blank uri and a lower-case hex SHA-256",
                evidence.uri
            ),
            at,
        ));
    }
}

fn check_evidence_list(
    evidence: &[EvidenceRef],
    list: &str,
    index: usize,
    out: &mut Vec<Diagnostic>,
) {
    for (position, reference) in evidence.iter().enumerate() {
        check_evidence(
            reference,
            Span::from_segments([list, &index.to_string(), "evidence", &position.to_string()]),
            out,
        );
    }
}
