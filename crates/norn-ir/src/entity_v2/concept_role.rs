//! GRAPH-ROLE-1: roles are declared or inherited; they are never inferred.
#![allow(
    unexpected_cfgs,
    reason = "also compiled directly by the standalone Kani harness"
)]

/// Semantic role of an ordinary concept, independent of lifecycle presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(
    not(kani),
    derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)
)]
#[cfg_attr(not(kani), serde(rename_all = "snake_case"))]
pub enum ConceptRole {
    /// A persistent thing.
    Thing,
    /// A concept that occurs, runs or terminates in time.
    Happening,
}

/// A role contract cannot be weakened by specialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleConflict {
    /// Parents supply different roles.
    Inherited,
    /// A declaration contradicts its parents.
    Declared,
}

impl ConceptRole {
    /// GRAPH-ROLE-1: meet every parent contract before accepting a declaration.
    /// With no declaration or parent, the documented default is `Thing`.
    ///
    /// # Errors
    /// Refuses conflicting parents or a declaration that contradicts them.
    pub fn resolve(declared: Option<Self>, inherited: &[Self]) -> Result<Self, RoleConflict> {
        let mut parent = None;
        for role in inherited {
            if parent.is_some_and(|previous| previous != *role) {
                return Err(RoleConflict::Inherited);
            }
            parent = Some(*role);
        }
        match (declared, parent) {
            (Some(local), Some(parent)) if local != parent => Err(RoleConflict::Declared),
            _ => Ok(declared.or(parent).unwrap_or(Self::Thing)),
        }
    }
}
