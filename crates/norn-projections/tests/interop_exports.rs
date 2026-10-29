#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! One-way interop exports (T8-STD-2, #2751): the causal chain of "why is
//! order #1001 on hold?" exports to `OpenLineage` 2-0-2 run events and to a
//! PROV-JSONLD (PROV-O) document, each validated against the pinned external
//! upstream schema; exports never change their source.

use std::collections::BTreeSet;

use norn_ir::{LineageInput, ObjectTypeName};
use norn_projections::{
    CausalChain, ExportScope, explain_latest,
    openlineage_run_events as export_openlineage_run_events, prov_document as export_prov_document,
    stable_uuid as export_stable_uuid,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const ORDER_ON_HOLD: &str =
    include_str!("../../../fixtures/lineage/order-1001-on-hold.lineage.json");
const NORN_FACETS: &str = include_str!("../schemas/openlineage-norn-facets.json");

fn input() -> LineageInput {
    serde_json::from_str(ORDER_ON_HOLD).expect("lineage fixture decodes")
}

fn chain(input: &LineageInput) -> CausalChain {
    explain_latest(
        input,
        &ObjectTypeName("commerce.SalesOrder".to_owned()),
        "1001",
    )
    .expect("order 1001 is explained")
}

fn json_of(value: &norn_ir::JsonValue) -> Value {
    serde_json::to_value(value).expect("export encodes")
}

fn scope() -> ExportScope {
    ExportScope::new("tenant-cell:commerce-demo").expect("test cell id is valid")
}

fn resource_id(stable_id: &str) -> String {
    scope()
        .resource_id(stable_id)
        .expect("stable id canonicalizes")
}

fn openlineage_run_events(chain: &CausalChain) -> Vec<norn_ir::JsonValue> {
    export_openlineage_run_events(&scope(), chain).expect("export identities canonicalize")
}

fn prov_document(chain: &CausalChain) -> norn_ir::JsonValue {
    export_prov_document(&scope(), chain).expect("export identities canonicalize")
}

fn stable_uuid(stable_id: &str) -> String {
    export_stable_uuid(&scope(), stable_id).expect("export identity canonicalizes")
}

fn dcat_catalog(objects: &[norn_ir::LineageObject]) -> norn_ir::JsonValue {
    norn_projections::dcat_catalog(&scope(), objects).expect("export identities canonicalize")
}

fn skos_concept_scheme(defs: &[norn_ir::ObjectTypeDef]) -> norn_ir::JsonValue {
    norn_projections::skos_concept_scheme(&scope(), defs).expect("export identities canonicalize")
}

/// Every validation error of `instance` against `schema`, formats asserted.
fn errors(schema: &Value, instance: &Value) -> Vec<String> {
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .expect("vendored schema compiles");
    validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

fn datasets(event: &Value, member: &str) -> Vec<(String, String)> {
    event[member]
        .as_array()
        .expect("dataset list")
        .iter()
        .map(|dataset| {
            (
                dataset["namespace"].as_str().expect("namespace").to_owned(),
                dataset["name"].as_str().expect("name").to_owned(),
            )
        })
        .collect()
}

fn pair(namespace: &str, name: &str) -> (String, String) {
    (namespace.to_owned(), name.to_owned())
}

#[test]
fn interop_exports_openlineage_run_events_validate() {
    let input = input();
    let events: Vec<Value> = openlineage_run_events(&chain(&input))
        .iter()
        .map(json_of)
        .collect();
    assert_eq!(events.len(), 3, "one run event per commit in the chain");

    let schema: Value = serde_json::from_str(&interop_input("openlineage-2-0-2.schema.json"))
        .expect("schema is JSON");
    let facets: Value = serde_json::from_str(NORN_FACETS).expect("facet schema is JSON");
    let facet_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "#/$defs/NornCommitRunFacet",
        "$defs": facets["$defs"].clone(),
    });
    for event in &events {
        assert_eq!(errors(&schema, event), Vec::<String>::new(), "{event:#}");
        assert_eq!(
            errors(&facet_schema, &event["run"]["facets"]["nornCommit"]),
            Vec::<String>::new()
        );
    }

    // The hold: decided by the credit policy, reading the customer and the
    // order, writing the order.
    let hold = &events[0];
    assert_eq!(hold["eventType"], "COMPLETE");
    assert_eq!(hold["eventTime"], "2026-09-30T08:15:00Z");
    let norn_namespace = hold["job"]["namespace"].as_str().expect("job namespace");
    assert!(norn_namespace.starts_with("urn:norn:cell:"));
    assert_eq!(hold["job"]["name"], "credit.hold_policy@2");
    let exported_commit = hold["run"]["facets"]["nornCommit"]["commit"]
        .as_str()
        .expect("scoped commit id");
    assert!(exported_commit.starts_with("commit:"));
    assert_ne!(exported_commit, "commit:main/3");
    assert!(
        hold["run"]["facets"]["nornCommit"]["decision"]
            .as_str()
            .expect("scoped decision id")
            .starts_with("decision:")
    );
    assert_eq!(hold["run"]["runId"], stable_uuid("commit:main/3").as_str());
    assert_eq!(
        datasets(hold, "inputs"),
        [
            pair(norn_namespace, "crm.Customer"),
            pair(norn_namespace, "commerce.SalesOrder")
        ]
    );
    assert_eq!(
        datasets(hold, "outputs"),
        [pair(norn_namespace, "commerce.SalesOrder")]
    );

    // The ingest commits: the job is the mapping version, the input is the
    // ERPNext source table it read.
    let order_ingest = &events[1];
    assert_eq!(order_ingest["job"]["name"], "erpnext.sales_order@3");
    assert!(
        order_ingest["run"]["facets"]["nornCommit"]
            .get("decision")
            .is_none()
    );
    let source_namespace = datasets(order_ingest, "inputs")[0].0.clone();
    assert!(source_namespace.starts_with("urn:norn:cell:"));
    assert_ne!(source_namespace, norn_namespace);
    assert_eq!(
        datasets(order_ingest, "inputs"),
        [pair(&source_namespace, "erpnext.prod.tabSales Order")]
    );
    assert_eq!(
        datasets(order_ingest, "outputs"),
        [pair(norn_namespace, "commerce.SalesOrder")]
    );
    let customer_ingest = &events[2];
    assert_eq!(customer_ingest["job"]["name"], "erpnext.customer@2");
    assert_eq!(
        datasets(customer_ingest, "inputs"),
        [pair(&source_namespace, "erpnext.prod.tabCustomer")]
    );

    let run_ids: BTreeSet<&str> = events
        .iter()
        .map(|event| event["run"]["runId"].as_str().expect("run id"))
        .collect();
    assert_eq!(run_ids.len(), 3, "run ids are distinct per commit");
}

#[test]
fn interop_exports_stable_uuid_is_rfc9562_v8() {
    let id = stable_uuid("commit:main/3");
    assert_eq!(id, stable_uuid("commit:main/3"), "deterministic");
    assert_ne!(id, stable_uuid("commit:main/2"));
    let groups: Vec<&str> = id.split('-').collect();
    assert_eq!(
        groups.iter().map(|group| group.len()).collect::<Vec<_>>(),
        [8, 4, 4, 4, 12]
    );
    assert!(groups[2].starts_with('8'), "version 8: {id}");
    assert!(
        matches!(groups[3].chars().next(), Some('8' | '9' | 'a' | 'b')),
        "RFC 9562 variant: {id}"
    );
}

#[test]
fn interop_exports_scope_rejects_empty_whitespace_and_control_ids() {
    assert!(matches!(
        ExportScope::new(""),
        Err(norn_projections::InteropError::EmptyCellId)
    ));
    assert!(matches!(
        ExportScope::new(" \t "),
        Err(norn_projections::InteropError::WhitespaceCellId)
    ));
    assert!(matches!(
        ExportScope::new("tenant\u{7}cell"),
        Err(norn_projections::InteropError::ControlCharacterInCellId)
    ));
    assert!(matches!(
        ExportScope::new("tenant cell"),
        Err(norn_projections::InteropError::WhitespaceCellId)
    ));
}

#[test]
fn interop_exports_scope_hashing_is_delimiter_safe_and_preserves_exact_unicode() {
    // Naive concatenation with ':' would make these pairs collide:
    // ("tenant/a", "b:c") and ("tenant/a:b", "c").
    let first_scope = ExportScope::new("tenant/a").expect("valid cell id");
    let second_scope = ExportScope::new("tenant/a:b").expect("valid cell id");
    assert_ne!(
        first_scope.resource_id("b:c").expect("first id hashes"),
        second_scope.resource_id("c").expect("second id hashes")
    );

    let composed = ExportScope::new("caf\u{e9}").expect("valid composed id");
    let decomposed = ExportScope::new("cafe\u{301}").expect("valid decomposed id");
    assert_ne!(
        composed
            .resource_id("commit:main/3")
            .expect("composed id hashes"),
        decomposed
            .resource_id("commit:main/3")
            .expect("decomposed id hashes")
    );
}

/// The PROV-JSONLD schema's definitions carry JSON-pointer fragment `$id`s
/// (`#/definitions/DateTime`), which draft-07 does not admit as identifiers;
/// every `$ref` in the schema is a plain JSON pointer, so dropping those
/// `$id`s leaves the constraints unchanged.
fn prov_schema() -> Value {
    let mut schema: Value =
        serde_json::from_str(&interop_input("prov-jsonld.schema.json")).expect("schema is JSON");
    for definition in schema["definitions"]
        .as_object_mut()
        .expect("definitions")
        .values_mut()
    {
        if let Some(members) = definition.as_object_mut()
            && members
                .get("$id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with('#'))
        {
            members.remove("$id");
        }
    }
    schema
}

#[test]
fn interop_exports_prov_document_validates() {
    let input = input();
    let chain = chain(&input);
    let document = json_of(&prov_document(&chain));
    assert_eq!(
        errors(&prov_schema(), &document),
        Vec::<String>::new(),
        "{document:#}"
    );

    let graph = document["@graph"].as_array().expect("graph");
    assert_eq!(graph.len(), chain.nodes.len() + chain.edges.len());
    assert_eq!(
        document["@context"][0],
        "https://openprovenance.org/prov-jsonld/context.jsonld"
    );
    assert_eq!(document["@context"][1], json!({ "norn": "urn:norn:" }));
    assert!(
        document["@context"][2]["cell"]
            .as_str()
            .expect("cell IRI prefix")
            .starts_with("urn:norn:cell:")
    );
    assert!(
        graph
            .iter()
            .filter(|item| item.get("@id").is_some())
            .all(|item| item["@id"]
                .as_str()
                .is_some_and(|id| id.starts_with("cell:")))
    );

    // Every member name is a term of the vendored context (top level, or
    // scoped to the statement's @type), or a prefixed name.
    let context: Value = serde_json::from_str(&interop_input("prov-jsonld.context.jsonld"))
        .expect("context is JSON");
    let terms = context["@context"].as_object().expect("context terms");
    for statement in graph {
        let statement = statement.as_object().expect("statement");
        let kind = statement["@type"].as_str().expect("@type");
        let term = terms
            .get(kind)
            .unwrap_or_else(|| panic!("@type {kind} is a context term"));
        let scoped = term.get("@context").and_then(Value::as_object);
        for name in statement.keys() {
            let known = name.starts_with('@')
                || name.starts_with("norn:")
                || terms.contains_key(name)
                || scoped.is_some_and(|scoped| scoped.contains_key(name));
            assert!(known, "{kind}.{name} is defined by the PROV-JSONLD context");
        }
    }

    let has = |expected: Value| {
        assert!(graph.contains(&expected), "{expected:#} is in the document");
    };
    has(json!({
        "@type": "Generation",
        "entity": resource_id("fact:commerce.SalesOrder/1001@v2"),
        "activity": resource_id("commit:main/3"),
        "time": "2026-09-30T08:15:00Z",
        "type": ["norn:writtenBy"]
    }));
    has(json!({
        "@type": "Association",
        "activity": resource_id("commit:main/3"),
        "plan": resource_id("decision:credit.hold_policy@2")
    }));
    has(json!({
        "@type": "Usage",
        "activity": resource_id("commit:main/3"),
        "entity": resource_id("fact:crm.Customer/C-77@v1"),
        "role": ["norn:read"]
    }));
    has(json!({
        "@type": "Derivation",
        "generatedEntity": resource_id("observation:obs-erp-so-1001"),
        "usedEntity": resource_id("source:ERPNext/Sales Order/SO-1001"),
        "type": ["norn:observedFrom"]
    }));
    has(json!({
        "@type": "Influence",
        "influencee": resource_id("observation:obs-erp-so-1001"),
        "influencer": resource_id("object:technical.Mapping/erpnext.sales_order@3"),
        "type": ["norn:mappedBy"],
        "norn:transform": [{ "@value": "sales_order.header" }]
    }));
    has(json!({
        "@type": "Attribution",
        "entity": resource_id("object:technical.Source/ERPNext"),
        "agent": resource_id("object:technical.Owner/finance-ops"),
        "type": ["norn:owner"]
    }));
    has(json!({
        "@type": "Entity",
        "@id": resource_id("decision:credit.hold_policy@2"),
        "label": [{ "@value": "decision credit.hold_policy@2" }],
        "type": ["prov:Plan", "norn:Decision"]
    }));
    has(json!({
        "@type": "Activity",
        "@id": resource_id("commit:main/3"),
        "label": [{ "@value": "commit main/3 by credit.hold_policy@2" }],
        "startTime": "2026-09-30T08:15:00Z",
        "endTime": "2026-09-30T08:15:00Z",
        "type": ["norn:Commit"]
    }));
}

#[test]
fn interop_exports_are_read_only_and_deterministic() {
    // Exports take the chain by shared reference and return new JSON; the
    // lineage input and the chain are unchanged, and a second export is
    // identical.
    let input = input();
    let pristine_input = input.clone();
    let chain = chain(&input);
    let pristine_chain = chain.clone();
    let defs = artifact_set();
    let first = (
        openlineage_run_events(&chain),
        prov_document(&chain),
        dcat_catalog(&input.objects),
        skos_concept_scheme(&defs),
    );
    let second = (
        openlineage_run_events(&chain),
        prov_document(&chain),
        dcat_catalog(&input.objects),
        skos_concept_scheme(&defs),
    );
    assert_eq!(first, second);
    assert_eq!(input, pristine_input);
    assert_eq!(chain, pristine_chain);
}

#[test]
fn interop_exports_all_four_formats_are_unique_per_cell() {
    let input = input();
    let defs = artifact_set();
    let chain = chain(&input);
    let first_scope = ExportScope::new("tenant/a").expect("valid cell id");
    let second_scope = ExportScope::new("tenant/a:b").expect("valid cell id");

    let first_events: Vec<Value> = export_openlineage_run_events(&first_scope, &chain)
        .expect("first OpenLineage export succeeds")
        .iter()
        .map(json_of)
        .collect();
    let second_events: Vec<Value> = export_openlineage_run_events(&second_scope, &chain)
        .expect("second OpenLineage export succeeds")
        .iter()
        .map(json_of)
        .collect();
    assert_eq!(
        first_events,
        export_openlineage_run_events(&first_scope, &chain)
            .expect("same-cell OpenLineage export succeeds")
            .iter()
            .map(json_of)
            .collect::<Vec<_>>()
    );
    assert_ne!(first_events[0], second_events[0]);
    assert_ne!(
        first_events[0]["run"]["runId"],
        second_events[0]["run"]["runId"]
    );
    assert_ne!(
        first_events[0]["run"]["facets"]["nornCommit"]["commit"],
        second_events[0]["run"]["facets"]["nornCommit"]["commit"]
    );
    assert_ne!(
        first_events[0]["job"]["namespace"],
        second_events[0]["job"]["namespace"]
    );
    assert_ne!(
        datasets(&first_events[1], "inputs"),
        datasets(&second_events[1], "inputs")
    );

    let first_prov =
        json_of(&export_prov_document(&first_scope, &chain).expect("first PROV export succeeds"));
    let second_prov =
        json_of(&export_prov_document(&second_scope, &chain).expect("second PROV export succeeds"));
    assert_eq!(
        first_prov,
        json_of(
            &export_prov_document(&first_scope, &chain).expect("same-cell PROV export succeeds")
        )
    );
    assert_ne!(
        first_prov["@graph"][0]["@id"],
        second_prov["@graph"][0]["@id"]
    );

    let first_dcat = json_of(
        &norn_projections::dcat_catalog(&first_scope, &input.objects)
            .expect("first DCAT export succeeds"),
    );
    let second_dcat = json_of(
        &norn_projections::dcat_catalog(&second_scope, &input.objects)
            .expect("second DCAT export succeeds"),
    );
    assert_eq!(
        first_dcat,
        json_of(
            &norn_projections::dcat_catalog(&first_scope, &input.objects)
                .expect("same-cell DCAT export succeeds")
        )
    );
    assert_ne!(first_dcat["@id"], second_dcat["@id"]);
    assert_ne!(
        first_dcat["dcat:dataset"][0]["@id"],
        second_dcat["dcat:dataset"][0]["@id"]
    );

    let first_skos = json_of(
        &norn_projections::skos_concept_scheme(&first_scope, &defs)
            .expect("first SKOS export succeeds"),
    );
    let second_skos = json_of(
        &norn_projections::skos_concept_scheme(&second_scope, &defs)
            .expect("second SKOS export succeeds"),
    );
    assert_eq!(
        first_skos,
        json_of(
            &norn_projections::skos_concept_scheme(&first_scope, &defs)
                .expect("same-cell SKOS export succeeds")
        )
    );
    assert_ne!(
        first_skos["@graph"][0]["@id"],
        second_skos["@graph"][0]["@id"]
    );
}

const TECHNICAL: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/technical-view.object-types.json");
const BUSINESS: &str =
    include_str!("../../../fixtures/ir/entity-v2/technical-view/business.object-types.json");

fn interop_input(name: &str) -> String {
    let directory = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR")
        .map(std::path::PathBuf::from)
        .expect("acquire pinned interop input with scripts/importers/erpnext_runtime.py --output EXTERNAL_SCRATCH --interop-only; set NORN_UPSTREAM_FIXTURE_DIR")
        .canonicalize()
        .expect("external runtime input directory exists");
    let input = directory
        .join(name)
        .canonicalize()
        .expect("pinned interop input exists");
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    assert!(
        !input.starts_with(repo),
        "interop input must remain external"
    );
    let bytes = std::fs::read(input).expect("read pinned interop input");
    let lock: Value = serde_json::from_str(include_str!(
        "../../../scripts/importers/erpnext-runtime.lock.json"
    ))
    .expect("runtime input pins decode");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        lock["interop"][name]["sha256"],
        "interop input must match the acquisition pin"
    );
    String::from_utf8(bytes).expect("interop input is UTF-8")
}

fn artifact_set() -> Vec<norn_ir::ObjectTypeDef> {
    let mut defs: Vec<norn_ir::ObjectTypeDef> =
        serde_json::from_str(TECHNICAL).expect("technical view decodes");
    defs.extend(
        serde_json::from_str::<Vec<norn_ir::ObjectTypeDef>>(BUSINESS).expect("business decodes"),
    );
    defs
}

/// Every member name and `@type` value with one of `prefixes`, recursively.
fn prefixed_terms(value: &Value, prefixes: &[&str], out: &mut BTreeSet<String>) {
    let is_term = |text: &str| {
        prefixes
            .iter()
            .any(|prefix| text.starts_with(&format!("{prefix}:")))
    };
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                if is_term(name) {
                    out.insert(name.clone());
                }
                if name == "@type"
                    && let Some(kind) = member.as_str()
                    && is_term(kind)
                {
                    out.insert(kind.to_owned());
                }
                prefixed_terms(member, prefixes, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                prefixed_terms(item, prefixes, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Whether Turtle `vocabulary` declares `term` (a subject at line start).
fn turtle_declares(vocabulary: &str, term: &str) -> bool {
    vocabulary
        .lines()
        .any(|line| line == term || line.starts_with(&format!("{term} ")))
}

#[test]
fn interop_exports_dcat_catalog_uses_declared_dcat_terms() {
    let dcterms = interop_input("dcterms.ttl");
    let dcat = interop_input("dcat3.ttl");
    let catalog = json_of(&dcat_catalog(&input().objects));
    assert_eq!(catalog["@type"], "dcat:Catalog");
    assert!(
        catalog["@context"]["cell"]
            .as_str()
            .expect("cell IRI prefix")
            .starts_with("urn:norn:cell:")
    );
    assert!(dcat.contains(&format!(
        "@prefix dcat: <{}> .",
        catalog["@context"]["dcat"].as_str().expect("dcat prefix")
    )));
    assert!(dcterms.contains(&format!(
        "@prefix dcterms: <{}> .",
        catalog["@context"]["dcterms"].as_str().expect("dcterms prefix")
    )));

    let mut terms = BTreeSet::new();
    prefixed_terms(&catalog, &["dcat", "dcterms"], &mut terms);
    assert!(terms.contains("dcat:Dataset") && terms.contains("dcterms:isPartOf"));
    for term in &terms {
        let vocabulary = if term.starts_with("dcat:") {
            &dcat
        } else {
            &dcterms
        };
        assert!(turtle_declares(vocabulary, term), "{term} is declared");
    }

    assert_eq!(
        catalog["dcat:dataset"],
        json!([
            {
                "@id": resource_id("object:technical.Dataset/erpnext.prod"),
                "@type": "dcat:Dataset",
                "dcterms:identifier": "erpnext.prod",
                "dcterms:title": "erpnext.prod",
                "dcterms:type": { "@id": "norn:type:technical.Dataset" },
                "dcterms:source": { "@id": resource_id("object:technical.Source/ERPNext") }
            },
            {
                "@id": resource_id("object:technical.Table/erpnext.prod.tabCustomer"),
                "@type": "dcat:Dataset",
                "dcterms:identifier": "erpnext.prod.tabCustomer",
                "dcterms:title": "erpnext.prod.tabCustomer",
                "dcterms:type": { "@id": "norn:type:technical.Table" },
                "dcterms:isPartOf": { "@id": resource_id("object:technical.Dataset/erpnext.prod") }
            },
            {
                "@id": resource_id("object:technical.Table/erpnext.prod.tabSales Order"),
                "@type": "dcat:Dataset",
                "dcterms:identifier": "erpnext.prod.tabSales Order",
                "dcterms:title": "erpnext.prod.tabSales Order",
                "dcterms:type": { "@id": "norn:type:technical.Table" },
                "dcterms:isPartOf": { "@id": resource_id("object:technical.Dataset/erpnext.prod") }
            }
        ])
    );
}

#[test]
fn interop_exports_skos_scheme_uses_declared_skos_terms() {
    let scheme = json_of(&skos_concept_scheme(&artifact_set()));
    let skos = interop_input("skos.rdf");
    assert!(skos.contains(&format!(
        "xmlns:skos=\"{}\"",
        scheme["@context"]["skos"].as_str().expect("skos prefix")
    )));
    let mut terms = BTreeSet::new();
    prefixed_terms(&scheme, &["skos"], &mut terms);
    for term in &terms {
        let local = term.trim_start_matches("skos:");
        assert!(
            skos.contains(&format!("rdf:about=\"#{local}\"")),
            "{term} is declared"
        );
    }

    let graph = scheme["@graph"].as_array().expect("graph");
    let ids: Vec<String> = graph
        .iter()
        .map(|item| item["@id"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(
        ids,
        [
            resource_id("scheme:crm"),
            resource_id("scheme:commerce"),
            resource_id("type:crm.Customer"),
            resource_id("type:commerce.SalesOrder"),
        ],
        "business concepts only: no technical-view type"
    );
    let order = &graph[3];
    assert_eq!(order["skos:prefLabel"], "Sales Order");
    assert_eq!(order["skos:altLabel"], "SalesOrder");
    assert_eq!(order["skos:notation"], "commerce.SalesOrder");
    assert_eq!(
        order["skos:inScheme"],
        json!({ "@id": resource_id("scheme:commerce") })
    );
    assert_eq!(
        order["skos:related"],
        json!([{ "@id": resource_id("type:crm.Customer") }]),
        "business links only: no backed_by/derived_from"
    );
    assert_eq!(
        order["skos:definition"],
        "A sales order that a credit decision may put on hold."
    );
    assert!(graph[2].get("skos:altLabel").is_none());
}

#[test]
fn interop_exports_all_four_from_one_artifact_set() {
    let input = input();
    let defs = artifact_set();
    let pristine = (input.clone(), defs.clone());
    let chain = chain(&input);
    assert_eq!(openlineage_run_events(&chain).len(), 3);
    assert!(
        json_of(&prov_document(&chain))["@graph"]
            .as_array()
            .is_some_and(|graph| !graph.is_empty())
    );
    assert!(
        json_of(&dcat_catalog(&input.objects))["dcat:dataset"]
            .as_array()
            .is_some_and(|datasets| !datasets.is_empty())
    );
    assert!(
        json_of(&skos_concept_scheme(&defs))["@graph"]
            .as_array()
            .is_some_and(|graph| !graph.is_empty())
    );
    assert_eq!((input, defs), pristine, "exports never change their source");
}
