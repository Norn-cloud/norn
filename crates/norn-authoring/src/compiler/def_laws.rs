//! Law (`Norn.law` v1 & v2), exception-tree, and timing lowering.
//!
//! Ports `law`, `lawV2`, `exceptions`, `lawTiming`, `lawBody`, and `lowerLaw`
//! from `tools/norn-ts/src/compiler/definitions.ts` and `builder/law.ts`.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_snake_ident;
use super::def_helpers::{
    ANY_TYPE, DefinitionCompiler, DefinitionSource, INTERFACE, LabelTarget, LoweredDef, Members,
    OBJECT_TYPES, SubjectSchema,
};
use super::def_literals::{build_label_entries, build_labels_artifact};
use super::def_norms::ExceptionTreeSpec;
use super::expr::{TemporalKind, narrow_subject_schema};
use super::types::{
    CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope, is_string_literal_like,
};
use super::value_schema::{ExpansionBudget, ObjectSchema, ValueSchema};

struct LawV2Subject {
    subject: String,
    owner: String,
    version: String,
    self_schema: SubjectSchema,
    deps: Vec<Value>,
}

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_law(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let args = call.as_call_expression().arguments().nodes();
        if let Some(&first_arg) = args.first()
            && is_string_literal_like(self.deref(checker, first_arg)?)
        {
            return self.compile_law_v2(checker, expansion, call, options);
        }
        let members = self.meta(
            checker,
            call,
            &[
                "id",
                "title",
                "version",
                "law",
                "dependencies",
                "label",
                "localizedDescription",
                "needsReview",
            ],
        )?;
        let id = self.string(checker, Self::member(&members, "id", call)?)?;
        let title = self.string(checker, Self::member(&members, "title", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        let localized = self.localized(checker, &members)?;
        let law_body = self.compile_law_body(
            checker,
            expansion,
            Self::member(&members, "law", call)?,
            &id,
        )?;
        let deps_val = self.json(checker, Self::member(&members, "dependencies", call)?, 0)?;
        let deps = deps_val.as_array().cloned().unwrap_or_default();
        let law_id = law_body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_owned();
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope(
            EnvelopeMeta {
                kind: "law".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: deps,
            },
            law_body,
            options,
        );
        Self::assert_expansion(expansion, &artifact, call)?;
        let entries = build_label_entries(&LabelTarget::Law(law_id), &localized, None);
        let labels = build_labels_artifact(&artifact, entries, options);
        Ok(LoweredDef {
            artifact,
            labels,
            at: None,
        })
    }

    fn compile_law_v2(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let args = call.as_call_expression().arguments().nodes();
        let (Some(&name_arg), Some(&spec_arg)) = (args.first(), args.get(1)) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a law takes its name and one spec object",
                call,
            ));
        };
        if args.len() != 2 {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a law takes its name and one spec object",
                call,
            ));
        }
        let name = self.string(checker, name_arg)?;
        if !is_snake_ident(&name) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("law name {name:?} must be snake_case"),
                name_arg,
            ));
        }
        let spec = self.closed_record(
            checker,
            spec_arg,
            &[
                "on",
                "inverses",
                "when",
                "must",
                "never",
                "eventually",
                "within",
                "severity",
                "because",
                "id",
                "unless",
            ],
            &["on", "because"],
        )?;
        let subj = self.resolve_law_v2_subject(checker, expansion, &spec, spec_arg)?;
        let law_id = match spec.get("id") {
            Some(i) => self.string(checker, i)?,
            None => format!("{}.{name}", subj.subject),
        };
        let artifact_id = format!("{law_id}.law");
        let (rationale, law_ir) = self.compile_law_v2_body(
            checker,
            expansion,
            &spec,
            spec_arg,
            &LawV2Target {
                subj: &subj,
                law_id: &law_id,
                artifact_id: &artifact_id,
                source_path: &options.source_path,
            },
        )?;
        let namespace = artifact_id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope(
            EnvelopeMeta {
                kind: "law".to_owned(),
                artifact_id,
                namespace,
                name: rationale,
                version: subj.version,
                dependencies: subj.deps,
            },
            law_ir,
            options,
        );
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: None,
        })
    }

    fn compile_law_v2_body(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: &Members,
        spec_arg: P<Node>,
        target: &LawV2Target<'_>,
    ) -> Result<(String, Value), CompileError> {
        let subj = target.subj;
        let law_id = target.law_id;
        let artifact_id = target.artifact_id;
        let modality = read_law_modality(spec, spec_arg)?;
        let severity = self.read_law_severity(checker, spec)?;
        let timing = match spec.get("within") {
            Some(w) => Some(self.compile_law_timing(checker, w, &subj.self_schema)?),
            None => None,
        };
        let mut condition: Option<String> = None;
        let mut response_self = subj.self_schema.clone();
        if let Some(when_node) = spec.get("when") {
            self.source_map.push(DefinitionSource {
                artifact_id: artifact_id.to_owned(),
                pointer: "/payload/condition".to_owned(),
                at: self.at(when_node),
            });
            let (expr, zen) = self.lambda_expr(checker, expansion, when_node, &subj.self_schema)?;
            condition = Some(zen);
            response_self = SubjectSchema {
                schema: narrow_subject_schema(&expr, &subj.self_schema.schema),
                temporal: subj.self_schema.temporal.clone(),
            };
        }
        let exceptions = match spec.get("unless") {
            Some(u) => self.compile_exceptions(
                checker,
                expansion,
                u,
                &ExceptionTreeSpec {
                    law_id,
                    artifact_id,
                    base_pointer: "/payload/exceptions",
                    self_schema: &response_self,
                },
            )?,
            None => Vec::new(),
        };
        let resp_node = Self::member(spec, modality, spec_arg)?;
        self.source_map.push(DefinitionSource {
            artifact_id: artifact_id.to_owned(),
            pointer: "/payload/response/expression".to_owned(),
            at: self.at(resp_node),
        });
        let resp_expr = self.lambda(checker, expansion, resp_node, &response_self)?;
        let rationale = self.string(checker, Self::member(spec, "because", spec_arg)?)?;
        let law_ir = build_law_v2_payload(
            subj,
            LawV2Fields {
                law_id,
                severity: &severity,
                rationale: &rationale,
                modality,
                condition,
                timing,
                resp_expr,
                exceptions,
                source_path: target.source_path,
            },
        );
        Ok((rationale, law_ir))
    }

    fn resolve_law_v2_subject(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: &Members,
        spec_arg: P<Node>,
    ) -> Result<LawV2Subject, CompileError> {
        let on = Self::member(spec, "on", spec_arg)?;
        let (subject_call, subject_members) = self.referenced(
            checker,
            on,
            ANY_TYPE,
            "a Norn.type or Norn.interfaceType definition",
        )?;
        let subject = self.string(
            checker,
            Self::member(&subject_members, "name", subject_call)?,
        )?;
        let Some((owner, _)) = subject.rsplit_once('.') else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "law subject {subject:?} is not a qualified type name (namespace.Type), so the law has no owner"
                ),
                on,
            ));
        };
        if owner.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "law subject {subject:?} is not a qualified type name (namespace.Type), so the law has no owner"
                ),
                on,
            ));
        }
        let version = self.string(
            checker,
            Self::member(&subject_members, "version", subject_call)?,
        )?;
        let mut seen = HashSet::new();
        let self_base = self.subject_schema(
            checker,
            expansion,
            subject_call,
            &subject_members,
            true,
            &mut seen,
        )?;
        let suffix = if self.call_id(checker, subject_call).as_deref() == Some(INTERFACE) {
            "interface"
        } else {
            "entity"
        };
        let mut dep = Map::new();
        dep.insert(
            "artifactId".to_owned(),
            Value::String(format!("{subject}.{suffix}")),
        );
        dep.insert("kind".to_owned(), Value::String("entity".to_owned()));
        dep.insert("version".to_owned(), Value::String(version.clone()));
        dep.insert("required".to_owned(), Value::Bool(true));
        let mut deps = vec![Value::Object(dep)];

        let mut self_props = match self_base.schema {
            ValueSchema::Object(obj) => obj.properties,
            ValueSchema::String { .. }
            | ValueSchema::Number { .. }
            | ValueSchema::Integer { .. }
            | ValueSchema::Boolean { .. }
            | ValueSchema::Null { .. }
            | ValueSchema::JsonValue { .. }
            | ValueSchema::Array { .. }
            | ValueSchema::Nullable(_) => BTreeMap::new(),
        };
        if let Some(inv_node) = spec.get("inverses") {
            self.collect_law_inverses(
                checker,
                expansion,
                inv_node,
                &subject,
                &mut self_props,
                &mut deps,
            )?;
        }
        Ok(LawV2Subject {
            subject,
            owner: owner.to_owned(),
            version,
            self_schema: SubjectSchema {
                schema: ValueSchema::object(ObjectSchema::record(self_props, &[])),
                temporal: self_base.temporal,
            },
            deps,
        })
    }

    fn collect_law_inverses(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        inv_node: P<Node>,
        subject: &str,
        self_props: &mut BTreeMap<String, ValueSchema>,
        deps: &mut Vec<Value>,
    ) -> Result<(), CompileError> {
        for (role, source_node) in self.record(checker, inv_node)?.iter() {
            let (s_call, s_members) =
                self.referenced(checker, source_node, OBJECT_TYPES, "a Norn.type definition")?;
            let s_name = self.string(checker, Self::member(&s_members, "name", s_call)?)?;
            let s_ver = self.string(checker, Self::member(&s_members, "version", s_call)?)?;
            let mut found_link = false;
            if let Some(links_node) = s_members.get("links") {
                for l_node in self.list(checker, links_node)? {
                    let link_json = self.compile_link(checker, l_node)?;
                    if link_json.get("inverse").and_then(Value::as_str) == Some(role)
                        && link_json
                            .get("cardinality")
                            .and_then(|c| c.get("from"))
                            .and_then(Value::as_str)
                            == Some("many")
                        && link_json.get("target").and_then(Value::as_str) == Some(subject)
                    {
                        found_link = true;
                        break;
                    }
                }
            }
            if !found_link {
                return Err(CompileError::new(
                    "NORN-FLOW-SLICE",
                    format!(
                        "{s_name} declares no from: \"many\" link targeting {subject} with inverse {role:?}"
                    ),
                    source_node,
                ));
            }
            if self_props.contains_key(role) {
                return Err(CompileError::new(
                    "NORN-FLOW-SLICE",
                    format!("inverse role {role:?} collides with an existing member of {subject}"),
                    source_node,
                ));
            }
            let mut inner_seen = HashSet::new();
            let s_schema = self.subject_schema(
                checker,
                expansion,
                s_call,
                &s_members,
                false,
                &mut inner_seen,
            )?;
            self_props.insert(role.to_owned(), ValueSchema::array(s_schema.schema));
            let dep_id = format!("{s_name}.entity");
            if !deps
                .iter()
                .any(|d| d.get("artifactId").and_then(Value::as_str) == Some(dep_id.as_str()))
            {
                let mut dep = Map::new();
                dep.insert("artifactId".to_owned(), Value::String(dep_id));
                dep.insert("kind".to_owned(), Value::String("entity".to_owned()));
                dep.insert("version".to_owned(), Value::String(s_ver));
                dep.insert("required".to_owned(), Value::Bool(true));
                deps.push(Value::Object(dep));
            }
        }
        Ok(())
    }

    fn read_law_severity(
        &self,
        checker: &mut Checker,
        spec: &Members,
    ) -> Result<String, CompileError> {
        let Some(s_node) = spec.get("severity") else {
            return Ok("error".to_owned());
        };
        let s = self.string(checker, s_node)?;
        if s != "error" && s != "warning" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "severity is \"error\" or \"warning\"; override severity is engine gap §8.2",
                s_node,
            ));
        }
        Ok(s)
    }

    fn compile_law_timing(
        &self,
        checker: &mut Checker,
        input: P<Node>,
        self_schema: &SubjectSchema,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(checker, input, &["days", "hours", "before"], &[])?;
        if let Some(before) = m.get("before") {
            if m.len() != 1 {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "within takes either before or days/hours",
                    input,
                ));
            }
            return self.compile_law_before_deadline(checker, before, self_schema);
        }
        let mut total: u128 = 0;
        for (key, unit) in [("days", 86_400_000_u128), ("hours", 3_600_000_u128)] {
            if let Some(node) = m.get(key) {
                let val = self.deref(checker, node)?;
                let text = val.text();
                if val.kind() != Kind::NumericLiteral
                    || text.is_empty()
                    || !text.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("within.{key} is a whole number literal"),
                        node,
                    ));
                }
                let n = text.parse::<u128>().map_err(|_err| {
                    CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "within is longer than LawIR's u64 milliseconds",
                        input,
                    )
                })?;
                total = total.saturating_add(n.saturating_mul(unit));
            }
        }
        if total == 0 {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "within needs a positive days or hours",
                input,
            ));
        }
        if total > u128::from(u64::MAX) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "within is longer than LawIR's u64 milliseconds",
                input,
            ));
        }
        let mut obj = Map::new();
        obj.insert("kind".to_owned(), Value::String("within".to_owned()));
        obj.insert("millis".to_owned(), Value::String(total.to_string()));
        Ok(Value::Object(obj))
    }

    fn compile_law_before_deadline(
        &self,
        checker: &mut Checker,
        before: P<Node>,
        self_schema: &SubjectSchema,
    ) -> Result<Value, CompileError> {
        let fn_node = self.deref(checker, before)?;
        if fn_node.kind() != Kind::ArrowFunction {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "within.before is a bare read of a timestamp property (self => self.due_at); a computed deadline needs an IR expression form that does not exist yet",
                before,
            ));
        }
        let valid_read = if let (Some(p), Some(b)) =
            (fn_node.parameters().first().copied(), fn_node.body())
            && p.as_parameter_declaration().name().kind() == Kind::Identifier
            && b.kind() == Kind::PropertyAccessExpression
        {
            let pa = b.as_property_access_expression();
            if pa.expression().kind() == Kind::Identifier
                && pa.expression().text() == p.as_parameter_declaration().name().text()
            {
                Some((pa.name().text().to_owned(), pa.name()))
            } else {
                None
            }
        } else {
            None
        };
        let Some((prop, name_node)) = valid_read else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "within.before is a bare read of a timestamp property (self => self.due_at); a computed deadline needs an IR expression form that does not exist yet",
                before,
            ));
        };
        if self_schema.temporal.get(&prop).copied() != Some(TemporalKind::Timestamp) {
            return Err(CompileError::new(
                "NORN-FLOW-SLICE",
                format!(
                    "within.before reads {prop}, which is not a timestamp property of the subject"
                ),
                name_node,
            ));
        }
        let mut obj = Map::new();
        obj.insert(
            "kind".to_owned(),
            Value::String("before_deadline".to_owned()),
        );
        obj.insert("property".to_owned(), Value::String(prop));
        Ok(Value::Object(obj))
    }

    fn compile_law_body(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        artifact_id: &str,
    ) -> Result<Value, CompileError> {
        let members = self.record(checker, input)?;
        let mut out = Map::new();
        for (key, value) in members.iter() {
            if key == "__proto__" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "__proto__ is not a portable JSON member name",
                    value.parent().unwrap_or(value),
                ));
            }
            if key == "condition" {
                let e = self.deref(checker, value)?;
                let s = if is_string_literal_like(e) {
                    e.text().to_owned()
                } else {
                    self.zen_at(
                        checker,
                        expansion,
                        value,
                        artifact_id,
                        "/payload/condition",
                        "self",
                    )?
                };
                out.insert(key.to_owned(), Value::String(s));
            } else if key == "response"
                && self.deref(checker, value)?.kind() == Kind::ObjectLiteralExpression
            {
                let mut resp = Map::new();
                for (field, member) in self.record(checker, value)?.iter() {
                    if field == "__proto__" {
                        return Err(CompileError::new(
                            "NORN-FLOW-LITERAL",
                            "__proto__ is not a portable JSON member name",
                            member.parent().unwrap_or(member),
                        ));
                    }
                    let v = if field == "expression" {
                        let e = self.deref(checker, member)?;
                        let s = if is_string_literal_like(e) {
                            e.text().to_owned()
                        } else {
                            self.zen_at(
                                checker,
                                expansion,
                                member,
                                artifact_id,
                                "/payload/response/expression",
                                "self",
                            )?
                        };
                        Value::String(s)
                    } else {
                        self.json(checker, member, 0)?
                    };
                    resp.insert(field.to_owned(), v);
                }
                out.insert(key.to_owned(), Value::Object(resp));
            } else {
                out.insert(key.to_owned(), self.json(checker, value, 0)?);
            }
        }
        Ok(Value::Object(out))
    }
}

fn read_law_modality(spec: &Members, spec_arg: P<Node>) -> Result<&'static str, CompileError> {
    let modalities: Vec<&'static str> = ["must", "never", "eventually"]
        .into_iter()
        .filter(|k| spec.has(k))
        .collect();
    let Some(&modality) = modalities.first() else {
        return Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "a law takes exactly one of must, never or eventually",
            spec_arg,
        ));
    };
    if modalities.len() != 1 {
        return Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "a law takes exactly one of must, never or eventually",
            spec_arg,
        ));
    }
    Ok(modality)
}

struct LawV2Fields<'a> {
    law_id: &'a str,
    severity: &'a str,
    rationale: &'a str,
    modality: &'a str,
    condition: Option<String>,
    timing: Option<Value>,
    resp_expr: String,
    exceptions: Vec<Value>,
    source_path: &'a str,
}

fn build_law_v2_payload(subj: &LawV2Subject, f: LawV2Fields<'_>) -> Value {
    let mut prov = Map::new();
    prov.insert(
        "source".to_owned(),
        Value::String("authoring_sdk".to_owned()),
    );
    prov.insert(
        "sourcePath".to_owned(),
        Value::String(f.source_path.to_owned()),
    );
    let mut scope_root = Map::new();
    scope_root.insert("kind".to_owned(), Value::String("entity".to_owned()));
    scope_root.insert("id".to_owned(), Value::String(subj.subject.clone()));
    let mut scope = Map::new();
    scope.insert("root".to_owned(), Value::Object(scope_root));
    scope.insert(
        "objectTypes".to_owned(),
        Value::Array(vec![Value::String(subj.subject.clone())]),
    );
    let mut mod_obj = Map::new();
    mod_obj.insert(
        "kind".to_owned(),
        Value::String(
            if f.modality == "must" {
                "always"
            } else {
                f.modality
            }
            .to_owned(),
        ),
    );
    let mut resp_obj = Map::new();
    resp_obj.insert("kind".to_owned(), Value::String("predicate".to_owned()));
    resp_obj.insert("expression".to_owned(), Value::String(f.resp_expr));

    let mut law_ir = Map::new();
    law_ir.insert("lawVersion".to_owned(), Value::String("0.1".to_owned()));
    law_ir.insert("id".to_owned(), Value::String(f.law_id.to_owned()));
    law_ir.insert("owner".to_owned(), Value::String(subj.owner.clone()));
    law_ir.insert("severity".to_owned(), Value::String(f.severity.to_owned()));
    law_ir.insert(
        "rationale".to_owned(),
        Value::String(f.rationale.to_owned()),
    );
    law_ir.insert("provenance".to_owned(), Value::Object(prov));
    law_ir.insert("scope".to_owned(), Value::Object(scope));
    law_ir.insert("subject".to_owned(), Value::String(subj.subject.clone()));
    if let Some(cond) = f.condition {
        law_ir.insert("condition".to_owned(), Value::String(cond));
    }
    law_ir.insert("modality".to_owned(), Value::Object(mod_obj));
    if let Some(t) = f.timing {
        law_ir.insert("timing".to_owned(), t);
    }
    law_ir.insert("response".to_owned(), Value::Object(resp_obj));
    if !f.exceptions.is_empty() {
        law_ir.insert("exceptions".to_owned(), Value::Array(f.exceptions));
    }
    Value::Object(law_ir)
}

struct LawV2Target<'a> {
    subj: &'a LawV2Subject,
    law_id: &'a str,
    artifact_id: &'a str,
    source_path: &'a str,
}
