//! The dev host's server policy: one local developer, named by the project
//! manifest, holding every grant on a loopback-only cell (DEV-LOOPBACK-1).
//!
//! The principal comes only from the session bearer token in
//! `.norn/dev.token`; any other caller is denied by default. This is server
//! policy, stated on every `ready` event, never client logic.
use crate::principal::PrincipalResolver;
use norn_api::authz::{AccessRequest, Authorizer, DenyAll, ReadRequest, RefusalDetailRequest};
use norn_api::refusal::Refusal;
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};

/// Every grant to exactly one actor; default deny for everyone else.
pub(crate) struct LocalDeveloper {
    pub(crate) actor: String,
}

impl LocalDeveloper {
    fn is(&self, actor: &str) -> bool {
        actor == self.actor
    }
}

impl Authorizer for LocalDeveloper {
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if self.is(&request.access.principal.subject_id) {
            Ok(())
        } else {
            Err(norn_api::authz::read_denied())
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
            })
        }
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        self.is(request.actor)
    }
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if request.actor.is_some_and(|actor| self.is(actor)) {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
}

/// The session token is the only credential; it names the manifest's actor.
pub(crate) struct DevPrincipals {
    pub(crate) bearer: String,
    pub(crate) actor: String,
}

impl PrincipalResolver for DevPrincipals {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        let presented = authorization?.strip_prefix("Bearer ")?;
        // Equal length first, then every byte: no early exit on a prefix.
        (presented.len() == self.bearer.len()
            && presented
                .bytes()
                .zip(self.bearer.bytes())
                .fold(0_u8, |seen, (a, b)| seen | (a ^ b))
                == 0)
            .then(|| self.actor.clone())
    }
    fn end_request(&self) {}
}

pub(crate) use crate::questions::DerivedQuestions as DevMetrics;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dev-host question fixtures"
)]
mod tests {
    //! The dev host answers ad-hoc questions over the metrics its active
    //! revision derives, through the same dispatch the HTTP transport uses,
    //! and re-derives them when a new revision is installed.
    use super::*;
    use crate::server::{Request, handle_with};
    use norn_api::host::DevHost;
    use norn_api::question_routes::AdHocQuestions;
    use norn_api::scope::CellScope;
    use norn_cell::CellStore;
    use norn_cell::engine::NativeEngine;
    use norn_cell::{CommitBatch, FactWriteRow, StoreOptions};
    use norn_ir::{Decimal, RuntimeValue};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    const ACTOR: &str = "dev@example";
    const BEARER: &str = "test-bearer-0123456789";
    const TYPE: &str = "shop.Sale";

    fn scope() -> CellScope {
        CellScope {
            org_id: "org_dev".into(),
            environment_id: "local".into(),
        }
    }

    /// A test project's one type. `amount` and `quantity` are flows, so they
    /// are declared additive; `price` is their declared ratio.
    fn artifacts(additive: bool) -> Vec<Value> {
        let flow = |scale: u64| {
            if additive {
                json!({"kind": "decimal", "scale": scale, "additivity": {"kind": "additive"}})
            } else {
                json!({"kind": "decimal", "scale": scale})
            }
        };
        let s = scope();
        vec![json!({
            "irVersion": "0.1", "artifactId": format!("{TYPE}.entity"), "kind": "entity",
            "namespace": "shop", "name": "Sale", "version": "0.1.0", "lifecycle": "active",
            "scope": {"orgId": s.org_id, "environmentId": s.environment_id},
            "dependencies": [], "provenance": {"source": "system", "sourcePath": "dev-host test"},
            "payload": {"entityVersion": "2.0", "objectType": {"name": TYPE, "properties": [
                {"id": "amount", "type": flow(2)},
                {"id": "quantity", "type": flow(3)},
                {"id": "region", "type": {"kind": "text"}},
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
            ]}}
        })]
    }

    fn sale(key: &str, amount: &str, quantity: &str, region: &str, at: &str) -> FactWriteRow {
        let decimal = |text: &str| RuntimeValue::from(Decimal::parse(text).unwrap());
        FactWriteRow::Put {
            object_type: TYPE.into(),
            key: key.into(),
            values: BTreeMap::from([
                ("amount".into(), decimal(amount)),
                ("quantity".into(), decimal(quantity)),
                ("region".into(), RuntimeValue::from(region)),
                ("occurred_at".into(), RuntimeValue::from(at)),
            ]),
        }
    }

    fn host(dir: &std::path::Path) -> DevHost<NativeEngine> {
        host_with(dir, 0)
    }

    /// The test host, plus `extra` generated sales over the first week of
    /// November in four regions, committed 1,000 at a time.
    fn host_with(dir: &std::path::Path, extra: usize) -> DevHost<NativeEngine> {
        let admitted = super::super::admit::admit(&artifacts(true), 1_000)
            .ok()
            .expect("admitted");
        let engine = NativeEngine::open(&dir.join("cell.sqlite"), StoreOptions::default()).unwrap();
        let mut store = CellStore::provision_definitions(
            engine,
            admitted.model,
            &admitted.input,
            &scope().key(),
        )
        .unwrap();
        store
            .commit(&CommitBatch {
                world: norn_cell::MAIN_WORLD.into(),
                scope: scope().key(),
                logical_time: 2_000,
                writes: vec![
                    sale("s1", "10.00", "2.000", "north", "2026-11-02T10:00:00Z"),
                    sale("s2", "5.50", "1.000", "north", "2026-11-02T12:00:00Z"),
                    sale("s3", "7.25", "3.000", "south", "2026-11-03T09:00:00Z"),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
        let regions = ["north", "south", "east", "west"];
        let days = ["01", "02", "03", "04", "05", "06", "07"];
        let generated: Vec<usize> = (0..extra).collect();
        for (logical_time, chunk) in (2_001..).zip(generated.chunks(1_000)) {
            let writes = chunk
                .iter()
                .map(|&i| {
                    sale(
                        &format!("m{i}"),
                        &format!("{}.{:02}", i % 997, i % 100),
                        "1.000",
                        regions[i % 4],
                        &format!("2026-11-{}T{:02}:00:00Z", days[i % 7], i % 24),
                    )
                })
                .collect();
            store
                .commit(&CommitBatch {
                    world: norn_cell::MAIN_WORLD.into(),
                    scope: scope().key(),
                    logical_time,
                    writes,
                    ..CommitBatch::default()
                })
                .unwrap();
        }
        DevHost::from_store(store, scope(), |command| command.payload.clone())
            .expect("host")
            .with_authorizer(LocalDeveloper {
                actor: ACTOR.into(),
            })
    }

    fn call(
        host: &DevHost<NativeEngine>,
        metrics: &DevMetrics,
        path: &str,
        body: &Value,
    ) -> (u16, Value) {
        let questions = metrics.questions(host).expect("runtime");
        let principals = DevPrincipals {
            bearer: BEARER.into(),
            actor: ACTOR.into(),
        };
        let body = body.to_string();
        let authorization = format!("Bearer {BEARER}");
        let response = handle_with(
            host,
            &principals,
            Some(&questions),
            &Request {
                method: "POST",
                path,
                query: "orgId=org_dev&environmentId=local",
                authorization: Some(&authorization),
                now: 1_800_000_000,
                body: &body,
            },
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap(),
        )
    }

    fn get(host: &DevHost<NativeEngine>, metrics: &DevMetrics, path: &str) -> (u16, Value) {
        let questions = metrics.questions(host).expect("runtime");
        let principals = DevPrincipals {
            bearer: BEARER.into(),
            actor: ACTOR.into(),
        };
        let authorization = format!("Bearer {BEARER}");
        let response = handle_with(
            host,
            &principals,
            Some(&questions),
            &Request {
                method: "GET",
                path,
                query: "orgId=org_dev&environmentId=local",
                authorization: Some(&authorization),
                now: 1_800_000_000,
                body: "",
            },
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap(),
        )
    }

    fn question(metric: &str) -> Value {
        json!({
            "irVersion": "0.1", "name": "dev.daily", "objectSet": TYPE,
            "aggregate": [{"metric": metric}],
            // The bounded profile groups by a plain property or a week.
            "groupBy": [{"path": {"property": "region"}}],
            "window": {"start": "2026-11-01T00:00:00Z", "end": "2026-11-08T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 60000},
            "calendar": "iso_utc"
        })
    }

    fn metric(
        metrics: &DevMetrics,
        host: &DevHost<NativeEngine>,
        property: &str,
        ratio: bool,
    ) -> String {
        let questions = metrics.questions(host).expect("runtime");
        AdHocQuestions::<NativeEngine>::definitions(&questions)
            .iter()
            .find(|definition| {
                let is_ratio = definition.type_params.numerator.is_some();
                is_ratio == ratio
                    && definition.label.contains(property)
                    && definition
                        .type_params
                        .metric_aggregation_params
                        .as_ref()
                        .is_none_or(|params| params.agg_time_dimension == "occurred_at")
            })
            .map_or_else(
                || {
                    panic!(
                        "no derived {property} metric among {:?}",
                        AdHocQuestions::<NativeEngine>::definitions(&questions)
                            .iter()
                            .map(|d| &d.label)
                            .collect::<Vec<_>>()
                    )
                },
                |definition| definition.name.clone(),
            )
    }

    #[test]
    fn an_ad_hoc_question_is_answered_and_traced_over_the_derived_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        // The metrics a question may name are listed for the reader.
        let (status, listed) = get(&host, &metrics, "/v1/questions:metrics");
        assert_eq!(status, 200, "{listed}");
        let listed = listed["metrics"].as_array().unwrap();
        let entry = listed
            .iter()
            .find(|metric| metric["name"] == amount.as_str())
            .unwrap();
        assert_eq!(entry["objectType"], TYPE);
        assert_eq!(entry["timeProperty"], "occurred_at");
        let (status, answer) = call(
            &host,
            &metrics,
            "/v1/questions:evaluate",
            &json!({"question": question(&amount)}),
        );
        assert_eq!(status, 200, "{answer}");
        let rows = answer["rows"].as_object().unwrap();
        assert_eq!(rows.len(), 2, "{answer}");
        let totals: Vec<String> = rows
            .values()
            // A total is a JSON number; its text keeps the digits.
            .map(|row| row["totals"][0].to_string())
            .collect();
        assert!(
            totals.iter().any(|total| total.starts_with("15.5")),
            "{totals:?}"
        );
        assert!(
            totals.iter().any(|total| total.starts_with("7.25")),
            "{totals:?}"
        );
        let engine = answer["engine"].as_str().unwrap();
        if cfg!(feature = "duckdb") {
            assert_eq!(engine, "duckdb");
        } else {
            assert_eq!(engine, "cell");
        }
        // The trace of a bucket reaches the objects that contributed to it.
        let (bucket, _) = rows
            .iter()
            .find(|(_, row)| row["totals"][0].to_string().starts_with("15.5"))
            .unwrap();
        let (status, trace) = call(
            &host,
            &metrics,
            "/v1/questions:trace",
            &json!({"question": question(&amount), "lineageRef": answer["lineageRef"], "bucket": bucket}),
        );
        assert_eq!(status, 200, "{trace}");
        let text = trace.to_string();
        assert!(
            text.contains("s1") && text.contains("s2") && !text.contains("\"s3\""),
            "{trace}"
        );
    }

    /// A ratio is governed by its operands' type only when both operands sum
    /// the same type; across two types it names none, so every read of it is
    /// refused (admission also authorizes each operand against its own type).
    #[test]
    fn a_ratio_is_governed_by_both_operands_types_or_by_none() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let (amount, quantity) = (
            metric(&metrics, &host, "amount", false),
            metric(&metrics, &host, "quantity", false),
        );
        let questions = metrics.questions(&host).expect("runtime");
        let mut definitions = AdHocQuestions::<NativeEngine>::definitions(&questions).to_vec();
        let mut ratio = definitions
            .iter()
            .find(|definition| definition.name == amount)
            .unwrap()
            .clone();
        ratio.name = "dev.price".into();
        ratio.type_params = norn_ir::metric::MetricTypeParams {
            numerator: Some(amount.clone()),
            denominator: Some(quantity.clone()),
            ..norn_ir::metric::MetricTypeParams::default()
        };
        definitions.push(ratio);
        assert_eq!(crate::questions::governed(&definitions, "dev.price"), TYPE);
        assert_eq!(crate::questions::governed(&definitions, &amount), TYPE);
        assert_eq!(crate::questions::governed(&definitions, TYPE), TYPE);
        for definition in &mut definitions {
            if definition.name == quantity {
                definition
                    .type_params
                    .metric_aggregation_params
                    .as_mut()
                    .unwrap()
                    .semantic_model = norn_ir::ObjectTypeName("shop.Other".into());
            }
        }
        assert_eq!(
            crate::questions::governed(&definitions, "dev.price"),
            "dev.price"
        );
    }

    /// The measurement behind the question scan bound (ruling 2026-11-12):
    /// `NORN_SCAN_ROWS=<n> cargo test --release -p norn-cell-do --lib
    /// measure_question_scan -- --ignored --nocapture`, one process per `n`.
    /// Prints the exact engine's time for one ad-hoc evaluation over `n` rows
    /// and the process's peak resident memory during it (Linux only).
    #[test]
    #[ignore = "a measurement, run by hand on the cell host"]
    fn measure_question_scan() {
        fn status(field: &str) -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .unwrap_or_default()
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
                .unwrap_or(0)
        }
        let rows: usize = std::env::var("NORN_SCAN_ROWS")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(10_000);
        let dir = tempfile::tempdir().unwrap();
        let loaded = std::time::Instant::now();
        let host = host_with(dir.path(), rows.saturating_sub(3));
        let load = loaded.elapsed();
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        let body = json!({"question": question(&amount)});
        let before = status("VmRSS:");
        // Reset the peak so it measures the evaluation, not the load.
        drop(std::fs::write("/proc/self/clear_refs", "5"));
        let mut timings = Vec::new();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let (status_code, answer) = call(&host, &metrics, "/v1/questions:evaluate", &body);
            timings.push(started.elapsed().as_millis());
            assert_eq!(status_code, 200, "{answer}");
        }
        println!(
            "rows={rows} load_s={:.1} evaluate_ms={timings:?} rss_before_kib={before} peak_kib={}",
            load.as_secs_f64(),
            status("VmHWM:")
        );
    }

    #[test]
    fn the_row_bound_is_the_profiles_question_bound() {
        assert_eq!(
            crate::questions::MAX_ROWS,
            norn_ir::ResourceLimitProfile::norn_limits_v1()
                .question_limits()
                .max_scanned_records
        );
    }

    #[test]
    fn every_derived_sum_is_answered_on_whichever_engine_lowers_it() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let questions = metrics.questions(&host).expect("runtime");
        assert!(!AdHocQuestions::<NativeEngine>::definitions(&questions).is_empty());
        // Every derived sum answers, on whichever engine lowered it.
        for definition in AdHocQuestions::<NativeEngine>::definitions(&questions)
            .iter()
            .filter(|d| d.type_params.numerator.is_none())
        {
            let (status, answer) = call(
                &host,
                &metrics,
                "/v1/questions:evaluate",
                &json!({"question": question(&definition.name)}),
            );
            assert_eq!(status, 200, "{} {answer}", definition.name);
            assert!(matches!(answer["engine"].as_str(), Some("cell" | "duckdb")));
        }
    }

    #[test]
    fn a_new_revision_is_derived_again_and_an_undeclared_measure_is_no_metric() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        let active = host.active_definitions().ok().flatten().expect("active");
        let admitted = super::super::admit::admit(&artifacts(false), 3_000)
            .ok()
            .expect("admitted");
        host.install_definitions(admitted.model, admitted.input, Some(active.version))
            .expect("installed");
        let questions = metrics.questions(&host).expect("runtime");
        assert!(
            AdHocQuestions::<NativeEngine>::definitions(&questions)
                .iter()
                .all(|definition| definition.name != amount)
        );
        let (status, refused) = call(
            &host,
            &metrics,
            "/v1/questions:evaluate",
            &json!({"question": question(&amount)}),
        );
        assert_ne!(status, 200, "{refused}");
    }

    #[test]
    fn a_profile_counts_the_visible_rows_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let (status, profile) = get(&host, &metrics, &format!("/v1/objects/{TYPE}:profile"));
        assert_eq!(status, 200, "{profile}");
        assert_eq!(profile["rows"], 3);
        assert_eq!(profile["withheldRows"], 0);
        assert_eq!(profile["approximate"], false);
        let property = |name: &str| {
            profile["properties"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["property"] == name)
                .unwrap()
                .clone()
        };
        let amount = property("amount");
        assert_eq!(amount["kind"], "numeric");
        assert_eq!(amount["count"], 3);
        assert_eq!(amount["nulls"], 0);
        assert_eq!(amount["distinct"], 3);
        // The reader's JSON parse keeps the number, not its trailing zeros.
        let number = |value: &Value| value.as_f64().unwrap();
        assert!((number(&amount["min"]) - 5.5).abs() < 1e-9, "{amount}");
        assert!((number(&amount["max"]) - 10.0).abs() < 1e-9, "{amount}");
        let median = amount["quantiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["percent"] == 50)
            .unwrap();
        assert!((number(&median["value"]) - 7.25).abs() < 1e-9, "{amount}");
        let at = property("occurred_at");
        assert_eq!(at["kind"], "temporal");
        assert_eq!(at["histogram"]["kind"], "day");
        assert_eq!(at["histogram"]["bins"].as_array().unwrap().len(), 2);
        let region = property("region");
        assert_eq!(
            (region["kind"].as_str(), region["distinct"].as_u64()),
            (Some("text"), Some(2))
        );
        let (status, _) = get(&host, &metrics, "/v1/objects/shop.Missing:profile");
        assert_ne!(status, 200);
    }
}
