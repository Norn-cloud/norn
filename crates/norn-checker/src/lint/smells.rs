//! Ontology smells with deterministic thresholds (T8-LINT-2), all warnings.
//! Every threshold is a field of [`LintThresholds`] whose default is
//! documented there and listed in the rule registry ([`super::RULES`]).
//!
//! - [`codes::MEGA_OBJECT`]: an object type declares more than
//!   [`LintThresholds::mega_object_max_members`] own properties and links
//!   together; it is several concepts in one.
//! - [`codes::DEEP_HIERARCHY`]: an object type has more than
//!   [`LintThresholds::deep_hierarchy_max_depth`] supertypes above it
//!   (resolved through the set and the base; the chain stops at an unknown
//!   or repeated name).
//! - [`codes::WEAK_RELATIONSHIP`]: a property id is `<type>_id`, `<type>_ids`,
//!   `<type>_ref`, or `<type>_key`, where `<type>` is the snake-cased local
//!   name of exactly one other object type of the set or the base, and neither the
//!   declaring type nor its supertypes (including their extensions) have a link to that type: a
//!   relationship hidden in a scalar. A type's own `<type>_id` is its
//!   identity, not a relationship, and is not flagged.
//! - [`codes::SOURCE_MIRRORING`]: an object type copies a source entity's
//!   columns instead of modelling the concept. With `shared` the number of
//!   the type's normalized own member names ([`super::fingerprint`]) that
//!   equal a normalized field of one source entity of the set's mapping
//!   coverage, the smell fires when `shared` is at least
//!   [`LintThresholds::source_mirror_min_shared`] and at least
//!   [`LintThresholds::source_mirror_min_percent`] percent of both the
//!   type's members and the source entity's fields.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::ObjectTypeDef;

use super::finding;
use super::fingerprint::{fingerprint, normalize_name};
use super::model::{LintSet, Owned};
use super::naming::snake_case;
use crate::Diagnostic;

/// Diagnostic codes of the smells.
pub mod codes {
    /// An object type has too many own members.
    pub const MEGA_OBJECT: &str = "ontology.lint.mega_object";
    /// An object type's inheritance chain is too deep.
    pub const DEEP_HIERARCHY: &str = "ontology.lint.deep_hierarchy";
    /// A relationship is modelled as a scalar reference property.
    pub const WEAK_RELATIONSHIP: &str = "ontology.lint.weak_relationship";
    /// An object type mirrors a source entity's fields.
    pub const SOURCE_MIRRORING: &str = "ontology.lint.source_mirroring";
}

/// Default of [`LintThresholds::mega_object_max_members`].
pub const MEGA_OBJECT_MAX_MEMBERS: usize = 40;
/// Default of [`LintThresholds::deep_hierarchy_max_depth`].
pub const DEEP_HIERARCHY_MAX_DEPTH: usize = 3;
/// Default of [`LintThresholds::source_mirror_min_shared`].
pub const SOURCE_MIRROR_MIN_SHARED: usize = 5;
/// Default of [`LintThresholds::source_mirror_min_percent`].
pub const SOURCE_MIRROR_MIN_PERCENT: usize = 80;

/// Property id suffixes that mark a scalar reference to another type.
const REFERENCE_SUFFIXES: [&str; 4] = ["_ids", "_id", "_ref", "_key"];

/// The deterministic thresholds of the smells. [`Default`] is the documented
/// profile every gate uses unless it states otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LintThresholds {
    /// The most own properties plus links an object type may declare before
    /// it is a mega-object (default 40).
    pub mega_object_max_members: usize,
    /// The most supertypes above an object type before its hierarchy is too
    /// deep (default 3).
    pub deep_hierarchy_max_depth: usize,
    /// The fewest shared member/field names that can make a mirror
    /// (default 5).
    pub source_mirror_min_shared: usize,
    /// The percentage of both the type's members and the source's fields the
    /// shared names must reach (default 80).
    pub source_mirror_min_percent: usize,
}

impl Default for LintThresholds {
    fn default() -> Self {
        Self {
            mega_object_max_members: MEGA_OBJECT_MAX_MEMBERS,
            deep_hierarchy_max_depth: DEEP_HIERARCHY_MAX_DEPTH,
            source_mirror_min_shared: SOURCE_MIRROR_MIN_SHARED,
            source_mirror_min_percent: SOURCE_MIRROR_MIN_PERCENT,
        }
    }
}

/// Every smell over every candidate object type.
#[must_use]
pub(crate) fn lint_smells(set: &LintSet<'_>, thresholds: &LintThresholds) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let types_by_snake = types_by_snake_name(set);
    let sources = source_fields(set);
    for owned in set.candidate_object_types() {
        mega_object(owned, thresholds, &mut out);
        deep_hierarchy(set, owned, thresholds, &mut out);
        weak_relationships(set, owned, &types_by_snake, &mut out);
        source_mirroring(owned, &sources, thresholds, &mut out);
    }
    out
}

fn mega_object(
    owned: &Owned<'_, ObjectTypeDef>,
    thresholds: &LintThresholds,
    out: &mut Vec<Diagnostic>,
) {
    let def = &owned.def;
    let members = def.properties.len().saturating_add(def.links.len());
    if members > thresholds.mega_object_max_members {
        out.push(finding(
            codes::MEGA_OBJECT,
            format!(
                "object type {} declares {members} properties and links, more than {}; split it into the concepts it combines",
                def.name.as_str(),
                thresholds.mega_object_max_members
            ),
            &owned.artifact.artifact_id,
            Some(owned.at("/name")),
        ));
    }
}

fn deep_hierarchy(
    set: &LintSet<'_>,
    owned: &Owned<'_, ObjectTypeDef>,
    thresholds: &LintThresholds,
    out: &mut Vec<Diagnostic>,
) {
    let chain = set.ancestry(&owned.def.name);
    let depth = chain.len().saturating_sub(1);
    if depth > thresholds.deep_hierarchy_max_depth {
        let path: Vec<&str> = chain.iter().map(|owned| owned.def.name.as_str()).collect();
        out.push(finding(
            codes::DEEP_HIERARCHY,
            format!(
                "object type {} has {depth} supertypes ({}), more than {}; prefer interfaces or composition",
                owned.def.name.as_str(),
                path.join(" -> "),
                thresholds.deep_hierarchy_max_depth
            ),
            &owned.artifact.artifact_id,
            Some(owned.at("/extends")),
        ));
    }
}

/// Snake-cased local name → its unique object type, or `None` if ambiguous.
fn types_by_snake_name<'s>(set: &'s LintSet<'_>) -> BTreeMap<String, Option<&'s ObjectTypeDef>> {
    let mut by_snake = BTreeMap::new();
    for owned in &set.object_types {
        by_snake
            .entry(snake_case(owned.def.name.local_name()))
            .and_modify(|target: &mut Option<&ObjectTypeDef>| {
                if target.is_some_and(|target| target.name != owned.def.name) {
                    *target = None;
                }
            })
            .or_insert(Some(&owned.def));
    }
    by_snake
}

fn weak_relationships(
    set: &LintSet<'_>,
    owned: &Owned<'_, ObjectTypeDef>,
    types_by_snake: &BTreeMap<String, Option<&ObjectTypeDef>>,
    out: &mut Vec<Diagnostic>,
) {
    let def = &owned.def;
    let own_snake = snake_case(def.name.local_name());
    let linked: BTreeSet<&str> = set
        .ancestry(&def.name)
        .iter()
        .flat_map(|ancestor| {
            ancestor.def.links.iter().chain(
                set.extensions_of(&ancestor.def.name)
                    .flat_map(|extension| extension.def.links.iter()),
            )
        })
        .map(|link| link.target.as_str())
        .collect();
    for (position, property) in def.properties.iter().enumerate() {
        let id = property.id.as_str();
        let Some(stem) = REFERENCE_SUFFIXES
            .iter()
            .find_map(|suffix| id.strip_suffix(*suffix))
        else {
            continue;
        };
        if stem == own_snake {
            continue;
        }
        let Some(Some(target)) = types_by_snake.get(stem) else {
            continue;
        };
        if linked.contains(target.name.as_str()) {
            continue;
        }
        out.push(finding(
            codes::WEAK_RELATIONSHIP,
            format!(
                "property {id} of {} refers to {} by a scalar value, and no link to {} exists; model the relationship as a link",
                def.name.as_str(),
                target.name.as_str(),
                target.name.as_str()
            ),
            &owned.artifact.artifact_id,
            Some(owned.at(&format!("/properties/{position}/id"))),
        ));
    }
}

/// One source entity of the coverage: `(schema id, entity)` → its
/// normalized field names (each source entity once).
fn source_fields<'c>(set: &LintSet<'c>) -> BTreeMap<(&'c str, &'c str), BTreeSet<String>> {
    let mut sources = BTreeMap::new();
    for coverage in set.coverage {
        sources
            .entry((
                coverage.source.schema_id.as_str(),
                coverage.source.entity.as_str(),
            ))
            .or_insert_with(|| {
                coverage
                    .source
                    .fields
                    .iter()
                    .map(|field| normalize_name(field))
                    .collect()
            });
    }
    sources
}

fn source_mirroring(
    owned: &Owned<'_, ObjectTypeDef>,
    sources: &BTreeMap<(&str, &str), BTreeSet<String>>,
    thresholds: &LintThresholds,
    out: &mut Vec<Diagnostic>,
) {
    let members = fingerprint(&owned.def).member_names;
    let reaches = |shared: usize, total: usize| {
        shared.saturating_mul(100) >= thresholds.source_mirror_min_percent.saturating_mul(total)
    };
    for ((schema_id, entity), fields) in sources {
        let shared = members.intersection(fields).count();
        if shared >= thresholds.source_mirror_min_shared
            && reaches(shared, members.len())
            && reaches(shared, fields.len())
        {
            out.push(finding(
                codes::SOURCE_MIRRORING,
                format!(
                    "object type {} mirrors source entity {entity} of {schema_id}: {shared} of its {} members match the {} fields of {entity} by name; model the business concept and map the source onto it",
                    owned.def.name.as_str(),
                    members.len(),
                    fields.len()
                ),
                &owned.artifact.artifact_id,
                Some(owned.at("/name")),
            ));
        }
    }
}
