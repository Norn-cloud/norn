#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Laws of the copy-on-write page store (Core 08, fork lifecycle and
//! isolation), as properties: L1 stored pages never change under their
//! address; L2 a fork is a snapshot; L3 collection never frees what a live
//! root reaches; L4 a copy-on-write fork reads exactly what the copying
//! reference engine's full copy reads. Plus FORK-ADDRESS-1 (addresses never
//! compare across tenants) and dedupe within a tenant.

use std::collections::BTreeMap;

use norn_cell::engine::NativeEngine;
use norn_cell::fork::ForkEngine;
use norn_cell::{CellError, CellStore, Model, SqliteCell, StoreImage};
use norn_kernel::cell_host::{JsonObject, JsonValue, ObjectTypeDef};
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, Consumed, Effect, FactKey, FactMutation, FactWrite,
    FreshnessPolicy, Generation, IdempotencyKey, Performed,
};
use norn_pages::{CowEngine, PAGE_SIZE, PageStore, TenantKey};
use proptest::prelude::*;
use serde_json::json;

// ------------------------------------------------------------- page store

const FILES: u8 = 4;
const SPAN: u64 = 40_000;

#[derive(Debug, Clone)]
enum FileOp {
    Create(u8),
    Write {
        file: u8,
        offset: u64,
        bytes: Vec<u8>,
    },
    Truncate {
        file: u8,
        size: u64,
    },
    Fork {
        from: u8,
        to: u8,
    },
    Delete(u8),
    Collect,
}

fn file_op() -> impl Strategy<Value = FileOp> {
    let file = 0..FILES;
    prop_oneof![
        2 => file.clone().prop_map(FileOp::Create),
        6 => (file.clone(), 0..SPAN, proptest::collection::vec(any::<u8>(), 0..9_000))
            .prop_map(|(file, offset, bytes)| FileOp::Write { file, offset, bytes }),
        2 => (file.clone(), 0..SPAN).prop_map(|(file, size)| FileOp::Truncate { file, size }),
        3 => (file.clone(), file.clone()).prop_map(|(from, to)| FileOp::Fork { from, to }),
        1 => file.prop_map(FileOp::Delete),
        2 => Just(FileOp::Collect),
    ]
}

fn name(file: u8) -> String {
    format!("f{file}")
}

fn contents(store: &PageStore, file: &str) -> Vec<u8> {
    let size = usize::try_from(store.root(file).expect("root").size()).expect("size");
    let mut buf = vec![0xAA; size];
    let read = store.read(file, 0, &mut buf).expect("reads");
    assert_eq!(read, size, "a whole-file read returns the whole file");
    buf
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// L1–L3 on the page store against plain byte vectors: after every
    /// operation each file reads exactly its model bytes (holes and
    /// truncated tails read as zeros), forks are independent snapshots, every
    /// stored entry sits under the address of its own content, and a
    /// collection changes no file.
    #[test]
    fn page_store_files_match_plain_bytes(ops in proptest::collection::vec(file_op(), 1..48)) {
        let store = PageStore::new(TenantKey::derive(b"tenant-a"));
        let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for op in ops {
            match &op {
                FileOp::Create(file) => {
                    let created = store.create_file(&name(*file));
                    prop_assert_eq!(created.is_ok(), !model.contains_key(&name(*file)));
                    model.entry(name(*file)).or_default();
                }
                FileOp::Write { file, offset, bytes } => {
                    let Some(bytes_model) = model.get_mut(&name(*file)) else { continue };
                    store.write(&name(*file), *offset, bytes).expect("writes");
                    let start = usize::try_from(*offset).expect("offset");
                    let end = start + bytes.len();
                    if bytes_model.len() < end {
                        bytes_model.resize(end, 0);
                    }
                    bytes_model[start..end].copy_from_slice(bytes);
                }
                FileOp::Truncate { file, size } => {
                    let Some(bytes_model) = model.get_mut(&name(*file)) else { continue };
                    store.truncate(&name(*file), *size).expect("truncates");
                    bytes_model.resize(usize::try_from(*size).expect("size"), 0);
                }
                FileOp::Fork { from, to } => {
                    let forked = store.fork_file(&name(*from), &name(*to));
                    let allowed = model.contains_key(&name(*from)) && !model.contains_key(&name(*to));
                    prop_assert_eq!(forked.is_ok(), allowed);
                    if allowed {
                        let copy = model[&name(*from)].clone();
                        model.insert(name(*to), copy);
                    }
                }
                FileOp::Delete(file) => {
                    prop_assert_eq!(store.delete_file(&name(*file)).is_ok(), model.remove(&name(*file)).is_some());
                }
                FileOp::Collect => {
                    store.collect_garbage().expect("collects");
                }
            }
            store.verify().expect("L1: every entry sits under its content's address");
            for (file, bytes) in &model {
                prop_assert_eq!(&contents(&store, file), bytes, "{:?} diverged after {:?}", file, op);
                // Nothing survives past the end of a file: a later extension reads zeros.
                let root = store.root(file).expect("root");
                let first_beyond = root.size().div_ceil(u64::try_from(PAGE_SIZE).expect("page size"));
                for index in first_beyond..first_beyond + 16 {
                    prop_assert!(store.page(&root, index).expect("page").is_none(), "{:?} keeps page {} after {:?}", file, index, op);
                }
            }
        }
        // L3 and no leak: after a final collection, only reachable content remains.
        let report = store.collect_garbage().expect("collects");
        let stats = store.stats().expect("stats");
        prop_assert_eq!((stats.pages, stats.nodes), (report.pages_live, report.nodes_live));
    }

    /// FORK-ADDRESS-1: the same content under two tenants' keys never shares
    /// an address; under one tenant it dedupes.
    #[test]
    fn addresses_never_compare_across_tenants(
        page in proptest::collection::vec(any::<u8>(), PAGE_SIZE..=PAGE_SIZE),
        secrets in (proptest::collection::vec(any::<u8>(), 1..32), proptest::collection::vec(any::<u8>(), 1..32)),
    ) {
        prop_assume!(secrets.0 != secrets.1);
        let ours = PageStore::new(TenantKey::derive(&secrets.0));
        let theirs = PageStore::new(TenantKey::derive(&secrets.1));
        for store in [&ours, &theirs] {
            store.create_file("a").expect("creates");
            store.write("a", 0, &page).expect("writes");
        }
        prop_assert_ne!(ours.root("a").expect("root").node(), theirs.root("a").expect("root").node());
        let before = ours.stats().expect("stats");
        ours.create_file("b").expect("creates");
        ours.write("b", 0, &page).expect("writes");
        prop_assert_eq!(ours.stats().expect("stats"), before, "identical content dedupes within a tenant");
    }
}

// ----------------------------------------------------- engine vs. oracle

type Cell<E> = SqliteCell<u8, u8, u8, E>;

fn model() -> Model {
    let types: Vec<ObjectTypeDef> = vec![
        serde_json::from_value(json!({
            "name": "law.Thing",
            "properties": [{ "id": "v", "type": { "kind": "integer" } }]
        }))
        .expect("decodes"),
    ];
    Model::from_definitions(&types, &[]).expect("projects")
}

fn serve<E: norn_cell::engine::SqlEngine>(store: CellStore<E>) -> Cell<E> {
    SqliteCell::new(store, "law", |command: &Command<u8>| command.payload).expect("serves")
}

const SLOTS: u8 = 3;

#[derive(Debug, Clone, Copy)]
struct Put {
    key: u8,
    value: u8,
    enqueue: bool,
}

#[derive(Debug, Clone, Copy)]
enum EngineOp {
    Main(Put),
    MainPerform,
    Create { slot: u8, from: Option<u8> },
    Commit(u8, Put),
    Simulate(u8, u8),
    Discard(u8),
    Collect,
}

fn put_strategy() -> impl Strategy<Value = Put> {
    (0..3_u8, any::<u8>(), any::<bool>()).prop_map(|(key, value, enqueue)| Put {
        key,
        value,
        enqueue,
    })
}

fn engine_op() -> impl Strategy<Value = EngineOp> {
    let slot = 0..SLOTS;
    prop_oneof![
        4 => put_strategy().prop_map(EngineOp::Main),
        1 => Just(EngineOp::MainPerform),
        3 => (slot.clone(), proptest::option::of(0..SLOTS)).prop_map(|(slot, from)| EngineOp::Create { slot, from }),
        4 => (slot.clone(), put_strategy()).prop_map(|(slot, put)| EngineOp::Commit(slot, put)),
        1 => (slot.clone(), any::<u8>()).prop_map(|(slot, value)| EngineOp::Simulate(slot, value)),
        1 => slot.prop_map(EngineOp::Discard),
        1 => Just(EngineOp::Collect),
    ]
}

fn commit<E: norn_cell::engine::SqlEngine>(cell: &mut Cell<E>, put: Put) -> String {
    let key = FactKey::new("law.Thing", format!("k{}", put.key));
    let expected = cell.load(&key).expect("loads").version;
    let generation = cell.store().verified_head().expect("head").commits;
    let values: JsonObject =
        JsonObject::from([("v".to_owned(), JsonValue::from(i64::from(put.value)))]);
    let effects = if put.enqueue {
        vec![Effect::Perform(Command {
            id: CommandId(format!("c{}", put.key)),
            generation: Generation(generation),
            idempotency_key: IdempotencyKey(format!("c{}/{generation}", put.key)),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: put.value,
        })]
    } else {
        Vec::new()
    };
    format!(
        "{:?}",
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key,
                expected,
                mutation: FactMutation::Put { values }
            }],
            events: vec![put.value],
            effects,
            ..CellCommit::empty()
        })
    )
}

fn consume<E: norn_cell::engine::SqlEngine>(
    cell: &mut Cell<E>,
    performed: Option<&Performed<u8>>,
) -> String {
    if let Some(performed) = performed {
        cell.commit(CellCommit::empty().consuming(Consumed::Result {
            command: performed.command.clone(),
            generation: performed.generation,
        }))
        .expect("consumes");
    }
    format!("{performed:?}")
}

/// One system: main plus fork slots, over one engine kind.
struct System<E: ForkEngine + norn_cell::engine::SqlEngine> {
    main: Cell<E>,
    forks: BTreeMap<u8, Cell<E>>,
}

impl<E: ForkEngine + norn_cell::engine::SqlEngine + 'static> System<E> {
    fn new(engine: E) -> Self {
        Self {
            main: serve(CellStore::from_engine(engine, model()).expect("opens")),
            forks: BTreeMap::new(),
        }
    }

    /// Apply `op`; the formatted outcome must match the other engine's.
    fn apply(&mut self, op: EngineOp) -> Result<String, CellError> {
        Ok(match op {
            EngineOp::Main(put) => commit(&mut self.main, put),
            EngineOp::MainPerform => {
                let performed = self.main.perform()?;
                consume(&mut self.main, performed.as_ref())
            }
            EngineOp::Create { slot, from } => {
                if self.forks.contains_key(&slot) {
                    return Ok("occupied".to_owned());
                }
                let parent = from
                    .and_then(|from| self.forks.get(&from))
                    .unwrap_or(&self.main);
                let base = parent.store().verified_head()?;
                let fork = parent.store().fork(&format!("fork-{slot}"), &base)?;
                self.forks.insert(slot, serve(fork));
                format!("forked at {}", base.commits)
            }
            EngineOp::Commit(slot, put) => match self.forks.get_mut(&slot) {
                Some(fork) => commit(fork, put),
                None => "absent".to_owned(),
            },
            EngineOp::Simulate(slot, value) => match self.forks.get_mut(&slot) {
                Some(fork) => {
                    let performed = fork.simulate_perform(|_command| value)?;
                    consume(fork, performed.as_ref())
                }
                None => "absent".to_owned(),
            },
            EngineOp::Discard(slot) => format!("{}", self.forks.remove(&slot).is_some()),
            EngineOp::Collect => "collected".to_owned(),
        })
    }

    fn images(&self) -> BTreeMap<Option<u8>, StoreImage> {
        let mut images = BTreeMap::from([(None, self.main.store().image().expect("image"))]);
        for (slot, fork) in &self.forks {
            images.insert(Some(*slot), fork.store().image().expect("image"));
        }
        images
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// L4 (and L2, L3 end to end): the copy-on-write engine and the copying
    /// reference engine, driven in lockstep through main commits, performs,
    /// forks of main and of forks, fork commits, simulated results, discards
    /// and collections, return identical outcomes and hold identical
    /// complete images in every instance after every operation; every stored
    /// page and node stays under its own address.
    #[test]
    fn copy_on_write_forks_match_the_copying_oracle(ops in proptest::collection::vec(engine_op(), 1..32)) {
        let store = PageStore::new(TenantKey::derive(b"tenant-a"));
        let mut oracle = System::new(NativeEngine::open_in_memory().expect("opens"));
        let mut cow = System::new(CowEngine::create(&store).expect("creates"));
        for op in ops {
            let expected = oracle.apply(op).map_err(|error| error.to_string());
            let actual = cow.apply(op).map_err(|error| error.to_string());
            prop_assert_eq!(&actual, &expected, "outcome of {:?}", op);
            if matches!(op, EngineOp::Collect) {
                store.collect_garbage().expect("collects");
            }
            store.verify().expect("L1");
            prop_assert_eq!(cow.images(), oracle.images(), "images after {:?}", op);
        }
        // Discarded forks and dropped engines leave nothing reachable behind.
        drop(cow);
        store.collect_garbage().expect("collects");
        let stats = store.stats().expect("stats");
        prop_assert_eq!((stats.pages, stats.nodes), (0, 0));
    }
}

/// Storage gate (deterministic): a fork stores only the pages it changes.
/// Forking copies no page (a fork writes only its pin record); each fork's commits add at most a small, bounded
/// number of pages, independent of the cell's size.
#[test]
fn a_fork_stores_only_the_pages_it_changes() {
    const COMMITS: usize = 5;
    let store = PageStore::new(TenantKey::derive(b"tenant-a"));
    let mut main = serve(
        CellStore::from_engine(CowEngine::create(&store).expect("creates"), model())
            .expect("opens"),
    );
    for n in 0..400_u16 {
        let key = FactKey::new("law.Thing", format!("seed{n}"));
        let values: JsonObject =
            JsonObject::from([("v".to_owned(), JsonValue::from(i64::from(n)))]);
        main.commit(CellCommit {
            writes: vec![FactWrite {
                key,
                expected: norn_kernel::FactVersion::ABSENT,
                mutation: FactMutation::Put { values },
            }],
            ..CellCommit::empty()
        })
        .expect("seeds");
    }
    store.collect_garbage().expect("collects");
    let base_pages = store.stats().expect("stats").pages;
    let base = main.store().verified_head().expect("head");
    let mut forks = Vec::new();
    for slot in 0..20 {
        forks.push(serve(
            main.store()
                .fork(&format!("f{slot}"), &base)
                .expect("forks"),
        ));
    }
    // Forking copies no page; each fork writes only its pin record, which
    // touches the metadata table's page and the header page.
    let pinned = store.stats().expect("stats").pages - base_pages;
    assert!(
        pinned <= 2 * forks.len(),
        "{pinned} pages for {} forks",
        forks.len()
    );
    for (slot, fork) in forks.iter_mut().enumerate() {
        for step in 0..COMMITS {
            // Distinct values per fork, so dedupe across forks cannot hide growth.
            let value = u8::try_from(slot * COMMITS + step).expect("small");
            let outcome = commit(
                fork,
                Put {
                    key: 0,
                    value,
                    enqueue: false,
                },
            );
            assert!(outcome.starts_with("Ok"), "{outcome}");
        }
    }
    store.collect_garbage().expect("collects");
    let grown = store.stats().expect("stats").pages - base_pages - pinned;
    let per_fork = grown / forks.len();
    // A commit touches the fact table, its indexes, the ledger, receipts and
    // the header page; history tables append. Measured, not assumed: the
    // bound is generous and still a small fraction of the base cell.
    assert!(
        per_fork <= COMMITS * 24,
        "{per_fork} pages per fork after {COMMITS} commits"
    );
    assert!(
        per_fork * 4 < base_pages,
        "{per_fork} of {base_pages} pages"
    );
}
