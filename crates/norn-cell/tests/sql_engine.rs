#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]
#![cfg(feature = "native")]

mod common;

use norn_cell::CellError;
use norn_cell::engine::{
    MAX_PARAMETERS, MAX_SAFE_INTEGER, MAX_STATEMENT_BYTES, NativeEngine, SqlEngine, SqlValue,
};

fn count(engine: &impl SqlEngine) -> i64 {
    engine
        .query_row("SELECT COUNT(*) FROM item", &[], |row| row.get(0))
        .expect("count")
}

#[test]
fn transaction_commits_or_rolls_back_the_same_engine() {
    let engine = NativeEngine::open_in_memory().expect("open");
    engine
        .execute_batch("CREATE TABLE item (id INTEGER PRIMARY KEY)")
        .expect("schema");
    let inserted = engine
        .transaction(|| {
            engine.execute("INSERT INTO item VALUES (?1)", &[SqlValue::Integer(1)])?;
            Ok::<_, CellError>(count(&engine))
        })
        .expect("commit");
    assert_eq!(inserted, 1);
    assert_eq!(count(&engine), 1);

    let failed = engine.transaction(|| {
        engine.execute("INSERT INTO item VALUES (?1)", &[SqlValue::Integer(2)])?;
        engine.execute("INSERT INTO item VALUES (?1)", &[SqlValue::Integer(1)])
    });
    assert!(matches!(failed, Err(CellError::Sqlite(_))));
    assert_eq!(count(&engine), 1);
}

#[test]
fn nested_transactions_refuse_and_leave_outer_atomic() {
    let engine = NativeEngine::open_in_memory().expect("open");
    engine
        .execute_batch("CREATE TABLE item (id INTEGER PRIMARY KEY)")
        .expect("schema");
    let failed = engine.transaction(|| {
        engine.execute("INSERT INTO item VALUES (1)", &[])?;
        engine.transaction(|| Ok::<_, CellError>(()))
    });
    assert!(matches!(failed, Err(CellError::Engine(_))));
    assert_eq!(count(&engine), 0);
    engine
        .transaction(|| engine.execute("INSERT INTO item VALUES (2)", &[]))
        .expect("engine remains usable");
    assert_eq!(count(&engine), 1);
}

#[test]
fn sql_cannot_commit_outside_the_transaction_closure() {
    let engine = NativeEngine::open_in_memory().expect("open");
    engine
        .execute_batch("CREATE TABLE item (id INTEGER PRIMARY KEY)")
        .expect("schema");
    for sql in ["COMMIT", "END", "ROLLBACK", "SAVEPOINT nested"] {
        let failed = engine.transaction(|| {
            engine.execute("INSERT INTO item VALUES (1)", &[])?;
            engine.execute(sql, &[])
        });
        assert!(failed.is_err(), "accepted transaction control: {sql}");
        assert_eq!(count(&engine), 0);
    }
    let failed = engine.transaction(|| engine.execute_batch("INSERT INTO item VALUES (1); COMMIT"));
    assert!(failed.is_err());
    assert_eq!(count(&engine), 0);
}

#[test]
fn unwinding_rolls_back_native_transaction() {
    let engine = NativeEngine::open_in_memory().expect("open");
    engine
        .execute_batch("CREATE TABLE item (id INTEGER PRIMARY KEY)")
        .expect("schema");
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "This regression deliberately unwinds the transaction; catch_unwind asserts rollback"
        )]
        let _: Result<(), CellError> = engine.transaction(|| {
            engine.execute("INSERT INTO item VALUES (1)", &[])?;
            panic!("simulated unwind");
        });
    }));
    assert!(unwound.is_err());
    assert_eq!(count(&engine), 0);
}

#[test]
fn integer_boundary_is_exact_in_both_directions() {
    let engine = NativeEngine::open_in_memory().expect("open");
    for integer in [-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER] {
        assert_eq!(
            engine
                .query_row("SELECT ?1", &[SqlValue::Integer(integer)], |row| {
                    row.get::<i64>(0)
                })
                .expect("exact boundary"),
            integer
        );
    }
    let too_large = MAX_SAFE_INTEGER.checked_add(1).expect("i64 fits");
    assert!(
        engine
            .query("SELECT ?1", &[SqlValue::Integer(too_large)])
            .is_err()
    );
    assert!(engine.query("SELECT 9007199254740993", &[]).is_err());
}

#[test]
fn native_engine_enforces_parameter_and_statement_limits() {
    let engine = NativeEngine::open_in_memory().expect("open");
    let sql = format!("SELECT {}", vec!["?"; MAX_PARAMETERS].join(", "));
    let params = vec![SqlValue::Null; MAX_PARAMETERS];
    assert_eq!(engine.query(&sql, &params).expect("100 bindings").len(), 1);
    let too_many = vec![SqlValue::Null; MAX_PARAMETERS.saturating_add(1)];
    assert!(engine.query(&sql, &too_many).is_err());

    let prefix = "SELECT 1";
    let sql = format!(
        "{prefix}{}",
        " ".repeat(MAX_STATEMENT_BYTES.saturating_sub(prefix.len()))
    );
    assert!(engine.query(&sql, &[]).is_ok());
    assert!(engine.query(&format!("{sql} "), &[]).is_err());
}

#[derive(Debug)]
struct MeasuredEngine {
    native: NativeEngine,
    largest_query: std::rc::Rc<std::cell::Cell<usize>>,
    query_calls: std::rc::Rc<std::cell::Cell<usize>>,
    unsafe_integer: std::rc::Rc<std::cell::Cell<bool>>,
    target_query_plans: Option<std::rc::Rc<std::cell::RefCell<Vec<String>>>>,
}

impl MeasuredEngine {
    fn note_values(&self, values: &[SqlValue]) {
        if values.iter().any(|value| {
            matches!(value, SqlValue::Integer(integer) if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(integer))
        }) {
            self.unsafe_integer.set(true);
        }
    }
}

impl SqlEngine for MeasuredEngine {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.note_values(params);
        self.native.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.query_calls
            .set(self.query_calls.get().saturating_add(1));
        self.note_values(params);
        if let Some(plans) = &self.target_query_plans
            && (sql.starts_with("SELECT octet_length(\"_source\")")
                || sql.starts_with("SELECT \"_source\"")
                || sql.contains("FROM \"_norn_ledger\"")
                || sql.starts_with("SELECT \"snapshot\", \"hash\", \"commit_seq\""))
        {
            for row in self
                .native
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)?
            {
                let Some(SqlValue::Text(detail)) = row.get(3) else {
                    return Err(CellError::Engine("query plan has no detail".to_owned()));
                };
                plans.borrow_mut().push(detail.clone());
            }
        }
        let rows = self.native.query(sql, params)?;
        if rows.iter().flatten().any(|value| {
            matches!(value, SqlValue::Integer(integer) if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(integer))
        }) {
            self.unsafe_integer.set(true);
        }
        self.largest_query
            .set(self.largest_query.get().max(rows.len()));
        Ok(rows)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.native.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.native.transaction(f)
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keeps target-scoped fanout and oversized identity preflights in one scenario"
)]
fn inverse_link_candidates_are_target_scoped_and_budgeted() {
    use norn_cell::reactive::{
        BudgetedIdentityRead, BudgetedLinksRead, DecodeBudget, ReactiveLimits,
    };
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD};
    use norn_ir::ResourceLimitProfile;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    let model = common::model_of(&[
        serde_json::from_value(json!({
            "name": "test.Source",
            "links": [{
                "id": "targets",
                "target": "test.Target",
                "cardinality": { "to": "many", "from": "many" },
                "inverse": "sources"
            }]
        }))
        .expect("source type"),
        serde_json::from_value(json!({ "name": "test.Target" })).expect("target type"),
    ]);
    let largest_query = Rc::new(std::cell::Cell::new(0));
    let query_calls = Rc::new(std::cell::Cell::new(0));
    let target_query_plans = Rc::new(std::cell::RefCell::new(Vec::new()));
    let engine = MeasuredEngine {
        native: NativeEngine::open_in_memory().expect("engine"),
        largest_query: Rc::clone(&largest_query),
        query_calls,
        unsafe_integer: Rc::new(std::cell::Cell::new(false)),
        target_query_plans: Some(Rc::clone(&target_query_plans)),
    };
    let mut store = CellStore::from_engine(engine, model).expect("store");
    let huge_source = format!("source-{}", "s".repeat(4_096));
    let huge_target = format!("target-{}", "t".repeat(4_096));
    let mut writes = vec![
        FactWriteRow::Put {
            object_type: "test.Source".to_owned(),
            key: "source".to_owned(),
            values: BTreeMap::new(),
        },
        FactWriteRow::Put {
            object_type: "test.Source".to_owned(),
            key: huge_source.clone(),
            values: BTreeMap::new(),
        },
        FactWriteRow::Put {
            object_type: "test.Target".to_owned(),
            key: "wanted".to_owned(),
            values: BTreeMap::new(),
        },
        FactWriteRow::Put {
            object_type: "test.Target".to_owned(),
            key: "other".to_owned(),
            values: BTreeMap::new(),
        },
        FactWriteRow::Put {
            object_type: "test.Target".to_owned(),
            key: huge_target.clone(),
            values: BTreeMap::new(),
        },
    ];
    let mut links = vec![LinkChange::Add {
        object_type: "test.Source".to_owned(),
        link: "targets".to_owned(),
        source: "source".to_owned(),
        target: "wanted".to_owned(),
        values: BTreeMap::new(),
        target_type: None,
    }];
    for index in 0..1_300 {
        let target = format!("unrelated-{index:04}");
        writes.push(FactWriteRow::Put {
            object_type: "test.Target".to_owned(),
            key: target.clone(),
            values: BTreeMap::new(),
        });
        links.push(LinkChange::Add {
            object_type: "test.Source".to_owned(),
            link: "targets".to_owned(),
            source: "source".to_owned(),
            target,
            values: BTreeMap::new(),
            target_type: None,
        });
    }
    links.extend([
        LinkChange::Add {
            object_type: "test.Source".to_owned(),
            link: "targets".to_owned(),
            source: huge_source,
            target: "other".to_owned(),
            values: BTreeMap::new(),
            target_type: None,
        },
        LinkChange::Add {
            object_type: "test.Source".to_owned(),
            link: "targets".to_owned(),
            source: "source".to_owned(),
            target: huge_target.clone(),
            values: BTreeMap::new(),
            target_type: None,
        },
    ]);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "test".to_owned(),
            writes,
            links,
            ..CommitBatch::default()
        })
        .expect("commit fanout fixture");

    largest_query.set(0);
    target_query_plans.borrow_mut().clear();
    let limits = ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1());
    let mut budget = DecodeBudget::new(&limits);
    let sources = store
        .read()
        .link_sources_to_with_budget(
            MAIN_WORLD,
            "test.Source",
            "targets",
            "wanted",
            1_024,
            &mut budget,
        )
        .expect("target-scoped read");
    assert_eq!(
        sources,
        BudgetedLinksRead::Complete(vec!["source".to_owned()])
    );
    assert_eq!(largest_query.get(), 1, "only target-matching rows are read");
    let plans = target_query_plans.borrow();
    assert_eq!(plans.len(), 2, "both target reads must have query plans");
    for detail in plans.iter() {
        assert!(
            detail.contains("/target") && detail.contains("(_world=? AND _target=?)"),
            "target lookup did not use the target index: {detail}"
        );
    }
    drop(plans);

    let bounded = ReactiveLimits {
        max_candidate_bytes_per_commit: 256,
        ..limits
    };
    let mut source_budget = DecodeBudget::new(&bounded);
    let source = store
        .read()
        .link_sources_to_with_budget(
            MAIN_WORLD,
            "test.Source",
            "targets",
            "other",
            1_024,
            &mut source_budget,
        )
        .expect("budgeted source identity preflight");
    assert_eq!(source, BudgetedLinksRead::Exhausted);

    let mut target_budget = DecodeBudget::new(&bounded);
    let target = store
        .read()
        .object_identity_with_budget(MAIN_WORLD, "test.Target", &huge_target, &mut target_budget)
        .expect("budgeted target identity preflight");
    assert_eq!(target, BudgetedIdentityRead::Exhausted);
}

#[test]
fn small_subtype_lists_exclude_siblings_in_bounded_queries() {
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD};
    use serde_json::json;

    let model = common::model_of(&[
        serde_json::from_value(json!({ "name": "test.Root" })).expect("root"),
        serde_json::from_value(json!({ "name": "test.Child", "extends": "test.Root" }))
            .expect("child"),
    ]);
    let largest_query = std::rc::Rc::new(std::cell::Cell::new(0));
    let query_calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let engine = MeasuredEngine {
        native: NativeEngine::open_in_memory().expect("engine"),
        largest_query: std::rc::Rc::clone(&largest_query),
        query_calls: std::rc::Rc::clone(&query_calls),
        unsafe_integer: std::rc::Rc::new(std::cell::Cell::new(false)),
        target_query_plans: None,
    };
    let mut store = CellStore::from_engine(engine, model).expect("store");
    let mut writes: Vec<_> = (0..260)
        .map(|id| FactWriteRow::Put {
            object_type: "test.Root".to_owned(),
            key: format!("sibling-{id:03}"),
            values: std::collections::BTreeMap::new(),
        })
        .collect();
    for key in ["z-child-1", "z-child-2", "z-child-3"] {
        writes.push(FactWriteRow::Put {
            object_type: "test.Child".to_owned(),
            key: key.to_owned(),
            values: std::collections::BTreeMap::new(),
        });
    }
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "test".to_owned(),
            writes,
            ..CommitBatch::default()
        })
        .expect("seed");
    largest_query.set(0);
    query_calls.set(0);
    let records = store
        .read()
        .list(MAIN_WORLD, "test.Child", 2)
        .expect("list");
    assert_eq!(
        records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["z-child-1", "z-child-2"]
    );
    assert!(largest_query.get() <= 2, "materialized an unbounded scan");
    assert_eq!(
        query_calls.get(),
        3,
        "list queried once, then hydrated each row"
    );
}

#[test]
fn subtype_list_chunks_large_descendant_sets_and_merges_ids_in_order() {
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD};
    use norn_ir::ObjectTypeDef;
    use serde_json::json;

    let mut definitions: Vec<ObjectTypeDef> =
        vec![serde_json::from_value(json!({ "name": "test.Root" })).expect("root type")];
    for index in 0..100 {
        definitions.push(
            serde_json::from_value::<ObjectTypeDef>(json!({
                "name": format!("test.Child{index:03}"),
                "extends": "test.Root"
            }))
            .expect("child type"),
        );
    }
    let model = common::model_of(&definitions);
    let largest_query = std::rc::Rc::new(std::cell::Cell::new(0));
    let query_calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let engine = MeasuredEngine {
        native: NativeEngine::open_in_memory().expect("engine"),
        largest_query: std::rc::Rc::clone(&largest_query),
        query_calls: std::rc::Rc::clone(&query_calls),
        unsafe_integer: std::rc::Rc::new(std::cell::Cell::new(false)),
        target_query_plans: None,
    };
    let mut store = CellStore::from_engine(engine, model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "test".to_owned(),
            writes: vec![
                FactWriteRow::Put {
                    object_type: "test.Child000".to_owned(),
                    key: "z-first-chunk".to_owned(),
                    values: std::collections::BTreeMap::new(),
                },
                FactWriteRow::Put {
                    object_type: "test.Child098".to_owned(),
                    key: "b-second-chunk".to_owned(),
                    values: std::collections::BTreeMap::new(),
                },
                FactWriteRow::Put {
                    object_type: "test.Child099".to_owned(),
                    key: "a-second-chunk".to_owned(),
                    values: std::collections::BTreeMap::new(),
                },
            ],
            ..CommitBatch::default()
        })
        .expect("seed descendants");
    largest_query.set(0);
    query_calls.set(0);

    let records = store
        .read()
        .list(MAIN_WORLD, "test.Root", 2)
        .expect("list descendants");
    assert_eq!(
        records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["a-second-chunk", "b-second-chunk"]
    );
    assert!(
        largest_query.get() <= 2,
        "materialized more than the requested rows"
    );
    assert_eq!(
        query_calls.get(),
        4,
        "two type chunks plus two row hydrations"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keeps full-range storage, read, dump, and corruption checks in one scenario"
)]
fn full_range_integers_round_trip_without_crossing_the_seam_as_integers() {
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD, StoreOptions};
    use norn_ir::RuntimeValue as Value;
    use norn_projections::sqlite_ddl::{
        ID_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, WORLD_COLUMN, quote_identifier,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    let model = common::model_of(&[serde_json::from_value(json!({
        "name": "test.Item",
        "properties": [{ "id": "amount", "type": { "kind": "integer" } }],
        "links": [{
            "id": "next",
            "target": "test.Item",
            "cardinality": { "to": "many", "from": "many" },
            "properties": [{ "id": "weight", "type": { "kind": "integer" } }]
        }]
    }))
    .expect("item type")]);
    let object_table = model
        .schema()
        .object_table("test.Item")
        .expect("object table")
        .table
        .clone();
    let link_table = model
        .schema()
        .link_table("test.Item", "next")
        .expect("link table")
        .table
        .clone();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let unsafe_integer = std::rc::Rc::new(std::cell::Cell::new(false));
    let engine = MeasuredEngine {
        native: NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
        largest_query: std::rc::Rc::new(std::cell::Cell::new(0)),
        query_calls: std::rc::Rc::new(std::cell::Cell::new(0)),
        unsafe_integer: std::rc::Rc::clone(&unsafe_integer),
        target_query_plans: None,
    };
    let mut store = CellStore::from_engine(engine, model.clone()).expect("store");
    let maximum = Value::from_json_text("9223372036854775807").expect("i64 max");
    let minimum = Value::from_json_text("-9223372036854775808").expect("i64 min");
    let beyond_safe = Value::from_json_text("9007199254740993").expect("beyond safe int");
    let values = |value: &Value| BTreeMap::from([("amount".to_owned(), value.clone())]);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "test".to_owned(),
            writes: vec![
                FactWriteRow::Put {
                    object_type: "test.Item".to_owned(),
                    key: "source".to_owned(),
                    values: values(&maximum),
                },
                FactWriteRow::Put {
                    object_type: "test.Item".to_owned(),
                    key: "target".to_owned(),
                    values: values(&minimum),
                },
                FactWriteRow::Put {
                    object_type: "test.Item".to_owned(),
                    key: "middle".to_owned(),
                    values: values(&beyond_safe),
                },
            ],
            links: vec![LinkChange::Add {
                object_type: "test.Item".to_owned(),
                link: "next".to_owned(),
                source: "source".to_owned(),
                target: "target".to_owned(),
                values: BTreeMap::from([("weight".to_owned(), maximum.clone())]),
                target_type: None,
            }],
            ..CommitBatch::default()
        })
        .expect("commit");

    let source = store
        .read()
        .object(MAIN_WORLD, "test.Item", "source")
        .expect("read source")
        .expect("source exists");
    assert_eq!(source.values.get("amount"), Some(&maximum));
    let listed = store
        .read()
        .list(MAIN_WORLD, "test.Item", 3)
        .expect("list items");
    assert_eq!(
        listed
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["middle", "source", "target"]
    );
    assert_eq!(
        listed
            .iter()
            .find(|record| record.id == "source")
            .and_then(|record| record.values.get("amount")),
        Some(&maximum)
    );
    assert_eq!(
        listed
            .iter()
            .find(|record| record.id == "target")
            .and_then(|record| record.values.get("amount")),
        Some(&minimum)
    );
    assert_eq!(
        listed
            .iter()
            .find(|record| record.id == "middle")
            .and_then(|record| record.values.get("amount")),
        Some(&beyond_safe)
    );
    let links = store
        .read()
        .links(MAIN_WORLD, "test.Item", "next", "source")
        .expect("read link");
    assert_eq!(
        links.first().and_then(|link| link.values.get("weight")),
        Some(&maximum)
    );

    let dump = store.read().dump().expect("dump");
    let object_rows: Vec<serde_json::Value> = dump
        .get(&object_table)
        .expect("object table in dump")
        .iter()
        .map(|row| serde_json::from_str(row).expect("object dump row"))
        .collect();
    assert!(
        object_rows
            .iter()
            .any(|row| { dump_row_has_integer(row, "source", 5, i64::MAX) })
    );
    assert!(
        object_rows
            .iter()
            .any(|row| { dump_row_has_integer(row, "target", 5, i64::MIN) })
    );
    assert!(
        object_rows
            .iter()
            .any(|row| { dump_row_has_integer(row, "middle", 5, 9_007_199_254_740_993_i64) })
    );
    let link_rows: Vec<serde_json::Value> = dump
        .get(&link_table)
        .expect("link table in dump")
        .iter()
        .map(|row| serde_json::from_str(row).expect("link dump row"))
        .collect();
    assert!(
        link_rows
            .iter()
            .any(|row| { dump_row_has_integer(row, "source", 3, i64::MAX) })
    );
    assert!(
        !unsafe_integer.get(),
        "an unsafe integer crossed the SQL seam"
    );

    drop(store);
    let verifier = NativeEngine::open(&path, StoreOptions::default()).expect("reopen engine");
    let object_storage = verifier
        .query_row(
            &format!(
                "SELECT typeof(\"amount\"), CAST(\"amount\" AS TEXT) FROM {} WHERE {} = ?1 AND {} = ?2",
                quote_identifier(&object_table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            &[SqlValue::from(MAIN_WORLD), SqlValue::from("source")],
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
        )
        .expect("verify object storage");
    assert_eq!(object_storage, ("integer".to_owned(), i64::MAX.to_string()));
    let link_storage = verifier
        .query_row(
            &format!(
                "SELECT typeof(\"weight\"), CAST(\"weight\" AS TEXT) FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3",
                quote_identifier(&link_table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(SOURCE_COLUMN),
                quote_identifier(TARGET_COLUMN),
            ),
            &[
                SqlValue::from(MAIN_WORLD),
                SqlValue::from("source"),
                SqlValue::from("target"),
            ],
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
        )
        .expect("verify link storage");
    assert_eq!(link_storage, ("integer".to_owned(), i64::MAX.to_string()));
    drop(verifier);

    for malformed in [SqlValue::Real(1.5), SqlValue::Blob(vec![0xff])] {
        let corruptor = NativeEngine::open(&path, StoreOptions::default()).expect("corruptor");
        corruptor
            .execute(
                &format!(
                    "UPDATE {} SET \"amount\" = ?1 WHERE {} = ?2 AND {} = ?3",
                    quote_identifier(&object_table),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(ID_COLUMN),
                ),
                &[
                    malformed,
                    SqlValue::from(MAIN_WORLD),
                    SqlValue::from("source"),
                ],
            )
            .expect("write malformed storage class");
        drop(corruptor);
        let corrupted = CellStore::open(&path, model.clone(), StoreOptions::default())
            .expect("corrupt property does not affect open");
        assert!(matches!(
            corrupted.read().object(MAIN_WORLD, "test.Item", "source"),
            Err(CellError::CorruptValue { .. })
        ));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keeps long-identifier object, link, and dump reads together"
)]
fn long_integer_columns_remain_readable_in_objects_links_and_dump() {
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD};
    use norn_ir::RuntimeValue as Value;
    use serde_json::json;
    use std::collections::BTreeMap;

    let long_property = "a".repeat(34_000);
    let model = common::model_of(&[serde_json::from_value(json!({
        "name": "test.Item",
        "properties": [{ "id": long_property, "type": { "kind": "integer" } }],
        "links": [{
            "id": "next",
            "target": "test.Item",
            "cardinality": { "to": "many", "from": "many" },
            "properties": [{ "id": long_property, "type": { "kind": "integer" } }]
        }]
    }))
    .expect("item type")]);
    let object_table = model
        .schema()
        .object_table("test.Item")
        .expect("object table")
        .table
        .clone();
    let link_table = model
        .schema()
        .link_table("test.Item", "next")
        .expect("link table")
        .table
        .clone();
    let value = Value::from_json_text("9223372036854775807").expect("i64 max");
    let values = |value: &Value| BTreeMap::from([(long_property.clone(), value.clone())]);
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "test".to_owned(),
            writes: vec![
                FactWriteRow::Put {
                    object_type: "test.Item".to_owned(),
                    key: "source".to_owned(),
                    values: values(&value),
                },
                FactWriteRow::Put {
                    object_type: "test.Item".to_owned(),
                    key: "target".to_owned(),
                    values: values(&value),
                },
            ],
            links: vec![LinkChange::Add {
                object_type: "test.Item".to_owned(),
                link: "next".to_owned(),
                source: "source".to_owned(),
                target: "target".to_owned(),
                values: values(&value),
                target_type: None,
            }],
            ..CommitBatch::default()
        })
        .expect("commit");

    let object = store
        .read()
        .object(MAIN_WORLD, "test.Item", "source")
        .expect("read object")
        .expect("object exists");
    assert_eq!(object.values.get(&long_property), Some(&value));
    let links = store
        .read()
        .links(MAIN_WORLD, "test.Item", "next", "source")
        .expect("read links");
    assert_eq!(
        links
            .first()
            .and_then(|link| link.values.get(&long_property)),
        Some(&value)
    );

    let dump = store.read().dump().expect("dump");
    let object_rows: Vec<serde_json::Value> = dump
        .get(&object_table)
        .expect("object table in dump")
        .iter()
        .map(|row| serde_json::from_str(row).expect("object dump row"))
        .collect();
    assert!(
        object_rows
            .iter()
            .any(|row| dump_row_has_integer(row, "source", 5, i64::MAX))
    );
    let link_rows: Vec<serde_json::Value> = dump
        .get(&link_table)
        .expect("link table in dump")
        .iter()
        .map(|row| serde_json::from_str(row).expect("link dump row"))
        .collect();
    assert!(
        link_rows
            .iter()
            .any(|row| dump_row_has_integer(row, "source", 3, i64::MAX))
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keeps wide-projection reads and dump assertions together"
)]
fn wide_integer_projections_split_before_the_statement_limit() {
    use norn_cell::reactive::{BudgetedRead, DecodeBudget, ReactiveLimits};
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD};
    use norn_ir::RuntimeValue as Value;
    use norn_projections::sqlite_ddl::{ID_COLUMN, WORLD_COLUMN, quote_identifier};
    use serde_json::json;
    use std::collections::BTreeMap;

    let property_ids: Vec<String> = (0..90)
        .map(|index| format!("p{index:03}_{}", "x".repeat(1_075)))
        .collect();
    let properties: Vec<serde_json::Value> = property_ids
        .iter()
        .map(|id| json!({ "id": id, "type": { "kind": "integer" } }))
        .collect();
    let object_type = serde_json::from_value(json!({
        "name": "test.Wide",
        "properties": properties
    }))
    .expect("wide type");
    let model = common::model_of(&[object_type]);
    let table = model
        .schema()
        .object_table("test.Wide")
        .expect("wide object table")
        .table
        .clone();
    assert!(
        model
            .schema()
            .statements()
            .all(|statement| statement.len() <= MAX_STATEMENT_BYTES),
        "the model's DDL must fit the statement limit"
    );
    let reactive_projection_bytes: usize = property_ids
        .iter()
        .map(|property| format!("COALESCE(octet_length({}), 8)", quote_identifier(property)).len())
        .sum();
    let reactive_suffix = format!(
        " FROM {} WHERE {} = ?1 AND {} = ?2",
        quote_identifier(&table),
        quote_identifier(WORLD_COLUMN),
        quote_identifier(ID_COLUMN),
    );
    let reactive_statement_bytes = "SELECT "
        .len()
        .saturating_add(reactive_projection_bytes)
        .saturating_add(property_ids.len().saturating_sub(1).saturating_mul(2))
        .saturating_add(reactive_suffix.len());
    assert!(
        reactive_statement_bytes > MAX_STATEMENT_BYTES,
        "the reactive metadata statement must cross the statement limit"
    );
    let value = Value::from_json_text("42").expect("integer value");
    let values: BTreeMap<String, Value> = property_ids
        .iter()
        .cloned()
        .map(|id| (id, value.clone()))
        .collect();
    let mut store = CellStore::open_in_memory(model).expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "test".to_owned(),
            writes: vec![FactWriteRow::Put {
                object_type: "test.Wide".to_owned(),
                key: "wide".to_owned(),
                values,
            }],
            ..CommitBatch::default()
        })
        .expect("commit");

    let object = store
        .read()
        .object(MAIN_WORLD, "test.Wide", "wide")
        .expect("read wide object")
        .expect("wide object exists");
    assert_eq!(object.values.len(), property_ids.len());
    assert!(
        property_ids
            .iter()
            .all(|id| object.values.get(id) == Some(&value))
    );
    let mut budget = DecodeBudget::new(&ReactiveLimits {
        max_records: 1,
        max_canonical_bytes: u64::MAX,
        max_fanout_per_commit: 1,
        max_scan: 1,
        max_candidate_bytes_per_commit: u64::MAX,
    });
    let BudgetedRead::Found(budgeted_object) = store
        .read()
        .object_with_budget(MAIN_WORLD, "test.Wide", "wide", &mut budget)
        .expect("budgeted wide read")
    else {
        panic!("wide object should fit the ample decode budget");
    };
    assert_eq!(budgeted_object.values.len(), property_ids.len());
    let dump = store.read().dump().expect("dump");
    let rows: Vec<serde_json::Value> = dump
        .get(&table)
        .expect("wide table in dump")
        .iter()
        .map(|row| serde_json::from_str(row).expect("wide dump row"))
        .collect();
    let cells = rows
        .first()
        .and_then(serde_json::Value::as_array)
        .expect("dump row cells");
    assert_eq!(cells.len(), 5_usize.saturating_add(property_ids.len()));
    assert!(
        cells
            .iter()
            .skip(5)
            .all(|cell| cell == &serde_json::Value::from(42))
    );
}

fn dump_row_has_integer(row: &serde_json::Value, id: &str, index: usize, value: i64) -> bool {
    let Some(cells) = row.as_array() else {
        return false;
    };
    cells.get(1).and_then(serde_json::Value::as_str) == Some(id)
        && cells.get(index) == Some(&serde_json::Value::from(value))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario proves exact versions, inherited identities, links and indexed work"
)]
fn fact_version_receipt_uses_indexed_bounded_lookup_after_unrelated_commits() {
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    let model = common::model_of(&[
        serde_json::from_value(json!({
            "name":"test.Root", "links":[{"id":"next","target":"test.Root",
            "cardinality":{"to":"many","from":"many"}}]
        }))
        .expect("root"),
        serde_json::from_value(json!({"name":"test.Child","extends":"test.Root"})).expect("child"),
    ]);
    let largest_query = Rc::new(std::cell::Cell::new(0));
    let query_calls = Rc::new(std::cell::Cell::new(0));
    let plans = Rc::new(std::cell::RefCell::new(Vec::new()));
    let engine = MeasuredEngine {
        native: NativeEngine::open_in_memory().expect("engine"),
        largest_query: Rc::clone(&largest_query),
        query_calls: Rc::clone(&query_calls),
        unsafe_integer: Rc::new(std::cell::Cell::new(false)),
        target_query_plans: Some(Rc::clone(&plans)),
    };
    let mut store = CellStore::from_engine(engine, model).expect("store");
    let put = |object_type: &str, key: &str| FactWriteRow::Put {
        object_type: object_type.into(),
        key: key.into(),
        values: BTreeMap::new(),
    };
    let first = store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: "test".into(),
            writes: vec![put("test.Child", "source"), put("test.Root", "target")],
            links: vec![LinkChange::Add {
                object_type: "test.Root".into(),
                link: "next".into(),
                source: "source".into(),
                target: "target".into(),
                target_type: None,
                values: BTreeMap::new(),
            }],
            ..CommitBatch::default()
        })
        .expect("linked commit");
    let second = store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: "test".into(),
            writes: vec![put("test.Child", "source")],
            ..CommitBatch::default()
        })
        .expect("version two");
    for id in 0..1024 {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: "test".into(),
                writes: vec![put("test.Root", &format!("unrelated-{id}"))],
                ..CommitBatch::default()
            })
            .expect("unrelated commit");
    }
    largest_query.set(0);
    query_calls.set(0);
    plans.borrow_mut().clear();
    let receipt = store
        .read()
        .receipt_for_fact_version(MAIN_WORLD, "test.Child", "source", 1)
        .expect("exact receipt");
    assert_eq!(&receipt, first.receipt());
    assert_eq!(receipt.body.linked.len(), 1);
    assert_eq!(
        query_calls.get(),
        2,
        "snapshot primary key plus one receipt"
    );
    assert_eq!(largest_query.get(), 1, "no ledger/history materialization");
    let plans = plans.borrow().clone();
    assert_eq!(plans.len(), 1);
    assert!(
        plans.iter().all(|plan| plan.contains("SEARCH")
            && plan.contains("world=? AND root_type=? AND id=? AND version=?")),
        "{plans:?}"
    );
    assert_eq!(
        &store
            .read()
            .receipt_for_fact_version(MAIN_WORLD, "test.Root", "source", 2)
            .expect("inherited identity/version two"),
        second.receipt()
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "One scenario checks indexed scope, lookup work and future evidence exclusion"
)]
fn finding_metadata_lookup_uses_commit_index_and_excludes_future_history() {
    use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
    use norn_cell::{CellStore, CommitBatch, MAIN_WORLD};
    use serde_json::json;
    use std::rc::Rc;
    let plans = Rc::new(std::cell::RefCell::new(Vec::new()));
    let largest_query = Rc::new(std::cell::Cell::new(0));
    let mut store = CellStore::from_engine(
        MeasuredEngine {
            native: NativeEngine::open_in_memory().expect("engine"),
            largest_query: Rc::clone(&largest_query),
            query_calls: Rc::new(std::cell::Cell::new(0)),
            unsafe_integer: Rc::new(std::cell::Cell::new(false)),
            target_query_plans: Some(Rc::clone(&plans)),
        },
        common::model_of(&[]),
    )
    .expect("store");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: "test".into(),
            logical_time: 1,
            events: vec![
                norn_ir::runtime::encode(
                    &json!({"op":"data_quality_finding","fact":{"id":"before"}}),
                )
                .expect("finding"),
                norn_ir::runtime::encode(
                    &json!({"type":"observation.recorded","observationId":"bounded"}),
                )
                .expect("metadata"),
                norn_ir::runtime::encode(
                    &json!({"op":"data_quality_finding","fact":{"id":"after"}}),
                )
                .expect("finding"),
            ],
            ..CommitBatch::default()
        })
        .expect("finding commit");
    // Future evidence must neither enter the membership lookup nor exhaust its
    // bytes: the requested metadata has exactly three same-commit neighbors.
    for _ in 0..64 {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: "test".into(),
                logical_time: 1,
                events: vec![
                    norn_ir::runtime::encode(
                        &json!({"op":"data_quality_finding","fact":{"raw":"x".repeat(8192)}}),
                    )
                    .expect("future finding"),
                ],
                ..CommitBatch::default()
            })
            .expect("future commit");
    }
    plans.borrow_mut().clear();
    largest_query.set(0);
    let mut limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    limits.max_scan = 4;
    limits.max_candidate_bytes_per_commit = 4096;
    let mut budget = DecodeBudget::new(&limits);
    let page = store
        .read()
        .finding_ledger_page_with_budget(1, 1, &mut budget)
        .expect("lookup")
        .expect("same commit fits");
    assert_eq!(page.scanned_rows, 4);
    assert_eq!(page.scanned_through, 2);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries.first().expect("metadata").seq, 2);
    assert_eq!(budget.remaining_rows(), 0);
    assert_eq!(
        largest_query.get(),
        3,
        "only three same-commit rows materialized"
    );
    let recorded = plans.borrow();
    assert!(
        recorded
            .iter()
            .any(|plan| plan.contains("_norn_ledger/commit") && plan.contains("commit_seq=?")),
        "missing indexed same-commit lookup: {recorded:?}"
    );
    assert!(
        !recorded
            .iter()
            .any(|plan| plan.contains("SCAN _norn_ledger")),
        "future history scanned: {recorded:?}"
    );
    drop(recorded);
    limits.max_scan = 3;
    assert!(
        store
            .read()
            .finding_ledger_page_with_budget(1, 1, &mut DecodeBudget::new(&limits))
            .expect("row bound")
            .is_none()
    );
}
