//! `EntityIR` v2 property and closed [`ValueSchema`] projection methods for
//! [`SchemaProjection`].
//!
//! Split out of `schema.rs` to keep every module under 800 LOC.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tsrs_ast::{Kind, Node};
use tsrs_core::P;

use crate::checker::resolve::node_text;

use super::canonical::{escaped_json_string_len, is_snake_ident};

use super::schema::{
    ExpandedCost, MAX_SCHEMA_DEPTH, NullableMode, ProjectedProperty, PropertyProjection,
    SchemaNode, SchemaNodeKind, SchemaProjection, flatten_union, projection_err,
};
use super::types::CompileError;
use super::value_schema::{BudgetExceeded, ExpansionBudget, ObjectSchema, ValueSchema};
use super::value_types::{
    DecimalType, PropertyAdditivity, PropertyType, Rounding, TimezonePolicy, UnitRef,
    registry_value_type,
};

const MAX_DECIMAL_SCALE: u32 = 28;

impl SchemaProjection<'_> {
    pub(crate) fn read_norn_call(
        &mut self,
        call: P<Node>,
        args: &[P<Node>],
        id: Option<String>,
    ) -> Result<Rc<SchemaNode>, CompileError> {
        match id.as_deref() {
            Some("@norn/authoring#Norn.rate") => self.read_rate(call, args),
            Some("@norn/authoring#Norn.decimal") => self.read_decimal(call, args),
            Some("@norn/authoring#Norn.boundDecimal") => {
                let group = Self::read_string_literal(
                    args.first().copied(),
                    call,
                    "the decimal binding group",
                )?;
                Ok(self.mk_node(
                    SchemaNodeKind::ValueType(PropertyType::DecimalBinding { group }),
                    call,
                ))
            }
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
                Ok(self.mk_node(
                    SchemaNodeKind::ValueType(PropertyType::Timestamp { timezone }),
                    call,
                ))
            }
            Some("@norn/authoring#Norn.date") => {
                Ok(self.mk_node(SchemaNodeKind::ValueType(PropertyType::Date), call))
            }
            Some(_) | None => {
                let label = id.unwrap_or_else(|| {
                    tsrs_ast::get_source_file_of_node(call).map_or_else(String::new, |sf| {
                        node_text(sf, call.as_call_expression().expression())
                    })
                });
                Err(projection_err(
                    format!("{label} is not a schema in the structural subset"),
                    call,
                ))
            }
        }
    }
    pub(crate) fn read_rate(
        &mut self,
        call: P<Node>,
        args: &[P<Node>],
    ) -> Result<Rc<SchemaNode>, CompileError> {
        let scale_node = args.first().copied().ok_or_else(|| {
            projection_err("@norn/authoring#Norn.rate is missing an argument", call)
        })?;
        let scale = Self::read_non_neg_int_u32(
            scale_node,
            18,
            "Norn.rate scale must be an integer literal from 0 to 18",
        )?;
        let opt_node = args.get(1).copied().ok_or_else(|| {
            projection_err("@norn/authoring#Norn.rate is missing an argument", call)
        })?;
        let options = Self::read_object_literal(opt_node)?;
        if options
            .keys()
            .any(|k| k != "currency" && k != "perUnit" && k != "rounding")
            || !options.contains_key("currency")
            || !options.contains_key("perUnit")
        {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "Norn.rate requires exactly currency and perUnit",
                call,
            ));
        }
        let per_node = options.get("perUnit").copied().ok_or_else(|| {
            CompileError::new("NORN-FLOW-LITERAL", "Norn.rate requires perUnit", call)
        })?;
        let per = Self::read_object_literal(per_node)?;
        let system =
            Self::read_string_literal(per.get("system").copied(), per_node, "perUnit.system")?;
        if per.len() != 2 || (system != "ucum" && system != "qudt") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "perUnit is exactly { system: ucum | qudt, code }",
                per_node,
            ));
        }
        let code = Self::read_string_literal(per.get("code").copied(), per_node, "perUnit.code")?;
        let currency =
            Self::read_string_literal(options.get("currency").copied(), call, "currency")?;
        let rounding = match options.get("rounding").copied() {
            None => None,
            Some(mode_node) => {
                let mode = Self::read_string_literal(Some(mode_node), call, "rounding")?;
                let parsed = Rounding::parse(&mode).ok_or_else(|| {
                    CompileError::new("NORN-FLOW-LITERAL", "unsupported rounding mode", call)
                })?;
                Some(parsed)
            }
        };
        Ok(self.mk_node(
            SchemaNodeKind::ValueType(PropertyType::rate(
                scale,
                currency,
                UnitRef { system, code },
                rounding,
            )),
            call,
        ))
    }

    pub(crate) fn read_decimal(
        &mut self,
        call: P<Node>,
        args: &[P<Node>],
    ) -> Result<Rc<SchemaNode>, CompileError> {
        let scale_node = args.first().copied().ok_or_else(|| {
            projection_err("@norn/authoring#Norn.decimal is missing an argument", call)
        })?;
        let scale = Self::read_non_neg_int_u32(
            scale_node,
            MAX_DECIMAL_SCALE,
            &format!(
                "Norn.decimal takes an integer literal scale between 0 and {MAX_DECIMAL_SCALE}"
            ),
        )?;
        let options = match args.get(1).copied() {
            Some(opt_node) => {
                let map = Self::read_object_literal(opt_node)?;
                for key in map.keys() {
                    if !matches!(
                        key.as_str(),
                        "unit" | "currency" | "rounding" | "additivity" | "unitSource"
                    ) {
                        return Err(CompileError::new(
                            "NORN-FLOW-LITERAL",
                            format!("unknown Norn.decimal option {key}"),
                            opt_node,
                        ));
                    }
                }
                map
            }
            None => BTreeMap::new(),
        };
        let unit_node = options.get("unit").copied();
        let currency_node = options.get("currency").copied();
        let rounding = match options.get("rounding").copied() {
            None => None,
            Some(mode_node) => {
                let mode = Self::read_string_literal(Some(mode_node), call, "rounding")?;
                let parsed = Rounding::parse(&mode).ok_or_else(|| {
                    CompileError::new("NORN-FLOW-LITERAL", "unsupported rounding mode", call)
                })?;
                Some(parsed)
            }
        };
        let additivity = Self::read_additivity(options.get("additivity").copied())?;
        let unit_source = Self::read_unit_source(options.get("unitSource").copied())?;
        let opt_arg = args.get(1).copied().unwrap_or(call);
        let mut dec = Self::read_decimal_core(scale, currency_node, unit_node, opt_arg)?;
        dec.rounding = rounding;
        dec.additivity = additivity;
        dec.unit_source = unit_source;
        Ok(self.mk_node(
            SchemaNodeKind::ValueType(PropertyType::Decimal(Box::new(dec))),
            call,
        ))
    }

    pub(crate) fn read_ref(
        &mut self,
        call: P<Node>,
        args: &[P<Node>],
    ) -> Result<Rc<SchemaNode>, CompileError> {
        let name =
            Self::read_string_literal(args.first().copied(), call, "the property type name")?;
        let options = match args.get(1).copied() {
            Some(opt_node) => {
                let map = Self::read_object_literal(opt_node)?;
                for key in map.keys() {
                    if key != "additivity" && key != "unitSource" {
                        return Err(CompileError::new(
                            "NORN-FLOW-LITERAL",
                            format!("unknown Norn.ref option {key}"),
                            opt_node,
                        ));
                    }
                }
                map
            }
            None => BTreeMap::new(),
        };
        let additivity = Self::read_additivity(options.get("additivity").copied())?;
        let unit_source = Self::read_unit_source(options.get("unitSource").copied())?;
        Ok(self.mk_node(
            SchemaNodeKind::ValueType(PropertyType::Ref {
                name,
                additivity,
                unit_source,
            }),
            call,
        ))
    }

    fn read_unit_source(node: Option<P<Node>>) -> Result<Vec<String>, CompileError> {
        let Some(node) = node else {
            return Ok(Vec::new());
        };
        let list = Self::unwrap(node);
        if list.kind() != Kind::ArrayLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "unitSource is a non-empty literal array of property ids",
                node,
            ));
        }
        let elems = list.as_array_literal_expression().elements.nodes();
        if elems.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "unitSource is a non-empty literal array of property ids",
                node,
            ));
        }
        let mut ids = Vec::with_capacity(elems.len());
        let mut seen = BTreeSet::new();
        for &el in elems {
            let id = Self::read_string_literal(Some(el), node, "a unitSource property id")?;
            if !is_snake_ident(&id) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "unitSource property ids match [a-z][a-z0-9_]*",
                    el,
                ));
            }
            if !seen.insert(id.clone()) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "unitSource property ids are distinct",
                    node,
                ));
            }
            ids.push(id);
        }
        Ok(ids)
    }

    fn read_additivity(node: Option<P<Node>>) -> Result<Option<PropertyAdditivity>, CompileError> {
        let Some(node) = node else {
            return Ok(None);
        };
        let fields = Self::read_object_literal(node)?;
        let kind = Self::read_string_literal(fields.get("kind").copied(), node, "additivity.kind")?;
        if kind == "ratio" && fields.len() == 3 {
            let operand = |field: &str| -> Result<String, CompileError> {
                let member = fields.get(field).copied();
                let value =
                    Self::read_string_literal(member, node, &format!("additivity.{field}"))?;
                if !is_snake_ident(&value) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("additivity.{field} must be a PropertyId matching [a-z][a-z0-9_]*"),
                        member.unwrap_or(node),
                    ));
                }
                Ok(value)
            };
            return Ok(Some(PropertyAdditivity::Ratio {
                numerator: operand("numerator")?,
                denominator: operand("denominator")?,
            }));
        }
        if fields.len() == 1 {
            return match kind.as_str() {
                "undeclared" => Ok(None),
                "additive" => Ok(Some(PropertyAdditivity::Additive)),
                "semi_additive" => Ok(Some(PropertyAdditivity::SemiAdditive)),
                "non_additive" => Ok(Some(PropertyAdditivity::NonAdditive)),
                _ => Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "additivity requires a closed class declaration; ratios require numerator and denominator",
                    node,
                )),
            };
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "additivity requires a closed class declaration; ratios require numerator and denominator",
            node,
        ))
    }

    fn read_decimal_core(
        scale: u32,
        currency_node: Option<P<Node>>,
        unit_node: Option<P<Node>>,
        opt_arg: P<Node>,
    ) -> Result<DecimalType, CompileError> {
        let mut dec = DecimalType {
            scale,
            unit: None,
            currency_source: None,
            per_unit: None,
            rounding: None,
            additivity: None,
            unit_source: Vec::new(),
        };
        if let Some(cur) = currency_node {
            if unit_node.is_some() {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a monetary decimal declares a currency, not a unit",
                    opt_arg,
                ));
            }
            let prop = Self::read_string_literal(Some(cur), opt_arg, "the currency property")?;
            dec.currency_source = Some(super::value_types::CurrencySource {
                links: Vec::new(),
                property: prop,
            });
        } else if let Some(u_node) = unit_node {
            let unit_map = Self::read_object_literal(u_node)?;
            let system =
                Self::read_string_literal(unit_map.get("system").copied(), u_node, "unit.system")?;
            if system != "ucum" && system != "qudt" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "unit.system is \"ucum\" or \"qudt\"",
                    u_node,
                ));
            }
            if unit_map.len() != 2 {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a unit is exactly { system, code }",
                    u_node,
                ));
            }
            let code =
                Self::read_string_literal(unit_map.get("code").copied(), u_node, "unit.code")?;
            dec.unit = Some(UnitRef { system, code });
        }
        Ok(dec)
    }

    fn expanded_cost(&mut self, schema: &SchemaNode) -> Result<ExpandedCost, CompileError> {
        if let Some(&known) = self.sizes.get(&schema.id) {
            return Ok(known);
        }
        self.guard(schema.node)?;
        let node_cap = self.max_expanded.saturating_add(1);
        let byte_cap = self.max_output_bytes.saturating_add(1);
        let mut nodes = 1_usize;
        let bytes = match &schema.kind {
            SchemaNodeKind::Literals(vals) => {
                nodes = nodes.saturating_add(vals.len()).min(node_cap);
                let mut total = 27_usize.min(byte_cap);
                for v in vals {
                    total = total
                        .saturating_add(escaped_json_string_len(v).saturating_add(1))
                        .min(byte_cap);
                    if total >= byte_cap {
                        break;
                    }
                }
                total
            }
            SchemaNodeKind::Struct(fields) => {
                let mut total = 2_usize.min(byte_cap);
                for f in fields {
                    let child = self.expanded_cost(&f.schema)?;
                    nodes = nodes.saturating_add(child.nodes).min(node_cap);
                    let key_bytes = escaped_json_string_len(&f.name).saturating_add(3);
                    total = total
                        .saturating_add(key_bytes)
                        .saturating_add(child.bytes)
                        .min(byte_cap);
                    if nodes >= node_cap || total >= byte_cap {
                        break;
                    }
                }
                total
            }
            SchemaNodeKind::Array(items) => {
                let child = self.expanded_cost(items)?;
                nodes = nodes.saturating_add(child.nodes).min(node_cap);
                24_usize.saturating_add(child.bytes).min(byte_cap)
            }
            SchemaNodeKind::Union(members) => {
                let mut total = 2_usize.min(byte_cap);
                for m in members {
                    let child = self.expanded_cost(m)?;
                    nodes = nodes.saturating_add(child.nodes).min(node_cap);
                    total = total
                        .saturating_add(child.bytes)
                        .saturating_add(1)
                        .min(byte_cap);
                    if nodes >= node_cap || total >= byte_cap {
                        break;
                    }
                }
                total
            }
            SchemaNodeKind::ValueType(vt) => vt.estimated_json_bytes().min(byte_cap),
            SchemaNodeKind::String
            | SchemaNodeKind::Int
            | SchemaNodeKind::Finite
            | SchemaNodeKind::Boolean
            | SchemaNodeKind::Null => 15_usize.min(byte_cap),
        };
        let cost = ExpandedCost { nodes, bytes };
        self.sizes.insert(schema.id, cost);
        Ok(cost)
    }

    fn bounded(
        &mut self,
        schema: &SchemaNode,
        expansion: &mut ExpansionBudget,
    ) -> Result<(), CompileError> {
        if schema.height > MAX_SCHEMA_DEPTH {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "schema nesting exceeds the compiler bound",
                schema.node,
            ));
        }
        let cost = self.expanded_cost(schema)?;
        if cost.nodes > self.max_expanded || cost.bytes > self.max_output_bytes {
            return Err(CompileError::new(
                "NORN-FLOW-UNBOUNDED",
                "schema expands beyond the compiler size or byte bound (shared definitions are counted at every use)",
                schema.node,
            ));
        }
        expansion
            .charge_literal(cost.nodes, cost.bytes)
            .map_err(|BudgetExceeded(msg)| {
                CompileError::new("NORN-FLOW-UNBOUNDED", msg, schema.node)
            })
    }

    /// Projects a struct schema to `EntityIR` v2 properties in declaration order.
    pub(crate) fn properties(
        &mut self,
        schema: &SchemaNode,
        expansion: &mut ExpansionBudget,
    ) -> Result<Vec<ProjectedProperty>, CompileError> {
        self.bounded(schema, expansion)?;
        let SchemaNodeKind::Struct(fields) = &schema.kind else {
            return Err(projection_err(
                "a type's properties are a Schema.Struct (or a Schema.Class)",
                schema.node,
            ));
        };
        let mut out = Vec::with_capacity(fields.len());
        for f in fields {
            if f.optional {
                return Err(projection_err(
                    "EntityIR v2 has one absence policy, nullable: write Schema.NullOr(...) instead of Schema.optionalKey(...)",
                    f.node,
                ));
            }
            out.push(ProjectedProperty {
                id: f.name.clone(),
                projection: self.property(&f.schema)?,
                node: f.node,
            });
        }
        Ok(out)
    }

    /// Projects one schema node to an `EntityIR` v2 property type and nullability.
    pub(crate) fn property(&self, schema: &SchemaNode) -> Result<PropertyProjection, CompileError> {
        match &schema.kind {
            SchemaNodeKind::String => Ok(PropertyProjection {
                prop_type: PropertyType::text(),
                nullable: false,
            }),
            SchemaNodeKind::Int => Ok(PropertyProjection {
                prop_type: PropertyType::integer(),
                nullable: false,
            }),
            SchemaNodeKind::Boolean => Ok(PropertyProjection {
                prop_type: PropertyType::Boolean,
                nullable: false,
            }),
            SchemaNodeKind::ValueType(vt) => Ok(PropertyProjection {
                prop_type: vt.clone(),
                nullable: false,
            }),
            SchemaNodeKind::Literals(vals) => Ok(PropertyProjection {
                prop_type: PropertyType::enumeration(vals.clone()),
                nullable: false,
            }),
            SchemaNodeKind::Finite => Err(projection_err(
                "EntityIR has no binary floating-point kind: use Schema.Int or Norn.decimal(scale)",
                schema.node,
            )),
            SchemaNodeKind::Null => Err(projection_err(
                "a property cannot hold only null",
                schema.node,
            )),
            SchemaNodeKind::Struct(_) | SchemaNodeKind::Array(_) => Err(projection_err(
                "EntityIR v2 properties are scalar: model a nested record as its own type with a link, and a list as a to-many link",
                schema.node,
            )),
            SchemaNodeKind::Union(_) => self.property_union(schema),
        }
    }

    fn property_union(&self, schema: &SchemaNode) -> Result<PropertyProjection, CompileError> {
        let members = flatten_union(schema);
        let present: Vec<&SchemaNode> = members
            .iter()
            .copied()
            .filter(|m| !matches!(m.kind, SchemaNodeKind::Null))
            .collect();
        let nullable = present.len() < members.len();
        if present.is_empty() {
            return Err(projection_err(
                "a property cannot hold only null",
                schema.node,
            ));
        }
        if present
            .iter()
            .all(|m| matches!(m.kind, SchemaNodeKind::Literals(_)))
        {
            let mut values = Vec::new();
            let mut seen = BTreeSet::new();
            for m in present {
                if let SchemaNodeKind::Literals(vs) = &m.kind {
                    for v in vs {
                        if seen.insert(v.as_str()) {
                            values.push(v.clone());
                        }
                    }
                }
            }
            return Ok(PropertyProjection {
                prop_type: PropertyType::enumeration(values),
                nullable,
            });
        }
        if present.len() == 1
            && let Some(&only) = present.first()
        {
            let inner = self.property(only)?;
            return Ok(PropertyProjection {
                prop_type: inner.prop_type,
                nullable: nullable || inner.nullable,
            });
        }
        Err(projection_err(
            "only literal unions (enum) and Schema.NullOr project to EntityIR properties",
            schema.node,
        ))
    }

    /// Projects a schema node to a closed [`ValueSchema`].
    pub(crate) fn closed(
        &mut self,
        schema: &SchemaNode,
        nullable: NullableMode,
        temporal_wire: bool,
        expansion: &mut ExpansionBudget,
    ) -> Result<ValueSchema, CompileError> {
        self.bounded(schema, expansion)?;
        self.closed_at(schema, nullable, temporal_wire)
    }

    /// Wire types for syntax-only expressions (`Norn.expression`).
    pub(crate) fn expression_input(
        &mut self,
        schema: &SchemaNode,
        expansion: &mut ExpansionBudget,
    ) -> Result<ValueSchema, CompileError> {
        self.bounded(schema, expansion)?;
        self.closed_at(schema, NullableMode::Any, true)
    }

    fn closed_at(
        &self,
        schema: &SchemaNode,
        nullable: NullableMode,
        temporal_wire: bool,
    ) -> Result<ValueSchema, CompileError> {
        self.guard(schema.node)?;
        match &schema.kind {
            SchemaNodeKind::String => Ok(ValueSchema::text()),
            SchemaNodeKind::Int => Ok(ValueSchema::integer()),
            SchemaNodeKind::Finite => Ok(ValueSchema::number()),
            SchemaNodeKind::Boolean => Ok(ValueSchema::boolean()),
            SchemaNodeKind::Null => Ok(ValueSchema::null_value()),
            SchemaNodeKind::Literals(vals) => Ok(ValueSchema::text_enum(vals.clone())),
            SchemaNodeKind::Array(items) => Ok(ValueSchema::array(self.closed_at(
                items,
                nullable,
                temporal_wire,
            )?)),
            SchemaNodeKind::Struct(fields) => {
                let mut seen = BTreeSet::new();
                let mut ordered = Vec::with_capacity(fields.len());
                let mut optional = Vec::new();
                for f in fields {
                    if !seen.insert(f.name.as_str()) {
                        return Err(projection_err(
                            format!("duplicate member {}", f.name),
                            f.node,
                        ));
                    }
                    let member = self.closed_at(&f.schema, nullable, temporal_wire)?;
                    ordered.push((f.name.clone(), member));
                    if f.optional {
                        optional.push(f.name.clone());
                    }
                }
                Ok(ValueSchema::object(ObjectSchema::record_ordered(
                    ordered, &optional,
                )))
            }
            SchemaNodeKind::Union(_) => self.closed_union(schema, nullable, temporal_wire),
            SchemaNodeKind::ValueType(declared) => {
                Self::closed_value_type(schema, declared, temporal_wire)
            }
        }
    }

    fn closed_union(
        &self,
        schema: &SchemaNode,
        nullable: NullableMode,
        temporal_wire: bool,
    ) -> Result<ValueSchema, CompileError> {
        let members = flatten_union(schema);
        if nullable != NullableMode::None
            && members.len() == 2
            && members
                .iter()
                .any(|m| matches!(m.kind, SchemaNodeKind::Null))
            && let Some(&non_null) = members
                .iter()
                .find(|m| !matches!(m.kind, SchemaNodeKind::Null))
        {
            let is_scalar_node = match &non_null.kind {
                SchemaNodeKind::String
                | SchemaNodeKind::Int
                | SchemaNodeKind::Finite
                | SchemaNodeKind::Boolean
                | SchemaNodeKind::Literals(_) => true,
                SchemaNodeKind::ValueType(vt) => {
                    matches!(vt, PropertyType::Decimal(_) | PropertyType::Ref { .. })
                        || (temporal_wire
                            && matches!(vt, PropertyType::Timestamp { .. } | PropertyType::Date))
                }
                SchemaNodeKind::Null
                | SchemaNodeKind::Struct(_)
                | SchemaNodeKind::Array(_)
                | SchemaNodeKind::Union(_) => false,
            };
            if nullable == NullableMode::Scalar && !is_scalar_node {
                return Err(projection_err(
                    "an action's absent value is a nullable scalar: Schema.NullOr of a string, literal set, integer, number, boolean or Norn.decimal (ActionIR admits no nullable struct or array)",
                    schema.node,
                ));
            }
            let inner = self.closed_at(non_null, nullable, temporal_wire)?;
            return Ok(inner.into_nullable());
        }
        if members
            .iter()
            .all(|m| matches!(m.kind, SchemaNodeKind::Literals(_)))
        {
            let mut vals = Vec::new();
            let mut seen = BTreeSet::new();
            for m in members {
                if let SchemaNodeKind::Literals(vs) = &m.kind {
                    for v in vs {
                        if seen.insert(v.as_str()) {
                            vals.push(v.clone());
                        }
                    }
                }
            }
            return Ok(ValueSchema::text_enum(vals));
        }
        Err(projection_err(
            "closed schemas have no unions: use a literal union, or Schema.optionalKey for an absent member",
            schema.node,
        ))
    }

    fn closed_value_type(
        schema: &SchemaNode,
        declared: &PropertyType,
        temporal_wire: bool,
    ) -> Result<ValueSchema, CompileError> {
        let resolved = match declared {
            PropertyType::Ref { name, .. } => {
                registry_value_type(name).unwrap_or_else(|| declared.clone())
            }
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Decimal(_)
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Boolean
            | PropertyType::Timestamp { .. }
            | PropertyType::Date
            | PropertyType::Duration
            | PropertyType::Enum { .. } => declared.clone(),
        };
        match resolved {
            PropertyType::Decimal(dec) => {
                if dec.scale > 18 {
                    return Err(projection_err(
                        "closed exact sites require a scale from 0 to 18",
                        schema.node,
                    ));
                }
                Ok(ValueSchema::exact_decimal(*dec))
            }
            PropertyType::Integer { .. } => Ok(ValueSchema::integer()),
            PropertyType::Boolean => Ok(ValueSchema::boolean()),
            PropertyType::Text { .. } if matches!(declared, PropertyType::Ref { .. }) => {
                Ok(ValueSchema::text())
            }
            PropertyType::Timestamp { .. } | PropertyType::Date if temporal_wire => {
                Ok(ValueSchema::text())
            }
            other @ (PropertyType::Text { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date
            | PropertyType::Duration
            | PropertyType::Enum { .. }
            | PropertyType::Ref { .. }) => {
                let source_kind = if matches!(declared, PropertyType::Ref { .. }) {
                    "ref"
                } else {
                    declared.kind_name()
                };
                Err(projection_err(
                    format!(
                        "Norn.{source_kind} projects to EntityIR properties only; closed schemas have no {} type yet",
                        other.kind_name()
                    ),
                    schema.node,
                ))
            }
        }
    }
}
