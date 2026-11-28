//! MANIFEST-LAWS-1 / LAW-04: batched per-row residue settlement for the `DuckDB` engine.

use std::collections::{BTreeMap, BTreeSet};

use duckdb::types::Value as Sql;
use duckdb::{Connection, params_from_iter};

use super::engine::{
    Outcome, dataset_table, db, finding_columns, held_values, quarantine_table, refusal,
    residue_table,
};
use super::{SettleRequest, TableSchema};

/// MANIFEST-LAWS-1 / LAW-04: record the per-row findings of the residue
/// rows up to `through` and remove those rows from the residue. A finding
/// must name a rule the engine left to that row, once, and match the
/// metadata the landing pinned for that rule.
pub(super) fn settle_rows(
    conn: &Connection,
    table: &TableSchema,
    request: &SettleRequest,
) -> Outcome<()> {
    let residue = residue_table(&table.name);
    if !request.findings.is_empty() {
        let mut statement = db(conn.prepare(&format!(
            "SELECT \":row\", \":rule\", count(*) FROM {residue} WHERE \":row\" <= ? GROUP BY \":row\", \":rule\""
        )))?;
        let residue_counts: BTreeMap<(u64, String), u64> = db(db(statement
            .query_map(params_from_iter([Sql::UBigInt(request.through)]), |row| {
                Ok(((row.get(0)?, row.get(1)?), row.get(2)?))
            }))?
        .collect::<Result<_, _>>())?;
        let checks: BTreeMap<_, _> = request
            .land
            .checks
            .iter()
            .map(|check| (check.rule.as_str(), check))
            .collect();
        let mut seen = BTreeSet::new();
        let mut validated = Vec::with_capacity(request.findings.len());
        for finding in &request.findings {
            let Some(check) = checks
                .get(finding.rule.as_str())
                .filter(|check| check.label == finding.label && check.holds == finding.holds)
            else {
                return Err(refusal(
                    "bulk.invalid_finding",
                    "the finding does not match the landed rule's label and hold",
                ));
            };
            let key = (finding.row, finding.rule.clone());
            if residue_counts.get(&key).copied().unwrap_or(0) != 1 || !seen.insert(key) {
                return Err(refusal(
                    "bulk.invalid_finding",
                    format!(
                        "row {} is not residue of that rule up to {}",
                        finding.row, request.through
                    ),
                ));
            }
            validated.push((finding.row, *check));
        }
        let quarantine = quarantine_table(&table.name);
        let dataset = dataset_table(&table.name);
        let columns = finding_columns(table);
        let held = held_values(table);
        let sep = if held.is_empty() { "" } else { ", " };
        // S3: stage all validated findings via Appender and insert in one
        // set-based join against the staged dataset, preserving finding order.
        db(conn.execute_batch(
            "CREATE OR REPLACE TABLE \":settle_stage\" \
             (\":ord\" UBIGINT, \":row\" UBIGINT, \":reason\" VARCHAR, \":rule\" VARCHAR, \":holds\" BOOLEAN)",
        ))?;
        let inserted: Outcome<()> = (|| {
            {
                let mut appender = db(conn.appender(":settle_stage"))?;
                for (ord, (finding_row, check)) in validated.iter().enumerate() {
                    db(appender.append_row(duckdb::appender_params_from_iter([
                        Sql::UBigInt(u64::try_from(ord).unwrap_or(u64::MAX)),
                        Sql::UBigInt(*finding_row),
                        Sql::Text(check.label.clone()),
                        Sql::Text(check.rule.clone()),
                        Sql::Boolean(check.holds),
                    ])))?;
                }
                db(appender.flush())?;
            }
            db(conn.execute(
                &format!(
                    "INSERT INTO {quarantine} ({columns}) \
                     SELECT s.\":row\", ?, s.\":reason\", s.\":rule\", s.\":holds\"{sep}{} \
                     FROM \":settle_stage\" AS s JOIN {dataset} AS d ON d.\":row\" = s.\":row\" \
                     ORDER BY s.\":ord\"",
                    held.join(", ")
                ),
                params_from_iter([Sql::Text(request.land.load.load.clone())]),
            ))?;
            Ok(())
        })();
        if inserted.is_ok() {
            db(conn.execute_batch("DROP TABLE \":settle_stage\""))?;
        } else {
            let _dropped = conn.execute_batch("DROP TABLE IF EXISTS \":settle_stage\"");
        }
        inserted?;
    }
    db(conn.execute(
        &format!("DELETE FROM {residue} WHERE \":row\" <= ?"),
        params_from_iter([Sql::UBigInt(request.through)]),
    ))?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "unit tests for batched DuckDB row settlement"
)]
mod tests {
    use super::*;
    use crate::analytics::engine::{dataset_ddl, in_transaction, quarantine_ddl};
    use crate::analytics::{
        Column, ColumnKind, FileFormat, FileSource, LandRequest, LoadRequest, RowFinding,
        RuleCheck, TenantId,
    };
    use crate::dataset_manifest::ManifestPins;

    fn sample_table() -> TableSchema {
        TableSchema {
            name: "t.Settle".into(),
            columns: vec![
                Column {
                    id: "amount".into(),
                    kind: ColumnKind::Decimal { scale: 3 },
                    nullable: true,
                    laws: Vec::new(),
                },
                Column {
                    id: "note".into(),
                    kind: ColumnKind::Text,
                    nullable: true,
                    laws: Vec::new(),
                },
                Column {
                    id: "flag".into(),
                    kind: ColumnKind::Boolean,
                    nullable: true,
                    laws: Vec::new(),
                },
            ],
            pin: "pin-1".into(),
        }
    }

    fn sample_request(
        table: &TableSchema,
        through: u64,
        findings: Vec<RowFinding>,
    ) -> SettleRequest {
        let tenant =
            TenantId::try_from("a".repeat(64)).expect("valid 64-char hex tenant identifier");
        SettleRequest {
            land: LandRequest {
                load: LoadRequest {
                    tenant,
                    table: table.clone(),
                    load: "load-42".into(),
                    file: FileSource {
                        url: String::new(),
                        uri: "s3://synthetic/settle.csv".into(),
                        sha256: "b".repeat(64),
                        format: FileFormat::Csv,
                    },
                    headers: BTreeMap::new(),
                },
                pins: ManifestPins {
                    definitions_version: 1,
                    model_hash: "m".into(),
                    binding: "b".into(),
                    mapping: "map".into(),
                    laws: "l".into(),
                },
                landing: "landing-1".into(),
                attempt: "a1".into(),
                checks: vec![
                    RuleCheck {
                        rule: "rule.positive".into(),
                        label: "amount must be positive".into(),
                        holds: true,
                        condition: None,
                        predicate: None,
                        expect: true,
                        fuel: 64,
                    },
                    RuleCheck {
                        rule: "rule.note".into(),
                        label: "note recommended".into(),
                        holds: false,
                        condition: None,
                        predicate: None,
                        expect: true,
                        fuel: 64,
                    },
                ],
                residue_page: 100,
            },
            through,
            findings,
        }
    }

    fn setup_conn(table: &TableSchema) -> Connection {
        let conn = Connection::open_in_memory().expect("duckdb");
        conn.execute_batch(&format!(
            "SET preserve_insertion_order=true; \
             CREATE TABLE {dataset} ({d_ddl}); \
             CREATE TABLE {quarantine} ({q_ddl}); \
             CREATE TABLE {residue} (\":row\" UBIGINT, \":rule\" VARCHAR); \
             INSERT INTO {dataset} (\":row\", \":id\", \":v\", \"amount\", \"note\", \"flag\") VALUES \
               (1, 'id-1', 1, CAST('1.500' AS DECIMAL(38,3)), 'alpha', true), \
               (2, 'id-2', 1, NULL, NULL, false), \
               (3, 'id-3', 1, CAST('20.000' AS DECIMAL(38,3)), 'gamma', NULL); \
             INSERT INTO {residue} VALUES \
               (1, 'rule.positive'), (1, 'rule.note'), \
               (2, 'rule.positive'), \
               (3, 'rule.positive');",
            dataset = dataset_table(&table.name),
            d_ddl = dataset_ddl(table),
            quarantine = quarantine_table(&table.name),
            q_ddl = quarantine_ddl(table),
            residue = residue_table(&table.name),
        ))
        .expect("setup tables");
        conn
    }

    type QuarantineRow = (
        u64,
        String,
        String,
        String,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    fn read_quarantine(conn: &Connection, table: &TableSchema) -> Vec<QuarantineRow> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT \":row\", \":load\", \":reason\", \":rule\", \":holds\", \"amount\", \"note\", \"flag\" \
                 FROM {}",
                quarantine_table(&table.name)
            ))
            .expect("prepare quarantine");
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .expect("query quarantine")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect quarantine")
    }

    #[test]
    fn batched_settle_preserves_order_nulls_decimal_trim_and_residue_boundary() {
        let table = sample_table();
        let conn = setup_conn(&table);
        let request = sample_request(
            &table,
            2,
            vec![
                RowFinding {
                    row: 2,
                    rule: "rule.positive".into(),
                    label: "amount must be positive".into(),
                    holds: true,
                },
                RowFinding {
                    row: 1,
                    rule: "rule.note".into(),
                    label: "note recommended".into(),
                    holds: false,
                },
                RowFinding {
                    row: 1,
                    rule: "rule.positive".into(),
                    label: "amount must be positive".into(),
                    holds: true,
                },
            ],
        );
        in_transaction(&conn, |conn| settle_rows(conn, &table, &request)).expect("settle");
        assert_eq!(
            read_quarantine(&conn, &table),
            vec![
                (
                    2,
                    "load-42".into(),
                    "amount must be positive".into(),
                    "rule.positive".into(),
                    true,
                    None,
                    None,
                    Some("false".into()),
                ),
                (
                    1,
                    "load-42".into(),
                    "note recommended".into(),
                    "rule.note".into(),
                    false,
                    Some("1.5".into()),
                    Some("alpha".into()),
                    Some("true".into()),
                ),
                (
                    1,
                    "load-42".into(),
                    "amount must be positive".into(),
                    "rule.positive".into(),
                    true,
                    Some("1.5".into()),
                    Some("alpha".into()),
                    Some("true".into()),
                ),
            ]
        );
        let remaining: Vec<(u64, String)> = conn
            .prepare(&format!(
                "SELECT \":row\", \":rule\" FROM {} ORDER BY \":row\", \":rule\"",
                residue_table(&table.name)
            ))
            .expect("prepare residue")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query residue")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect residue");
        assert_eq!(remaining, vec![(3, "rule.positive".into())]);
        let stage_exists: u64 = conn
            .query_row(
                "SELECT count(*) FROM information_schema.tables WHERE table_name = ':settle_stage'",
                [],
                |row| row.get(0),
            )
            .expect("check stage cleanup");
        assert_eq!(stage_exists, 0);
    }

    #[test]
    fn empty_settle_clears_residue_without_inserting_quarantine() {
        let table = sample_table();
        let conn = setup_conn(&table);
        let request = sample_request(&table, 2, Vec::new());
        in_transaction(&conn, |conn| settle_rows(conn, &table, &request)).expect("empty settle");
        assert!(read_quarantine(&conn, &table).is_empty());
        let remaining: u64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {}", residue_table(&table.name)),
                [],
                |row| row.get(0),
            )
            .expect("count residue");
        assert_eq!(remaining, 1);
    }

    #[test]
    fn invalid_findings_refuse_atomically() {
        let table = sample_table();
        let conn = setup_conn(&table);
        let cases = [
            // Row past `through`.
            vec![RowFinding {
                row: 3,
                rule: "rule.positive".into(),
                label: "amount must be positive".into(),
                holds: true,
            }],
            // Duplicate finding in same batch.
            vec![
                RowFinding {
                    row: 1,
                    rule: "rule.positive".into(),
                    label: "amount must be positive".into(),
                    holds: true,
                },
                RowFinding {
                    row: 1,
                    rule: "rule.positive".into(),
                    label: "amount must be positive".into(),
                    holds: true,
                },
            ],
            // Mismatched hold flag.
            vec![RowFinding {
                row: 1,
                rule: "rule.positive".into(),
                label: "amount must be positive".into(),
                holds: false,
            }],
            // Rule not left as residue for row 2.
            vec![RowFinding {
                row: 2,
                rule: "rule.note".into(),
                label: "note recommended".into(),
                holds: false,
            }],
        ];
        for findings in cases {
            let request = sample_request(&table, 2, findings);
            let err = in_transaction(&conn, |conn| settle_rows(conn, &table, &request))
                .expect_err("invalid finding must refuse");
            assert_eq!(err.code, "bulk.invalid_finding");
            assert!(read_quarantine(&conn, &table).is_empty());
            let remaining: u64 = conn
                .query_row(
                    &format!("SELECT count(*) FROM {}", residue_table(&table.name)),
                    [],
                    |row| row.get(0),
                )
                .expect("count residue");
            assert_eq!(remaining, 4);
        }
    }
}
