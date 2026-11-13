#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::unwrap_in_result,
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

/// Real-world SQL the dialect has no typed operator for. Each one refuses
/// `derived.validation.sql_unsupported` at the dialect boundary; none is read
/// as something close to what was written.
const OUTSIDE_THE_DIALECT: &[&str] = &[
    // Statements other than one query.
    "INSERT INTO \"shop.Order\" (amount) VALUES (1)",
    "UPDATE \"shop.Order\" SET amount = 0",
    "DELETE FROM \"shop.Order\"",
    "DROP TABLE \"shop.Order\"",
    "CREATE TABLE copy AS SELECT o.amount AS amount FROM \"shop.Order\" o",
    "SELECT o.amount AS amount FROM \"shop.Order\" o; DROP TABLE \"shop.Order\"",
    "EXPLAIN SELECT o.amount AS amount FROM \"shop.Order\" o",
    "COPY \"shop.Order\" TO '/tmp/out.csv'",
    "ATTACH 'other.db' AS other",
    "INSTALL httpfs",
    "PRAGMA table_info('shop.Order')",
    "",
    "-- only a comment",
    "SELECT 1",
    // External reads and table functions.
    "SELECT * FROM read_csv('/etc/passwd')",
    "SELECT * FROM read_parquet('s3://bucket/orders.parquet')",
    "SELECT * FROM generate_series(1, 10)",
    // Subqueries and common table expressions.
    "WITH recent AS (SELECT o.id AS id FROM \"shop.Order\" o) SELECT id FROM recent",
    "WITH RECURSIVE n AS (SELECT 1 AS k UNION ALL SELECT k + 1 FROM n) SELECT k FROM n",
    "SELECT o.amount AS amount FROM (SELECT * FROM \"shop.Order\") o",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.customer IN (SELECT c.id FROM \"shop.Customer\" c)",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE EXISTS (SELECT 1 FROM \"shop.Customer\" c WHERE c.id = o.customer)",
    "SELECT (SELECT MAX(c.region) FROM \"shop.Customer\" c) AS top FROM \"shop.Order\" o",
    // Joins other than one declared equality.
    "SELECT o.amount AS amount FROM \"shop.Order\" o RIGHT JOIN \"shop.Customer\" c ON o.customer = c.id",
    "SELECT o.amount AS amount FROM \"shop.Order\" o FULL OUTER JOIN \"shop.Customer\" c ON o.customer = c.id",
    "SELECT o.amount AS amount FROM \"shop.Order\" FULL JOIN \"shop.Customer\" c ON c.id = c.id",
    "SELECT o.amount AS amount FROM \"shop.Order\" o CROSS JOIN \"shop.Customer\" c",
    "SELECT o.amount AS amount FROM \"shop.Order\" o NATURAL JOIN \"shop.Customer\" c",
    "SELECT o.amount AS amount FROM \"shop.Order\" o, \"shop.Customer\" c WHERE o.customer = c.id",
    "SELECT o.amount AS amount FROM \"shop.Order\" o JOIN \"shop.Customer\" c USING (id)",
    "SELECT o.amount AS amount FROM \"shop.Order\" o JOIN \"shop.Customer\" c ON o.customer = c.id AND c.region = 'north'",
    "SELECT o.amount AS amount FROM \"shop.Order\" o JOIN \"shop.Customer\" c ON o.customer <> c.id",
    "SELECT o.amount AS amount FROM \"shop.Order\" o CROSS JOIN LATERAL (SELECT 1 AS one) x",
    // Grouping other than named keys.
    "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY 1",
    "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY 'x'",
    "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY ROLLUP (o.note)",
    "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY CUBE (o.note)",
    "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY GROUPING SETS ((o.note), ())",
    "SELECT o.note AS note, COUNT(*) AS n FROM \"shop.Order\" o GROUP BY o.note HAVING COUNT(*) > 1",
    "SELECT o.note AS note, AVG(o.amount) AS mean FROM \"shop.Order\" o GROUP BY o.note",
    "SELECT o.note AS note, SUM(DISTINCT o.amount) AS total FROM \"shop.Order\" o GROUP BY o.note",
    "SELECT o.note AS note, COUNT(*) FILTER (WHERE o.amount > 0) AS n FROM \"shop.Order\" o GROUP BY o.note",
    // Row order and row sets.
    "SELECT o.amount AS amount FROM \"shop.Order\" o ORDER BY o.amount",
    "SELECT o.amount AS amount FROM \"shop.Order\" o LIMIT 10",
    "SELECT o.amount AS amount FROM \"shop.Order\" o OFFSET 5",
    "SELECT o.amount AS amount FROM \"shop.Order\" o FETCH FIRST 10 ROWS ONLY",
    "SELECT TOP 10 o.amount AS amount FROM \"shop.Order\" o",
    "SELECT DISTINCT o.note AS note FROM \"shop.Order\" o",
    "SELECT DISTINCT ON (o.note) o.note AS note FROM \"shop.Order\" o",
    "SELECT o.note AS note FROM \"shop.Order\" o UNION SELECT l.source AS note FROM \"shop.Lead\" l",
    "SELECT o.note AS note FROM \"shop.Order\" o INTERSECT SELECT l.source AS note FROM \"shop.Lead\" l",
    "SELECT o.note AS note FROM \"shop.Order\" o EXCEPT SELECT l.source AS note FROM \"shop.Lead\" l",
    "SELECT o.note AS note FROM \"shop.Order\" o UNION ALL SELECT l.source AS note FROM \"shop.Lead\" l ORDER BY note",
    // Windows other than the fixed frames.
    "SELECT o.id AS id, SUM(o.amount) OVER (ORDER BY o.id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS s FROM \"shop.Order\" o",
    "SELECT o.id AS id, SUM(o.amount) OVER (ORDER BY o.id RANGE UNBOUNDED PRECEDING) AS s FROM \"shop.Order\" o",
    "SELECT o.id AS id, RANK() OVER w AS r FROM \"shop.Order\" o WINDOW w AS (ORDER BY o.id)",
    "SELECT o.id AS id, NTILE(4) OVER (ORDER BY o.id) AS q FROM \"shop.Order\" o",
    "SELECT o.id AS id, FIRST_VALUE(o.amount) OVER (ORDER BY o.id) AS f FROM \"shop.Order\" o",
    "SELECT o.id AS id, ROW_NUMBER() OVER (ORDER BY o.id NULLS FIRST) AS r FROM \"shop.Order\" o",
    "SELECT o.id AS id, ROW_NUMBER() OVER (ORDER BY 1) AS r FROM \"shop.Order\" o",
    "SELECT o.id AS id FROM \"shop.Order\" o QUALIFY ROW_NUMBER() OVER (ORDER BY o.id) = 1",
    // Expressions whose SQL meaning the plan would not keep.
    "SELECT o.amount / 2 AS half FROM \"shop.Order\" o",
    "SELECT o.amount % 2 AS odd FROM \"shop.Order\" o",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.note = NULL",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE NULL <> o.note",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.note LIKE 'a%'",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.note ILIKE 'a%'",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.amount BETWEEN 1 AND 2",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.note IN ('a', 'b')",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.note SIMILAR TO 'a'",
    "SELECT CAST(o.amount AS INTEGER) AS whole FROM \"shop.Order\" o",
    "SELECT o.amount::INTEGER AS whole FROM \"shop.Order\" o",
    "SELECT EXTRACT(YEAR FROM o.placed_at) AS year FROM \"shop.Order\" o",
    "SELECT o.note || 'x' AS tagged FROM \"shop.Order\" o",
    "SELECT UPPER(o.note) AS loud FROM \"shop.Order\" o",
    "SELECT NOW() AS at FROM \"shop.Order\" o",
    "SELECT CURRENT_DATE AS today FROM \"shop.Order\" o",
    "SELECT o.placed_at + INTERVAL '1 day' AS next FROM \"shop.Order\" o",
    "SELECT CASE o.note WHEN 'a' THEN 1 ELSE 0 END AS flag FROM \"shop.Order\" o",
    "SELECT CASE WHEN o.amount > 1 THEN 1 WHEN o.amount > 0 THEN 2 ELSE 0 END AS band FROM \"shop.Order\" o",
    "SELECT CASE WHEN o.amount > 1 THEN 1 END AS band FROM \"shop.Order\" o",
    "SELECT 1e3 AS big FROM \"shop.Order\" o",
    "SELECT 0x10 AS hex FROM \"shop.Order\" o",
    "SELECT 1.2.3 AS bad FROM \"shop.Order\" o",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.amount > $1",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.amount > ?",
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.amount > :min",
    "SELECT o.amount AS amount /* a comment */ FROM \"shop.Order\" o",
    "SELECT `o`.amount AS amount FROM \"shop.Order\" o",
    // Names that would splice into a ZEN path.
    "SELECT o.\"amount > 0 or true\" AS x FROM \"shop.Order\" o",
    "SELECT o.amount AS \"the amount\" FROM \"shop.Order\" o",
    "SELECT o.amount AS amount FROM \"shop.Order\" AS \"o o\"",
    "SELECT o.amount AS montant_€ FROM \"shop.Order\" o",
    // ZEN strings take no escapes: a text holding both quote kinds has no literal.
    "SELECT o.amount AS amount FROM \"shop.Order\" o WHERE o.note = 'say \"it''s\"'",
];

#[test]
fn real_world_sql_outside_the_dialect_refuses_cleanly() {
    for sql in OUTSIDE_THE_DIALECT {
        let found = compile(sql).expect_err(sql);
        assert!(
            !found.is_empty() && found.iter().all(|d| d.code == SQL_UNSUPPORTED),
            "{sql}: expected only {SQL_UNSUPPORTED}, found {found:#?}"
        );
    }
}
