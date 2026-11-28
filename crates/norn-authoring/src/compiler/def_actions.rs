//! Action (`Norn.action`) lowering, input labels/groups, and access references.
//!
//! Ports `action`, `accessRef`, and `inputLabels` from
//! `tools/norn-ts/src/compiler/definitions.ts` and `builder/action.ts`.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_dotted_lower_id;
use super::def_helpers::{
    ACCESS, DefinitionCompiler, LabelTarget, LoweredDef, Members, OBJECT_TYPES,
};
use super::def_literals::{build_label_entries, build_labels_artifact};
use super::schema::NullableMode;
use super::types::{
    CompileError, DependencyPin, EnvelopeMeta, EnvelopeOptions, build_envelope_checked,
    is_string_literal_like, pinned_dependencies,
};
use super::value_schema::{ExpansionBudget, InputGroup, ObjectSchema, SchemaLabels, ValueSchema};

const ACTION_ALLOWED: &[&str] = &[
    "id",
    "title",
    "version",
    "description",
    "scopeRequired",
    "idempotent",
    "ledgered",
    "ledgerCategory",
    "input",
    "output",
    "inputLabels",
    "inputGroups",
    "slice",
    "aggregates",
    "derive",
    "preconditions",
    "createLinks",
    "commands",
    "synchronousRule",
    "writes",
    "effect",
    "quantityEffect",
    "compensation",
    "dependencies",
    "access",
    "fromCaller",
    "independentOf",
    "label",
    "localizedDescription",
    "needsReview",
];

struct ActionPayloadSpec<'a> {
    call: P<Node>,
    members: &'a Members,
    id: &'a str,
    title: &'a str,
    artifact_id: &'a str,
    input_schema: &'a ObjectSchema,
    output_schema: &'a ObjectSchema,
    effect_json: Option<Value>,
    sync_rule: Option<Value>,
}

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_action(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let members = self.meta(checker, call, ACTION_ALLOWED)?;
        let id = self.string(checker, Self::member(&members, "id", call)?)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "action id {id:?} must be dotted lower-case segments (commerce.sales_order.approve)"
                ),
                call,
            ));
        }
        let title = self.string(checker, Self::member(&members, "title", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        let localized = self.localized(checker, &members)?;

        let mut input_schema =
            self.read_action_schema(checker, expansion, &members, call, "input", true)?;
        let output_schema =
            self.read_action_schema(checker, expansion, &members, call, "output", false)?;

        let mut extra_deps: Vec<DependencyPin> = Vec::new();
        let effect_json = self.lower_action_effect_and_deps(
            checker,
            expansion,
            &members,
            &mut input_schema,
            &mut extra_deps,
        )?;
        self.apply_action_input_ui(checker, &members, &mut input_schema)?;
        let (env_deps, sync_rule) =
            self.collect_action_deps_and_sync_rule(checker, &members, extra_deps)?;

        let artifact_id = format!("{id}.action");
        let payload = self.build_action_payload(
            checker,
            expansion,
            ActionPayloadSpec {
                call,
                members: &members,
                id: &id,
                title: &title,
                artifact_id: &artifact_id,
                input_schema: &input_schema,
                output_schema: &output_schema,
                effect_json,
                sync_rule,
            },
        )?;
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "action".to_owned(),
                artifact_id,
                namespace,
                name: title,
                version,
                dependencies: env_deps,
            },
            payload,
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        let entries = build_label_entries(&LabelTarget::Action(id), &localized, None);
        let labels = build_labels_artifact(&artifact, entries, options, call)?;
        Ok(LoweredDef {
            artifact,
            labels,
            at: Some(self.at(call)),
        })
    }

    fn lower_action_effect_and_deps(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        input_schema: &mut ObjectSchema,
        extra_deps: &mut Vec<DependencyPin>,
    ) -> Result<Option<Value>, CompileError> {
        let Some(effect_node) = members.get("effect") else {
            return Ok(None);
        };
        let (eff, row_ops_field) = self.compile_action_effect(checker, expansion, effect_node)?;
        if let Some(field_name) = row_ops_field {
            let is_array = matches!(
                input_schema.properties.get(&field_name),
                Some(ValueSchema::Array { .. })
            );
            if !is_array {
                return Err(CompileError::new(
                    "NORN-FLOW-SCHEMA-PROJECTION",
                    format!(
                        "mutate_parent_with_rows rowOpsField {field_name:?} must be a Schema.Array field on action input"
                    ),
                    effect_node,
                ));
            }
            input_schema
                .properties
                .insert(field_name, ValueSchema::array(ValueSchema::json_value()));
            let eff_members = self.record(checker, effect_node)?;
            let parent = self.referenced(
                checker,
                Self::member(&eff_members, "type", effect_node)?,
                OBJECT_TYPES,
                "a Norn.type definition",
            )?;
            let mut seen = HashSet::new();
            self.collect_owned_child_deps(checker, parent, &mut seen, extra_deps)?;
        }
        Ok(Some(eff))
    }

    fn apply_action_input_ui(
        &self,
        checker: &mut Checker,
        members: &Members,
        input_schema: &mut ObjectSchema,
    ) -> Result<(), CompileError> {
        if let Some(labels_node) = members.get("inputLabels") {
            let labels = self.compile_input_labels(checker, labels_node)?;
            input_schema.apply_property_labels(labels);
        }
        if let Some(groups_node) = members.get("inputGroups") {
            let mut groups = Vec::new();
            for entry in self.list(checker, groups_node)? {
                let gm = self.closed_record(
                    checker,
                    entry,
                    &["id", "title", "fields"],
                    &["id", "title", "fields"],
                )?;
                groups.push(InputGroup {
                    id: self.string(checker, Self::member(&gm, "id", entry)?)?,
                    title: self.string(checker, Self::member(&gm, "title", entry)?)?,
                    fields: self.strings(checker, Self::member(&gm, "fields", entry)?)?,
                });
            }
            input_schema.groups = groups;
        }
        Ok(())
    }

    fn collect_action_deps_and_sync_rule(
        &self,
        checker: &mut Checker,
        members: &Members,
        extra_deps: Vec<DependencyPin>,
    ) -> Result<(Vec<Value>, Option<Value>), CompileError> {
        let mut all_deps = Vec::new();
        if let Some(deps_node) = members.get("dependencies") {
            for pin in self.list(checker, deps_node)? {
                all_deps.push(self.decision_dependency(checker, pin)?);
            }
        }
        for dep in extra_deps {
            if !all_deps
                .iter()
                .any(|e| e.artifact_id == dep.artifact_id && e.version == dep.version)
            {
                all_deps.push(dep);
            }
        }
        let mut env_deps = pinned_dependencies(&all_deps, "entity");
        let sync_rule = match members.get("synchronousRule") {
            Some(node) => {
                let pin = self.decision_dependency(checker, node)?;
                if let Some(kind) = &pin.kind
                    && kind != "rule"
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "synchronous writer must pin a rule",
                        node,
                    ));
                }
                let mut dep = Map::new();
                dep.insert(
                    "artifactId".to_owned(),
                    Value::String(pin.artifact_id.clone()),
                );
                dep.insert("version".to_owned(), Value::String(pin.version.clone()));
                dep.insert("kind".to_owned(), Value::String("rule".to_owned()));
                dep.insert("required".to_owned(), Value::Bool(true));
                env_deps.push(Value::Object(dep));
                let mut sr = Map::new();
                sr.insert("artifactId".to_owned(), Value::String(pin.artifact_id));
                sr.insert("version".to_owned(), Value::String(pin.version));
                Some(Value::Object(sr))
            }
            None => None,
        };
        Ok((env_deps, sync_rule))
    }

    fn build_action_payload(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        spec: ActionPayloadSpec<'_>,
    ) -> Result<Value, CompileError> {
        let mut payload = Map::new();
        payload.insert("actionId".to_owned(), Value::String(spec.id.to_owned()));
        payload.insert("title".to_owned(), Value::String(spec.title.to_owned()));
        if let Some(desc) = spec.members.get("description") {
            payload.insert(
                "description".to_owned(),
                Value::String(self.string(checker, desc)?),
            );
        }
        payload.insert(
            "scopeRequired".to_owned(),
            Value::Array(
                self.strings(
                    checker,
                    Self::member(spec.members, "scopeRequired", spec.call)?,
                )?
                .into_iter()
                .map(Value::String)
                .collect(),
            ),
        );
        payload.insert(
            "idempotent".to_owned(),
            Value::Bool(self.boolean(
                checker,
                Self::member(spec.members, "idempotent", spec.call)?,
            )?),
        );
        payload.insert(
            "ledgered".to_owned(),
            Value::Bool(self.boolean(checker, Self::member(spec.members, "ledgered", spec.call)?)?),
        );
        if let Some(cat) = spec.members.get("ledgerCategory") {
            payload.insert(
                "ledgerCategory".to_owned(),
                Value::String(self.string(checker, cat)?),
            );
        }
        payload.insert("inputSchema".to_owned(), spec.input_schema.to_json());
        payload.insert("outputSchema".to_owned(), spec.output_schema.to_json());

        self.append_action_reads_and_guards(
            checker,
            expansion,
            spec.members,
            spec.artifact_id,
            &mut payload,
        )?;
        self.append_action_mutations(checker, expansion, spec.members, &mut payload)?;

        if let Some(eff) = spec.effect_json {
            payload.insert("effectProgram".to_owned(), eff);
        }
        if let Some(qe_node) = spec.members.get("quantityEffect") {
            payload.insert(
                "quantityEffect".to_owned(),
                self.compile_quantity_effect(checker, expansion, qe_node)?,
            );
        }
        if let Some(sr) = spec.sync_rule {
            payload.insert("synchronousRule".to_owned(), sr);
        }
        if let Some(comp) = spec.members.get("compensation") {
            let mut c = Map::new();
            c.insert(
                "actionId".to_owned(),
                Value::String(self.action_id(checker, comp)?),
            );
            payload.insert("compensation".to_owned(), Value::Object(c));
        }
        if let Some(acc) = spec.members.get("access") {
            payload.insert("access".to_owned(), self.compile_access_ref(checker, acc)?);
        }
        if let Some(ind) = spec.members.get("independentOf") {
            let list = self.strings(checker, ind)?;
            if !list.is_empty() {
                payload.insert(
                    "independentOf".to_owned(),
                    Value::Array(list.into_iter().map(Value::String).collect()),
                );
            }
        }
        Ok(Value::Object(payload))
    }

    fn append_action_reads_and_guards(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        artifact_id: &str,
        payload: &mut Map<String, Value>,
    ) -> Result<(), CompileError> {
        if let Some(slice_node) = members.get("slice") {
            let mut slice = Vec::new();
            for entry in self.list(checker, slice_node)? {
                slice.push(self.compile_slice_entry(checker, entry)?);
            }
            if !slice.is_empty() {
                payload.insert("slice".to_owned(), Value::Array(slice));
            }
        }
        if let Some(agg_node) = members.get("aggregates") {
            let mut aggs = Vec::new();
            for entry in self.list(checker, agg_node)? {
                aggs.push(self.compile_action_aggregate(checker, entry)?);
            }
            if !aggs.is_empty() {
                payload.insert("aggregates".to_owned(), Value::Array(aggs));
            }
        }
        let mut derives = self.compile_from_caller_derives(checker, members)?;
        let caller_count = derives.len();
        if let Some(derive_node) = members.get("derive") {
            for (index, entry) in self.list(checker, derive_node)?.into_iter().enumerate() {
                let dm = self.closed_record(
                    checker,
                    entry,
                    &["field", "expression", "inputRequired"],
                    &["field", "expression"],
                )?;
                let mut d = Map::new();
                d.insert(
                    "field".to_owned(),
                    Value::String(self.string(checker, Self::member(&dm, "field", entry)?)?),
                );
                let ptr = format!(
                    "/payload/derive/{}/expression",
                    index.saturating_add(caller_count)
                );
                let expr = self.zen_at(
                    checker,
                    expansion,
                    Self::member(&dm, "expression", entry)?,
                    artifact_id,
                    &ptr,
                    "input",
                )?;
                d.insert("expression".to_owned(), Value::String(expr));
                if let Some(ir) = dm.get("inputRequired")
                    && !self.boolean(checker, ir)?
                {
                    d.insert("inputRequired".to_owned(), Value::Bool(false));
                }
                derives.push(Value::Object(d));
            }
        }
        if !derives.is_empty() {
            payload.insert("derive".to_owned(), Value::Array(derives));
        }
        if let Some(pre_node) = members.get("preconditions") {
            let mut pres = Vec::new();
            for (index, expr_node) in self.list(checker, pre_node)?.into_iter().enumerate() {
                let ptr = format!("/payload/preconditions/{index}");
                pres.push(Value::String(self.zen_at(
                    checker,
                    expansion,
                    expr_node,
                    artifact_id,
                    &ptr,
                    "",
                )?));
            }
            if !pres.is_empty() {
                payload.insert("preconditions".to_owned(), Value::Array(pres));
            }
        }
        Ok(())
    }

    fn compile_from_caller_derives(
        &self,
        checker: &mut Checker,
        members: &Members,
    ) -> Result<Vec<Value>, CompileError> {
        let mut derives = Vec::new();
        if let Some(fc_node) = members.get("fromCaller") {
            for field in self.strings(checker, fc_node)? {
                let mut d = Map::new();
                d.insert("field".to_owned(), Value::String(field));
                d.insert(
                    "expression".to_owned(),
                    Value::String("actor.subjectId".to_owned()),
                );
                d.insert("inputRequired".to_owned(), Value::Bool(false));
                derives.push(Value::Object(d));
            }
        }
        Ok(derives)
    }

    fn append_action_mutations(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        payload: &mut Map<String, Value>,
    ) -> Result<(), CompileError> {
        if let Some(cmd_node) = members.get("commands") {
            let mut cmds = Vec::new();
            for c in self.list(checker, cmd_node)? {
                cmds.push(self.compile_command(checker, expansion, c)?);
            }
            if !cmds.is_empty() {
                payload.insert("commands".to_owned(), Value::Array(cmds));
            }
        }
        if let Some(cl_node) = members.get("createLinks") {
            let cls = self.compile_create_links(checker, cl_node)?;
            if !cls.is_empty() {
                payload.insert("createLinks".to_owned(), Value::Array(cls));
            }
        }
        if let Some(writes_node) = members.get("writes") {
            let mut writes = Vec::new();
            for w in self.list(checker, writes_node)? {
                let wm =
                    self.closed_record(checker, w, &["type", "change"], &["type", "change"])?;
                let change_node = Self::member(&wm, "change", w)?;
                let change = self.string(checker, change_node)?;
                if !matches!(change.as_str(), "created" | "modified" | "deleted") {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "a write change is created, modified, or deleted",
                        change_node,
                    ));
                }
                let mut wo = Map::new();
                wo.insert(
                    "entityKind".to_owned(),
                    Value::String(self.type_name(
                        checker,
                        Self::member(&wm, "type", w)?,
                        OBJECT_TYPES,
                    )?),
                );
                wo.insert("change".to_owned(), Value::String(change));
                writes.push(Value::Object(wo));
            }
            if !writes.is_empty() {
                payload.insert("writes".to_owned(), Value::Array(writes));
            }
        }
        Ok(())
    }

    fn compile_create_links(
        &self,
        checker: &mut Checker,
        cl_node: P<Node>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut cls = Vec::new();
        for entry in self.list(checker, cl_node)? {
            let cm = self.closed_record(
                checker,
                entry,
                &["link", "targetType", "targetIdField", "source"],
                &["link", "targetType", "targetIdField"],
            )?;
            let mut obj = Map::new();
            obj.insert(
                "link".to_owned(),
                Value::String(self.string(checker, Self::member(&cm, "link", entry)?)?),
            );
            obj.insert(
                "targetType".to_owned(),
                Value::String(self.type_name(
                    checker,
                    Self::member(&cm, "targetType", entry)?,
                    OBJECT_TYPES,
                )?),
            );
            obj.insert(
                "targetIdField".to_owned(),
                Value::String(self.string(checker, Self::member(&cm, "targetIdField", entry)?)?),
            );
            if let Some(src_node) = cm.get("source") {
                let sm = self.closed_record(
                    checker,
                    src_node,
                    &["objectType", "idField"],
                    &["objectType", "idField"],
                )?;
                let mut src = Map::new();
                src.insert(
                    "objectType".to_owned(),
                    Value::String(self.type_name(
                        checker,
                        Self::member(&sm, "objectType", src_node)?,
                        OBJECT_TYPES,
                    )?),
                );
                src.insert(
                    "idField".to_owned(),
                    Value::String(self.string(checker, Self::member(&sm, "idField", src_node)?)?),
                );
                obj.insert("source".to_owned(), Value::Object(src));
            }
            cls.push(Value::Object(obj));
        }
        Ok(cls)
    }

    fn read_action_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        call: P<Node>,
        key: &str,
        temporal_wire: bool,
    ) -> Result<ObjectSchema, CompileError> {
        let node = Self::member(members, key, call)?;
        let read = self.schemas.read(checker, node)?;
        let closed = self
            .schemas
            .closed(&read, NullableMode::Scalar, temporal_wire, expansion)?;
        match closed {
            ValueSchema::Object(obj) => Ok(*obj),
            ValueSchema::String { .. }
            | ValueSchema::Number { .. }
            | ValueSchema::Integer { .. }
            | ValueSchema::Boolean { .. }
            | ValueSchema::Null { .. }
            | ValueSchema::JsonValue { .. }
            | ValueSchema::Array { .. }
            | ValueSchema::Nullable(_) => Err(CompileError::new(
                "NORN-FLOW-SCHEMA-PROJECTION",
                format!("action {key} is a Schema.Struct"),
                node,
            )),
        }
    }

    fn collect_owned_child_deps(
        &self,
        checker: &mut Checker,
        parent: (P<Node>, Members),
        seen: &mut HashSet<P<Node>>,
        out: &mut Vec<DependencyPin>,
    ) -> Result<(), CompileError> {
        let (call, members) = parent;
        if !seen.insert(call) {
            return Ok(());
        }
        if let Some(owned_node) = members.get("ownedCollections") {
            for item in self.list(checker, owned_node)? {
                let cm = self.record(checker, item)?;
                if let Some(child_expr) = cm.get("childType")
                    && self.deref(checker, child_expr)?.kind() == Kind::CallExpression
                {
                    let (name, version) =
                        self.type_ref(checker, child_expr, OBJECT_TYPES, "a Norn.type definition")?;
                    out.push(DependencyPin {
                        artifact_id: format!("{name}.entity"),
                        version,
                        kind: Some("entity".to_owned()),
                    });
                }
            }
        }
        if let Some(ext_expr) = members.get("extends") {
            let deref = self.deref(checker, ext_expr)?;
            if !is_string_literal_like(deref) {
                let base_ref =
                    self.referenced(checker, ext_expr, OBJECT_TYPES, "a Norn.type definition")?;
                self.collect_owned_child_deps(checker, base_ref, seen, out)?;
            }
        }
        Ok(())
    }

    fn compile_input_labels(
        &self,
        checker: &mut Checker,
        node: P<Node>,
    ) -> Result<BTreeMap<String, SchemaLabels>, CompileError> {
        let e = self.deref(checker, node)?;
        if e.kind() != Kind::ObjectLiteralExpression {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "inputLabels is an object literal of field labels",
                node,
            ));
        }
        let mut out = BTreeMap::new();
        for &prop in e.as_object_literal_expression().properties.nodes() {
            if prop.kind() != Kind::PropertyAssignment {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "an input label is `field: { title, description }`",
                    prop,
                ));
            }
            let pa = prop.as_property_assignment();
            let nk = pa.name().kind();
            if nk != Kind::Identifier && nk != Kind::StringLiteral {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "an input label is `field: { title, description }`",
                    prop,
                ));
            }
            let m =
                self.closed_record(checker, pa.initializer(), &["title", "description"], &[])?;
            out.insert(
                pa.name().text().to_owned(),
                SchemaLabels {
                    title: match m.get("title") {
                        Some(t) => Some(self.string(checker, t)?),
                        None => None,
                    },
                    description: match m.get("description") {
                        Some(d) => Some(self.string(checker, d)?),
                        None => None,
                    },
                },
            );
        }
        Ok(out)
    }

    fn compile_access_ref(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        if self.deref(checker, input)?.kind() == Kind::CallExpression {
            let (call, members) = self.referenced(
                checker,
                input,
                &[ACCESS],
                "a Norn.access definition (or { artifactId, version? })",
            )?;
            let mut obj = Map::new();
            obj.insert(
                "artifactId".to_owned(),
                Value::String(format!(
                    "{}.policy",
                    self.string(checker, Self::member(&members, "id", call)?)?
                )),
            );
            obj.insert(
                "version".to_owned(),
                Value::String(self.string(checker, Self::member(&members, "version", call)?)?),
            );
            return Ok(Value::Object(obj));
        }
        let m = self.closed_record(checker, input, &["artifactId", "version"], &["artifactId"])?;
        let aid_node = Self::member(&m, "artifactId", input)?;
        let artifact_id = self.string(checker, aid_node)?;
        if artifact_id.strip_suffix(".policy").is_none() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "access names a policy artifact: <policy id>.policy",
                aid_node,
            ));
        }
        let mut obj = Map::new();
        obj.insert("artifactId".to_owned(), Value::String(artifact_id));
        if let Some(v) = m.get("version") {
            obj.insert(
                "version".to_owned(),
                Value::String(self.string(checker, v)?),
            );
        }
        Ok(Value::Object(obj))
    }
}
