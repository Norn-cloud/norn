//! Entity, interface, link, invariant, and owned-collection lowering.
//!
//! Ports `objectType`, `interfaceType`, `invariant`, `link`, `interfaceLink`,
//! `ownedCollection`, and `lowerEntity` from `tools/norn-ts/src/compiler/definitions.ts`
//! and `builder/entity.ts`.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};
use tsrs_ast::Node;
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_object_type_name;
use super::def_helpers::{
    ANY_TYPE, DefinitionCompiler, INTERFACES, LabelTarget, LoweredDef, Members, OBJECT_TYPES,
    TypeBase,
};
use super::def_literals::{build_label_entries, build_labels_artifact};
use super::types::{
    CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope_checked, pinned_dependencies,
};
use super::value_schema::ExpansionBudget;

const NORMATIVE_SLOT: &str = "norn.normative/1.0.0";
const VALID_ORIGINS: &[&str] = &["caller", "actor", "receipt", "derived", "clock", "default"];

const OBJECT_TYPE_ALLOWED: &[&str] = &[
    "name",
    "title",
    "version",
    "description",
    "role",
    "properties",
    "extends",
    "implements",
    "links",
    "ownedCollections",
    "identity",
    "defaults",
    "lifecycles",
    "markings",
    "propertyMarkings",
    "dependencies",
    "pathEquations",
    "protected",
    "origins",
    "immutableAfterCreate",
    "label",
    "localizedDescription",
    "needsReview",
    "propertyLabels",
    "valueLabels",
];

const INTERFACE_ALLOWED: &[&str] = &[
    "name",
    "title",
    "version",
    "description",
    "role",
    "extends",
    "disjointWith",
    "properties",
    "links",
    "invariants",
    "lifecycles",
    "markings",
    "propertyMarkings",
    "dependencies",
    "label",
    "localizedDescription",
    "needsReview",
    "propertyLabels",
    "valueLabels",
];

struct ObjectPropExtras<'a> {
    protected_ids: &'a HashSet<String>,
    origins: &'a BTreeMap<String, String>,
    immutable_after_create_ids: &'a HashSet<String>,
    prop_markings: &'a BTreeMap<String, Vec<String>>,
    defaults: &'a BTreeMap<String, Value>,
}

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_object_type(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let members = self.meta(checker, call, OBJECT_TYPE_ALLOWED)?;
        let base = self.type_base(checker, expansion, call, &members)?;
        let prop_ids: Vec<String> = base.projected.iter().map(|p| p.id.clone()).collect();
        let prop_markings = self.property_markings(checker, &members, &prop_ids)?;
        let protected_ids =
            self.collect_declared_prop_set(checker, &members, &prop_ids, "protected")?;
        let origins = self.collect_origins(checker, &members, &prop_ids)?;
        let immutable_ids =
            self.collect_declared_prop_set(checker, &members, &prop_ids, "immutableAfterCreate")?;
        let defaults = self.collect_defaults(checker, &members, &prop_ids)?;
        let localized = self.localized(checker, &members)?;
        let member_labels = self.member_labels(checker, &members, &prop_ids)?;
        if let Err(err) = Self::validate_type_name(&base.name, call) {
            self.deferred_errors.push(err);
        }

        let props_json = Self::build_object_properties(
            &base,
            &ObjectPropExtras {
                protected_ids: &protected_ids,
                origins: &origins,
                immutable_after_create_ids: &immutable_ids,
                prop_markings: &prop_markings,
                defaults: &defaults,
            },
        );
        let obj_type =
            self.build_object_type_body(checker, expansion, &members, &base, props_json)?;
        let extensions = self.build_object_extensions(checker, expansion, call, &members)?;

        let mut payload = Map::new();
        payload.insert("entityVersion".to_owned(), Value::String("2.0".to_owned()));
        if !extensions.is_empty() {
            payload.insert("extensions".to_owned(), Value::Object(extensions));
        }
        payload.insert("objectType".to_owned(), Value::Object(obj_type));

        let namespace = base
            .name
            .rsplit_once('.')
            .map_or("", |(ns, _)| ns)
            .to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "entity".to_owned(),
                artifact_id: format!("{}.entity", base.name),
                namespace,
                name: base.title,
                version: base.version,
                dependencies: pinned_dependencies(&base.dependencies, "entity"),
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        let entries = build_label_entries(
            &LabelTarget::ObjectType(base.name),
            &localized,
            Some(&member_labels),
        );
        let labels = build_labels_artifact(&artifact, entries, options, call)?;
        Ok(LoweredDef {
            artifact,
            labels,
            at: Some(self.at(call)),
        })
    }

    fn validate_type_name(name: &str, call: P<Node>) -> Result<(), CompileError> {
        if !is_object_type_name(name) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "type name {name:?} must be a dotted lower-case namespace and a PascalCase name (commerce.SalesOrder)"
                ),
                call,
            ));
        }
        Ok(())
    }

    fn build_object_properties(base: &TypeBase, extras: &ObjectPropExtras<'_>) -> Vec<Value> {
        let mut props_json = Vec::with_capacity(base.projected.len());
        for p in &base.projected {
            let mut obj = Map::new();
            if extras.protected_ids.contains(&p.id) {
                obj.insert("protected".to_owned(), Value::Bool(true));
            }
            if let Some(origin) = extras.origins.get(&p.id) {
                obj.insert("origin".to_owned(), Value::String(origin.clone()));
            }
            if extras.immutable_after_create_ids.contains(&p.id) {
                obj.insert("immutableAfterCreate".to_owned(), Value::Bool(true));
            }
            obj.insert("id".to_owned(), Value::String(p.id.clone()));
            if let Some(ms) = extras.prop_markings.get(&p.id)
                && !ms.is_empty()
            {
                obj.insert(
                    "markings".to_owned(),
                    Value::Array(ms.iter().cloned().map(Value::String).collect()),
                );
            }
            obj.insert("type".to_owned(), p.projection.prop_type.to_json());
            if p.projection.nullable {
                let mut c = Map::new();
                c.insert("nullable".to_owned(), Value::Bool(true));
                obj.insert("constraints".to_owned(), Value::Object(c));
            }
            if let Some(def_val) = extras.defaults.get(&p.id) {
                obj.insert("default".to_owned(), def_val.clone());
            }
            props_json.push(Value::Object(obj));
        }
        props_json
    }

    fn build_object_type_body(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        base: &TypeBase,
        props_json: Vec<Value>,
    ) -> Result<Map<String, Value>, CompileError> {
        let mut obj_type = Map::new();
        obj_type.insert("name".to_owned(), Value::String(base.name.clone()));
        if let Some(role) = &base.role {
            obj_type.insert("role".to_owned(), Value::String(role.clone()));
        }
        if !base.markings.is_empty() {
            obj_type.insert(
                "markings".to_owned(),
                Value::Array(base.markings.iter().cloned().map(Value::String).collect()),
            );
        }
        if let Some(desc) = &base.description {
            obj_type.insert("description".to_owned(), Value::String(desc.clone()));
        }
        if let Some(ext) = members.get("extends") {
            obj_type.insert(
                "extends".to_owned(),
                Value::String(self.type_name(checker, ext, OBJECT_TYPES)?),
            );
        }
        if let Some(impls_node) = members.get("implements") {
            let mut impls = Vec::new();
            for item in self.list(checker, impls_node)? {
                impls.push(Value::String(self.type_name(checker, item, INTERFACES)?));
            }
            if !impls.is_empty() {
                obj_type.insert("implements".to_owned(), Value::Array(impls));
            }
        }
        if !props_json.is_empty() {
            obj_type.insert("properties".to_owned(), Value::Array(props_json));
        }
        if let Some(links_node) = members.get("links") {
            let mut links = Vec::new();
            for item in self.list(checker, links_node)? {
                links.push(self.compile_link(checker, item)?);
            }
            if !links.is_empty() {
                obj_type.insert("links".to_owned(), Value::Array(links));
            }
        }
        if let Some(owned_node) = members.get("ownedCollections") {
            let mut cols = Vec::new();
            for item in self.list(checker, owned_node)? {
                cols.push(self.compile_owned_collection(checker, item)?);
            }
            if !cols.is_empty() {
                obj_type.insert("ownedCollections".to_owned(), Value::Array(cols));
            }
        }
        if let Some(ident_node) = members.get("identity") {
            let keys = self.compile_identity_keys(checker, ident_node)?;
            if !keys.is_empty() {
                obj_type.insert("identity".to_owned(), Value::Array(keys));
            }
        }
        if let Some(lc_node) = members.get("lifecycles") {
            let mut lcs = Vec::new();
            for item in self.list(checker, lc_node)? {
                lcs.push(self.compile_lifecycle(checker, expansion, item)?);
            }
            if !lcs.is_empty() {
                obj_type.insert("lifecycles".to_owned(), Value::Array(lcs));
            }
        }
        Ok(obj_type)
    }

    fn compile_identity_keys(
        &self,
        checker: &mut Checker,
        ident_node: P<Node>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut keys = Vec::new();
        for item in self.list(checker, ident_node)? {
            let m = self.closed_record(
                checker,
                item,
                &["name", "properties"],
                &["name", "properties"],
            )?;
            let mut k = Map::new();
            k.insert(
                "name".to_owned(),
                Value::String(self.string(checker, Self::member(&m, "name", item)?)?),
            );
            k.insert(
                "properties".to_owned(),
                Value::Array(
                    self.strings(checker, Self::member(&m, "properties", item)?)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            );
            keys.push(Value::Object(k));
        }
        Ok(keys)
    }

    fn build_object_extensions(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        members: &Members,
    ) -> Result<Map<String, Value>, CompileError> {
        let mut extensions = Map::new();
        if let Some(pe_node) = members.get("pathEquations") {
            let mut eqs = Vec::new();
            for item in self.list(checker, pe_node)? {
                let m = self.closed_record(
                    checker,
                    item,
                    &["id", "left", "right"],
                    &["id", "left", "right"],
                )?;
                let mut eq = Map::new();
                eq.insert(
                    "id".to_owned(),
                    Value::String(self.string(checker, Self::member(&m, "id", item)?)?),
                );
                eq.insert(
                    "left".to_owned(),
                    Value::Array(
                        self.strings(checker, Self::member(&m, "left", item)?)?
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
                eq.insert(
                    "right".to_owned(),
                    Value::Array(
                        self.strings(checker, Self::member(&m, "right", item)?)?
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
                eqs.push(Value::Object(eq));
            }
            if !eqs.is_empty() {
                extensions.insert("norn.path-equations/1.0.0".to_owned(), Value::Array(eqs));
            }
        }
        let norms = self.compile_norms(checker, expansion, call, members)?;
        if !norms.is_empty() {
            extensions.insert(NORMATIVE_SLOT.to_owned(), Value::Array(norms));
        }
        Ok(extensions)
    }

    pub(crate) fn compile_interface_type(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let members = self.meta(checker, call, INTERFACE_ALLOWED)?;
        let base = self.type_base(checker, expansion, call, &members)?;
        let prop_ids: Vec<String> = base.projected.iter().map(|p| p.id.clone()).collect();
        let prop_markings = self.property_markings(checker, &members, &prop_ids)?;
        let localized = self.localized(checker, &members)?;
        let member_labels = self.member_labels(checker, &members, &prop_ids)?;
        if let Err(err) = Self::validate_type_name(&base.name, call) {
            self.deferred_errors.push(err);
        }

        let empty_set = HashSet::new();
        let empty_origins = BTreeMap::new();
        let empty_defaults = BTreeMap::new();
        let props_json = Self::build_object_properties(
            &base,
            &ObjectPropExtras {
                protected_ids: &empty_set,
                origins: &empty_origins,
                immutable_after_create_ids: &empty_set,
                prop_markings: &prop_markings,
                defaults: &empty_defaults,
            },
        );
        let iface = self.build_interface_body(checker, expansion, &members, &base, props_json)?;

        let mut payload = Map::new();
        payload.insert("entityVersion".to_owned(), Value::String("2.0".to_owned()));
        payload.insert("interface".to_owned(), Value::Object(iface));

        let namespace = base
            .name
            .rsplit_once('.')
            .map_or("", |(ns, _)| ns)
            .to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "entity".to_owned(),
                artifact_id: format!("{}.interface", base.name),
                namespace,
                name: base.title,
                version: base.version,
                dependencies: pinned_dependencies(&base.dependencies, "entity"),
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        let entries = build_label_entries(
            &LabelTarget::ObjectType(base.name),
            &localized,
            Some(&member_labels),
        );
        let labels = build_labels_artifact(&artifact, entries, options, call)?;
        Ok(LoweredDef {
            artifact,
            labels,
            at: Some(self.at(call)),
        })
    }

    fn build_interface_body(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        base: &TypeBase,
        props_json: Vec<Value>,
    ) -> Result<Map<String, Value>, CompileError> {
        let mut iface = Map::new();
        iface.insert("name".to_owned(), Value::String(base.name.clone()));
        if let Some(role) = &base.role {
            iface.insert("role".to_owned(), Value::String(role.clone()));
        }
        if let Some(ext_node) = members.get("extends") {
            let mut exts = Vec::new();
            for item in self.list(checker, ext_node)? {
                exts.push(Value::String(self.type_name(checker, item, INTERFACES)?));
            }
            if !exts.is_empty() {
                iface.insert("extends".to_owned(), Value::Array(exts));
            }
        }
        if let Some(disj_node) = members.get("disjointWith") {
            let mut disjs = Vec::new();
            for item in self.list(checker, disj_node)? {
                disjs.push(Value::String(self.type_name(checker, item, INTERFACES)?));
            }
            if !disjs.is_empty() {
                iface.insert("disjointWith".to_owned(), Value::Array(disjs));
            }
        }
        if !base.markings.is_empty() {
            iface.insert(
                "markings".to_owned(),
                Value::Array(base.markings.iter().cloned().map(Value::String).collect()),
            );
        }
        if let Some(desc) = &base.description {
            iface.insert("description".to_owned(), Value::String(desc.clone()));
        }
        if !props_json.is_empty() {
            iface.insert("properties".to_owned(), Value::Array(props_json));
        }
        if let Some(links_node) = members.get("links") {
            let mut links = Vec::new();
            for item in self.list(checker, links_node)? {
                links.push(self.compile_interface_link(checker, item)?);
            }
            if !links.is_empty() {
                iface.insert("links".to_owned(), Value::Array(links));
            }
        }
        if let Some(inv_node) = members.get("invariants") {
            let mut invs = Vec::new();
            for item in self.list(checker, inv_node)? {
                invs.push(self.compile_invariant(checker, expansion, item)?);
            }
            if !invs.is_empty() {
                iface.insert("invariants".to_owned(), Value::Array(invs));
            }
        }
        if let Some(lc_node) = members.get("lifecycles") {
            let mut lcs = Vec::new();
            for item in self.list(checker, lc_node)? {
                lcs.push(self.compile_lifecycle(checker, expansion, item)?);
            }
            if !lcs.is_empty() {
                iface.insert("lifecycles".to_owned(), Value::Array(lcs));
            }
        }
        Ok(iface)
    }

    fn collect_declared_prop_set(
        &self,
        checker: &mut Checker,
        members: &Members,
        prop_ids: &[String],
        field: &str,
    ) -> Result<HashSet<String>, CompileError> {
        let mut out = HashSet::new();
        if let Some(node) = members.get(field) {
            for item in self.list(checker, node)? {
                let id = self.string(checker, item)?;
                if !prop_ids.iter().any(|p| p == &id) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("{field} names undeclared property {id}"),
                        item,
                    ));
                }
                if !out.insert(id.clone()) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("{field} names {id} twice"),
                        item,
                    ));
                }
            }
        }
        Ok(out)
    }

    fn collect_origins(
        &self,
        checker: &mut Checker,
        members: &Members,
        prop_ids: &[String],
    ) -> Result<BTreeMap<String, String>, CompileError> {
        let mut out = BTreeMap::new();
        if let Some(node) = members.get("origins") {
            for (key, value) in self.record(checker, node)?.iter() {
                if !prop_ids.iter().any(|p| p == key) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("origins names undeclared property {key}"),
                        value,
                    ));
                }
                let origin = self.string(checker, value)?;
                if !VALID_ORIGINS.contains(&origin.as_str()) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "origin must be one of caller, actor, receipt, derived, clock, default",
                        value,
                    ));
                }
                out.insert(key.to_owned(), origin);
            }
        }
        Ok(out)
    }

    fn collect_defaults(
        &self,
        checker: &mut Checker,
        members: &Members,
        prop_ids: &[String],
    ) -> Result<BTreeMap<String, Value>, CompileError> {
        let mut out = BTreeMap::new();
        if let Some(node) = members.get("defaults") {
            for (key, value) in self.record(checker, node)?.iter() {
                if !prop_ids.iter().any(|p| p == key) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!("default for undeclared property {key}"),
                        value,
                    ));
                }
                out.insert(key.to_owned(), self.default_value(checker, value)?);
            }
        }
        Ok(out)
    }

    pub(crate) fn compile_link(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &[
                "id",
                "description",
                "target",
                "cardinality",
                "inverse",
                "acyclic",
            ],
            &["id", "target", "cardinality"],
        )?;
        let mut obj = self.link_core(checker, &m, input, OBJECT_TYPES)?;
        if let Some(inv) = m.get("inverse") {
            obj.insert(
                "inverse".to_owned(),
                Value::String(self.string(checker, inv)?),
            );
        }
        if let Some(acyclic) = m.get("acyclic")
            && self.boolean(checker, acyclic)?
        {
            obj.insert("acyclic".to_owned(), Value::Bool(true));
        }
        Ok(Value::Object(obj))
    }

    fn compile_interface_link(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &["id", "description", "target", "cardinality"],
            &["id", "target", "cardinality"],
        )?;
        Ok(Value::Object(self.link_core(checker, &m, input, ANY_TYPE)?))
    }

    fn link_core(
        &self,
        checker: &mut Checker,
        m: &Members,
        input: P<Node>,
        targets: &[&str],
    ) -> Result<Map<String, Value>, CompileError> {
        let card_node = Self::member(m, "cardinality", input)?;
        let c = self.closed_record(
            checker,
            card_node,
            &["to", "from", "required"],
            &["to", "from"],
        )?;
        let to = self.read_multiplicity(checker, Self::member(&c, "to", card_node)?)?;
        let from = self.read_multiplicity(checker, Self::member(&c, "from", card_node)?)?;
        let mut card = Map::new();
        card.insert("to".to_owned(), Value::String(to));
        card.insert("from".to_owned(), Value::String(from));
        if let Some(req) = c.get("required")
            && self.boolean(checker, req)?
        {
            card.insert("required".to_owned(), Value::Bool(true));
        }
        let mut obj = Map::new();
        obj.insert(
            "id".to_owned(),
            Value::String(self.string(checker, Self::member(m, "id", input)?)?),
        );
        if let Some(desc) = m.get("description") {
            obj.insert(
                "description".to_owned(),
                Value::String(self.string(checker, desc)?),
            );
        }
        obj.insert(
            "target".to_owned(),
            Value::String(self.type_name(checker, Self::member(m, "target", input)?, targets)?),
        );
        obj.insert("cardinality".to_owned(), Value::Object(card));
        Ok(obj)
    }

    fn read_multiplicity(
        &self,
        checker: &mut Checker,
        node: P<Node>,
    ) -> Result<String, CompileError> {
        let v = self.string(checker, node)?;
        if v != "one" && v != "many" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "a multiplicity is \"one\" or \"many\"",
                node,
            ));
        }
        Ok(v)
    }

    fn compile_owned_collection(
        &self,
        checker: &mut Checker,
        input: P<Node>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            input,
            &[
                "id",
                "childType",
                "rowIdProperty",
                "orderingKey",
                "cascadeOnParentDelete",
                "maxRows",
            ],
            &[
                "id",
                "childType",
                "rowIdProperty",
                "orderingKey",
                "cascadeOnParentDelete",
                "maxRows",
            ],
        )?;
        let max_node = Self::member(&m, "maxRows", input)?;
        let max_rows = self.whole_positive_int(checker, max_node, "maxRows")?;
        if max_rows > 100 {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "maxRows must be between 1 and 100",
                max_node,
            ));
        }
        let mut obj = Map::new();
        obj.insert(
            "id".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "id", input)?)?),
        );
        obj.insert(
            "childType".to_owned(),
            Value::String(self.type_name(
                checker,
                Self::member(&m, "childType", input)?,
                OBJECT_TYPES,
            )?),
        );
        obj.insert(
            "rowIdProperty".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "rowIdProperty", input)?)?),
        );
        obj.insert(
            "orderingKey".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "orderingKey", input)?)?),
        );
        obj.insert(
            "cascadeOnParentDelete".to_owned(),
            Value::Bool(self.boolean(checker, Self::member(&m, "cascadeOnParentDelete", input)?)?),
        );
        obj.insert("maxRows".to_owned(), Value::Number(max_rows.into()));
        Ok(Value::Object(obj))
    }
}
