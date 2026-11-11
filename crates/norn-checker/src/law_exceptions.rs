//! Shape of a law's declared exception tree, and the tree fold over already
//! evaluated conditions. Structural only: expression evaluation, budgets and
//! commit-path semantics stay with their callers.
use norn_ir::LawException;
use std::collections::{BTreeMap, BTreeSet};

/// Most exceptions one law (or one rule) may declare. Beyond it, admission is
/// unknown, and unknown refuses.
pub const MAX_LAW_EXCEPTIONS: usize = 64;

/// Stable exception-tree refusal codes.
pub mod codes {
    /// A blank or duplicate id, an id equal to the law id, a blank rationale
    /// or an invalid source span.
    pub const INVALID_DECLARATION: &str = "law.exception.invalid_declaration";
    /// `overrides` names neither the law nor an exception of the same tree.
    pub const DANGLING_OVERRIDE: &str = "law.exception.dangling_override";
    /// Following `overrides` from an exception never reaches the law.
    pub const CYCLE: &str = "law.exception.cycle";
    /// The tree exceeds [`super::MAX_LAW_EXCEPTIONS`].
    pub const LIMIT: &str = "law.exception.limit";
    /// The law's response has no meaning under an exception (an invariant
    /// identity is unconditional).
    pub const UNSUPPORTED_RESPONSE: &str = "law.exception.unsupported_response";
    /// A rule declares exceptions both on itself and on its embedded law.
    pub const TWO_HOMES: &str = "law.exception.two_homes";
}

/// Why an exception tree is refused, naming the first offending declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionTreeRefusal {
    /// See [`codes::INVALID_DECLARATION`].
    InvalidDeclaration(usize),
    /// See [`codes::DANGLING_OVERRIDE`].
    DanglingOverride(usize),
    /// See [`codes::CYCLE`].
    Cycle(usize),
    /// See [`codes::LIMIT`].
    Limit,
}

impl ExceptionTreeRefusal {
    /// Stable locale-independent diagnostic identity.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidDeclaration(_) => codes::INVALID_DECLARATION,
            Self::DanglingOverride(_) => codes::DANGLING_OVERRIDE,
            Self::Cycle(_) => codes::CYCLE,
            Self::Limit => codes::LIMIT,
        }
    }

    /// Index of the offending exception, when one is named.
    #[must_use]
    pub const fn index(self) -> Option<usize> {
        match self {
            Self::InvalidDeclaration(index) | Self::DanglingOverride(index) | Self::Cycle(index) => {
                Some(index)
            }
            Self::Limit => None,
        }
    }
}

/// Check that `exceptions` form one tree rooted at `law_id`: bounded, unique
/// nonblank ids distinct from the law, nonblank rationales, valid sources,
/// every `overrides` target present, and no cycle. Overlap between siblings is
/// not checked: lift-only siblings always agree.
///
/// # Errors
/// The first refusal in declaration order.
pub fn check_tree(law_id: &str, exceptions: &[LawException]) -> Result<(), ExceptionTreeRefusal> {
    use ExceptionTreeRefusal as R;
    if exceptions.len() > MAX_LAW_EXCEPTIONS {
        return Err(R::Limit);
    }
    let mut ids = BTreeMap::new();
    for (index, exception) in exceptions.iter().enumerate() {
        if exception.id.trim().is_empty()
            || exception.id == law_id
            || exception.rationale.trim().is_empty()
            || !crate::deontic::valid_source(&exception.source)
            || ids.insert(exception.id.as_str(), index).is_some()
        {
            return Err(R::InvalidDeclaration(index));
        }
    }
    for (index, exception) in exceptions.iter().enumerate() {
        if exception.overrides != law_id && !ids.contains_key(exception.overrides.as_str()) {
            return Err(R::DanglingOverride(index));
        }
    }
    for (index, exception) in exceptions.iter().enumerate() {
        // Each node has one parent, so a walk longer than the tree is a cycle.
        let mut parent = exception.overrides.as_str();
        let mut steps = 0_usize;
        while parent != law_id {
            steps = steps.saturating_add(1);
            let next = ids.get(parent).and_then(|i| exceptions.get(*i));
            let Some(node) = next.filter(|_| steps <= exceptions.len()) else {
                return Err(R::Cycle(index));
            };
            parent = node.overrides.as_str();
        }
    }
    Ok(())
}

/// Fold an admitted tree over its evaluated conditions (`matched[i]` is the
/// value of `exceptions[i].condition`). An exception applies when its
/// condition holds and no exception overriding it applies. Returns whether the
/// law is lifted (some exception overriding the law applies) and every applied
/// exception id, sorted. Declaration order never affects the result.
///
/// The caller must have passed [`check_tree`]; a `matched` of another length
/// treats the missing conditions as false.
#[must_use]
pub fn fold<'a>(
    law_id: &str,
    exceptions: &'a [LawException],
    matched: &[bool],
) -> (bool, BTreeSet<&'a str>) {
    let mut children: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, exception) in exceptions.iter().enumerate() {
        children
            .entry(exception.overrides.as_str())
            .or_default()
            .push(index);
    }
    let mut applied = BTreeSet::new();
    let lifted = lifts(law_id, exceptions, matched, &children, &mut applied, 0);
    (lifted, applied)
}

/// Whether some child of `node` applies; records every applied descendant.
/// Siblings never short-circuit each other.
fn lifts<'a>(
    node: &str,
    exceptions: &'a [LawException],
    matched: &[bool],
    children: &BTreeMap<&str, Vec<usize>>,
    applied: &mut BTreeSet<&'a str>,
    depth: usize,
) -> bool {
    let mut any = false;
    if depth > exceptions.len() {
        return any;
    }
    for &index in children.get(node).into_iter().flatten() {
        let Some(exception) = exceptions.get(index) else {
            continue;
        };
        if !matched.get(index).copied().unwrap_or(false) {
            continue;
        }
        let overridden = lifts(
            exception.id.as_str(),
            exceptions,
            matched,
            children,
            applied,
            depth.saturating_add(1),
        );
        if !overridden {
            applied.insert(exception.id.as_str());
            any = true;
        }
    }
    any
}
