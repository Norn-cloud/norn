//! Model (`Norn.model`) and relational transform (`Norn.transform`) lowering
//! split out of [`super::def_catalog`].

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tsrs_ast::Node;
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_dotted_lower_id;
use super::def_helpers::{DefinitionCompiler, LoweredDef, Members, OBJECT_TYPES};
use super::def_literals::is_unsigned_decimal_text;
use super::types::{CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope_checked};
use super::value_schema::ExpansionBudget;

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_model(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
            &[
                "id",
                "title",
                "version",
                "input",
                "features",
                "target",
                "outputs",
                "trust",
                "contracts",
                "predictions",
                "method",
            ],
        )?;
        let id = self.string(checker, Self::member(&m, "id", call)?)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("model id {id:?} must be dotted snake_case segments (sales.churn_model)"),
                call,
            ));
        }
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let payload =
            self.build_model_payload(checker, expansion, &m, call, &id, &title, &version)?;
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "model".to_owned(),
                artifact_id: format!("{id}.model"),
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: Some(self.at(call)),
        })
    }

    fn build_model_payload(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        m: &Members,
        call: P<Node>,
        id: &str,
        title: &str,
        version: &str,
    ) -> Result<Map<String, Value>, CompileError> {
        let trust = self.read_model_trust(checker, Self::member(m, "trust", call)?)?;
        let method_obj = self.read_model_method(checker, Self::member(m, "method", call)?)?;
        let outputs_schema = self
            .schemas
            .read(checker, Self::member(m, "outputs", call)?)?;
        let outputs: Vec<Value> = self
            .schemas
            .properties(&outputs_schema, expansion)?
            .into_iter()
            .map(|p| {
                let mut o = Map::new();
                o.insert("id".to_owned(), Value::String(p.id));
                o.insert("type".to_owned(), p.projection.prop_type.to_json());
                o.insert("nullable".to_owned(), Value::Bool(p.projection.nullable));
                Value::Object(o)
            })
            .collect();

        let mut input_obj = Map::new();
        input_obj.insert("kind".to_owned(), Value::String("objectType".to_owned()));
        input_obj.insert(
            "objectType".to_owned(),
            Value::String(self.type_name(
                checker,
                Self::member(m, "input", call)?,
                OBJECT_TYPES,
            )?),
        );

        let mut payload = Map::new();
        payload.insert("modelId".to_owned(), Value::String(id.to_owned()));
        payload.insert("title".to_owned(), Value::String(title.to_owned()));
        payload.insert("version".to_owned(), Value::String(version.to_owned()));
        payload.insert("input".to_owned(), Value::Object(input_obj));
        payload.insert(
            "features".to_owned(),
            Value::Array(
                self.strings(checker, Self::member(m, "features", call)?)?
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
        if let Some(t) = m.get("target") {
            payload.insert("target".to_owned(), Value::String(self.string(checker, t)?));
        }
        payload.insert("outputs".to_owned(), Value::Array(outputs));
        payload.insert("trust".to_owned(), Value::String(trust));
        if let Some(c_node) = m.get("contracts") {
            let mut contracts = Vec::new();
            for item in self.list(checker, c_node)? {
                contracts.push(self.compile_model_contract(checker, item)?);
            }
            if !contracts.is_empty() {
                payload.insert("contracts".to_owned(), Value::Array(contracts));
            }
        }
        if let Some(preds) = m.get("predictions") {
            let mut po = Map::new();
            po.insert(
                "objectType".to_owned(),
                Value::String(self.type_name(checker, preds, OBJECT_TYPES)?),
            );
            payload.insert("predictions".to_owned(), Value::Object(po));
        }
        payload.insert("method".to_owned(), Value::Object(method_obj));
        Ok(payload)
    }

    fn read_model_trust(
        &self,
        checker: &mut Checker,
        trust_node: P<Node>,
    ) -> Result<String, CompileError> {
        let trust = self.string(checker, trust_node)?;
        if !matches!(
            trust.as_str(),
            "native" | "proven" | "contract_checked" | "opaque"
        ) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "trust is native, proven, contract_checked or opaque",
                trust_node,
            ));
        }
        Ok(trust)
    }

    fn read_model_method(
        &self,
        checker: &mut Checker,
        method_node: P<Node>,
    ) -> Result<Map<String, Value>, CompileError> {
        let method_rec =
            self.closed_record(checker, method_node, &["label", "version"], &["label"])?;
        let mut method_obj = Map::new();
        method_obj.insert(
            "label".to_owned(),
            Value::String(self.string(checker, Self::member(&method_rec, "label", method_node)?)?),
        );
        if let Some(mv) = method_rec.get("version") {
            method_obj.insert(
                "version".to_owned(),
                Value::String(self.string(checker, mv)?),
            );
        }
        Ok(method_obj)
    }

    fn compile_model_contract(
        &self,
        checker: &mut Checker,
        node: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            node,
            &["id", "output", "rule"],
            &["id", "output", "rule"],
        )?;
        let rule_node = Self::member(&m, "rule", node)?;
        let rule = self.closed_record(
            checker,
            rule_node,
            &["kind", "min", "max", "values"],
            &["kind"],
        )?;
        let kind_node = Self::member(&rule, "kind", rule_node)?;
        let kind = self.string(checker, kind_node)?;
        let check_only = |allowed: &[&str]| -> Result<(), CompileError> {
            for (k, v) in rule.iter() {
                if k != "kind" && !allowed.contains(&k) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("a {kind} rule has no member {k}"),
                        v,
                    ));
                }
            }
            Ok(())
        };
        let mut rule_obj = Map::new();
        rule_obj.insert("kind".to_owned(), Value::String(kind.clone()));
        match kind.as_str() {
            "range" => {
                check_only(&["min", "max"])?;
                for key in ["min", "max"] {
                    if let Some(v_node) = rule.get(key) {
                        let text = self.string(checker, v_node)?;
                        let unsigned = text.strip_prefix('-').unwrap_or(&text);
                        if !is_unsigned_decimal_text(unsigned) {
                            return Err(CompileError::new(
                                "NORN-FLOW-LITERAL",
                                format!("range {key} is exact decimal text (\"0.5\")"),
                                v_node,
                            ));
                        }
                        rule_obj.insert(key.to_owned(), Value::String(text));
                    }
                }
            }
            "notNull" | "finite" => {
                check_only(&[])?;
            }
            "oneOf" => {
                check_only(&["values"])?;
                let vals = self.strings(checker, Self::member(&rule, "values", rule_node)?)?;
                rule_obj.insert(
                    "values".to_owned(),
                    Value::Array(vals.into_iter().map(Value::String).collect()),
                );
            }
            _ => {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a contract rule is range, notNull, finite or oneOf",
                    kind_node,
                ));
            }
        }
        let mut out = Map::new();
        out.insert(
            "id".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "id", node)?)?),
        );
        out.insert(
            "output".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "output", node)?)?),
        );
        out.insert("rule".to_owned(), Value::Object(rule_obj));
        Ok(Value::Object(out))
    }

    pub(crate) fn compile_transform(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
            &["id", "title", "version", "inputs", "nodes", "output"],
        )?;
        let id_node = Self::member(&m, "id", call)?;
        let id = self.string(checker, id_node)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "transform id {id:?} must be a dotted lower-case id (shop.revenue_by_region)"
                ),
                id_node,
            ));
        }
        let (inputs, deps) =
            self.compile_transform_inputs(checker, Self::member(&m, "inputs", call)?)?;
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let mut calc = Map::new();
        calc.insert("kind".to_owned(), Value::String("relational".to_owned()));
        calc.insert(
            "nodes".to_owned(),
            self.json(checker, expansion, Self::member(&m, "nodes", call)?, 0)?,
        );
        calc.insert(
            "output".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "output", call)?)?),
        );
        let mut payload = Map::new();
        payload.insert("viewVersion".to_owned(), Value::String("0.1".to_owned()));
        payload.insert("viewId".to_owned(), Value::String(id.clone()));
        payload.insert("inputs".to_owned(), Value::Array(inputs));
        payload.insert("calc".to_owned(), Value::Object(calc));
        payload.insert("outputSchema".to_owned(), Value::Object(Map::new()));
        payload.insert(
            "maintenance".to_owned(),
            Value::String("recompute_on_read".to_owned()),
        );
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "derived".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: deps,
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: None,
        })
    }

    fn compile_transform_inputs(
        &self,
        checker: &mut Checker,
        inputs_node: P<Node>,
    ) -> Result<(Vec<Value>, Vec<Value>), CompileError> {
        let mut inputs = Vec::new();
        let mut pins: BTreeMap<String, Value> = BTreeMap::new();
        let mut pin_order: Vec<String> = Vec::new();
        for (alias, node) in self.record(checker, inputs_node)?.iter() {
            if !is_lower_ident(alias) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("input alias {alias:?} must be a lower-case identifier"),
                    node.parent().unwrap_or(node),
                ));
            }
            let (t_name, t_ver) =
                self.type_ref(checker, node, OBJECT_TYPES, "a Norn.type definition")?;
            let artifact_id = format!("{t_name}.entity");
            let mut inp = Map::new();
            inp.insert("artifactId".to_owned(), Value::String(artifact_id.clone()));
            inp.insert("kind".to_owned(), Value::String("entity".to_owned()));
            inp.insert("version".to_owned(), Value::String(t_ver.clone()));
            inp.insert("alias".to_owned(), Value::String(alias.to_owned()));
            inputs.push(Value::Object(inp));

            let key = format!("{artifact_id}@{t_ver}");
            if !pins.contains_key(&key) {
                pin_order.push(key.clone());
                let mut dep = Map::new();
                dep.insert("artifactId".to_owned(), Value::String(artifact_id));
                dep.insert("kind".to_owned(), Value::String("entity".to_owned()));
                dep.insert("version".to_owned(), Value::String(t_ver));
                dep.insert("required".to_owned(), Value::Bool(true));
                pins.insert(key, Value::Object(dep));
            }
        }
        if inputs.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a transform reads at least one input",
                inputs_node,
            ));
        }
        let deps = pin_order
            .into_iter()
            .filter_map(|k| pins.remove(&k))
            .collect();
        Ok((inputs, deps))
    }
}

fn is_lower_ident(text: &str) -> bool {
    let mut bytes = text.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
