//! GRAPH-ROLE-1: roles are declared or inherited; they are never inferred.
//!
//! This file has no dependencies so it compiles unchanged in every
//! configuration of this crate. The wire encoding lives in the sibling
//! `concept_role_wire` module.

/// Semantic role of an ordinary concept, independent of lifecycle presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

#[cfg(test)]
mod tests {
    use super::{ConceptRole, RoleConflict};

    #[test]
    fn role_meet_refuses_exactly_conflict_or_mismatch_up_to_four_parents() {
        let declared_choices = [None, Some(ConceptRole::Thing), Some(ConceptRole::Happening)];
        let mask_counts: [(usize, u8); 5] = [(0, 1), (1, 2), (2, 4), (3, 8), (4, 16)];
        let bit_masks: [u8; 4] = [1, 2, 4, 8];
        let mut checked = 0_usize;

        for declared in declared_choices {
            for (len, total_masks) in mask_counts {
                for mask in 0..total_masks {
                    let mut inherited = Vec::with_capacity(len);
                    for &bit_mask in bit_masks.iter().take(len) {
                        let is_happening = (mask & bit_mask) != 0;
                        inherited.push(if is_happening {
                            ConceptRole::Happening
                        } else {
                            ConceptRole::Thing
                        });
                    }
                    let thing = inherited.contains(&ConceptRole::Thing);
                    let happening = inherited.contains(&ConceptRole::Happening);
                    let result = ConceptRole::resolve(declared, &inherited);
                    if thing && happening {
                        assert_eq!(result, Err(RoleConflict::Inherited));
                    } else if (declared == Some(ConceptRole::Happening) && thing)
                        || (declared == Some(ConceptRole::Thing) && happening)
                    {
                        assert_eq!(result, Err(RoleConflict::Declared));
                    } else {
                        assert_eq!(
                            result,
                            Ok(declared.unwrap_or(if happening {
                                ConceptRole::Happening
                            } else {
                                ConceptRole::Thing
                            }))
                        );
                    }
                    checked = checked.saturating_add(1);
                }
            }
        }

        assert_eq!(checked, 93);
    }
}
