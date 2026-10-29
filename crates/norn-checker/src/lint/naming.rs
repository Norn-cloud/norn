//! Naming conventions (T8-LINT-1): the grammar of names is a T2 error of the
//! entity model; these are the style rules above it, all warnings.
//!
//! - [`codes::PLURAL_TYPE_NAME`]: an object type or interface names one
//!   instance, so its local name is singular. Deterministic test: the local
//!   name ends in `s` and the letter before it is not `s`, `u`, or `i`
//!   (`Orders`, `Categories` are flagged; `Address`, `Status`, `Analysis`
//!   are not).
//! - [`codes::REDUNDANT_MEMBER_PREFIX`]: a property or link id starts with
//!   its own type's snake-cased local name and `_` (`customer_name` on
//!   `crm.Customer`); the type already qualifies the member.
//! - [`codes::VERSIONED_NAME`]: a name carries a version or copy marker. A
//!   type or interface local name ends in `V<digits>`, `Legacy`, `Copy`,
//!   `Old`, `Tmp`, or `Temp` after a lower-case letter or digit
//!   (`CustomerV2`, `OrderLegacy`); a member id's last `_` segment is
//!   `v<digits>`, `legacy`, `copy`, `old`, `tmp`, or `temp` (`price_v2`).
//!   Versions belong to the artifact, not to the concept's name.

use norn_ir::{InterfaceDef, ObjectTypeDef};

use super::finding;
use super::model::{LintSet, Owned};
use crate::Diagnostic;

/// Diagnostic codes of the naming rules.
pub mod codes {
    /// An object type or interface local name looks plural.
    pub const PLURAL_TYPE_NAME: &str = "ontology.lint.plural_type_name";
    /// A member id repeats its type's name as a prefix.
    pub const REDUNDANT_MEMBER_PREFIX: &str = "ontology.lint.redundant_member_prefix";
    /// A type or member name carries a version or copy marker.
    pub const VERSIONED_NAME: &str = "ontology.lint.versioned_name";
}

/// Local-name suffixes that mark a copy or version of a type.
const TYPE_COPY_MARKERS: [&str; 5] = ["Legacy", "Copy", "Old", "Tmp", "Temp"];
/// Last id segments that mark a copy or version of a member.
const MEMBER_COPY_MARKERS: [&str; 5] = ["legacy", "copy", "old", "tmp", "temp"];

/// The naming rules over every candidate object type and interface.
#[must_use]
pub(crate) fn lint_naming(set: &LintSet<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for owned in set.candidate_object_types() {
        lint_object_type(owned, &mut out);
    }
    for owned in set.interfaces.iter().filter(|owned| owned.is_candidate()) {
        lint_interface(owned, &mut out);
    }
    out
}

fn lint_object_type(owned: &Owned<'_, ObjectTypeDef>, out: &mut Vec<Diagnostic>) {
    let def = &owned.def;
    type_name_findings(
        "object type",
        def.name.as_str(),
        def.name.local_name(),
        owned,
        out,
    );
    let members = def
        .properties
        .iter()
        .enumerate()
        .map(|(position, property)| ("properties", position, property.id.as_str()))
        .chain(
            def.links
                .iter()
                .enumerate()
                .map(|(position, link)| ("links", position, link.id.as_str())),
        );
    member_findings(
        def.name.as_str(),
        def.name.local_name(),
        members,
        owned,
        out,
    );
}

fn lint_interface(owned: &Owned<'_, InterfaceDef>, out: &mut Vec<Diagnostic>) {
    let def = &owned.def;
    let name = def.name.as_str();
    let local = name.rsplit_once('.').map_or(name, |(_, local)| local);
    type_name_findings("interface", name, local, owned, out);
    let members = def
        .properties
        .iter()
        .enumerate()
        .map(|(position, property)| ("properties", position, property.id.as_str()))
        .chain(
            def.links
                .iter()
                .enumerate()
                .map(|(position, link)| ("links", position, link.id.as_str())),
        );
    member_findings(name, local, members, owned, out);
}

fn type_name_findings<T>(
    what: &str,
    name: &str,
    local: &str,
    owned: &Owned<'_, T>,
    out: &mut Vec<Diagnostic>,
) {
    let pointer = Some(owned.at("/name"));
    if looks_plural(local) {
        out.push(finding(
            codes::PLURAL_TYPE_NAME,
            format!("{what} {name} has a plural local name; name the concept for one instance"),
            &owned.artifact.artifact_id,
            pointer.clone(),
        ));
    }
    if let Some(marker) = type_version_marker(local) {
        out.push(finding(
            codes::VERSIONED_NAME,
            format!(
                "{what} {name} carries the version or copy marker {marker:?}; version the artifact, not the concept's name"
            ),
            &owned.artifact.artifact_id,
            pointer,
        ));
    }
}

fn member_findings<'m, T>(
    type_name: &str,
    local: &str,
    members: impl Iterator<Item = (&'static str, usize, &'m str)>,
    owned: &Owned<'_, T>,
    out: &mut Vec<Diagnostic>,
) {
    let prefix = format!("{}_", snake_case(local));
    for (collection, position, id) in members {
        let pointer = Some(owned.at(&format!("/{collection}/{position}/id")));
        if id.starts_with(&prefix) {
            out.push(finding(
                codes::REDUNDANT_MEMBER_PREFIX,
                format!(
                    "member {id} of {type_name} repeats the type name as a prefix; the type already qualifies it"
                ),
                &owned.artifact.artifact_id,
                pointer.clone(),
            ));
        }
        if let Some(marker) = member_version_marker(id) {
            out.push(finding(
                codes::VERSIONED_NAME,
                format!(
                    "member {id} of {type_name} carries the version or copy marker {marker:?}; version the artifact, not the member's name"
                ),
                &owned.artifact.artifact_id,
                pointer,
            ));
        }
    }
}

/// `SalesOrder` → `sales_order`, `SKUItem` → `sku_item`.
pub(crate) fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut previous: Option<char> = None;
    for (position, &current) in chars.iter().enumerate() {
        if current.is_ascii_uppercase() {
            let next = chars.get(position.saturating_add(1)).copied();
            let after_lower =
                previous.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            let acronym_end = previous.is_some_and(|c| c.is_ascii_uppercase())
                && next.is_some_and(|c| c.is_ascii_lowercase());
            if after_lower || acronym_end {
                out.push('_');
            }
            out.push(current.to_ascii_lowercase());
        } else {
            out.push(current);
        }
        previous = Some(current);
    }
    out
}

fn looks_plural(local: &str) -> bool {
    let mut reversed = local.chars().rev();
    matches!(
        (reversed.next(), reversed.next()),
        (Some('s'), Some(before)) if !matches!(before, 's' | 'u' | 'i')
    )
}

fn ends_lower_or_digit(stem: &str) -> bool {
    stem.chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn type_version_marker(local: &str) -> Option<String> {
    let without_digits = local.trim_end_matches(|c: char| c.is_ascii_digit());
    if without_digits.len() < local.len()
        && let Some(stem) = without_digits.strip_suffix('V')
        && ends_lower_or_digit(stem)
    {
        return local.strip_prefix(stem).map(str::to_owned);
    }
    TYPE_COPY_MARKERS
        .iter()
        .find(|marker| {
            local
                .strip_suffix(**marker)
                .is_some_and(ends_lower_or_digit)
        })
        .map(|marker| (*marker).to_owned())
}

fn member_version_marker(id: &str) -> Option<String> {
    let (_, last) = id.rsplit_once('_')?;
    let versioned = last
        .strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
    (versioned || MEMBER_COPY_MARKERS.contains(&last)).then(|| last.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_splits_words_and_acronyms() {
        assert_eq!(snake_case("SalesOrder"), "sales_order");
        assert_eq!(snake_case("SKUItem"), "sku_item");
        assert_eq!(snake_case("Sku"), "sku");
        assert_eq!(snake_case("Iso3166Code"), "iso3166_code");
    }

    #[test]
    fn plural_names_are_detected_deterministically() {
        for plural in ["Orders", "Categories", "Customers"] {
            assert!(looks_plural(plural), "{plural}");
        }
        for singular in ["Address", "Status", "Analysis", "Sku", "S"] {
            assert!(!looks_plural(singular), "{singular}");
        }
    }

    #[test]
    fn version_markers_are_suffixes_after_a_word() {
        assert_eq!(type_version_marker("CustomerV2").as_deref(), Some("V2"));
        assert_eq!(
            type_version_marker("OrderLegacy").as_deref(),
            Some("Legacy")
        );
        assert_eq!(type_version_marker("V2"), None);
        assert_eq!(type_version_marker("Iso3166"), None);
        assert_eq!(type_version_marker("Copy"), None);
        assert_eq!(member_version_marker("price_v2").as_deref(), Some("v2"));
        assert_eq!(member_version_marker("total_old").as_deref(), Some("old"));
        assert_eq!(member_version_marker("v2"), None);
        assert_eq!(member_version_marker("is_new"), None);
        assert_eq!(member_version_marker("level_v"), None);
    }
}
