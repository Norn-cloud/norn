//! Action slice and predicate window lowering split out of
//! [`super::def_action_effects`].

use serde_json::{Map, Value};
use tsrs_ast::Node;
use tsrs_checker::Checker;
use tsrs_core::P;

use super::def_helpers::{DefinitionCompiler, OBJECT_TYPES};
use super::types::{CompileError, is_string_literal_like};

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_predicate_window(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["property", "start", "end", "startInclusive", "endInclusive"],
            &["property", "start", "end", "startInclusive", "endInclusive"],
        )?;
        let mut obj = Map::new();
        for key in ["property", "start", "end"] {
            obj.insert(
                key.to_owned(),
                Value::String(self.string(checker, Self::member(&m, key, input)?)?),
            );
        }
        for key in ["startInclusive", "endInclusive"] {
            obj.insert(
                key.to_owned(),
                Value::Bool(self.boolean(checker, Self::member(&m, key, input)?)?),
            );
        }
        Ok(Value::Object(obj))
    }

    pub(crate) fn compile_slice_entry(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["name", "type", "key", "optional"],
            &["name", "type", "key"],
        )?;
        let key_node = self.deref(checker, Self::member(&m, "key", input)?)?;
        let key_val = if is_string_literal_like(key_node) {
            if key_node.text() != "triggerObject" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "a slice key is \"triggerObject\", { property: { from, property } }, or { link: { from, link } }",
                    key_node,
                ));
            }
            Value::String("triggerObject".to_owned())
        } else {
            self.compile_slice_key_object(checker, key_node)?
        };
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
        obj.insert("key".to_owned(), key_val);
        if let Some(opt) = m.get("optional")
            && self.boolean(checker, opt)?
        {
            obj.insert("optional".to_owned(), Value::Bool(true));
        }
        Ok(Value::Object(obj))
    }

    fn compile_slice_key_object(
        &self,
        checker: &mut Checker,
        key_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let k = self.closed_record(
            checker,
            key_node,
            &["property", "link", "parameter", "predicate"],
            &[],
        )?;
        let count = ["property", "link", "parameter", "predicate"]
            .iter()
            .filter(|&&name| k.has(name))
            .count();
        if count != 1 {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a slice key is { property }, { link }, { parameter }, or { predicate }",
                key_node,
            ));
        }
        let mut key_obj = Map::new();
        if let Some(pred) = k.get("predicate") {
            key_obj.insert(
                "predicate".to_owned(),
                self.compile_slice_predicate(checker, pred)?,
            );
        } else if let Some(param) = k.get("parameter") {
            key_obj.insert(
                "parameter".to_owned(),
                self.compile_slice_parameter(checker, param)?,
            );
        } else if let Some(prop) = k.get("property") {
            let p =
                self.closed_record(checker, prop, &["from", "property"], &["from", "property"])?;
            let mut po = Map::new();
            po.insert(
                "from".to_owned(),
                Value::String(self.string(checker, Self::member(&p, "from", prop)?)?),
            );
            po.insert(
                "property".to_owned(),
                Value::String(self.string(checker, Self::member(&p, "property", prop)?)?),
            );
            key_obj.insert("property".to_owned(), Value::Object(po));
        } else if let Some(link) = k.get("link") {
            let l = self.closed_record(checker, link, &["from", "link"], &["from", "link"])?;
            let mut lo = Map::new();
            lo.insert(
                "from".to_owned(),
                Value::String(self.string(checker, Self::member(&l, "from", link)?)?),
            );
            lo.insert(
                "link".to_owned(),
                Value::String(self.string(checker, Self::member(&l, "link", link)?)?),
            );
            key_obj.insert("link".to_owned(), Value::Object(lo));
        }
        Ok(Value::Object(key_obj))
    }

    fn compile_slice_predicate(
        &self,
        checker: &mut Checker,
        pred: P<Node>,
    ) -> Result<Value, CompileError> {
        let p = self.closed_record(checker, pred, &["filter", "window", "maxItems"], &[])?;
        let mut po = Map::new();
        if let Some(fn_node) = p.get("filter") {
            let fmap = self.string_map(checker, fn_node)?;
            if !fmap.is_empty() {
                po.insert("filter".to_owned(), Value::Object(fmap));
            }
        }
        if let Some(win) = p.get("window") {
            po.insert(
                "window".to_owned(),
                self.compile_predicate_window(checker, win)?,
            );
        }
        if let Some(mi) = p.get("maxItems") {
            po.insert(
                "maxItems".to_owned(),
                Value::Number(self.whole_positive_int(checker, mi, "maxItems")?.into()),
            );
        }
        Ok(Value::Object(po))
    }

    fn compile_slice_parameter(
        &self,
        checker: &mut Checker,
        param: P<Node>,
    ) -> Result<Value, CompileError> {
        let p = self.closed_record(
            checker,
            param,
            &[
                "from",
                "settingKey",
                "effectiveAt",
                "precedence",
                "defaultFrom",
            ],
            &["from", "settingKey", "effectiveAt", "precedence"],
        )?;
        let mut prec = Vec::new();
        for scope_node in self.list(checker, Self::member(&p, "precedence", param)?)? {
            let sm = self.closed_record(
                checker,
                scope_node,
                &[
                    "orgId",
                    "tenantId",
                    "workspaceId",
                    "environmentId",
                    "businessUnitId",
                ],
                &[],
            )?;
            let mut so = Map::new();
            for (name, val) in sm.iter() {
                so.insert(name.to_owned(), Value::String(self.string(checker, val)?));
            }
            prec.push(Value::Object(so));
        }
        let mut po = Map::new();
        for key in ["from", "settingKey", "effectiveAt"] {
            po.insert(
                key.to_owned(),
                Value::String(self.string(checker, Self::member(&p, key, param)?)?),
            );
        }
        po.insert("precedence".to_owned(), Value::Array(prec));
        if let Some(df) = p.get("defaultFrom") {
            po.insert(
                "defaultFrom".to_owned(),
                Value::String(self.string(checker, df)?),
            );
        }
        Ok(Value::Object(po))
    }
}
