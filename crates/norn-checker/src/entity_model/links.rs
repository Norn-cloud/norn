//! Links and invariants of an `EntityIR` v2 model (T1-ONT-1d, #2514).
//!
//! - **One member name space.** A type's properties, its links, and the
//!   inverse roles of links that target it (directly, through a supertype,
//!   or through an interface it implements) share one name space, so a role
//!   name that `AccessIR` `rel(...)` or a traversal starts from resolves to
//!   exactly one member. A collision is `member_id_conflict`, or
//!   `extension_shadows_member` for an extension member, reported on the
//!   member that arrives second in the type's table.
//! - **Links.** A link's target is an object type or an interface in scope.
//!   Redeclaring an inherited link is an override that only narrows: a
//!   target that is the inherited one, a subtype of it, or a type
//!   implementing it; a cardinality with `many` narrowed to `one` and
//!   optional narrowed to required. The inverse role and link properties
//!   stay the inherited link's.
//! - **Interfaces.** A required link is met by a link of the type (declared,
//!   inherited, or overridden) whose target and cardinality narrow the
//!   requirement's. Several requirements for one id must be jointly
//!   satisfiable: no property requirement shares the id, and no two targets
//!   are object types unrelated by inheritance.
//! - **Namespaces.** Link ids, inverse roles, and invariant ids follow the
//!   member rule of T1-ONT-1c: `ns__name` when a tenant type or an extension
//!   adds them, never containing `__` otherwise. An inverse role is a member
//!   of its target, so a link declared without a namespace may not name one
//!   on a target another layer owns (`foreign_inverse`).
//! - **Invariants.** Object invariants are unique across the effective
//!   contract, including supertype and implemented-interface invariants;
//!   interface invariants are unique on their interface. Invariants cannot
//!   be removed or replaced. Their expressions must parse and type-check as
//!   bool against the object or interface `self` record.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use norn_ir::identifier_limits::{MAX_LINK_NAME_BYTES, MAX_OBJECT_TYPE_NAME_BYTES};
use norn_ir::{
    ExtensionNamespace, InterfaceDef, InterfaceLink, InterfaceName, Invariant, Link,
    LinkCardinality, Multiplicity, ObjectTypeDef, ObjectTypeExtension, ObjectTypeName, Property,
};

use super::Checker;
use crate::Diagnostic;
use crate::entity_instance::{EffectiveLink, EffectiveProperty};
use crate::object_types::{
    MEMBER_ID_GRAMMAR, Origin, Resolved, check_property_default, check_property_shape,
    check_zen_syntax, codes,
};
use crate::property_types::{error, matches_grammar, span_at};

/// One link declaration: the link, the type it belongs to (its declaring
/// type, or the target of the extension that adds it), where it is
/// declared, and its position in that declaration's `links`.
#[derive(Clone, Copy)]
pub(super) struct LinkDecl<'a> {
    link: &'a Link,
    source: &'a ObjectTypeName,
    origin: Origin<'a>,
    position: usize,
}

/// What a member of a type's name space is.
#[derive(Clone, Copy)]
enum MemberKind<'a> {
    Property(&'a Property),
    /// A declared link, or the override that narrows it.
    Link(LinkDecl<'a>),
    /// The inverse role of a link that targets the type.
    Inverse(LinkDecl<'a>),
}

/// One member of a type's name space and where it is declared.
#[derive(Clone, Copy)]
pub(super) struct Member<'a> {
    kind: MemberKind<'a>,
    /// Where the member is declared (for an inverse role: its link's
    /// declaration).
    origin: Origin<'a>,
    /// Position in the declaration's `properties` or `links`.
    position: usize,
}

impl<'a> Member<'a> {
    fn property(property: &'a Property, origin: Origin<'a>, position: usize) -> Self {
        Self {
            kind: MemberKind::Property(property),
            origin,
            position,
        }
    }

    fn link(decl: LinkDecl<'a>) -> Self {
        Self {
            kind: MemberKind::Link(decl),
            origin: decl.origin,
            position: decl.position,
        }
    }

    fn inverse(decl: LinkDecl<'a>) -> Self {
        Self {
            kind: MemberKind::Inverse(decl),
            origin: decl.origin,
            position: decl.position,
        }
    }

    fn id(&self) -> &'a str {
        match self.kind {
            MemberKind::Property(property) => property.id.as_str(),
            MemberKind::Link(decl) => decl.link.id.as_str(),
            MemberKind::Inverse(decl) => decl.link.inverse.as_ref().map_or("", |id| id.as_str()),
        }
    }

    /// `(list, field)` of the member's id within its declaration.
    const fn field(&self) -> (&'static str, &'static str) {
        match self.kind {
            MemberKind::Property(_) => ("properties", "id"),
            MemberKind::Link(_) => ("links", "id"),
            MemberKind::Inverse(_) => ("links", "inverse"),
        }
    }

    fn describe(&self) -> String {
        match self.kind {
            MemberKind::Property(property) => format!("property {:?}", property.id.as_str()),
            MemberKind::Link(decl) => format!("link {:?}", decl.link.id.as_str()),
            MemberKind::Inverse(decl) => format!(
                "inverse role {:?} of link {:?} on {:?}",
                self.id(),
                decl.link.id.as_str(),
                decl.source.as_str()
            ),
        }
    }
}

/// A type's effective member name space: properties, links, and inverse
/// roles, declared, inherited, and added by extensions.
pub(super) type LinkTable<'a> = BTreeMap<&'a str, Member<'a>>;

/// One required link of an interface a type is held to, with the position of
/// the `implements` entry that lists the interface directly.
type LinkRequirement<'a> = (Option<usize>, &'a InterfaceDef, &'a InterfaceLink);

/// `sub` narrows `sup`: no end widens from `one` to `many`, and a required
/// link stays required.
const fn cardinality_narrows(sub: LinkCardinality, sup: LinkCardinality) -> bool {
    end_narrows(sub.to, sup.to)
        && end_narrows(sub.from, sup.from)
        && (sub.required || !sup.required)
}

/// One end narrows: `many` admits anything, `one` only `one`.
const fn end_narrows(sub: Multiplicity, sup: Multiplicity) -> bool {
    matches!(sup, Multiplicity::Many) || matches!(sub, Multiplicity::One)
}

impl<'a> Checker<'a, '_> {
    /// Effective forward links of a registered type, including inherited
    /// inverse roles and properties of an overridden link.
    pub(super) fn effective_links(&mut self, name: &'a ObjectTypeName) -> Vec<EffectiveLink> {
        let table = self.link_table(name);
        let mut links = Vec::new();
        for member in table.values() {
            let MemberKind::Link(decl) = member.kind else {
                continue;
            };
            let (inverse, properties) = self.inherited_link_metadata(name, decl);
            let mut properties: Vec<EffectiveProperty> = properties
                .iter()
                .map(|property| {
                    let resolved = Resolved::new(
                        property,
                        self.context.catalog,
                        self.context.registry,
                        decl.origin,
                    );
                    EffectiveProperty::from_resolved(&resolved, resolved.markings.clone())
                })
                .collect();
            properties.sort_by(|left, right| left.id.cmp(&right.id));
            links.push(EffectiveLink {
                id: decl.link.id.clone(),
                target: decl.link.target.clone(),
                cardinality: decl.link.cardinality,
                acyclic: decl.link.acyclic,
                inverse,
                properties,
            });
        }
        links.sort_by(|left, right| left.id.cmp(&right.id));
        links
    }

    fn inherited_link_metadata(
        &mut self,
        name: &'a ObjectTypeName,
        decl: LinkDecl<'a>,
    ) -> (Option<norn_ir::LinkId>, Vec<norn_ir::Property>) {
        if decl.link.inverse.is_some() || !decl.link.properties.is_empty() {
            return (decl.link.inverse.clone(), decl.link.properties.clone());
        }
        let mut parent = self.parent(name);
        while let Some(owner) = parent {
            let inherited = self
                .link_table(owner)
                .get(decl.link.id.as_str())
                .and_then(|member| match member.kind {
                    MemberKind::Link(inherited) => Some(inherited),
                    MemberKind::Property(_) | MemberKind::Inverse(_) => None,
                });
            if let Some(inherited) = inherited
                && (inherited.link.inverse.is_some() || !inherited.link.properties.is_empty())
            {
                return (
                    inherited.link.inverse.clone(),
                    inherited.link.properties.clone(),
                );
            }
            parent = self.parent(owner);
        }
        (decl.link.inverse.clone(), decl.link.properties.clone())
    }

    // ----- scope ---------------------------------------------------------

    fn interface_named(&self, name: &ObjectTypeName) -> Option<&'a InterfaceDef> {
        self.interfaces
            .get(&InterfaceName(name.as_str().to_owned()))
            .map(|entry| entry.def)
    }

    /// `name` is an object type or an interface in scope.
    pub(super) fn definition_known(&self, name: &ObjectTypeName) -> bool {
        self.types.contains_key(name) || self.interface_named(name).is_some()
    }

    /// `name` is a base-layer definition (read-only to the checked layer).
    fn definition_foreign(&self, name: &ObjectTypeName) -> bool {
        self.types
            .get(name)
            .map(|entry| entry.local.is_none())
            .or_else(|| {
                self.interfaces
                    .get(&InterfaceName(name.as_str().to_owned()))
                    .map(|entry| entry.local.is_none())
            })
            .unwrap_or(false)
    }

    /// Every interface a registered type is held to: its own and its
    /// supertypes'.
    fn held_interfaces(&self, name: &ObjectTypeName) -> BTreeSet<&'a str> {
        std::iter::once(name)
            .filter_map(|name| self.types.get_key_value(name).map(|(name, _)| *name))
            .chain(self.ancestors(name))
            .filter_map(|name| self.types.get(name))
            .flat_map(|entry| entry.def.implements.iter().map(InterfaceName::as_str))
            .collect()
    }

    /// Whether every object `sub` admits, `sup` admits: the same definition,
    /// a subtype of an object type, or an object type held to an interface.
    /// `None` when either is unknown.
    fn target_narrows(&self, sub: &ObjectTypeName, sup: &ObjectTypeName) -> Option<bool> {
        if !self.definition_known(sub) || !self.definition_known(sup) {
            return None;
        }
        if sub == sup {
            return Some(true);
        }
        if !self.types.contains_key(sub) {
            return Some(false); // interfaces are flat
        }
        if self.types.contains_key(sup) {
            return Some(self.ancestors(sub).contains(&sup));
        }
        Some(self.held_interfaces(sub).contains(sup.as_str()))
    }

    /// One type could narrow both targets: one narrows the other, or either
    /// is an interface (a subtype may implement it). Two object types
    /// unrelated by inheritance never can.
    fn targets_jointly_satisfiable(&self, first: &ObjectTypeName, second: &ObjectTypeName) -> bool {
        self.target_narrows(first, second) != Some(false)
            || self.target_narrows(second, first) != Some(false)
            || self.interface_named(first).is_some()
            || self.interface_named(second).is_some()
    }

    /// Link ids the supertypes of `name` declare, or that extensions add to
    /// them: a type's link with one of these ids is an override.
    fn inherited_link_ids(&self, name: &ObjectTypeName) -> BTreeSet<&'a str> {
        let mut ids = BTreeSet::new();
        for ancestor in self.ancestors(name) {
            if let Some(entry) = self.types.get(ancestor) {
                ids.extend(entry.def.links.iter().map(|link| link.id.as_str()));
            }
            for (_, _, extension) in self.extensions.get(ancestor).into_iter().flatten() {
                ids.extend(extension.links.iter().map(|link| link.id.as_str()));
            }
        }
        ids
    }

    // ----- registration ----------------------------------------------------

    /// Index every link that names an inverse role by its target, base layer
    /// first. Overrides name none: their inverse role is the inherited
    /// link's.
    pub(super) fn register_inverses(&mut self) {
        let mut decls = Vec::new();
        let registered: Vec<(&'a ObjectTypeName, &'a ObjectTypeDef, bool)> = self
            .types
            .iter()
            .map(|(name, entry)| (*name, entry.def, entry.local.is_some()))
            .collect();
        for local in [false, true] {
            for &(name, def, _) in registered.iter().filter(|(_, _, is)| *is == local) {
                let inherited = self.inherited_link_ids(name);
                for (position, link) in def.links.iter().enumerate() {
                    if !inherited.contains(link.id.as_str()) {
                        decls.push(LinkDecl {
                            link,
                            source: name,
                            origin: Origin::Type(name),
                            position,
                        });
                    }
                }
            }
            let extensions = if local {
                self.model.extensions
            } else {
                self.context.base.extensions
            };
            for (index, extension) in extensions.iter().enumerate() {
                let Some((source, _)) = self.types.get_key_value(&extension.target) else {
                    continue;
                };
                for (position, link) in extension.links.iter().enumerate() {
                    decls.push(LinkDecl {
                        link,
                        source,
                        origin: Origin::Extension { index, local },
                        position,
                    });
                }
            }
        }
        for decl in decls {
            if decl.link.inverse.is_some() {
                self.incoming
                    .entry(decl.link.target.as_str())
                    .or_default()
                    .push(decl);
            }
        }
    }

    // ----- member tables -----------------------------------------------------

    /// The effective member table of a registered type (memoized). Building
    /// it reports every member conflict it contains.
    fn link_table(&mut self, name: &'a ObjectTypeName) -> Rc<LinkTable<'a>> {
        if let Some(table) = self.link_tables.get(name) {
            return Rc::clone(table);
        }
        let Some(entry) = self.types.get(name) else {
            return Rc::default();
        };
        let def = entry.def;
        let mut table = self
            .parent(name)
            .map_or_else(LinkTable::new, |parent| (*self.link_table(parent)).clone());

        let mut arrivals: Vec<Member<'a>> = Vec::new();
        let own = Origin::Type(name);
        for (position, property) in def.properties.iter().enumerate() {
            arrivals.push(Member::property(property, own, position));
        }
        for (position, link) in def.links.iter().enumerate() {
            arrivals.push(Member::link(LinkDecl {
                link,
                source: name,
                origin: own,
                position,
            }));
        }
        // Inverse roles that target the type, or an interface it lists and
        // no supertype already implements.
        let inherited: BTreeSet<&str> = self
            .parent(name)
            .map_or_else(BTreeSet::new, |parent| self.held_interfaces(parent));
        #[expect(
            clippy::needless_collect,
            reason = "BTreeSet sorts and deduplicates inherited interface targets, preserving inverse-member order"
        )]
        let targets = std::iter::once(name.as_str()).chain(
            def.implements
                .iter()
                .map(InterfaceName::as_str)
                .filter(|interface| !inherited.contains(interface))
                .collect::<BTreeSet<_>>(),
        );
        for target in targets {
            for decl in self.incoming.get(target).into_iter().flatten() {
                arrivals.push(Member::inverse(*decl));
            }
        }
        for (index, local, extension) in self.extensions.get(name).cloned().unwrap_or_default() {
            let origin = Origin::Extension { index, local };
            for (position, property) in extension.properties.iter().enumerate() {
                arrivals.push(Member::property(property, origin, position));
            }
            for (position, link) in extension.links.iter().enumerate() {
                arrivals.push(Member::link(LinkDecl {
                    link,
                    source: name,
                    origin,
                    position,
                }));
            }
        }

        for member in arrivals {
            self.arrive(&mut table, member, name);
        }
        let table = Rc::new(table);
        self.link_tables.insert(name, Rc::clone(&table));
        table
    }

    /// Add one member to a table: an override replaces what it narrows, and
    /// a collision is reported.
    fn arrive(&mut self, table: &mut LinkTable<'a>, member: Member<'a>, name: &'a ObjectTypeName) {
        let id = member.id();
        let Some(existing) = table.get(id).copied() else {
            table.insert(id, member);
            return;
        };
        let own = Origin::Type(name);
        match (existing.kind, member.kind) {
            // The same inverse role, reached twice.
            (MemberKind::Inverse(first), MemberKind::Inverse(second))
                if std::ptr::eq(first.link, second.link) => {}
            // Declared twice in one declaration: `duplicate_*` there.
            (
                MemberKind::Property(_) | MemberKind::Link(_),
                MemberKind::Property(_) | MemberKind::Link(_),
            ) if existing.origin == member.origin => {}
            // Property against property is T1-ONT-1c's (overrides and
            // `extension_shadows_member`).
            (MemberKind::Property(_), MemberKind::Property(_)) => {
                if member.origin == own {
                    table.insert(id, member);
                }
            }
            // A type's own link narrowing an inherited one.
            (MemberKind::Link(_), MemberKind::Link(_)) if member.origin == own => {
                table.insert(id, member);
            }
            _ => self.report_conflict(&existing, &member, name),
        }
    }

    /// The JSON pointer of a member's id in the checked model; `None` for a
    /// base-layer member.
    fn member_pointer(&self, member: &Member<'a>) -> Option<Vec<String>> {
        let (root, index) = match member.origin {
            Origin::Type(owner) => (self.roots.object_types, self.types.get(owner)?.local?),
            Origin::Extension { index, local: true } => (self.roots.extensions, index),
            Origin::Extension { local: false, .. } => return None,
        };
        let (list, field) = member.field();
        let mut at: Vec<String> = root.iter().map(|segment| (*segment).to_owned()).collect();
        at.extend([
            index.to_string(),
            list.to_owned(),
            member.position.to_string(),
            field.to_owned(),
        ]);
        Some(at)
    }

    /// Report a collision on the member that arrived second, or, if that one
    /// is in the base layer, on the first.
    fn report_conflict(&mut self, first: &Member<'a>, second: &Member<'a>, name: &ObjectTypeName) {
        let (reported, other, at) = match self.member_pointer(second) {
            Some(at) => (second, first, at),
            None => match self.member_pointer(first) {
                Some(at) => (first, second, at),
                None => return,
            },
        };
        let from_extension = matches!(reported.origin, Origin::Extension { .. })
            && !matches!(reported.kind, MemberKind::Inverse(_));
        let (code, message) = if from_extension {
            let owner = match other.kind {
                MemberKind::Inverse(decl) => format!(
                    "the inverse role of link {:?} on {:?}",
                    decl.link.id.as_str(),
                    decl.source.as_str()
                ),
                MemberKind::Property(_) | MemberKind::Link(_) => match other.origin {
                    Origin::Type(owner) => format!("object type {:?}", owner.as_str()),
                    Origin::Extension { .. } => "another extension".to_owned(),
                },
            };
            (
                codes::EXTENSION_SHADOWS_MEMBER,
                format!(
                    "extension {} shadows the member of the same id that {owner} declares on {:?}",
                    reported.describe(),
                    name.as_str()
                ),
            )
        } else {
            (
                codes::MEMBER_ID_CONFLICT,
                format!(
                    "{} takes the id of the {} on {:?}; properties, links, and inverse roles share one member name space",
                    reported.describe(),
                    other.describe(),
                    name.as_str()
                ),
            )
        };
        let pointer: Vec<&str> = at.iter().map(String::as_str).collect();
        self.report_once(error(code, message, span_at(&pointer, &[])));
    }

    fn report_once(&mut self, diagnostic: Diagnostic) {
        let pointer = diagnostic
            .span
            .as_ref()
            .map(|span| span.pointer.clone())
            .unwrap_or_default();
        let code = diagnostic.code.as_str().to_owned();
        if self.reported.insert((code, pointer)) {
            self.out.push(diagnostic);
        }
    }

    /// Build every registered type's member table, reporting its conflicts.
    pub(super) fn check_member_conflicts(&mut self) {
        let names: Vec<&'a ObjectTypeName> = self.types.keys().copied().collect();
        for name in names {
            self.link_table(name);
        }
    }

    // ----- declarations ------------------------------------------------------

    /// The links an object type declares: ids, targets, inverse roles, link
    /// properties, namespaces, and overrides.
    pub(super) fn check_type_links(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        registered: bool,
        parent: Option<&'a ObjectTypeName>,
    ) {
        let inherited = parent.map_or_else(|| Rc::new(LinkTable::new()), |p| self.link_table(p));
        let mut ids: BTreeSet<&str> = def
            .properties
            .iter()
            .map(|property| property.id.as_str())
            .collect();
        let tenant = self.context.tenant;
        for (position, link) in def.links.iter().enumerate() {
            let position = position.to_string();
            let at = [base, &["links", position.as_str()]].concat();
            let fresh = self.check_link_shape(&at, link, &mut ids, Origin::Type(&def.name));
            match inherited.get(link.id.as_str()).map(|member| member.kind) {
                Some(MemberKind::Link(overridden)) => {
                    if registered {
                        self.check_link_override(&at, link, &overridden);
                    }
                }
                None if fresh => {
                    self.check_member_namespace(&at, "id", "link", link.id.as_str(), tenant);
                    self.check_inverse(&at, link, tenant);
                }
                // A collision with an inherited property or inverse role is
                // reported by the member table; a duplicate id where it is
                // declared.
                Some(MemberKind::Property(_) | MemberKind::Inverse(_)) | None => {}
            }
        }
    }

    /// The links an extension adds.
    pub(super) fn check_extension_links(
        &mut self,
        base: &[&str],
        index: usize,
        extension: &'a ObjectTypeExtension,
        valid_namespace: bool,
    ) {
        let mut ids: BTreeSet<&str> = extension
            .properties
            .iter()
            .map(|property| property.id.as_str())
            .collect();
        let namespace = &extension.namespace;
        for (position, link) in extension.links.iter().enumerate() {
            let position = position.to_string();
            let at = [base, &["links", position.as_str()]].concat();
            let origin = Origin::Extension { index, local: true };
            let fresh = self.check_link_shape(&at, link, &mut ids, origin);
            if fresh && valid_namespace {
                self.check_member_namespace(&at, "id", "link", link.id.as_str(), Some(namespace));
                self.check_inverse(&at, link, Some(namespace));
            }
        }
    }

    /// Id and inverse grammar, duplicates within the declaration, the target,
    /// and link properties. Returns whether the id is new to the
    /// declaration.
    fn check_link_shape(
        &mut self,
        at: &[&str],
        link: &'a Link,
        ids: &mut BTreeSet<&'a str>,
        origin: Origin<'a>,
    ) -> bool {
        let id = link.id.as_str();
        if !matches_grammar(&MEMBER_ID_GRAMMAR, id) {
            self.out.push(error(
                codes::INVALID_LINK_ID,
                format!("link id {id:?} must match [a-z][a-z0-9_]*"),
                span_at(at, &["id"]),
            ));
        }
        if id.len() > MAX_LINK_NAME_BYTES {
            self.out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "link id is {} UTF-8 bytes; the admission limit is {MAX_LINK_NAME_BYTES}. Rename it before installing updated definitions",
                    id.len()
                ),
                span_at(at, &["id"]),
            ));
        }
        if link.target.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES {
            self.out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "link target name is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                    link.target.as_str().len()
                ),
                span_at(at, &["target"]),
            ));
        }
        let fresh = ids.insert(id);
        if !fresh {
            self.out.push(error(
                codes::DUPLICATE_LINK_ID,
                format!(
                    "link id {id:?} is declared twice, as a link or a property, in one declaration"
                ),
                span_at(at, &["id"]),
            ));
        }
        if !self.definition_known(&link.target) {
            self.out.push(error(
                codes::UNKNOWN_LINK_TARGET,
                format!(
                    "link target {:?} names no object type or interface",
                    link.target.as_str()
                ),
                span_at(at, &["target"]),
            ));
        }
        if let Some(inverse) = &link.inverse {
            if !matches_grammar(&MEMBER_ID_GRAMMAR, inverse.as_str()) {
                self.out.push(error(
                    codes::INVALID_LINK_ID,
                    format!(
                        "inverse role {:?} must match [a-z][a-z0-9_]*",
                        inverse.as_str()
                    ),
                    span_at(at, &["inverse"]),
                ));
            }
            if inverse.as_str().len() > MAX_LINK_NAME_BYTES {
                self.out.push(error(
                    crate::codes::IDENTIFIER_TOO_LONG,
                    format!(
                        "inverse role is {} UTF-8 bytes; the admission limit is {MAX_LINK_NAME_BYTES}. Rename it before installing updated definitions",
                        inverse.as_str().len()
                    ),
                    span_at(at, &["inverse"]),
                ));
            }
        }
        self.check_link_properties(at, link, origin);
        fresh
    }

    /// Link properties are checked like object type properties; their ids
    /// are local to the link and never contain `__`.
    fn check_link_properties(&mut self, at: &[&str], link: &'a Link, origin: Origin<'a>) {
        let (catalog, registry) = (self.context.catalog, self.context.registry);
        let mut ids = BTreeSet::new();
        for (position, property) in link.properties.iter().enumerate() {
            let position = position.to_string();
            let at = [at, &["properties", position.as_str()]].concat();
            check_property_shape(&at, property, catalog, registry, &mut self.out);
            if !ids.insert(&property.id) {
                self.out.push(error(
                    codes::DUPLICATE_PROPERTY_ID,
                    format!(
                        "link property id {:?} is declared twice",
                        property.id.as_str()
                    ),
                    span_at(&at, &["id"]),
                ));
                continue;
            }
            self.check_member_namespace(&at, "id", "link property", property.id.as_str(), None);
            let member = Resolved::new(property, catalog, registry, origin);
            if !member.markings.is_empty() {
                self.out.push(error(
                    codes::LINK_PROPERTY_MARKINGS_UNSUPPORTED,
                    "marked link properties require edge classification, which is not supported"
                        .to_owned(),
                    span_at(&at, &["markings"]),
                ));
            }
            check_property_default(&at, &member, &mut self.out);
        }
    }

    /// A new link's inverse role: namespaced like the link, never added to
    /// another layer's definition without a namespace, and distinct from the
    /// members an interface target requires.
    fn check_inverse(
        &mut self,
        at: &[&str],
        link: &'a Link,
        namespace: Option<&ExtensionNamespace>,
    ) {
        let Some(inverse) = &link.inverse else {
            return;
        };
        let id = inverse.as_str();
        if !matches_grammar(&MEMBER_ID_GRAMMAR, id) {
            return; // `invalid_link_id`
        }
        self.check_member_namespace(at, "inverse", "inverse role", id, namespace);
        if namespace.is_none() && self.definition_foreign(&link.target) {
            self.out.push(error(
                codes::FOREIGN_INVERSE,
                format!(
                    "inverse role {id:?} would add a member to {:?}, which another layer owns; declare the link in an extension so its inverse role is namespaced, or omit the inverse",
                    link.target.as_str()
                ),
                span_at(at, &["inverse"]),
            ));
        }
        if let Some(interface) = self.interface_named(&link.target) {
            let required = interface
                .properties
                .iter()
                .map(|property| (property.id.as_str(), "property"))
                .chain(
                    interface
                        .links
                        .iter()
                        .map(|link| (link.id.as_str(), "link")),
                )
                .find(|(required, _)| *required == id);
            if let Some((_, kind)) = required {
                let message = format!(
                    "inverse role {id:?} takes the id of the {kind} interface {:?} requires; properties, links, and inverse roles share one member name space",
                    interface.name.as_str()
                );
                self.report_once(error(
                    codes::MEMBER_ID_CONFLICT,
                    message,
                    span_at(at, &["inverse"]),
                ));
            }
        }
    }

    /// A link override only narrows the link it overrides, and inherits its
    /// inverse role and link properties.
    fn check_link_override(&mut self, at: &[&str], link: &'a Link, overridden: &LinkDecl<'a>) {
        let id = link.id.as_str();
        let owner = match overridden.origin {
            Origin::Type(owner) => format!("supertype {:?}", owner.as_str()),
            Origin::Extension { .. } => "an extension of a supertype".to_owned(),
        };
        for (member, present) in [
            ("inverse", link.inverse.is_some()),
            ("properties", !link.properties.is_empty()),
        ] {
            if present {
                self.out.push(error(
                    codes::LINK_OVERRIDE_REDECLARES,
                    format!(
                        "link {id:?} overrides the one {owner} declares and redeclares its {member}, which the override inherits; omit it"
                    ),
                    span_at(at, &[member]),
                ));
            }
        }
        let inherited = overridden.link;
        if self.target_narrows(&link.target, &inherited.target) == Some(false) {
            self.out.push(error(
                codes::LINK_OVERRIDE_TARGET_MISMATCH,
                format!(
                    "link {id:?} overrides the one {owner} declares with a target that is not {:?} or a narrowing of it; an override may only narrow",
                    inherited.target.as_str()
                ),
                span_at(at, &["target"]),
            ));
        }
        if inherited.acyclic != link.acyclic {
            self.out.push(error(
                codes::LINK_OVERRIDE_CHANGES_ACYCLIC,
                format!(
                    "link {id:?} overrides the one {owner} declares and changes its acyclic constraint; an override must preserve the inherited value (subtype-only acyclicity is unsound)"
                ),
                span_at(at, &["acyclic"]),
            ));
        }
        if !cardinality_narrows(link.cardinality, inherited.cardinality) {
            self.out.push(error(
                codes::LINK_OVERRIDE_WIDENS_CARDINALITY,
                format!(
                    "link {id:?} overrides the one {owner} declares with a wider cardinality; an override may only narrow (many to one, optional to required)"
                ),
                span_at(at, &["cardinality"]),
            ));
        }
    }

    // ----- interface link requirements -------------------------------------

    /// Required links of the interfaces a type lists: conflicts among every
    /// requirement the type is held to, then conformance of each.
    pub(super) fn check_link_requirements(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        registered: bool,
        ancestors: &[&'a ObjectTypeName],
        conflicted: &BTreeSet<&'a norn_ir::PropertyId>,
    ) {
        let held = self.held_with_positions(def, ancestors);
        let mut groups: BTreeMap<&'a str, Vec<LinkRequirement<'a>>> = BTreeMap::new();
        for (position, interface) in &held {
            let mut ids = BTreeSet::new();
            for requirement in &interface.links {
                if ids.insert(requirement.id.as_str()) {
                    groups.entry(requirement.id.as_str()).or_default().push((
                        *position,
                        interface,
                        requirement,
                    ));
                }
            }
        }
        let table = if registered {
            self.link_table(&def.name)
        } else {
            Rc::new(LinkTable::new())
        };

        for (id, group) in groups {
            let properties: Vec<(Option<usize>, &InterfaceDef)> = held
                .iter()
                .filter(|(_, interface)| {
                    interface
                        .properties
                        .iter()
                        .any(|property| property.id.as_str() == id)
                })
                .map(|(position, interface)| (*position, *interface))
                .collect();
            let Some(first) = group
                .iter()
                .map(|(position, _, _)| *position)
                .chain(properties.iter().map(|(position, _)| *position))
                .flatten()
                .min()
            else {
                continue; // the supertype's
            };
            if conflicted.iter().any(|property| property.as_str() == id) {
                continue;
            }
            if let Some(reason) = self.link_requirement_conflict(&group, &properties) {
                let names: Vec<&str> = group
                    .iter()
                    .map(|(_, interface, _)| interface.name.as_str())
                    .chain(
                        properties
                            .iter()
                            .map(|(_, interface)| interface.name.as_str()),
                    )
                    .collect();
                self.out.push(error(
                    codes::INTERFACE_REQUIREMENT_CONFLICT,
                    format!(
                        "interfaces {names:?} all require member {id:?}, but no single member can meet them together: {reason}"
                    ),
                    span_at(base, &["implements", first.to_string().as_str()]),
                ));
                continue;
            }
            if registered {
                for requirement in &group {
                    self.check_link_conformance(base, id, requirement, &table);
                }
            }
        }
    }

    /// Every interface a type is held to, with the position of the
    /// `implements` entry that lists it directly (`None` when only a
    /// supertype implements it).
    fn held_with_positions(
        &self,
        def: &'a ObjectTypeDef,
        ancestors: &[&'a ObjectTypeName],
    ) -> Vec<(Option<usize>, &'a InterfaceDef)> {
        let mut held: Vec<(Option<usize>, &'a InterfaceDef)> = Vec::new();
        let mut seen = BTreeSet::new();
        for (position, interface) in def.implements.iter().enumerate() {
            if let Some(entry) = self.interfaces.get(interface)
                && seen.insert(interface.as_str())
            {
                held.push((Some(position), entry.def));
            }
        }
        for ancestor in ancestors {
            let Some(entry) = self.types.get(ancestor) else {
                continue;
            };
            for interface in &entry.def.implements {
                if let Some(interface) = self.interfaces.get(interface)
                    && seen.insert(interface.def.name.as_str())
                {
                    held.push((None, interface.def));
                }
            }
        }

        held
    }

    /// Conformance of the type's link to one requirement of an interface it
    /// lists.
    fn check_link_conformance(
        &mut self,
        base: &[&str],
        id: &str,
        (position, interface, requirement): &LinkRequirement<'a>,
        table: &LinkTable<'a>,
    ) {
        let Some(position) = position else { return };
        let at = span_at(base, &["implements", position.to_string().as_str()]);
        let name = interface.name.as_str();
        let Some(MemberKind::Link(decl)) = table.get(id).map(|member| member.kind) else {
            let found = table.get(id).map_or_else(
                || "the type neither declares nor inherits it".to_owned(),
                |member| format!("the type's member of that id is the {}", member.describe()),
            );
            self.out.push(error(
                codes::MISSING_INTERFACE_LINK,
                format!("interface {name:?} requires link {id:?}, but {found}"),
                at,
            ));
            return;
        };
        if self.target_narrows(&decl.link.target, &requirement.target) == Some(false) {
            self.out.push(error(
                codes::INTERFACE_LINK_TARGET_MISMATCH,
                format!(
                    "link {id:?} does not target {:?}, which interface {name:?} requires, or a narrowing of it",
                    requirement.target.as_str()
                ),
                at.clone(),
            ));
        }
        if !cardinality_narrows(decl.link.cardinality, requirement.cardinality) {
            self.out.push(error(
                codes::INTERFACE_LINK_WIDENS_CARDINALITY,
                format!("link {id:?} has a wider cardinality than interface {name:?} requires"),
                at,
            ));
        }
    }

    /// Why the requirements for one link id cannot be met by one link: a
    /// property requirement shares the id, or two targets are object types
    /// unrelated by inheritance. Cardinalities always meet (the narrowest).
    fn link_requirement_conflict(
        &self,
        group: &[LinkRequirement<'a>],
        properties: &[(Option<usize>, &'a InterfaceDef)],
    ) -> Option<String> {
        if let (Some((_, link, _)), Some((_, property))) = (group.first(), properties.first()) {
            return Some(format!(
                "{:?} requires a link and {:?} a property",
                link.name.as_str(),
                property.name.as_str()
            ));
        }
        for (index, (_, first, first_link)) in group.iter().enumerate() {
            for (_, second, second_link) in group.iter().skip(index.saturating_add(1)) {
                if !self.targets_jointly_satisfiable(&first_link.target, &second_link.target) {
                    return Some(format!(
                        "the targets of {:?} ({:?}) and {:?} ({:?}) are object types that no one type narrows",
                        first.name.as_str(),
                        first_link.target.as_str(),
                        second.name.as_str(),
                        second_link.target.as_str()
                    ));
                }
            }
        }
        None
    }

    // ----- invariants --------------------------------------------------------

    /// Invariant ids (unique across the type's effective contract, including
    /// supertype and interface invariants), namespaces, and non-blank
    /// expressions and messages.
    pub(super) fn check_interface_invariants(&mut self, base: &[&str], def: &'a InterfaceDef) {
        self.check_invariant_list(
            base,
            &def.invariants,
            &BTreeSet::new(),
            None,
            InvariantSubject::Interface(&def.name),
        );
    }

    pub(super) fn check_invariants(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        ancestors: &[&'a ObjectTypeName],
    ) {
        let ancestor_interfaces: BTreeSet<&InterfaceName> = ancestors
            .iter()
            .filter_map(|ancestor| self.types.get(ancestor))
            .flat_map(|entry| entry.def.implements.iter())
            .collect();
        let mut inherited: BTreeSet<String> = ancestors
            .iter()
            .filter_map(|ancestor| self.types.get(ancestor))
            .flat_map(|entry| entry.def.invariants.iter())
            .map(|invariant| invariant.id.as_str().to_owned())
            .collect();
        for interface in &ancestor_interfaces {
            if let Some(entry) = self.interfaces.get(interface) {
                inherited.extend(
                    entry
                        .def
                        .invariants
                        .iter()
                        .map(|invariant| invariant.id.as_str().to_owned()),
                );
            }
        }

        // A newly implemented interface adds its invariant contract to the
        // type. IDs share the effective invariant namespace with object
        // invariants and with the other implemented interfaces.
        let mut effective_ids = inherited.clone();
        let mut declared_interfaces = BTreeSet::new();
        for (index, interface) in def.implements.iter().enumerate() {
            if ancestor_interfaces.contains(interface) || !declared_interfaces.insert(interface) {
                continue;
            }
            let Some(entry) = self.interfaces.get(interface) else {
                continue;
            };
            let mut local_ids = BTreeSet::new();
            for invariant in &entry.def.invariants {
                let id = invariant.id.as_str();
                if local_ids.insert(id) && !effective_ids.insert(id.to_owned()) {
                    let index = index.to_string();
                    let at = [base, &["implements", index.as_str()]].concat();
                    self.out.push(error(
                        codes::DUPLICATE_INVARIANT_ID,
                        format!(
                            "interface invariant id {id:?} conflicts with another invariant already required by this type"
                        ),
                        span_at(&at, &[]),
                    ));
                }
            }
        }

        self.check_invariant_list(
            base,
            &def.invariants,
            &effective_ids,
            self.context.tenant,
            InvariantSubject::Object(&def.name),
        );
    }

    fn check_invariant_list(
        &mut self,
        base: &[&str],
        invariants: &[Invariant],
        inherited: &BTreeSet<String>,
        tenant: Option<&ExtensionNamespace>,
        subject: InvariantSubject<'_>,
    ) {
        let mut ids = BTreeSet::new();
        for (position, invariant) in invariants.iter().enumerate() {
            let position = position.to_string();
            let at = [base, &["invariants", position.as_str()]].concat();
            let id = invariant.id.as_str();
            if !matches_grammar(&MEMBER_ID_GRAMMAR, id) {
                self.out.push(error(
                    codes::INVALID_INVARIANT_ID,
                    format!("invariant id {id:?} must match [a-z][a-z0-9_]*"),
                    span_at(&at, &["id"]),
                ));
            } else if !ids.insert(id) || inherited.contains(id) {
                self.out.push(error(
                    codes::DUPLICATE_INVARIANT_ID,
                    format!(
                        "invariant id {id:?} is already declared in this type's effective invariant contract; an invariant is never replaced"
                    ),
                    span_at(&at, &["id"]),
                ));
            } else {
                self.check_member_namespace(&at, "id", "invariant", id, tenant);
            }
            let expression_at = span_at(&at, &["expression"]);
            if invariant.expression.as_str().trim().is_empty() {
                self.out.push(error(
                    codes::EMPTY_INVARIANT_EXPRESSION,
                    format!("the expression of invariant {id:?} is blank"),
                    expression_at,
                ));
            } else {
                check_zen_syntax(
                    invariant.expression.as_str(),
                    codes::INVALID_INVARIANT_EXPRESSION,
                    &format!("the expression of invariant {id:?}"),
                    expression_at,
                    &mut self.out,
                );
                self.check_invariant_type(&at, invariant, subject);
            }
            if invariant.message.trim().is_empty() {
                self.out.push(error(
                    codes::EMPTY_INVARIANT_MESSAGE,
                    format!("the message of invariant {id:?} is blank"),
                    span_at(&at, &["message"]),
                ));
            }
        }
    }

    fn check_invariant_type(
        &mut self,
        at: &[&str],
        invariant: &Invariant,
        subject: InvariantSubject<'_>,
    ) {
        let Ok(expression) = norn_expr::Expression::compile(invariant.expression.as_str()) else {
            return;
        };
        // A per-artifact object check has no installed base layer. Its
        // missing supertype is already a model-level refusal, so defer this
        // invariant's member typing until the complete model is available.
        if let InvariantSubject::Object(name) = subject
            && self.types.get(name).is_some_and(|entry| {
                entry
                    .def
                    .extends
                    .as_ref()
                    .is_some_and(|parent| !self.types.contains_key(parent))
            })
        {
            return;
        }
        let (detail, cross_object_link) = self.invariant_expression_detail(&expression, subject);
        let Some(detail) = detail else {
            return;
        };
        let message = if cross_object_link {
            format!(
                "the expression of invariant {:?} reads a link or inverse role; {detail}",
                invariant.id.as_str()
            )
        } else {
            format!(
                "the expression of invariant {:?} is not a boolean ZEN Expression: {detail}",
                invariant.id.as_str()
            )
        };
        self.out.push(error(
            codes::INVALID_INVARIANT_EXPRESSION,
            message,
            span_at(at, &["expression"]),
        ));
    }

    fn invariant_expression_detail(
        &self,
        expression: &norn_expr::Expression,
        subject: InvariantSubject<'_>,
    ) -> (Option<String>, bool) {
        match self.expression_env.as_ref() {
            None => (
                Some("the ZEN type environment was not derived".to_owned()),
                false,
            ),
            Some(Err(errors)) => (
                Some(format!(
                    "the ZEN type environment could not be derived: {}",
                    errors.first().map_or_else(
                        || "unknown environment error".to_owned(),
                        ToString::to_string
                    )
                )),
                false,
            ),
            Some(Ok(type_env)) => {
                let bound = match subject {
                    InvariantSubject::Object(name) => type_env.clone().bind_self(name),
                    InvariantSubject::Interface(name) => type_env.clone().bind_interface_self(name),
                };
                let bound = match bound {
                    Ok(bound) => bound,
                    Err(error) => {
                        return (Some(format!("`self` could not be bound: {error}")), false);
                    }
                };
                if let Err(findings) =
                    norn_expr::check_expression(expression, &bound, &norn_expr::Type::Bool)
                {
                    return (
                        Some(findings.first().map_or_else(
                            || "expression failed type checking".to_owned(),
                            |finding| finding.message.clone(),
                        )),
                        false,
                    );
                }
                let type_env = match self.property_only_expression_env() {
                    Ok(type_env) => type_env,
                    Err(errors) => {
                        return (
                            Some(format!(
                                "the property-only ZEN type environment could not be derived: {}",
                                errors.first().map_or_else(
                                    || "unknown environment error".to_owned(),
                                    ToString::to_string
                                )
                            )),
                            false,
                        );
                    }
                };
                let property_only = match subject {
                    InvariantSubject::Object(name) => type_env.bind_self(name),
                    InvariantSubject::Interface(name) => type_env.bind_interface_self(name),
                };
                let property_only = match property_only {
                    Ok(property_only) => property_only,
                    Err(error) => {
                        return (
                            Some(format!("`self` could not be bound without links: {error}")),
                            false,
                        );
                    }
                };
                if norn_expr::check_expression(expression, &property_only, &norn_expr::Type::Bool)
                    .is_err()
                {
                    (
                        Some(
                            "cross-object constraints are not yet enforced; see epic #3121"
                                .to_owned(),
                        ),
                        true,
                    )
                } else {
                    (None, false)
                }
            }
        }
    }

    /// Derive the same invariant environment with every forward link removed.
    /// The expression AST is private to `norn-expr`, but the type environment
    /// retains field provenance. Checking against this property-only
    /// environment lets the existing type checker detect references to either
    /// forward links or inverse roles without inspecting expression text.
    fn property_only_expression_env(
        &self,
    ) -> Result<norn_expr::ZenTypeEnv, Vec<norn_expr::EnvError>> {
        let interfaces = self
            .context
            .base
            .interfaces
            .iter()
            .chain(self.model.interfaces)
            .cloned()
            .map(|mut definition| {
                definition.links.clear();
                definition
            })
            .collect::<Vec<_>>();
        let object_types = self
            .context
            .base
            .object_types
            .iter()
            .chain(self.model.object_types)
            .cloned()
            .map(|mut definition| {
                definition.links.clear();
                definition
            })
            .collect::<Vec<_>>();
        let extensions = self
            .context
            .base
            .extensions
            .iter()
            .chain(self.model.extensions)
            .cloned()
            .map(|mut extension| {
                extension.links.clear();
                extension
            })
            .collect::<Vec<_>>();
        let definitions = norn_expr::EntityDefinitions {
            property_types: self.context.catalog,
            interfaces: &interfaces,
            object_types: &object_types,
            extensions: &extensions,
        };
        norn_expr::ZenTypeEnv::from_artifact_set_for_checking(
            &definitions,
            self.context.registry,
            norn_expr::EnvOptions::default(),
        )
    }
}

#[derive(Clone, Copy)]
enum InvariantSubject<'a> {
    Object(&'a ObjectTypeName),
    Interface(&'a InterfaceName),
}
