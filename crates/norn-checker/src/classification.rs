//! GRAPH-CLASS-1: pure membership over complete, schema-checked base snapshots.
use crate::{ArtifactRef, EntityArtifactSetContext};
use norn_expr::{EvaluationBudget, Expression, Value};
use norn_ir::{ArtifactEnvelope, DefinedConceptRoot, RuntimeValue};
use std::collections::BTreeSet;

/// A classification cannot establish membership from unknown or partial inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncompleteClassification;

impl IncompleteClassification {
    /// The stable GRAPH-CLASS-1 refusal code.
    pub const CODE: &str = "core.graph.incomplete_classification";
}

/// An admitted predicate and the exact definitions that qualified it.
/// This holds no membership state and cannot emit effects or write objects.
pub struct Classifier {
    root: DefinedConceptRoot,
    expression: Expression,
    paths: BTreeSet<Vec<String>>,
    definitions: Vec<ArtifactRef>,
}

impl Classifier {
    /// Admit a separate defined-concept artifact against its retained base.
    ///
    /// # Errors
    /// Refuses invalid declarations, unresolved pins, cycles or unsupported reads.
    pub fn prepare(
        artifact: &ArtifactEnvelope,
        context: &EntityArtifactSetContext<'_>,
    ) -> Result<Self, IncompleteClassification> {
        if artifact.kind() != norn_ir::ArtifactKind::DefinedConcept {
            return Err(IncompleteClassification);
        }
        if crate::check_artifact_set(std::slice::from_ref(artifact), context)
            .iter()
            .any(|finding| finding.severity == norn_ir::DiagnosticSeverity::Error)
        {
            return Err(IncompleteClassification);
        }
        let root = crate::defined_concepts::decode(&artifact.payload)
            .map_err(|_error| IncompleteClassification)?;
        let pool: Vec<_> = std::iter::once(artifact).chain(context.base).collect();
        let expression = crate::defined_concepts::qualify(artifact, &root, &pool, context.registry)
            .map_err(|_error| IncompleteClassification)?;
        let paths = norn_expr::expression_read_paths(root.predicate.as_str(), "self")
            .map_err(|_error| IncompleteClassification)?;
        if paths.contains(&Vec::new()) {
            return Err(IncompleteClassification);
        }
        let definitions = pool
            .into_iter()
            .map(ArtifactRef::of)
            .collect::<Result<_, _>>()
            .map_err(|_error| IncompleteClassification)?;
        Ok(Self {
            root,
            expression,
            paths,
            definitions,
        })
    }

    /// The admitted declaration, including its existing resource-limit profile.
    #[must_use]
    pub const fn declaration(&self) -> &DefinedConceptRoot {
        &self.root
    }

    /// Conservative definition dependencies, including the predicate itself.
    #[must_use]
    pub fn definitions(&self) -> &[ArtifactRef] {
        &self.definitions
    }

    /// Evaluate one complete, authorized snapshot of schema-checked base rows.
    /// Keys are the original base object IDs; no copied objects are produced.
    /// The host must bind these rows to the pinned base at one revision and
    /// capture their read dependencies before calling this pure evaluator.
    ///
    /// # Errors
    /// Partial snapshots, absent inputs, duplicate IDs and exhausted aggregate
    /// record/byte/fuel bounds refuse the entire classification, including empty ones.
    pub fn classify(
        &self,
        rows: &[(String, RuntimeValue)],
        complete: bool,
    ) -> Result<BTreeSet<String>, IncompleteClassification> {
        if !complete
            || rows.len()
                > usize::try_from(self.root.bounds.slice.max_records)
                    .map_err(|_error| IncompleteClassification)?
        {
            return Err(IncompleteClassification);
        }
        let mut seen = BTreeSet::new();
        let mut members = BTreeSet::new();
        let encoded =
            norn_canonical::canonical_json_of(rows).map_err(|_error| IncompleteClassification)?;
        if u64::try_from(encoded.len()).map_err(|_error| IncompleteClassification)?
            > self.root.bounds.slice.max_canonical_bytes
        {
            return Err(IncompleteClassification);
        }
        let mut fuel = self.root.bounds.decide.fuel_units;
        for (id, row) in rows {
            let RuntimeValue::Object(_) = row else {
                return Err(IncompleteClassification);
            };
            if !seen.insert(id) || self.paths.iter().any(|path| !present(row, path)) {
                return Err(IncompleteClassification);
            }
            let value = Value::try_from(row).map_err(|_error| IncompleteClassification)?;
            let attempt = self.expression.evaluate_metered(
                &Value::object([("self", value)]),
                EvaluationBudget::new(fuel),
            );
            fuel = fuel
                .checked_sub(attempt.units_used)
                .ok_or(IncompleteClassification)?;
            match attempt
                .result
                .map_err(|_error| IncompleteClassification)?
                .into_value()
            {
                Value::Bool(true) => {
                    members.insert(id.clone());
                }
                Value::Bool(false) => {}
                Value::Null
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_)
                | Value::Object(_) => return Err(IncompleteClassification),
            }
        }
        Ok(members)
    }
}

fn present(mut value: &RuntimeValue, path: &[String]) -> bool {
    for member in path {
        let RuntimeValue::Object(fields) = value else {
            return false;
        };
        let Some(next) = fields.get(member) else {
            return false;
        };
        value = next;
    }
    true
}
