//! Localized labels: presentation metadata kept outside every identity digest
//! (LABEL-LOCALE-1, #679, norn-platform#3438 §1).
//!
//! A `labels` artifact maps canonical definition ids (an object type, a
//! property, an enum value, an action, a rule, a law or a page) to
//! human-readable labels and descriptions per BCP 47 locale tag. Labels live in
//! their own artifact kind rather than in the definitions they describe, so
//! they cannot reach an object definition, a law, a rule, the cell model hash
//! or any digest computed from those: localizing never changes an artifact
//! hash, a model hash or evidence identity. A label catalog is itself a
//! governed artifact with its own hash and history.
//!
//! Catalogs layer: a downstream pack (a tenant or locale pack) may add or
//! replace labels for upstream definitions without touching them. For one
//! target and locale, a candidate catalog's label replaces the installed
//! base's; two candidate catalogs that both label the same target and locale
//! are refused (`norn-checker`), never resolved by order.
use crate::widget_ux::LocaleText;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The canonical definition a label describes, by its language-neutral id.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LabelTarget {
    /// An object type or interface, by qualified name (`sales.Invoice`).
    ObjectType { object_type: String },
    /// A property of an object type or interface.
    Property {
        object_type: String,
        property: String,
    },
    /// One declared value of an `enum` property (inline or through `ref`).
    EnumValue {
        object_type: String,
        property: String,
        value: String,
    },
    /// An action, by `actionId`.
    Action { action_id: String },
    /// A rule, by `ruleId`.
    Rule { rule_id: String },
    /// A law, by `id`.
    Law { law_id: String },
    /// A page of a widget UX contract.
    Page { widget_id: String, page_id: String },
}

impl LabelTarget {
    /// A stable lookup key for the target, e.g. `property:sales.Invoice.total`
    /// or `enumValue:sales.Invoice.status=draft`. Property ids contain no `.`
    /// and the enum value comes last, so the key splits back unambiguously.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::ObjectType { object_type } => format!("objectType:{object_type}"),
            Self::Property {
                object_type,
                property,
            } => format!("property:{object_type}.{property}"),
            Self::EnumValue {
                object_type,
                property,
                value,
            } => format!("enumValue:{object_type}.{property}={value}"),
            Self::Action { action_id } => format!("action:{action_id}"),
            Self::Rule { rule_id } => format!("rule:{rule_id}"),
            Self::Law { law_id } => format!("law:{law_id}"),
            Self::Page { widget_id, page_id } => format!("page:{widget_id}/{page_id}"),
        }
    }
}

/// Review state of one locale's label and description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LabelReview {
    /// Proposed (for example machine-translated) and not yet reviewed by a
    /// person. Rendered, but listed for review.
    NeedsReview,
    /// Reviewed by a person. The default when a locale has no review entry.
    Reviewed,
}

/// Labels for one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabelEntry {
    /// The labelled definition.
    pub target: LabelTarget,
    /// Short human-readable name per BCP 47 tag; at least one, none blank.
    pub label: LocaleText,
    /// Longer human-readable description per BCP 47 tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<LocaleText>,
    /// Review state per BCP 47 tag; a tag that is not listed is reviewed.
    /// Every listed tag must have a label.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub review: BTreeMap<String, LabelReview>,
}

/// The strict payload of a `labels` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabelCatalogRoot {
    /// The form selector; always `0.1`.
    pub labels_version: crate::IrVersion01,
    /// One entry per target, each target at most once.
    pub entries: Vec<LabelEntry>,
    /// Reserved versioned extension slots, as on other artifact roots.
    #[serde(
        default,
        deserialize_with = "crate::members::present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "crate::JsonObject")]
    pub extensions: Option<crate::JsonObject>,
}

/// Whether `tag` is a well-formed BCP 47 language tag (RFC 5646 syntax, not
/// registry membership). The same rule the law and question wording use.
#[must_use]
pub fn is_valid_locale_tag(tag: &str) -> bool {
    crate::deontic::valid_locale_tag(tag)
}

/// One locale's resolved label for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLabel {
    /// The label text.
    pub label: String,
    /// The description, when the winning layer gives one for this locale.
    pub description: Option<String>,
    /// The winning layer's review state for this locale.
    pub review: LabelReview,
}

/// Resolved labels: target → locale tag → label.
pub type ResolvedLabels = BTreeMap<LabelTarget, BTreeMap<String, ResolvedLabel>>;

/// Resolve layered catalogs, lowest precedence first (the installed base,
/// then each downstream layer). For one target and locale (compared ignoring
/// case), the last layer that labels it supplies the label, description and
/// review state together; a locale no later layer labels keeps its earlier
/// label, so layering never drops a locale. Within one layer, admission
/// refuses a target that is labelled twice in one locale.
#[must_use]
pub fn resolve<'a>(layers: impl IntoIterator<Item = &'a LabelCatalogRoot>) -> ResolvedLabels {
    let mut resolved = ResolvedLabels::new();
    for layer in layers {
        for entry in &layer.entries {
            let locales = resolved.entry(entry.target.clone()).or_default();
            for (tag, label) in &entry.label.0 {
                locales.retain(|existing, _| !existing.eq_ignore_ascii_case(tag));
                let description = entry.description.as_ref().and_then(|text| {
                    text.0
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(tag))
                        .map(|(_, value)| value.clone())
                });
                let review = entry
                    .review
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(tag))
                    .map_or(LabelReview::Reviewed, |(_, review)| *review);
                locales.insert(
                    tag.clone(),
                    ResolvedLabel {
                        label: label.clone(),
                        description,
                        review,
                    },
                );
            }
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn target() -> impl Strategy<Value = LabelTarget> {
        prop_oneof![
            "[a-z]{1,3}\\.[A-Z][a-z]{0,3}"
                .prop_map(|object_type| LabelTarget::ObjectType { object_type }),
            ("[a-z]{1,3}\\.[A-Z][a-z]{0,3}", "[a-z]{1,4}").prop_map(|(object_type, property)| {
                LabelTarget::Property {
                    object_type,
                    property,
                }
            }),
            "[a-z.]{1,6}".prop_map(|action_id| LabelTarget::Action { action_id }),
        ]
    }

    fn layer() -> impl Strategy<Value = LabelCatalogRoot> {
        let locale = prop::sample::select(vec!["en", "EN", "ar", "ar-EG", "AR-eg", "fr"]);
        let entry = (
            target(),
            prop::collection::btree_map(
                locale.clone().prop_map(String::from),
                "[a-zء-ي]{1,6}",
                1..4,
            ),
            prop::collection::btree_map(
                locale.prop_map(String::from),
                Just(LabelReview::NeedsReview),
                0..2,
            ),
        )
            .prop_map(|(target, label, review)| LabelEntry {
                target,
                label: LocaleText(label),
                description: None,
                review,
            });
        prop::collection::vec(entry, 0..6).prop_map(|entries| LabelCatalogRoot {
            labels_version: crate::IrVersion01::V0_1,
            entries,
            extensions: None,
        })
    }

    proptest! {
        /// LABEL-LOCALE-1: the last layer that labels a target in a locale wins,
        /// and every labelled (target, locale) survives layering.
        #[test]
        fn later_layers_win_per_locale_and_no_locale_is_lost(layers in prop::collection::vec(layer(), 0..4)) {
            let resolved = resolve(&layers);
            for (index, layer) in layers.iter().enumerate() {
                for entry in &layer.entries {
                    for tag in entry.label.0.keys() {
                        let locales = resolved.get(&entry.target);
                        prop_assert!(locales.is_some_and(|l| l.keys().any(|k| k.eq_ignore_ascii_case(tag))));
                        let overridden = layers.iter().skip(index).skip(1).any(|later| later.entries.iter().any(|e| {
                            e.target == entry.target && e.label.0.keys().any(|k| k.eq_ignore_ascii_case(tag))
                        }));
                        let same_layer_later = layer.entries.iter().skip_while(|e| !std::ptr::eq(*e, entry)).skip(1).any(|e| {
                            e.target == entry.target && e.label.0.keys().any(|k| k.eq_ignore_ascii_case(tag))
                        });
                        if !overridden && !same_layer_later {
                            let (_, won) = locales.into_iter().flatten().find(|(k, _)| k.eq_ignore_ascii_case(tag)).expect("present");
                            let expected = entry.label.0.iter().rfind(|(k, _)| k.eq_ignore_ascii_case(tag)).expect("labelled").1;
                            prop_assert_eq!(&won.label, expected);
                        }
                    }
                }
            }
            for locales in resolved.values() {
                let mut seen = std::collections::BTreeSet::new();
                prop_assert!(locales.keys().all(|k| seen.insert(k.to_ascii_lowercase())));
            }
        }
    }
}
