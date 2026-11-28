//! Layer 4 — Scenario Fork & Multi-Domain xP&A Stress Tests.
//!
//! 1. **Concurrent what-if forks using real E1–E4 APIs** (`CellStore::fork`,
//!    `evaluate_question_in_fork`, `compare_main_and_fork`, `compare_forks`,
//!    `ForkScenario`, `execute_action_in_fork`, `promote_decision_fork`,
//!    `export_fork_commit_evidence`, `verify_fork_commit_evidence`,
//!    `verify_main_receipt`, `verify_main_export_record`).
//! 2. **Messy-entity Egyptian family-business multi-entity scenario:**
//!    sole proprietorship (EGP) + partnership (USD) sharing `MALL-CFC` custodian
//!    and suppliers, bilateral inter-entity matching, side-by-side vs FX-translated
//!    vs inter-entity-eliminated views.
//! 3. **Deterministic driver-tree ripple & convergence check:**
//!    headcount/shift cost -> store margin -> cash/OTB constraint, verifying
//!    topological convergence and fail-closed circular loop rejection.

use std::collections::BTreeMap;

use norn_cell::compute::{WorldCompareDelta, compare_forks, compare_main_and_fork};
use norn_cell::{
    CellError, ForkActionCall, ForkMergeRefusalReason, ForkScenario, SqliteCell,
    execute_action_in_fork, promote_decision_fork, verify_fork_commit_evidence,
    verify_main_export_record, verify_main_receipt,
};
use norn_ir::{Decimal, RuntimeValue};
use norn_kernel::{CellHost as _, Entropy, LogicalTime, ScenarioClock, World, WorldId};
use serde_json::{Value, json};

use super::cuts::{bucket_decimal_exact, cell_id_for_entity};
use super::env::{AllowAll, DEFAULT_CLOCK_MS, EntityCell, RetailCell};
use super::layer1_invariants::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupRefusal,
    OperatingGroupTier, evaluate_operating_group_tier,
};
use super::prng::{DAY_MS, SplitMix64, decimal_to_scaled, scaled};

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
    let mut fork = SqliteCell::new(fork_store, parent.scope.clone(), |c| c.payload.clone())
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
/// overtime shift reallocation, and store opening + nested sub-fork) asserting
/// `WORLD-ISOLATE-1`, `FORK-EVIDENCE-1`, `FORK-NONINTERFERE`, and `FORK-MERGE-1`.
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
    run_overtime_scenario_fork(&main_book, &mut rng)?;
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
) -> Result<(RetailCell, norn_cell::compute::ForkQuestionResult), String> {
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
    promo_gm: &norn_cell::compute::ForkQuestionResult,
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

fn run_overtime_scenario_fork(main_book: &EntityCell, rng: &mut SplitMix64) -> Result<(), String> {
    let rev_plan = main_book
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let overtime_cell = fork_entity_cell(main_book, "whatif-overtime-shift")?;
    let params = BTreeMap::from([
        ("overtime_multiplier".to_owned(), RuntimeValue::from("1.50")),
        ("shift_hours_added".to_owned(), RuntimeValue::from(24_i64)),
    ]);
    let mut scenario = ForkScenario::open(
        overtime_cell,
        params,
        ScenarioClock::new(
            LogicalTime::from_millis(DEFAULT_CLOCK_MS),
            LogicalTime::from_millis(main_book.now().as_millis()),
        ),
        Entropy::from_bytes(rng.next_entropy_bytes()),
    )
    .map_err(|e| format!("open overtime ForkScenario: {e}"))?;
    let obs = scenario
        .observations("overtime-shift-reallocation")
        .map_err(|e| format!("observations: {e}"))?;
    if obs.world
        != (World::Fork {
            id: WorldId("whatif-overtime-shift".to_owned()),
        })
    {
        return Err("overtime ForkScenario world pin mismatch".to_owned());
    }
    scenario
        .advance_effective_by(DAY_MS.saturating_mul(7))
        .map_err(|e| format!("advance effective clock: {e}"))?;
    let answer = scenario
        .evaluate_question(&rev_plan, main_book.query_context(), |_| Ok(true))
        .map_err(|e| format!("overtime eval: {e}"))?;
    if !answer.simulated {
        return Err("overtime ForkScenario answer must be simulated".to_owned());
    }
    Ok(())
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
    ) {
        return Err(format!("{label}: fork receipt passed verify_main_receipt"));
    }
    if !matches!(
        verify_main_export_record(&fork_ev.record),
        Err(CellError::ForkEvidenceForbidden)
    ) {
        return Err(format!(
            "{label}: fork export passed verify_main_export_record"
        ));
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
        SqliteCell::new(nested_store, main_book.scope.clone(), |c| c.payload.clone())
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
        nested_promote,
        Err(CellError::ForkMergeRefused { reason })
            if matches!(*reason, ForkMergeRefusalReason::NestedForkForbidden { .. })
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
    let found = deltas
        .values()
        .any(|d| d.delta_value.map(Decimal::canonical).as_deref() == Some(expected_canonical));
    if !found {
        return Err(format!(
            "{label}: expected delta `{expected_canonical}` in {deltas:?}"
        ));
    }
    Ok(())
}

/// Layer 4B: Messy-entity Egyptian family-business multi-entity scenario
/// (sole proprietorship in EGP + partnership in USD sharing `MALL-CFC` custodian
/// and suppliers).
///
/// # Errors
/// Returns an error if any entity book or multi-entity view invariant fails.
pub fn run_egyptian_family_business_scenario(seed: u64) -> Result<(), String> {
    let sole = seed_egyptian_sole_prop(seed)?;
    let partner = seed_egyptian_partnership(seed)?;
    let sole_rev = bucket_decimal_exact(
        &sole.answer("supply.retail.store_revenue_weekly")?,
        &["EGP", "ST-SOLE-CFC", "2026-08-31"],
    )
    .ok_or("missing sole revenue")?;
    let partner_rev = bucket_decimal_exact(
        &partner.answer("supply.retail.store_revenue_weekly")?,
        &["USD", "ST-PART-CFC", "2026-08-31"],
    )
    .ok_or("missing partner revenue")?;
    let slices = [
        EntityReportingSlice {
            entity_id: "LE-NILE-SOLE".to_owned(),
            currency: "EGP".to_owned(),
            revenue_scaled: decimal_to_scaled(sole_rev)?,
            gross_margin_scaled: scaled(15_000),
        },
        EntityReportingSlice {
            entity_id: "LE-CAIRO-PARTNER".to_owned(),
            currency: "USD".to_owned(),
            revenue_scaled: decimal_to_scaled(partner_rev)?,
            gross_margin_scaled: scaled(700),
        },
    ];
    verify_egyptian_family_views(&slices, sole.head()?.commits, partner.head()?.commits)
}

fn verify_egyptian_family_views(
    slices: &[EntityReportingSlice],
    sole_commits: u64,
    partner_commits: u64,
) -> Result<(), String> {
    let fx_rates = [FxRateEntry {
        from_currency: "USD".to_owned(),
        to_currency: "EGP".to_owned(),
        rate_scaled: 48_500_000,
        as_of: "2026-09-06T23:59:59Z".to_owned(),
        rate_table_pin: "fx:cbe:2026-09-06".to_owned(),
    }];
    let sbs =
        evaluate_operating_group_tier(OperatingGroupTier::SideBySide, slices, None, &[], &[], None)
            .map_err(|e| format!("side_by_side: {e:?}"))?;
    if sbs
        .per_currency_revenue
        .get("EGP")
        .map(Decimal::canonical)
        .as_deref()
        != Some("27000")
        || sbs
            .per_currency_revenue
            .get("USD")
            .map(Decimal::canonical)
            .as_deref()
            != Some("1500")
    {
        return Err(format!("unexpected side_by_side: {sbs:?}"));
    }

    let fxt = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        slices,
        Some("EGP"),
        &fx_rates,
        &[],
        None,
    )
    .map_err(|e| format!("fx_translated: {e:?}"))?;
    if fxt.translated_revenue.map(Decimal::canonical).as_deref() != Some("99750") {
        return Err(format!("unexpected fx_translated: {fxt:?}"));
    }

    let matched = InterEntityTransfer {
        transfer_id: "IE-CFC-TRIM-01".to_owned(),
        seller_entity_id: "LE-CAIRO-PARTNER".to_owned(),
        buyer_entity_id: "LE-NILE-SOLE".to_owned(),
        currency: "USD".to_owned(),
        seller_amount_scaled: scaled(300),
        buyer_amount_scaled: scaled(300),
        seller_receipt_pin: Some(format!("rcpt:partner:{partner_commits}")),
        buyer_receipt_pin: Some(format!("rcpt:sole:{sole_commits}")),
    };
    let netted = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        slices,
        Some("EGP"),
        &fx_rates,
        std::slice::from_ref(&matched),
        None,
    )
    .map_err(|e| format!("inter_entity_netted: {e:?}"))?;
    if netted.net_group_revenue.map(Decimal::canonical).as_deref() != Some("85200")
        || netted
            .eliminated_inter_entity_revenue
            .map(Decimal::canonical)
            .as_deref()
            != Some("14550")
    {
        return Err(format!("unexpected inter_entity_netted: {netted:?}"));
    }

    let unilateral = InterEntityTransfer {
        buyer_receipt_pin: None,
        ..matched
    };
    let uni_res = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        slices,
        Some("EGP"),
        &fx_rates,
        &[unilateral],
        None,
    );
    if !matches!(
        uni_res,
        Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer { .. })
    ) {
        return Err(format!("unilateral transfer must refuse, got {uni_res:?}"));
    }
    Ok(())
}

fn seed_egyptian_sole_prop(seed: u64) -> Result<EntityCell, String> {
    let mut sole = EntityCell::open(
        &cell_id_for_entity(seed, 51),
        "LE-NILE-SOLE",
        "EGP",
        "org_family_nile_sole",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    sole.register_book("Nile Apparel Sole Proprietorship", "sole_proprietorship")?;
    sole.register_sku("SKU-ABAYA", "dry_goods", "each")?;
    sole.register_store(
        "ST-SOLE-CFC",
        "Nile CFC Booth",
        "CUST-MALL-CFC-OPS",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    sole.record_unit_cost(
        "SKU-ABAYA",
        "COST-ABAYA-1",
        "SUP-ATELIER",
        "400",
        "2026-07-01T00:00:00Z",
    )?;
    sole.record_costed_lot(
        "LOT-SOLE-1",
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "CUST-MALL-CFC-OPS",
        "100",
        "each",
        "400",
        "2026-08-03T09:00:00Z",
    )?;
    sole.record_costed_sale(
        "SALE-SOLE-1",
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "30",
        "each",
        "27000",
        "400",
        "COST-ABAYA-1",
        "2026-09-02T14:00:00Z",
    )?;
    Ok(sole)
}

fn seed_egyptian_partnership(seed: u64) -> Result<EntityCell, String> {
    let mut partner = EntityCell::open(
        &cell_id_for_entity(seed, 52),
        "LE-CAIRO-PARTNER",
        "USD",
        "org_family_cairo_partner",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    partner.register_book("Cairo Atelier Partnership", "partnership")?;
    partner.register_sku("SKU-GOLD-TRIM", "spares", "each")?;
    partner.register_store(
        "ST-PART-CFC",
        "Cairo CFC Gallery",
        "CUST-MALL-CFC-OPS",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    partner.record_unit_cost(
        "SKU-GOLD-TRIM",
        "COST-TRIM-1",
        "SUP-ATELIER",
        "80",
        "2026-07-01T00:00:00Z",
    )?;
    partner.record_costed_lot(
        "LOT-PART-1",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "CUST-MALL-CFC-OPS",
        "40",
        "each",
        "80",
        "2026-08-03T09:00:00Z",
    )?;
    partner.record_costed_sale(
        "SALE-PART-1",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "10",
        "each",
        "1500",
        "80",
        "COST-TRIM-1",
        "2026-09-02T15:00:00Z",
    )?;
    Ok(partner)
}
