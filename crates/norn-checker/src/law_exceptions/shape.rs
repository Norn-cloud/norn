//! Exception-tree shape check and fold over abstract identities: the proved
//! core of `law_exceptions` (#718 U1). It uses only `core`, indexes slices with
//! `get`, and is generic over the identity type, so Charon/Aeneas extract this
//! file standalone and the Lean proof needs only lawful equality, not a string
//! model.

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
    /// The tree exceeds `MAX_LAW_EXCEPTIONS`.
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
            Self::InvalidDeclaration(index)
            | Self::DanglingOverride(index)
            | Self::Cycle(index) => Some(index),
            Self::Limit => None,
        }
    }
}

/// Index of the first identity equal to `id`.
fn position<T: PartialEq>(ids: &[T], id: &T) -> Option<usize> {
    let mut index = 0_usize;
    while let Some(candidate) = ids.get(index) {
        if candidate == id {
            return Some(index);
        }
        let Some(next) = index.checked_add(1) else {
            break;
        };
        index = next;
    }
    None
}

/// Whether following `parents` from exception `start` reaches `law` within
/// `ids.len()` lookups. Each node has one parent, so a longer walk is a cycle.
fn reaches_law<T: PartialEq>(law: &T, ids: &[T], parents: &[T], start: usize) -> bool {
    let mut node = start;
    let mut steps = 0_usize;
    while steps <= ids.len() {
        let Some(parent) = parents.get(node) else {
            return false;
        };
        if parent == law {
            return true;
        }
        let Some(next) = position(ids, parent) else {
            return false;
        };
        node = next;
        let Some(next_steps) = steps.checked_add(1) else {
            return false;
        };
        steps = next_steps;
    }
    false
}

/// The first exception whose declaration is invalid (`declared[i]` false),
/// whose identity is the law's, or whose identity repeats an earlier one.
fn first_invalid<T: PartialEq>(law: &T, ids: &[T], declared: &[bool]) -> Option<usize> {
    let mut index = 0_usize;
    while let Some(id) = ids.get(index) {
        let first = match position(ids, id) {
            Some(first) => first == index,
            None => false,
        };
        let Some(&valid) = declared.get(index) else {
            return Some(index);
        };
        if !valid || id == law || !first {
            return Some(index);
        }
        let Some(next) = index.checked_add(1) else {
            break;
        };
        index = next;
    }
    None
}

/// The first exception whose target is neither the law nor a declared identity.
fn first_dangling<T: PartialEq>(law: &T, ids: &[T], parents: &[T]) -> Option<usize> {
    let mut index = 0_usize;
    while let Some(parent) = parents.get(index) {
        if parent != law && position(ids, parent).is_none() {
            return Some(index);
        }
        let Some(next) = index.checked_add(1) else {
            break;
        };
        index = next;
    }
    None
}

/// The first exception from which following targets never reaches the law.
fn first_cycle<T: PartialEq>(law: &T, ids: &[T], parents: &[T]) -> Option<usize> {
    let mut index = 0_usize;
    while index < parents.len() {
        if !reaches_law(law, ids, parents, index) {
            return Some(index);
        }
        let Some(next) = index.checked_add(1) else {
            break;
        };
        index = next;
    }
    None
}

/// Check that exceptions `i` with identity `ids[i]`, `overrides` target
/// `parents[i]` and string-level validity `declared[i]` form one tree rooted at
/// `law`: bounded, valid declarations with unique identities distinct from the
/// law, every target present, and no cycle. The three slices have one entry per
/// exception.
///
/// # Errors
/// The first refusal in declaration order.
pub(crate) fn check_shape<T: PartialEq>(
    law: &T,
    ids: &[T],
    parents: &[T],
    declared: &[bool],
) -> Result<(), ExceptionTreeRefusal> {
    if ids.len() > MAX_LAW_EXCEPTIONS {
        return Err(ExceptionTreeRefusal::Limit);
    }
    if let Some(index) = first_invalid(law, ids, declared) {
        return Err(ExceptionTreeRefusal::InvalidDeclaration(index));
    }
    if let Some(index) = first_dangling(law, ids, parents) {
        return Err(ExceptionTreeRefusal::DanglingOverride(index));
    }
    if let Some(index) = first_cycle(law, ids, parents) {
        return Err(ExceptionTreeRefusal::Cycle(index));
    }
    Ok(())
}

/// Whether some exception overriding `node` has a holding condition (`matched`,
/// a missing entry is false) and is not lifted in `level` (a missing entry is
/// lifted).
fn overridden<T: PartialEq>(node: &T, parents: &[T], matched: &[bool], level: &[bool]) -> bool {
    let mut index = 0_usize;
    while let Some(parent) = parents.get(index) {
        if parent == node
            && let (Some(true), Some(false)) = (matched.get(index), level.get(index))
        {
            return true;
        }
        let Some(next) = index.checked_add(1) else {
            return false;
        };
        index = next;
    }
    false
}

/// One round of the fold: `out[k]` becomes whether exception `k` is lifted
/// given the previous round `level`.
fn next_level<T: PartialEq>(
    ids: &[T],
    parents: &[T],
    matched: &[bool],
    level: &[bool],
    out: &mut [bool],
) {
    let mut index = 0_usize;
    while let Some(id) = ids.get(index) {
        let lifted = overridden(id, parents, matched, level);
        if let Some(slot) = out.get_mut(index) {
            *slot = lifted;
        }
        let Some(next) = index.checked_add(1) else {
            return;
        };
        index = next;
    }
}

/// Set every entry of `target` to the matching entry of `source`.
fn copy_level(source: &[bool], target: &mut [bool]) {
    let mut index = 0_usize;
    while let Some(value) = source.get(index) {
        if let Some(slot) = target.get_mut(index) {
            *slot = *value;
        }
        let Some(next) = index.checked_add(1) else {
            return;
        };
        index = next;
    }
}

/// Set every entry of `target` to `false`.
fn clear_level(target: &mut [bool]) {
    let mut index = 0_usize;
    while let Some(slot) = target.get_mut(index) {
        *slot = false;
        let Some(next) = index.checked_add(1) else {
            return;
        };
        index = next;
    }
}

/// Whether the law `law` is lifted, given the evaluated conditions `matched`:
/// some exception overriding it has a holding condition and is not itself
/// lifted, where an exception is lifted when some exception overriding it is
/// matched and not lifted. The fold runs `ids.len()` rounds over the scratch
/// slices `level` and `next` (one entry per exception); afterwards `level[k]`
/// is whether exception `k` is lifted. Siblings never short-circuit each
/// other, and no recursion is used.
pub(crate) fn lifted<T: PartialEq>(
    law: &T,
    ids: &[T],
    parents: &[T],
    matched: &[bool],
    level: &mut [bool],
    next: &mut [bool],
) -> bool {
    clear_level(level);
    let mut round = 0_usize;
    while round < ids.len() {
        next_level(ids, parents, matched, level, next);
        copy_level(next, level);
        let Some(following) = round.checked_add(1) else {
            return false;
        };
        round = following;
    }
    overridden(law, parents, matched, level)
}

/// Whether every exception on the path from exception `index` up to the law,
/// excluding `index` itself, has a holding condition. A path longer than the
/// tree, or a missing target, is not one.
fn ancestors_hold<T: PartialEq>(
    law: &T,
    ids: &[T],
    parents: &[T],
    matched: &[bool],
    index: usize,
) -> bool {
    let mut node = index;
    let mut steps = 0_usize;
    while steps <= ids.len() {
        let Some(parent) = parents.get(node) else {
            return false;
        };
        if parent == law {
            return true;
        }
        let Some(next) = position(ids, parent) else {
            return false;
        };
        if !matches!(matched.get(next), Some(true)) {
            return false;
        }
        node = next;
        let Some(next_steps) = steps.checked_add(1) else {
            return false;
        };
        steps = next_steps;
    }
    false
}

/// After [`lifted`], `applied[k]` becomes whether exception `k` applies and
/// every exception above it holds: its condition holds, it is not lifted in
/// `level`, and its ancestors' conditions hold.
pub(crate) fn applied_exceptions<T: PartialEq>(
    law: &T,
    ids: &[T],
    parents: &[T],
    matched: &[bool],
    level: &[bool],
    applied: &mut [bool],
) {
    let mut index = 0_usize;
    while let Some(slot) = applied.get_mut(index) {
        *slot = matches!(
            (matched.get(index), level.get(index)),
            (Some(true), Some(false))
        ) && ancestors_hold(law, ids, parents, matched, index);
        let Some(next) = index.checked_add(1) else {
            return;
        };
        index = next;
    }
}
