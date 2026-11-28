//! Action effects, quantity effects, commands, and aggregates split out of
//! [`super::def_actions`].

use serde_json::{Map, Value};
use tsrs_ast::Node;
use tsrs_checker::Checker;
use tsrs_core::P;

use super::def_helpers::{DefinitionCompiler, Members, OBJECT_TYPES};
use super::types::CompileError;
use super::value_schema::ExpansionBudget;

impl DefinitionCompiler<'_> {
    pub(super) fn compile_action_effect(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<(Value, Option<String>), CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &[
                "kind",
                "type",
                "objectIdField",
                "lifecycle",
                "transition",
                "writeMode",
                "acceptReceipt",
                "setFields",
                "parentValuesField",
                "rowOpsField",
                "parentVersionField",
                "parentDeleteField",
                "collections",
            ],
            &["kind", "type", "objectIdField"],
        )?;
        let kind_node = Self::member(&m, "kind", input)?;
        let kind = self.string(checker, kind_node)?;
        let (t_name, t_ver) = self.type_ref(
            checker,
            Self::member(&m, "type", input)?,
            OBJECT_TYPES,
            "a Norn.type definition",
        )?;
        let obj_id_field = self.string(checker, Self::member(&m, "objectIdField", input)?)?;

        let mut obj = Map::new();
        obj.insert("kind".to_owned(), Value::String(kind.clone()));
        obj.insert("objectIdField".to_owned(), Value::String(obj_id_field));
        obj.insert("objectKind".to_owned(), Value::String(t_name.clone()));
        obj.insert(
            "entityArtifactId".to_owned(),
            Value::String(format!("{t_name}.entity")),
        );
        obj.insert("entityArtifactVersion".to_owned(), Value::String(t_ver));

        match kind.as_str() {
            "upsert_input_object" => self.finish_effect_upsert(checker, &m, obj),
            "mutate_parent_with_rows" => {
                self.finish_effect_mutate_rows(checker, expansion, &m, input, obj)
            }
            "fire_transition" => self.finish_effect_fire_transition(checker, &m, input, obj),
            _ => Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "an effect kind is fire_transition, upsert_input_object, or mutate_parent_with_rows",
                kind_node,
            )),
        }
    }

    fn finish_effect_upsert(
        &self,
        checker: &mut Checker,
        m: &Members,
        mut obj: Map<String, Value>,
    ) -> Result<(Value, Option<String>), CompileError> {
        for extra in [
            "lifecycle",
            "transition",
            "acceptReceipt",
            "setFields",
            "parentValuesField",
            "rowOpsField",
            "parentVersionField",
            "parentDeleteField",
            "collections",
        ] {
            if let Some(ex) = m.get(extra) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("upsert_input_object takes no {extra}"),
                    ex,
                ));
            }
        }
        if let Some(wm) = m.get("writeMode") {
            obj.insert(
                "writeMode".to_owned(),
                self.compile_write_mode(checker, wm)?,
            );
        }
        Ok((Value::Object(obj), None))
    }

    fn finish_effect_mutate_rows(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        m: &Members,
        input: P<Node>,
        mut obj: Map<String, Value>,
    ) -> Result<(Value, Option<String>), CompileError> {
        for extra in [
            "lifecycle",
            "transition",
            "writeMode",
            "acceptReceipt",
            "setFields",
        ] {
            if let Some(ex) = m.get(extra) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("mutate_parent_with_rows takes no {extra}"),
                    ex,
                ));
            }
        }
        let (Some(pv), Some(ro), Some(pver)) = (
            m.get("parentValuesField"),
            m.get("rowOpsField"),
            m.get("parentVersionField"),
        ) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "mutate_parent_with_rows requires parentValuesField, rowOpsField, and parentVersionField",
                input,
            ));
        };
        let row_ops_field = self.string(checker, ro)?;
        obj.insert(
            "parentValuesField".to_owned(),
            Value::String(self.string(checker, pv)?),
        );
        obj.insert(
            "rowOpsField".to_owned(),
            Value::String(row_ops_field.clone()),
        );
        obj.insert(
            "parentVersionField".to_owned(),
            Value::String(self.string(checker, pver)?),
        );
        if let Some(pd) = m.get("parentDeleteField") {
            obj.insert(
                "parentDeleteField".to_owned(),
                Value::String(self.string(checker, pd)?),
            );
        }
        if let Some(cols_node) = m.get("collections") {
            let cols = self.compile_effect_collections(checker, expansion, cols_node)?;
            if !cols.is_empty() {
                obj.insert("collections".to_owned(), Value::Array(cols));
            }
        }
        Ok((Value::Object(obj), Some(row_ops_field)))
    }

    fn compile_effect_collections(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        cols_node: P<Node>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut cols = Vec::new();
        for entry in self.list(checker, cols_node)? {
            let cm =
                self.closed_record(checker, entry, &["id", "preconditions", "derive"], &["id"])?;
            let mut c = Map::new();
            c.insert(
                "id".to_owned(),
                Value::String(self.string(checker, Self::member(&cm, "id", entry)?)?),
            );
            if let Some(pn) = cm.get("preconditions") {
                let mut ps = Vec::new();
                for expr in self.list(checker, pn)? {
                    ps.push(Value::String(self.zen(checker, expansion, expr, "self")?));
                }
                if !ps.is_empty() {
                    c.insert("preconditions".to_owned(), Value::Array(ps));
                }
            }
            if let Some(dn) = cm.get("derive") {
                let mut dm = Map::new();
                for (prop, expr) in self.record(checker, dn)?.iter() {
                    if prop == "__proto__" {
                        return Err(CompileError::new(
                            "NORN-FLOW-LITERAL",
                            "__proto__ is not a portable JSON member name",
                            expr.parent().unwrap_or(expr),
                        ));
                    }
                    dm.insert(
                        prop.to_owned(),
                        Value::String(self.zen(checker, expansion, expr, "self")?),
                    );
                }
                if !dm.is_empty() {
                    c.insert("derive".to_owned(), Value::Object(dm));
                }
            }
            cols.push(Value::Object(c));
        }
        Ok(cols)
    }

    fn finish_effect_fire_transition(
        &self,
        checker: &mut Checker,
        m: &Members,
        input: P<Node>,
        mut obj: Map<String, Value>,
    ) -> Result<(Value, Option<String>), CompileError> {
        for extra in [
            "writeMode",
            "parentValuesField",
            "rowOpsField",
            "parentVersionField",
            "parentDeleteField",
            "collections",
        ] {
            if let Some(ex) = m.get(extra) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("{extra} does not belong to fire_transition"),
                    ex,
                ));
            }
        }
        let (Some(lc), Some(tr)) = (m.get("lifecycle"), m.get("transition")) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "fire_transition names its lifecycle and transition",
                input,
            ));
        };
        obj.insert(
            "lifecycle".to_owned(),
            Value::String(self.string(checker, lc)?),
        );
        obj.insert(
            "transition".to_owned(),
            Value::String(self.string(checker, tr)?),
        );
        if let Some(receipt) = m.get("acceptReceipt") {
            let r = self.closed_record(
                checker,
                receipt,
                &[
                    "referenceField",
                    "manifestDigest",
                    "digestProperty",
                    "resultBindings",
                ],
                &[
                    "referenceField",
                    "manifestDigest",
                    "digestProperty",
                    "resultBindings",
                ],
            )?;
            let mut ro = Map::new();
            for key in ["referenceField", "manifestDigest", "digestProperty"] {
                ro.insert(
                    key.to_owned(),
                    Value::String(self.string(checker, Self::member(&r, key, receipt)?)?),
                );
            }
            ro.insert(
                "resultBindings".to_owned(),
                Value::Object(
                    self.string_map(checker, Self::member(&r, "resultBindings", receipt)?)?,
                ),
            );
            obj.insert("acceptReceipt".to_owned(), Value::Object(ro));
        }
        if let Some(set_fields) = m.get("setFields") {
            let map = self.string_map(checker, set_fields)?;
            if !map.is_empty() {
                obj.insert("setFields".to_owned(), Value::Object(map));
            }
        }
        Ok((Value::Object(obj), None))
    }

    fn compile_write_mode(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(checker, input, &["kind", "lifecycle", "state"], &["kind"])?;
        let kind_node = Self::member(&m, "kind", input)?;
        let kind = self.string(checker, kind_node)?;
        let mut obj = Map::new();
        obj.insert("kind".to_owned(), Value::String(kind.clone()));
        if matches!(kind.as_str(), "upsert" | "create_only" | "update_only") {
            if m.has("lifecycle") || m.has("state") {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("{kind} writeMode takes only kind"),
                    input,
                ));
            }
            return Ok(Value::Object(obj));
        }
        if kind == "draft_only" {
            let (Some(lc), Some(st)) = (m.get("lifecycle"), m.get("state")) else {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "draft_only writeMode needs lifecycle and state",
                    input,
                ));
            };
            obj.insert(
                "lifecycle".to_owned(),
                Value::String(self.string(checker, lc)?),
            );
            obj.insert("state".to_owned(), Value::String(self.string(checker, st)?));
            return Ok(Value::Object(obj));
        }
        Err(CompileError::new(
            "NORN-FLOW-LITERAL",
            "writeMode kind is upsert, create_only, update_only, or draft_only",
            kind_node,
        ))
    }

    pub(super) fn compile_quantity_effect(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["ledgerId", "balanceVersionsField", "operation"],
            &["ledgerId", "balanceVersionsField", "operation"],
        )?;
        let op_node = Self::member(&m, "operation", input)?;
        let op = self.record(checker, op_node)?;
        let kind = self.string(checker, Self::member(&op, "kind", op_node)?)?;
        let operation = match kind.as_str() {
            "reserve" => self.compile_quantity_reserve(checker, expansion, op_node)?,
            "post_reservation" => self.compile_quantity_post(checker, op_node)?,
            _ => {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "quantityEffect operation kind must be reserve or post_reservation",
                    op_node,
                ));
            }
        };
        let mut out = Map::new();
        out.insert(
            "ledgerId".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "ledgerId", input)?)?),
        );
        out.insert(
            "balanceVersionsField".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "balanceVersionsField", input)?)?),
        );
        out.insert("operation".to_owned(), operation);
        Ok(Value::Object(out))
    }

    fn compile_quantity_reserve(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        op_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let rm = self.closed_record(
            checker,
            op_node,
            &[
                "kind",
                "reservationIdField",
                "templateId",
                "parameters",
                "ttlMs",
            ],
            &[
                "kind",
                "reservationIdField",
                "templateId",
                "parameters",
                "ttlMs",
            ],
        )?;
        let mut params = Map::new();
        for (param_name, param_node) in self
            .record(checker, Self::member(&rm, "parameters", op_node)?)?
            .iter()
        {
            let pm = self.closed_record(
                checker,
                param_node,
                &["field", "scale"],
                &["field", "scale"],
            )?;
            let scale_node = Self::member(&pm, "scale", param_node)?;
            let scale = self.json(checker, expansion, scale_node, 0)?;
            if !scale.as_i64().is_some_and(|v| (0..=28).contains(&v)) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "quantityEffect parameter scale must be an integer from 0 to 28",
                    scale_node,
                ));
            }
            let mut po = Map::new();
            po.insert(
                "field".to_owned(),
                Value::String(self.string(checker, Self::member(&pm, "field", param_node)?)?),
            );
            po.insert("scale".to_owned(), scale);
            params.insert(param_name.to_owned(), Value::Object(po));
        }
        let ttl_node = Self::member(&rm, "ttlMs", op_node)?;
        let ttl = self.json(checker, expansion, ttl_node, 0)?;
        if ttl.as_i64().is_none_or(|v| v <= 0) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "quantityEffect reserve ttlMs must be a positive integer",
                ttl_node,
            ));
        }
        let mut oo = Map::new();
        oo.insert("kind".to_owned(), Value::String("reserve".to_owned()));
        oo.insert(
            "reservationIdField".to_owned(),
            Value::String(self.string(checker, Self::member(&rm, "reservationIdField", op_node)?)?),
        );
        oo.insert(
            "templateId".to_owned(),
            Value::String(self.string(checker, Self::member(&rm, "templateId", op_node)?)?),
        );
        oo.insert("parameters".to_owned(), Value::Object(params));
        oo.insert("ttlMs".to_owned(), ttl);
        Ok(Value::Object(oo))
    }

    fn compile_quantity_post(
        &self,
        checker: &mut Checker,
        op_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let pm = self.closed_record(
            checker,
            op_node,
            &["kind", "reservationIdField", "postingIdField"],
            &["kind", "reservationIdField", "postingIdField"],
        )?;
        let mut oo = Map::new();
        oo.insert(
            "kind".to_owned(),
            Value::String("post_reservation".to_owned()),
        );
        oo.insert(
            "reservationIdField".to_owned(),
            Value::String(self.string(checker, Self::member(&pm, "reservationIdField", op_node)?)?),
        );
        oo.insert(
            "postingIdField".to_owned(),
            Value::String(self.string(checker, Self::member(&pm, "postingIdField", op_node)?)?),
        );
        Ok(Value::Object(oo))
    }

    pub(super) fn compile_command(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["connector", "operation", "doctype", "name", "fields"],
            &["connector", "operation", "doctype"],
        )?;
        let op_node = Self::member(&m, "operation", input)?;
        let op = self.string(checker, op_node)?;
        if !matches!(op.as_str(), "submit" | "create" | "update" | "cancel") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a command operation is submit, create, update, or cancel",
                op_node,
            ));
        }
        let mut obj = Map::new();
        obj.insert(
            "connector".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "connector", input)?)?),
        );
        obj.insert("operation".to_owned(), Value::String(op));
        obj.insert(
            "doctype".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "doctype", input)?)?),
        );
        if let Some(name) = m.get("name") {
            obj.insert(
                "name".to_owned(),
                Value::String(self.zen(checker, expansion, name, "input")?),
            );
        }
        if let Some(fields_node) = m.get("fields") {
            let mut vals = Map::new();
            for (field, val) in self.record(checker, fields_node)?.iter() {
                if field == "__proto__" {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "__proto__ is not a portable JSON member name",
                        val.parent().unwrap_or(val),
                    ));
                }
                vals.insert(
                    field.to_owned(),
                    Value::String(self.zen(checker, expansion, val, "input")?),
                );
            }
            obj.insert("fields".to_owned(), Value::Object(vals));
        }
        Ok(Value::Object(obj))
    }

    pub(super) fn compile_action_aggregate(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["name", "type", "filter", "window", "aggregate", "maxItems"],
            &["name", "type", "aggregate", "maxItems"],
        )?;
        let agg_node = Self::member(&m, "aggregate", input)?;
        let agg = self.closed_record(checker, agg_node, &["kind", "property"], &["kind"])?;
        let kind_node = Self::member(&agg, "kind", agg_node)?;
        let kind = self.string(checker, kind_node)?;
        let mut agg_obj = Map::new();
        agg_obj.insert("kind".to_owned(), Value::String(kind.clone()));
        if kind == "count" {
            if let Some(p) = agg.get("property") {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "count aggregate takes no property",
                    p,
                ));
            }
        } else if kind == "sum" {
            let Some(p) = agg.get("property") else {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "sum aggregate requires property",
                    agg_node,
                ));
            };
            agg_obj.insert(
                "property".to_owned(),
                Value::String(self.string(checker, p)?),
            );
        } else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "aggregate kind must be sum or count",
                kind_node,
            ));
        }
        let mut obj = Map::new();
        obj.insert(
            "name".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "name", input)?)?),
        );
        obj.insert(
            "objectType".to_owned(),
            Value::String(self.type_name(
                checker,
                Self::member(&m, "type", input)?,
                OBJECT_TYPES,
            )?),
        );
        if let Some(fn_node) = m.get("filter") {
            let fmap = self.string_map(checker, fn_node)?;
            if !fmap.is_empty() {
                obj.insert("filter".to_owned(), Value::Object(fmap));
            }
        }
        if let Some(win_node) = m.get("window") {
            obj.insert(
                "window".to_owned(),
                self.compile_predicate_window(checker, win_node)?,
            );
        }
        obj.insert("aggregate".to_owned(), Value::Object(agg_obj));
        obj.insert(
            "maxItems".to_owned(),
            Value::Number(
                self.whole_positive_int(checker, Self::member(&m, "maxItems", input)?, "maxItems")?
                    .into(),
            ),
        );
        Ok(Value::Object(obj))
    }
}
