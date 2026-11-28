//! Source-to-ontology mapping (`Norn.mapping`) lowering split out of
//! [`super::def_actions`].

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::{is_dotted_lower_id, norn1_sha256, to_canonical_json};
use super::def_helpers::{
    DefinitionCompiler, INTERFACES, LoweredDef, Members, OBJECT_TYPES, PENDING_SOURCE_SHA256,
    SOURCE_SCHEMA,
};
use super::types::{
    CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope_checked, is_string_literal_like,
};
use super::value_schema::ExpansionBudget;

type ArtifactPin = (String, String);
type CompiledObjectMap = (Value, ArtifactPin, ArtifactPin);

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_mapping(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let members = self.meta(
            checker,
            call,
            &[
                "id",
                "title",
                "version",
                "source",
                "sourceContract",
                "interfaces",
                "maps",
            ],
        )?;
        let id = self.string(checker, Self::member(&members, "id", call)?)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "mapping id {id:?} must be dotted lower-case segments (erpnext.sales_order)"
                ),
                call,
            ));
        }
        let (payload, deps) = self.build_mapping_payload(checker, expansion, &members, call)?;
        let title = self.string(checker, Self::member(&members, "title", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "mapping".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: deps,
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: Some(self.at(call)),
        })
    }

    fn build_mapping_payload(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        members: &Members,
        call: P<Node>,
    ) -> Result<(Map<String, Value>, Vec<Value>), CompileError> {
        let (schema_id, source_version, source_sha) =
            self.read_mapping_source(checker, Self::member(members, "source", call)?)?;

        let mut deps: Vec<Value> = Vec::new();
        let mut pinned: BTreeMap<String, String> = BTreeMap::new();
        {
            let mut dep = Map::new();
            dep.insert("artifactId".to_owned(), Value::String(schema_id.clone()));
            dep.insert("kind".to_owned(), Value::String("source_schema".to_owned()));
            dep.insert("version".to_owned(), Value::String(source_version.clone()));
            dep.insert("required".to_owned(), Value::Bool(true));
            deps.push(Value::Object(dep));
            pinned.insert(schema_id.clone(), source_version.clone());
        }

        let mut interfaces = Vec::new();
        for item in self.list(checker, Self::member(members, "interfaces", call)?)? {
            let (name, ver) =
                self.type_ref(checker, item, INTERFACES, "a Norn.interfaceType definition")?;
            let aid = format!("{name}.interface");
            add_mapping_pin(&mut pinned, &mut deps, &aid, &ver, call)?;
            interfaces.push(Value::String(aid));
        }

        let mut maps_json = Vec::new();
        for item in self.list(checker, Self::member(members, "maps", call)?)? {
            let (map_val, iface_pin, ent_pin) =
                self.compile_object_map(checker, expansion, item)?;
            add_mapping_pin(&mut pinned, &mut deps, &iface_pin.0, &iface_pin.1, call)?;
            add_mapping_pin(&mut pinned, &mut deps, &ent_pin.0, &ent_pin.1, call)?;
            maps_json.push(map_val);
        }

        let mut payload = Map::new();
        payload.insert(
            "format".to_owned(),
            Value::String("norn.mapping/1".to_owned()),
        );
        if let Some(contract_node) = members.get("sourceContract") {
            payload.insert(
                "extensions".to_owned(),
                self.compile_source_contract(checker, contract_node)?,
            );
        }
        let mut src_obj = Map::new();
        src_obj.insert("schemaId".to_owned(), Value::String(schema_id));
        src_obj.insert("version".to_owned(), Value::String(source_version));
        src_obj.insert("sha256".to_owned(), Value::String(source_sha));
        payload.insert("source".to_owned(), Value::Object(src_obj));
        payload.insert("interfaces".to_owned(), Value::Array(interfaces));
        payload.insert("maps".to_owned(), Value::Array(maps_json));
        Ok((payload, deps))
    }

    fn read_mapping_source(
        &self,
        checker: &mut Checker,
        source_node: P<Node>,
    ) -> Result<(String, String, String), CompileError> {
        let source_call = self.deref(checker, source_node)?;
        if source_call.kind() == Kind::CallExpression
            && self.call_id(checker, source_call).as_deref() == Some(SOURCE_SCHEMA)
        {
            let schema = self.meta(
                checker,
                source_call,
                &[
                    "id",
                    "title",
                    "version",
                    "source",
                    "observedAt",
                    "adapter",
                    "rawMetadata",
                    "datasets",
                    "fields",
                    "constraints",
                    "codeSets",
                ],
            )?;
            return Ok((
                self.string(checker, Self::member(&schema, "id", source_call)?)?,
                self.string(checker, Self::member(&schema, "version", source_call)?)?,
                PENDING_SOURCE_SHA256.to_owned(),
            ));
        }
        let s = self.closed_record(
            checker,
            source_node,
            &["schemaId", "version", "sha256"],
            &["schemaId", "version", "sha256"],
        )?;
        Ok((
            self.string(checker, Self::member(&s, "schemaId", source_node)?)?,
            self.string(checker, Self::member(&s, "version", source_node)?)?,
            self.string(checker, Self::member(&s, "sha256", source_node)?)?,
        ))
    }

    fn compile_source_contract(
        &self,
        checker: &mut Checker,
        contract_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let cm = self.closed_record(
            checker,
            contract_node,
            &["kind", "system", "versions", "apiTerms"],
            &["kind"],
        )?;
        let kind = self.string(checker, Self::member(&cm, "kind", contract_node)?)?;
        let mut exts = Map::new();
        if kind == "generic" {
            self.closed_record(checker, contract_node, &["kind"], &["kind"])?;
            let mut g = Map::new();
            g.insert("parity".to_owned(), Value::String("unqualified".to_owned()));
            exts.insert("norn.mapping.generic/1.0.0".to_owned(), Value::Object(g));
            return Ok(Value::Object(exts));
        }
        if kind != "vendor" {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "sourceContract is generic or vendor",
                contract_node,
            ));
        }
        let fields = self.closed_record(
            checker,
            contract_node,
            &["kind", "system", "versions", "apiTerms"],
            &["kind", "system", "versions", "apiTerms"],
        )?;
        let versions = self.strings(checker, Self::member(&fields, "versions", contract_node)?)?;
        if versions.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "vendor mapping needs exact supported versions",
                contract_node,
            ));
        }
        let terms_node = Self::member(&fields, "apiTerms", contract_node)?;
        let terms =
            self.closed_record(checker, terms_node, &["uri", "sha256"], &["uri", "sha256"])?;
        let mut to = Map::new();
        to.insert(
            "uri".to_owned(),
            Value::String(self.string(checker, Self::member(&terms, "uri", terms_node)?)?),
        );
        to.insert(
            "sha256".to_owned(),
            Value::String(self.string(checker, Self::member(&terms, "sha256", terms_node)?)?),
        );
        let mut v = Map::new();
        v.insert(
            "system".to_owned(),
            Value::String(self.string(checker, Self::member(&fields, "system", contract_node)?)?),
        );
        v.insert(
            "versions".to_owned(),
            Value::Array(versions.into_iter().map(Value::String).collect()),
        );
        v.insert("apiTerms".to_owned(), Value::Object(to));
        exts.insert("norn.mapping.vendor/1.0.0".to_owned(), Value::Object(v));
        Ok(Value::Object(exts))
    }

    fn mapping_expression(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        input: P<Node>,
    ) -> Result<String, CompileError> {
        let e = self.deref(checker, input)?;
        if is_string_literal_like(e) {
            return self.string(checker, e);
        }
        self.zen(checker, expansion, input, "input")
    }

    fn compile_object_map(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        item: P<Node>,
    ) -> Result<CompiledObjectMap, CompileError> {
        let m = self.closed_record(
            checker,
            item,
            &[
                "id",
                "source",
                "target",
                "subject",
                "sourceRecord",
                "properties",
                "links",
                "lifecycles",
                "gaps",
                "ignoredFields",
                "when",
            ],
            &["id", "source", "target", "subject", "sourceRecord"],
        )?;
        let src_obj = self.compile_map_source(checker, Self::member(&m, "source", item)?)?;
        let target_node = Self::member(&m, "target", item)?;
        let target = self.closed_record(
            checker,
            target_node,
            &["type", "interface"],
            &["type", "interface"],
        )?;
        let (ent_name, ent_ver) = self.type_ref(
            checker,
            Self::member(&target, "type", target_node)?,
            OBJECT_TYPES,
            "a Norn.type definition",
        )?;
        let (iface_name, iface_ver) = self.type_ref(
            checker,
            Self::member(&target, "interface", target_node)?,
            INTERFACES,
            "a Norn.interfaceType definition",
        )?;
        let ent_aid = format!("{ent_name}.entity");
        let iface_aid = format!("{iface_name}.interface");

        let mut target_obj = Map::new();
        target_obj.insert("entity".to_owned(), Value::String(ent_aid.clone()));
        target_obj.insert("interface".to_owned(), Value::String(iface_aid.clone()));

        let mut obj = Map::new();
        obj.insert(
            "id".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "id", item)?)?),
        );
        obj.insert("source".to_owned(), Value::Object(src_obj));
        obj.insert("target".to_owned(), Value::Object(target_obj));
        if let Some(w) = m.get("when") {
            obj.insert(
                "when".to_owned(),
                Value::String(self.zen(checker, expansion, w, "input")?),
            );
        }
        obj.insert(
            "subject".to_owned(),
            Value::String(self.mapping_expression(
                checker,
                expansion,
                Self::member(&m, "subject", item)?,
            )?),
        );
        obj.insert(
            "sourceRecord".to_owned(),
            Value::String(self.mapping_expression(
                checker,
                expansion,
                Self::member(&m, "sourceRecord", item)?,
            )?),
        );
        self.append_object_map_tables(checker, expansion, &m, &mut obj)?;
        Ok((
            Value::Object(obj),
            (iface_aid, iface_ver),
            (ent_aid, ent_ver),
        ))
    }

    fn compile_map_source(
        &self,
        checker: &mut Checker,
        src_node: P<Node>,
    ) -> Result<Map<String, Value>, CompileError> {
        let src = self.closed_record(checker, src_node, &["dataset", "nestedIn"], &["dataset"])?;
        let mut src_obj = Map::new();
        src_obj.insert(
            "dataset".to_owned(),
            Value::String(self.string(checker, Self::member(&src, "dataset", src_node)?)?),
        );
        if let Some(nested) = src.get("nestedIn") {
            let nm = self.closed_record(
                checker,
                nested,
                &["dataset", "field"],
                &["dataset", "field"],
            )?;
            let mut no = Map::new();
            no.insert(
                "dataset".to_owned(),
                Value::String(self.string(checker, Self::member(&nm, "dataset", nested)?)?),
            );
            no.insert(
                "field".to_owned(),
                Value::String(self.string(checker, Self::member(&nm, "field", nested)?)?),
            );
            src_obj.insert("nestedIn".to_owned(), Value::Object(no));
        }
        Ok(src_obj)
    }

    fn append_object_map_tables(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        m: &Members,
        obj: &mut Map<String, Value>,
    ) -> Result<(), CompileError> {
        for (key, allowed, required) in [
            (
                "properties",
                ["property", "value"].as_slice(),
                ["property", "value"].as_slice(),
            ),
            (
                "links",
                ["link", "targetMap", "key"].as_slice(),
                ["link", "targetMap", "key"].as_slice(),
            ),
            (
                "lifecycles",
                ["property", "state", "transition", "onDelete"].as_slice(),
                ["property"].as_slice(),
            ),
            (
                "gaps",
                ["member", "reason", "note"].as_slice(),
                ["member", "reason"].as_slice(),
            ),
            (
                "ignoredFields",
                ["field", "reason"].as_slice(),
                ["field", "reason"].as_slice(),
            ),
        ] {
            if let Some(list_node) = m.get(key) {
                let mut rows = Vec::new();
                for el in self.list(checker, list_node)? {
                    let rec = self.closed_record(checker, el, allowed, required)?;
                    let mut row = Map::new();
                    for &field in allowed {
                        if let Some(v_node) = rec.get(field) {
                            let val = if key == "properties" && field == "value" {
                                self.mapping_expression(checker, expansion, v_node)?
                            } else {
                                self.string(checker, v_node)?
                            };
                            if key == "gaps"
                                && field == "reason"
                                && !matches!(
                                    val.as_str(),
                                    "not_in_source"
                                        | "defaulted"
                                        | "deferred"
                                        | "derived_elsewhere"
                                )
                            {
                                return Err(CompileError::new(
                                    "NORN-FLOW-LITERAL",
                                    "a gap reason is not_in_source, defaulted, deferred, or derived_elsewhere",
                                    list_node,
                                ));
                            }
                            row.insert(field.to_owned(), Value::String(val));
                        }
                    }
                    rows.push(Value::Object(row));
                }
                if !rows.is_empty() {
                    obj.insert(key.to_owned(), Value::Array(rows));
                }
            }
        }
        Ok(())
    }
}

fn add_mapping_pin(
    pinned: &mut BTreeMap<String, String>,
    deps: &mut Vec<Value>,
    aid: &str,
    ver: &str,
    call: P<Node>,
) -> Result<(), CompileError> {
    if let Some(known) = pinned.get(aid) {
        if known != ver {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "{aid} is pinned at both {known} and {ver}; a mapping binds one version of each artifact"
                ),
                call,
            ));
        }
        return Ok(());
    }
    pinned.insert(aid.to_owned(), ver.to_owned());
    let mut dep = Map::new();
    dep.insert("artifactId".to_owned(), Value::String(aid.to_owned()));
    dep.insert("kind".to_owned(), Value::String("entity".to_owned()));
    dep.insert("version".to_owned(), Value::String(ver.to_owned()));
    dep.insert("required".to_owned(), Value::Bool(true));
    deps.push(Value::Object(dep));
    Ok(())
}

/// Binds same-project `Norn.sourceSchema` `Norn1` hashes onto pending mappings (`MAPPING-SOURCE-REF-1`).
pub(crate) fn bind_compiled_source_schemas(batches: &mut [Vec<Value>]) {
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    for artifacts in &*batches {
        for artifact in artifacts {
            if artifact.get("kind").and_then(Value::as_str) != Some("source_schema") {
                continue;
            }
            let (Some(id), Some(version)) = (
                artifact.get("artifactId").and_then(Value::as_str),
                artifact.get("version").and_then(Value::as_str),
            ) else {
                continue;
            };
            if let Some(sha) = norn1_sha256(artifact) {
                hashes.insert(format!("{id}\0{version}"), sha);
            }
        }
    }
    if hashes.is_empty() {
        return;
    }
    for artifacts in &mut *batches {
        for artifact in artifacts {
            if artifact.get("kind").and_then(Value::as_str) != Some("mapping") {
                continue;
            }
            let key = {
                let Some(source) = artifact.get("payload").and_then(|p| p.get("source")) else {
                    continue;
                };
                if source.get("sha256").and_then(Value::as_str) != Some(PENDING_SOURCE_SHA256) {
                    continue;
                }
                let schema_id = source.get("schemaId").and_then(Value::as_str).unwrap_or("");
                let version = source.get("version").and_then(Value::as_str).unwrap_or("");
                format!("{schema_id}\0{version}")
            };
            if let Some(sha) = hashes.get(&key)
                && let Some(source) = artifact
                    .get_mut("payload")
                    .and_then(|p| p.get_mut("source"))
                    .and_then(Value::as_object_mut)
            {
                source.insert("sha256".to_owned(), Value::String(sha.clone()));
                if let Ok(canon) = to_canonical_json(artifact) {
                    *artifact = canon;
                }
            }
        }
    }
}
