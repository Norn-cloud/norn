//! The lint rule registry: every ontology lint rule, with its stable rule id,
//! the diagnostic code it emits, its tier, and its deterministic thresholds.
//!
//! Rule ids follow the 2026-09-09 ruling §6 (`NORN-OBJ-001` style), in the
//! families of the rule corpus (T8-RUL-1 #2591: `NAME`, `OBJ`, `REL`, `SRC`,
//! plus `MAP` for mapping coverage). Each rule emits exactly one diagnostic
//! code, registered in [`crate::codes::REGISTRY`]; the severity of a rule is
//! the severity of its code there, so the code registry is the single source
//! of severity. Ids and codes are never renamed or reused.

use norn_ir::DiagnosticSeverity;

use super::smells::{
    DEEP_HIERARCHY_MAX_DEPTH, MEGA_OBJECT_MAX_MEMBERS, SOURCE_MIRROR_MIN_PERCENT,
    SOURCE_MIRROR_MIN_SHARED,
};
use super::{duplicates, mapping, naming, provenance, smells};
use crate::codes;

/// The lint tier a rule belongs to (T8-LINT-1, T8-LINT-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LintTier {
    /// Naming, exact duplicates, provenance presence, mapping coverage.
    One,
    /// Structural fingerprints and smells with deterministic thresholds.
    Two,
}

/// A named, documented threshold of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Threshold {
    /// The threshold's field name in [`super::LintThresholds`].
    pub name: &'static str,
    /// The default value.
    pub default: usize,
    /// What the value bounds, and in which direction a finding fires.
    pub meaning: &'static str,
}

/// One registered lint rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LintRule {
    /// The stable rule id (`NORN-NAME-001`).
    pub id: &'static str,
    /// The diagnostic code every finding of the rule carries.
    pub code: &'static str,
    /// The tier.
    pub tier: LintTier,
    /// One-line statement of the rule.
    pub summary: &'static str,
    /// The rule's deterministic thresholds (empty when it has none).
    pub thresholds: &'static [Threshold],
}

impl LintRule {
    /// The severity of the rule's findings: its code's registered severity.
    #[must_use]
    pub fn severity(&self) -> Option<DiagnosticSeverity> {
        codes::lookup(self.code).map(|entry| entry.severity.clone())
    }
}

const fn rule(
    id: &'static str,
    code: &'static str,
    tier: LintTier,
    summary: &'static str,
) -> LintRule {
    LintRule {
        id,
        code,
        tier,
        summary,
        thresholds: &[],
    }
}

/// Every lint rule, ordered by rule id.
pub static RULES: &[LintRule] = &[
    rule(
        "NORN-MAP-001",
        mapping::codes::UNKNOWN_MAPPING_TARGET,
        LintTier::One,
        "a mapping targets an object type or member the ontology does not declare",
    ),
    rule(
        "NORN-MAP-002",
        mapping::codes::UNMAPPED_REQUIRED_MEMBER,
        LintTier::One,
        "every required property and link of a mapping's target is mapped or an explicit gap",
    ),
    rule(
        "NORN-MAP-003",
        mapping::codes::UNMAPPED_SOURCE_FIELD,
        LintTier::One,
        "every source field a mapping reads from is mapped or explicitly ignored",
    ),
    rule(
        "NORN-NAME-001",
        naming::codes::PLURAL_TYPE_NAME,
        LintTier::One,
        "object type and interface local names are singular",
    ),
    rule(
        "NORN-NAME-002",
        naming::codes::REDUNDANT_MEMBER_PREFIX,
        LintTier::One,
        "member ids do not repeat their type's name as a prefix",
    ),
    rule(
        "NORN-NAME-003",
        naming::codes::VERSIONED_NAME,
        LintTier::One,
        "type and member names carry no version or copy marker",
    ),
    rule(
        "NORN-OBJ-001",
        duplicates::codes::DUPLICATE_DEFINITION,
        LintTier::One,
        "no two differently named definitions have exactly the same structure",
    ),
    LintRule {
        thresholds: &[Threshold {
            name: "mega_object_max_members",
            default: MEGA_OBJECT_MAX_MEMBERS,
            meaning: "fires when own properties plus own links exceed this",
        }],
        ..rule(
            "NORN-OBJ-002",
            smells::codes::MEGA_OBJECT,
            LintTier::Two,
            "an object type does not combine several concepts in one oversized member list",
        )
    },
    LintRule {
        thresholds: &[Threshold {
            name: "deep_hierarchy_max_depth",
            default: DEEP_HIERARCHY_MAX_DEPTH,
            meaning: "fires when the number of supertypes above a type exceeds this",
        }],
        ..rule(
            "NORN-OBJ-003",
            smells::codes::DEEP_HIERARCHY,
            LintTier::Two,
            "inheritance chains stay shallow",
        )
    },
    rule(
        "NORN-REL-001",
        smells::codes::WEAK_RELATIONSHIP,
        LintTier::Two,
        "relationships between object types are links, not scalar <type>_id properties",
    ),
    rule(
        "NORN-SRC-001",
        provenance::codes::UNTRACEABLE_PROVENANCE,
        LintTier::One,
        "imported and migrated artifacts name their source path; agent artifacts name their actor",
    ),
    rule(
        "NORN-SRC-002",
        provenance::codes::INCOMPLETE_PROVENANCE,
        LintTier::One,
        "artifact provenance records its actor and creation time",
    ),
    LintRule {
        thresholds: &[
            Threshold {
                name: "source_mirror_min_shared",
                default: SOURCE_MIRROR_MIN_SHARED,
                meaning: "fires only when at least this many normalized member names equal source field names",
            },
            Threshold {
                name: "source_mirror_min_percent",
                default: SOURCE_MIRROR_MIN_PERCENT,
                meaning: "fires only when the shared names reach this percentage of both the type's members and the source entity's fields",
            },
        ],
        ..rule(
            "NORN-SRC-003",
            smells::codes::SOURCE_MIRRORING,
            LintTier::Two,
            "object types model business concepts rather than copying a source entity's columns",
        )
    },
];

/// The rule with stable id `id`.
#[must_use]
pub fn rule_by_id(id: &str) -> Option<&'static LintRule> {
    RULES.iter().find(|rule| rule.id == id)
}

/// The rule that emits diagnostic code `code`.
#[must_use]
pub fn rule_by_code(code: &str) -> Option<&'static LintRule> {
    RULES.iter().find(|rule| rule.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_sorted_unique_registered_and_one_code_each() {
        for pair in RULES.windows(2) {
            if let [left, right] = pair {
                assert!(left.id < right.id, "{} !< {}", left.id, right.id);
            }
        }
        for rule in RULES {
            assert!(
                codes::lookup(rule.code).is_some(),
                "{} emits unregistered code {}",
                rule.id,
                rule.code
            );
            assert!(rule.code.starts_with("ontology.lint."), "{}", rule.code);
            assert_eq!(rule_by_code(rule.code), Some(rule));
            assert_eq!(rule_by_id(rule.id), Some(rule));
        }
        let defaults = crate::lint::LintThresholds::default();
        let documented = |name: &str| {
            RULES
                .iter()
                .flat_map(|rule| rule.thresholds)
                .find(|threshold| threshold.name == name)
                .map(|threshold| threshold.default)
        };
        assert_eq!(
            documented("mega_object_max_members"),
            Some(defaults.mega_object_max_members)
        );
        assert_eq!(
            documented("deep_hierarchy_max_depth"),
            Some(defaults.deep_hierarchy_max_depth)
        );
        assert_eq!(
            documented("source_mirror_min_shared"),
            Some(defaults.source_mirror_min_shared)
        );
        assert_eq!(
            documented("source_mirror_min_percent"),
            Some(defaults.source_mirror_min_percent)
        );
    }
}
