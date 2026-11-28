//! Module-level definition compilation dispatch and retained `Norn.approval`
//! lowering split out of [`super::def_laws`].

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node, SourceFile, get_source_file_of_node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::canonical::to_canonical_json;
use super::def_helpers::{
    ACCESS, ACTION, APPROVAL, CONNECTOR, CompiledExample, DASHBOARD, DECISION, DefinitionCompiler,
    DefinitionSource, FLOW, INTERFACE, LABELS, LAW, LoweredDef, MAPPING, MARKINGS, MODEL,
    NORM_MEMBERS, OBJECT_TYPES, ONTOLOGY_PACK, PACK_LINK, QUESTION, RetainedDefinitionDoc, SETTING,
    SOURCE_SCHEMA, TRANSFORM, TYPE, is_definition_call, is_norm_call,
};
use super::types::{CompileError, EnvelopeOptions, diagnostic_for_node};
use super::value_schema::ExpansionBudget;

/// Output of compiling all definitions in one source file.
pub(crate) struct CompiledModuleDefs {
    pub(crate) artifacts: Vec<Value>,
    pub(crate) flows: usize,
    pub(crate) examples: Vec<CompiledExample>,
    pub(crate) retained: Vec<RetainedDefinitionDoc>,
}

#[derive(Default)]
struct ModuleAcc {
    artifacts: Vec<Value>,
    retained: Vec<RetainedDefinitionDoc>,
    def_count: usize,
    flows: usize,
    answers: usize,
}

impl DefinitionCompiler<'_> {
    /// Compiles every top-level Norn definition in `source`.
    pub(crate) fn compile_module(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        source: P<SourceFile>,
        options: &EnvelopeOptions,
        allow_empty: bool,
    ) -> Result<CompiledModuleDefs, CompileError> {
        let mut acc = ModuleAcc::default();
        for &stmt in source.as_node().statements() {
            let sk = stmt.kind();
            if sk == Kind::ExportAssignment {
                self.check_default_export_stmt(checker, stmt, &mut acc.flows)?;
                continue;
            }
            if sk != Kind::VariableStatement {
                continue;
            }
            let decls = stmt
                .as_variable_statement()
                .declaration_list
                .as_variable_declaration_list()
                .declarations
                .nodes();
            for &d in decls {
                if let Some(init) = d.as_variable_declaration().initializer() {
                    let u = Self::unwrap(init);
                    if u.kind() == Kind::CallExpression
                        && self.call_id(checker, u).as_deref() == Some(FLOW)
                    {
                        acc.flows = acc.flows.saturating_add(1);
                    }
                }
            }
            for &d in decls {
                self.compile_module_var_decl(checker, expansion, source, d, options, &mut acc)?;
            }
        }
        if acc.def_count == 0 && acc.retained.is_empty() && acc.answers == 0 && !allow_empty {
            return Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "the module declares no Norn.type, Norn.interfaceType, Norn.action, Norn.access, Norn.decision, Norn.mapping, Norn.connector or Norn.approval definition",
                source.as_node(),
            ));
        }
        let examples = self.compile_examples(checker, expansion, source)?;
        if let Some(err) = self.deferred_errors.drain(..).next() {
            return Err(err);
        }
        Ok(CompiledModuleDefs {
            artifacts: acc.artifacts,
            flows: acc.flows,
            examples,
            retained: acc.retained,
        })
    }

    fn check_default_export_stmt(
        &self,
        checker: &mut Checker,
        stmt: P<Node>,
        flows: &mut usize,
    ) -> Result<(), CompileError> {
        let e = Self::unwrap(stmt.as_export_assignment().expression());
        if e.kind() != Kind::CallExpression {
            return Ok(());
        }
        match self.call_id(checker, e).as_deref() {
            Some(QUESTION) => Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "questions require named const bindings and the question compiler",
                e,
            )),
            Some(DASHBOARD) => Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "dashboards require named const bindings and the dashboard compiler",
                e,
            )),
            Some(APPROVAL) => Err(CompileError::new(
                "NORN-FLOW-RESULT",
                "approvals require named const bindings",
                e,
            )),
            Some(FLOW) => {
                *flows = flows.saturating_add(1);
                Ok(())
            }
            Some(_) | None => Ok(()),
        }
    }

    fn compile_module_var_decl(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        source: P<SourceFile>,
        decl: P<Node>,
        options: &EnvelopeOptions,
        acc: &mut ModuleAcc,
    ) -> Result<(), CompileError> {
        let Some(raw_init) = decl.as_variable_declaration().initializer() else {
            return Ok(());
        };
        let init = Self::unwrap(raw_init);
        if init.kind() != Kind::CallExpression {
            return Ok(());
        }
        let Some(id) = self.call_id(checker, init) else {
            return Ok(());
        };
        if id == QUESTION || id == DASHBOARD {
            let msg = if id == QUESTION {
                "a Norn.question is compiled by the question compiler (`norn question`), not installed as an artifact"
            } else {
                "a Norn.dashboard is compiled by the dashboard compiler (`norn dashboard`); the cell admits it as a definition, not an artifact"
            };
            self.warnings
                .push(diagnostic_for_node(init, "NORN-FLOW-RESULT", msg));
            acc.answers = acc.answers.saturating_add(1);
            return Ok(());
        }
        if id == APPROVAL {
            let approval = self.compile_approval(checker, expansion, init)?;
            if acc.retained.iter().any(|other| other.id == approval.id) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("duplicate approval {}", approval.id),
                    init,
                ));
            }
            Self::assert_expansion(expansion, &approval.payload, init)?;
            acc.retained.push(approval);
            return Ok(());
        }
        if is_norm_call(&id) {
            self.check_norm_module_locality(checker, source, init)?;
            return Ok(());
        }
        if !is_definition_call(&id) {
            return Ok(());
        }
        let lowered = self.dispatch_definition(checker, expansion, &id, init, options)?;
        acc.def_count = acc.def_count.saturating_add(1);
        let aid = lowered
            .artifact
            .get("artifactId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        acc.artifacts.push(lowered.artifact);
        if let Some(labels) = lowered.labels {
            acc.artifacts.push(labels);
        }
        if let Some(at) = lowered.at {
            self.source_map.push(DefinitionSource {
                artifact_id: aid,
                pointer: String::new(),
                at,
            });
        }
        Ok(())
    }

    fn check_norm_module_locality(
        &self,
        checker: &mut Checker,
        source: P<SourceFile>,
        init: P<Node>,
    ) -> Result<(), CompileError> {
        let Some(&arg1) = init.as_call_expression().arguments().nodes().get(1) else {
            return Ok(());
        };
        let rec = self.closed_record(checker, arg1, NORM_MEMBERS, &["on"])?;
        if let Some(on) = rec.get("on") {
            let (subject_call, _) =
                self.referenced(checker, on, OBJECT_TYPES, "a Norn.type definition")?;
            if get_source_file_of_node(subject_call) != Some(source) {
                return Err(CompileError::new(
                    "NORN-FLOW-MODULE",
                    "declare a norm in the module that declares its subject type: it lowers into that type's artifact",
                    init,
                ));
            }
        }
        Ok(())
    }

    fn dispatch_definition(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        id: &str,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        match id {
            SOURCE_SCHEMA => self.compile_source_schema(checker, expansion, call, options),
            ONTOLOGY_PACK => self.compile_ontology_pack(checker, expansion, call, options),
            PACK_LINK => self.compile_pack_link(checker, expansion, call, options),
            MARKINGS => self.compile_markings(checker, expansion, call, options),
            TYPE => self.compile_object_type(checker, expansion, call, options),
            INTERFACE => self.compile_interface_type(checker, expansion, call, options),
            ACTION => self.compile_action(checker, expansion, call, options),
            ACCESS => self.compile_access(checker, expansion, call, options),
            CONNECTOR => self.compile_connector(checker, expansion, call, options),
            MAPPING => self.compile_mapping(checker, expansion, call, options),
            LAW => self.compile_law(checker, expansion, call, options),
            TRANSFORM => self.compile_transform(checker, expansion, call, options),
            SETTING => self.compile_setting(checker, expansion, call, options),
            LABELS => self.compile_labels(checker, expansion, call, options),
            MODEL => self.compile_model(checker, expansion, call, options),
            DECISION => self.compile_decision(checker, expansion, call, options),
            _ => Err(CompileError::new(
                "NORN-FLOW-UNSUPPORTED-SYNTAX",
                format!("unsupported definition call {id}"),
                call,
            )),
        }
    }

    fn compile_approval(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
    ) -> Result<RetainedDefinitionDoc, CompileError> {
        let m = self.meta(
            checker,
            call,
            &[
                "id",
                "action",
                "decider",
                "question",
                "risk",
                "expiresInMs",
                "on",
                "when",
            ],
        )?;
        if let Some(on) = m.get("on") {
            self.referenced(checker, on, OBJECT_TYPES, "a Norn.type definition")?;
        }
        let decider_node = Self::member(&m, "decider", call)?;
        let decider = self.closed_record(checker, decider_node, &["roles", "law"], &["roles"])?;
        let roles = self.strings(checker, Self::member(&decider, "roles", decider_node)?)?;
        if roles.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "an approval names at least one decider role",
                decider_node,
            ));
        }
        let risk_node = Self::member(&m, "risk", call)?;
        let risk = self.string(checker, risk_node)?;
        if !matches!(risk.as_str(), "low" | "medium" | "high") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "risk is low, medium or high",
                risk_node,
            ));
        }
        let mut dec_obj = Map::new();
        dec_obj.insert(
            "roles".to_owned(),
            Value::Array(roles.into_iter().map(Value::String).collect()),
        );
        if let Some(law) = decider.get("law") {
            dec_obj.insert("law".to_owned(), Value::String(self.string(checker, law)?));
        }
        let mut payload = Map::new();
        payload.insert(
            "action".to_owned(),
            Value::String(self.action_id(checker, Self::member(&m, "action", call)?)?),
        );
        payload.insert("decider".to_owned(), Value::Object(dec_obj));
        payload.insert(
            "question".to_owned(),
            Value::String(self.string(checker, Self::member(&m, "question", call)?)?),
        );
        payload.insert("risk".to_owned(), Value::String(risk));
        if let Some(exp) = m.get("expiresInMs") {
            payload.insert(
                "expiresInMs".to_owned(),
                self.json(checker, expansion, exp, 0)?,
            );
        }
        if let Some(when_node) = m.get("when") {
            payload.insert(
                "when".to_owned(),
                self.compile_approval_when(checker, expansion, when_node)?,
            );
        }
        let raw_payload = Value::Object(payload);
        let canonical = to_canonical_json(&raw_payload)
            .map_err(|err| CompileError::new("NORN-FLOW-LITERAL", err.to_string(), call))?;
        Ok(RetainedDefinitionDoc {
            kind: "approval".to_owned(),
            id: self.string(checker, Self::member(&m, "id", call)?)?,
            payload: canonical,
        })
    }

    fn compile_approval_when(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        when_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let w = self.closed_record(
            checker,
            when_node,
            &["property", "op", "value"],
            &["property", "op", "value"],
        )?;
        let op_node = Self::member(&w, "op", when_node)?;
        let op = self.string(checker, op_node)?;
        if !matches!(op.as_str(), "eq" | "ne" | "lt" | "le" | "gt" | "ge") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "when.op is eq, ne, lt, le, gt or ge",
                op_node,
            ));
        }
        let val_node = Self::member(&w, "value", when_node)?;
        let val = self.when_value(checker, expansion, val_node)?;
        if !matches!(val, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "when.value is text, a number or a boolean",
                val_node,
            ));
        }
        let mut wo = Map::new();
        wo.insert(
            "property".to_owned(),
            Value::String(self.string(checker, Self::member(&w, "property", when_node)?)?),
        );
        wo.insert("op".to_owned(), Value::String(op));
        wo.insert("value".to_owned(), val);
        Ok(Value::Object(wo))
    }
}
