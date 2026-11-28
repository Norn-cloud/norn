//! Norm (`Norn.obligation`, `Norn.prohibition`, `Norn.permission`) and
//! example (`Norn.example`) lowering split out of [`super::def_laws`].

use std::collections::HashSet;

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node, SourceFile, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_snake_ident;
use super::def_helpers::{
    ANY_TYPE, CompiledExample, DefinitionCompiler, DefinitionSource, EXAMPLE, ExampleExpectation,
    LAW, Members, NORM_MEMBERS, OBJECT_TYPES, OBLIGATION, PROHIBITION, SubjectSchema, is_norm_call,
};
use super::types::{CompileError, is_string_literal_like};
use super::value_schema::ExpansionBudget;

pub(super) struct ExceptionTreeSpec<'a> {
    pub(super) law_id: &'a str,
    pub(super) artifact_id: &'a str,
    pub(super) base_pointer: &'a str,
    pub(super) self_schema: &'a SubjectSchema,
}

struct NormContext<'a> {
    subject: &'a str,
    owner: &'a str,
    artifact_id: &'a str,
    self_schema: &'a SubjectSchema,
    found: &'a [(P<Node>, String)],
}

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_norms(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        type_call: P<Node>,
        type_members: &Members,
    ) -> Result<Vec<Value>, CompileError> {
        let Some(sf) = get_source_file_of_node(type_call) else {
            return Ok(Vec::new());
        };
        let found = self.collect_module_norms(checker, sf, type_call)?;
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let subject = self.string(checker, Self::member(type_members, "name", type_call)?)?;
        let Some((owner, _)) = subject.rsplit_once('.') else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "norm subject {subject:?} is not a qualified type name (namespace.Type), so the norm has no owner"
                ),
                type_call,
            ));
        };
        if owner.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "norm subject {subject:?} is not a qualified type name (namespace.Type), so the norm has no owner"
                ),
                type_call,
            ));
        }
        let mut seen = HashSet::new();
        let self_schema =
            self.subject_schema(checker, expansion, type_call, type_members, true, &mut seen)?;
        let artifact_id = format!("{subject}.entity");
        let ctx = NormContext {
            subject: &subject,
            owner,
            artifact_id: &artifact_id,
            self_schema: &self_schema,
            found: &found,
        };
        let mut names = HashSet::new();
        let mut out = Vec::with_capacity(found.len());
        for (index, &(call, ref name)) in found.iter().enumerate() {
            let rule =
                self.compile_one_norm(checker, expansion, &ctx, index, call, name, &mut names)?;
            out.push(rule);
        }
        Ok(out)
    }

    fn collect_module_norms(
        &self,
        checker: &mut Checker,
        sf: P<SourceFile>,
        type_call: P<Node>,
    ) -> Result<Vec<(P<Node>, String)>, CompileError> {
        let mut found: Vec<(P<Node>, String)> = Vec::new();
        for &stmt in sf.as_node().statements() {
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            let decls = stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes();
            for &d in decls {
                let Some(raw_init) = d.as_variable_declaration().initializer() else {
                    continue;
                };
                let init = Self::unwrap(raw_init);
                if init.kind() != Kind::CallExpression {
                    continue;
                }
                let Some(id) = self.call_id(checker, init) else {
                    continue;
                };
                if !is_norm_call(&id) {
                    continue;
                }
                let args = init.as_call_expression().arguments().nodes();
                let Some(&arg1) = args.get(1) else {
                    continue;
                };
                let spec =
                    self.closed_record(checker, arg1, NORM_MEMBERS, &["on", "cite", "because"])?;
                let (target_call, _) = self.referenced(
                    checker,
                    Self::member(&spec, "on", arg1)?,
                    OBJECT_TYPES,
                    "a Norn.type definition",
                )?;
                if target_call != type_call {
                    continue;
                }
                if args.len() != 2 {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "a norm takes its name and one spec object",
                        init,
                    ));
                }
                let Some(&arg0) = args.first() else {
                    continue;
                };
                found.push((init, self.string(checker, arg0)?));
            }
        }
        Ok(found)
    }

    fn compile_one_norm(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        ctx: &NormContext<'_>,
        index: usize,
        call: P<Node>,
        name: &str,
        names: &mut HashSet<String>,
    ) -> Result<Value, CompileError> {
        let args = call.as_call_expression().arguments().nodes();
        let name_node = args.first().copied().unwrap_or(call);
        let spec_node = args.get(1).copied().unwrap_or(call);
        if !is_snake_ident(name) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("norm name {name:?} must be snake_case"),
                name_node,
            ));
        }
        if !names.insert(name.to_owned()) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("norm name {name:?} is declared twice on {}", ctx.subject),
                name_node,
            ));
        }
        let kind = self.call_id(checker, call).unwrap_or_default();
        let spec =
            self.closed_record(checker, spec_node, NORM_MEMBERS, &["on", "cite", "because"])?;
        let slot = format!("/payload/extensions/norn.normative~11.0.0/{index}");
        let (modality, member, law_modality) = norm_modality_info(&kind);
        let body_node = read_norm_body_member(&spec, spec_node, modality, member)?;
        let cite_node = Self::member(&spec, "cite", spec_node)?;
        let cite = self.closed_record(
            checker,
            cite_node,
            &["authority", "version"],
            &["authority", "version"],
        )?;
        let law_id = format!("{}.{name}", ctx.subject);
        let defeated_by = self.collect_norm_defeated_by(checker, &spec, ctx)?;
        let exc_pointer = format!("{slot}/law/exceptions");
        let exceptions = match spec.get("unless") {
            Some(u) => self.compile_exceptions(
                checker,
                expansion,
                u,
                &ExceptionTreeSpec {
                    law_id: &law_id,
                    artifact_id: ctx.artifact_id,
                    base_pointer: &exc_pointer,
                    self_schema: ctx.self_schema,
                },
            )?,
            None => Vec::new(),
        };
        let condition = match spec.get("when") {
            Some(w) => {
                self.source_map.push(DefinitionSource {
                    artifact_id: ctx.artifact_id.to_owned(),
                    pointer: format!("{slot}/law/condition"),
                    at: self.at(w),
                });
                Some(self.lambda(checker, expansion, w, ctx.self_schema)?)
            }
            None => None,
        };
        self.source_map.push(DefinitionSource {
            artifact_id: ctx.artifact_id.to_owned(),
            pointer: format!("{slot}/law/response/expression"),
            at: self.at(body_node),
        });
        let body_expr = self.lambda(checker, expansion, body_node, ctx.self_schema)?;
        let rationale = self.string(checker, Self::member(&spec, "because", spec_node)?)?;
        let authority_id = self.string(checker, Self::member(&cite, "authority", cite_node)?)?;
        let authority_ver = self.string(checker, Self::member(&cite, "version", cite_node)?)?;

        Ok(build_norm_rule_from_fields(
            ctx,
            NormRuleFields {
                law_id: &law_id,
                law_modality,
                modality,
                rationale: &rationale,
                condition,
                body_expr,
                exceptions,
                slot: &slot,
                authority_id,
                authority_ver,
                defeated_by,
            },
        ))
    }

    pub(super) fn compile_exceptions(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        spec: &ExceptionTreeSpec<'_>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut out: Vec<Value> = Vec::new();
        let mut used: HashSet<String> = HashSet::new();
        self.walk_exceptions(
            checker,
            expansion,
            input,
            spec.law_id,
            spec,
            &mut out,
            &mut used,
        )?;
        Ok(out)
    }

    fn walk_exceptions(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        node: P<Node>,
        overrides: &str,
        spec: &ExceptionTreeSpec<'_>,
        out: &mut Vec<Value>,
        used: &mut HashSet<String>,
    ) -> Result<(), CompileError> {
        for (label, entry) in self.record(checker, node)?.iter() {
            if !is_snake_ident(label) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("exception label {label:?} must be snake_case"),
                    entry,
                ));
            }
            if label == spec.law_id || !used.insert(label.to_owned()) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("exception label {label:?} is used twice in this law's tree"),
                    entry,
                ));
            }
            let m = self.closed_record(
                checker,
                entry,
                &["when", "because", "unless"],
                &["when", "because"],
            )?;
            let pointer = format!("{}/{}", spec.base_pointer, out.len());
            let when_node = Self::member(&m, "when", entry)?;
            self.source_map.push(DefinitionSource {
                artifact_id: spec.artifact_id.to_owned(),
                pointer: format!("{pointer}/condition"),
                at: self.at(when_node),
            });
            let cond = self.lambda(checker, expansion, when_node, spec.self_schema)?;
            let mut src = Map::new();
            src.insert(
                "artifactId".to_owned(),
                Value::String(spec.artifact_id.to_owned()),
            );
            src.insert("pointer".to_owned(), Value::String(pointer));
            let mut ex = Map::new();
            ex.insert("id".to_owned(), Value::String(label.to_owned()));
            ex.insert("overrides".to_owned(), Value::String(overrides.to_owned()));
            ex.insert("condition".to_owned(), Value::String(cond));
            ex.insert(
                "rationale".to_owned(),
                Value::String(self.string(checker, Self::member(&m, "because", entry)?)?),
            );
            ex.insert("source".to_owned(), Value::Object(src));
            out.push(Value::Object(ex));
            if let Some(nested) = m.get("unless") {
                self.walk_exceptions(checker, expansion, nested, label, spec, out, used)?;
            }
        }
        Ok(())
    }

    fn collect_norm_defeated_by(
        &self,
        checker: &mut Checker,
        spec: &Members,
        ctx: &NormContext<'_>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut defeated_by = Vec::new();
        let Some(yt_node) = spec.get("yieldsTo") else {
            return Ok(defeated_by);
        };
        for r in self.list(checker, yt_node)? {
            let target = self.deref(checker, r)?;
            if target.kind() != Kind::CallExpression {
                return Err(CompileError::new(
                    "NORN-FLOW-VALUE",
                    "yieldsTo lists norm references",
                    r,
                ));
            }
            let Some((_, target_name)) = ctx.found.iter().find(|(c, _)| *c == target) else {
                return Err(CompileError::new(
                    "NORN-FLOW-VALUE",
                    "yieldsTo names a norm over the same type declared in this module",
                    target,
                ));
            };
            defeated_by.push(Value::String(format!("{}.{target_name}", ctx.subject)));
        }
        Ok(defeated_by)
    }

    pub(super) fn compile_examples(
        &mut self,
        checker: &mut Checker,
        source: P<SourceFile>,
    ) -> Result<Vec<CompiledExample>, CompileError> {
        let mut out = Vec::new();
        let mut names = HashSet::new();
        for &stmt in source.as_node().statements() {
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            let decls = stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes();
            for &d in decls {
                let Some(raw_init) = d.as_variable_declaration().initializer() else {
                    continue;
                };
                let call = Self::unwrap(raw_init);
                if call.kind() != Kind::CallExpression
                    || self.call_id(checker, call).as_deref() != Some(EXAMPLE)
                {
                    continue;
                }
                let args = call.as_call_expression().arguments().nodes();
                let (Some(&a0), Some(&a1)) = (args.first(), args.get(1)) else {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "an example takes its name and one spec object",
                        call,
                    ));
                };
                if args.len() != 2 {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "an example takes its name and one spec object",
                        call,
                    ));
                }
                let name = self.string(checker, a0)?;
                if !names.insert(name.clone()) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("example {name:?} is declared twice"),
                        a0,
                    ));
                }
                let spec = self.closed_record(
                    checker,
                    a1,
                    &["of", "given", "expect"],
                    &["of", "given", "expect"],
                )?;
                let (artifact_id, pointer) =
                    self.example_target(checker, Self::member(&spec, "of", a1)?)?;
                let given = self.json(checker, Self::member(&spec, "given", a1)?, 0)?;
                let expect =
                    self.example_expectation(checker, Self::member(&spec, "expect", a1)?)?;
                out.push(CompiledExample {
                    name,
                    artifact_id,
                    pointer,
                    given,
                    expect,
                    at: self.at(call),
                });
            }
        }
        Ok(out)
    }

    fn example_target(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<(String, String), CompileError> {
        let target = self.deref(checker, input)?;
        if target.kind() != Kind::CallExpression {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "an example is of a Norn.law or a norm",
                input,
            ));
        }
        let id = self.call_id(checker, target).unwrap_or_default();
        if id != LAW && !is_norm_call(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "an example is of a Norn.law or a norm",
                input,
            ));
        }
        let args = target.as_call_expression().arguments().nodes();
        if id == LAW {
            return self.example_law_target(checker, target, args, input);
        }
        self.example_norm_target(checker, target, args, input)
    }

    fn example_law_target(
        &self,
        checker: &mut Checker,
        target: P<Node>,
        args: &[P<Node>],
        input: P<Node>,
    ) -> Result<(String, String), CompileError> {
        if let Some(&first_arg) = args.first()
            && !is_string_literal_like(self.deref(checker, first_arg)?)
        {
            let m = self.meta(
                checker,
                target,
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
            let aid = self.string(checker, Self::member(&m, "id", target)?)?;
            return Ok((aid, "/payload".to_owned()));
        }
        let (Some(&a0), Some(&a1)) = (args.first(), args.get(1)) else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "an example is of a Norn.law or a norm",
                input,
            ));
        };
        let spec = self.record(checker, a1)?;
        let (s_call, s_members) = self.referenced(
            checker,
            Self::member(&spec, "on", a1)?,
            ANY_TYPE,
            "a type definition",
        )?;
        let subject = self.string(checker, Self::member(&s_members, "name", s_call)?)?;
        let law_id = match spec.get("id") {
            Some(i) => self.string(checker, i)?,
            None => format!("{subject}.{}", self.string(checker, a0)?),
        };
        Ok((format!("{law_id}.law"), "/payload".to_owned()))
    }

    fn example_norm_target(
        &self,
        checker: &mut Checker,
        target: P<Node>,
        args: &[P<Node>],
        input: P<Node>,
    ) -> Result<(String, String), CompileError> {
        let Some(&a1) = args.get(1) else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "an example is of a Norn.law or a norm",
                input,
            ));
        };
        let (type_call, _) = self.referenced(
            checker,
            Self::member(&self.record(checker, a1)?, "on", a1)?,
            OBJECT_TYPES,
            "a Norn.type definition",
        )?;
        let Some(&t_arg0) = type_call.as_call_expression().arguments().nodes().first() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "the norm is not declared in its subject's module",
                input,
            ));
        };
        let subject = self.string(
            checker,
            Self::member(&self.record(checker, t_arg0)?, "name", type_call)?,
        )?;
        let Some(sf) = get_source_file_of_node(type_call) else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "the norm is not declared in its subject's module",
                input,
            ));
        };
        let mut found_idx: Option<usize> = None;
        let mut pos: usize = 0;
        for &stmt in sf.as_node().statements() {
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            for &d in stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes()
            {
                let Some(raw_init) = d.as_variable_declaration().initializer() else {
                    continue;
                };
                let init = Self::unwrap(raw_init);
                if init.kind() != Kind::CallExpression {
                    continue;
                }
                let nid = self.call_id(checker, init).unwrap_or_default();
                if !is_norm_call(&nid) {
                    continue;
                }
                let Some(&n_arg1) = init.as_call_expression().arguments().nodes().get(1) else {
                    continue;
                };
                let (tc, _) = self.referenced(
                    checker,
                    Self::member(&self.record(checker, n_arg1)?, "on", n_arg1)?,
                    OBJECT_TYPES,
                    "a Norn.type definition",
                )?;
                if tc != type_call {
                    continue;
                }
                if init == target {
                    found_idx = Some(pos);
                }
                pos = pos.saturating_add(1);
            }
        }
        let Some(index) = found_idx else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "the norm is not declared in its subject's module",
                input,
            ));
        };
        Ok((
            format!("{subject}.entity"),
            format!("/payload/extensions/norn.normative~11.0.0/{index}/law"),
        ))
    }

    fn example_expectation(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<ExampleExpectation, CompileError> {
        let e = self.deref(checker, input)?;
        if is_string_literal_like(e) {
            let text = e.text();
            if matches!(text, "holds" | "violated" | "not_applicable") {
                return Ok(ExampleExpectation::Verdict(text.to_owned()));
            }
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "an example expects holds, violated, not_applicable or { excepted: [...] }",
                input,
            ));
        }
        let m = self.closed_record(checker, input, &["excepted"], &["excepted"])?;
        Ok(ExampleExpectation::Excepted {
            excepted: self.strings(checker, Self::member(&m, "excepted", input)?)?,
        })
    }
}

struct NormRuleFields<'a> {
    law_id: &'a str,
    law_modality: &'a str,
    modality: &'a str,
    rationale: &'a str,
    condition: Option<String>,
    body_expr: String,
    exceptions: Vec<Value>,
    slot: &'a str,
    authority_id: String,
    authority_ver: String,
    defeated_by: Vec<Value>,
}

fn build_norm_rule_from_fields(ctx: &NormContext<'_>, f: NormRuleFields<'_>) -> Value {
    let mut prov = Map::new();
    prov.insert(
        "source".to_owned(),
        Value::String("authoring_sdk".to_owned()),
    );
    let mut scope_root = Map::new();
    scope_root.insert("kind".to_owned(), Value::String("entity".to_owned()));
    scope_root.insert("id".to_owned(), Value::String(ctx.subject.to_owned()));
    let mut scope = Map::new();
    scope.insert("root".to_owned(), Value::Object(scope_root));
    scope.insert(
        "objectTypes".to_owned(),
        Value::Array(vec![Value::String(ctx.subject.to_owned())]),
    );
    let mut mod_obj = Map::new();
    mod_obj.insert("kind".to_owned(), Value::String(f.law_modality.to_owned()));
    let mut resp_obj = Map::new();
    resp_obj.insert("kind".to_owned(), Value::String("predicate".to_owned()));
    resp_obj.insert("expression".to_owned(), Value::String(f.body_expr));

    let mut law = Map::new();
    law.insert("lawVersion".to_owned(), Value::String("0.1".to_owned()));
    law.insert("id".to_owned(), Value::String(f.law_id.to_owned()));
    law.insert("owner".to_owned(), Value::String(ctx.owner.to_owned()));
    law.insert("severity".to_owned(), Value::String("error".to_owned()));
    law.insert(
        "rationale".to_owned(),
        Value::String(f.rationale.to_owned()),
    );
    law.insert("provenance".to_owned(), Value::Object(prov));
    law.insert("scope".to_owned(), Value::Object(scope));
    law.insert("subject".to_owned(), Value::String(ctx.subject.to_owned()));
    if let Some(cond) = f.condition {
        law.insert("condition".to_owned(), Value::String(cond));
    }
    law.insert("modality".to_owned(), Value::Object(mod_obj));
    law.insert("response".to_owned(), Value::Object(resp_obj));
    if !f.exceptions.is_empty() {
        law.insert("exceptions".to_owned(), Value::Array(f.exceptions));
    }

    let mut src_obj = Map::new();
    src_obj.insert(
        "artifactId".to_owned(),
        Value::String(ctx.artifact_id.to_owned()),
    );
    src_obj.insert("pointer".to_owned(), Value::String(f.slot.to_owned()));

    let mut rule = Map::new();
    rule.insert("law".to_owned(), Value::Object(law));
    rule.insert("modality".to_owned(), Value::String(f.modality.to_owned()));
    rule.insert("authorityId".to_owned(), Value::String(f.authority_id));
    rule.insert(
        "authorityVersion".to_owned(),
        Value::String(f.authority_ver),
    );
    rule.insert("source".to_owned(), Value::Object(src_obj));
    if !f.defeated_by.is_empty() {
        rule.insert("defeatedBy".to_owned(), Value::Array(f.defeated_by));
    }
    Value::Object(rule)
}

fn norm_modality_info(kind: &str) -> (&'static str, &'static str, &'static str) {
    if kind == OBLIGATION {
        ("obligation", "must", "always")
    } else if kind == PROHIBITION {
        ("prohibition", "never", "never")
    } else {
        ("permission", "may", "always")
    }
}

fn read_norm_body_member(
    spec: &Members,
    spec_node: P<Node>,
    modality: &str,
    member: &str,
) -> Result<P<Node>, CompileError> {
    for other in ["must", "never", "may"] {
        if other != member
            && let Some(node) = spec.get(other)
        {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("an {modality} takes {member}, not {other}"),
                node,
            ));
        }
    }
    spec.get(member).ok_or_else(|| {
        CompileError::new(
            "NORN-FLOW-LITERAL",
            format!("an {modality} needs {member}"),
            spec_node,
        )
    })
}
