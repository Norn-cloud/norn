//! Mapping obligations must bite: schema compatibility, instance validation
//! and mapping totality each fire on a planted defect and stay silent on a
//! clean exchange (norn#62, norn#63 lane D).

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::DiagnosticSeverity;
use norn_checker::consistency::{
    Atom, ChaseBudget, Dependency, Egd, Exchange, Fact, RuleOrigin, Term, Tgd, Theory, Value,
    check_instance, check_mapping_totality, check_schema_compatibility, codes,
};

fn var(name: &str) -> Term {
    Term::Var(name.to_owned())
}

fn konst(text: &str) -> Term {
    Term::Const(text.to_owned())
}

fn atom(relation: &str, terms: &[Term]) -> Atom {
    Atom {
        relation: relation.to_owned(),
        terms: terms.to_vec(),
    }
}

fn origin(id: &str) -> RuleOrigin {
    RuleOrigin {
        artifact_id: Some(format!("artifact.{id}")),
        pointer: format!("/rules/{id}"),
    }
}

fn tgd(id: &str, body: &[Atom], head: &[Atom]) -> Dependency {
    Dependency::Tgd(Tgd {
        id: id.to_owned(),
        origin: origin(id),
        body: body.to_vec(),
        head: head.to_vec(),
    })
}

fn egd(id: &str, body: &[Atom], left: Term, right: Term) -> Dependency {
    Dependency::Egd(Egd {
        id: id.to_owned(),
        origin: origin(id),
        body: body.to_vec(),
        left,
        right,
    })
}

fn fact(relation: &str, values: &[&str]) -> Fact {
    Fact {
        relation: relation.to_owned(),
        values: values
            .iter()
            .map(|v| Value::Const((*v).to_owned()))
            .collect(),
    }
}

fn schema() -> BTreeMap<String, Vec<String>> {
    [
        ("SInvoice", vec!["id", "customer", "status"]),
        ("SLine", vec!["id", "invoice"]),
    ]
    .into_iter()
    .map(|(relation, columns)| {
        (
            relation.to_owned(),
            columns.into_iter().map(str::to_owned).collect(),
        )
    })
    .collect()
}

/// A source system with a customer key and a line → invoice foreign key.
fn source() -> Theory {
    Theory {
        dependencies: vec![
            egd(
                "source.invoice_key",
                &[
                    atom("SInvoice", &[var("i"), var("c1"), var("s1")]),
                    atom("SInvoice", &[var("i"), var("c2"), var("s2")]),
                ],
                var("c1"),
                var("c2"),
            ),
            tgd(
                "source.line_fk",
                &[atom("SLine", &[var("l"), var("i")])],
                &[atom("SInvoice", &[var("i"), var("c"), var("s")])],
            ),
        ],
    }
}

fn mapping() -> Theory {
    Theory {
        dependencies: vec![
            tgd(
                "map.invoice",
                &[atom("SInvoice", &[var("i"), var("c"), var("s")])],
                &[atom("Invoice", &[var("i"), var("c")])],
            ),
            tgd(
                "map.line",
                &[atom("SLine", &[var("l"), var("i")])],
                &[atom("LineOf", &[var("l"), var("i")])],
            ),
        ],
    }
}

/// The canon: exactly one customer per invoice; a line's invoice exists.
fn target() -> Theory {
    Theory {
        dependencies: vec![
            egd(
                "canon.invoice_one_customer",
                &[
                    atom("Invoice", &[var("i"), var("c1")]),
                    atom("Invoice", &[var("i"), var("c2")]),
                ],
                var("c1"),
                var("c2"),
            ),
            tgd(
                "canon.line_has_invoice",
                &[atom("LineOf", &[var("l"), var("i")])],
                &[atom("Invoice", &[var("i"), var("c")])],
            ),
        ],
    }
}

fn without(theory: &Theory, rule: &str) -> Theory {
    Theory {
        dependencies: theory
            .dependencies
            .iter()
            .filter(|d| d.id() != rule)
            .cloned()
            .collect(),
    }
}

fn codes_of(diagnostics: &[norn_checker::Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[test]
fn a_clean_exchange_is_compatible_and_its_data_valid() {
    let (schema, source, mapping, target) = (schema(), source(), mapping(), target());
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    assert!(check_schema_compatibility(&exchange, ChaseBudget::default()).is_empty());
    let data = [
        fact("SInvoice", &["inv1", "acme", "paid"]),
        fact("SLine", &["l1", "inv1"]),
    ];
    assert!(check_instance(&exchange, &data, ChaseBudget::default()).is_empty());
}

#[test]
fn a_source_without_the_key_permits_what_the_canon_forbids_as_a_warning() {
    let (schema, mapping, target) = (schema(), mapping(), target());
    let source = without(&without(&source(), "source.invoice_key"), "source.line_fk");
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    let findings = check_schema_compatibility(&exchange, ChaseBudget::default());
    assert_eq!(
        codes_of(&findings),
        [codes::UNPRESERVED, codes::UNPRESERVED]
    );
    assert!(
        findings
            .iter()
            .all(|f| f.severity == DiagnosticSeverity::Warning)
    );
    assert!(
        findings[0]
            .message
            .contains("breaks canon.invoice_one_customer: SInvoice(_:n0, _:n1, _:n2), SInvoice(_:n0, _:n3, _:n4)"),
        "{}",
        findings[0].message
    );
    assert_eq!(
        findings[0].span.as_ref().unwrap().pointer,
        "/rules/canon.invoice_one_customer"
    );
    assert!(
        findings[1]
            .message
            .contains("breaks canon.line_has_invoice: SLine(_:n0, _:n1)")
    );
    // Overlap, not contradiction: one customer per invoice is still valid data.
    let data = [fact("SInvoice", &["inv1", "acme", "paid"])];
    assert!(check_instance(&exchange, &data, ChaseBudget::default()).is_empty());
}

#[test]
fn a_status_mapped_to_two_canon_states_is_incompatible() {
    let (schema, source) = (schema(), source());
    let mut mapping = mapping();
    for (id, state) in [("map.status_draft", "draft"), ("map.status_open", "open")] {
        mapping.dependencies.push(tgd(
            id,
            &[atom("SInvoice", &[var("i"), var("c"), var("s")])],
            &[atom("InvoiceState", &[var("i"), konst(state)])],
        ));
    }
    let mut target = target();
    target.dependencies.push(egd(
        "canon.one_state",
        &[
            atom("InvoiceState", &[var("i"), var("s1")]),
            atom("InvoiceState", &[var("i"), var("s2")]),
        ],
        var("s1"),
        var("s2"),
    ));
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    let findings = check_schema_compatibility(&exchange, ChaseBudget::default());
    // Lines imply an invoice through the foreign key, so both relations fail;
    // the same source data is also the witness that `canon.one_state` is not
    // guaranteed.
    assert_eq!(
        codes_of(&findings),
        [codes::INCOMPATIBLE, codes::INCOMPATIBLE, codes::UNPRESERVED]
    );
    assert_eq!(findings[0].severity, DiagnosticSeverity::Error);
    assert!(findings[2].message.contains("breaks canon.one_state"));
    let message = &findings[0].message;
    assert!(
        message.starts_with("no SInvoice tuple can be mapped"),
        "{message}"
    );
    assert!(
        message.contains("the rules [map.status_draft, map.status_open, canon.one_state]"),
        "{message}"
    );
    assert_eq!(
        findings[0].artifact_id.as_deref(),
        Some("artifact.canon.one_state")
    );
}

#[test]
fn data_that_breaks_either_system_is_an_instance_violation() {
    let (schema, mapping, target) = (schema(), mapping(), target());
    let keyless = without(&source(), "source.invoice_key");
    let exchange = Exchange {
        source_schema: &schema,
        source: &keyless,
        mapping: &mapping,
        target: &target,
    };
    let two_customers = [
        fact("SInvoice", &["inv1", "acme", "paid"]),
        fact("SInvoice", &["inv1", "globex", "paid"]),
        fact("SInvoice", &["inv2", "acme", "paid"]),
    ];
    let findings = check_instance(&exchange, &two_customers, ChaseBudget::default());
    assert_eq!(codes_of(&findings), [codes::INSTANCE_VIOLATION]);
    let message = &findings[0].message;
    assert!(
        message.contains("breaks canon.invoice_one_customer"),
        "{message}"
    );
    assert!(message.contains("\"inv1\", \"globex\""), "{message}");
    assert!(
        !message.contains("inv2"),
        "the witness is minimal: {message}"
    );

    let source = source();
    let exchange = Exchange {
        source: &source,
        ..exchange
    };
    let dangling = [fact("SLine", &["l1", "inv9"])];
    let findings = check_instance(&exchange, &dangling, ChaseBudget::default());
    assert_eq!(
        codes_of(&findings),
        [codes::INSTANCE_VIOLATION, codes::INSTANCE_VIOLATION]
    );
    assert!(findings[0].message.contains("breaks source.line_fk"));
    assert!(
        findings[1]
            .message
            .contains("breaks canon.line_has_invoice")
    );
}

#[test]
fn unmapped_relations_and_columns_are_reported_unless_declared_gaps() {
    let (source, mapping, target) = (source(), mapping(), target());
    let mut schema = schema();
    schema.insert("SNote".to_owned(), vec!["id".to_owned(), "text".to_owned()]);
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    let findings = check_mapping_totality(&exchange, &BTreeSet::new());
    let messages: Vec<&str> = findings.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "SInvoice.status never reaches the target",
            "source relation SNote is not mapped"
        ]
    );
    assert!(findings.iter().all(|f| f.code == codes::NON_TOTAL));
    let gaps = BTreeSet::from(["SNote".to_owned(), "SInvoice.status".to_owned()]);
    assert!(check_mapping_totality(&exchange, &gaps).is_empty());
}

#[test]
fn a_mapping_with_an_equation_is_refused() {
    let (schema, source, target) = (schema(), source(), target());
    let mut mapping = mapping();
    mapping.dependencies.push(egd(
        "map.bad",
        &[atom("SLine", &[var("l"), var("i")])],
        var("l"),
        var("i"),
    ));
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    assert_eq!(
        codes_of(&check_schema_compatibility(
            &exchange,
            ChaseBudget::default()
        )),
        [codes::MALFORMED]
    );
    assert_eq!(
        codes_of(&check_instance(&exchange, &[], ChaseBudget::default())),
        [codes::MALFORMED]
    );
    assert_eq!(
        codes_of(&check_mapping_totality(&exchange, &BTreeSet::new())),
        [codes::MALFORMED]
    );
}

#[test]
fn instance_validation_maps_the_source_as_its_equations_resolve_it() {
    // Sol R1 S1 on #70: `S(_:n0, "a")` is `S("a", "a")` under the source EGD,
    // which maps to `T("a")`, and the canon requires "b".
    let schema = BTreeMap::from([("S".to_owned(), vec!["x".to_owned(), "y".to_owned()])]);
    let source = Theory {
        dependencies: vec![egd(
            "source.diagonal",
            &[atom("S", &[var("x"), var("y")])],
            var("x"),
            var("y"),
        )],
    };
    let mapping = Theory {
        dependencies: vec![tgd(
            "map.s",
            &[atom("S", &[konst("a"), var("y")])],
            &[atom("T", &[var("y")])],
        )],
    };
    let target = Theory {
        dependencies: vec![egd(
            "canon.t_is_b",
            &[atom("T", &[var("y")])],
            var("y"),
            konst("b"),
        )],
    };
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    let data = [Fact {
        relation: "S".to_owned(),
        values: vec![Value::Null(0), Value::Const("a".to_owned())],
    }];
    let findings = check_instance(&exchange, &data, ChaseBudget::default());
    assert_eq!(codes_of(&findings), [codes::INSTANCE_VIOLATION]);
    assert!(
        findings[0].message.contains("breaks canon.t_is_b"),
        "{}",
        findings[0].message
    );
}

#[test]
fn an_unpreserved_witness_is_the_verified_source_instance() {
    // Sol R1 S2 on #70: the witness includes what the source constraints
    // add (`R`), so it is itself a valid source instance.
    let schema = BTreeMap::from([
        ("S".to_owned(), vec!["x".to_owned()]),
        ("R".to_owned(), vec!["x".to_owned()]),
    ]);
    let source = Theory {
        dependencies: vec![tgd(
            "source.s_is_r",
            &[atom("S", &[var("x")])],
            &[atom("R", &[var("x")])],
        )],
    };
    let mapping = Theory {
        dependencies: vec![tgd(
            "map.s",
            &[atom("S", &[var("x")])],
            &[atom("T", &[var("x")])],
        )],
    };
    let target = Theory {
        dependencies: vec![tgd(
            "canon.t_is_u",
            &[atom("T", &[var("x")])],
            &[atom("U", &[var("x")])],
        )],
    };
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    let findings = check_schema_compatibility(&exchange, ChaseBudget::default());
    assert_eq!(codes_of(&findings), [codes::UNPRESERVED]);
    assert!(
        findings[0]
            .message
            .ends_with("breaks canon.t_is_u: S(_:n0), R(_:n0)"),
        "{}",
        findings[0].message
    );
}

#[test]
fn join_columns_count_as_mapped() {
    // CodeRabbit on #70: `S(id, fk), R(fk, name) -> T(id, name)` uses both
    // `fk` columns to select what reaches the target.
    let schema = BTreeMap::from([
        ("S".to_owned(), vec!["id".to_owned(), "fk".to_owned()]),
        (
            "R".to_owned(),
            vec!["fk".to_owned(), "name".to_owned(), "note".to_owned()],
        ),
    ]);
    let (source, target) = (Theory::default(), Theory::default());
    let mapping = Theory {
        dependencies: vec![tgd(
            "map.join",
            &[
                atom("S", &[var("id"), var("fk")]),
                atom("R", &[var("fk"), var("name"), var("note")]),
            ],
            &[atom("T", &[var("id"), var("name")])],
        )],
    };
    let exchange = Exchange {
        source_schema: &schema,
        source: &source,
        mapping: &mapping,
        target: &target,
    };
    let messages: Vec<String> = check_mapping_totality(&exchange, &BTreeSet::new())
        .into_iter()
        .map(|f| f.message)
        .collect();
    assert_eq!(messages, ["R.note never reaches the target"]);
}
