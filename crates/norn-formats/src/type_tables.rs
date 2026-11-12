//! Deterministic type tables as data: `RuleIR` v2 (JDM) decision-table
//! artifacts under `crates/norn-formats/tables/`, evaluated through
//! `norn-decisions` (the pinned Norn ZEN profile).
//!
//! Each table is a `decide` rule whose graph is one first-hit
//! `decisionTableNode` followed by an expression node that emits the matched
//! row's outputs as one `norn.formats.type_table.row` command record. The
//! importer never commits that record: it is the typed carrier of the row,
//! validated against the rule's closed output schema. Row order is semantic
//! (first hit) and is never re-sorted; every lookup reports the matched row
//! `_id` so each type decision is explainable.

use std::collections::BTreeMap;

use norn_decisions::{AdmittedDecision, DecisionBinding, ENGINE_TAG, evaluate_explained};
use norn_expr::{EvaluationBudget, Value};
use norn_ir::{RuleRoot, RuntimeValue};

use crate::contract::{ComponentPin, TablePin, sha256_hex};

/// The command `actionId` carrying a table row.
const ROW_ACTION: &str = "norn.formats.type_table.row";

/// The evaluator pin recorded in import provenance.
#[must_use]
pub fn evaluator_pin() -> ComponentPin {
    ComponentPin {
        name: "zen-engine (Norn-cloud/zen)".to_owned(),
        version: ENGINE_TAG.to_owned(),
    }
}

/// One input value of a lookup.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TableInput {
    /// A string.
    Text(String),
    /// A boolean.
    Flag(bool),
}

/// The matched row of one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    /// The rule artifact id of the table.
    pub table: String,
    /// The matched row's `_id`.
    pub row: String,
    values: BTreeMap<String, RuntimeValue>,
}

impl TableRow {
    /// A string output (empty when absent).
    #[must_use]
    pub fn text(&self, output: &str) -> &str {
        if let Some(RuntimeValue::String(text)) = self.values.get(output) {
            text
        } else {
            ""
        }
    }

    /// A boolean output (`false` when absent).
    #[must_use]
    pub fn flag(&self, output: &str) -> bool {
        matches!(self.values.get(output), Some(RuntimeValue::Bool(true)))
    }

    /// A whole-number output (`None` when absent or not a whole number).
    #[must_use]
    pub fn whole(&self, output: &str) -> Option<u64> {
        if let Some(RuntimeValue::Number(number)) = self.values.get(output) {
            number.to_i64().and_then(|value| u64::try_from(value).ok())
        } else {
            None
        }
    }
}

/// One admitted table with a per-input memo.
pub struct TypeTable {
    pin: TablePin,
    decision: AdmittedDecision,
    memo: BTreeMap<Vec<(String, TableInput)>, Result<TableRow, String>>,
}

impl std::fmt::Debug for TypeTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypeTable")
            .field("pin", &self.pin)
            .finish_non_exhaustive()
    }
}

impl TypeTable {
    /// Admit the rule artifact `text` (a `rule` envelope).
    ///
    /// # Errors
    ///
    /// When the artifact is not a `RuleIR` v2 decide rule the Norn profile
    /// admits.
    pub fn load(text: &str) -> Result<Self, String> {
        let envelope: serde_json::Value =
            serde_json::from_str(text).map_err(|error| format!("table is not JSON: {error}"))?;
        let artifact_id = envelope
            .get("artifactId")
            .and_then(serde_json::Value::as_str)
            .ok_or("table has no artifactId")?
            .to_owned();
        let version = envelope
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or("table has no version")?
            .to_owned();
        let payload = envelope.get("payload").cloned().unwrap_or_default();
        let root: RuleRoot = serde_json::from_value(payload)
            .map_err(|error| format!("{artifact_id}: payload is not a rule: {error}"))?;
        let sha256 = sha256_hex(text.as_bytes());
        let binding = DecisionBinding {
            profile_id: norn_expr::check::CHECKER_PROFILE.to_owned(),
            environment_hash: format!("sha256:{sha256}"),
        };
        let decision = AdmittedDecision::admit(&root, &BTreeMap::new(), binding)
            .map_err(|error| format!("{artifact_id}: not admitted: {error}"))?;
        Ok(Self {
            pin: TablePin {
                artifact_id,
                version,
                sha256,
            },
            decision,
            memo: BTreeMap::new(),
        })
    }

    /// The table's pin.
    #[must_use]
    pub const fn pin(&self) -> &TablePin {
        &self.pin
    }

    /// Evaluate the table on `event`: the first matching row.
    ///
    /// # Errors
    ///
    /// When evaluation fails or the row record is malformed.
    pub fn lookup(&mut self, event: &[(&str, TableInput)]) -> Result<TableRow, String> {
        let key: Vec<(String, TableInput)> = event
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        if let Some(found) = self.memo.get(&key) {
            return found.clone();
        }
        let found = self.evaluate(event);
        self.memo.insert(key, found.clone());
        found
    }

    fn evaluate(&self, event: &[(&str, TableInput)]) -> Result<TableRow, String> {
        let table = &self.pin.artifact_id;
        let slice = self
            .decision
            .validate_slice(Value::object(Vec::<(String, Value)>::new()))
            .map_err(|error| format!("{table}: slice: {error}"))?;
        let event = self
            .decision
            .type_event(Value::object(event.iter().map(|(name, value)| {
                (
                    (*name).to_owned(),
                    match value {
                        TableInput::Text(text) => Value::from(text.as_str()),
                        TableInput::Flag(flag) => Value::from(*flag),
                    },
                )
            })))
            .map_err(|error| format!("{table}: event: {error}"))?;
        let evaluated =
            evaluate_explained(&self.decision, &slice, &event, EvaluationBudget::default())
                .map_err(|error| format!("{table}: evaluation failed: {error}"))?;
        let [record] = evaluated.output.commands.as_slice() else {
            return Err(format!(
                "{table}: expected exactly one row record, got {}",
                evaluated.output.commands.len()
            ));
        };
        if record.get("actionId") != Some(&RuntimeValue::String(ROW_ACTION.to_owned())) {
            return Err(format!("{table}: the row record is not {ROW_ACTION}"));
        }
        let [matched] = evaluated.matched_rows.as_slice() else {
            return Err(format!(
                "{table}: expected exactly one matched row, got {}",
                evaluated.matched_rows.len()
            ));
        };
        let row = matched
            .row_id
            .clone()
            .ok_or_else(|| format!("{table}: the matched row has no _id"))?;
        Ok(TableRow {
            table: table.clone(),
            row,
            values: record.clone(),
        })
    }
}
