//! Interface parent inheritance and `disjointWith` checks
//! (`INTERFACE-DISJOINT-1`).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::identifier_limits::MAX_OBJECT_TYPE_NAME_BYTES;
use norn_ir::{InterfaceDef, InterfaceName, ObjectTypeDef, PropertyId};

use super::{Checker, Narrowing, narrowing, requirement_conflict};
use crate::interfaces;
use crate::property_types::{error, span_at};

impl<'a> Checker<'a, '_> {
    pub(super) fn check_interface_parents(&mut self, base: &[&str], def: &'a InterfaceDef) {
        if def.extends.is_empty() {
            return;
        }
        let mut seen = BTreeSet::new();
        for (index, parent) in def.extends.iter().enumerate() {
            if !seen.insert(parent) {
                self.out.push(error(
                    interfaces::codes::INVALID_INHERITANCE,
                    format!("interface parent {:?} is listed twice", parent.as_str()),
                    span_at(base, &["extends", &index.to_string()]),
                ));
            }
        }
        if let Err(name) = norn_expr::env::interface_lineage(def, |name| {
            self.interfaces.get(name).map(|entry| entry.def)
        }) {
            self.out.push(error(
                interfaces::codes::INVALID_INHERITANCE,
                format!(
                    "interface ancestry contains missing or cyclic parent {:?}",
                    name.as_str()
                ),
                span_at(base, &["extends"]),
            ));
            return;
        }
        let requirements = self.requirements(def);
        let group: Vec<_> = requirements
            .iter()
            .map(|requirement| (Some(0), requirement))
            .collect();
        let mut groups: BTreeMap<&PropertyId, Vec<_>> = BTreeMap::new();
        for requirement in group {
            groups
                .entry(&requirement.1.property.id)
                .or_default()
                .push(requirement);
        }
        for group in groups.values() {
            let conflict = requirement_conflict(group);
            let widens = group.iter().any(|(_, child)| {
                child.interface == &def.name
                    && group.iter().any(|(_, parent)| {
                        parent.interface != &def.name
                            && matches!(
                                narrowing(&child.shape(), &parent.shape()),
                                Narrowing::TypeMismatch
                                    | Narrowing::AdditivityNotRefined
                                    | Narrowing::WidensNullability
                            )
                    })
            });
            if conflict.is_some() || widens {
                self.out.push(error(
                    interfaces::codes::INVALID_INHERITANCE,
                    conflict.unwrap_or_else(|| {
                        "interface redeclaration widens a parent member".to_owned()
                    }),
                    span_at(base, &["extends"]),
                ));
            }
        }
        self.check_interface_parent_links(base, def);
    }

    /// INTERFACE-DISJOINT-1: validate `disjointWith` declarations and ensure no
    /// interface inherits two mutually disjoint interfaces.
    pub(super) fn check_interface_disjoint(&mut self, base: &[&str], def: &'a InterfaceDef) {
        let lineage = self.interface_lineage(def);
        let lineage_names: BTreeSet<&InterfaceName> =
            lineage.iter().map(|iface| &iface.name).collect();
        let mut seen = BTreeSet::new();
        let mut reported_pairs: BTreeSet<(&InterfaceName, &InterfaceName)> = BTreeSet::new();
        for (index, target) in def.disjoint_with.iter().enumerate() {
            let position = index.to_string();
            let at = span_at(base, &["disjointWith", position.as_str()]);
            if target.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES {
                self.out.push(error(
                    crate::codes::IDENTIFIER_TOO_LONG,
                    format!(
                        "disjointWith reference is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                        target.as_str().len()
                    ),
                    at,
                ));
                continue;
            }
            if !seen.insert(target) {
                self.out.push(error(
                    interfaces::codes::DISJOINT_VIOLATION,
                    format!(
                        "disjointWith interface {:?} is listed twice",
                        target.as_str()
                    ),
                    at,
                ));
                continue;
            }
            if target == &def.name {
                self.out.push(error(
                    interfaces::codes::DISJOINT_VIOLATION,
                    format!(
                        "interface {:?} cannot be disjoint with itself",
                        def.name.as_str()
                    ),
                    at,
                ));
                continue;
            }
            if !self.interfaces.contains_key(target) {
                self.out.push(error(
                    interfaces::codes::DISJOINT_VIOLATION,
                    format!("disjointWith {:?} names no interface", target.as_str()),
                    at,
                ));
                continue;
            }
            if lineage_names.contains(target) {
                let pair = if &def.name <= target {
                    (&def.name, target)
                } else {
                    (target, &def.name)
                };
                reported_pairs.insert(pair);
                self.out.push(error(
                    interfaces::codes::DISJOINT_VIOLATION,
                    format!(
                        "interface {:?} cannot be disjoint with its own ancestor {:?}",
                        def.name.as_str(),
                        target.as_str()
                    ),
                    at,
                ));
            }
        }
        if !def.extends.is_empty() {
            for parent_name in &def.extends {
                if let Some(parent) = self.interfaces.get(parent_name).map(|entry| entry.def) {
                    let parent_lineage = self.interface_lineage(parent);
                    Self::collect_disjoint_pairs(&parent_lineage, &mut reported_pairs);
                }
            }
            self.report_disjoint_pairs(base, &["extends"], &lineage, &mut reported_pairs);
        }
    }

    fn collect_disjoint_pairs(
        effective: &[&'a InterfaceDef],
        pairs: &mut BTreeSet<(&'a InterfaceName, &'a InterfaceName)>,
    ) {
        let names: BTreeSet<&InterfaceName> = effective.iter().map(|iface| &iface.name).collect();
        for iface in effective {
            for target in &iface.disjoint_with {
                if target == &iface.name || !names.contains(target) {
                    continue;
                }
                let pair = if &iface.name <= target {
                    (&iface.name, target)
                } else {
                    (target, &iface.name)
                };
                pairs.insert(pair);
            }
        }
    }

    fn report_disjoint_pairs(
        &mut self,
        base: &[&str],
        path: &[&str],
        effective: &[&'a InterfaceDef],
        reported: &mut BTreeSet<(&'a InterfaceName, &'a InterfaceName)>,
    ) {
        let names: BTreeSet<&InterfaceName> = effective.iter().map(|iface| &iface.name).collect();
        for iface in effective {
            for target in &iface.disjoint_with {
                if target == &iface.name || !names.contains(target) {
                    continue;
                }
                let pair = if &iface.name <= target {
                    (&iface.name, target)
                } else {
                    (target, &iface.name)
                };
                if reported.insert(pair) {
                    self.out.push(error(
                        interfaces::codes::DISJOINT_VIOLATION,
                        format!(
                            "cannot inherit both {:?} and {:?}, which are declared disjoint",
                            pair.0.as_str(),
                            pair.1.as_str()
                        ),
                        span_at(base, path),
                    ));
                }
            }
        }
    }

    /// INTERFACE-DISJOINT-1: no object type may inherit two mutually disjoint
    /// interfaces directly, through interface parents, or through supertypes.
    pub(super) fn check_object_disjoint(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        direct: &[(usize, &'a InterfaceDef)],
        ancestor_interfaces: &BTreeSet<&'a InterfaceName>,
    ) {
        let mut reported = BTreeSet::new();
        for (_, interface) in direct {
            let lineage = self.interface_lineage(interface);
            Self::collect_disjoint_pairs(&lineage, &mut reported);
        }
        if !ancestor_interfaces.is_empty() {
            let mut ancestor_seen: BTreeSet<&InterfaceName> = BTreeSet::new();
            let mut ancestor_effective: Vec<&'a InterfaceDef> = Vec::new();
            for iface in ancestor_interfaces
                .iter()
                .filter_map(|name| self.interfaces.get(*name).map(|entry| entry.def))
                .flat_map(|interface| self.interface_lineage(interface))
            {
                if ancestor_seen.insert(&iface.name) {
                    ancestor_effective.push(iface);
                }
            }
            Self::collect_disjoint_pairs(&ancestor_effective, &mut reported);
        }
        let mut seen: BTreeSet<&InterfaceName> = BTreeSet::new();
        let mut effective: Vec<&'a InterfaceDef> = Vec::new();
        for iface in direct
            .iter()
            .map(|(_, interface)| *interface)
            .chain(
                ancestor_interfaces
                    .iter()
                    .filter_map(|name| self.interfaces.get(*name).map(|entry| entry.def)),
            )
            .flat_map(|interface| self.interface_lineage(interface))
        {
            if seen.insert(&iface.name) {
                effective.push(iface);
            }
        }
        if !effective.is_empty() {
            let path: &[&str] = if def.implements.is_empty() {
                &["extends"]
            } else {
                &["implements"]
            };
            self.report_disjoint_pairs(base, path, &effective, &mut reported);
        }
    }
}
