#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native lineage conformance"
)]
mod query_support;
use norn_cell::compute::lineage::LineageDisclosure;
use norn_cell::{CellError, CommitBatch};
use query_support::*;
const WEEK: &str = "\"2026-10-19\"";

#[test]
fn data_lineage_reconciles_294_over_300_with_exact_fact_versions_and_restricted_values() {
    let (mut store, plan, context) = fixture();
    load(&mut store);
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("result");
    let trace = store
        .trace_question(
            &plan,
            context.clone(),
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |row| Ok(row.id != "SYN-MELT-15-1"),
                source: &mut |_| Ok(true),
            },
        )
        .expect("trace");
    assert_eq!(trace.result, result);
    assert!(trace.contributions.iter().all(|c| matches!(
        c.source,
        norn_cell::compute::lineage::ContributionSource::Unavailable
    )));
    assert_eq!(trace.contributions.len(), 4);
    assert_eq!(trace.result.rows[WEEK].totals[0].canonical(), "294");
    assert_eq!(trace.result.rows[WEEK].totals[1].canonical(), "300");
    for member in &trace.contributions {
        assert_eq!(
            member.fact.version,
            result.rows[WEEK].members[&member.fact.key].version
        );
        // DATA-VALUES-1: a restricted bucket withholds every amount; the
        // engine's own answer (no reader) keeps them.
        let expected = if trace.contributions.iter().any(|c| c.restricted) {
            None
        } else {
            result.rows[WEEK].members[&member.fact.key].amounts.clone()
        };
        assert_eq!(member.amounts, expected);
        assert!(member.fact.stable_id().starts_with("fact:demo.Conversion/"));
        let receipt = store
            .read()
            .receipt_for_fact_version(WORLD, TYPE, &member.fact.key, member.fact.version)
            .expect("receipt");
        // #1175: the receipt hash commits to the values; withheld with them.
        assert_eq!(member.receipt_hash, None);
        assert_eq!(member.recorded_at_ms, receipt.body.logical_time);
        assert_eq!(member.restricted, member.fact.key == "SYN-MELT-15-1");
        // D53: one restricted member withholds every member's values in the bucket.
        assert!(member.values.is_none() && member.fact_digest.is_none());
    }
    let bytes = serde_json::to_string(&trace).expect("wire");
    assert!(bytes.contains("\"restricted\":true,\"values\":null"));
    let head = store.verified_head().expect("head");
    assert!(matches!(
        store.trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |row| Ok(row.id != "SYN-MELT-15-1"),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true)
            }
        ),
        Err(CellError::ComputeRefused(
            "core.projection.lineage_incomplete"
        ))
    ));
    assert_eq!(head, store.verified_head().expect("unchanged"));
}

/// #1175: a single-write commit (no consumes, no idempotency key) has a
/// receipt hash that commits to its values, so a reader without the Values
/// grant could test guesses offline. A restricted bucket withholds it; an
/// unrestricted bucket keeps it, unchanged.
#[test]
fn a_restricted_bucket_withholds_the_receipt_hash_of_a_single_write() {
    let (mut store, plan, context) = fixture();
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes: vec![put("solo", 1_000, 980, "2026-10-23T00:00:00Z", "gold")],
            ..CommitBatch::default()
        })
        .expect("single write");
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("result");
    let trace = |values_allowed: bool| {
        store
            .trace_question(
                &plan,
                context.clone(),
                &result.lineage_ref,
                WEEK,
                |_| Ok(true),
                LineageDisclosure {
                    values: &mut |_| Ok(values_allowed),
                    source: &mut |_| Ok(true),
                },
            )
            .expect("trace")
    };
    let withheld = trace(false);
    let [member] = withheld.contributions.as_slice() else {
        panic!("one contribution")
    };
    assert!(member.restricted);
    assert_eq!(member.receipt_hash, None);
    let receipt = store
        .read()
        .receipt_for_fact_version(WORLD, TYPE, &member.fact.key, member.fact.version)
        .expect("receipt");
    let wire = serde_json::to_string(&withheld).expect("wire");
    assert!(wire.contains("\"receiptHash\":null"), "{wire}");
    assert!(!wire.contains(&receipt.hash), "{wire}");
    let granted = trace(true);
    assert_eq!(
        granted.contributions[0].receipt_hash.as_deref(),
        Some(receipt.hash.as_str())
    );
}

/// #1175: one commit writes every fixture row, across buckets, and its receipt
/// hash commits to all of their values. Tracing a bucket whose members are all
/// readable still withholds that shared hash from a reader missing Values on a
/// row of another bucket; the members' own values stay disclosed.
#[test]
fn a_shared_commit_withholds_its_receipt_hash_from_every_bucket_it_wrote() {
    let (mut store, plan, context) = fixture();
    load(&mut store);
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("result");
    let other = result
        .rows
        .keys()
        .find(|bucket| *bucket != WEEK)
        .expect("bucket");
    let trace = |hidden: &str| {
        store
            .trace_question(
                &plan,
                context.clone(),
                &result.lineage_ref,
                other,
                |_| Ok(true),
                LineageDisclosure {
                    values: &mut |row| Ok(row.id != hidden),
                    source: &mut |_| Ok(true),
                },
            )
            .expect("trace")
    };
    let shared = store
        .read()
        .receipt_for_fact_version(WORLD, TYPE, "SYN-MELT-15-1", 1)
        .expect("shared receipt");
    let withheld = trace("SYN-MELT-15-1");
    assert!(!withheld.contributions.is_empty());
    for member in &withheld.contributions {
        assert_eq!(member.commit_seq, shared.body.commit_seq);
        assert!(!member.restricted && member.values.is_some());
        assert_eq!(member.receipt_hash, None);
    }
    let wire = serde_json::to_string(&withheld).expect("wire");
    assert!(!wire.contains(&shared.hash), "{wire}");
    let granted = trace("");
    assert!(
        granted
            .contributions
            .iter()
            .all(|member| member.receipt_hash.as_deref() == Some(shared.hash.as_str()))
    );
}

/// #1175 (B3): a receipt hash commits to its ledger head, which chains every
/// earlier event. A commit whose writes the reader fully covers still
/// withholds its hash while an earlier commit chained a value the reader
/// lacks (the predecessor head is the genesis constant, so that one value is
/// all a guess needs); a reader with Values over the whole prefix keeps it.
/// The same prefix decides whether the raw ledger head is disclosable.
#[test]
fn a_receipt_hash_is_withheld_while_an_earlier_commit_chains_a_restricted_value() {
    let (mut store, plan, context) = fixture();
    let hidden = put("hidden", 1_000, 990, "2026-10-14T00:00:00Z", "gold");
    let visible = put("visible", 1_000, 980, "2026-10-23T00:00:00Z", "gold");
    for (offset, write) in [(0, hidden), (1000, visible)] {
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK + offset,
                events: created_events(std::slice::from_ref(&write)),
                writes: vec![write],
                ..CommitBatch::default()
            })
            .expect("one write with its created event");
    }
    let head = store.verified_head().expect("head");
    assert_eq!(head.seq, 2, "both commits chain a value");
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("result");
    let trace = |hidden: &str| {
        store
            .trace_question(
                &plan,
                context.clone(),
                &result.lineage_ref,
                WEEK,
                |_| Ok(true),
                LineageDisclosure {
                    values: &mut |row| Ok(row.id != hidden),
                    source: &mut |_| Ok(true),
                },
            )
            .expect("trace")
    };
    let receipt = store
        .read()
        .receipt_for_fact_version(WORLD, TYPE, "visible", 1)
        .expect("receipt");
    let withheld = trace("hidden");
    let [member] = withheld.contributions.as_slice() else {
        panic!("one contribution")
    };
    assert!(!member.restricted && member.values.is_some());
    assert_eq!(member.receipt_hash, None);
    let wire = serde_json::to_string(&withheld).expect("wire");
    assert!(!wire.contains(&receipt.hash), "{wire}");
    assert_eq!(
        trace("").contributions[0].receipt_hash.as_deref(),
        Some(receipt.hash.as_str())
    );
    assert_eq!(
        store
            .disclosable_receipt_hash(receipt.body.commit_seq, &mut |row| Ok(row.id != "hidden"))
            .expect("cover"),
        None
    );
    assert!(
        !store
            .ledger_head_disclosable(&head, &mut |row| Ok(row.id != "hidden"))
            .expect("cover")
    );
    assert!(
        store
            .ledger_head_disclosable(&head, &mut |_| Ok(true))
            .expect("cover")
    );
}

#[test]
fn changed_generation_frontier_epoch_and_nonexact_inputs_refuse_without_revealing() {
    let (mut store, plan, context) = fixture();
    load(&mut store);
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("result");
    for change in 0..5 {
        let mut changed = context.clone();
        match change {
            0 => changed.authorization_epoch += 1,
            1 => changed.population_pin.push_str("/new"),
            2 => changed.watermarks.clear(),
            3 => {
                changed
                    .source_cursors
                    .get_mut("erp")
                    .expect("source")
                    .insert("measurements".into(), "new-frontier".into());
            }
            _ => changed.now_ms += 1,
        }
        let expected = if change < 2 {
            "core.projection.lineage_incomplete"
        } else {
            "core.compute.stale_input"
        };
        assert!(
            matches!(store.trace_question(&plan, changed, &result.lineage_ref, WEEK,
            |_| Ok(true), LineageDisclosure { values: &mut |_| panic!("no disclosure before pin/freshness checks"), source: &mut |_| panic!("no source disclosure before pin/freshness checks") }),
            Err(CellError::ComputeRefused(code)) if code == expected)
        );
    }
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes: vec![put(
                "correction",
                1_000,
                980,
                "2026-10-23T00:00:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("change");
    assert!(matches!(
        store.trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| panic!("no changed-generation disclosure"),
                source: &mut |_| panic!("no changed-generation source disclosure")
            }
        ),
        Err(CellError::ComputeRefused(
            "core.projection.lineage_incomplete"
        ))
    ));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn generated_members_reconcile_and_hidden_values_withhold_amounts(
        amounts in proptest::collection::vec((1i64..100_000,0i64..100_000),1..8), hide in proptest::bool::ANY, hide_source in proptest::bool::ANY
    ) {
        let (mut store, plan, context) = fixture();
        let writes = amounts.iter().enumerate().map(|(i,(input,recovered))| put(&format!("row-{i}"),*input,*recovered,"2026-10-23T00:00:00Z","gold")).collect();
        store.commit(&CommitBatch {world:WORLD.into(),scope:"synthetic".into(),logical_time:CLOCK,writes,..CommitBatch::default()}).expect("commit");
        let result = store.evaluate_question(&plan,context.clone(),|_|Ok(true)).expect("result");
        let trace = store.trace_question(&plan,context,&result.lineage_ref,WEEK,|_|Ok(true), LineageDisclosure { values: &mut |_|Ok(!hide), source: &mut |_| Ok(!hide_source) }).expect("trace");
        proptest::prop_assert_eq!(&trace.result,&result);
        proptest::prop_assert_eq!(trace.contributions.len(),amounts.len());
        let recovered: i64 = amounts.iter().map(|(_,value)|value).sum();
        let input: i64 = amounts.iter().map(|(value,_)|value).sum();
        proptest::prop_assert_eq!(&trace.result.rows[WEEK].totals, &vec![
            norn_ir::Decimal::parse(&format!("{recovered}e-3")).expect("numerator"),
            norn_ir::Decimal::parse(&format!("{input}e-3")).expect("denominator")]);
        for member in trace.contributions {
            // DATA-VALUES-1: hidden values withhold every amount in the bucket.
            let expected = if hide { None } else { result.rows[WEEK].members[&member.fact.key].amounts.clone() };
            proptest::prop_assert_eq!(&member.amounts,&expected);
            proptest::prop_assert_eq!(member.values.is_none(),hide);
            proptest::prop_assert_eq!(member.fact_digest.is_none(),hide);
            proptest::prop_assert_eq!(member.receipt_hash.is_none(),hide);
            proptest::prop_assert_eq!(member.restricted,hide);
            proptest::prop_assert_eq!(matches!(member.source,norn_cell::compute::lineage::ContributionSource::Restricted),hide_source);
        }
    }
}
