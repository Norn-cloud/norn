//! Core 08 (E3, #1946): `FORK-MERGE-1` action-command replay promotion from a
//! decision fork `D_i` into `main`, and `FORK-LIFE-1` closed-fork enforcement.
//!
//! Every write goes through real `supply-lab` admitted definitions, real
//! `norn_kernel::commit_action`, and a real `ActionAuthorizer` — never copying
//! fork rows, pages, or receipts into `main`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use norn_cell::{
    AdmittedDefinitions, CellError, CellStore, ForkActionCall, ForkMergeRefusalReason,
    ForkScenario, ForkState, InstalledDefinitionInput, MAIN_WORLD, SqliteCell,
    execute_action_in_fork, promote_decision_fork,
};
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use norn_kernel::cell_host::{FactKey, JsonObject};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit, CellHost as _, DecisionTurn,
    Entropy, FactEvent, LogicalTime, ScenarioClock, commit_action,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const SCOPE_STR: &str = "org_demo/dev";
const BASE_CLOCK_MS: u64 = 1_792_972_800_000;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_envelopes(dir: &Path) -> Result<Vec<ArtifactEnvelope>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text =
                fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect()
}

fn load_supply_envelopes() -> Result<Vec<ArtifactEnvelope>, String> {
    let core = repo_root().join("registry/core/artifacts");
    let mut all = Vec::new();
    for name in [
        "core.value_types.entity.json",
        "std.value_types.entity.json",
        "core.units.json",
        "core.time.json",
    ] {
        let text =
            fs::read_to_string(core.join(name)).map_err(|error| format!("{name}: {error}"))?;
        let env: ArtifactEnvelope =
            serde_json::from_str(&text).map_err(|error| format!("{name}: {error}"))?;
        all.push(env);
    }
    for pack in ["tracking", "ml", "supply-lab"] {
        all.extend(load_envelopes(
            &repo_root()
                .join("registry/packs")
                .join(pack)
                .join("artifacts"),
        )?);
    }
    Ok(all)
}

fn load_supply_definitions() -> Result<AdmittedDefinitions, String> {
    let all = load_supply_envelopes()?;
    AdmittedDefinitions::from_envelopes(&all).map_err(|error| format!("definitions: {error}"))
}

fn scope_map() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), "org_demo".to_owned()),
        ("environmentId".to_owned(), "dev".to_owned()),
    ])
}

fn json_obj(value: Value) -> Result<JsonObject, String> {
    serde_json::from_value(value).map_err(|error| format!("json object: {error}"))
}

struct AllowAll;

impl ActionAuthorizer for AllowAll {
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

struct DenyActor {
    denied_actor: String,
    denial: ActionDenial,
}

impl ActionAuthorizer for DenyActor {
    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.actor == self.denied_actor {
            return Err(self.denial.clone());
        }
        Ok(())
    }

    fn authorize_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if request.actor == self.denied_actor {
            return Err(self.denial.clone());
        }
        Ok(())
    }

    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.actor == self.denied_actor {
            return Err(self.denial.clone());
        }
        Ok(())
    }
}

fn open_seeded_main(definitions: &AdmittedDefinitions) -> Result<Cell, String> {
    let store = CellStore::open_in_memory(definitions.model().clone())
        .map_err(|error| format!("open_in_memory: {error}"))?;
    let mut cell = Cell::new(store, SCOPE_STR, |cmd| cmd.payload.clone())
        .map_err(|error| format!("cell new: {error}"))?;
    cell.advance_to(LogicalTime::from_millis(BASE_CLOCK_MS));

    let seeds = [
        (
            "supply.warehouse.register",
            "eng:dina",
            "seed-wh-a",
            json!({
                "warehouse_id": "WH-A",
                "name": "Warehouse WH-A",
                "region": "delta",
                "latitude": "30.044420",
                "longitude": "31.235700"
            }),
        ),
        (
            "supply.sku.register",
            "eng:dina",
            "seed-sku-water",
            json!({
                "sku_id": "SKU-WATER",
                "category": "dry_goods",
                "unit_of_measure": "each",
                "shelf_life_days": 365
            }),
        ),
        (
            "supply.po.create",
            "planner:amal",
            "seed-po-base",
            json!({
                "po_id": "PO-BASE-1",
                "supplier_id": "SUP-NILE",
                "promised_at": "2026-11-15T00:00:00Z",
                "currency": "EGP"
            }),
        ),
    ];

    for (idx, (action_id, actor, key, input)) in seeds.into_iter().enumerate() {
        let step = u64::try_from(idx.saturating_add(1)).unwrap_or(1);
        let tick = BASE_CLOCK_MS.saturating_add(step.saturating_mul(1_000));
        cell.advance_to(LogicalTime::from_millis(tick));
        commit_main_action(
            &mut cell,
            definitions,
            action_id,
            actor,
            key,
            input,
            &AllowAll,
        )?;
    }
    Ok(cell)
}

fn commit_main_action(
    cell: &mut Cell,
    definitions: &AdmittedDefinitions,
    action_id: &str,
    actor: &str,
    key: &str,
    input: Value,
    authorizer: &dyn ActionAuthorizer,
) -> Result<ActionOutcome, String> {
    let admitted = definitions
        .action(action_id)
        .ok_or_else(|| format!("missing action {action_id}"))?;
    let catalog = definitions.catalog();
    let action_call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: scope_map(),
        actor: actor.to_owned(),
        approved_intent: None,
        input: json_obj(input)?,
        idempotency_key: key.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: key.to_owned(),
            now: cell.now(),
            world: cell.store().world(),
            consumes: None,
        },
        trace: None,
    };
    commit_action(cell, &catalog, &action_call, authorizer)
        .map_err(|error| format!("commit {action_id} ({key}): {error}"))
}

fn fork_cell(parent: &Cell, world: &str) -> Result<Cell, String> {
    let head = parent
        .store()
        .verified_head()
        .map_err(|error| format!("verified_head: {error}"))?;
    let fork_store = parent
        .store()
        .fork(world, &head)
        .map_err(|error| format!("fork {world}: {error}"))?;
    let mut fork = Cell::new(fork_store, SCOPE_STR, |cmd| cmd.payload.clone())
        .map_err(|error| format!("fork cell new: {error}"))?;
    fork.advance_to(parent.now());
    Ok(fork)
}

fn po_create_call(po_id: &str, actor: &str, key: &str) -> Result<ForkActionCall, String> {
    Ok(ForkActionCall {
        action_id: "supply.po.create".to_owned(),
        scope: scope_map(),
        actor: actor.to_owned(),
        input: json_obj(json!({
            "po_id": po_id,
            "supplier_id": "SUP-NILE",
            "promised_at": "2026-11-20T00:00:00Z",
            "currency": "EGP"
        }))?,
        idempotency_key: key.to_owned(),
        object_type: None,
        trace: None,
        approved_intent: None,
    })
}

fn line_add_call(line_id: &str, po_id: &str, key: &str) -> Result<ForkActionCall, String> {
    Ok(ForkActionCall {
        action_id: "supply.line.add".to_owned(),
        scope: scope_map(),
        actor: "planner:amal".to_owned(),
        input: json_obj(json!({
            "line_id": line_id,
            "po_id": po_id,
            "sku_id": "SKU-WATER",
            "promised_at": "2026-11-20T00:00:00Z",
            "ordered_qty": "250",
            "received_qty": "0",
            "fill_rate": "0",
            "unit": "each",
            "unit_price": "2.5",
            "currency": "EGP"
        }))?,
        idempotency_key: key.to_owned(),
        object_type: None,
        trace: None,
        approved_intent: None,
    })
}

fn fork_receipt_hash(cell: &Cell, commit_seq: u64) -> Result<String, String> {
    cell.store()
        .read()
        .receipts_after(commit_seq.saturating_sub(1), 1)
        .map_err(|error| format!("receipt: {error}"))?
        .first()
        .map(|receipt| receipt.hash.clone())
        .ok_or_else(|| format!("missing receipt at {commit_seq}"))
}

#[test]
fn promote_supply_po_create_replays_into_main_and_closes_decision_fork() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut decision_fork = fork_cell(&main, "D_1")?;

    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(10_000),
    ));
    let po_cmd = po_create_call("PO-PROMOTE-1", "planner:amal", "fork-po-promote-1")?;
    let fork_outcome = execute_action_in_fork(&mut decision_fork, &definitions, &po_cmd, &AllowAll)
        .map_err(|error| format!("fork execute: {error}"))?;
    let ActionOutcome::Committed { receipt, .. } = fork_outcome else {
        return Err("expected fresh fork commit".to_owned());
    };
    let fork_commit_seq = receipt.seq.0;
    let fork_hash = fork_receipt_hash(&decision_fork, fork_commit_seq)?;

    assert!(
        main.store()
            .read()
            .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-PROMOTE-1")
            .map_err(|error| format!("main read: {error}"))?
            .is_none()
    );

    let report = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &definitions,
        &AllowAll,
        std::slice::from_ref(&po_cmd),
    )
    .map_err(|error| format!("promote D_1: {error}"))?;
    assert_eq!(report.source_world, "D_1");
    let promoted = report
        .promoted
        .first()
        .ok_or_else(|| "expected promoted command".to_owned())?;
    assert_eq!(promoted.action_id, "supply.po.create");
    assert_eq!(promoted.fork_commit_seq, fork_commit_seq);
    assert_eq!(
        promoted.target,
        FactKey::new("supply.PurchaseOrder", "PO-PROMOTE-1")
    );
    assert_eq!(promoted.target_version, 1);
    assert_ne!(promoted.main_receipt_hash, fork_hash);

    let main_po = main
        .store()
        .read()
        .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-PROMOTE-1")
        .map_err(|error| format!("main read promoted: {error}"))?
        .ok_or_else(|| "PO-PROMOTE-1 missing on main".to_owned())?;
    assert_eq!(main_po.version, 1);

    assert_eq!(
        decision_fork
            .fork_state()
            .map_err(|error| format!("fork_state: {error}"))?,
        Some(ForkState::Promoted)
    );
    assert!(matches!(
        decision_fork
            .store()
            .read()
            .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-PROMOTE-1"),
        Err(CellError::ForkClosed { ref world, ref state }) if world == "D_1" && state == "promoted"
    ));
    let second_cmd = po_create_call("PO-AFTER-PROMOTE", "planner:amal", "fork-po-after")?;
    assert!(matches!(
        execute_action_in_fork(&mut decision_fork, &definitions, &second_cmd, &AllowAll)
            .err()
            .as_deref(),
        Some(norn_kernel::ActionRefusal::Decision(
            norn_kernel::DecisionRefusal::Host(CellError::ForkClosed { world, state })
        )) if world == "D_1" && state == "promoted"
    ));
    assert!(matches!(
        promote_decision_fork(
            &mut main,
            &mut decision_fork,
            &definitions,
            &AllowAll,
            std::slice::from_ref(&po_cmd),
        ),
        Err(CellError::ForkClosed { ref world, ref state }) if world == "D_1" && state == "promoted"
    ));
    Ok(())
}

#[test]
fn nested_exploratory_subfork_refuses_promotion_and_never_enters_main() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut decision_fork = fork_cell(&main, "D_NESTED")?;
    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(10_000),
    ));
    let po_cmd = po_create_call("PO-D-NESTED", "planner:amal", "d-nested-po")?;
    execute_action_in_fork(&mut decision_fork, &definitions, &po_cmd, &AllowAll)
        .map_err(|error| format!("d_nested execute: {error}"))?;

    let mut sub_fork = fork_cell(&decision_fork, "S_1")?;
    sub_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(11_000),
    ));
    let sub_cmd = po_create_call("PO-EXPLORATORY-S1", "planner:amal", "subfork-po-1")?;
    execute_action_in_fork(&mut sub_fork, &definitions, &sub_cmd, &AllowAll)
        .map_err(|error| format!("sub_fork execute: {error}"))?;

    let nested_err = promote_decision_fork(
        &mut main,
        &mut sub_fork,
        &definitions,
        &AllowAll,
        &[sub_cmd],
    )
    .expect_err("nested sub-fork S_1 must refuse promotion");
    assert!(matches!(
        nested_err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::NestedForkForbidden { world, parent }
                    if world == "S_1" && parent == "D_NESTED"
            )
    ));
    assert!(
        main.store()
            .read()
            .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-EXPLORATORY-S1")
            .map_err(|error| format!("main read s1: {error}"))?
            .is_none()
    );
    Ok(())
}

#[test]
fn promote_refuses_when_read_version_moved_and_material_outcome_changed() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut decision_fork = fork_cell(&main, "D_2")?;

    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(10_000),
    ));
    let cmd1 = po_create_call("PO-BATCH-1", "planner:amal", "d2-cmd-1")?;
    execute_action_in_fork(&mut decision_fork, &definitions, &cmd1, &AllowAll)
        .map_err(|error| format!("d2 cmd1: {error}"))?;

    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(11_000),
    ));
    let cmd2 = line_add_call("PO-BASE-1/1", "PO-BASE-1", "d2-cmd-2")?;
    execute_action_in_fork(&mut decision_fork, &definitions, &cmd2, &AllowAll)
        .map_err(|error| format!("d2 cmd2: {error}"))?;

    main.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(12_000),
    ));
    commit_main_action(
        &mut main,
        &definitions,
        "supply.po.submit",
        "ops:omar",
        "main-submit-po-base",
        json!({ "po_id": "PO-BASE-1" }),
        &AllowAll,
    )?;

    let main_head_before = main
        .store()
        .verified_head()
        .map_err(|error| format!("head before: {error}"))?;
    let main_dump_before = main
        .store()
        .read()
        .dump()
        .map_err(|error| format!("dump before: {error}"))?;

    let err = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &definitions,
        &AllowAll,
        &[cmd1, cmd2],
    )
    .expect_err("stale read version breaking precondition must refuse promotion");
    assert!(matches!(
        err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::MaterialOutcomeChanged {
                    action_id,
                    idempotency_key,
                    moved_reads,
                    ..
                } if action_id == "supply.line.add"
                    && idempotency_key == "d2-cmd-2"
                    && moved_reads.iter().any(|entry| entry.contains("PO-BASE-1"))
            )
    ));
    assert_eq!(
        main.store()
            .verified_head()
            .map_err(|error| format!("head after: {error}"))?,
        main_head_before
    );
    assert_eq!(
        main.store()
            .read()
            .dump()
            .map_err(|error| format!("dump after: {error}"))?,
        main_dump_before
    );
    assert_eq!(
        decision_fork
            .fork_state()
            .map_err(|error| format!("fork_state: {error}"))?,
        Some(ForkState::Open)
    );
    Ok(())
}

#[test]
fn promote_succeeds_after_unrelated_main_commits_when_outcome_matches() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut decision_fork = fork_cell(&main, "D_3")?;

    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(10_000),
    ));
    let cmd = po_create_call("PO-CONCURRENT-OK", "planner:amal", "d3-po-1")?;
    execute_action_in_fork(&mut decision_fork, &definitions, &cmd, &AllowAll)
        .map_err(|error| format!("d3 cmd: {error}"))?;

    main.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(12_000),
    ));
    commit_main_action(
        &mut main,
        &definitions,
        "supply.po.create",
        "planner:amal",
        "main-unrelated-po",
        json!({
            "po_id": "PO-MAIN-OTHER",
            "supplier_id": "SUP-NILE",
            "promised_at": "2026-11-22T00:00:00Z",
            "currency": "EGP"
        }),
        &AllowAll,
    )?;

    let report = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &definitions,
        &AllowAll,
        &[cmd],
    )
    .map_err(|error| format!("promote D_3: {error}"))?;
    assert_eq!(
        report.main_head_after.commits,
        report.main_head_before.commits.saturating_add(1)
    );
    assert!(
        main.store()
            .read()
            .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-CONCURRENT-OK")
            .map_err(|error| format!("read promoted: {error}"))?
            .is_some()
    );
    assert_eq!(
        decision_fork
            .fork_state()
            .map_err(|error| format!("fork_state: {error}"))?,
        Some(ForkState::Promoted)
    );
    Ok(())
}

#[test]
fn promote_refuses_revoked_principal_fail_closed_with_zero_main_writes() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut decision_fork = fork_cell(&main, "D_4")?;

    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(10_000),
    ));
    let cmd = po_create_call("PO-REVOKED-1", "planner:amal", "d4-revoked-1")?;
    execute_action_in_fork(&mut decision_fork, &definitions, &cmd, &AllowAll)
        .map_err(|error| format!("d4 cmd: {error}"))?;

    let main_head_before = main
        .store()
        .verified_head()
        .map_err(|error| format!("head before: {error}"))?;

    let forbidden_authz = DenyActor {
        denied_actor: "planner:amal".to_owned(),
        denial: ActionDenial::Forbidden {
            reason: "actor.revoked".to_owned(),
        },
    };
    let err = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &definitions,
        &forbidden_authz,
        std::slice::from_ref(&cmd),
    )
    .expect_err("revoked actor must be refused fail-closed");
    assert!(matches!(
        err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::AuthorizationDenied { actor, .. }
                    if actor == "planner:amal"
            )
    ));
    assert_eq!(
        main.store()
            .verified_head()
            .map_err(|error| format!("head after forbidden: {error}"))?,
        main_head_before
    );

    let invisible_authz = DenyActor {
        denied_actor: "planner:amal".to_owned(),
        denial: ActionDenial::Invisible,
    };
    let invisible_err = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &definitions,
        &invisible_authz,
        std::slice::from_ref(&cmd),
    )
    .expect_err("invisible denial must refuse fail-closed");
    assert!(matches!(
        invisible_err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::AuthorizationDenied { actor, .. }
                    if actor == "planner:amal"
            )
    ));
    assert_eq!(
        main.store()
            .verified_head()
            .map_err(|error| format!("head after invisible: {error}"))?,
        main_head_before
    );
    Ok(())
}

fn canonical_envelopes_json(envelopes: &[ArtifactEnvelope]) -> Result<String, String> {
    let value = serde_json::to_value(envelopes).map_err(|error| format!("to_value: {error}"))?;
    let bytes = RuntimeValue::from_json(value)
        .map_err(|error| format!("runtime: {error}"))?
        .canonical_json()
        .map_err(|error| format!("canonical: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("utf8: {error}"))
}

fn open_installed_supply_main(
    envelopes: &[ArtifactEnvelope],
    defs: &AdmittedDefinitions,
) -> Result<Cell, String> {
    let mut store = CellStore::open_in_memory(defs.model().clone())
        .map_err(|error| format!("open: {error}"))?;
    store
        .install_definitions(
            defs.model().clone(),
            InstalledDefinitionInput {
                canonical_set: canonical_envelopes_json(envelopes)?,
                registry: Vec::new(),
                receipt_sha256: "a".repeat(64),
                admission_receipt: None,
                readmission: None,
                installed_at: BASE_CLOCK_MS,
                diff: json!({"added": [], "removed": [], "changed": [], "narrowing": []}),
                layout: None,
            },
            Some(0),
            SCOPE_STR,
        )
        .map_err(|error| format!("install v1: {error}"))?;
    let mut main = Cell::new(store, SCOPE_STR, |cmd| cmd.payload.clone())
        .map_err(|error| format!("cell: {error}"))?;
    main.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(1_000),
    ));
    Ok(main)
}

#[test]
fn promote_refuses_tampered_authorization_scope() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut decision_fork = fork_cell(&main, "D_SCOPE")?;
    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(2_000),
    ));
    let cmd = po_create_call("PO-SCOPE-1", "planner:amal", "scope-cmd-1")?;
    execute_action_in_fork(&mut decision_fork, &definitions, &cmd, &AllowAll)
        .map_err(|error| format!("fork execute: {error}"))?;

    let mut tampered = cmd;
    tampered
        .scope
        .insert("orgId".to_owned(), "org_other".to_owned());
    let err = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &definitions,
        &AllowAll,
        &[tampered],
    )
    .expect_err("tampered scope must refuse promotion");
    assert!(matches!(
        err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::UnauthenticatedForkHistory { .. }
            )
    ));
    Ok(())
}

#[test]
fn promote_refuses_stale_installed_definitions_and_retired_actions() -> Result<(), String> {
    let all = load_supply_envelopes()?;
    let full_defs =
        AdmittedDefinitions::from_envelopes(&all).map_err(|error| format!("full defs: {error}"))?;
    let without_po_create: Vec<ArtifactEnvelope> = all
        .iter()
        .filter(|env| env.artifact_id != "supply.po.create.action")
        .cloned()
        .collect();
    let retired_defs = AdmittedDefinitions::from_envelopes(&without_po_create)
        .map_err(|error| format!("retired defs: {error}"))?;

    let mut main = open_installed_supply_main(&all, &full_defs)?;
    let mut decision_fork = fork_cell(&main, "D_STALE_DEF")?;
    decision_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(2_000),
    ));
    let cmd = po_create_call("PO-DEF-1", "planner:amal", "def-cmd-1")?;
    execute_action_in_fork(&mut decision_fork, &full_defs, &cmd, &AllowAll)
        .map_err(|error| format!("fork execute: {error}"))?;

    main.install_definitions(
        retired_defs.model().clone(),
        InstalledDefinitionInput {
            canonical_set: canonical_envelopes_json(&without_po_create)?,
            registry: Vec::new(),
            receipt_sha256: "b".repeat(64),
            admission_receipt: None,
            readmission: None,
            installed_at: BASE_CLOCK_MS.saturating_add(3_000),
            diff: json!({"added": [], "removed": ["supply.po.create.action"], "changed": [], "narrowing": []}),
            layout: None,
        },
        Some(1),
    )
    .map_err(|error| format!("install v2: {error}"))?;

    let stale_def_err = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &full_defs,
        &AllowAll,
        std::slice::from_ref(&cmd),
    )
    .expect_err("stale AdmittedDefinitions must refuse promotion");
    assert!(matches!(
        stale_def_err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::ReplayRejected { code, .. }
                    if code == "core.world.definition_mismatch"
            )
    ));

    let retired_err = promote_decision_fork(
        &mut main,
        &mut decision_fork,
        &retired_defs,
        &AllowAll,
        &[cmd],
    )
    .expect_err("retired action on main must refuse promotion");
    assert!(matches!(
        retired_err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::ActionNotAdmitted { action_id }
                    if action_id == "supply.po.create"
            )
    ));
    Ok(())
}

#[test]
fn promote_refuses_empty_or_non_action_fork_history() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;
    let mut empty_fork = fork_cell(&main, "D_EMPTY")?;

    let empty_err = promote_decision_fork(&mut main, &mut empty_fork, &definitions, &AllowAll, &[])
        .expect_err("empty decision fork must refuse promotion");
    assert!(matches!(
        empty_err,
        CellError::ForkMergeRefused { reason }
            if matches!(reason.as_ref(), ForkMergeRefusalReason::NoCommandsToPromote { .. })
    ));

    let mut raw_fork = fork_cell(&main, "D_RAW")?;
    raw_fork.advance_to(LogicalTime::from_millis(
        BASE_CLOCK_MS.saturating_add(10_000),
    ));
    raw_fork
        .commit(CellCommit {
            consumes: None,
            reads: Vec::new(),
            writes: Vec::new(),
            events: Vec::new(),
            effects: Vec::new(),
            links: Vec::new(),
            link_reads: Vec::new(),
            predicate_reads: Vec::new(),
        })
        .map_err(|error| format!("raw fork commit: {error}"))?;
    let forged = po_create_call("PO-RAW-1", "planner:amal", "forged-key")?;
    let raw_err =
        promote_decision_fork(&mut main, &mut raw_fork, &definitions, &AllowAll, &[forged])
            .expect_err("raw non-action fork commit must refuse promotion");
    assert!(matches!(
        raw_err,
        CellError::ForkMergeRefused { reason }
            if matches!(
                reason.as_ref(),
                ForkMergeRefusalReason::UnauthenticatedForkHistory { .. }
            )
    ));
    Ok(())
}

#[test]
fn discarded_fork_refuses_read_commit_fork_and_scenario_open() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let mut main = open_seeded_main(&definitions)?;

    assert!(matches!(
        main.discard_fork(),
        Err(CellError::WorldSimulationForbidden)
    ));
    assert!(matches!(
        main.expire_fork(),
        Err(CellError::WorldSimulationForbidden)
    ));

    let mut discarded = fork_cell(&main, "D_DISCARD")?;
    assert_eq!(
        discarded
            .fork_state()
            .map_err(|error| format!("state: {error}"))?,
        Some(ForkState::Open)
    );
    discarded
        .discard_fork()
        .map_err(|error| format!("discard: {error}"))?;
    assert_eq!(
        discarded
            .fork_state()
            .map_err(|error| format!("state after discard: {error}"))?,
        Some(ForkState::Discarded)
    );
    assert!(matches!(
        discarded.discard_fork(),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_DISCARD" && state == "discarded"
    ));
    assert!(matches!(
        discarded
            .store()
            .read()
            .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-BASE-1"),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_DISCARD" && state == "discarded"
    ));
    assert!(matches!(
        discarded.store().verified_head(),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_DISCARD" && state == "discarded"
    ));
    let cmd = po_create_call("PO-CLOSED", "planner:amal", "closed-key")?;
    assert!(matches!(
        execute_action_in_fork(&mut discarded, &definitions, &cmd, &AllowAll)
            .err()
            .as_deref(),
        Some(norn_kernel::ActionRefusal::Decision(
            norn_kernel::DecisionRefusal::Host(CellError::ForkClosed { world, state })
        )) if world == "D_DISCARD" && state == "discarded"
    ));
    let base = main
        .store()
        .verified_head()
        .map_err(|error| format!("main head: {error}"))?;
    assert!(matches!(
        discarded.store().fork("S_AFTER_DISCARD", &base),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_DISCARD" && state == "discarded"
    ));
    assert!(matches!(
        ForkScenario::open(
            discarded,
            BTreeMap::new(),
            ScenarioClock::synchronized(LogicalTime::from_millis(
                BASE_CLOCK_MS.saturating_add(20_000)
            )),
            Entropy::from_bytes([7_u8; 32]),
        ),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_DISCARD" && state == "discarded"
    ));
    Ok(())
}

#[test]
fn expired_fork_refuses_read_and_simulate() -> Result<(), String> {
    let definitions = load_supply_definitions()?;
    let main = open_seeded_main(&definitions)?;
    let mut expired = fork_cell(&main, "D_EXPIRE")?;
    expired
        .expire_fork()
        .map_err(|error| format!("expire: {error}"))?;
    assert_eq!(
        expired
            .fork_state()
            .map_err(|error| format!("state after expire: {error}"))?,
        Some(ForkState::Expired)
    );
    assert!(matches!(
        expired
            .store()
            .read()
            .object(MAIN_WORLD, "supply.PurchaseOrder", "PO-BASE-1"),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_EXPIRE" && state == "expired"
    ));
    let mut expired_store = expired.into_store();
    assert!(matches!(
        expired_store.record_simulated("cmd-1", 0, &RuntimeValue::Null),
        Err(CellError::ForkClosed { ref world, ref state })
            if world == "D_EXPIRE" && state == "expired"
    ));
    Ok(())
}
