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
    assert_eq!(
        result
            .loss_report
            .validate_inventory(&XsdReader.inventory(&files, &c))
            .len(),
        0
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
        assert_eq!(
            result
                .loss_report
                .validate_inventory(&XsdReader.inventory(&files, &synthetic(xml.as_bytes())))
                .len(),
            0
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
    assert_eq!(
        result
            .loss_report
            .validate_inventory(&XsdReader.inventory(&files, &c))
            .len(),
        0
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
        assert_ne!(findings, [], "unadmitted source {mutation}");
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
    assert_ne!(
        norn_checker::source_schema::check_source_schema(&schema),
        []
    );
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

fn refusal(bytes: &[u8]) -> String {
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes,
    }];
    let result = XsdReader.import(&files, &synthetic(bytes));
    assert!(
        result.source_schema.is_none(),
        "hostile input must be refused"
    );
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|d| d.code == "format.xsd.invalid")
        .expect("a typed format.xsd.invalid refusal");
    diagnostic.message.clone()
}

fn schema_with(body: &str) -> Vec<u8> {
    format!(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#)
        .into_bytes()
}

#[test]
fn xsd_oversized_file_is_refused_before_parsing() {
    // Not XML at all: a parse would fail differently, so the refusal shows the
    // size check comes first.
    let bytes = vec![0xff_u8; norn_formats::xsd::MAX_FILE_BYTES + 1];
    assert!(refusal(&bytes).starts_with("schema exceeds 16 MiB"));
}

#[test]
fn xsd_oversized_bundle_is_refused_before_parsing() {
    let chunk = vec![0xff_u8; 13 * 1024 * 1024];
    let paths: Vec<String> = (0..5)
        .map(|i| format!("https://example.test/synthetic-document/1.0/part{i}.xsd"))
        .collect();
    let files: Vec<XsdFile<'_>> = paths
        .iter()
        .map(|path| XsdFile {
            path,
            bytes: &chunk,
        })
        .collect();
    let mut config = synthetic(&chunk);
    config.admission.files = Some(
        paths
            .iter()
            .map(|path| norn_ir::OntologySourceFile {
                path: path.clone(),
                sha256: norn_formats::contract::sha256_hex(&chunk),
            })
            .collect(),
    );
    let result = XsdReader.import(&files, &config);
    assert!(result.source_schema.is_none());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "format.xsd.invalid" && d.message == "schema bundle exceeds 64 MiB"),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn xsd_depth_limit_is_a_typed_refusal() {
    let depth = norn_formats::xsd::MAX_DEPTH + 1;
    let body = format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
    assert_eq!(refusal(&schema_with(&body)), "XML depth limit");
}

#[test]
fn xsd_node_limit_is_a_typed_refusal() {
    let body = "<a/>".repeat(norn_formats::xsd::MAX_NODES);
    assert_eq!(refusal(&schema_with(&body)), "XML node limit");
}

#[test]
fn xsd_namespace_declarations_per_element_are_capped() {
    let declarations: String = (0..=norn_formats::xsd::MAX_DECLARATIONS_PER_ELEMENT)
        .map(|i| format!(r#" xmlns:p{i}="urn:p{i}""#))
        .collect();
    let body = format!("<a{declarations}/>");
    assert_eq!(refusal(&schema_with(&body)), "namespace binding limit");
}

#[test]
fn xsd_namespace_scopes_per_file_are_capped() {
    let body: String = (0..=norn_formats::xsd::MAX_SCOPES)
        .map(|i| format!(r#"<a xmlns:p="urn:p{i}"/>"#))
        .collect();
    assert_eq!(refusal(&schema_with(&body)), "namespace binding limit");
}

#[test]
fn xsd_unpinned_bytes_are_never_parsed() {
    // A well-formed schema with a wrong pin: a parse would inventory its nodes
    // and attributes, so an inventory of the file alone shows it never ran.
    let bytes = DOC;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes,
    }];
    let mut config = synthetic(bytes);
    config.admission.files.as_mut().unwrap()[0].sha256 = "0".repeat(64);
    let result = XsdReader.import(&files, &config);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.starts_with("unadmitted bytes"))
    );
    assert_eq!(XsdReader.inventory(&files, &config).len(), 1);
    assert_eq!(result.loss_report.entries.len(), 1);
}

#[test]
fn xsd_min_occurs_is_numeric_and_repeats_are_lists() {
    let xml = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:t" elementFormDefault="qualified">
      <xs:complexType name="R"><xs:sequence>
        <xs:element name="a" type="xs:string" minOccurs="00"/>
        <xs:element name="b" type="xs:string" minOccurs="+0"/>
        <xs:element name="c" type="xs:string" maxOccurs="unbounded"/>
        <xs:element name="d" type="xs:string"/>
      </xs:sequence></xs:complexType></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let schema = XsdReader
        .import(&files, &synthetic(xml))
        .source_schema
        .unwrap();
    let field = |local: &str| {
        schema
            .fields
            .iter()
            .find(|f| f.name.contains(&format!("}}{local}@")))
            .unwrap()
    };
    assert_eq!(field("a").required, Some(false));
    assert_eq!(field("b").required, Some(false));
    assert_eq!(field("d").required, Some(true));
    use norn_ir::source_schema::NormalizedType::{Json, String as Text};
    assert_eq!(
        field("c").type_candidate.normalized,
        Json,
        "a repeating string is a list"
    );
    assert_eq!(field("d").type_candidate.normalized, Text);
    let repeating = norn_formats::xsd::repeating_fields(&schema);
    assert!(repeating.iter().any(|(_, name)| name.contains("}c@")));
    assert_eq!(repeating.len(), 1);
}

#[test]
fn xsd_reserved_namespaces_may_be_named_by_ordinary_attributes() {
    let xml = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:import namespace="http://www.w3.org/XML/1998/namespace"/><xs:complexType name="R"/></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let result = XsdReader.import(&files, &synthetic(xml));
    assert_eq!(result.diagnostics, [], "{:?}", result.diagnostics);
    assert!(result.source_schema.is_some());
}

#[test]
fn xsd_nillable_elements_are_never_non_null() {
    let xml = br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:t" elementFormDefault="qualified"><xs:complexType name="R"><xs:sequence><xs:element name="v" type="xs:string" nillable="true"/></xs:sequence></xs:complexType></xs:schema>"#;
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes: xml,
    }];
    let schema = XsdReader
        .import(&files, &synthetic(xml))
        .source_schema
        .unwrap();
    assert_eq!(schema.fields[0].required, Some(false));
}

#[test]
fn xsd_bindings_in_scope_are_capped() {
    let declare = |tag: &str, prefix: &str| -> String {
        (0..64)
            .map(|i| format!(r#" xmlns:{prefix}{i}="urn:{tag}{i}""#))
            .collect()
    };
    // 1 (xml) + 64 + 64 = 129 bindings in scope, with no element over its own cap.
    let body = format!("<a{}><b{}/></a>", declare("a", "a"), declare("b", "b"));
    assert_eq!(refusal(&schema_with(&body)), "namespace binding limit");
}

#[test]
fn xsd_namespace_binding_bytes_are_capped() {
    // 2,200 declaring elements, each with one 500-byte URI: every count cap
    // and the URI length cap pass, and the 1 MiB byte cap refuses.
    let uri = "u".repeat(500);
    let children: String = (0..2200)
        .map(|i| format!(r#"<b xmlns:q="urn:{uri}{i}"/>"#))
        .collect();
    assert_eq!(refusal(&schema_with(&children)), "namespace binding limit");
}

#[test]
fn xsd_namespace_uri_length_is_capped() {
    let uri = "u".repeat(norn_formats::xsd::MAX_URI_BYTES + 1);
    let body = format!(r#"<a xmlns:p="{uri}"/>"#);
    assert_eq!(refusal(&schema_with(&body)), "namespace binding limit");
    let target = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="{uri}"><xs:complexType name="R"/></xs:schema>"#
    );
    assert_eq!(refusal(target.as_bytes()), "namespace binding limit");
}

#[test]
fn xsd_expanded_name_bytes_are_capped() {
    // 200,000 elements in one 500-byte namespace: ~1.6 MiB of input would
    // expand to ~100 MiB of names. Refused before the names are built.
    let uri = "u".repeat(500);
    let xml = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:p="{uri}"><xs:annotation><xs:appinfo>{}</xs:appinfo></xs:annotation></xs:schema>"#,
        "<p:a/>".repeat(200_000)
    );
    assert_eq!(refusal(xml.as_bytes()), "XML expanded-name limit");
}

#[test]
fn xsd_shadowed_redeclarations_count_against_the_in_scope_cap() {
    // 255 nested elements each redeclaring p0..p63: no new prefix after the
    // first, so only counting redeclarations keeps a lookup's walk short.
    let declarations: String = (0..64)
        .map(|i| format!(r#" xmlns:p{i}="urn:p{i}""#))
        .collect();
    let body = format!(
        "{}{}",
        format!("<a{declarations}>").repeat(255),
        "</a>".repeat(255)
    );
    assert_eq!(refusal(&schema_with(&body)), "namespace binding limit");
}

#[test]
fn xsd_output_carries_each_nodes_own_declarations_only() {
    // 60 root prefixes with 500-byte URIs, then 20,000 elements: writing each
    // node's in-scope set would emit ~600 MB; own declarations keep it small.
    let uri = "u".repeat(480);
    let root: String = (0..60)
        .map(|i| format!(r#" xmlns:p{i}="urn:{uri}{i}""#))
        .collect();
    let xml = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"{root}>{}<xs:complexType name="R"/></xs:schema>"#,
        "<a/>".repeat(20_000)
    );
    let bytes = xml.as_bytes();
    let files = [XsdFile {
        path: "https://example.test/synthetic-document/1.0/profile.xsd",
        bytes,
    }];
    let result = XsdReader.import(&files, &synthetic(bytes));
    assert_eq!(result.diagnostics, [], "{:?}", result.diagnostics);
    let output = result.canonical_json().unwrap();
    assert!(
        output.len() < 32 * 1024 * 1024,
        "output {} bytes",
        output.len()
    );
}

#[test]
fn xsd_duplicate_global_simple_types_are_refused() {
    let body = r#"<xs:simpleType name="T"><xs:restriction base="xs:string"/></xs:simpleType><xs:simpleType name="T"><xs:restriction base="xs:token"/></xs:simpleType>"#;
    assert!(refusal(&schema_with(body)).starts_with("duplicate global type"));
}
