//! `norn-ir`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-ir::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-ir --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.
#![allow(
    dead_code,
    unreachable_pub,
    reason = "each suite keeps the visibility it had as its own test binary"
)]

#[path = "access_literals.rs"]
mod access_literals;
#[path = "aggregate_input.rs"]
mod aggregate_input;
#[path = "artifact_governance.rs"]
mod artifact_governance;
#[path = "data_quality_finding.rs"]
mod data_quality_finding;
#[path = "decision_question.rs"]
mod decision_question;
#[path = "dectest_corpus.rs"]
mod dectest_corpus;
#[path = "entitlement.rs"]
mod entitlement;
#[path = "entity_v2_inheritance.rs"]
mod entity_v2_inheritance;
#[path = "entity_v2_lifecycles.rs"]
mod entity_v2_lifecycles;
#[path = "entity_v2_links.rs"]
mod entity_v2_links;
#[path = "entity_v2_object_types.rs"]
mod entity_v2_object_types;
#[path = "entity_v2_property_types.rs"]
mod entity_v2_property_types;
#[path = "facts.rs"]
mod facts;
#[path = "feed_patch_laws.rs"]
mod feed_patch_laws;
#[path = "function_manifest.rs"]
mod function_manifest;
#[path = "law_ir.rs"]
mod law_ir;
#[path = "limit_profile.rs"]
mod limit_profile;
#[path = "mapping_ir.rs"]
mod mapping_ir;
#[path = "metric.rs"]
mod metric;
#[path = "policy_values.rs"]
mod policy_values;
#[path = "proptest_ir_roundtrip.rs"]
mod proptest_ir_roundtrip;
#[path = "provenance_envelope.rs"]
mod provenance_envelope;
#[path = "rule_hit_policy.rs"]
mod rule_hit_policy;
#[path = "runtime_values.rs"]
mod runtime_values;
#[path = "source_schema_ir.rs"]
mod source_schema_ir;
#[path = "source_tree.rs"]
mod source_tree;
#[path = "technical_view.rs"]
mod technical_view;
#[path = "typed_action_rule_roots.rs"]
mod typed_action_rule_roots;
#[path = "typed_entity_relation_roots.rs"]
mod typed_entity_relation_roots;
#[path = "typed_mapping_roots.rs"]
mod typed_mapping_roots;
#[path = "typed_payload_roots_fixtures.rs"]
mod typed_payload_roots_fixtures;
#[path = "typed_payload_roots_schema.rs"]
mod typed_payload_roots_schema;
#[path = "typed_remaining_roots.rs"]
mod typed_remaining_roots;
#[path = "typed_setting_business_pack_roots.rs"]
mod typed_setting_business_pack_roots;
