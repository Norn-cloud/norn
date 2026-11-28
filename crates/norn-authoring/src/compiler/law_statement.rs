//! FRET-style ontology-bound law statements and lossless sentence serialization.
//!
//! Faithful Rust port of `tools/norn-ts/src/law-statement.ts`.

use serde_json::{Map, Value};
use thiserror::Error;

use super::canonical::{canonical_json, compact_canonical_json};

const MAX_SENTENCE_BYTES: usize = 262_144;
const MAX_STATEMENT_BYTES: usize = 524_288;
const MAX_SLOT_DEPTH: usize = 64;

/// Error returned while converting, rendering, or parsing a [`LawStatement`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct StatementError(pub String);

impl StatementError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Sidecar metadata carried beside a [`LawStatement`] sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LawMetadata {
    /// `LawIR` schema version (`"0.1"`).
    pub law_version: String,
    /// Stable law identifier.
    pub id: String,
    /// Owning domain namespace.
    pub owner: String,
    /// Enforcement severity (`"error"` or `"warning"`).
    pub severity: String,
    /// Human-readable rationale.
    pub rationale: String,
    /// Provenance metadata object.
    pub provenance: Value,
}

impl LawMetadata {
    /// Converts metadata to canonical JSON object.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "lawVersion".to_owned(),
            Value::String(self.law_version.clone()),
        );
        map.insert("id".to_owned(), Value::String(self.id.clone()));
        map.insert("owner".to_owned(), Value::String(self.owner.clone()));
        map.insert("severity".to_owned(), Value::String(self.severity.clone()));
        map.insert(
            "rationale".to_owned(),
            Value::String(self.rationale.clone()),
        );
        map.insert("provenance".to_owned(), self.provenance.clone());
        Value::Object(map)
    }
}

/// FRET-style structured law statement paired with protected metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LawStatement {
    /// Scope JSON object (`LawScope`).
    pub scope: Value,
    /// Optional ZEN condition predicate.
    pub condition: Option<String>,
    /// Subject component (entity/interface name).
    pub component: String,
    /// Obligation keyword (`"shall"`).
    pub shall: String,
    /// Modality JSON object (`always`, `never`, `eventually`, or `until`).
    pub modality: Value,
    /// Optional timing JSON object (`within` or `before_deadline`).
    pub timing: Option<Value>,
    /// Response JSON object (`predicate`, `invariant`, `action`, or `transition`).
    pub response: Value,
    /// Sidecar metadata.
    pub metadata: LawMetadata,
}

/// Converts a `LawIR` JSON object (without exceptions) into a [`LawStatement`].
///
/// # Errors
///
/// Returns [`StatementError`] if the law carries `exceptions` or is missing required fields.
pub fn to_statement(law: &Value) -> Result<LawStatement, StatementError> {
    let obj = expect_object(law)?;
    if let Some(Value::Array(ex)) = obj.get("exceptions")
        && !ex.is_empty()
    {
        return Err(StatementError::new(
            "a law statement cannot carry exceptions; keep the LawIR form",
        ));
    }
    let metadata = LawMetadata {
        law_version: expect_string_field(obj, "lawVersion")?,
        id: expect_string_field(obj, "id")?,
        owner: expect_string_field(obj, "owner")?,
        severity: expect_string_field(obj, "severity")?,
        rationale: expect_string_field(obj, "rationale")?,
        provenance: obj
            .get("provenance")
            .cloned()
            .ok_or_else(|| StatementError::new("missing provenance"))?,
    };
    let scope = obj
        .get("scope")
        .cloned()
        .ok_or_else(|| StatementError::new("missing scope"))?;
    let component = expect_string_field(obj, "subject")?;
    let condition = match obj.get("condition") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(StatementError::new("condition must be a string")),
        None => None,
    };
    let modality = obj
        .get("modality")
        .cloned()
        .ok_or_else(|| StatementError::new("missing modality"))?;
    let timing = obj.get("timing").cloned();
    let response = obj
        .get("response")
        .cloned()
        .ok_or_else(|| StatementError::new("missing response"))?;
    Ok(LawStatement {
        scope,
        condition,
        component,
        shall: "shall".to_owned(),
        modality,
        timing,
        response,
        metadata,
    })
}

/// Converts a [`LawStatement`] back to its `LawIR` JSON object.
///
/// # Errors
///
/// Returns [`StatementError`] if `statement.shall != "shall"`.
pub fn from_statement(statement: &LawStatement) -> Result<Value, StatementError> {
    if statement.shall != "shall" {
        return Err(StatementError::new("the obligation word must be shall"));
    }
    let mut map = Map::new();
    map.insert(
        "lawVersion".to_owned(),
        Value::String(statement.metadata.law_version.clone()),
    );
    map.insert(
        "id".to_owned(),
        Value::String(statement.metadata.id.clone()),
    );
    map.insert(
        "owner".to_owned(),
        Value::String(statement.metadata.owner.clone()),
    );
    map.insert(
        "severity".to_owned(),
        Value::String(statement.metadata.severity.clone()),
    );
    map.insert(
        "rationale".to_owned(),
        Value::String(statement.metadata.rationale.clone()),
    );
    map.insert(
        "provenance".to_owned(),
        statement.metadata.provenance.clone(),
    );
    map.insert("scope".to_owned(), statement.scope.clone());
    map.insert(
        "subject".to_owned(),
        Value::String(statement.component.clone()),
    );
    if let Some(cond) = &statement.condition {
        map.insert("condition".to_owned(), Value::String(cond.clone()));
    }
    map.insert("modality".to_owned(), statement.modality.clone());
    if let Some(timing) = &statement.timing {
        map.insert("timing".to_owned(), timing.clone());
    }
    map.insert("response".to_owned(), statement.response.clone());
    Ok(Value::Object(map))
}

fn quote_json(value: &Value) -> Result<String, StatementError> {
    compact_canonical_json(value).map_err(|err| StatementError::new(err.to_string()))
}

/// Renders a [`LawStatement`] into its canonical FRET-style sentence.
///
/// # Errors
///
/// Returns [`StatementError`] if a slot contains malformed structure or non-canonical JSON.
pub fn render_sentence(statement: &LawStatement) -> Result<String, StatementError> {
    let scope_q = quote_json(&statement.scope)?;
    let cond_part = match &statement.condition {
        Some(cond) => format!("When {}, ", quote_json(&Value::String(cond.clone()))?),
        None => String::new(),
    };
    let comp_q = quote_json(&Value::String(statement.component.clone()))?;
    let mod_obj = expect_object(&statement.modality)?;
    let mod_kind = expect_string_field(mod_obj, "kind")?;
    let modality = if mod_kind == "until" {
        let pred = expect_string_field(mod_obj, "predicate")?;
        format!("until {}", quote_json(&Value::String(pred))?)
    } else {
        mod_kind
    };
    let resp_obj = expect_object(&statement.response)?;
    let resp_kind = expect_string_field(resp_obj, "kind")?;
    let behavior = match resp_kind.as_str() {
        "predicate" => {
            let expr = expect_string_field(resp_obj, "expression")?;
            format!("satisfy {}", quote_json(&Value::String(expr))?)
        }
        "action" => {
            let id = expect_string_field(resp_obj, "artifactId")?;
            format!("invoke {}", quote_json(&Value::String(id))?)
        }
        "invariant" => {
            let mut slot = Map::new();
            slot.insert(
                "objectType".to_owned(),
                Value::String(expect_string_field(resp_obj, "objectType")?),
            );
            slot.insert(
                "invariantId".to_owned(),
                Value::String(expect_string_field(resp_obj, "invariantId")?),
            );
            format!("satisfy invariant {}", quote_json(&Value::Object(slot))?)
        }
        _ => {
            let mut slot = Map::new();
            slot.insert(
                "objectType".to_owned(),
                Value::String(expect_string_field(resp_obj, "objectType")?),
            );
            slot.insert(
                "property".to_owned(),
                Value::String(expect_string_field(resp_obj, "property")?),
            );
            slot.insert(
                "transitionId".to_owned(),
                Value::String(expect_string_field(resp_obj, "transitionId")?),
            );
            format!("take transition {}", quote_json(&Value::Object(slot))?)
        }
    };
    let bound = match &statement.timing {
        None => String::new(),
        Some(timing) => {
            let t_obj = expect_object(timing)?;
            let t_kind = expect_string_field(t_obj, "kind")?;
            if t_kind == "within" {
                let millis = expect_string_field(t_obj, "millis")?;
                format!(" within {} ms", quote_json(&Value::String(millis))?)
            } else {
                let prop = expect_string_field(t_obj, "property")?;
                format!(" before deadline {}", quote_json(&Value::String(prop))?)
            }
        }
    };
    Ok(format!(
        "In {scope_q}, {cond_part}{comp_q} shall {modality} {behavior}{bound}"
    ))
}

struct Reader<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Result<Self, StatementError> {
        if text.len() > MAX_SENTENCE_BYTES {
            return Err(StatementError::new(
                "sentence exceeds the structural size bound",
            ));
        }
        Ok(Self { text, offset: 0 })
    }

    fn rest(&self) -> &'a str {
        self.text.get(self.offset..).unwrap_or("")
    }

    fn take(&mut self, token: &str) -> bool {
        if self.rest().starts_with(token) {
            self.offset = self.offset.saturating_add(token.len());
            true
        } else {
            false
        }
    }

    fn need(&mut self, token: &str) -> Result<(), StatementError> {
        if !self.take(token) {
            return Err(StatementError::new(format!(
                "expected {token} at {}",
                self.offset
            )));
        }
        Ok(())
    }

    fn atom(&mut self) -> Result<Value, StatementError> {
        let start = self.offset;
        let mut quoted = false;
        let mut escaped = false;
        let mut depth: usize = 0;
        let rest = self.rest();
        let mut consumed = 0_usize;
        for (rel_idx, ch) in rest.char_indices() {
            let next_rel = rel_idx.saturating_add(ch.len_utf8());
            if quoted {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quoted = false;
                    if depth == 0 {
                        consumed = next_rel;
                        break;
                    }
                }
            } else if ch == '"' {
                quoted = true;
            } else if ch == '{' || ch == '[' {
                depth = depth.saturating_add(1);
                if depth > MAX_SLOT_DEPTH {
                    return Err(StatementError::new("slot nesting exceeds 64"));
                }
            } else if ch == '}' || ch == ']' {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    consumed = next_rel;
                    break;
                }
            } else if depth == 0 {
                consumed = rel_idx;
                break;
            }
            consumed = next_rel;
        }
        self.offset = start.saturating_add(consumed);
        let slice = self.text.get(start..self.offset).unwrap_or("");
        serde_json::from_str(slice)
            .map_err(|_err| StatementError::new(format!("invalid JSON slot at {start}")))
    }

    fn string(&mut self) -> Result<String, StatementError> {
        match self.atom()? {
            Value::String(s) => Ok(s),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => Err(StatementError::new("slot must be a string")),
        }
    }

    fn end(&self) -> Result<(), StatementError> {
        if self.offset != self.text.len() {
            return Err(StatementError::new(format!(
                "unexpected sentence suffix at {}",
                self.offset
            )));
        }
        Ok(())
    }
}

fn expect_object(value: &Value) -> Result<&Map<String, Value>, StatementError> {
    match value {
        Value::Object(map) => Ok(map),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err(StatementError::new("slot must be a closed object"))
        }
    }
}

fn check_keys(
    obj: &Map<String, Value>,
    allowed: &[&str],
    required: &[&str],
) -> Result<(), StatementError> {
    if obj.keys().any(|k| !allowed.contains(&k.as_str()))
        || required.iter().any(|k| !obj.contains_key(*k))
    {
        return Err(StatementError::new("unknown or missing slot member"));
    }
    Ok(())
}

fn expect_string_field(obj: &Map<String, Value>, key: &str) -> Result<String, StatementError> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_),
        )
        | None => Err(StatementError::new(format!("{key} must be a string"))),
    }
}

fn expect_string_array(value: &Value) -> Result<(), StatementError> {
    let Value::Array(items) = value else {
        return Err(StatementError::new("slot must list strings"));
    };
    if !items.iter().all(|v| matches!(v, Value::String(_))) {
        return Err(StatementError::new("slot must list strings"));
    }
    Ok(())
}

fn validate_scope_slot(value: &Value) -> Result<(), StatementError> {
    let scope = expect_object(value)?;
    check_keys(
        scope,
        &["root", "objectTypes", "lifecycleStates", "commitments"],
        &["root", "objectTypes"],
    )?;
    let root = expect_object(
        scope
            .get("root")
            .ok_or_else(|| StatementError::new("missing root"))?,
    )?;
    check_keys(root, &["kind", "id"], &["kind", "id"])?;
    let root_kind = expect_string_field(root, "kind")?;
    if !matches!(root_kind.as_str(), "company" | "app" | "cell" | "entity") {
        return Err(StatementError::new("unknown scope root"));
    }
    let _ = expect_string_field(root, "id")?;
    if let Some(types) = scope.get("objectTypes") {
        expect_string_array(types)?;
    }
    if let Some(commitments) = scope.get("commitments") {
        expect_string_array(commitments)?;
    }
    if let Some(states) = scope.get("lifecycleStates") {
        let Value::Array(items) = states else {
            return Err(StatementError::new("lifecycleStates must be an array"));
        };
        for item in items {
            let state = expect_object(item)?;
            check_keys(
                state,
                &["objectType", "property", "states"],
                &["objectType", "property", "states"],
            )?;
            let _ = expect_string_field(state, "objectType")?;
            let _ = expect_string_field(state, "property")?;
            if let Some(st_arr) = state.get("states") {
                expect_string_array(st_arr)?;
            }
        }
    }
    Ok(())
}

/// Parses a FRET-style law sentence and pairs it with `metadata`.
///
/// # Errors
///
/// Returns [`StatementError`] if the sentence violates the published grammar.
pub fn parse_sentence(
    sentence: &str,
    metadata: LawMetadata,
) -> Result<LawStatement, StatementError> {
    let mut reader = Reader::new(sentence)?;
    reader.need("In ")?;
    let scope = reader.atom()?;
    validate_scope_slot(&scope)?;
    reader.need(", ")?;
    let condition = if reader.take("When ") {
        let c = reader.string()?;
        reader.need(", ")?;
        Some(c)
    } else {
        None
    };
    let component = reader.string()?;
    reader.need(" shall ")?;
    let modality = parse_modality(&mut reader)?;
    reader.need(" ")?;
    let response = parse_response(&mut reader)?;
    let timing = parse_timing(&mut reader)?;
    reader.end()?;
    Ok(LawStatement {
        scope,
        condition,
        component,
        shall: "shall".to_owned(),
        modality,
        timing,
        response,
        metadata,
    })
}

fn parse_modality(reader: &mut Reader<'_>) -> Result<Value, StatementError> {
    let mut map = Map::new();
    if reader.take("until ") {
        map.insert("kind".to_owned(), Value::String("until".to_owned()));
        map.insert("predicate".to_owned(), Value::String(reader.string()?));
    } else if reader.take("always") {
        map.insert("kind".to_owned(), Value::String("always".to_owned()));
    } else if reader.take("never") {
        map.insert("kind".to_owned(), Value::String("never".to_owned()));
    } else if reader.take("eventually") {
        map.insert("kind".to_owned(), Value::String("eventually".to_owned()));
    } else {
        return Err(StatementError::new("unsupported temporal word"));
    }
    Ok(Value::Object(map))
}

fn parse_response(reader: &mut Reader<'_>) -> Result<Value, StatementError> {
    let mut map = Map::new();
    if reader.take("satisfy invariant ") {
        let atom = reader.atom()?;
        let slot = expect_object(&atom)?;
        check_keys(
            slot,
            &["objectType", "invariantId"],
            &["objectType", "invariantId"],
        )?;
        map.insert("kind".to_owned(), Value::String("invariant".to_owned()));
        map.insert(
            "objectType".to_owned(),
            Value::String(expect_string_field(slot, "objectType")?),
        );
        map.insert(
            "invariantId".to_owned(),
            Value::String(expect_string_field(slot, "invariantId")?),
        );
    } else if reader.take("satisfy ") {
        map.insert("kind".to_owned(), Value::String("predicate".to_owned()));
        map.insert("expression".to_owned(), Value::String(reader.string()?));
    } else if reader.take("invoke ") {
        map.insert("kind".to_owned(), Value::String("action".to_owned()));
        map.insert("artifactId".to_owned(), Value::String(reader.string()?));
    } else if reader.take("take transition ") {
        let atom = reader.atom()?;
        let slot = expect_object(&atom)?;
        check_keys(
            slot,
            &["objectType", "property", "transitionId"],
            &["objectType", "property", "transitionId"],
        )?;
        map.insert("kind".to_owned(), Value::String("transition".to_owned()));
        map.insert(
            "objectType".to_owned(),
            Value::String(expect_string_field(slot, "objectType")?),
        );
        map.insert(
            "property".to_owned(),
            Value::String(expect_string_field(slot, "property")?),
        );
        map.insert(
            "transitionId".to_owned(),
            Value::String(expect_string_field(slot, "transitionId")?),
        );
    } else {
        return Err(StatementError::new("unsupported response slot"));
    }
    Ok(Value::Object(map))
}

fn parse_timing(reader: &mut Reader<'_>) -> Result<Option<Value>, StatementError> {
    if reader.take(" within ") {
        let millis = reader.string()?;
        reader.need(" ms")?;
        let mut map = Map::new();
        map.insert("kind".to_owned(), Value::String("within".to_owned()));
        map.insert("millis".to_owned(), Value::String(millis));
        return Ok(Some(Value::Object(map)));
    }
    if reader.take(" before deadline ") {
        let property = reader.string()?;
        let mut map = Map::new();
        map.insert(
            "kind".to_owned(),
            Value::String("before_deadline".to_owned()),
        );
        map.insert("property".to_owned(), Value::String(property));
        return Ok(Some(Value::Object(map)));
    }
    Ok(None)
}

/// Serializes a [`LawStatement`] into canonical wire JSON (`statementVersion: "0.1"`).
///
/// # Errors
///
/// Returns [`StatementError`] if rendering or canonicalization fails.
pub fn serialize_statement(statement: &LawStatement) -> Result<String, StatementError> {
    let sentence = render_sentence(statement)?;
    let mut wire = Map::new();
    wire.insert(
        "statementVersion".to_owned(),
        Value::String("0.1".to_owned()),
    );
    wire.insert("sentence".to_owned(), Value::String(sentence));
    wire.insert("metadata".to_owned(), statement.metadata.to_json());
    canonical_json(&Value::Object(wire)).map_err(|err| StatementError::new(err.to_string()))
}

/// Parses a wire JSON law statement back into a [`LawStatement`].
///
/// # Errors
///
/// Returns [`StatementError`] if size bounds, schema, metadata, or sentence grammar fail.
pub fn parse_statement(source: &str) -> Result<LawStatement, StatementError> {
    if source.len() > MAX_STATEMENT_BYTES {
        return Err(StatementError::new("statement exceeds size bound"));
    }
    let parsed: Value = serde_json::from_str(source)
        .map_err(|err| StatementError::new(format!("invalid JSON: {err}")))?;
    let wire = expect_object(&parsed)?;
    check_keys(
        wire,
        &["statementVersion", "sentence", "metadata"],
        &["statementVersion", "sentence", "metadata"],
    )?;
    if expect_string_field(wire, "statementVersion")? != "0.1" {
        return Err(StatementError::new("unsupported statement version"));
    }
    let meta_val = wire
        .get("metadata")
        .ok_or_else(|| StatementError::new("missing metadata"))?;
    let meta = expect_object(meta_val)?;
    check_keys(
        meta,
        &[
            "lawVersion",
            "id",
            "owner",
            "severity",
            "rationale",
            "provenance",
        ],
        &[
            "lawVersion",
            "id",
            "owner",
            "severity",
            "rationale",
            "provenance",
        ],
    )?;
    let law_version = expect_string_field(meta, "lawVersion")?;
    let id = expect_string_field(meta, "id")?;
    let owner = expect_string_field(meta, "owner")?;
    let severity = expect_string_field(meta, "severity")?;
    let rationale = expect_string_field(meta, "rationale")?;
    if law_version != "0.1" || !matches!(severity.as_str(), "error" | "warning") {
        return Err(StatementError::new("invalid law metadata"));
    }
    let prov_val = meta
        .get("provenance")
        .ok_or_else(|| StatementError::new("missing provenance"))?;
    let prov = expect_object(prov_val)?;
    check_keys(
        prov,
        &["source", "sourcePath", "actorId", "createdAt"],
        &["source"],
    )?;
    let prov_source = expect_string_field(prov, "source")?;
    if !matches!(
        prov_source.as_str(),
        "authoring_sdk" | "agent" | "migration" | "import" | "system"
    ) {
        return Err(StatementError::new("invalid provenance source"));
    }
    for key in ["sourcePath", "actorId", "createdAt"] {
        if prov.contains_key(key) {
            let _ = expect_string_field(prov, key)?;
        }
    }
    let metadata = LawMetadata {
        law_version,
        id,
        owner,
        severity,
        rationale,
        provenance: prov_val.clone(),
    };
    let sentence = expect_string_field(wire, "sentence")?;
    parse_sentence(&sentence, metadata)
}
