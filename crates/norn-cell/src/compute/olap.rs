//! QUERY-ENGINE-1: lower admitted keyed aggregates over an exact contribution table.
use super::{
    BTreeMap, CellError, CellStore, Decimal, Engine, Member, ObjectRecord, QueryContext,
    QueryResult, QueryRow, QuestionPlan, Snapshot, SqlEngine, UNSUPPORTED_ENGINE, bucket_key,
    decimal, refused,
};
use duckdb::{Connection, params};
use norn_checker::entity_instance::EffectiveKind;
use norn_ir::PropertyType;

fn db<T>(value: duckdb::Result<T>) -> Result<T, CellError> {
    value.map_err(|error| CellError::Engine(error.to_string()))
}

impl<E: SqlEngine> CellStore<E> {
    /// QUERY-ENGINE-1: exact DECIMAL aggregation over the authorized contribution snapshot.
    /// # Errors
    /// Returns snapshot/refusal errors, or a host SQL engine error on execution failure.
    pub fn evaluate_question_duckdb(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        let (schema, aggregate) = lower(plan)?;
        let snapshot = self.question_snapshot(plan, context, &mut visible)?;
        let rows = execute(plan, &snapshot, &schema, &aggregate)?;
        let mut result = snapshot.result;
        result.rows = rows;
        result.engine = Engine::Duckdb;
        let evidence = super::FindingEvidence {
            context: &result.context,
            exclusions: &result.exclusions,
        };
        let lineage_ref = crate::codec::hash_json(&(
            &result.query_pin,
            &result.revision,
            evidence,
            &result.rows,
        ))?;
        result.lineage_ref = lineage_ref;
        Ok(result)
    }
}

fn scale(sum: &norn_checker::metric::compute::KeyedSum) -> Result<u64, CellError> {
    match &sum.property.kind {
        EffectiveKind::Base(PropertyType::Decimal { scale, .. }) if scale.0 <= 6 => Ok(scale.0),
        EffectiveKind::Base(_) | EffectiveKind::Registry(_) | EffectiveKind::Unresolved => {
            Err(refused(UNSUPPORTED_ENGINE))
        }
    }
}

fn lower(plan: &QuestionPlan) -> Result<(String, String), CellError> {
    // METRIC-SNAPSHOT-ASOF-1 / METRIC-GRAIN-LINK-1: these lowerings belong to the analytics engine.
    if plan.as_of() || plan.time_link().is_some() {
        return Err(refused(UNSUPPORTED_ENGINE));
    }
    let (a, b, ratio) = match plan.sums() {
        [a] => (scale(a)?, 0, false),
        [a, b] => (scale(a)?, scale(b)?, true),
        _ => return Err(refused(UNSUPPORTED_ENGINE)),
    };
    let schema = format!(
        "CREATE TABLE input(bucket VARCHAR NOT NULL,id VARCHAR PRIMARY KEY,version UBIGINT NOT NULL,a DECIMAL(38,{a}),b DECIMAL(38,{b}),na UBIGINT NOT NULL,nb UBIGINT NOT NULL)"
    );
    let value = if ratio {
        "CASE WHEN d=0 THEN NULL ELSE v + CASE WHEN abs(r)>=abs(d)-abs(r) THEN CASE WHEN (n<0)=(d<0) THEN 1 ELSE -1 END ELSE 0 END END"
    } else {
        "n"
    };
    // Decimal division produces DOUBLE in DuckDB. Only HUGEINT quotient/remainder is used.
    let aggregate = format!(
        "WITH s AS (SELECT bucket,COALESCE(SUM(a),0) AS a,COALESCE(SUM(b),0) AS b,SUM(na) AS na,SUM(nb) AS nb FROM input GROUP BY bucket),
        c AS (SELECT *,CAST(a*1000000 AS HUGEINT) AS n,CAST(b*1000000 AS HUGEINT) AS d FROM s),
        q AS (SELECT *,n*1000000 // NULLIF(d,0) AS v,n*1000000 % NULLIF(d,0) AS r FROM c)
        SELECT bucket,CAST(a AS VARCHAR),CAST(b AS VARCHAR),CAST({value} AS VARCHAR),CAST(na AS UBIGINT),CAST(nb AS UBIGINT) FROM q ORDER BY bucket"
    );
    Ok((schema, aggregate))
}

fn operand(value: Option<i64>) -> Result<Option<String>, CellError> {
    value
        .map(|n| decimal(i128::from(n)).map(|d| d.canonical()))
        .transpose()
}

fn execute(
    plan: &QuestionPlan,
    snapshot: &Snapshot,
    schema: &str,
    aggregate: &str,
) -> Result<BTreeMap<String, QueryRow>, CellError> {
    let conn = db(Connection::open_in_memory())?;
    db(conn.execute_batch("SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; SET enable_external_access=false; SET threads=1;"))?;
    db(conn.execute_batch(schema))?;
    let mut insert = db(conn.prepare("INSERT INTO input VALUES (?,?,?,?,?,?,?)"))?;
    for (id, (version, contribution)) in super::entries(&snapshot.contributions) {
        let (a, b) = match contribution.amounts.as_slice() {
            [a] => (*a, None),
            [a, b] => (*a, *b),
            _ => return Err(refused(UNSUPPORTED_ENGINE)),
        };
        // A null measured value is skipped by SUM (it is SQL NULL) and counted here.
        let null =
            |index: usize| u64::from(contribution.nulls.get(index).copied().unwrap_or(false));
        db(insert.execute(params![
            bucket_key(contribution)?,
            id,
            version,
            operand(a)?,
            operand(b)?,
            null(0),
            null(1)
        ]))?;
    }
    let mut rows = aggregate_rows(&conn, aggregate, plan.sums().len(), &snapshot.result.rows)?;
    let mut members = db(conn.prepare("SELECT bucket,id,version,CAST(a AS VARCHAR),CAST(b AS VARCHAR) FROM input ORDER BY bucket,id"))?;
    let captured = db(members.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u64>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    }))?;
    for member in captured {
        let (bucket, id, version, a, b) = db(member)?;
        let amounts = [a, b]
            .into_iter()
            .take(plan.sums().len())
            .map(|v| {
                v.map(|v| Decimal::parse(&v).map_err(CellError::Value))
                    .transpose()
            })
            .collect::<Result<_, _>>()?;
        if rows
            .get_mut(&bucket)
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?
            .members
            .insert(id, Member::visible(version, amounts))
            .is_some()
        {
            return Err(refused(UNSUPPORTED_ENGINE));
        }
    }
    Ok(rows)
}

fn aggregate_rows(
    conn: &Connection,
    sql: &str,
    arity: usize,
    representatives: &BTreeMap<String, QueryRow>,
) -> Result<BTreeMap<String, QueryRow>, CellError> {
    let mut stmt = db(conn.prepare(sql))?;
    let totals = db(stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, u64>(4)?,
            row.get::<_, u64>(5)?,
        ))
    }))?;
    let mut rows = BTreeMap::new();
    for total in totals {
        let (key, a, b, value, na, nb) = db(total)?;
        let counts: Vec<u64> = [na, nb].into_iter().take(arity).collect();
        let null_counts = if counts.iter().any(|count| *count > 0) {
            counts
        } else {
            Vec::new()
        };
        let bucket = representatives
            .get(&key)
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?
            .bucket
            .clone();
        let totals = [a, b]
            .into_iter()
            .take(arity)
            .map(|s| Decimal::parse(&s).map_err(CellError::Value))
            .collect::<Result<_, _>>()?;
        let value = value
            .map(|v| Decimal::parse(&format!("{v}e-6")).map_err(CellError::Value))
            .transpose()?;
        rows.insert(
            key,
            QueryRow {
                bucket,
                totals,
                value,
                members: BTreeMap::new(),
                suppressed: false,
                null_counts,
            },
        );
    }
    Ok(rows)
}
