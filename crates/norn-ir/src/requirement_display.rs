//! Deterministic views of one structured requirement; display is never proof input.
use crate::deontic::{
    DeonticModality, LocalizedRequirement, RequirementTiming, StructuredRequirement,
};
use crate::reviewed_locale::{isolate, visible};
use crate::{LawModality, LawResponse, LawRoot, LawTiming};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A read-only editor projection carries the original IR, not a parsed sentence.
/// UI consumers must honor `direction()` and render text as text, never HTML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementView {
    pub requirement: StructuredRequirement,
    pub locale: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementSlot {
    /// Law identity plus a stable semantic slot path, independent of locale.
    pub id: String,
    pub label: String,
    pub text: String,
}

impl RequirementView {
    /// # Errors
    /// Refuses invalid wording or an unknown requested locale.
    pub fn direction(&self) -> Result<&'static str, &'static str> {
        Ok(self.wording()?.direction.as_str())
    }

    // LAW-WORDING-LOCALE-1 applies to every declared locale before either view.
    // Admission-level checks (`within` shape, every locale's `within` term)
    // live in norn-checker's `render_requirement`; this projection only
    // refuses what it cannot render.
    fn wording(&self) -> Result<&LocalizedRequirement, &'static str> {
        let wording = self.requirement.wording.locale(&self.locale)?;
        if matches!(self.requirement.norm.law.scope.root, LawRoot::Pack(_))
            && self
                .requirement
                .wording
                .0
                .values()
                .any(|entry| entry.terms.pack.is_none())
        {
            return Err("law.wording.incomplete_locale");
        }
        Ok(wording)
    }

    /// All locales expose identical semantic slots in identical order.
    /// LTR identifiers/expressions use Unicode isolates inside RTL sentences.
    ///
    /// # Errors
    /// Refuses unsupported timing, contradictory grades and duplicate exception IDs.
    pub fn slots(&self) -> Result<Vec<RequirementSlot>, &'static str> {
        let r = &self.requirement;
        let law = &r.norm.law;
        // Render preconditions only: the temporal wrapper must have a template.
        let window = match (&r.timing, &law.modality, &law.timing) {
            (
                RequirementTiming::Within(w),
                LawModality::Eventually {},
                Some(LawTiming::Within { millis }),
            ) => Some((w, millis)),
            (RequirementTiming::Within(_), _, _) => {
                return Err("law.validation.unsupported_temporal");
            }
            (_, LawModality::Always {} | LawModality::Never {}, None) => None,
            _ => return Err("law.validation.unsupported_temporal"),
        };
        let grade_matches = match window {
            Some(_) => r.norm.modality == DeonticModality::Obligation,
            None => {
                matches!(r.norm.modality, DeonticModality::Prohibition)
                    == matches!(law.modality, LawModality::Never {})
            }
        };
        if !grade_matches {
            return Err("law.validation.invalid_document");
        }
        let w = self.wording()?;
        let root = match &law.scope.root {
            LawRoot::Company(id) => format!("{} {}", visible(&w.terms.company), isolate(id)),
            LawRoot::App(id) => format!("{} {}", visible(&w.terms.application), isolate(id)),
            LawRoot::Pack(id) => {
                let pack = w
                    .terms
                    .pack
                    .as_deref()
                    .ok_or("law.wording.incomplete_locale")?;
                format!("{} {}", visible(pack), isolate(id))
            }
            LawRoot::Cell(id) => format!("{} {}", visible(&w.terms.cell), isolate(id)),
            LawRoot::Entity(id) => format!("{} {}", visible(&w.terms.entity), isolate(id)),
        };
        let mut scope = vec![format!(
            "{}; {root}; {}",
            visible(&w.scope),
            isolate(&law.scope.object_types.join(", "))
        )];
        for state in &law.scope.lifecycle_states {
            scope.push(format!(
                "; {} ∈ {}",
                isolate(&format!("{}.{}", state.object_type, state.property)),
                isolate(&state.states.join(", "))
            ));
        }
        if !law.scope.commitments.is_empty() {
            scope.push(format!(
                "; {} {}",
                visible(&w.terms.commitments),
                isolate(&law.scope.commitments.join(", "))
            ));
        }
        let activation = format!(
            "{}; {}",
            visible(&w.activation),
            isolate(law.condition.as_ref().map_or("true", |c| c.0.as_str()))
        );
        let response = match &law.response {
            LawResponse::Predicate { expression } => expression.0.clone(),
            LawResponse::Invariant {
                object_type,
                invariant_id,
            } => format!("{object_type}.{invariant_id}"),
            LawResponse::Action { .. } | LawResponse::Transition { .. } => {
                return Err("law.validation.invalid_document");
            }
        };
        let grade = match r.norm.modality {
            DeonticModality::Obligation => visible(&w.terms.obligation),
            DeonticModality::Permission => visible(&w.terms.permission),
            DeonticModality::Prohibition => visible(&w.terms.prohibition),
        };
        let timing = match &r.timing {
            RequirementTiming::SameCommit => visible(&w.terms.same_commit),
            RequirementTiming::EveryCommittedState => visible(&w.terms.every_committed_state),
            RequirementTiming::Within(_) => {
                let (within, millis) = window.ok_or("law.validation.unsupported_temporal")?;
                let term = w
                    .terms
                    .within
                    .as_ref()
                    .ok_or("law.wording.incomplete_locale")?;
                let window = format!("{millis} ms; {}", within.trigger);
                format!("{}; {}", visible(term), isolate(&window))
            }
        };
        let source = format!(
            "{}@{}; {}#{}",
            r.norm.authority_id,
            r.norm.authority_version,
            r.norm.source.artifact_id,
            r.norm.source.pointer
        );
        let source = isolate(&source);
        let component = format!("{}; {}", visible(&w.component), isolate(&law.subject));
        let response = format!("{}; {}", visible(&w.response), isolate(&response));
        let preferred = format!(
            "{}; {}",
            visible(&w.terms.grounding),
            isolate(&r.norm.defeated_by.join(", "))
        );
        let mut slots = vec![
            slot(&law.id, "/norm/law/scope", &w.labels.scope, scope.join("")),
            slot(
                &law.id,
                "/norm/law/condition",
                &w.labels.condition,
                activation,
            ),
            slot(&law.id, "/norm/law/subject", &w.labels.component, component),
            slot(&law.id, "/norm/modality", &w.labels.grade, grade),
            slot(&law.id, "/timing", &w.labels.timing, timing),
            slot(&law.id, "/norm/law/response", &w.labels.response, response),
            slot(&law.id, "/norm/source", &w.labels.source, source),
            slot(
                &law.id,
                "/norm/defeatedBy",
                &w.labels.preferences,
                preferred,
            ),
        ];
        // One home for exceptions: the rule's list or the law's, never both.
        let (home, declared) = match (r.norm.exceptions.is_empty(), law.exceptions.is_empty()) {
            (false, false) => return Err("law.validation.invalid_document"),
            (true, false) => ("/norm/law/exceptions", &law.exceptions),
            (_, true) => ("/norm/exceptions", &r.norm.exceptions),
        };
        let mut exceptions = std::collections::BTreeSet::new();
        for e in declared {
            if !exceptions.insert(&e.id) {
                return Err("law.validation.invalid_document");
            }
            slots.push(slot(
                &law.id,
                &format!(
                    "{home}/by-id/{}",
                    e.id.replace('~', "~0").replace('/', "~1")
                ),
                &w.labels.exception,
                // A nested exception names the exception it lifts; a
                // top-level one lifts the law and renders as before.
                isolate(&if e.overrides == law.id {
                    format!(
                        "{}; {}#{}",
                        e.condition.0, e.source.artifact_id, e.source.pointer
                    )
                } else {
                    format!(
                        "{}; overrides {}; {}#{}",
                        e.condition.0, e.overrides, e.source.artifact_id, e.source.pointer
                    )
                }),
            ));
        }
        Ok(slots)
    }
}

fn slot(law_id: &str, pointer: &str, label: &str, text: String) -> RequirementSlot {
    RequirementSlot {
        id: format!("{law_id}#{pointer}"),
        label: visible(label),
        text,
    }
}

/// Public requested-locale inspection contract.
#[must_use]
pub fn requirement_view_schema() -> schemars::Schema {
    schemars::schema_for!(RequirementView)
}
