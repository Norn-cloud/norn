//! GRAPH-ROLE-1 resolves the existing specialization DAG without role inference.
use crate::entity_model::EntityModel;
use norn_ir::{ConceptRole, RoleConflict};
use std::collections::BTreeMap;

pub(crate) fn resolve(
    model: EntityModel<'_>,
    base: EntityModel<'_>,
) -> BTreeMap<String, Result<ConceptRole, RoleConflict>> {
    let mut nodes = BTreeMap::new();
    for layer in [base, model] {
        for def in layer.interfaces {
            nodes
                .entry(def.name.as_str().to_owned())
                .or_insert_with(|| {
                    (
                        def.role,
                        def.extends
                            .iter()
                            .map(|n| n.as_str().to_owned())
                            .collect::<Vec<_>>(),
                    )
                });
        }
        for def in layer.object_types {
            nodes
                .entry(def.name.as_str().to_owned())
                .or_insert_with(|| {
                    (
                        def.role,
                        def.extends
                            .iter()
                            .map(|n| n.as_str().to_owned())
                            .chain(def.implements.iter().map(|n| n.as_str().to_owned()))
                            .collect(),
                    )
                });
        }
    }
    let mut roles: BTreeMap<String, Result<ConceptRole, RoleConflict>> = BTreeMap::new();
    loop {
        let mut changed = false;
        for (name, (declared, parents)) in &nodes {
            if roles.contains_key(name) {
                continue;
            }
            let Some(inherited) = parents
                .iter()
                .map(|name| roles.get(name).and_then(|role| role.as_ref().ok()).copied())
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let role = ConceptRole::resolve(*declared, &inherited);
            roles.insert(name.clone(), role);
            changed = true;
        }
        if !changed {
            return roles;
        }
    }
}
