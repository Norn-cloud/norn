//! ANALYSIS-PUBLISH-1: actual Function receipts enter the governed action.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    missing_docs,
    reason = "synthetic publication acceptance"
)]
#[path = "support/artifacts.rs"]
mod fixture;
#[path = "support/publication_host.rs"]
mod host;
#[path = "support/publication_policy.rs"]
mod policy;
#[path = "support/scrap_world.rs"]
mod source;
use norn_ir::RuntimeValue as Value;
use norn_kernel::{
    ActionCall, ActionOutcome, CellHost, DecisionTurn, FactKey, LogicalTime, TypeCatalog, World,
    commit_action,
};
use policy::reviewer;
use std::collections::BTreeMap;
#[test]
fn governed_publish_accepts_timed_and_refuses_bulk_or_stale_without_partial_effects() {
    let action = policy::admitted_publish();
    for (bulk, fresh) in [(false, true), (true, true), (false, false)] {
        let host::Host {
            mut cell,
            receipt,
            reference,
            prepared,
            model,
        } = host::retained_host(bulk, fresh);
        let now = source::millis(source::BULK_AT);
        let catalog = TypeCatalog::new(model.object_types()).with_interfaces(model.interfaces());
        let request = ActionCall {
            action: &action,
            action_artifact_id: "core.analytics.publish.action",
            action_version: "0.1.0",
            scope: BTreeMap::from([
                ("orgId".into(), "org_demo".into()),
                ("environmentId".into(), "dev".into()),
            ]),
            actor: "reviewer".into(),
            input: norn_ir::runtime_object! {"id"=>"forecast","receipt"=>reference.0.clone()}
                .as_object()
                .expect("reference input")
                .clone(),
            idempotency_key: "publish".into(),
            object_type: None,
            turn: DecisionTurn {
                id: "publish".into(),
                now: LogicalTime::from_millis(now),
                world: World::Main,
                consumes: None,
            },
        };
        let before = cell.store().verified_head().expect("head");
        assert!(
            commit_action(
                &mut cell,
                &catalog,
                &request,
                &reviewer(&action, &model, false)
            )
            .is_err(),
            "reviewer role is mandatory"
        );
        assert_eq!(cell.store().verified_head().expect("unchanged"), before);
        let outcome = commit_action(
            &mut cell,
            &catalog,
            &request,
            &reviewer(&action, &model, true),
        );
        if bulk || !fresh {
            let detail = format!("{:?}", outcome.expect_err("invalid evidence refuses"));
            assert!(
                detail.contains("core.projection.publication_guard")
                    && detail.contains(if bulk { "temporal_leakage" } else { "fresh" }),
                "{detail}"
            );
            assert_eq!(
                cell.store().verified_head().expect("no partial effect"),
                before
            );
            assert!(
                cell.resolve_accepted_receipt(
                    &norn_kernel::receipt_binding::ReceiptReference::parse(&Value::from(
                        reference.0.clone()
                    ))
                    .expect("reference"),
                    &receipt.manifest
                )
                .expect("unconsumed")
                .is_some()
            );
        } else {
            assert!(matches!(
                outcome.expect("governed publication"),
                ActionOutcome::Committed { .. }
            ));
            let published = cell
                .load(&FactKey::new("core.analytics.Publication", "forecast"))
                .expect("publication")
                .values
                .expect("live");
            assert_eq!(published["status"], Value::from("published"));
            assert_eq!(published["receipt_digest"], Value::from(reference.0));
            assert_eq!(published["training_count"], Value::from(48));
            assert_eq!(published["holdout_count"], Value::from(16));
            println!("completed governed publication: {:?}", prepared.rmse_nanos);
        }
    }
}
