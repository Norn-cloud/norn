//! Layer 4 — Scenario Fork & Multi-Domain xP&A Stress Tests.
//!
//! 1. **Concurrent what-if forks using real E1–E4 APIs** (`CellStore::fork`,
//!    `evaluate_question_in_fork`, `compare_main_and_fork`, `compare_forks`,
//!    `ForkScenario`, `execute_action_in_fork`, `promote_decision_fork`,
//!    `export_fork_commit_evidence`, `verify_fork_commit_evidence`,
//!    `verify_main_receipt`, `verify_main_export_record`), with W9/W10 overtime
//!    deltas driven directly from pinned `ForkScenario` parameters.
//! 2. **Messy-entity Egyptian family-business multi-entity scenario:**
//!    sole proprietorship (EGP) + partnership (USD) sharing `MALL-CFC` custodian
//!    and suppliers, real `CombinedViewReport` cuts (`READ-LOCAL-1`), real
//!    verified `main` export records bound to the actual bilateral transfer
//!    commits (`supply.PosSale` + `supply.StockLot`), and fail-closed
//!    unilateral/unrelated-tip/wrong-cell refusals.

use std::collections::BTreeMap;

use norn_cell::compute::{
    ForkQuestionResult, WorldCompareDelta, compare_forks, compare_main_and_fork,
};
use norn_cell::export::ExportRecord;
use norn_cell::{
    CellError, ForkActionCall, ForkMergeRefusalReason, ForkScenario, execute_action_in_fork,
    promote_decision_fork, verify_fork_commit_evidence, verify_main_export_record,
    verify_main_receipt,
};
use norn_ir::{Decimal, RuntimeValue, UtcTimestamp};
use norn_kernel::{Entropy, LogicalTime, ScenarioClock, World, WorldId};
use serde_json::{Value, json};

use super::actions::seed_egyptian_family_fixture;
use super::cuts::{
    bucket_decimal_exact, cell_id_for_entity, dst_coverage_spec, evaluate_multi_cell_report,
};
use super::env::{AllowAll, DEFAULT_CLOCK_MS, EntityCell, RetailCell};
use super::operating_group::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupRefusal,
    OperatingGroupTier, TransferLegEvidence, evaluate_operating_group_tier,
    slice_from_cut_snapshot,
};
use super::prng::{DAY_MS, SplitMix64};

fn json_object(value: Value) -> Result<norn_kernel::cell_host::JsonObject, String> {
    serde_json::from_value(value).map_err(|e| format!("json object: {e}"))
}

fn scope_map(org_id: &str, env_id: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), org_id.to_owned()),
        ("environmentId".to_owned(), env_id.to_owned()),
    ])
}

fn fork_entity_cell(parent: &EntityCell, world: &str) -> Result<RetailCell, String> {
    let head = parent.head()?;
    let fork_store = parent
        .cell()?
        .store()
        .fork(world, &head)
        .map_err(|e| format!("fork {world}: {e}"))?;
    let mut fork = RetailCell::new(fork_store, parent.scope.clone(), |c| c.payload.clone())
        .map_err(|e| format!("open fork {world}: {e}"))?;
    fork.advance_to(parent.now());
    Ok(fork)
}

fn run_fork_action(
    fork: &mut RetailCell,
    parent: &EntityCell,
    action_id: &str,
    actor: &str,
    key: &str,
    input: Value,
) -> Result<(), String> {
    let call = ForkActionCall {
        action_id: action_id.to_owned(),
        scope: scope_map(&parent.org_id, &parent.env_id),
        actor: actor.to_owned(),
        input: json_object(input)?,
        idempotency_key: key.to_owned(),
        object_type: None,
        trace: None,
        approved_intent: None,
    };
    let next = LogicalTime::from_millis(fork.now().as_millis().saturating_add(1_000));
    fork.advance_to(next);
    execute_action_in_fork(fork, &parent.pack.definitions, &call, &AllowAll)
        .map(|_| ())
        .map_err(|e| format!("fork action {action_id} ({key}): {e}"))
}

/// Layer 4A: Run four concurrent what-if forks (pricing promo, tariff/FX shock,
/// parameter-driven overtime shift reallocation, and store opening + nested
/// sub-fork) asserting `WORLD-ISOLATE-1`, `FORK-EVIDENCE-1`,
/// `FORK-NONINTERFERE`, and `FORK-MERGE-1`.
///
/// # Errors
/// Returns an error if any fork isolation, comparison, evidence, or promotion check fails.
pub fn run_concurrent_what_if_forks(seed: u64) -> Result<(), String> {
    let mut rng = SplitMix64::new(seed).fork(0xF024_0001);
    let mut main_book = seed_fork_baseline_book(seed)?;
    let main_head_before = main_book.head()?;
    let main_image_before = main_book
        .cell()?
        .store()
        .image()
        .map_err(|e| format!("main image before: {e}"))?;

    let (promo_fork, tariff_fork) = run_promo_and_tariff_forks(&main_book)?;
    run_overtime_scenario_forks(&main_book, &mut rng)?;
    assert_fork_evidence_fence(&main_book.cell_id, "pricing-promo", &promo_fork)?;
    assert_fork_evidence_fence(&main_book.cell_id, "tariff-shock", &tariff_fork)?;

    if main_book.head()? != main_head_before {
        return Err("exploratory forks mutated main head".to_owned());
    }
    let main_image_after = main_book
        .cell()?
        .store()
        .image()
        .map_err(|e| format!("main image after: {e}"))?;
    if main_image_after != main_image_before {
        return Err("FORK-NONINTERFERE violated: main store image changed".to_owned());
    }
    verify_store_opening_promotion_and_nested_refusal(&mut main_book, main_head_before.commits)
}

fn seed_fork_baseline_book(seed: u64) -> Result<EntityCell, String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(seed, 40),
        "LE-XPA-MAIN",
        "EGP",
        "org_xpa_forks",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Cairo Fashion Flagship", "sole_proprietorship")?;
    book.register_sku("SKU-DRESS", "dry_goods", "each")?;
    book.register_store(
        "ST-FLAG",
        "CFC Flagship",
        "CUST-CFC",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-DRESS",
        "COST-BASE",
        "SUP-ATELIER",
        "100",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-BASE",
        "ST-FLAG",
        "SKU-DRESS",
        "CUST-CFC",
        "100",
        "each",
        "100",
        "2026-08-03T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-BASE-1",
        "ST-FLAG",
        "SKU-DRESS",
        "10",
        "each",
        "2000",
        "100",
        "COST-BASE",
        "2026-09-02T12:00:00Z",
    )?;
    book.ingest_baseline_feeds(
        "ST-FLAG",
        "SKU-DRESS",
        "100",
        "2026-09-02T12:00:00Z",
        "forkbase",
    )?;
    Ok(book)
}

fn run_promo_and_tariff_forks(main_book: &EntityCell) -> Result<(RetailCell, RetailCell), String> {
    let gm_plan = main_book
        .pack
        .retail_question("supply.retail.store_gross_margin_weekly")?;
    let (promo_fork, promo_gm) = run_pricing_promo_fork(main_book, &gm_plan)?;
    let tariff_fork = run_tariff_shock_fork(main_book, &gm_plan, &promo_gm)?;
    Ok((promo_fork, tariff_fork))
}

fn run_pricing_promo_fork(
    main_book: &EntityCell,
    gm_plan: &norn_checker::metric::compute::QuestionPlan,
) -> Result<(RetailCell, ForkQuestionResult), String> {
    let rev_plan = main_book
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let base_ctx = main_book.query_context();
    let main_rev = main_book
        .cell()?
        .store()
        .evaluate_question(&rev_plan, base_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("main rev: {e}"))?;
    let main_gm = main_book
        .cell()?
        .store()
        .evaluate_question(gm_plan, base_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("main gm: {e}"))?;

    let mut promo_fork = fork_entity_cell(main_book, "whatif-pricing-promo")?;
    run_fork_action(
        &mut promo_fork,
        main_book,
        "supply.pos_sale.record",
        "clerk:hana",
        "fork-promo-sale-1",
        json!({
            "sale_id": "SALE-PROMO-1", "store_id": "ST-FLAG", "sku_id": "SKU-DRESS",
            "sold_qty": "10", "unit": "each", "net_revenue": "1600",
            "applied_unit_cost": "100", "cost_status": "costed",
            "costed_ref": "COST-BASE", "currency": "EGP", "sold_at": "2026-09-03T12:00:00Z",
        }),
    )?;
    let promo_rev = promo_fork
        .store()
        .evaluate_question_in_fork(&rev_plan, base_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("promo rev: {e}"))?;
    let promo_gm = promo_fork
        .store()
        .evaluate_question_in_fork(gm_plan, base_ctx, |_| Ok(true))
        .map_err(|e| format!("promo gm: {e}"))?;
    if !promo_rev.simulated || promo_rev.fork_query_pin == main_rev.query_pin {
        return Err("promo fork answer must be simulated with distinct query_pin".to_owned());
    }
    let promo_rev_diff =
        compare_main_and_fork(&main_rev, &promo_rev).map_err(|e| format!("diff promo rev: {e}"))?;
    let promo_gm_diff =
        compare_main_and_fork(&main_gm, &promo_gm).map_err(|e| format!("diff promo gm: {e}"))?;
    assert_delta_contains(&promo_rev_diff.deltas, "1600", "pricing promo rev delta")?;
    assert_delta_contains(&promo_gm_diff.deltas, "600", "pricing promo gm delta")?;
    Ok((promo_fork, promo_gm))
}

fn run_tariff_shock_fork(
    main_book: &EntityCell,
    gm_plan: &norn_checker::metric::compute::QuestionPlan,
    promo_gm: &ForkQuestionResult,
) -> Result<RetailCell, String> {
    let mut tariff_fork = fork_entity_cell(main_book, "whatif-tariff-fx-shock")?;
    run_fork_action(
        &mut tariff_fork,
        main_book,
        "supply.sku.register",
        "eng:dina",
        "fork-tariff-sku-1",
        json!({
            "sku_id": "SKU-DRESS-IMPORT", "category": "dry_goods",
            "unit_of_measure": "each", "shelf_life_days": 365,
        }),
    )?;
    run_fork_action(
        &mut tariff_fork,
        main_book,
        "supply.unit_cost.record",
        "eng:dina",
        "fork-tariff-cost-1",
        json!({
            "sku_id": "SKU-DRESS-IMPORT", "cost_ref": "COST-TARIFF-SHOCK",
            "owner_entity_id": "LE-XPA-MAIN", "supplier_id": "SUP-ATELIER",
            "unit_cost": "145", "currency": "EGP", "effective_from": "2026-09-01T00:00:00Z",
        }),
    )?;
    run_fork_action(
        &mut tariff_fork,
        main_book,
        "supply.pos_sale.record",
        "clerk:hana",
        "fork-tariff-sale-1",
        json!({
            "sale_id": "SALE-TARIFF-1", "store_id": "ST-FLAG", "sku_id": "SKU-DRESS-IMPORT",
            "sold_qty": "10", "unit": "each", "net_revenue": "2000",
            "applied_unit_cost": "145", "cost_status": "costed",
            "costed_ref": "COST-TARIFF-SHOCK", "currency": "EGP", "sold_at": "2026-09-04T12:00:00Z",
        }),
    )?;
    let tariff_gm = tariff_fork
        .store()
        .evaluate_question_in_fork(gm_plan, main_book.query_context(), |_| Ok(true))
        .map_err(|e| format!("tariff gm: {e}"))?;
    let promo_vs_tariff =
        compare_forks(promo_gm, &tariff_gm).map_err(|e| format!("compare forks: {e}"))?;
    assert_delta_contains(
        &promo_vs_tariff.deltas,
        "-50",
        "promo vs tariff margin delta",
    )?;
    Ok(tariff_fork)
}

#[derive(Debug, Clone, Copy)]
struct OvertimeScenarioSpec {
    world: &'static str,
    shift_hours_added: i64,
    opening_share_pct: i64,
    units_per_shift_hour: i64,
    unit_price_egp: i64,
}

struct DerivedOvertimeSales {
    opening_week_qty: i64,
    opening_week_rev: i64,
    followup_week_qty: i64,
    followup_week_rev: i64,
}

impl OvertimeScenarioSpec {
    fn to_param_map(self) -> BTreeMap<String, RuntimeValue> {
        BTreeMap::from([
            (
                "shift_hours_added".to_owned(),
                RuntimeValue::from(self.shift_hours_added),
            ),
            (
                "opening_share_pct".to_owned(),
                RuntimeValue::from(self.opening_share_pct),
            ),
            (
                "units_per_shift_hour".to_owned(),
                RuntimeValue::from(self.units_per_shift_hour),
            ),
            (
                "unit_price_egp".to_owned(),
                RuntimeValue::from(self.unit_price_egp),
            ),
        ])
    }

    fn derive_from_params(
        params: &BTreeMap<String, RuntimeValue>,
    ) -> Result<DerivedOvertimeSales, String> {
        let param_i64 = |key: &str| -> Result<i64, String> {
            match params.get(key) {
                Some(RuntimeValue::Number(dec)) => dec
                    .canonical()
                    .parse::<i64>()
                    .map_err(|e| format!("parse overtime param {key}: {e}")),
                other => Err(format!("missing integer overtime param {key}: {other:?}")),
            }
        };
        let total_hours = param_i64("shift_hours_added")?;
        let opening_pct = param_i64("opening_share_pct")?;
        let units_per_hour = param_i64("units_per_shift_hour")?;
        let unit_price = param_i64("unit_price_egp")?;
        let opening_hours = total_hours
            .saturating_mul(opening_pct)
            .checked_div(100)
            .ok_or("overtime hour split divide failed")?;
        let followup_hours = total_hours.saturating_sub(opening_hours);
        let opening_week_qty = opening_hours.saturating_mul(units_per_hour);
        let followup_week_qty = followup_hours.saturating_mul(units_per_hour);
        Ok(DerivedOvertimeSales {
            opening_week_qty,
            opening_week_rev: opening_week_qty.saturating_mul(unit_price),
            followup_week_qty,
            followup_week_rev: followup_week_qty.saturating_mul(unit_price),
        })
    }
}

/// Run two differently parameterized overtime scenarios (`12h` vs `24h` added
/// shifts), driving every simulated W9/W10 sale quantity & revenue directly
/// from the `ForkScenario` parameter map and proving via `compare_forks` that
/// the W9/W10 KPI deltas scale linearly with `shift_hours_added`.
fn run_overtime_scenario_forks(main_book: &EntityCell, rng: &mut SplitMix64) -> Result<(), String> {
    let rev_plan = main_book
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let unit_price = i64::try_from(rng.range_u64(180, 240)).unwrap_or(200);
    let compact_spec = OvertimeScenarioSpec {
        world: "whatif-overtime-12h",
        shift_hours_added: 12,
        opening_share_pct: 50,
        units_per_shift_hour: 1,
        unit_price_egp: unit_price,
    };
    let expanded_spec = OvertimeScenarioSpec {
        world: "whatif-overtime-24h",
        shift_hours_added: 24,
        opening_share_pct: 50,
        units_per_shift_hour: 1,
        unit_price_egp: unit_price,
    };

    let (compact_followup, compact_followup_rev) =
        evaluate_overtime_scenario(main_book, &rev_plan, compact_spec, rng)?;
    let (expanded_followup, expanded_followup_rev) =
        evaluate_overtime_scenario(main_book, &rev_plan, expanded_spec, rng)?;

    let expected_step = expanded_followup_rev.saturating_sub(compact_followup_rev);
    let fork_comparison = compare_forks(&compact_followup, &expanded_followup)
        .map_err(|e| format!("compare overtime forks: {e}"))?;
    assert_delta_contains(
        &fork_comparison.deltas,
        &expected_step.to_string(),
        "overtime 12h vs 24h parameter-driven delta",
    )
}

fn evaluate_overtime_scenario(
    main_book: &EntityCell,
    rev_plan: &norn_checker::metric::compute::QuestionPlan,
    spec: OvertimeScenarioSpec,
    rng: &mut SplitMix64,
) -> Result<(ForkQuestionResult, i64), String> {
    let params = spec.to_param_map();
    let derived = OvertimeScenarioSpec::derive_from_params(&params)?;
    let mut overtime_cell = fork_entity_cell(main_book, spec.world)?;
    run_fork_action(
        &mut overtime_cell,
        main_book,
        "supply.pos_sale.record",
        "clerk:hana",
        &format!("fork-ot-w9-{}", spec.shift_hours_added),
        json!({
            "sale_id": format!("SALE-OT-W9-{}", spec.shift_hours_added),
            "store_id": "ST-FLAG", "sku_id": "SKU-DRESS",
            "sold_qty": derived.opening_week_qty.to_string(), "unit": "each",
            "net_revenue": derived.opening_week_rev.to_string(),
            "applied_unit_cost": "100", "cost_status": "costed",
            "costed_ref": "COST-BASE", "currency": "EGP",
            "sold_at": "2026-09-05T22:00:00Z",
        }),
    )?;
    run_fork_action(
        &mut overtime_cell,
        main_book,
        "supply.pos_sale.record",
        "clerk:hana",
        &format!("fork-ot-w10-{}", spec.shift_hours_added),
        json!({
            "sale_id": format!("SALE-OT-W10-{}", spec.shift_hours_added),
            "store_id": "ST-FLAG", "sku_id": "SKU-DRESS",
            "sold_qty": derived.followup_week_qty.to_string(), "unit": "each",
            "net_revenue": derived.followup_week_rev.to_string(),
            "applied_unit_cost": "100", "cost_status": "costed",
            "costed_ref": "COST-BASE", "currency": "EGP",
            "sold_at": "2026-09-10T21:00:00Z",
        }),
    )?;

    let recorded_now = overtime_cell.now();
    let mut scenario = ForkScenario::open(
        overtime_cell,
        params,
        ScenarioClock::new(LogicalTime::from_millis(DEFAULT_CLOCK_MS), recorded_now),
        Entropy::from_bytes(rng.next_entropy_bytes()),
    )
    .map_err(|e| format!("open overtime ForkScenario: {e}"))?;
    let pinned_derived =
        OvertimeScenarioSpec::derive_from_params(&scenario.context().parameter_pins)?;
    if pinned_derived.opening_week_rev != derived.opening_week_rev
        || pinned_derived.followup_week_rev != derived.followup_week_rev
    {
        return Err("ForkScenario parameter pins drifted from simulation inputs".to_owned());
    }
    let obs = scenario
        .observations("overtime-shift-reallocation")
        .map_err(|e| format!("observations: {e}"))?;
    if obs.world
        != (World::Fork {
            id: WorldId(spec.world.to_owned()),
        })
    {
        return Err("overtime ForkScenario world pin mismatch".to_owned());
    }
    let after_adv = verify_overtime_effective_clock_deltas(
        main_book,
        &mut scenario,
        rev_plan,
        recorded_now,
        pinned_derived.opening_week_rev,
        pinned_derived.followup_week_rev,
    )?;
    Ok((after_adv, pinned_derived.followup_week_rev))
}

type RetailScenario = ForkScenario<
    norn_kernel::FactEvent,
    norn_kernel::cell_host::JsonObject,
    norn_kernel::cell_host::JsonObject,
>;

fn verify_overtime_effective_clock_deltas(
    main_book: &EntityCell,
    scenario: &mut RetailScenario,
    rev_plan: &norn_checker::metric::compute::QuestionPlan,
    recorded_now: LogicalTime,
    expected_opening_rev: i64,
    expected_followup_rev: i64,
) -> Result<ForkQuestionResult, String> {
    let mut base_ctx = main_book.query_context();
    base_ctx.now_ms = recorded_now.as_millis();
    let cut_plan = rev_plan
        .with_effective_end(UtcTimestamp("2026-09-07T12:00:00Z".to_owned()))
        .ok_or("narrow effective end for overtime baseline")?;
    let main_at_cut = main_book
        .cell()?
        .store()
        .evaluate_question(&cut_plan, base_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("main rev at cut: {e}"))?;
    let before_adv = scenario
        .evaluate_question(rev_plan, base_ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("overtime eval before advance: {e}"))?;
    if !before_adv.simulated {
        return Err("overtime ForkScenario answer must be simulated".to_owned());
    }
    let opening_diff = compare_main_and_fork(&main_at_cut, &before_adv)
        .map_err(|e| format!("compare main vs overtime fork: {e}"))?;
    assert_delta_contains(
        &opening_diff.deltas,
        &expected_opening_rev.to_string(),
        "overtime W9 parameter-driven revenue delta",
    )?;
    if bucket_decimal_exact(&before_adv.result, &["EGP", "ST-FLAG", "2026-09-07"]).is_some() {
        return Err("W10 overtime sale leaked before virtual effective clock advanced".to_owned());
    }

    scenario
        .advance_effective_by(DAY_MS.saturating_mul(7))
        .map_err(|e| format!("advance effective clock: {e}"))?;
    let after_adv = scenario
        .evaluate_question(rev_plan, base_ctx, |_| Ok(true))
        .map_err(|e| format!("overtime eval after advance: {e}"))?;
    let followup_rev = bucket_decimal_exact(&after_adv.result, &["EGP", "ST-FLAG", "2026-09-07"])
        .ok_or("W10 overtime revenue missing after advancing effective clock")?;
    if followup_rev.canonical() != expected_followup_rev.to_string()
        || after_adv.fork_query_pin == before_adv.fork_query_pin
    {
        return Err(format!(
            "overtime W10 shift delta mismatch: got {followup_rev}, expected {expected_followup_rev}"
        ));
    }
    Ok(after_adv)
}

fn assert_fork_evidence_fence(
    main_cell_id: &norn_ir::cell_id::CellId,
    label: &str,
    fork_cell: &RetailCell,
) -> Result<(), String> {
    let tip = fork_cell
        .store()
        .verified_head()
        .map_err(|e| format!("{label} head: {e}"))?
        .commits;
    if !matches!(
        fork_cell.store().export_record(tip),
        Err(CellError::ForkEvidenceForbidden)
    ) {
        return Err(format!(
            "{label}: fork commit {tip} MUST refuse main export_record"
        ));
    }
    let fork_ev = fork_cell
        .store()
        .export_fork_commit_evidence(tip)
        .map_err(|e| format!("{label} export_fork_commit_evidence: {e}"))?
        .ok_or_else(|| format!("{label}: missing fork commit evidence"))?;
    verify_fork_commit_evidence(&fork_ev, main_cell_id)
        .map_err(|e| format!("{label} verify_fork_commit_evidence: {e}"))?;
    if !matches!(
        verify_main_receipt(&fork_ev.record.receipt),
        Err(CellError::ForkEvidenceForbidden)
    ) || !matches!(
        verify_main_export_record(&fork_ev.record),
        Err(CellError::ForkEvidenceForbidden)
    ) {
        return Err(format!("{label}: fork evidence passed main verifier"));
    }
    Ok(())
}

fn verify_store_opening_promotion_and_nested_refusal(
    main_book: &mut EntityCell,
    main_horizon: u64,
) -> Result<(), String> {
    let mut open_fork = fork_entity_cell(main_book, "decision-store-opening")?;
    let open_call = ForkActionCall {
        action_id: "supply.store.register".to_owned(),
        scope: scope_map(&main_book.org_id, &main_book.env_id),
        actor: "eng:dina".to_owned(),
        input: json_object(json!({
            "store_id": "ST-NEW-ZAYED", "name": "Sheikh Zayed Pop-Up",
            "entity_id": "LE-XPA-MAIN", "custodian_id": "CUST-ZAYED",
            "location_kind": "standalone", "mall_code": null, "currency": "EGP",
        }))?,
        idempotency_key: "fork-open-zayed-1".to_owned(),
        object_type: None,
        trace: None,
        approved_intent: None,
    };
    let next_ts = LogicalTime::from_millis(open_fork.now().as_millis().saturating_add(1_000));
    open_fork.advance_to(next_ts);
    execute_action_in_fork(
        &mut open_fork,
        &main_book.pack.definitions,
        &open_call,
        &AllowAll,
    )
    .map_err(|e| format!("open store in decision fork: {e}"))?;

    let open_head = open_fork
        .store()
        .verified_head()
        .map_err(|e| format!("open_fork head: {e}"))?;
    let nested_store = open_fork
        .store()
        .fork("scenario-nested-promo", &open_head)
        .map_err(|e| format!("nested fork: {e}"))?;
    if nested_store.fork_pin().map(|p| p.main_horizon) != Some(main_horizon) {
        return Err("nested sub-fork failed to inherit main_horizon unchanged".to_owned());
    }
    let mut nested_cell =
        RetailCell::new(nested_store, main_book.scope.clone(), |c| c.payload.clone())
            .map_err(|e| format!("open nested cell: {e}"))?;
    nested_cell.advance_to(open_fork.now());
    let defs = &main_book.pack.definitions;
    let nested_promote = promote_decision_fork(
        main_book.cell_mut()?,
        &mut nested_cell,
        defs,
        &AllowAll,
        std::slice::from_ref(&open_call),
    );
    if !matches!(
        &nested_promote,
        Err(CellError::ForkMergeRefused { reason })
            if matches!(reason.as_ref(), ForkMergeRefusalReason::NestedForkForbidden { .. })
    ) {
        return Err(format!(
            "nested sub-fork must refuse NestedForkForbidden, got {nested_promote:?}"
        ));
    }

    let report = promote_decision_fork(
        main_book.cell_mut()?,
        &mut open_fork,
        defs,
        &AllowAll,
        &[open_call],
    )
    .map_err(|e| format!("promote decision-store-opening: {e}"))?;
    if report.promoted.len() != 1
        || main_book
            .cell()?
            .store()
            .read()
            .object("main", "supply.Store", "ST-NEW-ZAYED")
            .map_err(|e| format!("read promoted store: {e}"))?
            .is_none()
    {
        return Err("promoted store ST-NEW-ZAYED missing on main after FORK-MERGE-1".to_owned());
    }
    Ok(())
}

fn assert_delta_contains(
    deltas: &BTreeMap<String, WorldCompareDelta>,
    expected_canonical: &str,
    label: &str,
) -> Result<(), String> {
    let found = deltas.values().any(|d| {
        d.delta_value.as_ref().map(Decimal::canonical).as_deref() == Some(expected_canonical)
    });
    if !found {
        return Err(format!(
            "{label}: expected delta `{expected_canonical}` in {deltas:?}"
        ));
    }
    Ok(())
}

/// Layer 4B: Messy-entity Egyptian family-business multi-entity scenario
/// (sole proprietorship in EGP + partnership in USD sharing `MALL-CFC` custodian
/// and suppliers), deriving revenue directly from real disclosed `QueryResult`s
/// and binding bilateral inter-entity elimination to the actual committed
/// transfer `ExportRecord`s (`SALE-IE-CFC-TRIM-01` + `LOT-IE-CFC-TRIM-01`).
///
/// # Errors
/// Returns an error if any entity book or multi-entity view invariant fails.
pub fn run_egyptian_family_business_scenario(seed: u64) -> Result<(), String> {
    let fixture = seed_egyptian_family_fixture(seed)?;
    let spec = dst_coverage_spec();
    let entity_books = [&fixture.sole_book, &fixture.partner_book];
    let report = evaluate_multi_cell_report(&entity_books, &spec, |idx, _, plan, ctx| {
        let current = entity_books
            .get(idx)
            .ok_or_else(|| CellError::Engine("missing cell".to_owned()))?;
        current
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })?;
    let sole_snapshot = report.cells.first().ok_or("missing sole cut snapshot")?;
    let partner_snapshot = report.cells.get(1).ok_or("missing partner cut snapshot")?;
    let slices = [
        slice_from_cut_snapshot(
            "LE-NILE-SOLE",
            "EGP",
            sole_snapshot,
            "supply.retail.store_revenue_weekly",
        )?,
        slice_from_cut_snapshot(
            "LE-CAIRO-PARTNER",
            "USD",
            partner_snapshot,
            "supply.retail.store_revenue_weekly",
        )?,
    ];
    let fx_rates = [FxRateEntry {
        from_currency: "USD".to_owned(),
        to_currency: "EGP".to_owned(),
        rate_scaled: 48_500_000,
        as_of: "2026-09-06T23:59:59Z".to_owned(),
        rate_table_pin: "fx:cbe:2026-09-06".to_owned(),
    }];
    verify_egyptian_sbs_and_fx(&slices, &fx_rates)?;
    verify_egyptian_bilateral_elimination(
        &slices,
        &fx_rates,
        &fixture.partner_sale_export,
        &fixture.sole_buyer_lot_export,
        &fixture.sole_unrelated_tip_export,
    )
}

fn verify_egyptian_sbs_and_fx(
    slices: &[EntityReportingSlice],
    fx_rates: &[FxRateEntry],
) -> Result<(), String> {
    let side_by_side =
        evaluate_operating_group_tier(OperatingGroupTier::SideBySide, slices, None, &[], &[], None)
            .map_err(|e| format!("side_by_side: {e:?}"))?;
    if side_by_side
        .per_currency_revenue
        .get("EGP")
        .map(Decimal::canonical)
        .as_deref()
        != Some("27000")
        || side_by_side
            .per_currency_revenue
            .get("USD")
            .map(Decimal::canonical)
            .as_deref()
            != Some("1500")
    {
        return Err(format!("unexpected side_by_side: {side_by_side:?}"));
    }
    let fx_translated = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        slices,
        Some("EGP"),
        fx_rates,
        &[],
        None,
    )
    .map_err(|e| format!("fx_translated: {e:?}"))?;
    if fx_translated
        .translated_revenue
        .as_ref()
        .map(Decimal::canonical)
        .as_deref()
        != Some("99750")
    {
        return Err(format!("unexpected fx_translated: {fx_translated:?}"));
    }
    Ok(())
}

fn verify_egyptian_bilateral_elimination(
    slices: &[EntityReportingSlice],
    fx_rates: &[FxRateEntry],
    partner_sale_export: &ExportRecord,
    sole_buyer_lot_export: &ExportRecord,
    sole_unrelated_tip_export: &ExportRecord,
) -> Result<(), String> {
    let matched = InterEntityTransfer {
        transfer_id: "IE-CFC-TRIM-01".to_owned(),
        seller_entity_id: "LE-CAIRO-PARTNER".to_owned(),
        buyer_entity_id: "LE-NILE-SOLE".to_owned(),
        currency: "USD".to_owned(),
        settlement_fx_rate: None,
        seller_leg: Some(TransferLegEvidence {
            export_record: partner_sale_export.clone(),
            object_type: "supply.PosSale".to_owned(),
            object_id: "SALE-IE-CFC-TRIM-01".to_owned(),
        }),
        buyer_leg: Some(TransferLegEvidence {
            export_record: sole_buyer_lot_export.clone(),
            object_type: "supply.StockLot".to_owned(),
            object_id: "LOT-IE-CFC-TRIM-01".to_owned(),
        }),
    };
    let netted = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        slices,
        Some("EGP"),
        fx_rates,
        std::slice::from_ref(&matched),
        None,
    )
    .map_err(|e| format!("inter_entity_netted: {e:?}"))?;
    if netted
        .net_group_revenue
        .as_ref()
        .map(Decimal::canonical)
        .as_deref()
        != Some("85200")
        || netted
            .eliminated_inter_entity_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("14550")
    {
        return Err(format!("unexpected inter_entity_netted: {netted:?}"));
    }

    for (label, bad_xfer) in [
        (
            "unilateral transfer",
            InterEntityTransfer {
                buyer_leg: None,
                ..matched.clone()
            },
        ),
        (
            "unrelated ledger-tip export",
            InterEntityTransfer {
                buyer_leg: Some(TransferLegEvidence {
                    export_record: sole_unrelated_tip_export.clone(),
                    object_type: "supply.StockLot".to_owned(),
                    object_id: "LOT-IE-CFC-TRIM-01".to_owned(),
                }),
                ..matched
            },
        ),
    ] {
        let res = evaluate_operating_group_tier(
            OperatingGroupTier::InterEntityNetted,
            slices,
            Some("EGP"),
            fx_rates,
            &[bad_xfer],
            None,
        );
        if !matches!(
            res,
            Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer { .. })
        ) {
            return Err(format!("{label} must refuse, got {res:?}"));
        }
    }
    Ok(())
}
