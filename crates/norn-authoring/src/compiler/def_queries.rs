//! Question (`Norn.question`), dashboard (`Norn.dashboard`), metric-id
//! (`Norn.metricId`), and exported Effect Schema value-schema lowering split
//! out of [`super::def_laws`].

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node, SourceFile};
use tsrs_checker::Checker;
use tsrs_core::P;

use crate::checker::resolve::resolve_symbol;

use super::canonical::{is_snake_ident, to_canonical_json};
use super::def_helpers::{
    DASHBOARD, DefinitionCompiler, METRIC_ID, Members, OBJECT_TYPES, QUESTION,
};
use super::schema::NullableMode;
use super::types::{CompileError, is_string_literal_like};
use super::value_schema::{ExpansionBudget, ValueSchema};

const VALUE_TYPE_CALLS: &[&str] = &[
    "@norn/authoring#Norn.boundDecimal",
    "@norn/authoring#Norn.rate",
    "@norn/authoring#Norn.decimal",
    "@norn/authoring#Norn.ref",
    "@norn/authoring#Norn.timestamp",
    "@norn/authoring#Norn.date",
];

impl DefinitionCompiler<'_> {
    /// Compiles all named `Norn.question` definitions in `source`.
    pub(crate) fn compile_questions(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        source: P<SourceFile>,
    ) -> Result<Vec<Value>, CompileError> {
        let map = self.question_bindings(checker, expansion, source)?;
        if map.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "the module declares no Norn.question",
                source.as_node(),
            ));
        }
        Ok(map.into_iter().map(|(_, q)| q).collect())
    }

    fn question_bindings(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        source: P<SourceFile>,
    ) -> Result<Vec<(String, Value)>, CompileError> {
        let mut questions: Vec<(String, Value)> = Vec::new();
        let mut names: HashSet<String> = HashSet::new();
        for &stmt in source.as_node().statements() {
            if stmt.kind() == Kind::ExportAssignment {
                let call = Self::unwrap(stmt.as_export_assignment().expression());
                if call.kind() == Kind::CallExpression
                    && self.call_id(checker, call).as_deref() == Some(QUESTION)
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-RESULT",
                        "questions require named const bindings",
                        call,
                    ));
                }
            }
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            for &decl in stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes()
            {
                let Some(raw_init) = decl.as_variable_declaration().initializer() else {
                    continue;
                };
                let call = Self::unwrap(raw_init);
                if call.kind() != Kind::CallExpression
                    || self.call_id(checker, call).as_deref() != Some(QUESTION)
                {
                    continue;
                }
                let q = self.compile_one_question(checker, expansion, call, &mut names)?;
                let binding_name = decl.as_variable_declaration().name().text().to_owned();
                questions.push((binding_name, q));
            }
        }
        Ok(questions)
    }

    fn compile_one_question(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        names: &mut HashSet<String>,
    ) -> Result<Value, CompileError> {
        let input = self.meta(
            checker,
            call,
            &["name", "of", "where", "measure", "by", "asOf", "population"],
        )?;
        let name = self.string(checker, Self::member(&input, "name", call)?)?;
        if !names.insert(name.clone()) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("duplicate question {name}"),
                call,
            ));
        }
        let pop_node = Self::member(&input, "population", call)?;
        let pop = self.closed_record(checker, pop_node, &["kind"], &["kind"])?;
        if self.string(checker, Self::member(&pop, "kind", pop_node)?)? != "authorized_objects" {
            return Err(CompileError::new(
                "core.compute.unsupported_engine",
                "this question profile requires the host-authorized exact cell population",
                pop_node,
            ));
        }
        let (win, fr) =
            self.compile_question_as_of(checker, expansion, Self::member(&input, "asOf", call)?)?;
        let filters = self.compile_question_filters(
            checker,
            expansion,
            Self::member(&input, "where", call)?,
        )?;
        let group_by =
            self.compile_question_group_by(checker, Self::member(&input, "by", call)?)?;

        let mut agg = Map::new();
        agg.insert(
            "metric".to_owned(),
            Value::String(self.compile_measure(checker, Self::member(&input, "measure", call)?)?),
        );

        let mut q = Map::new();
        q.insert("irVersion".to_owned(), Value::String("0.1".to_owned()));
        q.insert("name".to_owned(), Value::String(name));
        q.insert(
            "objectSet".to_owned(),
            Value::String(self.type_name(
                checker,
                Self::member(&input, "of", call)?,
                OBJECT_TYPES,
            )?),
        );
        q.insert(
            "aggregate".to_owned(),
            Value::Array(vec![Value::Object(agg)]),
        );
        if !filters.is_empty() {
            q.insert("filter".to_owned(), Value::Array(filters));
        }
        if !group_by.is_empty() {
            q.insert("groupBy".to_owned(), Value::Array(group_by));
        }
        q.insert("window".to_owned(), Value::Object(win));
        q.insert("calendar".to_owned(), Value::String("iso_utc".to_owned()));
        q.insert("freshness".to_owned(), Value::Object(fr));

        let raw = Value::Object(q);
        let canon = to_canonical_json(&raw)
            .map_err(|err| CompileError::new("NORN-FLOW-LITERAL", err.to_string(), call))?;
        Self::assert_expansion(expansion, &canon, call)?;
        Ok(canon)
    }

    fn compile_question_as_of(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        time_node: P<Node>,
    ) -> Result<(Map<String, Value>, Map<String, Value>), CompileError> {
        let time = self.closed_record(
            checker,
            time_node,
            &["basis", "from", "to", "freshness"],
            &["basis"],
        )?;
        if self.string(checker, Self::member(&time, "basis", time_node)?)? != "effective" {
            return Err(CompileError::new(
                "core.compute.unsupported_engine",
                "knowledge-time questions require authenticated cell history and qualified ASOF-KNOWLEDGE-1 policy admission (#497); a caller timestamp is not a cutoff",
                time_node,
            ));
        }
        let fresh_node = Self::member(&time, "freshness", time_node)?;
        let fresh = self.closed_record(
            checker,
            fresh_node,
            &["kind", "maxAgeMs"],
            &["kind", "maxAgeMs"],
        )?;
        let f_kind = self.string(checker, Self::member(&fresh, "kind", fresh_node)?)?;
        if f_kind != "live" && f_kind != "delayed" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "effective questions declare live or delayed freshness",
                fresh_node,
            ));
        }
        let age_node = Self::member(&fresh, "maxAgeMs", fresh_node)?;
        let age = self.json(checker, expansion, age_node, 0)?;
        if !age.as_i64().is_some_and(|v| v > 0) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "freshness requires a positive exact integer age",
                age_node,
            ));
        }
        let mut win = Map::new();
        win.insert(
            "start".to_owned(),
            Value::String(self.string(checker, Self::member(&time, "from", time_node)?)?),
        );
        win.insert(
            "end".to_owned(),
            Value::String(self.string(checker, Self::member(&time, "to", time_node)?)?),
        );
        let mut fr = Map::new();
        fr.insert("kind".to_owned(), Value::String(f_kind));
        fr.insert("maxAgeMs".to_owned(), age);
        Ok((win, fr))
    }

    fn compile_question_filters(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        where_node: P<Node>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut filters = Vec::new();
        for node in self.list(checker, where_node)? {
            let f = self.closed_record(
                checker,
                node,
                &["link", "links", "property", "operator", "value"],
                &["property", "operator", "value"],
            )?;
            let op = self.string(checker, Self::member(&f, "operator", node)?)?;
            if op != "eq" && op != "ne" {
                return Err(CompileError::new(
                    "core.compute.unsupported_engine",
                    "the qualified compute profile admits Eq/Ne predicates only",
                    node,
                ));
            }
            let val_node = Self::member(&f, "value", node)?;
            if self.deref(checker, val_node)?.kind() == Kind::CallExpression {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "question predicates use scalar literals, not expression escapes",
                    val_node,
                ));
            }
            let val = self.json(checker, expansion, val_node, 0)?;
            if !matches!(val, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a question filter requires a scalar literal",
                    node,
                ));
            }
            let links = self.question_links(checker, &f, node)?;
            let prop = self.string(checker, Self::member(&f, "property", node)?)?;
            let mut fo = Map::new();
            fo.insert("path".to_owned(), question_path_json(&links, &prop));
            fo.insert("operator".to_owned(), Value::String(op));
            fo.insert("value".to_owned(), val);
            filters.push(Value::Object(fo));
        }
        Ok(filters)
    }

    fn compile_question_group_by(
        &self,
        checker: &mut Checker,
        by_node: P<Node>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut group_by = Vec::new();
        for node in self.list(checker, by_node)? {
            let dim = self.closed_record(
                checker,
                node,
                &["link", "links", "property", "grain"],
                &["property"],
            )?;
            let links = self.question_links(checker, &dim, node)?;
            let prop = self.string(checker, Self::member(&dim, "property", node)?)?;
            let mut go = Map::new();
            go.insert("path".to_owned(), question_path_json(&links, &prop));
            if let Some(g_node) = dim.get("grain") {
                let g = self.string(checker, g_node)?;
                if !matches!(g.as_str(), "day" | "week" | "month" | "quarter" | "year") {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "unknown calendar grain",
                        g_node,
                    ));
                }
                go.insert("grain".to_owned(), Value::String(g));
            }
            group_by.push(Value::Object(go));
        }
        Ok(group_by)
    }

    fn question_links(
        &self,
        checker: &mut Checker,
        fields: &Members,
        node: P<Node>,
    ) -> Result<Vec<String>, CompileError> {
        let one = fields.get("link");
        let many = fields.get("links");
        if one.is_some() && many.is_some() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a question path declares link or links, not both",
                node,
            ));
        }
        let links = match (one, many) {
            (Some(o), _) => vec![self.string(checker, o)?],
            (_, Some(m)) => self.strings(checker, m)?,
            (None, None) => Vec::new(),
        };
        for link in &links {
            if !is_snake_ident(link) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a link id matches [a-z][a-z0-9_]*",
                    node,
                ));
            }
        }
        Ok(links)
    }

    fn compile_measure(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<String, CompileError> {
        let e = self.deref(checker, input)?;
        if e.kind() != Kind::CallExpression
            || self.call_id(checker, e).as_deref() != Some(METRIC_ID)
        {
            return self.string(checker, input);
        }
        let spec_node = e
            .as_call_expression()
            .arguments()
            .nodes()
            .first()
            .copied()
            .unwrap_or(e);
        let spec = self.closed_record(
            checker,
            spec_node,
            &["kind", "of", "property", "time"],
            &["kind", "of", "property", "time"],
        )?;
        let kind_node = Self::member(&spec, "kind", spec_node)?;
        let kind = self.string(checker, kind_node)?;
        if !matches!(kind.as_str(), "sum" | "as_of" | "ratio") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a metric kind is sum, as_of or ratio",
                kind_node,
            ));
        }
        let time_node = Self::member(&spec, "time", spec_node)?;
        let time_lit = self.deref(checker, time_node)?;
        let (links, time_prop) = if is_string_literal_like(time_lit) {
            (Vec::new(), time_lit.text().to_owned())
        } else {
            let fields = self.closed_record(
                checker,
                time_node,
                &["link", "links", "property"],
                &["property"],
            )?;
            (
                self.question_links(checker, &fields, time_node)?,
                self.string(checker, Self::member(&fields, "property", time_node)?)?,
            )
        };
        let of = self.type_name(checker, Self::member(&spec, "of", spec_node)?, OBJECT_TYPES)?;
        let property = self.string(checker, Self::member(&spec, "property", spec_node)?)?;
        derived_metric_id(&kind, &of, &property, &links, &time_prop)
            .map_err(|msg| CompileError::new("NORN-FLOW-LITERAL", msg, e))
    }

    /// Compiles all named `Norn.dashboard` definitions in `source`.
    pub(crate) fn compile_dashboards(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        source: P<SourceFile>,
    ) -> Result<Vec<Value>, CompileError> {
        let questions: BTreeMap<String, Value> = self
            .question_bindings(checker, expansion, source)?
            .into_iter()
            .collect();
        let mut dashboards = Vec::new();
        for &stmt in source.as_node().statements() {
            if stmt.kind() == Kind::ExportAssignment {
                let call = Self::unwrap(stmt.as_export_assignment().expression());
                if call.kind() == Kind::CallExpression
                    && self.call_id(checker, call).as_deref() == Some(DASHBOARD)
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-RESULT",
                        "dashboards require named const bindings",
                        call,
                    ));
                }
            }
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            for &decl in stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes()
            {
                let Some(raw_init) = decl.as_variable_declaration().initializer() else {
                    continue;
                };
                let call = Self::unwrap(raw_init);
                if call.kind() != Kind::CallExpression
                    || self.call_id(checker, call).as_deref() != Some(DASHBOARD)
                {
                    continue;
                }
                dashboards.push(self.compile_one_dashboard(checker, call, &questions)?);
            }
        }
        if dashboards.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "the module declares no Norn.dashboard",
                source.as_node(),
            ));
        }
        Ok(dashboards)
    }

    fn compile_one_dashboard(
        &self,
        checker: &mut Checker,
        call: P<Node>,
        questions: &BTreeMap<String, Value>,
    ) -> Result<Value, CompileError> {
        let input = self.meta(checker, call, &["title", "tiles"])?;
        let mut x: u32 = 0;
        let mut y: u32 = 0;
        let mut tiles = Vec::new();
        for (index, node) in self
            .list(checker, Self::member(&input, "tiles", call)?)?
            .into_iter()
            .enumerate()
        {
            tiles.push(self.build_dashboard_tile(checker, index, node, questions, &mut x, &mut y)?);
        }
        let mut origin = Map::new();
        origin.insert("by".to_owned(), Value::String("code".to_owned()));
        let mut db = Map::new();
        db.insert(
            "title".to_owned(),
            Value::String(self.string(checker, Self::member(&input, "title", call)?)?),
        );
        db.insert("tiles".to_owned(), Value::Array(tiles));
        db.insert("origin".to_owned(), Value::Object(origin));
        let raw = Value::Object(db);
        to_canonical_json(&raw)
            .map_err(|err| CompileError::new("NORN-FLOW-LITERAL", err.to_string(), call))
    }

    fn build_dashboard_tile(
        &self,
        checker: &mut Checker,
        index: usize,
        node: P<Node>,
        questions: &BTreeMap<String, Value>,
        x: &mut u32,
        y: &mut u32,
    ) -> Result<Value, CompileError> {
        let tile = self.closed_record(
            checker,
            node,
            &["title", "question", "view", "width"],
            &["title", "question", "view"],
        )?;
        let q_ref = Self::member(&tile, "question", node)?;
        let question = if q_ref.kind() == Kind::Identifier {
            questions.get(q_ref.text()).cloned()
        } else {
            None
        };
        let Some(question) = question else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a tile's question names a Norn.question declared in this module",
                q_ref,
            ));
        };
        let view_node = Self::member(&tile, "view", node)?;
        let view = self.string(checker, view_node)?;
        if !matches!(
            view.as_str(),
            "number" | "line" | "area" | "bar" | "pie" | "table"
        ) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a view is number, line, area, bar, pie or table",
                view_node,
            ));
        }
        let width = self.read_tile_width(checker, &tile, &view)?;
        let w: u32 = if width == "full" { 2 } else { 1 };
        if x.saturating_add(w) > 2 {
            *x = 0;
            *y = y.saturating_add(1);
        }
        let mut layout = Map::new();
        layout.insert("x".to_owned(), Value::Number((*x).into()));
        layout.insert("y".to_owned(), Value::Number((*y).into()));
        layout.insert("w".to_owned(), Value::Number(w.into()));
        layout.insert("h".to_owned(), Value::Number(1_u32.into()));
        *x = x.saturating_add(w);
        if *x >= 2 {
            *x = 0;
            *y = y.saturating_add(1);
        }
        let mut view_obj = Map::new();
        if view == "number" || view == "table" {
            view_obj.insert("kind".to_owned(), Value::String(view));
        } else {
            view_obj.insert("kind".to_owned(), Value::String("chart".to_owned()));
            view_obj.insert("chart".to_owned(), Value::String(view));
        }
        let mut t_obj = Map::new();
        t_obj.insert(
            "id".to_owned(),
            Value::String(format!("t{}", index.saturating_add(1))),
        );
        t_obj.insert(
            "title".to_owned(),
            Value::String(self.string(checker, Self::member(&tile, "title", node)?)?),
        );
        t_obj.insert("question".to_owned(), question);
        t_obj.insert("view".to_owned(), Value::Object(view_obj));
        t_obj.insert("layout".to_owned(), Value::Object(layout));
        Ok(Value::Object(t_obj))
    }

    fn read_tile_width(
        &self,
        checker: &mut Checker,
        tile: &Members,
        view: &str,
    ) -> Result<String, CompileError> {
        let Some(w_node) = tile.get("width") else {
            return Ok(if view == "number" || view == "pie" {
                "half".to_owned()
            } else {
                "full".to_owned()
            });
        };
        let w = self.string(checker, w_node)?;
        if w != "half" && w != "full" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a width is half or full",
                w_node,
            ));
        }
        Ok(w)
    }

    /// Compiles every exported top-level Effect Schema const in `source` to its closed [`ValueSchema`].
    pub(crate) fn compile_value_schemas(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        source: P<SourceFile>,
    ) -> Result<BTreeMap<String, ValueSchema>, CompileError> {
        let mut out = BTreeMap::new();
        for &stmt in source.as_node().statements() {
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            let is_export = stmt
                .as_variable_statement()
                .modifiers()
                .is_some_and(|mods| {
                    mods.nodes()
                        .iter()
                        .any(|&m| m.kind() == Kind::ExportKeyword)
                });
            if !is_export {
                continue;
            }
            for &decl in stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes()
            {
                let vd = decl.as_variable_declaration();
                if vd.name().kind() != Kind::Identifier {
                    continue;
                }
                let Some(init) = vd.initializer() else {
                    continue;
                };
                if !self.is_schema_syntax(checker, init)? {
                    continue;
                }
                let read = self.schemas.read(checker, init)?;
                let closed = self
                    .schemas
                    .closed(&read, NullableMode::None, false, expansion)?;
                out.insert(vd.name().text().to_owned(), closed);
            }
        }
        Ok(out)
    }

    fn is_schema_syntax(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<bool, CompileError> {
        let e = self.deref(checker, input)?;
        let kind = e.kind();
        let target = if kind == Kind::CallExpression {
            let callee = e.as_call_expression().expression();
            if callee.kind() == Kind::PropertyAccessExpression {
                Some(callee.as_property_access_expression().name())
            } else {
                Some(callee)
            }
        } else if kind == Kind::PropertyAccessExpression {
            Some(e.as_property_access_expression().name())
        } else if kind == Kind::Identifier {
            Some(e)
        } else {
            None
        };
        let Some(name_node) = target else {
            return Ok(false);
        };
        let id = resolve_symbol(
            checker,
            self.intrinsics_module,
            self.effect_dist_prefix,
            self.vfs,
            name_node,
        )
        .id;
        Ok(id.as_deref().is_some_and(|s| {
            s.starts_with("effect/Schema#")
                || s == "effect/Pipeable#Pipeable.pipe"
                || s == "effect/Function#pipe"
                || VALUE_TYPE_CALLS.contains(&s)
        }))
    }
}

fn question_path_json(links: &[String], property: &str) -> Value {
    let mut obj = Map::new();
    if !links.is_empty() {
        obj.insert(
            "links".to_owned(),
            Value::Array(links.iter().cloned().map(Value::String).collect()),
        );
    }
    obj.insert("property".to_owned(), Value::String(property.to_owned()));
    Value::Object(obj)
}

fn encoded_hex_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().saturating_mul(2));
    for byte in name.as_bytes() {
        let hi = byte >> 4;
        let lo = byte & 0x0f;
        let hex = |n: u8| -> char {
            if n < 10 {
                char::from(b'0'.saturating_add(n))
            } else {
                char::from(b'a'.saturating_add(n.saturating_sub(10)))
            }
        };
        out.push(hex(hi));
        out.push(hex(lo));
    }
    out
}

fn metric_field_name(links: &[String], property: &str) -> String {
    if links.is_empty()
        && property.len() >= 2
        && !property.ends_with('_')
        && !property.contains("__")
    {
        return property.to_owned();
    }
    let parts: Vec<String> = links
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(property))
        .map(|c| format!("{}_{}", c.len(), c.replace('_', "_u")))
        .collect();
    format!("norn_path_{}_value", parts.join("_"))
}

fn derived_metric_id(
    kind: &str,
    of: &str,
    property: &str,
    links: &[String],
    time_prop: &str,
) -> Result<String, String> {
    let object = format!("analysis.object_{}.{property}", encoded_hex_name(of));
    if kind == "as_of" {
        if !links.is_empty() {
            return Err(
                "METRIC-SNAPSHOT-ASOF-1: a snapshot keeps its own valid time; it has no linked time"
                    .to_owned(),
            );
        }
        return Ok(format!("{object}.as_of_{time_prop}"));
    }
    let time_seg = if links.is_empty() {
        time_prop.to_owned()
    } else {
        metric_field_name(links, time_prop)
    };
    let sum = format!("{object}.by_{time_seg}");
    Ok(if kind == "ratio" {
        format!("{sum}.ratio_of_sums")
    } else {
        sum
    })
}
