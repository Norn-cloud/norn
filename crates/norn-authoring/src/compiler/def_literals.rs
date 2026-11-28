//! Literal JSON/decimal readers, localized label builders, and source-schema
//! hash binding split out of [`super::def_helpers`].

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;
use unicode_normalization::UnicodeNormalization;

use crate::checker::resolve::node_text;

use super::canonical::{
    MAX_SAFE_INTEGER_I64, MIN_SAFE_INTEGER_I64, escaped_json_string_len, js_number_len,
};
use super::def_helpers::{
    DECISIONS, DefinitionCompiler, LabelTarget, LocalizedData, MAX_DATA_DEPTH, MemberLabelsData,
    Members, TypeBase,
};
use super::types::{
    CompileError, DependencyPin, EnvelopeMeta, EnvelopeOptions, build_envelope_checked,
    diagnostic_for_node, exact_number, is_string_literal_like,
};
use super::value_schema::{BudgetExceeded, ExpansionBudget};

impl DefinitionCompiler<'_> {
    fn charge_literal(
        expansion: &mut ExpansionBudget,
        work: usize,
        bytes: usize,
        node: P<Node>,
    ) -> Result<(), CompileError> {
        expansion
            .charge_literal(work, bytes)
            .map_err(|BudgetExceeded(msg)| CompileError::new("NORN-FLOW-UNBOUNDED", msg, node))
    }

    pub(crate) fn json(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        depth: usize,
    ) -> Result<Value, CompileError> {
        self.guard(input)?;
        if depth > MAX_DATA_DEPTH {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "literal data nesting exceeds the compiler bound",
                input,
            ));
        }
        Self::charge_literal(expansion, 1, 0, input)?;
        let e = self.deref(checker, input)?;
        if e.kind() == Kind::CallExpression
            && self.call_id(checker, e).as_deref() == Some("@norn/authoring#Norn.unsafeZen")
        {
            return self.json_unsafe_zen(checker, expansion, e, depth);
        }
        if is_string_literal_like(e) {
            let text = e.text();
            Self::charge_literal(expansion, 1, escaped_json_string_len(text), e)?;
            return Ok(Value::String(text.to_owned()));
        }
        self.json_atom_or_container(checker, expansion, e, depth)
    }

    fn json_unsafe_zen(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        e: P<Node>,
        depth: usize,
    ) -> Result<Value, CompileError> {
        if self.forbid_unsafe_zen {
            return Err(CompileError::new(
                "NORN-AUTHORING-UNSAFE-ZEN",
                "unsafeZen is forbidden (--forbid-unsafe-zen); replace it with a typed TypeScript decision callback",
                e,
            ));
        }
        self.warnings.push(diagnostic_for_node(
            e,
            "NORN-AUTHORING-UNSAFE-ZEN",
            "unsafeZen imports untyped ZEN/JDM; prefer a plain TypeScript decision callback",
        ));
        let Some(&a0) = e.as_call_expression().arguments().nodes().first() else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "unsafeZen requires a literal argument",
                e,
            ));
        };
        self.json(checker, expansion, a0, depth.saturating_add(1))
    }

    fn json_atom_or_container(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        e: P<Node>,
        depth: usize,
    ) -> Result<Value, CompileError> {
        let kind = e.kind();
        if kind == Kind::TrueKeyword {
            Self::charge_literal(expansion, 1, 4, e)?;
            return Ok(Value::Bool(true));
        }
        if kind == Kind::FalseKeyword {
            Self::charge_literal(expansion, 1, 5, e)?;
            return Ok(Value::Bool(false));
        }
        if kind == Kind::NullKeyword {
            Self::charge_literal(expansion, 1, 4, e)?;
            return Ok(Value::Null);
        }
        if kind == Kind::NumericLiteral {
            let Some(sf) = get_source_file_of_node(e) else {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "missing source file",
                    e,
                ));
            };
            let n = exact_number(sf, e, self.started, self.limits.max_millis, false)?;
            Self::charge_literal(expansion, 1, js_number_len(&n), e)?;
            return Ok(Value::Number(n));
        }
        if kind == Kind::PrefixUnaryExpression {
            let pu = e.as_prefix_unary_expression();
            if pu.operator == Kind::MinusToken && pu.operand.kind() == Kind::NumericLiteral {
                let Some(sf) = get_source_file_of_node(pu.operand) else {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "missing source file",
                        e,
                    ));
                };
                let n = exact_number(sf, pu.operand, self.started, self.limits.max_millis, true)?;
                Self::charge_literal(expansion, 1, js_number_len(&n), e)?;
                return Ok(Value::Number(n));
            }
        }
        if kind == Kind::ArrayLiteralExpression {
            Self::charge_literal(expansion, 1, 2, e)?;
            let mut arr = Vec::new();
            for item in self.list(checker, e)? {
                Self::charge_literal(expansion, 0, 2, item)?;
                arr.push(self.json(checker, expansion, item, depth.saturating_add(1))?);
            }
            return Ok(Value::Array(arr));
        }
        if kind == Kind::ObjectLiteralExpression {
            Self::charge_literal(expansion, 1, 2, e)?;
            let mut obj = Map::new();
            for (key, value) in self.record(checker, e)?.iter() {
                if key == "__proto__" {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "__proto__ is not a portable JSON member name",
                        value.parent().unwrap_or(value),
                    ));
                }
                Self::charge_literal(
                    expansion,
                    0,
                    escaped_json_string_len(key).saturating_add(4),
                    value,
                )?;
                obj.insert(
                    key.to_owned(),
                    self.json(checker, expansion, value, depth.saturating_add(1))?,
                );
            }
            return Ok(Value::Object(obj));
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "expected literal JSON data",
            e,
        ))
    }

    pub(crate) fn when_value(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let e = self.deref(checker, input)?;
        let (negative, literal) = if e.kind() == Kind::PrefixUnaryExpression
            && e.as_prefix_unary_expression().operator == Kind::MinusToken
        {
            (
                true,
                self.deref(checker, e.as_prefix_unary_expression().operand)?,
            )
        } else {
            (false, e)
        };
        if literal.kind() != Kind::NumericLiteral {
            return self.json(checker, expansion, input, 0);
        }
        let Some(sf) = get_source_file_of_node(literal) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "missing source file",
                literal,
            ));
        };
        let text: String = node_text(sf, literal)
            .chars()
            .filter(|&c| c != '_')
            .collect();
        if !is_unsigned_decimal_text(&text) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a numeric when.value is a plain decimal literal, such as 1000 or 1000.10 (no exponent or radix prefix)",
                literal,
            ));
        }
        let signed = if negative {
            format!("-{text}")
        } else {
            text.clone()
        };
        if text.contains('.') {
            return Ok(Value::String(signed));
        }
        let parsed = signed
            .parse::<i64>()
            .ok()
            .filter(|&v| (MIN_SAFE_INTEGER_I64..=MAX_SAFE_INTEGER_I64).contains(&v));
        let Some(val) = parsed else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "an integer when.value past 2^53 has no exact number; write it as decimal text",
                literal,
            ));
        };
        Ok(Value::Number(val.into()))
    }

    pub(crate) fn default_value(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let e = self.deref(checker, input)?;
        if is_string_literal_like(e) {
            return Ok(Value::String(e.text().to_owned()));
        }
        if e.kind() == Kind::TrueKeyword {
            return Ok(Value::Bool(true));
        }
        if e.kind() == Kind::FalseKeyword {
            return Ok(Value::Bool(false));
        }
        let (negative, literal) = if e.kind() == Kind::PrefixUnaryExpression
            && e.as_prefix_unary_expression().operator == Kind::MinusToken
        {
            (true, Self::unwrap(e.as_prefix_unary_expression().operand))
        } else {
            (false, e)
        };
        if literal.kind() == Kind::NumericLiteral {
            let Some(sf) = get_source_file_of_node(literal) else {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "missing source file",
                    literal,
                ));
            };
            let text = node_text(sf, literal);
            if !is_unsigned_decimal_text(&text) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "write a numeric default as a plain decimal literal (no exponent, separators, or radix prefix)",
                    literal,
                ));
            }
            return Ok(Value::String(if negative {
                format!("-{text}")
            } else {
                text
            }));
        }
        if e.kind() == Kind::NullKeyword {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a default cannot be null; omit it (an absent default already means no value)",
                e,
            ));
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "a default is a string, boolean, or decimal literal",
            e,
        ))
    }

    pub(crate) fn whole_positive_int(
        &self,
        checker: &mut Checker,
        input: P<Node>,
        label: &str,
    ) -> Result<u32, CompileError> {
        let e = self.deref(checker, input)?;
        if e.kind() != Kind::NumericLiteral {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{label} must be a positive whole integer literal"),
                e,
            ));
        }
        let text = e.text();
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{label} must be a positive whole integer literal"),
                e,
            ));
        }
        let parsed = text.parse::<u128>().map_err(|_err| {
            CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{label} must be between 1 and 4294967295"),
                e,
            )
        })?;
        if parsed < 1 || parsed > u128::from(u32::MAX) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{label} must be between 1 and 4294967295"),
                e,
            ));
        }
        u32::try_from(parsed).map_err(|_err| {
            CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{label} must be between 1 and 4294967295"),
                e,
            )
        })
    }

    pub(crate) fn string_map(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Map<String, Value>, CompileError> {
        let mut out = Map::new();
        for (key, value) in self.record(checker, input)?.iter() {
            if key == "__proto__" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "__proto__ is not a portable JSON member name",
                    value.parent().unwrap_or(value),
                ));
            }
            out.insert(key.to_owned(), Value::String(self.string(checker, value)?));
        }
        Ok(out)
    }

    pub(crate) fn locale_text(
        &self,
        checker: &mut Checker,
        node: P<Node>,
    ) -> Result<Vec<(String, String)>, CompileError> {
        let mut out = Vec::new();
        for (tag, text) in self.record(checker, node)?.iter() {
            if tag == "__proto__" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "__proto__ is not a portable JSON member name",
                    text.parent().unwrap_or(text),
                ));
            }
            out.push((tag.to_owned(), self.string(checker, text)?));
        }
        Ok(out)
    }

    pub(crate) fn localized(
        &self,
        checker: &mut Checker,
        members: &Members,
    ) -> Result<LocalizedData, CompileError> {
        let label = members.get("label");
        let desc = members.get("localizedDescription");
        let review = members.get("needsReview");
        if let Some(d) = desc
            && label.is_none()
        {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "localizedDescription needs a label: a description is shown under its label, in the same locales",
                d,
            ));
        }
        Ok(LocalizedData {
            label: match label {
                Some(n) => Some(self.locale_text(checker, n)?),
                None => None,
            },
            localized_description: match desc {
                Some(n) => Some(self.locale_text(checker, n)?),
                None => None,
            },
            needs_review: match review {
                Some(n) => Some(self.strings(checker, n)?),
                None => None,
            },
        })
    }

    pub(crate) fn member_labels(
        &self,
        checker: &mut Checker,
        members: &Members,
        properties: &[String],
    ) -> Result<MemberLabelsData, CompileError> {
        let prop_refs: Vec<&str> = properties.iter().map(String::as_str).collect();
        let property_labels = match members.get("propertyLabels") {
            Some(node) => {
                let mut entries = Vec::new();
                for (id, text) in self.closed_record(checker, node, &prop_refs, &[])?.iter() {
                    entries.push((id.to_owned(), self.locale_text(checker, text)?));
                }
                Some(entries)
            }
            None => None,
        };
        let value_labels = match members.get("valueLabels") {
            Some(node) => {
                let mut entries = Vec::new();
                for (id, values_node) in self.closed_record(checker, node, &prop_refs, &[])?.iter()
                {
                    let mut by_val = Vec::new();
                    for (val, text) in self.record(checker, values_node)?.iter() {
                        by_val.push((val.to_owned(), self.locale_text(checker, text)?));
                    }
                    entries.push((id.to_owned(), by_val));
                }
                Some(entries)
            }
            None => None,
        };
        Ok(MemberLabelsData {
            property_labels,
            value_labels,
        })
    }

    pub(crate) fn property_markings(
        &self,
        checker: &mut Checker,
        members: &Members,
        properties: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, CompileError> {
        let mut out = BTreeMap::new();
        if let Some(node) = members.get("propertyMarkings") {
            let prop_refs: Vec<&str> = properties.iter().map(String::as_str).collect();
            for (id, val) in self.closed_record(checker, node, &prop_refs, &[])?.iter() {
                out.insert(id.to_owned(), self.strings(checker, val)?);
            }
        }
        Ok(out)
    }

    pub(crate) fn decision_dependency(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<DependencyPin, CompileError> {
        let referenced = self.deref(checker, input)?;
        if referenced.kind() == Kind::CallExpression {
            let (call, members) =
                self.referenced(checker, input, DECISIONS, "a Norn.decision definition")?;
            let id = self.string(checker, Self::member(&members, "id", call)?)?;
            let artifact_id = match members.get("artifactId") {
                Some(a) => self.string(checker, a)?,
                None => format!("{id}.rule"),
            };
            let version = self.string(checker, Self::member(&members, "version", call)?)?;
            return Ok(DependencyPin {
                artifact_id,
                version,
                kind: Some("rule".to_owned()),
            });
        }
        let pin = self.closed_record(
            checker,
            input,
            &["artifactId", "version", "kind"],
            &["artifactId", "version"],
        )?;
        let kind = match pin.get("kind") {
            Some(k) => {
                let s = self.string(checker, k)?;
                if s != "entity" && s != "rule" {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "dependency kind must be entity or rule",
                        input,
                    ));
                }
                Some(s)
            }
            None => None,
        };
        Ok(DependencyPin {
            artifact_id: self.string(checker, Self::member(&pin, "artifactId", input)?)?,
            version: self.string(checker, Self::member(&pin, "version", input)?)?,
            kind,
        })
    }

    pub(crate) fn type_base(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        members: &Members,
    ) -> Result<TypeBase, CompileError> {
        let role = match members.get("role") {
            Some(node) => {
                let r = self.string(checker, node)?;
                if r != "thing" && r != "happening" {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "role must be thing or happening",
                        node,
                    ));
                }
                Some(r)
            }
            None => None,
        };
        let description = match members.get("description") {
            Some(d) => Some(self.string(checker, d)?),
            None => None,
        };
        let markings = match members.get("markings") {
            Some(m) => self.strings(checker, m)?,
            None => Vec::new(),
        };
        let mut dependencies = Vec::new();
        if let Some(deps_node) = members.get("dependencies") {
            for pin in self.list(checker, deps_node)? {
                dependencies.push(self.decision_dependency(checker, pin)?);
            }
        }
        let props_node = Self::member(members, "properties", call)?;
        let schema = self.schemas.read(checker, props_node)?;
        let projected = self.schemas.properties(&schema, expansion)?;
        Ok(TypeBase {
            name: self.string(checker, Self::member(members, "name", call)?)?,
            title: self.string(checker, Self::member(members, "title", call)?)?,
            version: self.string(checker, Self::member(members, "version", call)?)?,
            role,
            description,
            markings,
            dependencies,
            projected,
        })
    }

    pub(crate) fn assert_expansion(
        expansion: &mut ExpansionBudget,
        val: &Value,
        node: P<Node>,
    ) -> Result<(), CompileError> {
        expansion
            .assert_value(val)
            .map_err(|BudgetExceeded(msg)| CompileError::new("NORN-FLOW-UNBOUNDED", msg, node))
    }
}

/// Checks `DECIMAL_TEXT` (`/^(0|[1-9][0-9]*)(\.[0-9]+)?$/`).
#[must_use]
pub(crate) fn is_unsigned_decimal_text(text: &str) -> bool {
    let (int_part, frac_part) = match text.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (text, None),
    };
    let int_ok = if int_part == "0" {
        true
    } else {
        let mut bytes = int_part.bytes();
        bytes.next().is_some_and(|b| (b'1'..=b'9').contains(&b))
            && bytes.all(|b| b.is_ascii_digit())
    };
    let frac_ok = match frac_part {
        None => true,
        Some(f) => !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
    };
    int_ok && frac_ok
}

fn nfc_map(entries: &[(String, String)]) -> Value {
    let mut map = Map::new();
    for (tag, text) in entries {
        let normalized: String = text.nfc().collect();
        map.insert(tag.clone(), Value::String(normalized));
    }
    Value::Object(map)
}

pub(crate) fn build_label_entry(
    target: Value,
    label: &[(String, String)],
    description: Option<&[(String, String)]>,
    needs_review: &[String],
) -> Value {
    let mut review = Map::new();
    for tag in needs_review {
        let tag_lower = tag.to_ascii_lowercase();
        if label
            .iter()
            .any(|(k, _)| k.to_ascii_lowercase() == tag_lower)
        {
            review.insert(tag.clone(), Value::String("needs_review".to_owned()));
        }
    }
    let mut obj = Map::new();
    obj.insert("target".to_owned(), target);
    obj.insert("label".to_owned(), nfc_map(label));
    if let Some(desc) = description {
        obj.insert("description".to_owned(), nfc_map(desc));
    }
    if !review.is_empty() {
        obj.insert("review".to_owned(), Value::Object(review));
    }
    Value::Object(obj)
}

pub(crate) fn build_label_entries(
    target: &LabelTarget,
    localized: &LocalizedData,
    member_labels: Option<&MemberLabelsData>,
) -> Vec<Value> {
    let needs_review = localized.needs_review.as_deref().unwrap_or(&[]);
    let mut entries = Vec::new();
    let own_target = match target {
        LabelTarget::ObjectType(name) => {
            let mut m = Map::new();
            m.insert("kind".to_owned(), Value::String("objectType".to_owned()));
            m.insert("objectType".to_owned(), Value::String(name.clone()));
            Value::Object(m)
        }
        LabelTarget::Action(id) => {
            let mut m = Map::new();
            m.insert("kind".to_owned(), Value::String("action".to_owned()));
            m.insert("actionId".to_owned(), Value::String(id.clone()));
            Value::Object(m)
        }
        LabelTarget::Rule(id) => {
            let mut m = Map::new();
            m.insert("kind".to_owned(), Value::String("rule".to_owned()));
            m.insert("ruleId".to_owned(), Value::String(id.clone()));
            Value::Object(m)
        }
        LabelTarget::Law(id) => {
            let mut m = Map::new();
            m.insert("kind".to_owned(), Value::String("law".to_owned()));
            m.insert("lawId".to_owned(), Value::String(id.clone()));
            Value::Object(m)
        }
    };
    if let Some(label) = &localized.label {
        entries.push(build_label_entry(
            own_target,
            label,
            localized.localized_description.as_deref(),
            needs_review,
        ));
    }
    if let (LabelTarget::ObjectType(object_type), Some(ml)) = (target, member_labels) {
        if let Some(prop_labels) = &ml.property_labels {
            for (prop, label) in prop_labels {
                let mut m = Map::new();
                m.insert("kind".to_owned(), Value::String("property".to_owned()));
                m.insert("objectType".to_owned(), Value::String(object_type.clone()));
                m.insert("property".to_owned(), Value::String(prop.clone()));
                entries.push(build_label_entry(
                    Value::Object(m),
                    label,
                    None,
                    needs_review,
                ));
            }
        }
        if let Some(val_labels) = &ml.value_labels {
            for (prop, values) in val_labels {
                for (val, label) in values {
                    let mut m = Map::new();
                    m.insert("kind".to_owned(), Value::String("enumValue".to_owned()));
                    m.insert("objectType".to_owned(), Value::String(object_type.clone()));
                    m.insert("property".to_owned(), Value::String(prop.clone()));
                    m.insert("value".to_owned(), Value::String(val.clone()));
                    entries.push(build_label_entry(
                        Value::Object(m),
                        label,
                        None,
                        needs_review,
                    ));
                }
            }
        }
    }
    entries
}

/// Emits `<owner>.labels` when `entries` is non-empty (`lowerLabels` in `builder/labels.ts`).
pub(crate) fn build_labels_artifact(
    owner_artifact: &Value,
    entries: Vec<Value>,
    options: &EnvelopeOptions,
    at: P<Node>,
) -> Result<Option<Value>, CompileError> {
    if entries.is_empty() {
        return Ok(None);
    }
    let artifact_id = owner_artifact
        .get("artifactId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let namespace = owner_artifact
        .get("namespace")
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = owner_artifact
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = owner_artifact
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut payload = Map::new();
    payload.insert("labelsVersion".to_owned(), Value::String("0.1".to_owned()));
    payload.insert("entries".to_owned(), Value::Array(entries));
    let artifact = build_envelope_checked(
        EnvelopeMeta {
            kind: "labels".to_owned(),
            artifact_id: format!("{artifact_id}.labels"),
            namespace: namespace.to_owned(),
            name: format!("{name} labels"),
            version: version.to_owned(),
            dependencies: Vec::new(),
        },
        Value::Object(payload),
        options,
        at,
    )?;
    Ok(Some(artifact))
}
