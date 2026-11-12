//! The order of the commit admission stages, as one straight-line driver
//! (#718 P8, K1/K3/K4).
//!
//! [`run_stages`] is the only place the order lives. `check_commit` supplies
//! the stage bodies through [`CommitStages`]; this file decides which stage
//! runs next and that the first refusal ends admission. It depends on `core`
//! alone, so the proof tools extract this production file itself
//! (`scripts/verify-kernel-commit-aeneas.sh`).

/// One admission stage of `check_commit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitStage {
    /// A prepared observation's event limit.
    EventLimit,
    /// No key twice, declared links, admitted targets.
    Shape,
    /// Declared posting groups decode, are complete and balance.
    Accounting,
    /// The consumed delivery, if any, is pending.
    Delivery,
    /// A write never replaces an immutable object it observed.
    Immutable,
    /// Identity reuse, compare-and-swap and type checks on reads and writes.
    Versions,
    /// Link writes carry their source's authority.
    LinkAuthority,
    /// No link dangles once the commit applies.
    Endpoints,
    /// Every added link's values are its declaration's effective values.
    LinkValues,
    /// To-one and from-one cardinality after the commit applies.
    Cardinality,
    /// Required owned collections keep at least one link.
    RequiredLinks,
    /// Acyclic link declarations stay acyclic.
    Acyclic,
    /// Every final `Put` satisfies its type's invariants.
    Invariants,
    /// Every final `Put` satisfies its governing laws.
    Laws,
}

/// The stage bodies the driver sequences. `run` checks one stage; a refusal
/// is returned as `Err` and ends admission.
pub(crate) trait CommitStages<E> {
    /// Checks `stage`.
    ///
    /// # Errors
    ///
    /// The stage's refusal.
    fn run(&mut self, stage: CommitStage) -> Result<(), E>;
}

/// Runs every admission stage in order and returns the first refusal. No
/// stage runs after a refusal.
///
/// # Errors
///
/// The refusal of the first stage that refuses.
pub(crate) fn run_stages<E, S: CommitStages<E>>(stages: &mut S) -> Result<(), E> {
    stages.run(CommitStage::EventLimit)?;
    stages.run(CommitStage::Shape)?;
    stages.run(CommitStage::Accounting)?;
    stages.run(CommitStage::Delivery)?;
    stages.run(CommitStage::Immutable)?;
    stages.run(CommitStage::Versions)?;
    stages.run(CommitStage::LinkAuthority)?;
    stages.run(CommitStage::Endpoints)?;
    stages.run(CommitStage::LinkValues)?;
    stages.run(CommitStage::Cardinality)?;
    stages.run(CommitStage::RequiredLinks)?;
    stages.run(CommitStage::Acyclic)?;
    stages.run(CommitStage::Invariants)?;
    stages.run(CommitStage::Laws)
}
