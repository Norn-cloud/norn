//! A lending cell over an imported OWL vocabulary (#1512).
//!
//! The OWL reader turns an ontology's classes into interfaces. This file
//! makes records of them: object types that implement the imported
//! interfaces, a borrower (a legal entity playing the borrower role) and a
//! loan linked to it, installed through `CellStore::install_definitions`
//! with a checker admission receipt, then committed under a governing law
//! that admits a loan with a positive principal and refuses one without.
//!
//! An implementing type must carry every member its interfaces declare or
//! inherit, and a required link needs a record at its target. So the plan
//! here is mechanical: the example types take every member of their
//! lineage, targets stay the imported interfaces, and each required link
//! whose target no planned type implements adds a supporting type for it,
//! until the closure is reached. One seed record per type closes every
//! required link; each measured commit then adds one loan.
//!
//! - `ledger_lending_cell_admits_and_refuses` runs in CI over the synthetic
//!   ledger ontology (the OWL reader's golden output).
//! - `owl_lending_cell` is the FIBO run: it reads an import written by
//!   `norn-format-read --emit-dir` (FIBO is fetched by pin, never committed)
//!   and prints one `NORN_OWL_LENDING` JSON line for `NORN_LENDING_MODE`:
//!   `ontology` (the vocabulary installed and implemented) or `plain` (the
//!   same types, records and laws with no interfaces: only the members the
//!   records fill, links retargeted to the concrete types). One mode per
//!   process, so the peak resident set is that mode's.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    clippy::too_many_lines,
    missing_docs,
    reason = "proof and measurement harness"
)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use norn_cell::{CellStore, InstalledDefinitionInput, Model, SqliteCell};
use norn_checker::EntityArtifactSetContext;
use norn_checker::receipt::{ReceiptSources, check_artifact_set_with_receipt};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, Decimal, EntityRoot, ResourceLimitProfile, RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
    LinkMutation, LinkWrite,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const SCOPE: &str = "org_demo/dev";
const LOAN: &str = "lending.TermLoan";
const BORROWER: &str = "lending.CorporateBorrower";

/// The example: which imported interfaces the borrower and the loan
/// implement, and the laws over the loan.
struct Example {
    borrower: &'static [&'static str],
    loan: &'static [&'static str],
    /// `(law id, expression, rationale)` over `lending.TermLoan`.
    laws: &'static [(&'static str, &'static str, &'static str)],
    /// The optional imported properties the laws read (kept in `plain` mode).
    reads: &'static [&'static str],
}

const PRINCIPAL_LAW: (&str, &str, &str) = (
    "lending.principal_is_positive",
    "self.principal > 0",
    "A loan advances a positive principal.",
);

const LEDGER: Example = Example {
    borrower: &["ex.pty.LegalEntity"],
    loan: &["ex.ln.Loan"],
    laws: &[PRINCIPAL_LAW],
    reads: &[],
};

const FIBO: Example = Example {
    borrower: &["fibo.fbc.dae.dbt.Borrower", "cmns.org.LegalEntity"],
    loan: &["fibo.loan.ln.ln.Loan"],
    laws: &[
        PRINCIPAL_LAW,
        (
            "lending.deferred_loans_are_not_in_default",
            "not ((self.in_default ?? false) and (self.is_deferred ?? false))",
            "A loan in deferment has its payments postponed, so it is not in default.",
        ),
    ],
    reads: &["in_default", "is_deferred"],
};

/// The imported interfaces by name.
struct Vocabulary {
    artifacts: Vec<ArtifactEnvelope>,
    interfaces: BTreeMap<String, Value>,
}

impl Vocabulary {
    fn new(artifacts: Vec<ArtifactEnvelope>) -> Self {
        let interfaces = artifacts
            .iter()
            .map(|artifact| {
                let value = serde_json::to_value(artifact).expect("wire");
                let interface = value["payload"]["interface"].clone();
                (
                    interface["name"].as_str().expect("interface").to_owned(),
                    interface,
                )
            })
            .collect();
        Self {
            artifacts,
            interfaces,
        }
    }

    /// The interfaces `roots` name and every interface they extend.
    fn lineage(&self, roots: &[String]) -> Vec<String> {
        let mut seen = Vec::new();
        let mut queue: VecDeque<String> = roots.iter().cloned().collect();
        while let Some(name) = queue.pop_front() {
            if seen.contains(&name) {
                continue;
            }
            if let Some(parents) = self.interfaces[&name]["extends"].as_array() {
                queue.extend(parents.iter().filter_map(|p| p.as_str().map(str::to_owned)));
            }
            seen.push(name);
        }
        seen
    }

    /// Every property and link of the lineage. A member several interfaces
    /// declare takes their meet, as an implementation must narrow each: a
    /// value if any requires one, a link to one if any says one, required if
    /// any requires it, and the most specific target.
    fn members(&self, roots: &[String]) -> (Vec<Value>, Vec<Value>) {
        let mut properties: BTreeMap<String, Value> = BTreeMap::new();
        let mut links: BTreeMap<String, Value> = BTreeMap::new();
        for name in self.lineage(roots) {
            for property in self.interfaces[&name]["properties"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let id = property["id"].as_str().expect("id").to_owned();
                let Some(held) = properties.get_mut(&id) else {
                    properties.insert(id, property.clone());
                    continue;
                };
                if property["constraints"]["nullable"].as_bool() != Some(true)
                    && let Some(object) = held.as_object_mut()
                {
                    object.remove("constraints");
                }
            }
            for link in self.interfaces[&name]["links"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let id = link["id"].as_str().expect("id").to_owned();
                let Some(held) = links.get_mut(&id) else {
                    links.insert(id, link.clone());
                    continue;
                };
                for end in ["to", "from"] {
                    if link["cardinality"][end] == "one" {
                        held["cardinality"][end] = json!("one");
                    }
                }
                if required(link) {
                    held["cardinality"]["required"] = json!(true);
                }
                let (mine, theirs) = (
                    held["target"].as_str().expect("target").to_owned(),
                    link["target"].as_str().expect("target").to_owned(),
                );
                if !self.lineage(std::slice::from_ref(&mine)).contains(&theirs)
                    && self.lineage(std::slice::from_ref(&theirs)).contains(&mine)
                {
                    held["target"] = json!(theirs);
                }
            }
        }
        (
            properties.into_values().collect(),
            links.into_values().collect(),
        )
    }
}

fn ledger() -> Vocabulary {
    let output: Value = serde_json::from_str(include_str!(
        "../../norn-formats/tests/goldens/owl/ledger.output.json"
    ))
    .expect("ledger golden");
    Vocabulary::new(
        serde_json::from_value(output["layers"][0]["artifacts"].clone()).expect("layer"),
    )
}

fn required(link: &Value) -> bool {
    link["cardinality"]["required"].as_bool() == Some(true)
}

/// One planned object type.
struct TypePlan {
    implements: Vec<String>,
    properties: Vec<Value>,
    links: Vec<Value>,
}

/// The types the example needs and, for every required link target, the
/// planned type whose records stand at it.
struct Plan {
    types: BTreeMap<String, TypePlan>,
    order: Vec<String>,
    choice: BTreeMap<String, String>,
}

impl Plan {
    fn new(vocabulary: &Vocabulary, example: &Example) -> Self {
        let mut plan = Self {
            types: BTreeMap::new(),
            order: Vec::new(),
            choice: BTreeMap::new(),
        };
        let owned = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let mut work = VecDeque::from([
            (BORROWER.to_owned(), owned(example.borrower)),
            (LOAN.to_owned(), owned(example.loan)),
        ]);
        while let Some((name, implements)) = work.pop_front() {
            if plan.types.contains_key(&name) {
                continue;
            }
            let (properties, links) = vocabulary.members(&implements);
            plan.order.push(name.clone());
            plan.types.insert(
                name.clone(),
                TypePlan {
                    implements,
                    properties,
                    links,
                },
            );
            // Every required target gets a planned type that implements it.
            for link in plan.types[&name].links.iter().filter(|l| required(l)) {
                let target = link["target"].as_str().expect("target").to_owned();
                if plan.choice.contains_key(&target) {
                    continue;
                }
                let chosen = plan
                    .order
                    .iter()
                    .chain(work.iter().map(|(n, _)| n))
                    .find(|candidate| {
                        let implements = plan.types.get(*candidate).map_or_else(
                            || {
                                work.iter()
                                    .find(|(n, _)| n == *candidate)
                                    .map(|(_, i)| i.clone())
                            },
                            |t| Some(t.implements.clone()),
                        );
                        implements.is_some_and(|i| vocabulary.lineage(&i).contains(&target))
                    })
                    .cloned();
                let chosen = chosen.unwrap_or_else(|| {
                    let support = format!("lending.support.{target}");
                    work.push_back((support.clone(), vec![target.clone()]));
                    support
                });
                plan.choice.insert(target, chosen);
            }
        }
        plan
    }

    /// The object type of `name`: in `ontology` mode it implements its
    /// interfaces and carries their members; in `plain` mode it carries only
    /// the members its records fill. A link whose target interface has a
    /// planned type targets that type (a narrowing of the interface's
    /// target); the others keep the interface.
    fn object_type(&self, name: &str, ontology: bool, reads: &[&str]) -> Value {
        let plan = &self.types[name];
        let nullable = |p: &Value| p["constraints"]["nullable"].as_bool() == Some(true);
        let read = |p: &Value| name == LOAN && reads.contains(&p["id"].as_str().unwrap_or(""));
        let mut properties: Vec<Value> = plan
            .properties
            .iter()
            .filter(|p| ontology || !nullable(p) || read(p))
            .cloned()
            .collect();
        let links: Vec<Value> = plan
            .links
            .iter()
            .filter(|l| ontology || required(l))
            .map(|link| {
                let mut link = link.clone();
                if let Some(chosen) = self.choice.get(link["target"].as_str().expect("target")) {
                    link["target"] = json!(chosen);
                }
                link
            })
            .collect();
        if name == LOAN {
            properties.push(json!({
                "id": "principal",
                "type": {
                    "kind": "decimal", "scale": 2,
                    "currencySource": { "links": [], "property": "principal_currency" },
                },
            }));
            properties.push(json!({ "id": "principal_currency", "type": { "kind": "text" } }));
        }
        let mut object = json!({ "name": name, "properties": properties, "links": links });
        if ontology {
            object["implements"] = json!(plan.implements);
        }
        object
    }

    /// The values of a record of `name`: every required property filled.
    fn values(&self, name: &str) -> JsonObject {
        let mut values = JsonObject::new();
        for property in &self.types[name].properties {
            if property["constraints"]["nullable"].as_bool() == Some(true) {
                continue;
            }
            let id = property["id"].as_str().expect("id").to_owned();
            let value = match property["type"]["kind"].as_str() {
                Some("boolean") => RuntimeValue::from(false),
                _ => RuntimeValue::from(format!("example {id}")),
            };
            values.insert(id, value);
        }
        values
    }

    /// Writes creating `name`/`key` and its required links: to the seed
    /// record of the chosen type, or, for a link whose target may have one
    /// source only, to a fresh record of its own.
    fn create(&self, name: &str, key: &str, commit: &mut Commit) {
        commit.writes.push(FactWrite {
            key: FactKey::new(name, key),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: self.values(name),
            },
        });
        for link in self.types[name].links.iter().filter(|l| required(l)) {
            let id = link["id"].as_str().expect("id");
            let target_type = &self.choice[link["target"].as_str().expect("target")];
            let target_key = if key != "seed" && link["cardinality"]["from"] == "one" {
                let fresh = format!("{key}.{id}");
                self.create(target_type, &fresh, commit);
                fresh
            } else {
                "seed".to_owned()
            };
            commit.links.push(LinkWrite {
                key: LinkKey::new(
                    FactKey::new(name, key),
                    id,
                    FactKey::new(target_type.as_str(), target_key),
                ),
                expected: FactVersion::ABSENT,
                mutation: LinkMutation::Add {
                    values: JsonObject::new(),
                },
            });
        }
    }
}

type Commit = CellCommit<FactEvent, JsonObject, JsonObject>;

fn entity(name: &str, object_type: &Value) -> ArtifactEnvelope {
    let (namespace, local) = name.rsplit_once('.').expect("qualified");
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
        "namespace": namespace, "name": local, "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "dependencies": [], "provenance": { "source": "system" },
        "payload": { "entityVersion": "2.0", "objectType": object_type },
    }))
    .unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn law((id, expression, rationale): (&str, &str, &str)) -> ArtifactEnvelope {
    let local = id.rsplit_once('.').expect("qualified").1;
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": format!("{id}.law"), "kind": "law",
        "namespace": "lending", "name": local, "version": "1.0.0", "lifecycle": "draft",
        "scope": {}, "provenance": { "source": "system" },
        "dependencies": [{ "artifactId": format!("{LOAN}.entity"), "kind": "entity", "required": true, "version": "1.0.0" }],
        "payload": {
            "id": id, "lawVersion": "0.1", "modality": { "kind": "always" }, "owner": "lending",
            "provenance": { "source": "system" }, "rationale": rationale,
            "response": { "kind": "predicate", "expression": expression },
            "scope": { "objectTypes": [LOAN], "root": { "id": LOAN, "kind": "entity" } },
            "severity": "error", "subject": LOAN,
        },
    }))
    .unwrap_or_else(|error| panic!("{id}: {error}"))
}

/// The definitions set of one mode: the vocabulary (ontology mode only),
/// the planned object types and the laws.
fn definitions(
    vocabulary: &Vocabulary,
    plan: &Plan,
    example: &Example,
    ontology: bool,
) -> Vec<ArtifactEnvelope> {
    let mut set = if ontology {
        vocabulary.artifacts.clone()
    } else {
        Vec::new()
    };
    set.extend(
        plan.order
            .iter()
            .map(|name| entity(name, &plan.object_type(name, ontology, example.reads))),
    );
    set.extend(example.laws.iter().copied().map(law));
    set
}

/// The model a host projects from a set: entity roots and governing laws.
fn model(artifacts: &[ArtifactEnvelope]) -> Model {
    let (mut objects, mut interfaces) = (Vec::new(), Vec::new());
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
        )
        .expect("entity root");
        match root {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(_)
            | EntityRoot::V2Markings(_)
            | EntityRoot::V2Extension(_)
            | EntityRoot::V1(_) => panic!("{} is outside the example", artifact.artifact_id),
        }
    }
    Model::from_definitions_with_interfaces(&objects, &[], &interfaces)
        .expect("model")
        .with_governing_laws(artifacts)
        .expect("governing laws")
}

/// Admission (with its receipt) and install, timed.
struct Installed {
    store: CellStore,
    admit_ms: u128,
    install_ms: u128,
    errors: Vec<String>,
}

fn install(artifacts: &[ArtifactEnvelope]) -> Installed {
    let values: Vec<RuntimeValue> = artifacts
        .iter()
        .map(|a| RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("value"))
        .collect();
    let started = Instant::now();
    let sources = ReceiptSources::new(&values, &[]).expect("sources");
    let checked = check_artifact_set_with_receipt(
        &sources,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("checked");
    let admit_ms = started.elapsed().as_millis();
    let errors: Vec<String> = checked
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| format!("{} {:?}: {}", d.code.as_str(), d.artifact_id, d.message))
        .collect();
    let started = Instant::now();
    let model = model(artifacts);
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    if checked.receipt.admitted {
        let input = InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                RuntimeValue::from_json(serde_json::to_value(artifacts).expect("set"))
                    .expect("value")
                    .canonical_json()
                    .expect("canonical"),
            )
            .expect("UTF-8"),
            registry: vec![],
            receipt_sha256: checked.receipt.content_sha256().expect("receipt digest"),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().expect("receipt"))
                    .expect("UTF-8"),
            ),
            readmission: None,
            diff: json!({}),
            installed_at: 1,
            layout: None,
        };
        store
            .install_definitions(model, input, Some(0), SCOPE)
            .expect("installed");
    }
    Installed {
        store,
        admit_ms,
        install_ms: started.elapsed().as_millis(),
        errors,
    }
}

fn loan(plan: &Plan, key: &str, principal: &str, flags: &[(&str, bool)]) -> Commit {
    let mut commit = Commit::empty();
    plan.create(LOAN, key, &mut commit);
    if let Some(FactWrite {
        mutation: FactMutation::Put { values },
        ..
    }) = commit.writes.first_mut()
    {
        values.insert(
            "principal".into(),
            RuntimeValue::from(Decimal::parse(principal).expect("decimal")),
        );
        values.insert(
            "principal_currency".into(),
            RuntimeValue::from("EGP".to_owned()),
        );
        for (flag, value) in flags {
            values.insert((*flag).into(), RuntimeValue::from(*value));
        }
    }
    commit
}

fn seed(plan: &Plan) -> Commit {
    let mut commit = Commit::empty();
    for name in &plan.order {
        plan.create(name, "seed", &mut commit);
    }
    if let Some(write) = commit
        .writes
        .iter_mut()
        .find(|w| w.key == FactKey::new(LOAN, "seed"))
        && let FactMutation::Put { values } = &mut write.mutation
    {
        values.insert(
            "principal".into(),
            RuntimeValue::from(Decimal::parse("1.00").expect("decimal")),
        );
        values.insert(
            "principal_currency".into(),
            RuntimeValue::from("EGP".to_owned()),
        );
    }
    commit
}

fn percentile(sorted: &[u128], p: usize) -> u128 {
    sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)]
}

fn peak_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}

/// Install one mode, seed it, commit `loans` loans and the refusals; the
/// measurements as JSON.
fn run(vocabulary: &Vocabulary, example: &Example, ontology: bool, loans: usize) -> Value {
    let plan = Plan::new(vocabulary, example);
    let set = definitions(vocabulary, &plan, example, ontology);
    let installed = install(&set);
    assert_eq!(
        installed.errors.len(),
        0,
        "the {} set is admitted: {:#?}",
        if ontology { "ontology" } else { "plain" },
        installed.errors
    );
    let mut cell =
        Cell::new(installed.store, SCOPE, |command| command.payload.clone()).expect("cell");
    let seed = seed(&plan);
    let seed_records = seed.writes.len();
    let seed_links = seed.links.len();
    cell.commit(seed)
        .expect("the seed records close every required link");
    let _warm = norn_ir::counters::take();
    let mut samples = Vec::with_capacity(loans);
    let mut counts = Value::Null;
    let mut loan_writes = (0, 0);
    for index in 0..loans {
        let commit = loan(&plan, &format!("loan-{index}"), "250000.00", &[]);
        loan_writes = (commit.writes.len(), commit.links.len());
        let started = Instant::now();
        cell.commit(commit)
            .expect("a loan with a positive principal is admitted");
        samples.push(started.elapsed().as_nanos());
        if index == 0 {
            let taken = norn_ir::counters::take();
            counts = taken
                .entries()
                .into_iter()
                .map(|(name, value)| (name.to_owned(), json!(value)))
                .collect::<serde_json::Map<_, _>>()
                .into();
        }
    }
    let refused = cell
        .commit(loan(&plan, "loan-zero", "0.00", &[]))
        .expect_err("a loan with no principal is refused");
    let mut refusals = vec![
        json!({ "law": PRINCIPAL_LAW.0, "code": refused.code(), "message": refused.to_string() }),
    ];
    assert!(
        refused.to_string().contains("principal") || refused.to_string().contains(PRINCIPAL_LAW.0),
        "{refused}"
    );
    if example.laws.len() > 1 {
        let flags = [("in_default", true), ("is_deferred", true)];
        let refused = cell
            .commit(loan(&plan, "loan-deferred-default", "1000.00", &flags))
            .expect_err("a deferred loan in default is refused");
        refusals.push(json!({ "law": example.laws[1].0, "code": refused.code(), "message": refused.to_string() }));
        cell.commit(loan(
            &plan,
            "loan-deferred",
            "1000.00",
            &[("is_deferred", true)],
        ))
        .expect("a deferred loan that is not in default is admitted");
    }
    samples.sort_unstable();
    let members = |name: &str| {
        let object = plan.object_type(name, ontology, example.reads);
        json!({
            "properties": object["properties"].as_array().map_or(0, Vec::len),
            "links": object["links"].as_array().map_or(0, Vec::len),
        })
    };
    json!({
        "mode": if ontology { "ontology" } else { "plain" },
        "definitions": set.len(),
        "objectTypes": plan.order.len(),
        "supportTypes": plan.order.iter().filter(|n| n.starts_with("lending.support.")).count(),
        "borrower": members(BORROWER),
        "loan": members(LOAN),
        "seedRecords": seed_records,
        "seedLinks": seed_links,
        "loanCommit": { "records": loan_writes.0, "links": loan_writes.1 },
        "admitMillis": installed.admit_ms,
        "installMillis": installed.install_ms,
        "commit": { "n": samples.len(), "p50_us": percentile(&samples, 50) / 1000, "p95_us": percentile(&samples, 95) / 1000 },
        "firstLoanCounts": counts,
        "refusals": refusals,
        "peakKiB": peak_kib(),
    })
}

#[test]
fn ledger_lending_cell_admits_and_refuses() {
    let vocabulary = ledger();
    for ontology in [true, false] {
        let record = run(&vocabulary, &LEDGER, ontology, 3);
        assert_eq!(record["objectTypes"], json!(2), "{record}");
        let code = record["refusals"][0]["code"].as_str().expect("code");
        assert_ne!(code.len(), 0, "{record}");
    }
}

#[test]
#[ignore = "reads an OWL import fetched by pin; run on Boat with NORN_OWL_EMIT_DIR and NORN_LENDING_MODE"]
fn owl_lending_cell() {
    let dir = std::env::var("NORN_OWL_EMIT_DIR").expect("NORN_OWL_EMIT_DIR: an owl --emit-dir");
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(
        &std::fs::read_to_string(std::path::Path::new(&dir).join("artifacts.json"))
            .expect("artifacts.json"),
    )
    .expect("vocabulary");
    let vocabulary = Vocabulary::new(artifacts);
    let ontology = std::env::var("NORN_LENDING_MODE").map_or(true, |mode| mode != "plain");
    let loans = std::env::var("NORN_LENDING_LOANS").map_or(200, |n| n.parse().expect("count"));
    if let Ok(path) = std::env::var("NORN_LENDING_DUMP") {
        // The installed definitions, for a Studio rendering of the cell.
        let plan = Plan::new(&vocabulary, &FIBO);
        let set = definitions(&vocabulary, &plan, &FIBO, ontology);
        std::fs::write(&path, serde_json::to_string(&set).expect("set")).expect("dump");
    }
    let record = run(&vocabulary, &FIBO, ontology, loans);
    println!("NORN_OWL_LENDING {record}");
}
