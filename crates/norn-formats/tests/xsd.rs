#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration tests"
)]
mod support;
use norn_formats::{
    SchemaImporter,
    xsd::{XsdConfig, XsdFile, XsdReader},
};
use norn_ir::OntologySource;
/// A synthetic two-type document schema: a header type with a repeated
/// line, as business documents are shaped.
const DOC: &[u8] = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:test:doc" targetNamespace="urn:test:doc" elementFormDefault="qualified">
  <xs:complexType name="DocumentType"><xs:sequence><xs:element name="ID" type="xs:string"/><xs:element name="IssueDate" type="xs:date"/><xs:element name="Line" type="t:LineType" maxOccurs="unbounded"/></xs:sequence></xs:complexType>
  <xs:complexType name="LineType"><xs:sequence><xs:element name="ID" type="xs:string"/><xs:element name="Amount" type="xs:decimal"/></xs:sequence></xs:complexType>
</xs:schema>"#;
fn config() -> XsdConfig {
    let mut admission =
        serde_json::from_str::<OntologySource>(include_str!("fixtures/synthetic/source.json"))
            .unwrap();
    admission.files = Some(vec![norn_ir::OntologySourceFile {
        path: "https://example.test/synthetic-document/1.0/doc.xsd".into(),
        sha256: norn_formats::contract::sha256_hex(DOC),
    }]);
    XsdConfig {
        admission,
        observed_at: "2026-10-21T00:00:00Z".into(),
    }
}
#[test]
fn xsd_document_conformance() {
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/doc.xsd",
        bytes: DOC,
    }];
    let result = support::check_conformance(&XsdReader, &files, &config(), "document");
    let schema = result.source_schema.unwrap();
    assert_eq!(schema.datasets.len(), 2);
    assert!(schema.datasets[0].native_name.ends_with("}DocumentType"));
    assert!(schema.fields.iter().any(|f| f.name.contains("}Line@")));
    assert_eq!(result.loss_report.summary().unexplained, 0);
}
#[test]
fn xsd_unadmitted_bytes_fail_closed_with_complete_inventory() {
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/doc.xsd",
        bytes: DOC,
    }];
    let mut c = config();
    c.admission.files.as_mut().unwrap()[0].sha256 = "0".repeat(64);
    let result = XsdReader.import(&files, &c);
    assert!(result.source_schema.is_none());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "format.xsd.invalid")
    );
    assert!(
        result
            .loss_report
            .validate_inventory(&XsdReader.inventory(&files, &c))
            .is_empty()
    );
}
#[test]
fn xsd_dtd_rejected() {
    let bytes = b"<!DOCTYPE schema [<!ENTITY x 'secret'>]><schema xmlns='http://www.w3.org/2001/XMLSchema'/>";
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/test.xsd",
        bytes,
    }];
    let mut c = config();
    c.admission.files = Some(vec![norn_ir::OntologySourceFile {
        path: "https://example.test/synthetic-document/1.0/test.xsd".into(),
        sha256: norn_formats::contract::sha256_hex(bytes),
    }]);
    let result = XsdReader.import(&files, &c);
    assert!(result.source_schema.is_none());
}

fn synthetic(bytes: &[u8]) -> XsdConfig {
    let mut c = config();
    c.admission.files = Some(vec![norn_ir::OntologySourceFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd".into(),
        sha256: norn_formats::contract::sha256_hex(bytes),
    }]);
    c
}
#[test]
fn xsd_particles_facets_and_nil_are_preserved_without_float_rounding() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:test" targetNamespace="urn:test" elementFormDefault="qualified">
      <xs:simpleType name="Exact"><xs:restriction base="xs:decimal"><xs:fractionDigits value="18"/><xs:minInclusive value="9007199254740993.123456789012345678"/></xs:restriction></xs:simpleType>
      <xs:complexType name="Record"><xs:sequence><xs:element name="label" type="xs:string"/><xs:element name="flag" type="xs:boolean" minOccurs="0" nillable="true"/><xs:choice minOccurs="0" maxOccurs="unbounded"><xs:element name="a" type="t:Exact"/><xs:element name="b" type="xs:string"/></xs:choice></xs:sequence></xs:complexType>
    </xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let result = support::check_conformance(&XsdReader, &files, &synthetic(xml), "profile");
    let schema = result.source_schema.as_ref().unwrap();
    let ext = serde_json::to_value(schema.extensions.as_ref().unwrap()).unwrap();
    let profile = &ext["norn.formats.xsd/0.1.0"]["profile"];
    assert!(
        profile["facets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["lexicalValue"] == "9007199254740993.123456789012345678")
    );
    assert!(
        profile["particles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["kind"] == "choice" && p["maxOccurs"]["kind"] == "unbounded")
    );
    assert!(
        profile["particles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "flag" && p["minOccurs"] == "0" && p["nillable"] == true)
    );
    let artifact = serde_json::to_value(&result.layers[0].artifacts[0]).unwrap();
    let props = artifact["payload"]["objectType"]["properties"]
        .as_array()
        .unwrap();
    assert_eq!(props[0]["type"]["kind"], "text");
    assert_eq!(props[1]["type"]["kind"], "boolean");
    assert_eq!(props[3]["type"]["kind"], "json-ext");
}
#[test]
fn xsd_invalid_occurrence_and_unbound_qnames_are_rejected() {
    for xml in [
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="A"><xs:sequence><xs:element name="x" type="xs:string" minOccurs="2" maxOccurs="1"/></xs:sequence></xs:complexType></xs:schema>"#,
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="A"><xs:sequence><xs:element name="x" type="missing:Type"/></xs:sequence></xs:complexType></xs:schema>"#,
    ] {
        let files = [XsdFile {
            path: "https://example.test/synthetic-document/1.0/profile.xsd",
            bytes: xml.as_bytes(),
        }];
        let result = XsdReader.import(&files, &synthetic(xml.as_bytes()));
        assert!(result.source_schema.is_none());
        assert!(
            result
                .loss_report
                .validate_inventory(&XsdReader.inventory(&files, &synthetic(xml.as_bytes())))
                .is_empty()
        );
    }
}
#[test]
fn xsd_external_references_are_evidence_and_never_fetched() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:import namespace="urn:elsewhere" schemaLocation="https://unreachable.invalid/schema.xsd"/><xs:complexType name="Local"/></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let result = support::check_conformance(&XsdReader, &files, &synthetic(xml), "offline");
    assert!(
        result
            .loss_report
            .entries
            .iter()
            .any(|e| matches!(&e.disposition, norn_formats::Disposition::Deferred { .. }))
    );
}

#[test]
fn xsd_valid_sibling_inventory_survives_invalid_utf8() {
    let files = [
        XsdFile {
            path: "https://example.test/synthetic-document/1.0/doc.xsd",
            bytes: DOC,
        },
        XsdFile {
            path: "https://example.test/synthetic-document/1.0/invalid.xsd",
            bytes: &[0xff],
        },
    ];
    let mut c = config();
    c.admission
        .files
        .as_mut()
        .unwrap()
        .push(norn_ir::OntologySourceFile {
            path: "https://example.test/synthetic-document/1.0/invalid.xsd".into(),
            sha256: norn_formats::contract::sha256_hex(&[0xff]),
        });
    let result = XsdReader.import(&files, &c);
    assert!(result.source_schema.is_none());
    assert!(
        result
            .loss_report
            .validate_inventory(&XsdReader.inventory(&files, &c))
            .is_empty()
    );
    assert!(result.loss_report.entries.iter().any(|entry|matches!(&entry.element,norn_formats::contract::SourceElement::Dataset {dataset} if dataset.ends_with("}DocumentType"))));
}
#[test]
fn xsd_large_occurrences_remain_exact_lexical_values() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="R"><xs:sequence><xs:element name="x" type="xs:string" minOccurs="9007199254740993" maxOccurs="9007199254740994"/></xs:sequence></xs:complexType></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let result = support::check_conformance(&XsdReader, &files, &synthetic(xml), "large");
    let extensions =
        serde_json::to_value(result.source_schema.unwrap().extensions.unwrap()).unwrap();
    let particles = extensions["norn.formats.xsd/0.1.0"]["profile"]["particles"]
        .as_array()
        .unwrap();
    assert!(
        particles
            .iter()
            .any(|p| p["minOccurs"] == "9007199254740993"
                && p["maxOccurs"]["value"] == "9007199254740994")
    );
}

#[test]
fn xsd_annotation_attributes_are_not_schema_qnames() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:m="urn:metadata"><xs:annotation><xs:appinfo><m:codeList ref="https://example.test/list" type="https://example.test/type"/><xs:complexType name="NotADeclaration"/></xs:appinfo></xs:annotation><xs:complexType name="R"><xs:sequence><xs:element name="value" type=" xs:string "/></xs:sequence></xs:complexType></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let result = support::check_conformance(&XsdReader, &files, &synthetic(xml), "annotation");
    assert_eq!(result.source_schema.as_ref().unwrap().datasets.len(), 1);
    let output = result.canonical_json().unwrap();
    assert!(output.contains("https://example.test/list"));
    assert_eq!(
        result.source_schema.as_ref().unwrap().fields[0].native_type,
        "{http://www.w3.org/2001/XMLSchema}string"
    );
}
#[test]
fn xsd_canonical_checker_admission_rejects_mutable_or_unlicensed_sources() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="R"/></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    for mutation in 0..6 {
        let mut c = synthetic(xml);
        match mutation {
            0 => c.admission.release_id = "latest".into(),
            1 => c.admission.released_at = "2026-02-30".into(),
            2 => c.admission.retrieved_at = "tomorrow".into(),
            3 => c.admission.canonical_url = "https://".into(),
            4 => {
                c.admission.redistribution = norn_ir::SourceRedistribution::Vendored;
                c.admission.license = "unreviewed-source-license".into();
            }
            _ => c.admission.source_id = "source.Bad".into(),
        }
        let result = XsdReader.import(&files, &c);
        // Syntax compilation is not authority; checker admission is mandatory.
        let findings =
            norn_checker::source_schema::check_source_schema(&result.source_schema.unwrap());
        assert!(!findings.is_empty(), "unadmitted source {mutation}");
    }
}
#[test]
fn xsd_checker_rejects_missing_admission() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:complexType name="R"/></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let mut schema = XsdReader
        .import(&files, &synthetic(xml))
        .source_schema
        .unwrap();
    schema.extensions = None;
    assert!(!norn_checker::source_schema::check_source_schema(&schema).is_empty());
}

#[test]
fn xsd_nested_schema_examples_cannot_define_global_elements() {
    let xml=br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:test" targetNamespace="urn:test"><xs:annotation><xs:appinfo><xs:schema targetNamespace="urn:test"><xs:element name="External" type="https://example.test/not-a-qname"/></xs:schema></xs:appinfo></xs:annotation><xs:element name="External" type="xs:string"/><xs:complexType name="R"><xs:sequence><xs:element ref="t:External"/></xs:sequence></xs:complexType></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let result =
        support::check_conformance(&XsdReader, &files, &synthetic(xml), "nested_annotation");
    let schema = result.source_schema.unwrap();
    assert_eq!(schema.datasets.len(), 1);
    assert_eq!(schema.fields.len(), 1);
    assert_eq!(
        schema.fields[0].native_type,
        "{http://www.w3.org/2001/XMLSchema}string"
    );
}
