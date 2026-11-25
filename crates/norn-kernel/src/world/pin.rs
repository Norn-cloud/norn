//! P5 (#1698): typed world identity and the pin every snapshot read carries.
//!
//! A `&str` world accepts `"main"` from a fork; [`WorldId`] does not. A fork
//! is bound to the ledger entry it left its parent at ([`LedgerAnchor`]), so
//! two forks that share a name but not a base are different worlds. The wire
//! form is `main` for the main world (unchanged) and
//! `fork:<seq>:<entry hash>:<name>` for a fork; [`WorldId::parse`] accepts
//! exactly what [`WorldId::encode`] writes.
//!
//! [`Bounded`] keeps a truncated answer distinct from a complete one: it has
//! no accessor that drops the distinction, so a consumer must `match`.

use std::fmt;

use norn_ir::cell_id::CellId;
use serde::{Deserialize, Serialize};

use crate::read_envelope::DefinitionPin;

/// The wire form of the main world: the only `main` literal a typed world
/// needs.
pub const MAIN: &str = "main";

const FORK_PREFIX: &str = "fork:";

/// An immutable ledger entry: its sequence number and its hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerAnchor {
    /// The entry's sequence number (`0` for the empty ledger).
    pub seq: u64,
    /// The entry's hash: 64 lowercase hex digits.
    pub entry_hash: String,
}

/// Why a world identity is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorldIdError {
    /// A fork name is empty or is the main world's name.
    #[error("a fork name must be non-empty and not `main`")]
    ForkName,
    /// A ledger anchor hash is not 64 lowercase hex digits.
    #[error("a ledger anchor hash must be 64 lowercase hex digits")]
    AnchorHash,
    /// A fork's base sequence is not a canonical decimal `u64`.
    #[error("a fork base sequence must be a canonical decimal u64")]
    AnchorSeq,
    /// The text is neither `main` nor a `fork:` world.
    #[error("a world is `main` or `fork:<seq>:<hash>:<name>`")]
    Unknown,
}

impl LedgerAnchor {
    /// An anchor at `seq` with `entry_hash`.
    ///
    /// # Errors
    ///
    /// [`WorldIdError::AnchorHash`] unless `entry_hash` is 64 lowercase hex
    /// digits.
    pub fn new(seq: u64, entry_hash: impl Into<String>) -> Result<Self, WorldIdError> {
        let entry_hash = entry_hash.into();
        if entry_hash.len() != 64
            || !entry_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(WorldIdError::AnchorHash);
        }
        Ok(Self { seq, entry_hash })
    }
}

/// A fork world: its name and the parent ledger entry it was taken at.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ForkId {
    name: String,
    base: LedgerAnchor,
}

impl ForkId {
    /// The fork `name` taken at `base`.
    ///
    /// # Errors
    ///
    /// [`WorldIdError::ForkName`] for an empty name or `main`;
    /// [`WorldIdError::AnchorHash`] for a malformed base hash.
    pub fn new(name: impl Into<String>, base: LedgerAnchor) -> Result<Self, WorldIdError> {
        let name = name.into();
        if name.is_empty() || name == MAIN {
            return Err(WorldIdError::ForkName);
        }
        let base = LedgerAnchor::new(base.seq, base.entry_hash)?;
        Ok(Self { name, base })
    }

    /// The fork's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parent ledger entry the fork was taken at.
    #[must_use]
    pub const fn base(&self) -> &LedgerAnchor {
        &self.base
    }
}

/// The world a read or a write belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum WorldId {
    /// The tenant's authoritative world.
    Main,
    /// A scenario fork, bound to its base.
    Fork(ForkId),
}

impl WorldId {
    /// Whether this is the main world.
    #[must_use]
    pub const fn is_main(&self) -> bool {
        matches!(self, Self::Main)
    }

    /// The wire form: `main`, or `fork:<seq>:<entry hash>:<name>`.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Main => MAIN.to_owned(),
            Self::Fork(fork) => format!(
                "{FORK_PREFIX}{}:{}:{}",
                fork.base.seq, fork.base.entry_hash, fork.name
            ),
        }
    }

    /// The world `text` names: exactly the inverse of [`Self::encode`].
    ///
    /// # Errors
    ///
    /// A [`WorldIdError`] for any text [`Self::encode`] does not write.
    pub fn parse(text: &str) -> Result<Self, WorldIdError> {
        if text == MAIN {
            return Ok(Self::Main);
        }
        let rest = text
            .strip_prefix(FORK_PREFIX)
            .ok_or(WorldIdError::Unknown)?;
        let (seq, rest) = rest.split_once(':').ok_or(WorldIdError::Unknown)?;
        let (hash, name) = rest.split_once(':').ok_or(WorldIdError::Unknown)?;
        let canonical = seq.bytes().all(|byte| byte.is_ascii_digit())
            && !seq.is_empty()
            && (seq == "0" || !seq.starts_with('0'));
        if !canonical {
            return Err(WorldIdError::AnchorSeq);
        }
        let seq = seq.parse().map_err(|_overflow| WorldIdError::AnchorSeq)?;
        Ok(Self::Fork(ForkId::new(
            name,
            LedgerAnchor::new(seq, hash)?,
        )?))
    }
}

impl fmt::Display for WorldId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.encode())
    }
}

impl From<WorldId> for String {
    fn from(world: WorldId) -> Self {
        world.encode()
    }
}

impl TryFrom<String> for WorldId {
    type Error = WorldIdError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

/// What one snapshot read is pinned to: the cell, the world, the revision
/// the rows were read at, and the definitions that interpret them. Every
/// artifact derived from the read carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotPin {
    /// The cell.
    pub cell: CellId,
    /// The world.
    pub world: WorldId,
    /// The revision (commit count) the snapshot reads at.
    pub revision: u64,
    /// The definitions in force at that revision.
    pub definitions: DefinitionPin,
}

/// A bounded answer that is either complete or says it was cut short. There
/// is no accessor that drops the distinction: a consumer matches.
#[must_use = "a bounded answer may be truncated; match it"]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Bounded<T> {
    /// Every member.
    Complete {
        /// The answer.
        value: T,
    },
    /// The first `limit` members seen; more exist.
    Truncated {
        /// The members seen.
        seen: T,
        /// The bound that cut the answer short.
        limit: u32,
    },
}

impl<T> Bounded<T> {
    /// Whether every member is present.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete { .. })
    }

    /// The same bound over a transformed answer.
    pub fn map<U>(self, transform: impl FnOnce(T) -> U) -> Bounded<U> {
        match self {
            Self::Complete { value } => Bounded::Complete {
                value: transform(value),
            },
            Self::Truncated { seen, limit } => Bounded::Truncated {
                seen: transform(seen),
                limit,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

    fn fork(name: &str, seq: u64) -> WorldId {
        WorldId::Fork(ForkId::new(name, LedgerAnchor::new(seq, HASH).unwrap()).unwrap())
    }

    #[test]
    fn the_world_codec_round_trips_and_main_never_equals_a_fork() {
        let worlds = [
            WorldId::Main,
            fork("what-if", 0),
            fork("what-if", 7),
            fork("main:shadow", u64::MAX),
            fork("a:b:c", 1),
        ];
        for world in &worlds {
            let wire = world.encode();
            assert_eq!(WorldId::parse(&wire).as_ref(), Ok(world), "{wire}");
            let json = serde_json::to_string(world).unwrap();
            assert_eq!(&serde_json::from_str::<WorldId>(&json).unwrap(), world);
            assert_eq!(world.is_main(), wire == MAIN);
        }
        for (index, left) in worlds.iter().enumerate() {
            for (other, right) in worlds.iter().enumerate() {
                assert_eq!(left == right, index == other);
                assert_eq!(left.encode() == right.encode(), index == other);
            }
        }
        assert_eq!(serde_json::to_string(&WorldId::Main).unwrap(), "\"main\"");
    }

    #[test]
    fn malformed_worlds_are_refused() {
        let refused: Vec<(String, WorldIdError)> = vec![
            (String::new(), WorldIdError::Unknown),
            ("Main".to_owned(), WorldIdError::Unknown),
            ("what-if".to_owned(), WorldIdError::Unknown),
            ("fork:".to_owned(), WorldIdError::Unknown),
            (format!("fork:1:{HASH}"), WorldIdError::Unknown),
            (format!("fork:1:{HASH}:"), WorldIdError::ForkName),
            (format!("fork:1:{HASH}:main"), WorldIdError::ForkName),
            (format!("fork:01:{HASH}:x"), WorldIdError::AnchorSeq),
            (format!("fork:+1:{HASH}:x"), WorldIdError::AnchorSeq),
            (format!("fork::{HASH}:x"), WorldIdError::AnchorSeq),
            (
                format!("fork:18446744073709551616:{HASH}:x"),
                WorldIdError::AnchorSeq,
            ),
            ("fork:1:ABC:x".to_owned(), WorldIdError::AnchorHash),
            (
                format!("fork:1:{}:x", HASH.to_uppercase()),
                WorldIdError::AnchorHash,
            ),
        ];
        for (text, error) in refused {
            assert_eq!(WorldId::parse(&text), Err(error), "{text:?}");
        }
        assert!(serde_json::from_str::<WorldId>("\"fork:x\"").is_err());
    }

    #[test]
    fn a_bounded_answer_keeps_truncation_through_map() {
        let complete: Bounded<Vec<u8>> = Bounded::Complete { value: vec![1, 2] };
        let truncated: Bounded<Vec<u8>> = Bounded::Truncated {
            seen: vec![1],
            limit: 1,
        };
        assert!(complete.is_complete());
        assert!(!truncated.is_complete());
        assert_eq!(
            truncated.map(|seen| seen.len()),
            Bounded::Truncated { seen: 1, limit: 1 }
        );
        assert_eq!(
            complete.map(|value| value.len()),
            Bounded::Complete { value: 2 }
        );
    }
}
