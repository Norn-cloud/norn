//! GRAPH-ROLE-1 checks the production role meet for up to four parents.
#[path = "../../crates/norn-ir/src/entity_v2/concept_role.rs"]
mod concept_role;
use concept_role::{ConceptRole, RoleConflict};
fn role(value: bool) -> ConceptRole {
    if value {
        ConceptRole::Happening
    } else {
        ConceptRole::Thing
    }
}

#[kani::proof]
#[kani::unwind(6)]
fn role_meet_refuses_exactly_conflict_or_mismatch() {
    let declared = if kani::any() {
        Some(role(kani::any()))
    } else {
        None
    };
    let parents = [
        role(kani::any()),
        role(kani::any()),
        role(kani::any()),
        role(kani::any()),
    ];
    let len: usize = kani::any();
    kani::assume(len <= parents.len());
    let inherited = &parents[..len];
    let thing = inherited.contains(&ConceptRole::Thing);
    let happening = inherited.contains(&ConceptRole::Happening);
    let result = ConceptRole::resolve(declared, inherited);
    if thing && happening {
        assert_eq!(result, Err(RoleConflict::Inherited));
    } else if declared == Some(ConceptRole::Happening) && thing
        || declared == Some(ConceptRole::Thing) && happening
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
}
