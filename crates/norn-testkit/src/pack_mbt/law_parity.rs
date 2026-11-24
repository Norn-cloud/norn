//! `LAW-EXAMPLE-1` → real cell runtime parity driver (#1656).
//!
//! Replays every [`LawExampleCase`] through both `norn-checker`
//! (`evaluate_runtime`) and a real [`PackHost::commit`], asserting exact
//! agreement on law pin `(artifact_id, law_id)`, subject key, and
//! `DataQualityOutcome::NotSatisfied`.

use std::sync::{Arc, Mutex};

use norn_kernel::cell_host::{FactKey, FactMutation, FactVersion, FactWrite, JsonValue};
use norn_kernel::governing_laws::LawEvaluationError;
use norn_kernel::pack_mbt_support::{
    DataQualityOutcome, DataQualityRuleRef, LawExampleVerdict as Verdict, LawIR,
    evaluate_law_example_runtime as evaluate_law_example, law_content_pin,
};
use norn_kernel::{CellCommit, CommitError, CommitReceipt, TypeCatalog};

use super::coverage::DomainCoverage;
use super::driver::{assert_zero_ledger_writes, with_cov};
use super::synth::collect_required_link_graph;
use super::{AdmittedPack, LawExampleCase, PackHost};

pub(crate) fn run_law_example_parity_phase<H, F>(
    pack: &AdmittedPack,
    _catalog: &TypeCatalog<'_>,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let mut checked = 0usize;
    for (idx, example) in pack.law_examples.iter().enumerate() {
        verify_single_law_example(pack, example, idx, make_host, coverage)?;
        checked = checked.saturating_add(1);
    }
    Ok(checked)
}

fn build_atomic_subject_commit(
    pack: &AdmittedPack,
    subject_type: &str,
    key: &FactKey,
    subject_values: norn_kernel::cell_host::JsonObject,
    seed: u64,
) -> Result<
    CellCommit<
        norn_kernel::FactEvent,
        norn_kernel::cell_host::JsonObject,
        norn_kernel::cell_host::JsonObject,
    >,
    String,
> {
    let mut writes = Vec::new();
    let mut links = Vec::new();
    if let Some(subject_ty) = pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == subject_type)
    {
        let graph = collect_required_link_graph(pack, subject_ty, key, seed)?;
        writes.extend(graph.setup_writes);
        links.extend(graph.setup_links);
        links.extend(graph.direct_links);
    }
    writes.push(FactWrite {
        key: key.clone(),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: subject_values,
        },
    });
    Ok(CellCommit {
        writes,
        links,
        ..CellCommit::empty()
    })
}

fn verify_single_law_example<H, F>(
    pack: &AdmittedPack,
    example: &LawExampleCase,
    idx: usize,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let (law_pos, law) = pack
        .laws
        .iter()
        .enumerate()
        .find(|(_, law)| law.id == example.law_id)
        .ok_or_else(|| {
            format!(
                "LAW-EXAMPLE-1 '{}': unknown law '{}'",
                example.name, example.law_id
            )
        })?;
    let expected_artifact_id = pack
        .law_envelopes
        .get(law_pos)
        .map(|env| env.artifact_id.as_str())
        .ok_or_else(|| {
            format!(
                "LAW-EXAMPLE-1 '{}': missing retained envelope for law '{}'",
                example.name, law.id
            )
        })?;
    let checker_verdict = evaluate_law_example(law, &JsonValue::Object(example.subject.clone()))
        .map_err(|err| format!("LAW-EXAMPLE-1 '{}': checker error: {err}", example.name))?;
    if checker_verdict != example.expected {
        return Err(format!(
            "LAW-EXAMPLE-1 '{}': expected {:?}, checker returned {:?}",
            example.name, example.expected, checker_verdict
        ));
    }

    let expected_pin = law_content_pin(law).map_err(|err| {
        format!(
            "LAW-EXAMPLE-1 '{}': failed pinning law '{}': {err}",
            example.name, law.id
        )
    })?;
    let mut host = make_host(&format!("law-example-{idx}"))?;
    let key = FactKey::new(law.subject.as_str(), format!("law-ex-{idx}"));
    let seed = u64::try_from(idx).unwrap_or(0).saturating_add(1);
    let commit = build_atomic_subject_commit(
        pack,
        law.subject.as_str(),
        &key,
        example.subject.clone(),
        seed,
    )?;
    let ledger_before = host.ledger_len();
    let runtime_result = host.commit(commit);
    check_law_runtime_outcome(
        LawOutcomeCtx {
            law,
            expected_artifact_id,
            expected_pin: &expected_pin,
            example,
            key: &key,
            ledger_before,
            coverage,
        },
        &host,
        &checker_verdict,
        runtime_result,
    )
}

#[derive(Clone, Copy)]
struct LawOutcomeCtx<'a> {
    law: &'a LawIR,
    expected_artifact_id: &'a str,
    expected_pin: &'a str,
    example: &'a LawExampleCase,
    key: &'a FactKey,
    ledger_before: usize,
    coverage: &'a Arc<Mutex<DomainCoverage>>,
}

fn check_law_runtime_outcome<H: PackHost>(
    ctx: LawOutcomeCtx<'_>,
    host: &H,
    checker_verdict: &Verdict,
    runtime_result: Result<CommitReceipt, CommitError<H::Error>>,
) -> Result<(), String> {
    let LawOutcomeCtx {
        law,
        expected_artifact_id,
        expected_pin,
        example,
        key,
        ledger_before,
        coverage,
    } = ctx;
    match (checker_verdict, runtime_result) {
        (Verdict::Holds, Ok(_)) => {
            let host_pin = host.governing_law_pin(law.subject.as_str(), expected_artifact_id);
            if host_pin.as_deref() != Some(expected_pin) {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': host runtime model does not attach exact law pin for '{}' ({expected_artifact_id}): expected {expected_pin}, host has {host_pin:?}",
                    example.name, law.id
                ));
            }
            if host.ledger_len() != ledger_before.saturating_add(1) {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': accepted commit did not advance ledger",
                    example.name
                ));
            }
            with_cov(coverage, |cov| cov.hit_law_hold(law.id.clone()));
            host.verify_ledger()
        }
        (Verdict::NotApplicable, Ok(_)) => {
            let host_pin = host.governing_law_pin(law.subject.as_str(), expected_artifact_id);
            if host_pin.as_deref() != Some(expected_pin) {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': host runtime model does not attach exact law pin for '{}' ({expected_artifact_id}): expected {expected_pin}, host has {host_pin:?}",
                    example.name, law.id
                ));
            }
            if host.ledger_len() != ledger_before.saturating_add(1) {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': NotApplicable commit did not advance ledger",
                    example.name
                ));
            }
            host.verify_ledger()
        }
        (
            Verdict::Violated,
            Err(CommitError::Law {
                key: err_key,
                reason: LawEvaluationError::Violated(failure),
            }),
        ) => {
            let pin_matches = matches!(
                &failure.rule_ref,
                DataQualityRuleRef::Law {
                    artifact_id,
                    law_id,
                    version,
                } if law_id == &law.id
                    && artifact_id == expected_artifact_id
                    && version == expected_pin
            );
            if err_key != *key
                || !pin_matches
                || failure.outcome != DataQualityOutcome::NotSatisfied
            {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': runtime violated wrong law/subject/outcome: key={err_key:?} expected={key:?}, rule_ref={:?} expected=({expected_artifact_id}, {}), outcome={:?}",
                    example.name, failure.rule_ref, law.id, failure.outcome
                ));
            }
            assert_zero_ledger_writes(host, ledger_before, &example.name, "law violation")?;
            with_cov(coverage, |cov| cov.hit_law_violation(law.id.clone()));
            host.verify_ledger()
        }
        (Verdict::Excepted { .. }, _) => Err(format!(
            "LAW-EXAMPLE-1 '{}': law '{}' uses exception trees, which are unsupported on the commit path",
            example.name, law.id
        )),
        (verdict, outcome) => Err(format!(
            "LAW-EXAMPLE-1 '{}' parity mismatch: checker={verdict:?}, runtime={outcome:?}",
            example.name
        )),
    }
}
