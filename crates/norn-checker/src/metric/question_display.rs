//! Read-only localized views of an already admitted question; never execution.
use super::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use crate::EffectiveKind;
pub use norn_ir::reviewed_locale::{QuestionLabels, QuestionWording};
use norn_ir::{PropertyType, QuestionIR, deontic::TextDirection, reviewed_locale};

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

fn canonical(
    bytes: Result<Vec<u8>, norn_canonical::CanonicalJsonError>,
) -> Result<String, &'static str> {
    String::from_utf8(bytes.map_err(|_error| UNSUPPORTED_ENGINE)?)
        .map_err(|_error| UNSUPPORTED_ENGINE)
}
fn digest(
    bytes: Result<Vec<u8>, norn_canonical::CanonicalJsonError>,
) -> Result<String, &'static str> {
    let bytes = bytes.map_err(|_error| UNSUPPORTED_ENGINE)?;
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
    let labels = wording.select(locale)?;
    let question = plan.question();
    let contracts: Vec<_> = plan
        .sums()
        .iter()
        .map(|sum| match &sum.property.kind {
            EffectiveKind::Base(kind) => Ok((&sum.property.id, kind)),
            EffectiveKind::Registry(_) | EffectiveKind::Unresolved => Err(UNSUPPORTED_ENGINE),
        })
        .collect::<Result<_, _>>()?;
    let question_digest = digest(norn_canonical::canonical_json_of(question))?;
    let binding_digest = digest(norn_canonical::canonical_json_of(&(
        question,
        plan.definitions(),
        &contracts,
    )))?;
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
            | PropertyType::Ref { .. }
            | PropertyType::DecimalBinding { .. } => "1".to_owned(),
        };
        operands.push(format!(
            "Σ({}) [{units}]; {}",
            sum.property.id.as_str(),
            canonical(norn_canonical::canonical_json_of(&sum.filters))?
        ));
    }
    let entries = [
        (
            "/objectSet",
            &labels.population,
            canonical(norn_canonical::canonical_json_of(&(
                &question.object_set,
                &question.search_around,
            )))?,
        ),
        ("/aggregate", &labels.measure, operands.join(" / ")),
        (
            "/groupBy",
            &labels.by,
            canonical(norn_canonical::canonical_json_of(&question.dimensions))?,
        ),
        (
            "/filter",
            &labels.filters,
            canonical(norn_canonical::canonical_json_of(&question.filters))?,
        ),
        (
            "/window",
            &labels.effective_time,
            canonical(norn_canonical::canonical_json_of(&(
                plan.sums()
                    .iter()
                    .map(|sum| &sum.time_property)
                    .collect::<Vec<_>>(),
                &question.time_window,
                &question.calendar,
            )))?,
        ),
        (
            "/freshness",
            &labels.knowledge_time,
            canonical(norn_canonical::canonical_json_of(&question.freshness))?,
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
