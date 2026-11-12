//! The OFBiz reader on authored toy fixtures: the output and its loss report
//! against goldens, the conformance rules every importer meets, and the
//! refusals (an unknown element, a pin mismatch).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "bounded fixture assertions"
)]

use std::collections::BTreeSet;
use std::path::Path;

use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_formats::contract::{DropReason, sha256_hex, sorted_keys};
use norn_formats::{Disposition, SchemaImport, SchemaImporter as _};
use norn_importers::ofbiz::{EXTENSION, refused};
use norn_importers::{OfbizConfig, OfbizFile, OfbizReader, Role};
use norn_ir::DiagnosticSeverity;

const UPDATE_ENV: &str = "NORN_IMPORTERS_UPDATE_GOLDENS";

fn fixture(relative: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(relative))
        .expect("fixture")
}

fn golden(relative: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(relative);
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "golden {relative} is stale; rerun with {UPDATE_ENV}=1 and review the diff\n----- ACTUAL -----\n{actual}"
    );
}

fn config(files: &[OfbizFile<'_>]) -> OfbizConfig {
    OfbizConfig {
        system: "Toy ledger".into(),
        repository: "example/toy-ledger".into(),
        revision: "0000000000000000000000000000000000000001".into(),
        observed_at: "2026-11-12T00:00:00Z".into(),
        licence: "Apache-2.0".into(),
        pins: files
            .iter()
            .map(|file| (file.path.to_owned(), sha256_hex(file.bytes)))
            .collect(),
    }
}

struct Toy(Vec<(&'static str, Role, Vec<u8>)>);

impl Toy {
    fn new() -> Self {
        Self(vec![
            ("entitydef/entitymodel.xml", Role::Entity, fixture("ofbiz/entitydef/entitymodel.xml")),
            ("entitydef/entitygroup.xml", Role::Group, fixture("ofbiz/entitydef/entitygroup.xml")),
            ("data/ToySeedData.xml", Role::Seed, fixture("ofbiz/data/ToySeedData.xml")),
            ("LICENSE", Role::Licence, fixture("ofbiz/LICENSE")),
            ("NOTICE", Role::Notice, fixture("ofbiz/NOTICE")),
        ])
    }

    fn files(&self) -> Vec<OfbizFile<'_>> {
        self.0
            .iter()
            .map(|(path, role, bytes)| OfbizFile { path, role: *role, bytes })
            .collect()
    }
}

fn loss_jsonl(import: &SchemaImport) -> String {
    import
        .loss_report
        .entries
        .iter()
        .map(|entry| {
            let value = sorted_keys(serde_json::to_value(entry).unwrap());
            format!("{}\n", serde_json::to_string(&value).unwrap())
        })
        .collect()
}

fn without_loss(import: &SchemaImport) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&import.canonical_json().unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("lossReport");
    format!("{}\n", serde_json::to_string_pretty(&sorted_keys(value)).unwrap())
}

#[test]
fn the_toy_model_imports_conformantly_and_matches_its_goldens() {
    let toy = Toy::new();
    let files = toy.files();
    let config = config(&files);
    let import = OfbizReader.import(&files, &config);
    // Deterministic: a second run is byte-identical.
    assert_eq!(import.canonical_json().unwrap(), OfbizReader.import(&files, &config).canonical_json().unwrap());
    assert!(!refused(&import), "{:#?}", import.diagnostics);
    // Every inventoried element has exactly one disposition, none unexplained,
    // all codes registered and every dataset and field accounted for.
    let inventory = OfbizReader.inventory(&files, &config);
    let mut findings = import.loss_report.validate_inventory(&inventory);
    findings.extend(import.loss_report.validate(&import.layers, import.source_schema.as_ref()));
    assert!(findings.is_empty(), "{}", findings.join("\n"));
    for entry in &import.loss_report.entries {
        if let Disposition::Dropped { reason, .. } = &entry.disposition {
            assert!(DropReason::ALL.contains(reason));
        }
    }
    // The SourceSchemaIR artifact checks clean.
    let schema = import.source_schema_envelope("toy").unwrap().unwrap();
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext { registry: &registry, base: &[], tenant: None };
    let diagnostics = check_artifact_set(&[schema], &context);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    golden("ofbiz.toy.json", &without_loss(&import));
    golden("ofbiz.toy.loss.jsonl", &loss_jsonl(&import));
}

#[test]
fn field_descriptors_carry_what_a_mapping_proposer_needs() {
    let toy = Toy::new();
    let files = toy.files();
    let import = OfbizReader.import(&files, &config(&files));
    let schema = import.source_schema.unwrap();
    let field = |dataset: &str, name: &str| {
        schema.fields.iter().find(|field| field.dataset == dataset && field.name == name).unwrap()
    };
    // Type, required and documentation.
    let opened = field("Book", "opened");
    assert_eq!(opened.native_type, "date-time");
    assert_eq!(opened.description.as_deref(), Some("When the book was opened."));
    assert_eq!(field("Book", "bookId").required, Some(true));
    assert_eq!(field("Book", "bookTypeId").required, Some(true));
    // A field an extension adds belongs to the entity.
    assert!(schema.fields.iter().any(|field| field.dataset == "Book" && field.name == "closedBy"));
    // Keys, references and observed code sets.
    let has = |id: &str| schema.constraints.iter().any(|constraint| constraint.id == id);
    assert!(has("Line:pk") && has("Line:unique:LINE_CODE") && has("Book:rel:0"));
    let types = schema.code_sets.iter().find(|set| set.id == "Book.bookTypeId").unwrap();
    assert_eq!(types.values.iter().map(|value| value.value.as_str()).collect::<Vec<_>>(), ["GENERAL", "SUB"]);
    // Status bindings are typed gaps that keep what is known.
    let extension = schema.extensions.as_ref().unwrap().get(EXTENSION).unwrap();
    let gap = extension["codeSetGaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gap| gap["dataset"] == "Book")
        .unwrap();
    assert_eq!(gap["kind"], "status-type-undeclared");
    assert_eq!(gap["field"], "statusId");
    assert_eq!(gap["title"], "Current");
    assert_eq!(gap["target"], "StatusItem");
    assert!(import.diagnostics.iter().any(|diagnostic| diagnostic.path.as_deref() == Some("Book.statusId")));
    // Lifecycle candidates per status type, bound to no field.
    let lifecycle = &extension["lifecycleCandidates"][0];
    assert_eq!(lifecycle["statusTypeId"], "BOOK_STATUS");
    assert_eq!(lifecycle["states"].as_array().unwrap().len(), 2);
    assert_eq!(lifecycle["transitions"][0]["from"], "BOOK_OPEN");
    // The notice's copyright line is attributed.
    assert_eq!(extension["notice"]["copyright"], "Toy Ledger, Copyright 2026 Norn test fixtures");
}

#[test]
fn an_element_the_reader_does_not_know_refuses_the_import() {
    let bytes = fixture("ofbiz-unknown/entitymodel.xml");
    let files = [OfbizFile { path: "entitymodel.xml", role: Role::Entity, bytes: &bytes }];
    let import = OfbizReader.import(&files, &config(&files));
    assert!(refused(&import));
    assert!(import.source_schema.is_none());
    let unexplained: Vec<String> = import
        .loss_report
        .entries
        .iter()
        .filter(|entry| entry.disposition == Disposition::Unexplained)
        .map(|entry| format!("{:?}", entry.element))
        .collect();
    // An unknown top-level element, an unknown field attribute, and an unknown
    // attribute of a closed-grammar element.
    assert_eq!(unexplained.len(), 3, "{unexplained:#?}");
    assert!(unexplained.iter().any(|element| element.contains("entity-macro")));
    assert!(unexplained.iter().any(|element| element.contains("attr:shiny")));
    assert!(unexplained.iter().any(|element| element.contains("@cascade")));
    assert!(import.diagnostics.iter().any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error));
}

#[test]
fn bytes_that_do_not_match_their_pin_are_never_parsed() {
    let toy = Toy::new();
    let files = toy.files();
    let mut config = config(&files);
    config.pins.insert("data/ToySeedData.xml".into(), "0".repeat(64));
    let import = OfbizReader.import(&files, &config);
    assert!(refused(&import));
    assert!(import.loss_report.entries.is_empty());
    let message = &import.diagnostics[0].message;
    assert!(message.contains("data/ToySeedData.xml"), "{message}");
    let mut config = self::config(&files);
    config.pins.insert("extra.xml".into(), "0".repeat(64));
    assert!(refused(&OfbizReader.import(&files, &config)));
}
