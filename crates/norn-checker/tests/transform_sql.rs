#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]

//! VIEW-SQL-1: SQL is an authoring surface. It lowers to the closed operator
//! plan, the checker types that plan like any other, and anything without a
//! typed operator is refused rather than run.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::transform::eval::{InputRow, TransformInput, evaluate};
use norn_checker::transform::sql::{SQL_UNSUPPORTED, SqlMeta, sql_artifact};
use norn_checker::transform::{TransformPlan, admit};
use norn_ir::{ArtifactEnvelope, Decimal, RuntimeValue};
use serde_json::{Value, json};

fn entity(name: &str, object_type: Value) -> ArtifactEnvelope {
    let mut object_type = object_type;
    object_type["name"] = json!(name);
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
        "namespace": "shop", "name": name, "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
        "provenance": {"source": "authoring_sdk"},
        "payload": {"entityVersion": "2.0", "objectType": object_type}
    }))
    .expect("entity")
}

fn pool() -> Vec<ArtifactEnvelope> {
    vec![
        entity(
            "shop.Customer",
            json!({
                "properties": [
                    {"id": "email", "type": {"kind": "text"}},
                    {"id": "region", "type": {"kind": "text"}}
                ],
                "identity": [{"name": "email", "properties": ["email"]}]
            }),
        ),
        entity(
            "shop.Order",
            json!({
                "properties": [
                    {"id": "placed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                    {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                    {"id": "rate", "type": {"kind": "decimal", "scale": 4}},
                    {"id": "note", "constraints": {"nullable": true}, "type": {"kind": "text"}}
                ],
                "links": [{"id": "customer", "target": "shop.Customer",
                    "cardinality": {"to": "one", "from": "many"}}]
            }),
        ),
        entity(
            "shop.Lead",
            json!({
                "properties": [
                    {"id": "email", "type": {"kind": "text"}},
                    {"id": "source", "type": {"kind": "text"}}
                ]
            }),
        ),
    ]
}

fn compile(sql: &str) -> Result<(Value, TransformPlan), Vec<norn_checker::Diagnostic>> {
    let pool = pool();
    let artifact = sql_artifact(
        sql,
        &SqlMeta {
            id: "shop.sql_view",
            title: "SQL view",
            version: "0.1.0",
            scope: json!({"orgId": "org_demo", "environmentId": "dev"}),
        },
        &pool,
        &BTreeSet::new(),
    )?;
    let envelope: ArtifactEnvelope = serde_json::from_value(artifact.clone()).expect("envelope");
    let mut all = pool;
    all.push(envelope.clone());
    let plan = admit(&envelope, &all, &BTreeSet::new())?;
    Ok((artifact, plan))
}

#[track_caller]
fn refused(sql: &str, code: &str) {
    let found = compile(sql).expect_err("refused");
    assert!(
        found.iter().any(|d| d.code == code),
        "expected {code}, found {found:#?}"
    );
}

fn num(text: &str) -> RuntimeValue {
    RuntimeValue::Number(Decimal::parse(text).expect("decimal"))
}

fn s(text: &str) -> RuntimeValue {
    RuntimeValue::String(text.to_owned())
}

fn row(id: &str, values: &[(&str, RuntimeValue)]) -> InputRow {
    InputRow {
        id: id.to_owned(),
        version: 1,
        values: values
            .iter()
            .map(|(n, v)| ((*n).to_owned(), v.clone()))
            .collect(),
    }
}

#[test]
fn a_declared_link_join_grouped_by_week_lowers_admits_and_evaluates() {
    let (artifact, plan) = compile(
        "SELECT c.region AS region, DATE_TRUNC('week', o.placed_at) AS week, SUM(o.amount) AS revenue, COUNT(*) AS orders
         FROM \"shop.Order\" o LEFT JOIN \"shop.Customer\" c ON o.customer = c.id
         WHERE o.amount > 0
         GROUP BY c.region, DATE_TRUNC('week', o.placed_at)",
    )
    .expect("admitted");
    let nodes = &artifact["payload"]["calc"]["nodes"];
    assert!(
        nodes
            .as_object()
            .expect("nodes")
            .values()
            .any(|n| n["op"] == "join" && n["on"]["link"]["link"] == "customer")
    );
    assert_eq!(plan.output_schema().grain, ["region", "week"]);
    let input = TransformInput {
        rows: BTreeMap::from([
            (
                "o".to_owned(),
                vec![
                    row(
                        "o1",
                        &[
                            ("placed_at", s("2026-11-02T10:00:00Z")),
                            ("amount", num("10.00")),
                            ("rate", num("1")),
                        ],
                    ),
                    row(
                        "o2",
                        &[
                            ("placed_at", s("2026-11-03T10:00:00Z")),
                            ("amount", num("5.00")),
                            ("rate", num("1")),
                        ],
                    ),
                    row(
                        "o3",
                        &[
                            ("placed_at", s("2026-11-03T10:00:00Z")),
                            ("amount", num("0")),
                            ("rate", num("1")),
                        ],
                    ),
                ],
            ),
            (
                "c".to_owned(),
                vec![row("c1", &[("email", s("a@x")), ("region", s("north"))])],
            ),
        ]),
        links: BTreeMap::from([(
            ("o".to_owned(), "customer".to_owned()),
            BTreeMap::from([
                ("o1".to_owned(), vec!["c1".to_owned()]),
                ("o2".to_owned(), vec!["c1".to_owned()]),
            ]),
        )]),
    };
    let out = evaluate(&plan, &input).expect("evaluated");
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].values["region"], s("north"));
    assert_eq!(out.rows[0].values["week"], s("2026-11-02"));
    assert_eq!(out.rows[0].values["revenue"], num("15"));
    assert_eq!(out.rows[0].values["orders"], num("2"));
}

#[test]
fn a_key_join_and_windows_lower_to_typed_operators() {
    compile(
        "SELECT l.email AS email, l.source AS source, c.region AS region
         FROM \"shop.Lead\" l LEFT JOIN \"shop.Customer\" c ON l.email = c.email",
    )
    .expect("key join on the customer's declared email key");
    let (_, plan) = compile(
        "SELECT o.amount AS amount, ROW_NUMBER() OVER (ORDER BY amount DESC, id) AS rank_in_all, SUM(amount) OVER (ORDER BY amount DESC, id) AS to_date
         FROM \"shop.Order\" o",
    )
    .expect("windows");
    assert!(plan.output_schema().column("to_date").is_some());
}

#[test]
fn what_has_no_typed_operator_is_refused_never_run() {
    // An ad-hoc join on arbitrary columns.
    refused(
        "SELECT o.amount AS amount FROM \"shop.Order\" o JOIN \"shop.Lead\" l ON o.note = l.source",
        SQL_UNSUPPORTED,
    );
    refused(
        "SELECT o.amount AS amount FROM \"shop.Order\" o ORDER BY o.amount",
        SQL_UNSUPPORTED,
    );
    refused(
        "SELECT DISTINCT o.note AS note FROM \"shop.Order\" o",
        SQL_UNSUPPORTED,
    );
    refused(
        "SELECT o.note AS note FROM \"shop.Order\" o UNION SELECT l.source AS note FROM \"shop.Lead\" l",
        SQL_UNSUPPORTED,
    );
    refused(
        "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY o.note HAVING COUNT(*) > 1",
        SQL_UNSUPPORTED,
    );
    refused(
        "SELECT o.amount AS amount FROM \"shop.Missing\" o",
        SQL_UNSUPPORTED,
    );
    refused(
        "SELECT UPPER(o.note) AS note FROM \"shop.Order\" o",
        SQL_UNSUPPORTED,
    );
    // The kernel's laws apply to SQL exactly as to an authored plan.
    refused(
        "SELECT o.note AS note, SUM(o.rate) AS total FROM \"shop.Order\" o GROUP BY o.note",
        "metric.validation.undeclared_additivity",
    );
    refused(
        "SELECT c.region AS region, SUM(c.region) AS total FROM \"shop.Customer\" c GROUP BY c.region",
        "derived.validation.type_mismatch",
    );
}
