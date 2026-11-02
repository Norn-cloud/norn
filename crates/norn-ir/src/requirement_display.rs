//! Deterministic views of one structured requirement; display is never proof input.
use crate::deontic::{DeonticModality, RequirementText, RequirementTiming, StructuredRequirement};
use crate::{LawModality, LawResponse, LawRoot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementLocale {
    En,
    Ar,
}

/// A read-only editor projection carries the original IR, not a parsed sentence.
/// UI consumers must honor `direction()` and render text as text, never HTML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementView {
    pub requirement: StructuredRequirement,
    pub locale: RequirementLocale,
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
    #[must_use]
    pub const fn direction(&self) -> &'static str {
        self.word("ltr", "rtl")
    }

    const fn word<'a>(&self, en: &'a str, ar: &'a str) -> &'a str {
        match self.locale {
            RequirementLocale::En => en,
            RequirementLocale::Ar => ar,
        }
    }

    fn template(&self, text: &RequirementText) -> String {
        visible(self.word(&text.en, &text.ar))
    }

    fn slot(&self, pointer: &str, en: &str, ar: &str, text: String) -> RequirementSlot {
        RequirementSlot {
            id: format!("{}#{pointer}", self.requirement.norm.law.id),
            label: self.word(en, ar).into(),
            text,
        }
    }

    /// Both languages expose identical semantic slots in identical order.
    /// LTR identifiers/expressions use Unicode isolates inside RTL sentences.
    ///
    /// # Errors
    /// Refuses unsupported timing, contradictory grades and duplicate exception IDs.
    pub fn slots(&self) -> Result<Vec<RequirementSlot>, &'static str> {
        let r = &self.requirement;
        let law = &r.norm.law;
        if law.timing.is_some()
            || !matches!(law.modality, LawModality::Always {} | LawModality::Never {})
        {
            return Err("law.validation.unsupported_temporal");
        }
        if matches!(r.norm.modality, DeonticModality::Prohibition)
            != matches!(law.modality, LawModality::Never {})
        {
            return Err("law.validation.invalid_document");
        }
        let w = &r.wording;
        let root = match &law.scope.root {
            LawRoot::Company(id) => format!("{} {}", self.word("company", "شركة"), isolate(id)),
            LawRoot::App(id) => format!("{} {}", self.word("application", "تطبيق"), isolate(id)),
            LawRoot::Cell(id) => format!("{} {}", self.word("cell", "خلية"), isolate(id)),
            LawRoot::Entity(id) => format!("{} {}", self.word("entity", "كيان"), isolate(id)),
        };
        let mut scope = vec![format!(
            "{}; {root}; {}",
            self.template(&w.scope),
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
                self.word("commitments", "الالتزامات"),
                isolate(&law.scope.commitments.join(", "))
            ));
        }
        let activation = format!(
            "{}; {}",
            self.template(&w.activation),
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
            DeonticModality::Obligation => self.word("must", "يجب"),
            DeonticModality::Permission => {
                self.word("may (no authorization grant)", "يجوز (لا يمنح صلاحية)")
            }
            DeonticModality::Prohibition => self.word("must not", "يُحظر"),
        };
        let timing = match r.timing {
            RequirementTiming::SameCommit => self.word("same commit", "التثبيت نفسه"),
            RequirementTiming::EveryCommittedState => self.word("every state", "كل حالة مثبتة"),
        };
        let source = format!(
            "{}@{}; {}#{}",
            r.norm.authority_id,
            r.norm.authority_version,
            r.norm.source.artifact_id,
            r.norm.source.pointer
        );
        let source = isolate(&source);
        let component = format!("{}; {}", self.template(&w.component), isolate(&law.subject));
        let response = format!("{}; {}", self.template(&w.response), isolate(&response));
        let preferred = format!(
            "{}; {}",
            self.word("requires grounding", "يتطلب التأصيل"),
            isolate(&r.norm.defeated_by.join(", "))
        );
        let mut slots = vec![
            self.slot("/norm/law/scope", "Scope", "النطاق", scope.join("")),
            self.slot("/norm/law/condition", "Condition", "الشرط", activation),
            self.slot("/norm/law/subject", "Component", "المكوّن", component),
            self.slot("/norm/modality", "Grade", "الحكم", grade.into()),
            self.slot("/timing", "Timing", "التوقيت", timing.into()),
            self.slot("/norm/law/response", "Response", "الاستجابة", response),
            self.slot("/norm/source", "Unverified source", "مصدر بلا تحقق", source),
            self.slot("/norm/defeatedBy", "Preferences", "تفضيلات", preferred),
        ];
        let mut exceptions = std::collections::BTreeSet::new();
        for e in &r.norm.exceptions {
            if !exceptions.insert(&e.id) {
                return Err("law.validation.invalid_document");
            }
            slots.push(self.slot(
                &format!(
                    "/norm/exceptions/by-id/{}",
                    e.id.replace('~', "~0").replace('/', "~1")
                ),
                "Exception",
                "استثناء",
                isolate(&format!(
                    "{}; {}#{}",
                    e.condition.0, e.source.artifact_id, e.source.pointer
                )),
            ));
        }
        Ok(slots)
    }
}
