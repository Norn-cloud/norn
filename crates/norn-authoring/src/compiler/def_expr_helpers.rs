//! ZEN expression lowering, law lambda lowering, and subject-schema projection
//! split out of [`super::def_helpers`].

use std::collections::{BTreeMap, HashSet};

use tsrs_ast::{Kind, Node, SourceFile, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::def_helpers::{
    ANY_TYPE, CALLER_ID, COMMIT_TIME, COMMIT_TIME_MILLIS, DefinitionCompiler, DefinitionSource,
    INTERFACE, INTERFACES, Members, OBJECT_TYPES, SubjectSchema,
};
use super::expr::{Binding, Expr, Scope, TemporalKind, parsed_temporal, render, type_of};
use super::flow_syntax::{SyntaxCompiler, SyntaxConfig};
use super::types::{CompileError, diagnostic_for_node, is_string_literal_like};
use super::value_schema::{ExpansionBudget, ObjectSchema, ValueSchema};
use super::value_types::{PropertyType, registry_value_type};

struct SubjectAcc<'a> {
    seen: &'a mut HashSet<P<Node>>,
    properties: &'a mut BTreeMap<String, ValueSchema>,
    temporal: &'a mut BTreeMap<String, TemporalKind>,
}

impl<'a> DefinitionCompiler<'a> {
    pub(crate) fn syntax_compiler(&self, sf: P<SourceFile>) -> SyntaxCompiler<'a> {
        SyntaxCompiler::new(&SyntaxConfig {
            intrinsics_module: self.intrinsics_module,
            effect_dist_prefix: self.effect_dist_prefix,
            vfs: self.vfs,
            program: self.program,
            source: sf,
            started: self.started,
            limits: self.limits,
            source_root: self.tool_root,
            tool_root: self.tool_root,
            forbid_unsafe_zen: self.forbid_unsafe_zen,
        })
    }
}

impl DefinitionCompiler<'_> {
    pub(crate) fn zen_at(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        artifact_id: &str,
        pointer: &str,
        root: &str,
    ) -> Result<String, CompileError> {
        self.source_map.push(DefinitionSource {
            artifact_id: artifact_id.to_owned(),
            pointer: pointer.to_owned(),
            at: self.at(input),
        });
        self.zen(checker, expansion, input, root)
    }

    pub(crate) fn zen(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        root: &str,
    ) -> Result<String, CompileError> {
        let e = self.deref(checker, input)?;
        if e.kind() == Kind::CallExpression {
            let id = self.call_id(checker, e);
            let args = e.as_call_expression().arguments().nodes();
            if id.as_deref() == Some(CALLER_ID) {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.callerId() takes no argument",
                        e,
                    ));
                }
                return Ok("actor.subjectId".to_owned());
            }
            if id.as_deref() == Some(COMMIT_TIME) {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.commitTime() takes no argument",
                        e,
                    ));
                }
                return Ok("clock.nowIso".to_owned());
            }
            if id.as_deref() == Some(COMMIT_TIME_MILLIS) {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        "NORN-FLOW-EXPRESSION",
                        "Norn.commitTimeMillis() takes no argument",
                        e,
                    ));
                }
                return Ok("clock.nowMillis".to_owned());
            }
            if id.as_deref() == Some("@norn/authoring#Norn.unsafeZen") {
                return self.zen_unsafe(checker, e, args);
            }
            if id.as_deref() == Some("@norn/authoring#Norn.expression") {
                return self.zen_expression(checker, expansion, e, args, root);
            }
        }
        Err(CompileError::new(
            "NORN-FLOW-EXPRESSION",
            "raw ZEN is not typed authoring; use Norn.expression(Schema, input => ...) or the explicit unsafeZen escape",
            input,
        ))
    }

    fn zen_unsafe(
        &mut self,
        checker: &mut Checker,
        e: P<Node>,
        args: &[P<Node>],
    ) -> Result<String, CompileError> {
        if self.forbid_unsafe_zen {
            return Err(CompileError::new(
                "NORN-AUTHORING-UNSAFE-ZEN",
                "unsafeZen is forbidden (--forbid-unsafe-zen); replace it with a typed TypeScript expression callback",
                e,
            ));
        }
        self.warnings.push(diagnostic_for_node(
            e,
            "NORN-AUTHORING-UNSAFE-ZEN",
            "unsafeZen bypasses typed authoring; replace it with a TypeScript expression callback",
        ));
        let Some(&a0) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "unsafeZen requires a string literal",
                e,
            ));
        };
        self.string(checker, a0)
    }

    fn zen_expression(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        e: P<Node>,
        args: &[P<Node>],
        root: &str,
    ) -> Result<String, CompileError> {
        let (Some(&schema_arg), Some(&body)) = (args.first(), args.get(1)) else {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "use an inline expression callback",
                e,
            ));
        };
        let read = self.schemas.read(checker, schema_arg)?;
        let schema = self.schemas.expression_input(&read, expansion)?;
        let bk = body.kind();
        if bk != Kind::ArrowFunction && bk != Kind::FunctionExpression {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "use an inline expression callback",
                e,
            ));
        }
        let Some(sf) = get_source_file_of_node(e) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "missing source file",
                e,
            ));
        };
        let mut syntax = self.syntax_compiler(sf);
        let rows = syntax.decision_rows(body, checker, expansion, None)?;
        self.warnings.extend(syntax.warnings);
        let Some(row) = rows.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "use a conditional expression for a scalar expression",
                body,
            ));
        };
        if rows.len() != 1 {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "use a conditional expression for a scalar expression",
                body,
            ));
        }
        if let Some(&options) = args.get(2) {
            return self.render_context_expression(checker, options, schema, &row.value, body);
        }
        let mut scope = Scope::new();
        scope.insert(
            "input".to_owned(),
            Binding::Zen {
                zen: root.to_owned(),
                schema,
            },
        );
        render(&row.value, &scope).map_err(|err| CompileError::new(err.code, err.message, body))
    }

    fn render_context_expression(
        &self,
        checker: &mut Checker,
        options: P<Node>,
        schema: ValueSchema,
        value: &Expr,
        body: P<Node>,
    ) -> Result<String, CompileError> {
        let opts = self.closed_record(checker, options, &["root"], &["root"])?;
        let root_val = self.string(checker, Self::member(&opts, "root", options)?)?;
        let ValueSchema::Object(obj) = &schema else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "a context expression needs a record schema and root: context",
                options,
            ));
        };
        if root_val != "context" {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "a context expression needs a record schema and root: context",
                options,
            ));
        }
        let mut context = Scope::new();
        let mut inline_fields = Vec::new();
        for (name, member) in &obj.properties {
            context.insert(
                name.clone(),
                Binding::Zen {
                    zen: name.clone(),
                    schema: member.clone(),
                },
            );
            inline_fields.push((name.clone(), Expr::input(name.clone(), Vec::new())));
        }
        let mut outer = Scope::new();
        outer.insert(
            "input".to_owned(),
            Binding::Inline {
                inline: Expr::Record(inline_fields),
                scope: context,
                schema,
            },
        );
        render(value, &outer).map_err(|err| CompileError::new(err.code, err.message, body))
    }

    pub(crate) fn lambda(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        self_schema: &SubjectSchema,
    ) -> Result<String, CompileError> {
        Ok(self.lambda_expr(checker, expansion, input, self_schema)?.1)
    }

    pub(crate) fn lambda_expr(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
        self_schema: &SubjectSchema,
    ) -> Result<(Expr, String), CompileError> {
        let fn_node = self.deref(checker, input)?;
        let fk = fn_node.kind();
        if fk != Kind::ArrowFunction && fk != Kind::FunctionExpression {
            return Err(CompileError::new(
                "NORN-FLOW-CAPTURE",
                "a law predicate is an inline lambda (self => ...)",
                input,
            ));
        }
        if fn_node.parameters().len() != 1 {
            return Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                "a law predicate takes one parameter, the subject",
                fn_node,
            ));
        }
        let Some(sf) = get_source_file_of_node(fn_node) else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "missing source file",
                fn_node,
            ));
        };
        let mut syntax = self.syntax_compiler(sf);
        let rows = syntax.decision_rows(fn_node, checker, expansion, None)?;
        self.warnings.append(&mut syntax.warnings);
        let Some(row) = rows.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "use a conditional expression for a law predicate",
                fn_node,
            ));
        };
        if rows.len() != 1 {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "use a conditional expression for a law predicate",
                fn_node,
            ));
        }
        let src_node = syntax.find_expression_source(&row.value).unwrap_or(fn_node);
        let mut scope = Scope::new();
        scope.insert(
            "input".to_owned(),
            Binding::Zen {
                zen: "self".to_owned(),
                schema: self_schema.schema.clone(),
            },
        );
        let value = parsed_temporal(row.value.clone(), &self_schema.temporal)
            .map_err(|err| CompileError::new(err.code, err.message, src_node))?;
        let inferred = type_of(&value, &scope)
            .map_err(|err| CompileError::new(err.code, err.message, src_node))?;
        if !matches!(inferred, ValueSchema::Boolean { .. }) {
            return Err(CompileError::new(
                "NORN-FLOW-EXPRESSION",
                "a law predicate must be Boolean",
                src_node,
            ));
        }
        let zen = render(&value, &scope)
            .map_err(|err| CompileError::new(err.code, err.message, src_node))?;
        Ok((value, zen))
    }

    pub(crate) fn subject_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        members: &Members,
        include_links: bool,
        seen: &mut HashSet<P<Node>>,
    ) -> Result<SubjectSchema, CompileError> {
        if !seen.insert(call) {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                "cyclic type inheritance",
                call,
            ));
        }
        let mut properties: BTreeMap<String, ValueSchema> = BTreeMap::new();
        let mut temporal: BTreeMap<String, TemporalKind> = BTreeMap::new();
        let is_iface = self.call_id(checker, call).as_deref() == Some(INTERFACE);
        {
            let mut acc = SubjectAcc {
                seen,
                properties: &mut properties,
                temporal: &mut temporal,
            };
            if is_iface {
                self.inherit_subject(
                    checker,
                    expansion,
                    members.get("extends"),
                    INTERFACES,
                    true,
                    &mut acc,
                )?;
            } else {
                self.inherit_subject(
                    checker,
                    expansion,
                    members.get("implements"),
                    INTERFACES,
                    true,
                    &mut acc,
                )?;
                self.inherit_subject(
                    checker,
                    expansion,
                    members.get("extends"),
                    OBJECT_TYPES,
                    false,
                    &mut acc,
                )?;
            }
        }
        self.subject_properties(
            checker,
            expansion,
            call,
            members,
            &mut properties,
            &mut temporal,
        )?;
        if include_links && let Some(links_node) = members.get("links") {
            self.subject_links(
                checker,
                expansion,
                links_node,
                &mut properties,
                &mut temporal,
            )?;
        }
        seen.remove(&call);
        Ok(SubjectSchema {
            schema: ValueSchema::object(ObjectSchema::record(properties, &[])),
            temporal,
        })
    }

    fn subject_properties(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        members: &Members,
        properties: &mut BTreeMap<String, ValueSchema>,
        temporal: &mut BTreeMap<String, TemporalKind>,
    ) -> Result<(), CompileError> {
        let props_node = Self::member(members, "properties", call)?;
        let read = self.schemas.read(checker, props_node)?;
        for prop in self.schemas.properties(&read, expansion)? {
            let declared = prop.projection.prop_type;
            let resolved = match &declared {
                PropertyType::Ref { name, .. } => registry_value_type(name).unwrap_or(declared),
                PropertyType::Text { .. }
                | PropertyType::Integer { .. }
                | PropertyType::Decimal(_)
                | PropertyType::DecimalBinding { .. }
                | PropertyType::Boolean
                | PropertyType::Timestamp { .. }
                | PropertyType::Date
                | PropertyType::Duration
                | PropertyType::Enum { .. } => declared,
            };
            match &resolved {
                PropertyType::Date => {
                    temporal.insert(prop.id.clone(), TemporalKind::Date);
                }
                PropertyType::Timestamp { .. } => {
                    temporal.insert(prop.id.clone(), TemporalKind::Timestamp);
                }
                PropertyType::Text { .. }
                | PropertyType::Integer { .. }
                | PropertyType::Decimal(_)
                | PropertyType::DecimalBinding { .. }
                | PropertyType::Boolean
                | PropertyType::Duration
                | PropertyType::Enum { .. }
                | PropertyType::Ref { .. } => {
                    temporal.remove(&prop.id);
                }
            }
            let base = match resolved {
                PropertyType::Integer { .. } => ValueSchema::integer(),
                PropertyType::Decimal(_) | PropertyType::DecimalBinding { .. } => {
                    ValueSchema::number()
                }
                PropertyType::Boolean => ValueSchema::boolean(),
                PropertyType::Enum { values } => {
                    ValueSchema::text_enum(values.into_iter().map(|v| v.value).collect())
                }
                PropertyType::Text { .. }
                | PropertyType::Timestamp { .. }
                | PropertyType::Date
                | PropertyType::Duration
                | PropertyType::Ref { .. } => ValueSchema::text(),
            };
            properties.insert(
                prop.id,
                if prop.projection.nullable {
                    ValueSchema::nullable(base)
                } else {
                    base
                },
            );
        }
        Ok(())
    }

    fn subject_links(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        links_node: P<Node>,
        properties: &mut BTreeMap<String, ValueSchema>,
        temporal: &mut BTreeMap<String, TemporalKind>,
    ) -> Result<(), CompileError> {
        for link_node in self.list(checker, links_node)? {
            let m = self.record(checker, link_node)?;
            let target = self.deref(checker, Self::member(&m, "target", link_node)?)?;
            if is_string_literal_like(target) {
                continue;
            }
            let (t_call, t_members) =
                self.referenced(checker, target, ANY_TYPE, "a type definition")?;
            let mut inner_seen = HashSet::new();
            let linked = self.subject_schema(
                checker,
                expansion,
                t_call,
                &t_members,
                false,
                &mut inner_seen,
            )?;
            let link_id = self.string(checker, Self::member(&m, "id", link_node)?)?;
            let card = self.closed_record(
                checker,
                Self::member(&m, "cardinality", link_node)?,
                &["to", "from", "required"],
                &["to", "from"],
            )?;
            let to = self.string(checker, Self::member(&card, "to", link_node)?)?;
            let required = match card.get("required") {
                Some(r) => self.boolean(checker, r)?,
                None => false,
            };
            let link_schema = if to == "many" {
                ValueSchema::array(linked.schema)
            } else if required {
                linked.schema
            } else {
                ValueSchema::nullable(linked.schema)
            };
            properties.insert(link_id.clone(), link_schema);
            for (k, v) in linked.temporal {
                temporal.insert(format!("{link_id}.{k}"), v);
            }
        }
        Ok(())
    }

    fn inherit_subject(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        node: Option<P<Node>>,
        kinds: &[&str],
        many: bool,
        acc: &mut SubjectAcc<'_>,
    ) -> Result<(), CompileError> {
        let Some(node) = node else {
            return Ok(());
        };
        let parents = if many {
            self.list(checker, node)?
        } else {
            vec![node]
        };
        for parent in parents {
            let e = self.deref(checker, parent)?;
            if is_string_literal_like(e) {
                continue;
            }
            let (p_call, p_members) =
                self.referenced(checker, parent, kinds, "a type definition")?;
            let inherited =
                self.subject_schema(checker, expansion, p_call, &p_members, false, acc.seen)?;
            if let ValueSchema::Object(obj) = inherited.schema {
                for (k, v) in obj.properties {
                    acc.properties.insert(k, v);
                }
            }
            for (k, v) in inherited.temporal {
                acc.temporal.insert(k, v);
            }
        }
        Ok(())
    }
}
