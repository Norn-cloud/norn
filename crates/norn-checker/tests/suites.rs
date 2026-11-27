//! `norn-checker`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-checker::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-checker --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::duplicate_mod,
    reason = "each suite keeps the visibility and the shared helpers it had as its own test binary"
)]

#[path = "action_aggregate_set.rs"]
mod action_aggregate_set;
#[path = "action_effect_program.rs"]
mod action_effect_program;
#[path = "action_firing.rs"]
mod action_firing;
#[path = "action_slice_set.rs"]
mod action_slice_set;
#[path = "analytics_and_units.rs"]
mod analytics_and_units;
#[path = "artifact_set.rs"]
mod artifact_set;
#[path = "check_receipt.rs"]
mod check_receipt;
#[path = "code_registry.rs"]
mod code_registry;
#[path = "consistency.rs"]
mod consistency;
#[path = "consistency_divergence.rs"]
mod consistency_divergence;
#[path = "consistency_exchange.rs"]
mod consistency_exchange;
#[path = "core_time.rs"]
mod core_time;
#[path = "core_units.rs"]
mod core_units;
#[path = "core_value_types.rs"]
mod core_value_types;
#[path = "defined_concepts.rs"]
mod defined_concepts;
#[path = "definition_catalog.rs"]
mod definition_catalog;
#[path = "definitions_diff.rs"]
mod definitions_diff;
#[path = "demo_measurements.rs"]
mod demo_measurements;
#[path = "deontic.rs"]
mod deontic;
#[path = "diagnosis.rs"]
mod diagnosis;
#[path = "diagnosis_report.rs"]
mod diagnosis_report;
#[path = "diagnostic_snapshots.rs"]
mod diagnostic_snapshots;
#[path = "entitlement_join_budget.rs"]
mod entitlement_join_budget;
#[path = "entity_artifact_set.rs"]
mod entity_artifact_set;
#[path = "entity_instance.rs"]
mod entity_instance;
#[path = "entity_lowering.rs"]
mod entity_lowering;
#[path = "entity_model.rs"]
mod entity_model;
#[path = "entity_relation_roots.rs"]
mod entity_relation_roots;
#[path = "evidence_pins.rs"]
mod evidence_pins;
#[path = "explain.rs"]
mod explain;
#[path = "extension_scope.rs"]
mod extension_scope;
#[path = "extension_scope_consumers.rs"]
mod extension_scope_consumers;
#[path = "function_admission.rs"]
mod function_admission;
#[path = "incremental.rs"]
mod incremental;
#[path = "inheritance_ascent.rs"]
mod inheritance_ascent;
#[path = "labels.rs"]
mod labels;
#[path = "law_ir.rs"]
mod law_ir;
#[path = "layout_admission.rs"]
mod layout_admission;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "lifecycle_writes.rs"]
mod lifecycle_writes;
#[path = "mapping.rs"]
mod mapping;
#[path = "message_catalogs.rs"]
mod message_catalogs;
#[path = "metric.rs"]
mod metric;
#[path = "metric_interchange.rs"]
mod metric_interchange;
#[path = "model.rs"]
mod model;
#[path = "norm_precedence.rs"]
mod norm_precedence;
#[path = "object_types.rs"]
mod object_types;
#[path = "obligations.rs"]
mod obligations;
#[path = "ontology_link.rs"]
mod ontology_link;
#[path = "ontology_lint.rs"]
mod ontology_lint;
#[path = "ontology_pack.rs"]
mod ontology_pack;
#[path = "ontology_registry.rs"]
mod ontology_registry;
#[path = "owned_collections.rs"]
mod owned_collections;
#[path = "path_mapping.rs"]
mod path_mapping;
#[path = "pr_c_honesty_and_red_team.rs"]
mod pr_c_honesty_and_red_team;
#[path = "predicate_slice_and_links.rs"]
mod predicate_slice_and_links;
#[path = "principal_inputs.rs"]
mod principal_inputs;
#[path = "property_types.rs"]
mod property_types;
#[path = "quantity_ledger.rs"]
mod quantity_ledger;
#[path = "quantity_ledger_boundary.rs"]
mod quantity_ledger_boundary;
#[path = "quantity_ledger_expression.rs"]
mod quantity_ledger_expression;
#[path = "quantity_ledger_properties.rs"]
mod quantity_ledger_properties;
#[path = "quantity_unit_ontology_pack.rs"]
mod quantity_unit_ontology_pack;
#[path = "ref_additivity.rs"]
mod ref_additivity;
#[path = "registry_packs.rs"]
mod registry_packs;
#[path = "restatement.rs"]
mod restatement;
#[path = "shared_lifecycle_semantics.rs"]
mod shared_lifecycle_semantics;
#[path = "source_schema.rs"]
mod source_schema;
#[path = "stress_proposals.rs"]
mod stress_proposals;
#[path = "table_analysis.rs"]
mod table_analysis;
#[path = "technical_view.rs"]
mod technical_view;
#[path = "three_systems.rs"]
mod three_systems;
#[path = "tool_effect.rs"]
mod tool_effect;
#[path = "transform.rs"]
mod transform;
#[path = "transform_sql.rs"]
mod transform_sql;
#[path = "typed_members.rs"]
mod typed_members;
#[path = "typed_payload_roots_compatibility.rs"]
mod typed_payload_roots_compatibility;
#[path = "vendor_mapping_install.rs"]
mod vendor_mapping_install;
#[path = "write_back_lens.rs"]
mod write_back_lens;
