//! Core types, constants, and AST/reference helpers for [`DefinitionCompiler`].
//!
//! Ports the core helpers of `tools/norn-ts/src/compiler/definitions.ts`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tsrs_ast::{Kind, Node, NodeFlags, is_const_type_reference};
use tsrs_checker::Checker;
use tsrs_compiler::Program;
use tsrs_core::P;

use crate::checker::resolve::resolve_symbol;
use crate::{BoundedVfs, ProfileDiagnostic};

use super::expr::TemporalKind;
use super::flow_source_map::SourceLocation;
use super::schema::{ProjectedProperty, SchemaProjection};
use super::types::{
    CompileError, CompilerLimits, DependencyPin, is_string_literal_like, source_location_of,
};
use super::value_schema::ValueSchema;

/// Pending source-schema hash sentinel (`MAPPING-SOURCE-REF-1`).
pub const PENDING_SOURCE_SHA256: &str = "pending:same-project-source-schema";

pub(crate) const SOURCE_SCHEMA: &str = "@norn/authoring#Norn.sourceSchema";
pub(crate) const ONTOLOGY_PACK: &str = "@norn/authoring#Norn.ontologyPack";
pub(crate) const PACK_LINK: &str = "@norn/authoring#Norn.packLink";
pub(crate) const MARKINGS: &str = "@norn/authoring#Norn.markings";
pub(crate) const TYPE: &str = "@norn/authoring#Norn.type";
pub(crate) const INTERFACE: &str = "@norn/authoring#Norn.interfaceType";
pub(crate) const ACTION: &str = "@norn/authoring#Norn.action";
pub(crate) const ACCESS: &str = "@norn/authoring#Norn.access";
pub(crate) const DECISION: &str = "@norn/authoring#Norn.decision";
pub(crate) const CONNECTOR: &str = "@norn/authoring#Norn.connector";
pub(crate) const MAPPING: &str = "@norn/authoring#Norn.mapping";
pub(crate) const LAW: &str = "@norn/authoring#Norn.law";
pub(crate) const SETTING: &str = "@norn/authoring#Norn.setting";
pub(crate) const TRANSFORM: &str = "@norn/authoring#Norn.transform";
pub(crate) const EXAMPLE: &str = "@norn/authoring#Norn.example";
pub(crate) const OBLIGATION: &str = "@norn/authoring#Norn.obligation";
pub(crate) const PROHIBITION: &str = "@norn/authoring#Norn.prohibition";
pub(crate) const PERMISSION: &str = "@norn/authoring#Norn.permission";
pub(crate) const LABELS: &str = "@norn/authoring#Norn.labels";
pub(crate) const MODEL: &str = "@norn/authoring#Norn.model";
pub(crate) const FLOW: &str = "@norn/authoring#Norn.flow";
pub(crate) const QUESTION: &str = "@norn/authoring#Norn.question";
pub(crate) const DASHBOARD: &str = "@norn/authoring#Norn.dashboard";
pub(crate) const METRIC_ID: &str = "@norn/authoring#Norn.metricId";
pub(crate) const APPROVAL: &str = "@norn/authoring#Norn.approval";
pub(crate) const CALLER_ID: &str = "@norn/authoring#Norn.callerId";

pub(crate) const NORM_MEMBERS: &[&str] = &[
    "on", "when", "must", "never", "may", "cite", "yieldsTo", "unless", "because",
];
pub(crate) const OBJECT_TYPES: &[&str] = &[TYPE];
pub(crate) const INTERFACES: &[&str] = &[INTERFACE];
pub(crate) const ANY_TYPE: &[&str] = &[TYPE, INTERFACE];
pub(crate) const ACTIONS: &[&str] = &[ACTION];
pub(crate) const DECISIONS: &[&str] = &[DECISION];

pub(crate) const MAX_DATA_DEPTH: usize = 64;

pub(crate) fn is_norm_call(id: &str) -> bool {
    matches!(id, OBLIGATION | PROHIBITION | PERMISSION)
}

pub(crate) fn is_definition_call(id: &str) -> bool {
    matches!(
        id,
        SOURCE_SCHEMA
            | ONTOLOGY_PACK
            | PACK_LINK
            | MARKINGS
            | TYPE
            | INTERFACE
            | ACTION
            | ACCESS
            | DECISION
            | MAPPING
            | LAW
            | CONNECTOR
            | LABELS
            | MODEL
            | TRANSFORM
            | SETTING
    )
}

/// Source attribution entry for a lowered definition artifact or expression slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionSource {
    /// Target artifact identifier.
    pub artifact_id: String,
    /// JSON Pointer into the artifact (`""` for the root definition).
    pub pointer: String,
    /// Source location in the authoring module.
    pub at: SourceLocation,
}

/// Expected verdict of a `Norn.example` case (`LAW-EXAMPLE-1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExampleExpectation {
    /// `"holds"`, `"violated"`, or `"not_applicable"`.
    Verdict(String),
    /// Expected active exception ids (`{ excepted: [...] }`).
    Excepted {
        /// Exception labels expected to apply.
        excepted: Vec<String>,
    },
}

/// Compiled `Norn.example` case (`CompiledExample` in `examples.ts`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledExample {
    /// Example name.
    pub name: String,
    /// Target law or entity artifact id.
    pub artifact_id: String,
    /// JSON Pointer to the target `LawIR`.
    pub pointer: String,
    /// Given subject JSON value.
    pub given: Value,
    /// Expected verdict.
    pub expect: ExampleExpectation,
    /// Source location of the example declaration.
    pub at: SourceLocation,
}

/// Retained cell definition (`Norn.approval`), admitted beside artifacts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetainedDefinitionDoc {
    /// Retained definition kind (currently `"approval"`).
    pub kind: String,
    /// Retained definition identifier.
    pub id: String,
    /// Canonical JSON payload.
    pub payload: Value,
}

/// Insertion-ordered object-literal member map matching JS `Map<string, ts.Expression>`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Members(Vec<(String, P<Node>)>);

impl Members {
    pub(crate) fn insert(&mut self, key: String, val: P<Node>) {
        if let Some((_, existing)) = self.0.iter_mut().find(|(k, _)| k == &key) {
            *existing = val;
        } else {
            self.0.push((key, val));
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<P<Node>> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, P<Node>)> {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Inline localized labels on a definition (`Localized`).
#[derive(Clone, Debug, Default)]
pub(crate) struct LocalizedData {
    pub(crate) label: Option<Vec<(String, String)>>,
    pub(crate) localized_description: Option<Vec<(String, String)>>,
    pub(crate) needs_review: Option<Vec<String>>,
}

/// Inline property and enum-value labels on a type (`MemberLabels`).
#[derive(Clone, Debug, Default)]
pub(crate) struct MemberLabelsData {
    pub(crate) property_labels: Option<Vec<(String, Vec<(String, String)>)>>,
    pub(crate) value_labels: Option<Vec<(String, Vec<(String, Vec<(String, String)>)>)>>,
}

/// A law subject's expression schema and temporal property path map.
#[derive(Clone, Debug)]
pub(crate) struct SubjectSchema {
    pub(crate) schema: ValueSchema,
    pub(crate) temporal: BTreeMap<String, TemporalKind>,
}

/// Common lowered fields for `Norn.type` and `Norn.interfaceType`.
pub(crate) struct TypeBase {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) role: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) markings: Vec<String>,
    pub(crate) dependencies: Vec<DependencyPin>,
    pub(crate) projected: Vec<ProjectedProperty>,
}

/// One lowered definition output (its main artifact plus optional `.labels` artifact).
pub(crate) struct LoweredDef {
    pub(crate) artifact: Value,
    pub(crate) labels: Option<Value>,
    pub(crate) at: Option<SourceLocation>,
}

/// Label target classification for `<owner>.labels` generation.
pub(crate) enum LabelTarget {
    ObjectType(String),
    Action(String),
    Rule(String),
    Law(String),
}

/// Configuration for constructing a [`DefinitionCompiler`].
pub(crate) struct DefCompilerConfig<'a> {
    pub(crate) intrinsics_module: &'a str,
    pub(crate) effect_dist_prefix: Option<&'a str>,
    pub(crate) vfs: &'a BoundedVfs,
    pub(crate) program: &'static Program,
    pub(crate) started: Instant,
    pub(crate) limits: CompilerLimits,
    pub(crate) tool_root: &'a str,
    pub(crate) forbid_unsafe_zen: bool,
}

/// Syntax-only compiler for all `Norn.*` ontology/action/access/decision/law/etc. definitions.
pub(crate) struct DefinitionCompiler<'a> {
    pub(crate) intrinsics_module: &'a str,
    pub(crate) effect_dist_prefix: Option<&'a str>,
    pub(crate) vfs: &'a BoundedVfs,
    pub(crate) program: &'static Program,
    pub(crate) started: Instant,
    pub(crate) limits: CompilerLimits,
    pub(crate) tool_root: &'a str,
    pub(crate) forbid_unsafe_zen: bool,
    pub(crate) schemas: SchemaProjection<'a>,
    pub(crate) source_map: Vec<DefinitionSource>,
    pub(crate) warnings: Vec<ProfileDiagnostic>,
}

impl<'a> DefinitionCompiler<'a> {
    pub(crate) fn new(config: DefCompilerConfig<'a>) -> Self {
        let schemas = SchemaProjection::new(
            config.intrinsics_module,
            config.effect_dist_prefix,
            config.vfs,
            config.started,
            config.limits,
        );
        Self {
            intrinsics_module: config.intrinsics_module,
            effect_dist_prefix: config.effect_dist_prefix,
            vfs: config.vfs,
            program: config.program,
            started: config.started,
            limits: config.limits,
            tool_root: config.tool_root,
            forbid_unsafe_zen: config.forbid_unsafe_zen,
            schemas,
            source_map: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn guard(&self, node: P<Node>) -> Result<(), CompileError> {
        if self.started.elapsed() >= Duration::from_millis(self.limits.max_millis) {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "compiler time bound exceeded",
                node,
            ));
        }
        Ok(())
    }

    pub(crate) fn at(&self, node: P<Node>) -> SourceLocation {
        source_location_of(node, self.tool_root)
    }

    pub(crate) fn call_id(&self, checker: &mut Checker, call: P<Node>) -> Option<String> {
        let expr = call.as_call_expression().expression();
        let target = if expr.kind() == Kind::PropertyAccessExpression {
            expr.as_property_access_expression().name()
        } else {
            expr
        };
        resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            target,
        )
        .id
    }

    pub(crate) fn unwrap(mut expr: P<Node>) -> P<Node> {
        loop {
            let kind = expr.kind();
            if kind == Kind::ParenthesizedExpression {
                expr = expr.as_parenthesized_expression().expression();
            } else if kind == Kind::SatisfiesExpression {
                expr = expr.as_satisfies_expression().expression;
            } else if kind == Kind::AsExpression {
                let as_expr = expr.as_as_expression();
                if is_const_type_reference(as_expr.type_) {
                    expr = as_expr.expression;
                } else {
                    return expr;
                }
            } else {
                return expr;
            }
        }
    }

    pub(crate) fn deref(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<P<Node>, CompileError> {
        let mut expr = Self::unwrap(input);
        let mut hops = 0_usize;
        while expr.kind() == Kind::Identifier || expr.kind() == Kind::PropertyAccessExpression {
            if hops > 128 {
                return Err(CompileError::new(
                    "NORN-FLOW-UNBOUNDED",
                    "constant references exceed the compiler bound",
                    input,
                ));
            }
            hops = hops.saturating_add(1);
            let name = if expr.kind() == Kind::PropertyAccessExpression {
                expr.as_property_access_expression().name()
            } else {
                expr
            };
            let resolved = resolve_symbol(
                checker,
                self.intrinsics_module,
                self.effect_dist_prefix,
                self.vfs,
                name,
            );
            let Some(decl) = resolved.target.and_then(|s| s.value_declaration()) else {
                return Ok(expr);
            };
            if decl.kind() != Kind::VariableDeclaration {
                return Ok(expr);
            }
            let Some(parent) = decl.parent() else {
                return Ok(expr);
            };
            if !parent.flags().intersects(NodeFlags::Const) {
                return Ok(expr);
            }
            let Some(init) = decl.as_variable_declaration().initializer() else {
                return Ok(expr);
            };
            expr = Self::unwrap(init);
        }
        Ok(expr)
    }

    pub(crate) fn referenced(
        &self,
        checker: &mut Checker,
        input: P<Node>,
        kinds: &[&str],
        what: &str,
    ) -> Result<(P<Node>, Members), CompileError> {
        let call = self.deref(checker, input)?;
        if call.kind() != Kind::CallExpression {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                format!("expected a reference to {what}"),
                input,
            ));
        }
        let id = self.call_id(checker, call);
        let Some(id_str) = id.as_deref() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                format!("expected a reference to {what}"),
                input,
            ));
        };
        let Some(&arg0) = call.as_call_expression().arguments().nodes().first() else {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                format!("expected a reference to {what}"),
                input,
            ));
        };
        if !kinds.contains(&id_str) {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                format!("expected a reference to {what}"),
                input,
            ));
        }
        Ok((call, self.record(checker, arg0)?))
    }

    pub(crate) fn member(
        members: &Members,
        key: &str,
        at: P<Node>,
    ) -> Result<P<Node>, CompileError> {
        members.get(key).ok_or_else(|| {
            CompileError::new("NORN-FLOW-LITERAL", format!("a definition needs {key}"), at)
        })
    }

    pub(crate) fn type_ref(
        &self,
        checker: &mut Checker,
        input: P<Node>,
        kinds: &[&str],
        what: &str,
    ) -> Result<(String, String), CompileError> {
        let (call, members) = self.referenced(checker, input, kinds, what)?;
        let name = self.string(checker, Self::member(&members, "name", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        Ok((name, version))
    }

    pub(crate) fn type_name(
        &self,
        checker: &mut Checker,
        input: P<Node>,
        kinds: &[&str],
    ) -> Result<String, CompileError> {
        let e = self.deref(checker, input)?;
        if is_string_literal_like(e) {
            return Ok(e.text().to_owned());
        }
        Ok(self
            .type_ref(
                checker,
                input,
                kinds,
                "a Norn.type definition (or a qualified type name)",
            )?
            .0)
    }

    pub(crate) fn action_id(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<String, CompileError> {
        let e = self.deref(checker, input)?;
        if is_string_literal_like(e) {
            return Ok(e.text().to_owned());
        }
        let (call, members) = self.referenced(
            checker,
            input,
            ACTIONS,
            "a Norn.action definition (or an action id)",
        )?;
        self.string(checker, Self::member(&members, "id", call)?)
    }

    pub(crate) fn meta(
        &self,
        checker: &mut Checker,
        call: P<Node>,
        allowed: &[&str],
    ) -> Result<Members, CompileError> {
        let args = call.as_call_expression().arguments().nodes();
        let Some(&arg0) = args.first() else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a definition takes one metadata object",
                call,
            ));
        };
        let is_decision_two =
            self.call_id(checker, call).as_deref() == Some(DECISION) && args.len() == 2;
        if args.len() != 1 && !is_decision_two {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a definition takes one metadata object",
                call,
            ));
        }
        let members = self.record(checker, arg0)?;
        for (key, value) in members.iter() {
            if !allowed.contains(&key) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("unknown definition member {key}"),
                    value.parent().unwrap_or(value),
                ));
            }
        }
        Ok(members)
    }

    pub(crate) fn plain_member_nodes(prop: P<Node>) -> Result<(P<Node>, P<Node>), CompileError> {
        let pk = prop.kind();
        if pk == Kind::PropertyAssignment {
            let pa = prop.as_property_assignment();
            let name = pa.name();
            let nk = name.kind();
            if nk != Kind::Identifier && nk != Kind::StringLiteral {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "expected plain name: value members",
                    prop,
                ));
            }
            return Ok((name, pa.initializer()));
        }
        if pk == Kind::ShorthandPropertyAssignment {
            let name = prop.as_shorthand_property_assignment().name();
            return Ok((name, name));
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "expected plain name: value members",
            prop,
        ))
    }

    pub(crate) fn record(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Members, CompileError> {
        let o = self.deref(checker, input)?;
        if o.kind() != Kind::ObjectLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "expected an object literal",
                o,
            ));
        }
        let mut out = Members::default();
        for &prop in o.as_object_literal_expression().properties.nodes() {
            self.guard(prop)?;
            let (name, value) = Self::plain_member_nodes(prop)?;
            out.insert(name.text().to_owned(), value);
        }
        Ok(out)
    }

    pub(crate) fn closed_record(
        &self,
        checker: &mut Checker,
        input: P<Node>,
        allowed: &[&str],
        required: &[&str],
    ) -> Result<Members, CompileError> {
        let m = self.record(checker, input)?;
        for (key, value) in m.iter() {
            if !allowed.contains(&key) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("unknown member {key}"),
                    value.parent().unwrap_or(value),
                ));
            }
        }
        for &key in required {
            if !m.has(key) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("missing member {key}"),
                    input,
                ));
            }
        }
        Ok(m)
    }

    pub(crate) fn list(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Vec<P<Node>>, CompileError> {
        let a = self.deref(checker, input)?;
        if a.kind() != Kind::ArrayLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "expected an array literal",
                a,
            ));
        }
        let mut out = Vec::new();
        for &el in a.as_array_literal_expression().elements.nodes() {
            let ek = el.kind();
            if ek == Kind::SpreadElement || ek == Kind::OmittedExpression {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "array elements must be listed explicitly",
                    el,
                ));
            }
            self.guard(el)?;
            out.push(el);
        }
        Ok(out)
    }

    pub(crate) fn string(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<String, CompileError> {
        let e = self.deref(checker, input)?;
        if is_string_literal_like(e) {
            return Ok(e.text().to_owned());
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "expected a string literal",
            e,
        ))
    }

    pub(crate) fn strings(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Vec<String>, CompileError> {
        let mut out = Vec::new();
        for item in self.list(checker, input)? {
            out.push(self.string(checker, item)?);
        }
        Ok(out)
    }

    pub(crate) fn boolean(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<bool, CompileError> {
        let e = self.deref(checker, input)?;
        if e.kind() == Kind::TrueKeyword {
            return Ok(true);
        }
        if e.kind() == Kind::FalseKeyword {
            return Ok(false);
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "expected a boolean literal",
            e,
        ))
    }
}
