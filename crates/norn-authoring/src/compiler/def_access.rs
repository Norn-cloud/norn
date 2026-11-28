//! Access policy (`Norn.access`) and connector (`Norn.connector`) lowering
//! split out of [`super::def_actions`].

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{Map, Value};
use tsrs_ast::Node;
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::is_dotted_lower_id;
use super::def_helpers::{DefinitionCompiler, LoweredDef, Members, OBJECT_TYPES};
use super::types::{CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope};
use super::value_schema::ExpansionBudget;

const SELF_SCOPE_ATTRS: &[&str] = &[
    "id",
    "subject_id",
    "subjectid",
    "actor_id",
    "actorid",
    "user_id",
    "userid",
    "principal_id",
    "principalid",
    "employee_id",
    "employeeid",
    "person_id",
    "personid",
    "member_id",
    "memberid",
];

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_access(
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
                "rules",
                "principalCatalog",
                "scopedRoles",
            ],
        )?;
        let id = self.string(checker, Self::member(&members, "id", call)?)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "policy id {id:?} must be dotted lower-case segments (commerce.sales_order.access)"
                ),
                call,
            ));
        }
        let title = self.string(checker, Self::member(&members, "title", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        let scoped_roles_node = members.get("scopedRoles");
        let scoped_roles = self.read_scoped_roles(checker, scoped_roles_node)?;
        let mut used_scoped_roles: HashSet<String> = HashSet::new();
        let mut rules_json = Vec::new();
        for r in self.list(checker, Self::member(&members, "rules", call)?)? {
            rules_json.push(self.compile_access_rule(
                checker,
                r,
                &scoped_roles,
                &mut used_scoped_roles,
            )?);
        }
        if let Some(sr_node) = scoped_roles_node {
            for (role, _, _) in &scoped_roles {
                if !used_scoped_roles.contains(role) {
                    return Err(CompileError::new(
                        "NORN-FLOW-VALUE",
                        format!(
                            "scopedRoles declares role {role:?} which does not appear in any rule"
                        ),
                        sr_node,
                    ));
                }
            }
        }
        let mut payload = Map::new();
        payload.insert("accessVersion".to_owned(), Value::String("0.1".to_owned()));
        payload.insert("policyId".to_owned(), Value::String(id.clone()));
        payload.insert("rules".to_owned(), Value::Array(rules_json));
        if let Some(pc_node) = members.get("principalCatalog") {
            let mut pc = Vec::new();
            for entry in self.list(checker, pc_node)? {
                pc.push(self.json(checker, entry, 0)?);
            }
            if !pc.is_empty() {
                payload.insert("principalCatalog".to_owned(), Value::Array(pc));
            }
        }
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope(
            EnvelopeMeta {
                kind: "policy".to_owned(),
                artifact_id: format!("{id}.policy"),
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
            },
            Value::Object(payload),
            options,
        );
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: Some(self.at(call)),
        })
    }

    fn read_scoped_roles(
        &self,
        checker: &mut Checker,
        node: Option<P<Node>>,
    ) -> Result<Vec<(String, String, String)>, CompileError> {
        let mut out = Vec::new();
        let Some(sr_node) = node else {
            return Ok(out);
        };
        for entry in self.list(checker, sr_node)? {
            let s = self.closed_record(
                checker,
                entry,
                &["role", "attribute", "property"],
                &["role", "attribute", "property"],
            )?;
            out.push((
                self.string(checker, Self::member(&s, "role", entry)?)?,
                self.string(checker, Self::member(&s, "attribute", entry)?)?,
                self.string(checker, Self::member(&s, "property", entry)?)?,
            ));
        }
        Ok(out)
    }

    fn compile_access_rule(
        &mut self,
        checker: &mut Checker,
        r: P<Node>,
        scoped_roles: &[(String, String, String)],
        used_scoped_roles: &mut HashSet<String>,
    ) -> Result<Value, CompileError> {
        let m = self.closed_record(
            checker,
            r,
            &["ruleId", "actions", "resourceType", "scope", "allOf"],
            &["ruleId", "actions", "resourceType", "scope"],
        )?;
        let scope_node = Self::member(&m, "scope", r)?;
        let scope = self.closed_record(checker, scope_node, &["dimensions"], &["dimensions"])?;
        let mut all_of = Vec::new();
        if let Some(ao_node) = m.get("allOf") {
            for cond in self.list(checker, ao_node)? {
                all_of.push(self.json(checker, cond, 0)?);
            }
        }
        if !scoped_roles.is_empty() {
            inject_scoped_roles(scoped_roles, used_scoped_roles, &mut all_of, r)?;
        }
        let mut actions = Vec::new();
        for a in self.list(checker, Self::member(&m, "actions", r)?)? {
            actions.push(Value::String(self.action_id(checker, a)?));
        }
        let mut scope_obj = Map::new();
        scope_obj.insert(
            "dimensions".to_owned(),
            Value::Array(
                self.strings(checker, Self::member(&scope, "dimensions", scope_node)?)?
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
        let mut rule_obj = Map::new();
        rule_obj.insert(
            "ruleId".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "ruleId", r)?)?),
        );
        rule_obj.insert("actions".to_owned(), Value::Array(actions));
        rule_obj.insert(
            "resourceType".to_owned(),
            Value::String(self.type_name(
                checker,
                Self::member(&m, "resourceType", r)?,
                OBJECT_TYPES,
            )?),
        );
        rule_obj.insert("scope".to_owned(), Value::Object(scope_obj));
        if !all_of.is_empty() {
            rule_obj.insert("allOf".to_owned(), Value::Array(all_of));
        }
        Ok(Value::Object(rule_obj))
    }

    pub(crate) fn compile_connector(
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
                "auth",
                "resources",
                "mappings",
                "cursor",
                "deadLetterPolicy",
                "poll",
                "pull",
            ],
        )?;
        let id = self.string(checker, Self::member(&members, "id", call)?)?;
        if !is_dotted_lower_id(&id) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "connector id must be dotted lower-case segments",
                call,
            ));
        }
        let auth = Self::member(&members, "auth", call)?;
        let resources = Self::member(&members, "resources", call)?;
        let mappings = Self::member(&members, "mappings", call)?;
        self.validate_connector_auth(checker, auth)?;
        self.validate_connector_resources(checker, resources)?;
        self.validate_connector_mappings_and_extras(checker, mappings, &members)?;

        let mut payload = Map::new();
        payload.insert(
            "connectorVersion".to_owned(),
            Value::String("0.1".to_owned()),
        );
        payload.insert("auth".to_owned(), self.json(checker, auth, 0)?);
        payload.insert("resources".to_owned(), self.json(checker, resources, 0)?);
        payload.insert("mappings".to_owned(), self.json(checker, mappings, 0)?);
        if let Some(c) = members.get("cursor") {
            payload.insert("cursor".to_owned(), self.json(checker, c, 0)?);
        }
        if let Some(dl) = members.get("deadLetterPolicy") {
            payload.insert("deadLetterPolicy".to_owned(), self.json(checker, dl, 0)?);
        }
        let mut exts = Map::new();
        if let Some(p) = members.get("poll") {
            exts.insert("norn.poll/1".to_owned(), self.json(checker, p, 0)?);
        }
        if let Some(p) = members.get("pull") {
            exts.insert("norn.pull/1".to_owned(), self.json(checker, p, 0)?);
        }
        if !exts.is_empty() {
            payload.insert("extensions".to_owned(), Value::Object(exts));
        }

        let title = self.string(checker, Self::member(&members, "title", call)?)?;
        let version = self.string(checker, Self::member(&members, "version", call)?)?;
        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope(
            EnvelopeMeta {
                kind: "connector".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
            },
            Value::Object(payload),
            options,
        );
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: Some(self.at(call)),
        })
    }

    fn validate_connector_mappings_and_extras(
        &self,
        checker: &mut Checker,
        mappings: P<Node>,
        members: &Members,
    ) -> Result<(), CompileError> {
        for mapping in self.list(checker, mappings)? {
            let m = self.record(checker, mapping)?;
            let kind = self.string(checker, Self::member(&m, "kind", mapping)?)?;
            if kind != "field" && kind != "external_id" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "unknown connector mapping kind",
                    mapping,
                ));
            }
            let last_key = if kind == "field" {
                "targetField"
            } else {
                "externalSource"
            };
            self.closed_record(
                checker,
                mapping,
                &[
                    "kind",
                    "resourceId",
                    "sourcePath",
                    "targetObjectKind",
                    last_key,
                ],
                &["kind", "resourceId", "sourcePath", "targetObjectKind"],
            )?;
        }
        if let Some(pull) = members.get("pull") {
            self.validate_connector_pull(checker, pull)?;
        }
        if let Some(cursor) = members.get("cursor") {
            self.closed_record(
                checker,
                cursor,
                &["cursorId", "resourceId", "storageKey"],
                &["cursorId", "resourceId", "storageKey"],
            )?;
        }
        if let Some(dl) = members.get("deadLetterPolicy") {
            self.closed_record(
                checker,
                dl,
                &["maxFailures", "route"],
                &["maxFailures", "route"],
            )?;
        }
        if let Some(poll) = members.get("poll") {
            let pm = self.closed_record(checker, poll, &["feeds"], &["feeds"])?;
            for feed in self.list(checker, Self::member(&pm, "feeds", poll)?)? {
                let fm = self.closed_record(
                    checker,
                    feed,
                    &["resourceId", "recordKeyPath", "fanout"],
                    &["resourceId"],
                )?;
                if let Some(fan) = fm.get("fanout") {
                    self.closed_record(
                        checker,
                        fan,
                        &["parentResourceId", "pathBinding", "keyPath"],
                        &["parentResourceId", "pathBinding", "keyPath"],
                    )?;
                }
            }
        }
        Ok(())
    }

    fn validate_connector_auth(
        &self,
        checker: &mut Checker,
        auth: P<Node>,
    ) -> Result<(), CompileError> {
        let prelim = self.closed_record(
            checker,
            auth,
            &[
                "kind",
                "authId",
                "placement",
                "name",
                "secretRef",
                "prefix",
                "format",
                "authorizationUrl",
                "tokenUrl",
                "scopes",
                "clientSecretRef",
            ],
            &["kind"],
        )?;
        let kind = self.string(checker, Self::member(&prelim, "kind", auth)?)?;
        let keys: &[&str] = match kind.as_str() {
            "none" => &["kind"],
            "api_key" => &[
                "kind",
                "authId",
                "placement",
                "name",
                "secretRef",
                "prefix",
                "format",
            ],
            "oauth2" => &[
                "kind",
                "authId",
                "authorizationUrl",
                "tokenUrl",
                "scopes",
                "clientSecretRef",
            ],
            _ => {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "unknown connector auth kind",
                    auth,
                ));
            }
        };
        let auth_m = self.closed_record(checker, auth, keys, &["kind"])?;
        if let Some(fmt) = auth_m.get("format") {
            let fm = self.closed_record(checker, fmt, &["kind", "separator"], &["kind"])?;
            let fkind = self.string(checker, Self::member(&fm, "kind", fmt)?)?;
            let fkeys: &[&str] = if fkind == "pair" {
                &["kind", "separator"]
            } else {
                &["kind"]
            };
            self.closed_record(checker, fmt, fkeys, &["kind"])?;
        }
        Ok(())
    }

    fn validate_connector_resources(
        &self,
        checker: &mut Checker,
        resources: P<Node>,
    ) -> Result<(), CompileError> {
        for resource in self.list(checker, resources)? {
            let m = self.record(checker, resource)?;
            let kind = self.string(checker, Self::member(&m, "kind", resource)?)?;
            let keys: &[&str] = match kind.as_str() {
                "paginated" => &[
                    "kind",
                    "resourceId",
                    "endpoint",
                    "itemsPath",
                    "missingItemsIsEmpty",
                    "queryParams",
                    "method",
                    "pageParam",
                    "pageSizeParam",
                    "pageMode",
                    "pageStart",
                    "pageSize",
                ],
                "cursor" => &[
                    "kind",
                    "resourceId",
                    "endpoint",
                    "itemsPath",
                    "missingItemsIsEmpty",
                    "queryParams",
                    "cursorParam",
                    "nextCursorPath",
                ],
                "delta" => &[
                    "kind",
                    "resourceId",
                    "endpoint",
                    "itemsPath",
                    "missingItemsIsEmpty",
                    "queryParams",
                    "sinceParam",
                    "updatedAtPath",
                ],
                "webhook" => &["kind", "resourceId", "eventType", "payloadPath"],
                "csv_import" => &["kind", "resourceId", "delimiter", "columns"],
                _ => {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "unknown connector resource kind",
                        resource,
                    ));
                }
            };
            self.closed_record(checker, resource, keys, &["kind", "resourceId"])?;
        }
        Ok(())
    }

    fn validate_connector_pull(
        &self,
        checker: &mut Checker,
        pull: P<Node>,
    ) -> Result<(), CompileError> {
        let m = self.closed_record(
            checker,
            pull,
            &["source", "pipeline", "feeds", "token"],
            &["source", "pipeline", "feeds"],
        )?;
        if let Some(tok) = m.get("token") {
            self.closed_record(checker, tok, &["header", "prefix"], &["header"])?;
        }
        for feed in self.list(checker, Self::member(&m, "feeds", pull)?)? {
            self.validate_connector_pull_feed(checker, feed)?;
        }
        Ok(())
    }

    fn validate_connector_pull_feed(
        &self,
        checker: &mut Checker,
        feed: P<Node>,
    ) -> Result<(), CompileError> {
        let f = self.closed_record(
            checker,
            feed,
            &[
                "dataset",
                "request",
                "itemsPath",
                "paging",
                "idPath",
                "versionPath",
                "fields",
                "initial",
                "mode",
                "versionKind",
            ],
            &[
                "dataset",
                "request",
                "itemsPath",
                "paging",
                "idPath",
                "versionPath",
                "fields",
                "initial",
            ],
        )?;
        if let Some(mode) = f.get("mode") {
            let v = self.string(checker, mode)?;
            if v != "full_snapshot" && v != "patch" {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("unknown pull feed mode {v}"),
                    mode,
                ));
            }
        }
        if let Some(vk) = f.get("versionKind") {
            let v = self.string(checker, vk)?;
            if !matches!(
                v.as_str(),
                "lexicographical" | "int64" | "uint64" | "rfc3339"
            ) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("unknown pull feed versionKind {v}"),
                    vk,
                ));
            }
        }
        self.closed_record(
            checker,
            Self::member(&f, "request", feed)?,
            &["method", "path", "query", "headers", "body"],
            &["method", "path"],
        )?;
        let paging_node = Self::member(&f, "paging", feed)?;
        let paging = self.closed_record(
            checker,
            paging_node,
            &["kind", "size", "nextCursorPath", "hasNextPath"],
            &["kind", "size"],
        )?;
        if self.string(checker, Self::member(&paging, "kind", paging_node)?)? == "keyset" {
            self.closed_record(checker, paging_node, &["kind", "size"], &["kind", "size"])?;
        }
        let init_node = Self::member(&f, "initial", feed)?;
        let init = self.closed_record(
            checker,
            init_node,
            &["version", "id", "idDecimal"],
            &["version"],
        )?;
        if init.has("id") == init.has("idDecimal") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "pull feed initial must declare exactly one of id or idDecimal",
                init_node,
            ));
        }
        if let Some(id_dec) = init.get("idDecimal") {
            self.string(checker, id_dec)?;
        }
        Ok(())
    }
}

fn inject_scoped_roles(
    scoped_roles: &[(String, String, String)],
    used_scoped_roles: &mut HashSet<String>,
    all_of: &mut Vec<Value>,
    rule_node: P<Node>,
) -> Result<(), CompileError> {
    let mut rule_roles: BTreeSet<String> = BTreeSet::new();
    for cond in all_of.iter() {
        if let Some(arr) = cond
            .get("role")
            .and_then(|r| r.get("anyOf"))
            .and_then(Value::as_array)
        {
            for v in arr {
                if let Some(s) = v.as_str() {
                    rule_roles.insert(s.to_owned());
                }
            }
        }
    }
    if rule_roles.is_empty() {
        return Ok(());
    }
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, (String, String, HashSet<String>)> = BTreeMap::new();
    for (role, attr, prop) in scoped_roles {
        if rule_roles.contains(role) {
            used_scoped_roles.insert(role.clone());
        }
        let key = format!("{attr}\0{prop}");
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
            groups.insert(key.clone(), (attr.clone(), prop.clone(), HashSet::new()));
        }
        if let Some(entry) = groups.get_mut(&key) {
            entry.2.insert(role.clone());
        }
    }
    let has_explicit_self_scope = all_of.iter().any(|cond| {
        let Some(res) = cond.get("resource") else {
            return false;
        };
        res.get("op").and_then(Value::as_str) == Some("eq")
            && res
                .get("value")
                .and_then(|v| v.get("principalAttribute"))
                .and_then(Value::as_str)
                .is_some_and(|attr| SELF_SCOPE_ATTRS.contains(&attr.to_ascii_lowercase().as_str()))
    });
    for key in group_order {
        let Some((attr, prop, roles)) = groups.get(&key) else {
            continue;
        };
        let matched: Vec<&str> = rule_roles
            .iter()
            .filter(|r| roles.contains(r.as_str()))
            .map(String::as_str)
            .collect();
        if matched.is_empty() {
            continue;
        }
        let already_scoped = all_of.iter().any(|cond| {
            let Some(res) = cond.get("resource") else {
                return false;
            };
            res.get("name").and_then(Value::as_str) == Some(prop.as_str())
                && res.get("op").and_then(Value::as_str) == Some("in")
                && res
                    .get("value")
                    .and_then(|v| v.get("principalAttribute"))
                    .and_then(Value::as_str)
                    == Some(attr.as_str())
        });
        if already_scoped || has_explicit_self_scope {
            continue;
        }
        if matched.len() < rule_roles.len() {
            return Err(CompileError::new(
                "NORN-FLOW-VALUE",
                format!(
                    "rule mixes scoped role(s) [{}] with unscoped roles; split the rule or declare an explicit resource scope predicate",
                    matched.join(", ")
                ),
                rule_node,
            ));
        }
        let mut pa = Map::new();
        pa.insert("principalAttribute".to_owned(), Value::String(attr.clone()));
        let mut res = Map::new();
        res.insert("name".to_owned(), Value::String(prop.clone()));
        res.insert("op".to_owned(), Value::String("in".to_owned()));
        res.insert("value".to_owned(), Value::Object(pa));
        let mut cond = Map::new();
        cond.insert("resource".to_owned(), Value::Object(res));
        all_of.push(Value::Object(cond));
    }
    Ok(())
}
