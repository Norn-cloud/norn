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
fn a_group_key_aliased_onto_a_source_id_is_refused_never_grouped_by_the_id() {
    // #1706: the scan's `id` passed through in place of `id = input.region`,
    // so two customers in one region came back as two rows with `n = 1`.
    refused(
        "SELECT region AS id, COUNT(*) AS n FROM \"shop.Customer\" GROUP BY region",
        SQL_UNSUPPORTED,
    );
    // A joined source's id unaliased is named `id` too, never the base id.
    refused(
        "SELECT c.id, COUNT(*) AS n FROM \"shop.Order\" o JOIN \"shop.Customer\" c ON o.customer = c.id GROUP BY c.id",
        SQL_UNSUPPORTED,
    );
    // A group key named like a measure's own value column.
    refused(
        "SELECT o.note AS total_value, SUM(o.amount) AS total FROM \"shop.Order\" o GROUP BY o.note",
        SQL_UNSUPPORTED,
    );
    // Grouping by an id under its own name is still the id.
    compile("SELECT id, COUNT(*) AS n FROM \"shop.Customer\" GROUP BY id").expect("grouped by id");
}

#[test]
fn a_group_key_aliased_onto_another_input_column_groups_by_the_expression() {
    let (_, plan) =
        compile("SELECT region AS email, COUNT(*) AS n FROM \"shop.Customer\" c GROUP BY region")
            .expect("admitted");
    assert_eq!(plan.output_schema().grain, ["email"]);
    let customers = vec![
        row("c1", &[("email", s("a@x")), ("region", s("north"))]),
        row("c2", &[("email", s("b@x")), ("region", s("north"))]),
    ];
    let input = TransformInput {
        rows: BTreeMap::from([("c".to_owned(), customers)]),
        links: BTreeMap::new(),
    };
    let out = evaluate(&plan, &input).expect("evaluated");
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.rows[0].values["email"], s("north"));
    assert_eq!(out.rows[0].values["n"], num("2"));
}

#[test]
fn a_window_modifier_without_a_typed_field_is_refused_never_dropped() {
    // #1706: `DISTINCT` was dropped, so `[5, 5]` summed to 10.
    for sql in [
        "SELECT id, amount, SUM(DISTINCT amount) OVER (ORDER BY id) AS total FROM \"shop.Order\"",
        "SELECT id, LAG(DISTINCT amount) OVER (ORDER BY id) AS before FROM \"shop.Order\"",
        "SELECT id, ROW_NUMBER(*) OVER (ORDER BY id) AS n FROM \"shop.Order\"",
    ] {
        refused(sql, SQL_UNSUPPORTED);
    }
    compile("SELECT id, amount, SUM(amount) OVER (ORDER BY id) AS total FROM \"shop.Order\"")
        .expect("a running sum is still lowered");
}

#[test]
fn a_window_reads_a_column_only_where_it_carries_that_value() {
    // #1706 r1: `c.email` was read by its bare name, `email`, which holds the region.
    for sql in [
        "SELECT c.region AS email, LAG(c.email) OVER (ORDER BY c.id) AS prior FROM \"shop.Customer\" c",
        "SELECT c.region AS email, ROW_NUMBER() OVER (PARTITION BY c.email ORDER BY c.id) AS n FROM \"shop.Customer\" c",
        "SELECT c.region AS email, ROW_NUMBER() OVER (ORDER BY c.email, c.id) AS n FROM \"shop.Customer\" c",
    ] {
        refused(sql, SQL_UNSUPPORTED);
    }
    // The same source column under another name is read through that name.
    let (artifact, _) = compile(
        "SELECT c.region AS place, LAG(region) OVER (PARTITION BY c.region ORDER BY id) AS prior FROM \"shop.Customer\" c",
    )
    .expect("admitted");
    let window = artifact["payload"]["calc"]["nodes"]
        .as_object()
        .expect("nodes")
        .values()
        .find(|node| node["op"] == "window")
        .expect("a window node");
    assert_eq!(window["columns"][0]["column"], "place");
    assert_eq!(window["partitionBy"], json!(["place"]));
    assert_eq!(window["orderBy"][0]["column"], "id");
}

#[test]
fn a_source_column_is_never_read_through_another_sources_flattened_name() {
    // #1706 r2: `o.c_region` does not exist; flattened it was `c.region`.
    let joined = "FROM \"shop.Order\" o JOIN \"shop.Customer\" c ON o.customer = c.id";
    refused(
        &format!(
            "SELECT c.region AS place, LAG(o.c_region) OVER (ORDER BY o.id, c.id) AS prior {joined}"
        ),
        SQL_UNSUPPORTED,
    );
    refused(
        &format!("SELECT o.c_region AS place {joined}"),
        SQL_UNSUPPORTED,
    );
    let (artifact, _) = compile(&format!(
        "SELECT c.region AS place, LAG(c.region) OVER (ORDER BY o.id, c.id) AS prior {joined}"
    ))
    .expect("admitted");
    let window = artifact["payload"]["calc"]["nodes"]
        .as_object()
        .expect("nodes")
        .values()
        .find(|node| node["op"] == "window")
        .expect("a window node");
    assert_eq!(window["columns"][0]["column"], "place");
}

#[test]
fn a_joined_column_is_read_only_from_a_source_that_has_it() {
    // #1706 r3: Customer has no `at`; `placed."at"` flattened to Order's `placed_at`.
    let joined = "FROM \"shop.Order\" o JOIN \"shop.Customer\" placed ON o.customer = placed.id";
    refused(
        &format!("SELECT placed.\"at\" AS at_time {joined}"),
        SQL_UNSUPPORTED,
    );
    // Order's own `placed_at` is still read: no Customer column flattens onto it.
    let (_, plan) = compile(&format!(
        "SELECT o.placed_at AS placed_time, placed.region AS region {joined}"
    ))
    .expect("admitted");
    assert!(plan.output_schema().column("placed_time").is_some());
}

#[test]
fn an_unqualified_column_in_a_join_reads_the_one_source_that_has_it() {
    // #1706 r4: both sources have `id`; it silently read `o.id`.
    let joined = "FROM \"shop.Order\" o JOIN \"shop.Customer\" c ON o.customer = c.id";
    refused(&format!("SELECT id AS picked {joined}"), SQL_UNSUPPORTED);
    refused(
        &format!("SELECT missing AS picked {joined}"),
        SQL_UNSUPPORTED,
    );
    // Only Customer has `region`: unqualified, it is `c.region`.
    let (artifact, _) = compile(&format!("SELECT region AS place {joined}")).expect("admitted");
    let projected = artifact["payload"]["calc"]["nodes"]
        .as_object()
        .expect("nodes")
        .values()
        .find(|node| node["op"] == "project")
        .expect("a projection");
    assert!(
        projected["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .any(|column| column["name"] == "place" && column["expression"] == "input.c_region"),
        "{projected:#}"
    );
}

#[test]
fn a_join_on_a_declared_key_named_id_is_refused() {
    // #1706 r4: a key named `id` reads as the row id in the plan, so
    // `ON o.id = c.email` would join on the customer's id instead.
    let mut pool = pool();
    pool[0] = entity(
        "shop.Customer",
        json!({
            "properties": [
                {"id": "email", "type": {"kind": "text"}},
                {"id": "region", "type": {"kind": "text"}}
            ],
            "identity": [{"name": "id", "properties": ["email"]}]
        }),
    );
    let found = sql_artifact(
        "SELECT o.amount AS amount FROM \"shop.Order\" o JOIN \"shop.Customer\" c ON o.id = c.email",
        &SqlMeta {
            id: "shop.sql_view",
            title: "SQL view",
            version: "0.1.0",
            scope: json!({"orgId": "org_demo", "environmentId": "dev"}),
        },
        &pool,
        &BTreeSet::new(),
    )
    .expect_err("refused");
    assert!(
        found
            .iter()
            .any(|d| d.code == SQL_UNSUPPORTED && d.message.contains("named \"id\"")),
        "{found:#?}"
    );
}

/// Every query is refused with `SQL_UNSUPPORTED`.
#[track_caller]
fn assert_unsupported(queries: &[&str]) {
    for sql in queries {
        let found = compile(sql).expect_err("refused");
        assert!(
            found.iter().any(|d| d.code == SQL_UNSUPPORTED),
            "{sql}: {found:#?}"
        );
    }
}

#[test]
fn a_modifier_on_a_group_key_or_coalesce_is_refused_never_dropped() {
    assert_unsupported(&[
        // #1706 r1: `OVER`/`DISTINCT` on a calendar key lowered to plain weekly grouping.
        "SELECT COUNT(*) AS n FROM \"shop.Order\" o GROUP BY DATE_TRUNC('week', o.placed_at) OVER (ORDER BY o.id)",
        "SELECT COUNT(*) AS n FROM \"shop.Order\" o GROUP BY DATE_TRUNC(DISTINCT 'week', o.placed_at)",
        // #1706 r1: `DISTINCT` and `OVER` on COALESCE were dropped by the parser.
        "SELECT COUNT(*) AS n FROM \"shop.Customer\" c GROUP BY COALESCE(DISTINCT c.region, c.email)",
        "SELECT COALESCE(c.region, c.email) OVER (ORDER BY c.id) AS k FROM \"shop.Customer\" c",
    ]);
}

#[test]
fn a_group_key_or_column_selected_under_two_names_is_refused_never_dropped() {
    assert_unsupported(&[
        // #1706 r1: `b` was silently left out of the output.
        "SELECT region AS a, region AS b, COUNT(*) AS n FROM \"shop.Customer\" GROUP BY region",
        // #1706 r2: the same key under one name twice dropped the second `a`.
        "SELECT region AS a, region AS a, COUNT(*) AS n FROM \"shop.Customer\" GROUP BY region",
        "SELECT c.region AS a, c.email AS a FROM \"shop.Customer\" c",
    ]);
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

/// The stack the bound tests run the dialect on, in KiB: 1024, or
/// `SQL_STACK_KIB`, the same 1 MiB a Worker's Wasm stack has. CI runs these
/// tests in the dev profile, whose frames are about five times the release
/// and wasm32 builds': the deepest input the bounds admit needs about
/// 850 KiB here, 175 KiB in a native release build and under 100 KiB in the
/// wasm32 release build, so passing here covers those builds with margin.
fn stack_bytes() -> usize {
    std::env::var("SQL_STACK_KIB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1024)
        .saturating_mul(1024)
}

/// Runs `sql` through the dialect (parse, lower, admit) on a thread with the
/// bounded stack of [`stack_bytes`]; a stack overflow aborts the test binary.
fn on_a_small_stack(sql: String) -> Result<(), Vec<norn_checker::Diagnostic>> {
    std::thread::Builder::new()
        .stack_size(stack_bytes())
        .spawn(move || compile(&sql).map(|_| ()))
        .expect("spawn")
        .join()
        .expect("no panic")
}

fn refused_on_a_small_stack(sql: String) -> Vec<norn_checker::Diagnostic> {
    on_a_small_stack(sql).expect_err("refused")
}

#[track_caller]
fn refused_saying(found: &[norn_checker::Diagnostic], words: &str) {
    assert!(
        found
            .iter()
            .any(|d| d.code == SQL_UNSUPPORTED && d.message.contains(words)),
        "expected {SQL_UNSUPPORTED} saying {words:?}, found {found:#?}"
    );
}

fn filtered(condition: &str) -> String {
    format!("SELECT o.amount AS amount FROM \"shop.Order\" o WHERE {condition}")
}

#[test]
fn deeply_nested_parentheses_are_refused_by_the_depth_bound() {
    let condition = format!("{}o.amount > 0{}", "(".repeat(5000), ")".repeat(5000));
    refused_saying(
        &refused_on_a_small_stack(filtered(&condition)),
        "too deeply",
    );
}

#[test]
fn a_deep_not_chain_is_refused_by_the_depth_bound() {
    let condition = format!("{}o.amount > 0", "NOT ".repeat(3000));
    refused_saying(
        &refused_on_a_small_stack(filtered(&condition)),
        "too deeply",
    );
}

#[test]
fn a_deep_negation_chain_is_refused_by_the_depth_bound() {
    let condition = format!("{}o.amount > 0", "- ".repeat(5000));
    refused_saying(
        &refused_on_a_small_stack(filtered(&condition)),
        "too deeply",
    );
}

#[test]
fn a_long_operator_chain_is_refused_by_the_depth_bound() {
    let condition = format!("o.amount{} > 0", " + o.amount".repeat(500));
    refused_saying(
        &refused_on_a_small_stack(filtered(&condition)),
        "too deeply",
    );
    let condition = vec!["o.amount > 0"; 500].join(" AND ");
    refused_saying(
        &refused_on_a_small_stack(filtered(&condition)),
        "too deeply",
    );
}

#[test]
fn source_past_the_size_bound_is_refused_before_it_is_read() {
    let condition = format!("{}o.amount > 0{}", "(".repeat(40_000), ")".repeat(40_000));
    refused_saying(
        &refused_on_a_small_stack(filtered(&condition)),
        "bytes, more than",
    );
}

#[test]
fn nesting_within_the_bounds_still_compiles() {
    let condition = format!("{}o.amount > 0{}", "(".repeat(20), ")".repeat(20));
    compile(&filtered(&condition)).expect("admitted");
    compile(&filtered(&format!("{}o.amount > 0", "NOT ".repeat(20)))).expect("admitted");
    compile(&filtered(&vec!["o.amount > 0"; 60].join(" AND "))).expect("admitted");
}

/// The nested levels the dialect admits: an expression in a select item or
/// `WHERE` is the first, and each parenthesis, call argument, `CASE` branch,
/// window key, `NOT` or unary minus inside it is one more.
const NESTED_LEVELS: usize = 24;

/// `shape(levels)` is SQL whose deepest expression sits `levels` nested levels
/// down. At the bound it runs through the dialect on the bounded stack without
/// the depth refusal (whatever else refuses it is not the depth bound, and
/// the run must not overflow); one level past it, the depth bound refuses it.
#[track_caller]
fn holds_at_the_depth_bound(shape: impl Fn(usize) -> String) {
    if let Err(found) = on_a_small_stack(shape(NESTED_LEVELS)) {
        assert!(
            found.iter().all(|d| !d.message.contains("too deeply")),
            "{found:#?}"
        );
    }
    refused_saying(
        &refused_on_a_small_stack(shape(NESTED_LEVELS + 1)),
        "too deeply",
    );
}

/// `inner` wrapped `levels - 1` times by `wrap`, as the condition of a `WHERE`.
fn wrapped(levels: usize, wrap: impl Fn(String) -> String) -> String {
    let inner = (1..levels).fold("o.amount".to_owned(), |inner, _| wrap(inner));
    filtered(&format!("{inner} > 0"))
}

#[test]
fn nested_parentheses_hold_at_the_depth_bound() {
    holds_at_the_depth_bound(|levels| wrapped(levels, |inner| format!("({inner})")));
}

#[test]
fn nested_coalesce_calls_hold_at_the_depth_bound() {
    holds_at_the_depth_bound(|levels| wrapped(levels, |inner| format!("coalesce({inner}, 0)")));
}

#[test]
fn nested_case_branches_hold_at_the_depth_bound() {
    holds_at_the_depth_bound(|levels| {
        wrapped(levels, |inner| {
            format!("CASE WHEN o.amount > 0 THEN o.amount ELSE {inner} END")
        })
    });
    // A parenthesised ELSE spends two levels per CASE; an odd remainder is a
    // parenthesis.
    holds_at_the_depth_bound(|levels| {
        let cases = (levels - 1) / 2;
        let inner = (0..cases).fold(
            if (levels - 1) % 2 == 1 {
                "(o.amount)".to_owned()
            } else {
                "o.amount".to_owned()
            },
            |inner, _| format!("CASE WHEN o.amount > 0 THEN o.amount ELSE ({inner}) END"),
        );
        filtered(&format!("{inner} > 0"))
    });
}

#[test]
fn nested_window_keys_hold_at_the_depth_bound() {
    holds_at_the_depth_bound(|levels| {
        let key = (1..levels).fold("o.note".to_owned(), |inner, _| {
            format!("sum(o.amount) OVER (PARTITION BY {inner})")
        });
        format!("SELECT {key} AS w FROM \"shop.Order\" o")
    });
}

/// The tallest tree the dialect builds: each operator, comparison, call or
/// `CASE` is one taller than its tallest operand, and a parenthesis adds
/// nothing.
const MAX_HEIGHT: usize = 64;

/// `SELECT <e> AS x`, its expression built by `iterations` rounds of `round`.
fn selected(seed: &str, iterations: usize, round: impl Fn(String) -> String) -> String {
    let e = (0..iterations).fold(seed.to_owned(), |e, _| round(e));
    format!("SELECT {e} AS x FROM \"shop.Order\" o")
}

#[test]
fn a_flat_chain_holds_at_the_height_bound() {
    // `o.amount` and n chained additions are n + 1 tall; the comparison is one more.
    let chain = |n: usize| filtered(&format!("o.amount{} > 0", " + o.amount".repeat(n)));
    on_a_small_stack(chain(MAX_HEIGHT - 2)).expect("admitted at the bound");
    refused_saying(
        &refused_on_a_small_stack(chain(MAX_HEIGHT - 1)),
        "too deeply",
    );
}

#[test]
fn chains_under_parentheses_are_bounded_by_their_height() {
    // Each round parenthesises the expression so far and chains onto it: the
    // nesting stays shallow while the tree grows 60 taller per round.
    let sql = selected("1", 30, |e| format!("({e}){}", "+1".repeat(60)));
    assert!(sql.len() < 16_384, "{}", sql.len());
    refused_saying(&refused_on_a_small_stack(sql), "too deeply");
    // Within the nesting bound, it is the height that refuses it.
    refused_saying(
        &refused_on_a_small_stack(selected("1", 20, |e| format!("({e}){}", "+1".repeat(60)))),
        "built too deeply",
    );
    // Just under the bound the same shape is admitted: 9 rounds of 7 are 64 tall.
    let rounds = |n: usize| {
        selected("o.amount", n, |e| {
            format!("({e}){}", " + o.amount".repeat(7))
        })
    };
    on_a_small_stack(rounds(9)).expect("admitted at the bound");
    refused_saying(&refused_on_a_small_stack(rounds(10)), "built too deeply");
}

#[test]
fn wide_shallow_expressions_are_admitted() {
    // 64 leaves in a balanced tree is 7 tall, however many nodes it has.
    on_a_small_stack(selected("o.amount", 6, |e| format!("({e}) + ({e})")))
        .expect("a balanced tree is admitted");
    // Siblings each just under the bound are as tall as the tallest of them.
    let tall = format!("o.amount{}", " + o.amount".repeat(MAX_HEIGHT - 3));
    on_a_small_stack(format!(
        "SELECT ({tall}) * ({tall}) AS x, ({tall}) AS y FROM \"shop.Order\" o"
    ))
    .expect("tall siblings are admitted");
}

#[test]
fn the_tallest_tree_under_the_deepest_nesting_holds_on_the_bounded_stack() {
    // Every level parenthesises and chains once, and the innermost level is a
    // chain as long as the height bound leaves: the parse recurses its
    // deepest and the walks after it descend the tallest tree at once.
    // `o.amount` and k additions are k + 1 tall, each of the other levels
    // adds one, and the comparison one more.
    let chain = MAX_HEIGHT - NESTED_LEVELS - 1;
    let innermost = format!("o.amount{}", " + o.amount".repeat(chain));
    let condition = (1..NESTED_LEVELS).fold(innermost, |inner, _| format!("({inner}) + o.amount"));
    on_a_small_stack(filtered(&format!("{condition} > 0"))).expect("admitted at both bounds");
}
