//! Read-only localized views of an already admitted question; never execution.
use super::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use crate::EffectiveKind;
use norn_ir::{PropertyType, QuestionIR, deontic::TextDirection, reviewed_locale};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Complete reviewed labels; semantic expressions and identities are untranslated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionLabels {
    /// Direction of the surrounding text; identifiers are isolated separately.
    pub direction: TextDirection,
    /// Label for the declared root population and traversals.
    pub population: String,
    /// Label for the selected sum or ratio of sums.
    pub measure: String,
    /// Label for grouping dimensions and calendar grains.
    pub by: String,
    /// Label for additional question predicates.
    pub filters: String,
    /// Label for effective-time properties, window and calendar.
    pub effective_time: String,
    /// Label for the declared knowledge/freshness policy.
    pub knowledge_time: String,
}
impl QuestionLabels {
    fn validate(&self) -> Result<(), &'static str> {
        if [
            &self.population,
            &self.measure,
            &self.by,
            &self.filters,
            &self.effective_time,
            &self.knowledge_time,
        ]
        .iter()
        .any(|label| label.trim().is_empty())
        {
            Err("law.wording.incomplete_locale")
        } else {
            Ok(())
        }
    }
}

/// Same ordered, complete, exact-match locale-map mechanism as law wording (#496).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct QuestionWording(pub BTreeMap<String, QuestionLabels>);
impl<'de> Deserialize<'de> for QuestionWording {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        reviewed_locale::deserialize(deserializer, QuestionLabels::validate).map(Self)
    }
}

/// Original semantic question plus a localized read-only projection.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionRendering {
    /// Unchanged semantic declaration for lossless inspection.
    pub question: QuestionIR,
    /// Norn1 identity of the unchanged `QuestionIR`, independent of locale.
    pub question_digest: String,
    /// Binds displayed definitions and numeric contracts, including units.
    pub binding_digest: String,
    /// Direction of surrounding localized labels.
    pub direction: TextDirection,
    /// Ordered semantic slots with reviewed labels.
    pub slots: Vec<QuestionSlot>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
/// One stable semantic slot in a localized question view.
pub struct QuestionSlot {
    /// Question digest plus a stable semantic pointer, identical in every locale.
    pub id: String,
    /// Reviewed localized label with direction controls escaped.
    pub label: String,
    /// Locale-independent semantic text inside a Unicode isolate.
    pub text: String,
}

fn canonical(value: &impl Serialize) -> Result<String, &'static str> {
    String::from_utf8(
        norn_canonical::canonical_json_of(value).map_err(|_error| UNSUPPORTED_ENGINE)?,
    )
    .map_err(|_error| UNSUPPORTED_ENGINE)
}
fn digest(value: &impl Serialize) -> Result<String, &'static str> {
    let bytes = norn_canonical::canonical_json_of(value).map_err(|_error| UNSUPPORTED_ENGINE)?;
    Ok(norn_canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes))
}

/// QUESTION-LOCALE-1: EN/AR and other reviewed locales render the same admitted
/// question, slot IDs, quantity units, denominator and temporal declarations.
/// Runtime authorization and authenticated cutoffs remain the host's authority.
///
/// # Errors
/// Uses existing wording refusals for incomplete/unknown locales, and existing
/// compute refusal for an unrenderable contract. There is no guessed plan.
pub fn render_question(
    plan: &QuestionPlan,
    wording: &QuestionWording,
    locale: &str,
) -> Result<QuestionRendering, &'static str> {
    let labels = reviewed_locale::select(&wording.0, locale, QuestionLabels::validate)?;
    let question = plan.question();
    let contracts: Vec<_> = plan
        .sums()
        .iter()
        .map(|sum| match &sum.property.kind {
            EffectiveKind::Base(kind) => Ok((&sum.property.id, kind)),
            EffectiveKind::Registry(_) | EffectiveKind::Unresolved => Err(UNSUPPORTED_ENGINE),
        })
        .collect::<Result<_, _>>()?;
    let question_digest = digest(question)?;
    let binding_digest = digest(&(question, plan.definitions(), &contracts))?;
    let mut operands = Vec::new();
    for (sum, (_, kind)) in plan.sums().iter().zip(&contracts) {
        let units = match kind {
            PropertyType::Decimal { unit, per_unit, .. } => {
                let top = unit.as_ref().map_or("1", |unit| unit.code.as_str());
                per_unit
                    .as_ref()
                    .map_or_else(|| top.to_owned(), |bottom| format!("{top}/{}", bottom.code))
            }
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Boolean { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date { .. }
            | PropertyType::Duration { .. }
            | PropertyType::Enum { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::Ref { .. } => "1".to_owned(),
        };
        operands.push(format!(
            "Σ({}) [{units}]; {}",
            sum.property.id.as_str(),
            canonical(&sum.filters)?
        ));
    }
    let entries = [
        (
            "/objectSet",
            &labels.population,
            canonical(&(&question.object_set, &question.search_around))?,
        ),
        ("/aggregate", &labels.measure, operands.join(" / ")),
        ("/groupBy", &labels.by, canonical(&question.dimensions)?),
        ("/filter", &labels.filters, canonical(&question.filters)?),
        (
            "/window",
            &labels.effective_time,
            canonical(&(
                plan.sums()
                    .iter()
                    .map(|sum| &sum.time_property)
                    .collect::<Vec<_>>(),
                &question.time_window,
                &question.calendar,
            ))?,
        ),
        (
            "/freshness",
            &labels.knowledge_time,
            canonical(&question.freshness)?,
        ),
    ];
    let slots = entries
        .into_iter()
        .map(|(pointer, label, text)| QuestionSlot {
            id: format!("{question_digest}#{pointer}"),
            label: reviewed_locale::visible(label),
            text: reviewed_locale::isolate(&text),
        })
        .collect();
    Ok(QuestionRendering {
        question: question.clone(),
        question_digest,
        binding_digest,
        direction: labels.direction,
        slots,
    })
}
