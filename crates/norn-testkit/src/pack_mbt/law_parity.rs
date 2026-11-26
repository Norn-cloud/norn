//! `LAW-EXAMPLE-1` → real cell runtime parity driver (#1656).
//!
//! Replays every [`LawExampleCase`] through both `norn-checker`
//! (`evaluate_runtime`) and a real [`PackHost::commit`], asserting exact
//! agreement on law pin `(artifact_id, law_id)`, subject key, and
//! `DataQualityOutcome::NotSatisfied`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue, LinkKey,
};
use norn_kernel::governing_laws::{
    LawEvaluationError, LinkTargets, LinkedTarget, evaluate_linked, link_law_index, reaches,
};
use norn_kernel::pack_mbt_support::{
    DataQualityOutcome, DataQualityRuleRef, LawExampleVerdict as Verdict, LawIR, ObjectTypeDef,
    ObjectTypeName, ZenExpression, evaluate_law_example_runtime as evaluate_law_example,
    law_content_pin,
};
use norn_kernel::{CellCommit, CommitError, CommitReceipt, TypeCatalog};

use super::coverage::DomainCoverage;
use super::driver::{assert_zero_ledger_writes, bfs_paths_to_states, with_cov};
use super::synth::{
    GuardEval, collect_required_link_graph, evaluate_guards, synthesize_guard_satisfying,
};
use super::{AdmittedPack, LawExampleCase, PackHost};

pub(crate) fn run_law_example_parity_phase<H, F>(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let mut checked = 0usize;
    for (idx, example) in pack.law_examples.iter().enumerate() {
        verify_single_law_example(pack, catalog, example, idx, make_host, coverage)?;
        checked = checked.saturating_add(1);
    }
    Ok(checked)
}

fn build_atomic_subject_commit(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
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
    let mut events = Vec::new();
    if let Some(subject_ty) = pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == subject_type)
    {
        let graph = collect_required_link_graph(pack, subject_ty, key, seed)?;
        writes.extend(graph.setup_writes);
        links.extend(graph.setup_links);
        links.extend(graph.direct_links);
        events = subject_evidence(pack, catalog, subject_ty, key, &subject_values, seed)?;
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
        events,
        ..CellCommit::empty()
    })
}

/// GRAPH-STATE-1 (#1685): a law example's subject is a snapshot, but a cell
/// admits a first write only in its lifecycles' initial states or as the end
/// of the transitions the same commit records. The fixture records the legal
/// sequence that reaches the snapshot: a creation in the initial states, the
/// shortest declared path to each example state, and, only where a guard on
/// that path does not hold for the snapshot, a guard-satisfying creation
/// value that is set back to the snapshot's own value afterwards. The final
/// row is still exactly the example subject, so the runtime evaluates the
/// same law on the same values as the checker.
fn subject_evidence(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    subject_ty: &ObjectTypeDef,
    key: &FactKey,
    subject: &JsonObject,
    seed: u64,
) -> Result<Vec<norn_kernel::FactEvent>, String> {
    let unreachable = |why: String| format!("law example subject {key} is unreachable: {why}");
    let mut created = subject.clone();
    let steps =
        lifecycle_steps(catalog, subject_ty, subject, &mut created).map_err(&unreachable)?;
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    let mut current = created.clone();
    let mut repaired = BTreeSet::new();
    for (lifecycle, transition, _) in &steps {
        if evaluate_guards(&transition.guards, &current) != GuardEval::True {
            let satisfying = synthesize_guard_satisfying(
                pack,
                subject_ty,
                lifecycle,
                &transition.guards,
                &key.key,
                seed,
            );
            for property in guard_inputs(&transition.guards) {
                if let Some(value) = satisfying.get(&property) {
                    created.insert(property.clone(), value.clone());
                    current.insert(property.clone(), value.clone());
                    repaired.insert(property);
                }
            }
            if evaluate_guards(&transition.guards, &current) != GuardEval::True {
                return Err(unreachable(format!(
                    "no guard-satisfying values for {}",
                    transition.id.as_str()
                )));
            }
        }
        current.insert(
            lifecycle.property.as_str().to_owned(),
            JsonValue::String(transition.to.clone()),
        );
    }
    let mut events = vec![norn_kernel::FactEvent::Created {
        key: key.clone(),
        values: created,
    }];
    for (lifecycle, transition, from) in steps {
        events.push(norn_kernel::FactEvent::Transitioned {
            key: key.clone(),
            lifecycle: lifecycle.property,
            transition: transition.id,
            from,
            to: transition.to,
        });
    }
    for property in repaired {
        let value = subject.get(&property).cloned().unwrap_or(JsonValue::Null);
        events.push(norn_kernel::FactEvent::Set {
            key: key.clone(),
            property: norn_kernel::pack_mbt_support::PropertyId(property),
            value,
        });
    }
    Ok(events)
}

/// One declared transition on the path to a law example's lifecycle state.
type Step = (
    norn_kernel::pack_mbt_support::Lifecycle,
    norn_kernel::pack_mbt_support::Transition,
    String,
);

/// The shortest declared path from an initial state to each non-initial
/// lifecycle state of `subject`, with `created` set to those initial states.
fn lifecycle_steps(
    catalog: &TypeCatalog<'_>,
    subject_ty: &ObjectTypeDef,
    subject: &JsonObject,
    created: &mut JsonObject,
) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for lifecycle in catalog.lifecycles(&subject_ty.name).into_values() {
        let property = lifecycle.property.as_str().to_owned();
        let Some(state) = subject.get(&property).and_then(JsonValue::as_str) else {
            continue;
        };
        if lifecycle.initial.iter().any(|initial| initial == state) {
            continue;
        }
        let Some((initial, path)) = bfs_paths_to_states(&lifecycle).remove(state) else {
            return Err(format!("no declared path reaches {property} {state:?}"));
        };
        created.insert(property, JsonValue::String(initial.clone()));
        let mut from = initial;
        for id in path {
            let transition = lifecycle
                .transitions
                .iter()
                .find(|transition| transition.id.as_str() == id)
                .ok_or_else(|| format!("undeclared transition {id}"))?;
            let left = std::mem::replace(&mut from, transition.to.clone());
            steps.push((lifecycle.clone(), transition.clone(), left));
        }
    }
    Ok(steps)
}

/// The `self.<property>` inputs a guard reads.
fn guard_inputs(guards: &[ZenExpression]) -> BTreeSet<String> {
    let mut inputs = BTreeSet::new();
    for guard in guards {
        if let Ok(paths) =
            norn_kernel::pack_mbt_support::expression_read_paths(guard.as_str(), "self")
        {
            inputs.extend(paths.into_iter().filter_map(|path| path.into_iter().next()));
        }
    }
    inputs
}

fn verify_single_law_example<H, F>(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
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
        catalog,
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
        (verdict @ (Verdict::Holds | Verdict::NotApplicable | Verdict::Excepted { .. }), Ok(_)) => {
            let host_pin = host.governing_law_pin(law.subject.as_str(), expected_artifact_id);
            if host_pin.as_deref() != Some(expected_pin) {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': host runtime model does not attach exact law pin for '{}' ({expected_artifact_id}): expected {expected_pin}, host has {host_pin:?}",
                    example.name, law.id
                ));
            }
            if host.ledger_len() != ledger_before.saturating_add(1) {
                let label = if *verdict == Verdict::Holds {
                    "accepted"
                } else if *verdict == Verdict::NotApplicable {
                    "NotApplicable"
                } else {
                    "Excepted"
                };
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': {label} commit did not advance ledger",
                    example.name
                ));
            }
            if *verdict == Verdict::Holds {
                with_cov(coverage, |cov| cov.hit_law_hold(law.id.clone()));
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
        (verdict, outcome) => Err(format!(
            "LAW-EXAMPLE-1 '{}' parity mismatch: checker={verdict:?}, runtime={outcome:?}",
            example.name
        )),
    }
}

/// Shadow fact/link graph used by the DST oracle to predict governing-law and
/// invariant outcomes independently of the host under test.
pub(crate) struct OracleGraph<'a> {
    pub(crate) pack: &'a AdmittedPack,
    pub(crate) catalog: &'a TypeCatalog<'a>,
    pub(crate) facts: &'a BTreeMap<FactKey, (JsonObject, FactVersion)>,
    pub(crate) links: &'a BTreeSet<LinkKey>,
}

struct ShadowLinks<'a> {
    catalog: &'a TypeCatalog<'a>,
    facts: &'a BTreeMap<FactKey, (JsonObject, FactVersion)>,
    links: &'a BTreeSet<LinkKey>,
    subject: FactKey,
}

impl LinkTargets for ShadowLinks<'_> {
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError> {
        let mut matches = self.links.iter().filter(|edge| {
            edge.link == link
                && edge.source.key == self.subject.key
                && reaches(
                    self.catalog,
                    &self.subject.object_type,
                    &edge.source.object_type,
                )
        });
        let Some(edge) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(LawEvaluationError::Unsupported(format!(
                "more than one target on link {link}"
            )));
        }
        let Some((values, version)) = self.facts.get(&edge.target) else {
            return Err(LawEvaluationError::Unsupported(format!(
                "missing target {} for link {link}",
                edge.target
            )));
        };
        Ok(Some(LinkedTarget {
            key: edge.target.clone(),
            version: version.0,
            values: values.clone(),
        }))
    }

    fn sources(
        &mut self,
        _role: &str,
        source: &ObjectTypeName,
        link: &str,
    ) -> Result<Vec<LinkedTarget>, LawEvaluationError> {
        let mut out = Vec::new();
        for edge in self.links {
            if edge.link != link
                || edge.target.key != self.subject.key
                || !reaches(
                    self.catalog,
                    &self.subject.object_type,
                    &edge.target.object_type,
                )
            {
                continue;
            }
            if !reaches(self.catalog, &edge.source.object_type, source) {
                continue;
            }
            let Some((values, version)) = self.facts.get(&edge.source) else {
                continue;
            };
            out.push(LinkedTarget {
                key: edge.source.clone(),
                version: version.0,
                values: values.clone(),
            });
        }
        Ok(out)
    }
}

pub(crate) fn immutable_lifecycle_state(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    values: &JsonObject,
) -> Option<(String, String)> {
    catalog
        .lifecycles(object_type)
        .into_iter()
        .find_map(|(prop_id, lc)| {
            let current = values.get(prop_id.as_str()).and_then(JsonValue::as_str)?;
            lc.immutable
                .iter()
                .any(|s| s == current)
                .then(|| (prop_id.as_str().to_owned(), current.to_owned()))
        })
}

fn entity_invariants_violated(pack: &AdmittedPack, ty_name: &str, proposed: &JsonObject) -> bool {
    let inv_fails = |expr: &ZenExpression| {
        evaluate_guards(std::slice::from_ref(expr), proposed) == GuardEval::False
    };
    let mut visited = BTreeSet::new();
    let mut walk_ty = pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == ty_name);
    while let Some(def) = walk_ty {
        if !visited.insert(def.name.as_str().to_owned()) {
            break;
        }
        if def.invariants.iter().any(|inv| inv_fails(&inv.expression)) {
            return true;
        }
        let iface_violated = def.implements.iter().any(|iface_name| {
            pack.interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
                .is_some_and(|iface| {
                    iface
                        .contract_owners(|n| pack.interfaces.iter().find(|i| i.name == *n))
                        .into_iter()
                        .flat_map(|owner| &owner.invariants)
                        .any(|inv| inv_fails(&inv.expression))
                })
        });
        if iface_violated {
            return true;
        }
        walk_ty = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    false
}

/// Kernel commit-wide law evaluation fuel budget (`norn-cell` `LAW_FUEL`).
const KERNEL_LAW_FUEL: u64 = 1_000_000;

/// Predicts whether committing `touched_keys` into `graph` will be refused by
/// any direct/inherited entity invariant or any direct/inherited/linked
/// governing law (on either a written subject or a linked neighbor).
pub(crate) fn predict_oracle_law_violation(
    graph: &OracleGraph<'_>,
    touched_keys: &[FactKey],
) -> bool {
    let Ok(index) = link_law_index(graph.catalog) else {
        return true;
    };
    let mut linked_neighbors = BTreeSet::new();
    let mut remaining = norn_kernel::TurnFuel::new(KERNEL_LAW_FUEL);
    for key in touched_keys {
        let Some((proposed, _)) = graph.facts.get(key) else {
            continue;
        };
        if entity_invariants_violated(graph.pack, key.object_type.as_str(), proposed) {
            return true;
        }
        let mut links = ShadowLinks {
            catalog: graph.catalog,
            facts: graph.facts,
            links: graph.links,
            subject: key.clone(),
        };
        match evaluate_linked(graph.catalog, key, proposed, &mut links, &mut remaining) {
            Ok(failures) if !failures.is_empty() => return true,
            Err(_) => return true,
            Ok(_) => {}
        }
        if index.is_empty() {
            continue;
        }
        for edge in graph.links {
            if edge.target == *key && edge.source != *key {
                let has_forward_dep = index.forward.iter().any(|(subj, link, declared_target)| {
                    *subj == edge.source.object_type
                        && *link == edge.link
                        && reaches(graph.catalog, &key.object_type, declared_target)
                });
                if has_forward_dep {
                    linked_neighbors.insert(edge.source.clone());
                }
            }
            if edge.source == *key && edge.target != *key {
                let has_inverse_dep = index.inverse.iter().any(|(subj, declared_src, link)| {
                    *subj == edge.target.object_type
                        && *link == edge.link
                        && reaches(graph.catalog, &key.object_type, declared_src)
                });
                if has_inverse_dep {
                    linked_neighbors.insert(edge.target.clone());
                }
            }
        }
    }
    for neighbor in linked_neighbors {
        if touched_keys.contains(&neighbor) {
            continue;
        }
        let Some((values, _)) = graph.facts.get(&neighbor) else {
            continue;
        };
        let mut links = ShadowLinks {
            catalog: graph.catalog,
            facts: graph.facts,
            links: graph.links,
            subject: neighbor.clone(),
        };
        match evaluate_linked(graph.catalog, &neighbor, values, &mut links, &mut remaining) {
            Ok(failures) if !failures.is_empty() => return true,
            Err(_) => return true,
            Ok(_) => {}
        }
    }
    false
}

/// Predicts effective persisted values for each `FactMutation::Put` setup write.
pub(crate) fn predict_setup_writes(
    catalog: &TypeCatalog<'_>,
    writes: &[FactWrite],
) -> Result<Vec<(FactKey, JsonObject)>, String> {
    let mut out = Vec::with_capacity(writes.len());
    for write in writes {
        if let FactMutation::Put { values } = &write.mutation {
            let effective = catalog
                .effective_object_values(&write.key.object_type, values)
                .map_err(|err| format!("predict effective values for {}: {err}", write.key))?;
            out.push((write.key.clone(), effective));
        }
    }
    Ok(out)
}

/// Predicts whether an atomic linked-create commit will violate any entity
/// invariant or governing law over `base_facts`/`base_links` augmented with
/// `setup_writes`, `(subject_key, subject_values)`, and `new_links`.
pub(crate) fn predict_linked_create_law_violation(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    base_facts: &BTreeMap<FactKey, (JsonObject, FactVersion)>,
    base_links: &BTreeSet<LinkKey>,
    setup_writes: &[(FactKey, JsonObject)],
    subject: (&FactKey, &JsonObject),
    new_links: &[LinkKey],
) -> bool {
    let mut facts = base_facts.clone();
    for (setup_key, effective) in setup_writes {
        facts.insert(setup_key.clone(), (effective.clone(), FactVersion(1)));
    }
    facts.insert(subject.0.clone(), (subject.1.clone(), FactVersion(1)));
    let mut links = base_links.clone();
    for lk in new_links {
        links.insert(lk.clone());
    }
    let mut touched: Vec<FactKey> = setup_writes.iter().map(|(k, _)| k.clone()).collect();
    touched.push(subject.0.clone());
    predict_oracle_law_violation(
        &OracleGraph {
            pack,
            catalog,
            facts: &facts,
            links: &links,
        },
        &touched,
    )
}
