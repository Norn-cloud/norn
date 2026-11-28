//! Shared in-memory `CellHost` and `ActionAuthorizer` test harnesses for `norn-kernel` suites.

use std::cell::{Ref, RefCell};
use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_kernel::cell_host::{BoundedFact, JsonObject};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, CellCommit, CellHost, Command, CommitError, CommitReceipt,
    DueTimer, FactEvent, FactKey, FactState, LinkKey, LinkReadTarget, LinkState,
    NamingSeriesInspectionRequest, Observed, Performed, PredicateObject, PredicateQuery,
};
use norn_testkit::cell_host::MemoryCell;

pub(crate) type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

pub(crate) fn new_cell() -> Cell {
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
}

pub(crate) fn demo_scope() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), "org_demo".to_owned()),
        ("environmentId".to_owned(), "dev".to_owned()),
    ])
}

/// Authorizes every action call while keeping default fail-closed read and naming-series inspection.
pub(crate) struct AllowAction;

impl ActionAuthorizer for AllowAction {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

/// Authorizes every action call plus slice/type reads.
pub(crate) struct AllowAll;

impl ActionAuthorizer for AllowAll {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }

    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

/// Authorizes every action call and permits naming-series inspection.
pub(crate) struct AllowWithNamingSeries;

impl ActionAuthorizer for AllowWithNamingSeries {
    fn can_inspect_naming_series(&self, _: &NamingSeriesInspectionRequest<'_>) -> bool {
        true
    }

    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

/// Always denies with the configured [`ActionDenial`].
pub(crate) struct DenyAction(pub(crate) ActionDenial);

impl ActionAuthorizer for DenyAction {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Err(self.0.clone())
    }
}

type CommitHook = Box<
    dyn FnMut(
        &mut Cell,
        &CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<(), CommitError<Infallible>>,
>;

/// Delegating [`CellHost`] wrapper around [`Cell`] that supports commit counting,
/// pre-commit race injection hooks, and simulated unsupported predicate scans.
pub(crate) struct HookCell {
    cell: RefCell<Cell>,
    on_commit: Option<CommitHook>,
    predicate_unsupported: bool,
    commit_calls: usize,
}

impl HookCell {
    pub(crate) const fn new(cell: Cell) -> Self {
        Self {
            cell: RefCell::new(cell),
            on_commit: None,
            predicate_unsupported: false,
            commit_calls: 0,
        }
    }

    pub(crate) fn on_commit_once(cell: Cell, hook: impl FnOnce(&mut Cell) + 'static) -> Self {
        let mut hook = Some(hook);
        Self {
            cell: RefCell::new(cell),
            on_commit: Some(Box::new(move |inner, _| {
                if let Some(run) = hook.take() {
                    run(inner);
                }
                Ok(())
            })),
            predicate_unsupported: false,
            commit_calls: 0,
        }
    }

    pub(crate) fn on_commit_with_commit(
        cell: Cell,
        hook: impl FnMut(
            &mut Cell,
            &CellCommit<FactEvent, JsonObject, JsonObject>,
        ) -> Result<(), CommitError<Infallible>>
        + 'static,
    ) -> Self {
        Self {
            cell: RefCell::new(cell),
            on_commit: Some(Box::new(hook)),
            predicate_unsupported: false,
            commit_calls: 0,
        }
    }

    pub(crate) const fn without_predicates(cell: Cell) -> Self {
        Self {
            cell: RefCell::new(cell),
            on_commit: None,
            predicate_unsupported: true,
            commit_calls: 0,
        }
    }

    pub(crate) fn cell(&self) -> Ref<'_, Cell> {
        self.cell.borrow()
    }

    pub(crate) fn cell_mut(&mut self) -> &mut Cell {
        self.cell.get_mut()
    }

    pub(crate) const fn commit_calls(&self) -> usize {
        self.commit_calls
    }
}

impl CellHost for HookCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.borrow().load(key)
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        self.cell.borrow().load_bounded(key, max_bytes)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.borrow().load_link(key)
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        self.cell
            .borrow()
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }

    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, Self::Error> {
        if self.predicate_unsupported {
            return Ok(None);
        }
        self.cell
            .borrow()
            .read_predicate(predicate, max_members, max_bytes)
    }

    fn commit(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        self.commit_calls = self.commit_calls.saturating_add(1);
        if let Some(hook) = self.on_commit.as_mut() {
            hook(self.cell.get_mut(), &commit)?;
        }
        self.cell.get_mut().commit(commit)
    }

    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error> {
        self.cell.get_mut().perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error> {
        self.cell.get_mut().observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error> {
        self.cell.get_mut().clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Self::Error> {
        self.cell.get_mut().publish(receipt)
    }
}
