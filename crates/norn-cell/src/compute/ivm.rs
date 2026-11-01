//! QUERY-ENGINE-1: receipt-driven maintenance at the exact cell frontier.
use super::{Contributions, Engine, QueryContext, QueryResult, Totals, finish, refused};
use crate::{
    CellError, CellStore, ObjectRecord,
    engine::SqlEngine,
    reactive::{BudgetedRead, DecodeBudget, ReactiveLimits},
};
use norn_checker::metric::compute::{QuestionPlan, contribution::project};
use norn_ir::ResourceLimitProfile;
use std::collections::BTreeSet;

/// Private complete generation; restart explicitly rebuilds from the cell.
#[derive(Clone)]
pub struct IncrementalQuestion {
    plan: QuestionPlan,
    contributions: Contributions,
    members: BTreeSet<String>,
    totals: Totals,
    result: QueryResult,
    rebuilt: bool,
    receipt_pin: Option<String>,
}
impl IncrementalQuestion {
    /// Rebuild all bounded inputs before publishing a complete generation.
    /// # Errors
    /// Returns the same admission, completeness and scope refusals as cell evaluation.
    pub fn rebuild<E: SqlEngine>(
        store: &CellStore<E>,
        plan: &QuestionPlan,
        context: QueryContext,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<Self, CellError> {
        let snapshot = store.question_snapshot(plan, context, &mut visible)?;
        let mut totals = Totals::default();
        for (_, value) in snapshot.contributions.values() {
            totals.adjust(value, false)?;
        }
        let receipt_pin = store
            .read()
            .receipt_at(snapshot.result.revision.commits)?
            .map(|r| r.hash);
        let mut result = snapshot.result;
        result.engine = Engine::Ivm;
        Ok(Self {
            plan: plan.clone(),
            contributions: snapshot.contributions,
            members: snapshot.members,
            totals,
            result,
            rebuilt: true,
            receipt_pin,
        })
    }
    /// Last atomically published generation.
    #[must_use]
    pub const fn result(&self) -> &QueryResult {
        &self.result
    }
    /// Distinguishes a full restart/rebuild from a receipt-maintained generation.
    #[must_use]
    pub const fn was_rebuilt(&self) -> bool {
        self.rebuilt
    }

    /// QUERY-ENGINE-1 / COMPUTE-DETERMINISTIC-1: retract old contributions and add new ones once.
    /// The host must keep authorization decisions stable within its population pin and epoch.
    /// # Errors
    /// Refuses skipped receipts, changed dependency pins, incomplete reads or arithmetic failure.
    /// Refusal leaves the previous generation intact.
    pub fn advance<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        context: QueryContext,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<&QueryResult, CellError> {
        let mut staged = self.clone();
        store
            .conn
            .transaction(|| staged.apply(store, context, &mut visible))?;
        *self = staged;
        Ok(&self.result)
    }

    fn apply<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        context: QueryContext,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<(), CellError> {
        let old = &self.result.context;
        let stale = || refused("core.compute.stale_input");
        if (
            &old.world,
            &old.scope,
            &old.population_pin,
            old.authorization_epoch,
            &old.mapping_pin,
        ) != (
            &context.world,
            &context.scope,
            &context.population_pin,
            context.authorization_epoch,
            &context.mapping_pin,
        ) {
            return Err(stale());
        }
        let reader = store.read();
        if reader
            .receipt_at(self.result.revision.commits)?
            .map(|r| r.hash)
            != self.receipt_pin
        {
            return Err(stale());
        }
        let definition_pin = reader.installed_definitions(None)?.map_or_else(
            || store.model().model_hash().to_owned(),
            |d| d.receipt_sha256,
        );
        if definition_pin != self.result.definition_pin {
            return Err(stale());
        }
        let head = store.verified_head()?;
        let mut changed = BTreeSet::new();
        if head != self.result.revision {
            let next = self
                .result
                .revision
                .commits
                .checked_add(1)
                .ok_or_else(stale)?;
            if head.commits != next {
                return Err(stale());
            }
            let receipt = reader.receipt_at(next)?.ok_or_else(stale)?;
            if receipt.body.world == context.world {
                changed.extend(
                    receipt
                        .body
                        .written
                        .iter()
                        .filter(|w| {
                            store
                                .model()
                                .is_a(&w.object_type, self.plan.grain().as_str())
                        })
                        .map(|w| w.key.clone()),
                );
            }
        }
        changed.extend(
            old.exclusions
                .keys()
                .chain(context.exclusions.keys())
                .filter(|id| old.exclusions.get(*id) != context.exclusions.get(*id))
                .cloned(),
        );
        let limits = ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1());
        let mut budget = DecodeBudget::new(&limits);
        for id in changed {
            if let Some((_, previous)) = self.contributions.remove(&id) {
                self.totals.adjust(&previous, true)?;
            }
            match reader.object_with_budget(
                &context.world,
                self.plan.grain().as_str(),
                &id,
                &mut budget,
            )? {
                BudgetedRead::Found(record) => {
                    if record.scope != context.scope {
                        return Err(stale());
                    }
                    self.members.insert(id.clone());
                    if visible(&record)?
                        && !context.exclusions.contains_key(&id)
                        && let Some(value) = project(&self.plan, &record.values).map_err(refused)?
                    {
                        self.totals.adjust(&value, false)?;
                        self.contributions.insert(id, (record.version, value));
                    }
                }
                BudgetedRead::Missing => {
                    self.members.remove(&id);
                }
                BudgetedRead::Exhausted => return Err(refused("core.compute.bound_exceeded")),
            }
        }
        if self.members.len() > usize::try_from(self.plan.max_items()).map_err(|_n| stale())? {
            return Err(refused("core.compute.bound_exceeded"));
        }
        let mut result = finish(
            &self.plan,
            context,
            head,
            self.result.query_pin.clone(),
            definition_pin,
            &self.contributions,
            &self.totals,
        )?;
        result.engine = Engine::Ivm;
        self.receipt_pin = reader.receipt_at(result.revision.commits)?.map(|r| r.hash);
        self.result = result;
        self.rebuilt = false;
        Ok(())
    }
}
