#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Ontology lint (T8-LINT-1 #2593, T8-LINT-2 #2594): every registered rule
//! has a positive fixture that emits its code with the registered severity
//! and a negative fixture that does not.
//!
//! Fixtures live in `fixtures/ir/lint/<rule id>/{positive,negative}.json`.
//! Each is `{ "artifacts": [envelope], "base": [envelope], "coverage":
//! [coverage] }` (`base` and `coverage` optional); coverage uses the JSON
//! shape decoded by [`Coverage`] below.

use std::{fs, path::PathBuf};

use norn_checker::lint::{
    FailSeverity, IgnoredSourceField, LintInput, LintThresholds, MappingCoverage, MemberCoverage,
    MemberRef, MemberStatus, RULES, SourceRef, lint_artifact_set_with,
};
use norn_checker::{Diagnostic, codes};
use norn_ir::{ArtifactEnvelope, LinkId, ObjectTypeName, PropertyId};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    artifacts: Vec<ArtifactEnvelope>,
    #[serde(default)]
    base: Vec<ArtifactEnvelope>,
    #[serde(default)]
    coverage: Vec<Coverage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Coverage {
    mapping_artifact_id: String,
    mapping_version: String,
    source: Source,
    target: String,
    members: Vec<Member>,
    #[serde(default)]
    ignored_source_fields: Vec<Ignored>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Source {
    schema_id: String,
    entity: String,
    fields: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Member {
    property: Option<String>,
    link: Option<String>,
    source_fields: Option<Vec<String>>,
    gap: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ignored {
    field: String,
    reason: String,
}

impl Coverage {
    fn into_coverage(self) -> MappingCoverage {
        MappingCoverage {
            mapping_artifact_id: self.mapping_artifact_id,
            mapping_version: self.mapping_version,
            source: SourceRef {
                schema_id: self.source.schema_id,
                entity: self.source.entity,
                fields: self.source.fields,
            },
            target: ObjectTypeName(self.target),
            members: self
                .members
                .into_iter()
                .map(|member| MemberCoverage {
                    member: match (member.property, member.link) {
                        (Some(id), None) => MemberRef::Property(PropertyId(id)),
                        (None, Some(id)) => MemberRef::Link(LinkId(id)),
                        _ => panic!("a member names exactly one property or link"),
                    },
                    status: match (member.source_fields, member.gap) {
                        (Some(source_fields), None) => MemberStatus::Mapped { source_fields },
                        (None, Some(reason)) => MemberStatus::Gap { reason },
                        _ => panic!("a member has exactly one of sourceFields or gap"),
                    },
                })
                .collect(),
            ignored_source_fields: self
                .ignored_source_fields
                .into_iter()
                .map(|ignored| IgnoredSourceField {
                    field: ignored.field,
                    reason: ignored.reason,
                })
                .collect(),
        }
    }
}

fn fixture_path(rule: &str, polarity: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ir/lint")
        .join(rule)
        .join(format!("{polarity}.json"))
}

fn lint_fixture(rule: &str, polarity: &str) -> Vec<Diagnostic> {
    lint_fixture_with(rule, polarity, &LintThresholds::default())
}

fn load_fixture(rule: &str, polarity: &str) -> Fixture {
    let path = fixture_path(rule, polarity);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should decode: {error}", path.display()))
}

fn lint_fixture_with(rule: &str, polarity: &str, thresholds: &LintThresholds) -> Vec<Diagnostic> {
    lint_loaded_fixture(load_fixture(rule, polarity), thresholds)
}

fn lint_loaded_fixture(fixture: Fixture, thresholds: &LintThresholds) -> Vec<Diagnostic> {
    let coverage: Vec<MappingCoverage> = fixture
        .coverage
        .into_iter()
        .map(Coverage::into_coverage)
        .collect();
    let input = LintInput {
        artifacts: &fixture.artifacts,
        base: &fixture.base,
        coverage: &coverage,
    };
    let first = lint_artifact_set_with(&input, thresholds);
    assert_eq!(
        first,
        lint_artifact_set_with(&input, thresholds),
        "lint output is not deterministic"
    );
    first
}

fn edit_object_type(
    artifact: &mut ArtifactEnvelope,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) {
    let mut value = serde_json::to_value(&*artifact).expect("serialize artifact");
    let def = value
        .pointer_mut("/payload/objectType")
        .expect("object type")
        .as_object_mut()
        .expect("definition object");
    edit(def);
    *artifact = serde_json::from_value(value).expect("decode edited artifact");
}

#[test]
fn every_rule_has_a_positive_and_a_negative_fixture() {
    assert!(!RULES.is_empty());
    for rule in RULES {
        let registered = codes::lookup(rule.code)
            .unwrap_or_else(|| panic!("{} code {} is unregistered", rule.id, rule.code));

        let positive = lint_fixture(rule.id, "positive");
        let hits: Vec<&Diagnostic> = positive
            .iter()
            .filter(|finding| finding.code == rule.code)
            .collect();
        assert!(
            !hits.is_empty(),
            "{} positive fixture emits no {}; got {positive:#?}",
            rule.id,
            rule.code
        );
        for hit in hits {
            assert_eq!(hit.severity, registered.severity, "{}", rule.id);
            assert!(
                hit.artifact_id.is_some(),
                "{} finding has no artifact",
                rule.id
            );
        }

        let negative = lint_fixture(rule.id, "negative");
        let misses: Vec<&Diagnostic> = negative
            .iter()
            .filter(|finding| finding.code == rule.code)
            .collect();
        assert!(
            misses.is_empty(),
            "{} negative fixture emits {}: {misses:#?}",
            rule.id,
            rule.code
        );
    }
}

#[test]
fn every_lint_code_belongs_to_exactly_one_rule() {
    let lint_codes: Vec<&str> = codes::REGISTRY
        .iter()
        .map(|entry| entry.code)
        .filter(|code| code.starts_with("ontology.lint."))
        .collect();
    let rule_codes: Vec<&str> = {
        let mut rule_codes: Vec<&str> = RULES.iter().map(|rule| rule.code).collect();
        rule_codes.sort_unstable();
        rule_codes
    };
    assert_eq!(lint_codes, rule_codes);
}

#[test]
fn rules_have_both_warning_and_error_severities() {
    let severities: Vec<_> = RULES
        .iter()
        .filter_map(norn_checker::lint::LintRule::severity)
        .collect();
    assert_eq!(severities.len(), RULES.len());
    assert!(severities.contains(&norn_ir::DiagnosticSeverity::Error));
    assert!(severities.contains(&norn_ir::DiagnosticSeverity::Warning));
}

#[test]
fn gating_is_separate_from_severity() {
    // NORN-NAME-001 emits only warnings: reported, but not failing by default.
    let findings = lint_fixture("NORN-NAME-001", "positive");
    assert!(!findings.is_empty());
    assert!(findings.iter().all(|finding| !finding.is_error()));
    assert!(!FailSeverity::default().fails(&findings));
    assert!(FailSeverity::Warning.fails(&findings));
    assert!(!FailSeverity::Never.fails(&findings));

    // NORN-OBJ-001 emits an error, which fails the default gate.
    let findings = lint_fixture("NORN-OBJ-001", "positive");
    assert!(FailSeverity::default().fails(&findings));
}

#[test]
fn duplicate_is_reported_against_the_first_definition() {
    let findings = lint_fixture("NORN-OBJ-001", "positive");
    let duplicate = findings
        .iter()
        .find(|finding| finding.code == "ontology.lint.duplicate_definition")
        .expect("a duplicate finding");
    assert_eq!(duplicate.artifact_id.as_deref(), Some("crm.Client.entity"));
    assert_eq!(
        duplicate.span.as_ref().map(|span| span.pointer.as_str()),
        Some("/payload/objectType/name")
    );
    assert!(
        duplicate.message.contains("crm.Customer"),
        "{}",
        duplicate.message
    );
}

#[test]
fn mapping_requiredness_uses_the_effective_model() {
    // crm.Customer extends crm.Party (whose `tier` has a default), declares
    // `memo` through a nullable shared property type, and gains the required
    // `acme__shelf_label` from an extension.
    let required: Vec<String> = lint_fixture("NORN-MAP-002", "effective.positive")
        .into_iter()
        .filter(|finding| finding.code == "ontology.lint.unmapped_required_member")
        .map(|finding| finding.message)
        .collect();
    assert_eq!(required.len(), 1, "{required:#?}");
    assert!(
        required
            .iter()
            .all(|message| message.contains("acme__shelf_label")),
        "{required:#?}"
    );

    let negative = lint_fixture("NORN-MAP-002", "effective.negative");
    assert!(
        negative.iter().all(
            |finding| finding.code != "ontology.lint.unmapped_required_member"
                && finding.code != "ontology.lint.unknown_mapping_target"
        ),
        "{negative:#?}"
    );
}

#[test]
fn default_thresholds_are_the_documented_profile() {
    let defaults = LintThresholds::default();
    assert_eq!(defaults.mega_object_max_members, 40);
    assert_eq!(defaults.deep_hierarchy_max_depth, 3);
    assert_eq!(defaults.source_mirror_min_shared, 5);
    assert_eq!(defaults.source_mirror_min_percent, 80);
}

#[test]
fn smells_follow_their_thresholds() {
    let has =
        |findings: &[Diagnostic], code: &str| findings.iter().any(|finding| finding.code == code);
    // 41 members: a mega-object at the default 40, not at 41.
    let loose = LintThresholds {
        mega_object_max_members: 41,
        ..LintThresholds::default()
    };
    assert!(has(
        &lint_fixture("NORN-OBJ-002", "positive"),
        "ontology.lint.mega_object"
    ));
    assert!(!has(
        &lint_fixture_with("NORN-OBJ-002", "positive", &loose),
        "ontology.lint.mega_object"
    ));
    // Three supertypes pass at the default and fail at depth 2.
    let strict = LintThresholds {
        deep_hierarchy_max_depth: 2,
        ..LintThresholds::default()
    };
    assert!(has(
        &lint_fixture_with("NORN-OBJ-003", "negative", &strict),
        "ontology.lint.deep_hierarchy"
    ));
    // Four shared names mirror once the minimum drops to four.
    let four = LintThresholds {
        source_mirror_min_shared: 4,
        source_mirror_min_percent: 60,
        ..LintThresholds::default()
    };
    assert!(has(
        &lint_fixture_with("NORN-SRC-003", "negative", &four),
        "ontology.lint.source_mirroring"
    ));
}

#[test]
fn weak_relationship_points_at_the_scalar_property() {
    let findings = lint_fixture("NORN-REL-001", "positive");
    let weak = findings
        .iter()
        .find(|finding| finding.code == "ontology.lint.weak_relationship")
        .expect("a weak-relationship finding");
    assert_eq!(weak.artifact_id.as_deref(), Some("sales.Order.entity"));
    assert_eq!(
        weak.span.as_ref().map(|span| span.pointer.as_str()),
        Some("/payload/objectType/properties/0/id")
    );
}

#[test]
fn tier_two_findings_are_warnings_and_obey_the_default_gate() {
    for rule in RULES
        .iter()
        .filter(|rule| rule.tier == norn_checker::lint::LintTier::Two)
    {
        let findings: Vec<_> = lint_fixture(rule.id, "positive")
            .into_iter()
            .filter(|finding| finding.code == rule.code)
            .collect();
        assert!(!findings.is_empty(), "{}", rule.id);
        assert!(!FailSeverity::default().fails(&findings), "{}", rule.id);
        assert!(FailSeverity::Warning.fails(&findings), "{}", rule.id);
    }
}

#[test]
fn source_mirroring_requires_the_percentage_on_both_sides() {
    let mirrors = |fixture| {
        lint_loaded_fixture(fixture, &LintThresholds::default())
            .iter()
            .any(|finding| finding.code == "ontology.lint.source_mirroring")
    };
    // Five of six source fields: above 80%, and five of five members.
    assert!(mirrors(load_fixture("NORN-SRC-003", "positive")));
    // Five of seven source fields fails even with a perfect member match.
    let mut fixture = load_fixture("NORN-SRC-003", "positive");
    fixture
        .coverage
        .first_mut()
        .expect("coverage")
        .source
        .fields
        .push("bookkeeping".to_owned());
    assert!(!mirrors(fixture));

    // Five of seven members fails even when all source fields are copied.
    let mut fixture = load_fixture("NORN-SRC-003", "positive");
    let artifact = fixture.artifacts.first_mut().expect("object artifact");
    edit_object_type(artifact, |def| {
        let properties = def
            .get_mut("properties")
            .expect("properties")
            .as_array_mut()
            .expect("property array");
        for id in ["business_label", "business_category"] {
            properties.push(serde_json::json!({"id": id, "type": {"kind": "text"}}));
        }
    });
    assert!(!mirrors(fixture));

    // Integer ratios have no rounding: 5/6 is below 84% and above 83%.
    for (percent, expected) in [(83, true), (84, false)] {
        let thresholds = LintThresholds {
            source_mirror_min_percent: percent,
            ..LintThresholds::default()
        };
        assert_eq!(
            lint_fixture_with("NORN-SRC-003", "positive", &thresholds)
                .iter()
                .any(|finding| finding.code == "ontology.lint.source_mirroring"),
            expected
        );
    }
}

#[test]
fn deep_hierarchy_resolves_base_ancestors_and_reports_only_candidates() {
    let mut fixture = load_fixture("NORN-OBJ-003", "positive");
    let branch = fixture.artifacts.pop().expect("leaf");
    fixture.base = std::mem::take(&mut fixture.artifacts);
    fixture.artifacts.push(branch);
    let findings: Vec<_> = lint_loaded_fixture(fixture, &LintThresholds::default())
        .into_iter()
        .filter(|finding| finding.code == "ontology.lint.deep_hierarchy")
        .collect();
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings
            .first()
            .expect("leaf finding")
            .artifact_id
            .as_deref(),
        Some("core.Branch.entity")
    );
}

#[test]
fn weak_relationship_accepts_an_inherited_link_from_the_base() {
    let mut fixture = load_fixture("NORN-REL-001", "negative");
    let mut parent = fixture.artifacts.last().expect("order").clone();
    parent.artifact_id = "sales.OrderBase.entity".to_owned();
    edit_object_type(&mut parent, |def| {
        def.insert("name".to_owned(), serde_json::json!("sales.OrderBase"));
        def.remove("properties");
    });
    fixture.base.push(parent);
    edit_object_type(fixture.artifacts.last_mut().expect("order"), |def| {
        def.remove("links");
        def.insert("extends".to_owned(), serde_json::json!("sales.OrderBase"));
    });
    assert!(
        lint_loaded_fixture(fixture, &LintThresholds::default())
            .iter()
            .all(|finding| finding.code != "ontology.lint.weak_relationship")
    );
}

#[test]
fn weak_relationship_accepts_extension_links() {
    let mut fixture = load_fixture("NORN-REL-001", "positive");
    let mut extension = serde_json::to_value(fixture.artifacts.last().expect("order"))
        .expect("serialize extension envelope");
    extension.as_object_mut().expect("envelope").insert(
        "artifactId".to_owned(),
        serde_json::json!("acme.order.extension"),
    );
    extension.as_object_mut().expect("envelope").insert(
        "payload".to_owned(),
        serde_json::json!({
            "entityVersion": "2.0",
            "objectTypeExtension": {
                "namespace": "acme", "target": "sales.Order",
                "links": [{
                    "id": "acme__customer", "target": "crm.Customer",
                    "cardinality": {"to": "one", "from": "many"}
                }]
            }
        }),
    );
    fixture
        .artifacts
        .push(serde_json::from_value(extension).expect("extension"));
    assert!(
        lint_loaded_fixture(fixture, &LintThresholds::default())
            .iter()
            .all(|finding| finding.code != "ontology.lint.weak_relationship")
    );
}

#[test]
fn weak_relationship_skips_ambiguous_local_names() {
    let mut fixture = load_fixture("NORN-REL-001", "positive");
    let mut other = fixture.artifacts.first().expect("customer").clone();
    other.artifact_id = "support.Customer.entity".to_owned();
    edit_object_type(&mut other, |def| {
        def.insert("name".to_owned(), serde_json::json!("support.Customer"));
    });
    fixture.base.push(other);
    assert!(
        lint_loaded_fixture(fixture, &LintThresholds::default())
            .iter()
            .all(|finding| finding.code != "ontology.lint.weak_relationship")
    );
}
