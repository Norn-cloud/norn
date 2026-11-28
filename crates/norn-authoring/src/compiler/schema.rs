//! Effect Schema syntax → `EntityIR` v2 properties and closed [`ValueSchema`] projection.
//!
//! Faithful Rust port of `tools/norn-ts/src/compiler/schema.ts`. Reads Effect
//! Schema AST calls and value references by resolved profile symbol identity
//! without ever executing authored code.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use tsrs_ast::{Kind, Node, NodeFlags, is_const_type_reference};
use tsrs_checker::Checker;
use tsrs_core::P;

use crate::BoundedVfs;
use crate::checker::resolve::{node_text, resolve_symbol};

use super::types::{CompileError, is_string_literal_like};
use super::value_types::{PropertyType, TimezonePolicy};

const MAX_SCHEMA_DEPTH: usize = 32;

/// Nullable admission mode for [`SchemaProjection::closed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NullableMode {
    /// Admit no `Schema.NullOr`.
    None,
    /// Admit `Schema.NullOr` of scalar types only (`ActionIR` input/output).
    Scalar,
    /// Admit any `Schema.NullOr` (`RuleIR` contracts and expression inputs).
    Any,
}

/// Structural reading of an Effect Schema AST expression.
#[derive(Clone, Debug)]
pub(crate) enum SchemaNodeKind {
    String,
    Int,
    Finite,
    Boolean,
    Null,
    Literals(Vec<String>),
    Struct(Vec<StructField>),
    Array(Box<SchemaNode>),
    Union(Vec<SchemaNode>),
    ValueType(PropertyType),
}

/// Structural schema node carrying its originating AST node for diagnostics.
#[derive(Clone, Debug)]
pub(crate) struct SchemaNode {
    pub(crate) kind: SchemaNodeKind,
    pub(crate) node: P<Node>,
}

/// One field of a `Schema.Struct` / `Schema.Class` / `Schema.TaggedStruct`.
#[derive(Clone, Debug)]
pub(crate) struct StructField {
    pub(crate) name: String,
    pub(crate) schema: SchemaNode,
    pub(crate) optional: bool,
    pub(crate) node: P<Node>,
}

/// `EntityIR` v2 property projection result.
#[derive(Clone, Debug)]
pub(crate) struct PropertyProjection {
    pub(crate) prop_type: PropertyType,
    pub(crate) nullable: bool,
}

/// Projected property entry in declaration order.
#[derive(Clone, Debug)]
pub(crate) struct ProjectedProperty {
    pub(crate) id: String,
    pub(crate) projection: PropertyProjection,
    pub(crate) node: P<Node>,
}

pub(crate) fn projection_err(message: impl Into<String>, node: P<Node>) -> CompileError {
    CompileError::new("NORN-FLOW-SCHEMA-PROJECTION", message, node)
}

/// Structural Effect Schema reader and projector (`SchemaProjection` in `schema.ts`).
pub(crate) struct SchemaProjection<'a> {
    intrinsics_module: &'a str,
    effect_dist_prefix: Option<&'a str>,
    vfs: &'a BoundedVfs,
    started: Instant,
    max_millis: u64,
    pub(crate) max_expanded: usize,
    memo: HashMap<P<Node>, SchemaNode>,
    active: HashSet<P<Node>>,
    pub(crate) sizes: HashMap<P<Node>, usize>,
}

impl<'a> SchemaProjection<'a> {
    pub(crate) fn new(
        intrinsics_module: &'a str,
        effect_dist_prefix: Option<&'a str>,
        vfs: &'a BoundedVfs,
        started: Instant,
        max_millis: u64,
        max_expanded: usize,
    ) -> Self {
        Self {
            intrinsics_module,
            effect_dist_prefix,
            vfs,
            started,
            max_millis,
            max_expanded,
            memo: HashMap::new(),
            active: HashSet::new(),
            sizes: HashMap::new(),
        }
    }

    pub(crate) fn guard(&self, node: P<Node>) -> Result<(), CompileError> {
        if self.started.elapsed() >= Duration::from_millis(self.max_millis) {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "compiler time bound exceeded",
                node,
            ));
        }
        Ok(())
    }

    fn identity(&self, checker: &mut Checker, node: P<Node>) -> Option<String> {
        resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            node,
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

    fn callee_name(call: P<Node>) -> P<Node> {
        let callee = call.as_call_expression().expression();
        if callee.kind() == Kind::PropertyAccessExpression {
            callee.as_property_access_expression().name()
        } else {
            callee
        }
    }

    fn const_array(&self, checker: &mut Checker, input: P<Node>) -> Result<P<Node>, CompileError> {
        let mut expr = Self::unwrap(input);
        let mut hops = 0_usize;
        while expr.kind() == Kind::Identifier {
            if hops > 128 {
                return Err(CompileError::new(
                    "NORN-FLOW-UNBOUNDED",
                    "constant references exceed the compiler bound",
                    input,
                ));
            }
            hops = hops.saturating_add(1);
            let sym = resolve_symbol(
                checker,
                self.intrinsics_module,
                self.effect_dist_prefix,
                self.vfs,
                expr,
            )
            .target;
            let decl = sym.and_then(|s| s.value_declaration());
            let Some(decl) = decl else {
                break;
            };
            if decl.kind() != Kind::VariableDeclaration {
                break;
            }
            let Some(parent) = decl.parent() else {
                break;
            };
            if !parent.flags().intersects(NodeFlags::Const) {
                break;
            }
            let Some(init) = decl.as_variable_declaration().initializer() else {
                break;
            };
            expr = Self::unwrap(init);
        }
        self.guard(expr)?;
        Ok(expr)
    }

    /// Reads an Effect Schema AST expression structurally.
    pub(crate) fn read(
        &mut self,
        checker: &mut Checker,
        expression: P<Node>,
    ) -> Result<SchemaNode, CompileError> {
        self.read_at(checker, expression, 0)
    }

    fn read_at(
        &mut self,
        checker: &mut Checker,
        input: P<Node>,
        depth: usize,
    ) -> Result<SchemaNode, CompileError> {
        self.guard(input)?;
        if depth > MAX_SCHEMA_DEPTH {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "schema nesting exceeds the compiler bound",
                input,
            ));
        }
        let expr = Self::unwrap(input);
        if let Some(known) = self.memo.get(&expr) {
            return Ok(known.clone());
        }
        if self.active.contains(&expr) {
            return Err(projection_err(
                "recursive schemas (suspend/lazy) have no EntityIR projection",
                expr,
            ));
        }
        self.active.insert(expr);
        let res = if expr.kind() == Kind::CallExpression {
            self.read_call(checker, expr, depth)
        } else {
            self.read_reference(checker, expr, depth)
        };
        self.active.remove(&expr);
        let node = res?;
        self.memo.insert(expr, node.clone());
        Ok(node)
    }

    fn read_reference(
        &mut self,
        checker: &mut Checker,
        expr: P<Node>,
        depth: usize,
    ) -> Result<SchemaNode, CompileError> {
        if expr.kind() != Kind::Identifier && expr.kind() != Kind::PropertyAccessExpression {
            return Err(projection_err(
                format!("{:?} is not a schema in the structural subset", expr.kind()),
                expr,
            ));
        }
        let target_node = if expr.kind() == Kind::PropertyAccessExpression {
            expr.as_property_access_expression().name()
        } else {
            expr
        };
        let resolved = resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            target_node,
        );
        if let Some(id) = resolved.id.as_deref() {
            let kind = match id {
                "effect/Schema#String" => SchemaNodeKind::String,
                "effect/Schema#Int" => SchemaNodeKind::Int,
                "effect/Schema#Finite" => SchemaNodeKind::Finite,
                "effect/Schema#Boolean" => SchemaNodeKind::Boolean,
                "effect/Schema#Null" => SchemaNodeKind::Null,
                other => {
                    return Err(projection_err(
                        format!("{other} has no EntityIR projection"),
                        expr,
                    ));
                }
            };
            return Ok(SchemaNode { kind, node: expr });
        }
        let decl = resolved.target.and_then(|sym| {
            sym.value_declaration()
                .or_else(|| sym.declarations().first().copied())
        });
        if let Some(decl) = decl {
            if decl.kind() == Kind::VariableDeclaration
                && decl
                    .parent()
                    .is_some_and(|p| p.flags().intersects(NodeFlags::Const))
                && let Some(init) = decl.as_variable_declaration().initializer()
            {
                return self.read_at(checker, init, depth.saturating_add(1));
            }
            if decl.kind() == Kind::ClassDeclaration
                && let Some(clauses) = decl.as_class_declaration().heritage_clauses()
                && let Some(&first_clause) = clauses.nodes().first()
                && let Some(&first_type) = first_clause.as_heritage_clause().types().nodes().first()
            {
                let heritage = first_type.as_expression_with_type_arguments().expression();
                if heritage.kind() == Kind::CallExpression {
                    let outer = heritage.as_call_expression();
                    let inner = outer.expression();
                    if inner.kind() == Kind::CallExpression
                        && self.identity(checker, Self::callee_name(inner)).as_deref()
                            == Some("effect/Schema#Class")
                    {
                        let Some(&fields_arg) = outer.arguments().nodes().first() else {
                            return Err(projection_err(
                                "Schema.Class needs its field struct",
                                heritage,
                            ));
                        };
                        let fields = self.read_fields(checker, fields_arg, depth)?;
                        return Ok(SchemaNode {
                            kind: SchemaNodeKind::Struct(fields),
                            node: expr,
                        });
                    }
                }
            }
        }
        let text = tsrs_ast::get_source_file_of_node(expr).map_or("", |sf| node_text(sf, expr));
        Err(CompileError::new(
            "NORN-FLOW-VALUE",
            format!(
                "{text} is not a schema definition (a top-level const Schema value or a Schema.Class)"
            ),
            expr,
        ))
    }

    fn read_call(
        &mut self,
        checker: &mut Checker,
        call: P<Node>,
        depth: usize,
    ) -> Result<SchemaNode, CompileError> {
        let id = self.identity(checker, Self::callee_name(call));
        let args = call.as_call_expression().arguments().nodes();
        let arg = |idx: usize| -> Result<P<Node>, CompileError> {
            args.get(idx).copied().ok_or_else(|| {
                projection_err(
                    format!("{} is missing an argument", id.as_deref().unwrap_or("call")),
                    call,
                )
            })
        };
        let next_depth = depth.saturating_add(1);
        match id.as_deref() {
            Some("effect/Schema#Struct") => {
                let fields = self.read_fields(checker, arg(0)?, depth)?;
                Ok(SchemaNode {
                    kind: SchemaNodeKind::Struct(fields),
                    node: call,
                })
            }
            Some("effect/Schema#TaggedStruct") => {
                self.read_tagged_struct_call(checker, call, arg(0)?, arg(1)?, depth)
            }
            Some("effect/Schema#Literal") => Ok(SchemaNode {
                kind: SchemaNodeKind::Literals(vec![Self::read_string_enum_literal(arg(0)?)?]),
                node: call,
            }),
            Some("effect/Schema#Literals") => self.read_literals_call(checker, call, arg(0)?),
            Some("effect/Schema#NullOr") => {
                let inner = self.read_at(checker, arg(0)?, next_depth)?;
                Ok(SchemaNode {
                    kind: SchemaNodeKind::Union(vec![
                        inner,
                        SchemaNode {
                            kind: SchemaNodeKind::Null,
                            node: call,
                        },
                    ]),
                    node: call,
                })
            }
            Some("effect/Schema#Union") => {
                self.read_union_call(checker, call, arg(0)?, args.get(1).copied(), next_depth)
            }
            Some("effect/Schema#ArraySchema") => {
                let items = self.read_at(checker, arg(0)?, next_depth)?;
                Ok(SchemaNode {
                    kind: SchemaNodeKind::Array(Box::new(items)),
                    node: call,
                })
            }
            Some("effect/Schema#TaggedUnion") => Err(projection_err(
                "discriminated unions have no qualified EntityIR or closed-schema projection in v1",
                call,
            )),
            Some("effect/Schema#optionalKey") => Err(projection_err(
                "Schema.optionalKey marks a struct member; use it only as a Schema.Struct field",
                call,
            )),
            Some("effect/Pipeable#Pipeable.pipe" | "effect/Function#pipe") => self.read_pipe_call(
                checker,
                call,
                args,
                id.as_deref() == Some("effect/Function#pipe"),
                next_depth,
            ),
            _ => self.read_norn_call(call, args, id),
        }
    }

    fn read_tagged_struct_call(
        &mut self,
        checker: &mut Checker,
        call: P<Node>,
        tag_arg: P<Node>,
        fields_arg: P<Node>,
        depth: usize,
    ) -> Result<SchemaNode, CompileError> {
        let tag = Self::unwrap(tag_arg);
        if tag.kind() != Kind::StringLiteral {
            return Err(projection_err(
                "TaggedStruct tags project only as string literals",
                tag,
            ));
        }
        let mut fields = self.read_fields(checker, fields_arg, depth)?;
        if fields.iter().any(|f| f.name == "_tag") {
            return Err(projection_err(
                "TaggedStruct fields cannot redeclare _tag",
                call,
            ));
        }
        let tag_field = StructField {
            name: "_tag".to_owned(),
            schema: SchemaNode {
                kind: SchemaNodeKind::Literals(vec![tag.text().to_owned()]),
                node: tag,
            },
            optional: false,
            node: tag,
        };
        fields.insert(0, tag_field);
        Ok(SchemaNode {
            kind: SchemaNodeKind::Struct(fields),
            node: call,
        })
    }

    fn read_literals_call(
        &self,
        checker: &mut Checker,
        call: P<Node>,
        list_arg: P<Node>,
    ) -> Result<SchemaNode, CompileError> {
        let list = self.const_array(checker, list_arg)?;
        if list.kind() != Kind::ArrayLiteralExpression {
            return Err(projection_err(
                "Schema.Literals takes a non-empty literal array",
                list,
            ));
        }
        let elems = list.as_array_literal_expression().elements.nodes();
        if elems.is_empty() {
            return Err(projection_err(
                "Schema.Literals takes a non-empty literal array",
                list,
            ));
        }
        let mut vals = Vec::with_capacity(elems.len());
        for &el in elems {
            vals.push(Self::read_string_enum_literal(el)?);
        }
        Ok(SchemaNode {
            kind: SchemaNodeKind::Literals(vals),
            node: call,
        })
    }

    fn read_union_call(
        &mut self,
        checker: &mut Checker,
        call: P<Node>,
        list_arg: P<Node>,
        extra: Option<P<Node>>,
        next_depth: usize,
    ) -> Result<SchemaNode, CompileError> {
        let list = Self::unwrap(list_arg);
        if list.kind() != Kind::ArrayLiteralExpression {
            return Err(projection_err(
                "Schema.Union takes a non-empty literal array of members",
                list,
            ));
        }
        let elems = list.as_array_literal_expression().elements.nodes();
        if elems.is_empty() {
            return Err(projection_err(
                "Schema.Union takes a non-empty literal array of members",
                list,
            ));
        }
        if let Some(extra_arg) = extra {
            return Err(projection_err(
                "Schema.Union options have no EntityIR projection",
                extra_arg,
            ));
        }
        let mut members = Vec::with_capacity(elems.len());
        for &el in elems {
            members.push(self.read_at(checker, el, next_depth)?);
        }
        Ok(SchemaNode {
            kind: SchemaNodeKind::Union(members),
            node: call,
        })
    }

    fn read_pipe_call(
        &mut self,
        checker: &mut Checker,
        call: P<Node>,
        args: &[P<Node>],
        is_fn_pipe: bool,
        next_depth: usize,
    ) -> Result<SchemaNode, CompileError> {
        let base = if is_fn_pipe {
            args.first().copied().ok_or_else(|| {
                projection_err("effect/Function#pipe is missing an argument", call)
            })?
        } else {
            call.as_call_expression()
                .expression()
                .as_property_access_expression()
                .expression()
        };
        let operators = if is_fn_pipe {
            args.get(1..).unwrap_or(&[])
        } else {
            args
        };
        for &op in operators {
            let o = Self::unwrap(op);
            if o.kind() != Kind::CallExpression
                || self.identity(checker, Self::callee_name(o)).as_deref()
                    != Some("effect/Schema#brand")
            {
                return Err(projection_err(
                    "only Schema.brand may be piped onto a schema (checks and transforms have no projection)",
                    o,
                ));
            }
            let brand_arg = o
                .as_call_expression()
                .arguments()
                .nodes()
                .first()
                .map(|&n| Self::unwrap(n));
            if brand_arg.is_none_or(|n| n.kind() != Kind::StringLiteral) {
                return Err(projection_err("Schema.brand takes a string literal", o));
            }
        }
        self.read_at(checker, base, next_depth)
    }

    fn read_norn_call(
        &self,
        call: P<Node>,
        args: &[P<Node>],
        id: Option<String>,
    ) -> Result<SchemaNode, CompileError> {
        match id.as_deref() {
            Some("@norn/authoring#Norn.rate") => self.read_rate(call, args),
            Some("@norn/authoring#Norn.decimal") => self.read_decimal(call, args),
            Some("@norn/authoring#Norn.boundDecimal") => Ok(SchemaNode {
                kind: SchemaNodeKind::ValueType(PropertyType::DecimalBinding {
                    group: Self::read_string_literal(
                        args.first().copied(),
                        call,
                        "the decimal binding group",
                    )?,
                }),
                node: call,
            }),
            Some("@norn/authoring#Norn.ref") => self.read_ref(call, args),
            Some("@norn/authoring#Norn.timestamp") => {
                let tz_str =
                    Self::read_string_literal(args.first().copied(), call, "the timezone policy")?;
                let Some(timezone) = TimezonePolicy::parse(&tz_str) else {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "the timezone policy is \"utc\" or \"offset\"",
                        call,
                    ));
                };
                Ok(SchemaNode {
                    kind: SchemaNodeKind::ValueType(PropertyType::Timestamp { timezone }),
                    node: call,
                })
            }
            Some("@norn/authoring#Norn.date") => Ok(SchemaNode {
                kind: SchemaNodeKind::ValueType(PropertyType::Date),
                node: call,
            }),
            _ => {
                let label = id.unwrap_or_else(|| {
                    tsrs_ast::get_source_file_of_node(call)
                        .map_or("", |sf| {
                            node_text(sf, call.as_call_expression().expression())
                        })
                        .to_owned()
                });
                Err(projection_err(
                    format!("{label} is not a schema in the structural subset"),
                    call,
                ))
            }
        }
    }

    fn read_fields(
        &mut self,
        checker: &mut Checker,
        input: P<Node>,
        depth: usize,
    ) -> Result<Vec<StructField>, CompileError> {
        let entries = Self::read_struct_entries(input)?;
        let mut fields = Vec::with_capacity(entries.len());
        let next_depth = depth.saturating_add(1);
        for (name, value, node) in entries {
            let v = Self::unwrap(value);
            if v.kind() == Kind::CallExpression
                && self.identity(checker, Self::callee_name(v)).as_deref()
                    == Some("effect/Schema#optionalKey")
            {
                let Some(&inner) = v.as_call_expression().arguments().nodes().first() else {
                    return Err(projection_err("Schema.optionalKey needs a schema", v));
                };
                let schema = self.read_at(checker, inner, next_depth)?;
                fields.push(StructField {
                    name,
                    schema,
                    optional: true,
                    node,
                });
            } else {
                let schema = self.read_at(checker, v, next_depth)?;
                fields.push(StructField {
                    name,
                    schema,
                    optional: false,
                    node,
                });
            }
        }
        Ok(fields)
    }

    fn read_struct_entries(
        input: P<Node>,
    ) -> Result<Vec<(String, P<Node>, P<Node>)>, CompileError> {
        let obj = Self::unwrap(input);
        if obj.kind() != Kind::ObjectLiteralExpression {
            return Err(projection_err(
                "struct fields must be an inline object literal",
                obj,
            ));
        }
        let mut out = Vec::new();
        for &prop in obj.as_object_literal_expression().properties.nodes() {
            let entry = if prop.kind() == Kind::PropertyAssignment {
                let pa = prop.as_property_assignment();
                let nk = pa.name().kind();
                if nk != Kind::Identifier && nk != Kind::StringLiteral {
                    return Err(projection_err(
                        "struct members are plain name: schema entries",
                        prop,
                    ));
                }
                (pa.name().text().to_owned(), pa.initializer(), prop)
            } else if prop.kind() == Kind::ShorthandPropertyAssignment {
                let sp = prop.as_shorthand_property_assignment();
                (sp.name().text().to_owned(), sp.name(), prop)
            } else {
                return Err(projection_err(
                    "struct members are plain name: schema entries",
                    prop,
                ));
            };
            if entry.0 == "__proto__" {
                return Err(projection_err(
                    "__proto__ is not a portable member name",
                    prop,
                ));
            }
            out.push(entry);
        }
        Ok(out)
    }

    pub(crate) fn read_object_literal(
        input: P<Node>,
    ) -> Result<BTreeMap<String, P<Node>>, CompileError> {
        let obj = Self::unwrap(input);
        if obj.kind() != Kind::ObjectLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "expected an inline object literal",
                obj,
            ));
        }
        let mut out = BTreeMap::new();
        for &prop in obj.as_object_literal_expression().properties.nodes() {
            if prop.kind() != Kind::PropertyAssignment {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "expected plain name: value members",
                    prop,
                ));
            }
            let pa = prop.as_property_assignment();
            let nk = pa.name().kind();
            if nk != Kind::Identifier && nk != Kind::StringLiteral {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "expected plain name: value members",
                    prop,
                ));
            }
            out.insert(pa.name().text().to_owned(), pa.initializer());
        }
        Ok(out)
    }

    pub(crate) fn read_string_literal(
        input: Option<P<Node>>,
        at: P<Node>,
        what: &str,
    ) -> Result<String, CompileError> {
        let unwrapped = input.map(Self::unwrap);
        let Some(node) = unwrapped.filter(|&n| is_string_literal_like(n)) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{what} must be a string literal"),
                unwrapped.unwrap_or(at),
            ));
        };
        Ok(node.text().to_owned())
    }

    fn read_string_enum_literal(input: P<Node>) -> Result<String, CompileError> {
        let expr = Self::unwrap(input);
        if is_string_literal_like(expr) {
            return Ok(expr.text().to_owned());
        }
        Err(projection_err(
            "only string literals project (as enum values); numeric, boolean, and bigint literals have no exact projection",
            expr,
        ))
    }

    pub(crate) fn read_non_neg_int_u32(
        input: P<Node>,
        max: u32,
        msg: &str,
    ) -> Result<u32, CompileError> {
        let expr = Self::unwrap(input);
        if expr.kind() != Kind::NumericLiteral {
            return Err(CompileError::new("NORN-FLOW-LITERAL", msg, expr));
        }
        let raw = tsrs_ast::get_source_file_of_node(expr).map_or("", |sf| node_text(sf, expr));
        let valid_plain = raw == "0"
            || (raw
                .bytes()
                .next()
                .is_some_and(|b| (b'1'..=b'9').contains(&b))
                && raw.bytes().all(|b| b.is_ascii_digit()));
        if !valid_plain {
            return Err(CompileError::new("NORN-FLOW-LITERAL", msg, expr));
        }
        let Ok(value) = raw.parse::<u32>() else {
            return Err(CompileError::new("NORN-FLOW-LITERAL", msg, expr));
        };
        if value > max {
            return Err(CompileError::new("NORN-FLOW-LITERAL", msg, expr));
        }
        Ok(value)
    }
}
