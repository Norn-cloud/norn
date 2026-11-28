//! Catalog definition lowering: `sourceSchema`, `ontologyPack`, `packLink`,
//! `markings`, and `labels`.
//!
//! Ports the corresponding methods in `tools/norn-ts/src/compiler/definitions.ts`
//! and `tools/norn-ts/src/builder/{source-schema,ontology-pack,pack-link,entity,labels}.ts`.

use serde_json::{Map, Value};
use tsrs_ast::Node;
use tsrs_checker::Checker;
use tsrs_core::P;

use super::def_helpers::{
    DefinitionCompiler, LabelTarget, LocalizedData, LoweredDef, MemberLabelsData, Members,
};
use super::def_literals::build_label_entries;
use super::types::{CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope_checked};
use super::value_schema::ExpansionBudget;

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_source_schema(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
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
        self.validate_source_schema_members(checker, &m, call)?;

        let id = self.string(checker, Self::member(&m, "id", call)?)?;
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let mut payload = Map::new();
        payload.insert(
            "format".to_owned(),
            Value::String("norn.source-schema/1".to_owned()),
        );
        for key in ["source", "observedAt", "adapter", "rawMetadata"] {
            payload.insert(
                key.to_owned(),
                self.json(checker, expansion, Self::member(&m, key, call)?, 0)?,
            );
        }
        for key in ["datasets", "fields", "codeSets", "constraints"] {
            if let Some(node) = m.get(key) {
                let val = self.json(checker, expansion, node, 0)?;
                if val.as_array().is_some_and(|a| !a.is_empty()) {
                    payload.insert(key.to_owned(), val);
                }
            }
        }
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "source_schema".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
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

    fn validate_source_schema_members(
        &self,
        checker: &mut Checker,
        m: &Members,
        call: P<Node>,
    ) -> Result<(), CompileError> {
        self.closed_record(
            checker,
            Self::member(m, "source", call)?,
            &["system", "instance", "version"],
            &["system", "instance"],
        )?;
        self.closed_record(
            checker,
            Self::member(m, "adapter", call)?,
            &["name", "version"],
            &["name", "version"],
        )?;
        self.closed_record(
            checker,
            Self::member(m, "rawMetadata", call)?,
            &["uri", "sha256"],
            &["uri", "sha256"],
        )?;
        for node in self.list(checker, Self::member(m, "datasets", call)?)? {
            self.closed_record(
                checker,
                node,
                &["id", "nativeName", "namespace", "kind"],
                &["id", "nativeName", "kind"],
            )?;
        }
        for node in self.list(checker, Self::member(m, "fields", call)?)? {
            let field = self.closed_record(
                checker,
                node,
                &[
                    "dataset",
                    "name",
                    "ordinal",
                    "nativeType",
                    "typeCandidate",
                    "required",
                    "markings",
                ],
                &["dataset", "name", "ordinal", "nativeType", "typeCandidate"],
            )?;
            self.closed_record(
                checker,
                Self::member(&field, "typeCandidate", node)?,
                &["type", "origin"],
                &["type", "origin"],
            )?;
        }
        if let Some(cs_node) = m.get("codeSets") {
            for node in self.list(checker, cs_node)? {
                let set = self.closed_record(
                    checker,
                    node,
                    &["id", "dataset", "field", "origin", "values"],
                    &["id", "dataset", "field", "origin", "values"],
                )?;
                for val in self.list(checker, Self::member(&set, "values", node)?)? {
                    self.closed_record(checker, val, &["value", "label"], &["value"])?;
                }
            }
        }
        if let Some(c_node) = m.get("constraints") {
            for node in self.list(checker, c_node)? {
                let constraint = self.closed_record(
                    checker,
                    node,
                    &[
                        "id",
                        "kind",
                        "dataset",
                        "fields",
                        "target",
                        "collection",
                        "origin",
                    ],
                    &["id", "kind", "dataset", "fields", "origin"],
                )?;
                if let Some(target) = constraint.get("target") {
                    self.closed_record(
                        checker,
                        target,
                        &["dataset", "fields"],
                        &["dataset", "fields"],
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn compile_ontology_pack(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
            &[
                "id", "title", "version", "sources", "terms", "mappings", "shapes",
            ],
        )?;
        self.validate_ontology_pack_sources_and_terms(checker, &m, call)?;
        self.validate_ontology_pack_mappings_and_shapes(checker, &m, call)?;

        let id = self.string(checker, Self::member(&m, "id", call)?)?;
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let mut payload = Map::new();
        payload.insert("packId".to_owned(), Value::String(id.clone()));
        payload.insert("packVersion".to_owned(), Value::String("0.1".to_owned()));
        payload.insert("packKind".to_owned(), Value::String("ontology".to_owned()));
        for key in ["sources", "terms", "mappings", "shapes"] {
            payload.insert(
                key.to_owned(),
                self.json(checker, expansion, Self::member(&m, key, call)?, 0)?,
            );
        }
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "business_pack".to_owned(),
                artifact_id: id,
                namespace: "norn.ontology".to_owned(),
                name: title,
                version,
                dependencies: Vec::new(),
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

    fn validate_ontology_pack_sources_and_terms(
        &self,
        checker: &mut Checker,
        m: &Members,
        call: P<Node>,
    ) -> Result<(), CompileError> {
        for node in self.list(checker, Self::member(m, "sources", call)?)? {
            let source = self.closed_record(
                checker,
                node,
                &[
                    "sourceId",
                    "releaseId",
                    "releasedAt",
                    "retrievedAt",
                    "canonicalUrl",
                    "license",
                    "redistribution",
                    "importMode",
                    "importer",
                    "files",
                ],
                &[
                    "sourceId",
                    "releaseId",
                    "releasedAt",
                    "retrievedAt",
                    "canonicalUrl",
                    "license",
                    "redistribution",
                    "importMode",
                ],
            )?;
            if let Some(imp) = source.get("importer") {
                self.closed_record(checker, imp, &["name", "version"], &["name", "version"])?;
            }
            if let Some(files) = source.get("files") {
                for f in self.list(checker, files)? {
                    self.closed_record(checker, f, &["path", "sha256"], &["path", "sha256"])?;
                }
            }
        }
        for node in self.list(checker, Self::member(m, "terms", call)?)? {
            self.closed_record(
                checker,
                node,
                &[
                    "termId",
                    "label",
                    "definition",
                    "termKind",
                    "status",
                    "semanticAuthority",
                    "parentTermId",
                    "supersededBy",
                    "domainTermId",
                ],
                &[
                    "termId",
                    "label",
                    "definition",
                    "termKind",
                    "status",
                    "semanticAuthority",
                ],
            )?;
        }
        Ok(())
    }

    fn validate_ontology_pack_mappings_and_shapes(
        &self,
        checker: &mut Checker,
        m: &Members,
        call: P<Node>,
    ) -> Result<(), CompileError> {
        for node in self.list(checker, Self::member(m, "mappings", call)?)? {
            self.closed_record(
                checker,
                node,
                &[
                    "mappingId",
                    "termId",
                    "sourceId",
                    "externalIri",
                    "relation",
                    "justification",
                    "evidenceRefs",
                    "confidenceBasisPoints",
                    "reviewedBy",
                    "reviewedAt",
                    "supersededByMappingId",
                    "ratifiedBy",
                    "ratifiedAt",
                ],
                &[
                    "mappingId",
                    "termId",
                    "sourceId",
                    "externalIri",
                    "relation",
                    "justification",
                    "evidenceRefs",
                    "confidenceBasisPoints",
                    "reviewedBy",
                    "reviewedAt",
                ],
            )?;
        }
        for node in self.list(checker, Self::member(m, "shapes", call)?)? {
            let shape = self.closed_record(
                checker,
                node,
                &["shapeId", "termId", "closed", "fields"],
                &["shapeId", "termId", "closed", "fields"],
            )?;
            for f in self.list(checker, Self::member(&shape, "fields", node)?)? {
                self.closed_record(
                    checker,
                    f,
                    &[
                        "fieldId",
                        "valueType",
                        "targetTermId",
                        "minCount",
                        "maxCount",
                    ],
                    &["fieldId", "valueType", "minCount", "maxCount"],
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn compile_pack_link(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
            &[
                "id",
                "title",
                "version",
                "fromPack",
                "toPack",
                "correspondences",
                "residuals",
            ],
        )?;
        self.validate_pack_link_members(checker, &m, call)?;

        let id = self.string(checker, Self::member(&m, "id", call)?)?;
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let mut payload = Map::new();
        payload.insert("packId".to_owned(), Value::String(id.clone()));
        payload.insert("packVersion".to_owned(), Value::String("0.1".to_owned()));
        payload.insert("packKind".to_owned(), Value::String("link".to_owned()));
        for key in ["fromPack", "toPack", "correspondences"] {
            payload.insert(
                key.to_owned(),
                self.json(checker, expansion, Self::member(&m, key, call)?, 0)?,
            );
        }
        if let Some(res_node) = m.get("residuals") {
            let val = self.json(checker, expansion, res_node, 0)?;
            if val.as_array().is_some_and(|a| !a.is_empty()) {
                payload.insert("residuals".to_owned(), val);
            }
        }
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "business_pack".to_owned(),
                artifact_id: id,
                namespace: "norn.ontology".to_owned(),
                name: title,
                version,
                dependencies: Vec::new(),
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

    fn validate_pack_link_members(
        &self,
        checker: &mut Checker,
        m: &Members,
        call: P<Node>,
    ) -> Result<(), CompileError> {
        for key in ["fromPack", "toPack"] {
            self.closed_record(
                checker,
                Self::member(m, key, call)?,
                &["packId", "version"],
                &["packId", "version"],
            )?;
        }
        for node in self.list(checker, Self::member(m, "correspondences", call)?)? {
            self.closed_record(
                checker,
                node,
                &[
                    "correspondenceId",
                    "fromTermId",
                    "relation",
                    "toTermIds",
                    "justification",
                    "evidenceRefs",
                    "confidenceBasisPoints",
                    "reviewedBy",
                    "reviewedAt",
                    "transformRef",
                    "rulingRef",
                ],
                &[
                    "correspondenceId",
                    "fromTermId",
                    "relation",
                    "toTermIds",
                    "justification",
                    "evidenceRefs",
                    "confidenceBasisPoints",
                    "reviewedBy",
                    "reviewedAt",
                ],
            )?;
        }
        if let Some(res_node) = m.get("residuals") {
            for node in self.list(checker, res_node)? {
                self.closed_record(
                    checker,
                    node,
                    &["termId", "reason", "note"],
                    &["termId", "reason", "note"],
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn compile_markings(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(checker, call, &["id", "title", "version", "markings"])?;
        let id = self.string(checker, Self::member(&m, "id", call)?)?;
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let mut items = Vec::new();
        for node in self.list(checker, Self::member(&m, "markings", call)?)? {
            let d = self.closed_record(
                checker,
                node,
                &["id", "label", "severity", "category", "mask"],
                &["id", "label", "severity", "category", "mask"],
            )?;
            let mask = Self::member(&d, "mask", node)?;
            let mask_rec =
                self.closed_record(checker, mask, &["kind", "count", "subject"], &["kind"])?;
            let kind = self.string(checker, Self::member(&mask_rec, "kind", mask)?)?;
            let fields: &[&str] = match kind.as_str() {
                "last-n" => &["kind", "count"],
                "pseudonym" => &["kind", "subject"],
                _ => &["kind"],
            };
            self.closed_record(checker, mask, fields, fields)?;
            items.push(self.json(checker, expansion, node, 0)?);
        }
        let mut payload = Map::new();
        payload.insert("entityVersion".to_owned(), Value::String("2.0".to_owned()));
        payload.insert("markings".to_owned(), Value::Array(items));
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "entity".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
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

    pub(crate) fn compile_labels(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
            &[
                "id",
                "title",
                "version",
                "requiredLocales",
                "needsReview",
                "types",
                "actions",
            ],
        )?;
        let id = self.string(checker, Self::member(&m, "id", call)?)?;
        let title = self.string(checker, Self::member(&m, "title", call)?)?;
        let version = self.string(checker, Self::member(&m, "version", call)?)?;
        let required = match m.get("requiredLocales") {
            Some(r) => self.strings(checker, r)?,
            None => Vec::new(),
        };
        let default_review = match m.get("needsReview") {
            Some(r) => self.strings(checker, r)?,
            None => Vec::new(),
        };
        let mut entries = Vec::new();
        if let Some(types_node) = m.get("types") {
            self.collect_type_label_entries(checker, types_node, &default_review, &mut entries)?;
        }
        if let Some(actions_node) = m.get("actions") {
            self.collect_action_label_entries(
                checker,
                actions_node,
                &default_review,
                &mut entries,
            )?;
        }
        let mut payload = Map::new();
        payload.insert("labelsVersion".to_owned(), Value::String("0.1".to_owned()));
        payload.insert("entries".to_owned(), Value::Array(entries));
        if !required.is_empty() {
            payload.insert(
                "requiredLocales".to_owned(),
                Value::Array(required.into_iter().map(Value::String).collect()),
            );
        }
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope_checked(
            EnvelopeMeta {
                kind: "labels".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
            },
            Value::Object(payload),
            options,
            call,
        )?;
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: None,
        })
    }

    fn collect_type_label_entries(
        &self,
        checker: &mut Checker,
        types_node: P<Node>,
        default_review: &[String],
        entries: &mut Vec<Value>,
    ) -> Result<(), CompileError> {
        for (type_name, node) in self.record(checker, types_node)?.iter() {
            let tm = self.closed_record(
                checker,
                node,
                &[
                    "label",
                    "localizedDescription",
                    "needsReview",
                    "properties",
                    "values",
                ],
                &[],
            )?;
            let mut loc = self.localized(checker, &tm)?;
            if loc.needs_review.is_none() {
                loc.needs_review = Some(default_review.to_vec());
            }
            let property_labels = match tm.get("properties") {
                Some(p_node) => {
                    let mut list = Vec::new();
                    for (pid, txt) in self.record(checker, p_node)?.iter() {
                        list.push((pid.to_owned(), self.locale_text(checker, txt)?));
                    }
                    Some(list)
                }
                None => None,
            };
            let value_labels = match tm.get("values") {
                Some(v_node) => {
                    let mut list = Vec::new();
                    for (pid, by_val) in self.record(checker, v_node)?.iter() {
                        let mut inner = Vec::new();
                        for (val, txt) in self.record(checker, by_val)?.iter() {
                            inner.push((val.to_owned(), self.locale_text(checker, txt)?));
                        }
                        list.push((pid.to_owned(), inner));
                    }
                    Some(list)
                }
                None => None,
            };
            let ml = MemberLabelsData {
                property_labels,
                value_labels,
            };
            entries.extend(build_label_entries(
                &LabelTarget::ObjectType(type_name.to_owned()),
                &loc,
                Some(&ml),
            ));
        }
        Ok(())
    }

    fn collect_action_label_entries(
        &self,
        checker: &mut Checker,
        actions_node: P<Node>,
        default_review: &[String],
        entries: &mut Vec<Value>,
    ) -> Result<(), CompileError> {
        for (action_id, node) in self.record(checker, actions_node)?.iter() {
            let am = self.closed_record(
                checker,
                node,
                &["label", "localizedDescription", "needsReview"],
                &[],
            )?;
            let mut loc: LocalizedData = self.localized(checker, &am)?;
            if loc.needs_review.is_none() {
                loc.needs_review = Some(default_review.to_vec());
            }
            entries.extend(build_label_entries(
                &LabelTarget::Action(action_id.to_owned()),
                &loc,
                None,
            ));
        }
        Ok(())
    }
}
