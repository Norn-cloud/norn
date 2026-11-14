//! Bounded Ascent evaluation over one complete registered catalog snapshot.
//!
//! The caller supplies the chosen local/base registration map in name order.
//! These inputs are complete for this query, unlike declared/incomplete IR
//! projections. Catalog-local integer IDs and all relation storage are owned by
//! this call; no scope, layer, snapshot or authorization result is cached.

use std::collections::BTreeSet;

use norn_ir::ObjectTypeName;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/inheritance_ascent.rs"));
}

pub(crate) use generated::CYCLE_RULE_ID;

/// Registered refusal: no partial classification may reach ancestry walkers.
pub(crate) const BUDGET_EXHAUSTED: &str = "object_type.validation.inheritance_budget_exhausted";

#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    pub(crate) types: usize,
    pub(crate) text_bytes: usize,
    pub(crate) steps: usize,
    pub(crate) facts: usize,
    pub(crate) work: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            types: 4_096,
            text_bytes: 1_048_576,
            steps: 65_536,
            facts: 1_048_576,
            work: 32_000_000,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Usage {
    work: usize,
    facts: usize,
    steps: usize,
    text_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) dimension: &'static str,
    pub(crate) limit: usize,
}

struct Meter {
    budget: Budget,
    usage: Usage,
}

fn reserve(
    value: &mut usize,
    amount: usize,
    limit: usize,
    dimension: &'static str,
) -> Result<(), Refusal> {
    *value = value
        .checked_add(amount)
        .filter(|total| *total <= limit)
        .ok_or(Refusal { dimension, limit })?;
    Ok(())
}

impl Meter {
    fn work(&mut self, amount: usize) -> Result<(), Refusal> {
        reserve(&mut self.usage.work, amount, self.budget.work, "work")
    }

    fn input(&mut self, name: &str) -> Result<(), Refusal> {
        reserve(
            &mut self.usage.text_bytes,
            name.len(),
            self.budget.text_bytes,
            "text bytes",
        )?;
        self.work(name.len())
    }

    fn facts(&mut self, amount: usize) -> Result<(), Refusal> {
        reserve(
            &mut self.usage.facts,
            amount,
            self.budget.facts,
            "materialized facts",
        )
    }

    fn step(&mut self, tree_height: usize) -> Result<(), Refusal> {
        reserve(
            &mut self.usage.steps,
            1,
            self.budget.steps,
            "frontier steps",
        )?;
        // Three input rows, at most two derived rows, and fresh indexes every
        // time. The fixed AST has no recursive SCC: two rules, each joining
        // singleton relations. Reserve all five rows and 256 logical work
        // units for setup, input/index rebuild, both joins, fixed-point control,
        // output materialization and teardown, plus ordered seen-set work.
        self.facts(5)?;
        self.work(256_usize.saturating_add(tree_height.saturating_mul(16)))
    }
}

pub(crate) struct Classification<'a> {
    pub(crate) cyclic: BTreeSet<&'a ObjectTypeName>,
    #[cfg(test)]
    usage: Usage,
}

/// `entries` is exactly the checker's sorted, registered local/base catalog.
/// Unknown parents yield no edge; their existing diagnostic remains separate.
pub(crate) fn classify<'a>(
    entries: impl ExactSizeIterator<Item = (&'a ObjectTypeName, Option<&'a ObjectTypeName>)>,
    budget: Budget,
) -> Result<Classification<'a>, Refusal> {
    let size = entries.len();
    if size > budget.types {
        return Err(Refusal {
            dimension: "registered types",
            limit: budget.types,
        });
    }
    let mut meter = Meter {
        budget,
        usage: Usage::default(),
    };
    meter.work(size)?;
    meter.facts(size.saturating_mul(2))?; // registered types and possible parent edges
    let mut catalog = Vec::with_capacity(size);
    let mut longest = 0;
    for (name, parent) in entries {
        meter.input(name.as_str())?;
        longest = longest.max(name.as_str().len());
        if let Some(parent) = parent {
            meter.input(parent.as_str())?;
            longest = longest.max(parent.as_str().len());
        }
        meter.work(2)?;
        catalog.push((name, parent));
    }
    // Binary-search name comparisons and ordered seen sets are charged with
    // conservative bounds before their operations, including variable strings.
    let height = usize::try_from(usize::BITS.saturating_sub(size.leading_zeros()))
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let lookup_work = height.saturating_mul(longest.saturating_add(1));
    for pair in catalog.windows(2) {
        meter.work(lookup_work)?;
        if let [(left, _), (right, _)] = pair
            && left >= right
        {
            // Only the private sorted registered adapter is admitted.
            return Err(Refusal {
                dimension: "catalog ordering",
                limit: 0,
            });
        }
    }
    meter.work(size)?;
    let mut parents = Vec::with_capacity(size);
    for (_, parent) in &catalog {
        meter.work(lookup_work)?;
        parents.push(parent.and_then(|parent| {
            catalog
                .binary_search_by(|(name, _)| (*name).cmp(parent))
                .ok()
        }));
    }
    let mut cyclic = BTreeSet::new();
    for (root, (name, _)) in catalog.iter().enumerate() {
        meter.work(height.saturating_mul(16))?;
        let mut seen = BTreeSet::new();
        let mut current = root;
        while let Some(parent) = parents.get(current).copied().flatten() {
            meter.step(height)?;
            let mut query = generated::RegisteredInheritanceStep::default();
            // Scope zero is private to this single selected catalog invocation;
            // it is not a wildcard and cannot join another catalog's facts.
            query.query_root.push((0, root));
            query.frontier.push((0, current));
            query.extends.push((0, current, parent));
            query.run();
            if !query.inheritance_cycle.is_empty() {
                meter.work(lookup_work.saturating_mul(16))?;
                cyclic.insert(*name);
                break;
            }
            let Some((_, next)) = query.next.first() else {
                break;
            };
            if !seen.insert(*next) {
                break;
            }
            current = *next;
        }
    }
    Ok(Classification {
        cyclic,
        #[cfg(test)]
        usage: meter.usage,
    })
}

#[cfg(test)]
mod tests;
