//! Deterministic views of one structured requirement; display is never proof input.
use crate::deontic::{
    DeonticModality, LocalizedRequirement, RequirementTiming, StructuredRequirement,
    TimingShapeError,
};
use crate::{LawResponse, LawRoot};
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

fn visible(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
        {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}
fn isolate(text: &str) -> String {
    format!("\u{2068}{}\u{2069}", visible(text))
}

impl RequirementView {
    /// # Errors
    /// Refuses invalid wording or an unknown requested locale.
    pub fn direction(&self) -> Result<&'static str, &'static str> {
        Ok(self.wording()?.direction.as_str())
    }

    // LAW-WORDING-LOCALE-1 applies to every declared locale before either view.
    fn wording(&self) -> Result<&LocalizedRequirement, &'static str> {
        let wording = self.requirement.wording.locale(&self.locale)?;
        self.requirement.validate_wording()?;
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
        let within = r.within().map_err(TimingShapeError::code)?;
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
                let (within, millis) = within.ok_or("law.validation.unsupported_temporal")?;
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
        let mut exceptions = std::collections::BTreeSet::new();
        for e in &r.norm.exceptions {
            if !exceptions.insert(&e.id) {
                return Err("law.validation.invalid_document");
            }
            slots.push(slot(
                &law.id,
                &format!(
                    "/norm/exceptions/by-id/{}",
                    e.id.replace('~', "~0").replace('/', "~1")
                ),
                &w.labels.exception,
                isolate(&format!(
                    "{}; {}#{}",
                    e.condition.0, e.source.artifact_id, e.source.pointer
                )),
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
