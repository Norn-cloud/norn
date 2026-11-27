//! Interface catalog projection and transition lineage resolution
//! (`INTERFACE-LIFECYCLE-1`).

use std::collections::BTreeSet;

use norn_ir::definition_catalog::{
    ArtifactKey, LifecycleStates, Member, PropertyRow, TransitionRow, TypeKind, TypeRow,
};
use norn_ir::{ArtifactEnvelope, ObjectTypeDef, PropertyType};

use super::{PinnedType, Rows, at, key, property_member, type_member};
use crate::action_set::AdmittedType;
use crate::entity_scopes::ScopedView;

type InterfaceSlot<'a> = Option<(&'a ArtifactEnvelope, &'a norn_ir::InterfaceDef)>;

fn collect_interface_post_order<'a>(
    name: &'a str,
    ifaces: &std::collections::BTreeMap<&'a str, InterfaceSlot<'a>>,
    visited: &mut BTreeSet<&'a str>,
    out: &mut Vec<(&'a ArtifactEnvelope, &'a norn_ir::InterfaceDef)>,
) {
    if !visited.insert(name) {
        return;
    }
    let Some(Some((envelope, def))) = ifaces.get(name) else {
        return;
    };
    for parent in def.extends.iter().rev() {
        collect_interface_post_order(parent.as_str(), ifaces, visited, out);
    }
    out.push((*envelope, *def));
}

impl PinnedType {
    /// Object-type lineage followed by implemented interfaces in child-first
    /// topological order, each pinned to its visible artifact in `view`.
    pub(super) fn transition_lineage(
        admitted: AdmittedType<'_, '_>,
        view: &ScopedView<'_>,
    ) -> Vec<Self> {
        let mut out = Self::lineage(admitted, view.model());
        if !matches!(
            view.catalog().object_type(&admitted.artifact().artifact_id),
            Some(Ok(_))
        ) {
            return out;
        }
        let layer = view.layer();
        let pool = view.pool();
        let mut objects: std::collections::BTreeMap<(ArtifactKey, &str), &ObjectTypeDef> =
            std::collections::BTreeMap::new();
        for (def, &owner) in layer.object_types.iter().zip(&layer.object_type_owners) {
            if let Some(envelope) = pool.get(owner) {
                objects
                    .entry((key(envelope), def.name.as_str()))
                    .or_insert(def);
            }
        }
        let mut ifaces: std::collections::BTreeMap<&str, InterfaceSlot<'_>> =
            std::collections::BTreeMap::new();
        for (def, &owner) in layer.interfaces.iter().zip(&layer.interface_owners) {
            if let Some(envelope) = pool.get(owner) {
                ifaces
                    .entry(def.name.as_str())
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some((envelope, def)));
            }
        }
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        let mut post_order: Vec<(&ArtifactEnvelope, &norn_ir::InterfaceDef)> = Vec::new();
        for pinned in out.iter().rev() {
            let Some(obj_def) = objects.get(&(pinned.artifact.clone(), pinned.name.as_str()))
            else {
                continue;
            };
            for iface_name in obj_def.implements.iter().rev() {
                collect_interface_post_order(
                    iface_name.as_str(),
                    &ifaces,
                    &mut visited,
                    &mut post_order,
                );
            }
        }
        for (envelope, def) in post_order.into_iter().rev() {
            out.push(Self {
                artifact: key(envelope),
                name: def.name.as_str().to_owned(),
            });
        }
        out
    }
}

impl Rows {
    pub(super) fn interface_effective_states(
        &self,
        def: &norn_ir::InterfaceDef,
        property: &norn_ir::PropertyId,
        view: &ScopedView<'_>,
    ) -> Option<Vec<String>> {
        let mut by_name: std::collections::BTreeMap<&str, &norn_ir::InterfaceDef> =
            std::collections::BTreeMap::new();
        for iface in &view.layer().interfaces {
            by_name.entry(iface.name.as_str()).or_insert(iface);
        }
        let owners = def.contract_owners(|name| by_name.get(name.as_str()).copied());
        let mut enum_lists: Vec<Vec<String>> = Vec::new();
        let mut own_list: Option<Vec<String>> = None;
        for owner in owners {
            if let Some(member) = owner.properties.iter().find(|p| p.id == *property) {
                let PropertyType::Enum { values } = crate::property_types::resolved_property_type(
                    &member.property_type,
                    &self.property_types,
                )?
                else {
                    return None;
                };
                let list: Vec<String> = values.iter().map(|v| v.value.clone()).collect();
                if owner.name == def.name {
                    own_list = Some(list.clone());
                }
                enum_lists.push(list);
            }
        }
        let base = own_list.or_else(|| enum_lists.first().cloned())?;
        Some(
            base.into_iter()
                .filter(|state| enum_lists.iter().all(|list| list.contains(state)))
                .collect(),
        )
    }

    pub(super) fn interface(
        &mut self,
        artifact: &ArtifactEnvelope,
        def: &norn_ir::InterfaceDef,
        view: &ScopedView<'_>,
    ) {
        let name = def.name.as_str().to_owned();
        self.types.insert(TypeRow {
            at: at(artifact, type_member(name.clone())),
            kind: TypeKind::Interface,
            role: self
                .roles
                .get(&key(artifact))
                .and_then(|roles| roles.get(&name))
                .copied()
                .unwrap_or(norn_ir::ConceptRole::Thing),
            extends: def.extends.iter().map(|n| n.as_str().to_owned()).collect(),
            implements: Vec::new(),
        });
        for property in &def.properties {
            let lifecycle = def
                .lifecycles
                .iter()
                .find(|lc| lc.property == property.id)
                .and_then(|lc| {
                    self.interface_effective_states(def, &property.id, view)
                        .map(|states| LifecycleStates {
                            states,
                            initial: lc.initial.clone(),
                            terminal: lc.terminal.clone(),
                        })
                });
            self.properties.insert(PropertyRow {
                at: at(artifact, property_member(&name, property.id.as_str())),
                computed: false,
                lifecycle,
            });
        }
        for link in &def.links {
            self.link(artifact, &name, link.id.as_str(), link.target.as_str());
        }
        for lifecycle in &def.lifecycles {
            if let Some(states) = self.interface_effective_states(def, &lifecycle.property, view) {
                self.states.extend(states.iter().map(|state| {
                    at(
                        artifact,
                        Member::State {
                            type_name: name.clone(),
                            lifecycle: lifecycle.property.as_str().to_owned(),
                            state: state.clone(),
                        },
                    )
                }));
            }
            for transition in &lifecycle.transitions {
                self.transitions.insert(TransitionRow {
                    guards: transition.guards.clone(),
                    at: at(
                        artifact,
                        Member::Transition {
                            type_name: name.clone(),
                            lifecycle: lifecycle.property.as_str().to_owned(),
                            transition: transition.id.as_str().to_owned(),
                        },
                    ),
                    from: transition.from.clone(),
                    to: transition.to.clone(),
                });
            }
        }
    }
}
