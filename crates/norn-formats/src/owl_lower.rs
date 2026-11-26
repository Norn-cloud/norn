//! Lowering an OWL [`Model`] to Norn interfaces: one interface per owned
//! class, `extends` for named superclasses, a link per object property and a
//! property per datatype property where a closed-world type can carry it, and
//! one loss disposition per accounted [`Item`].
//!
//! # The closed-world reading
//!
//! - `SubClassOf(C D)`, both owned classes: `C extends D`. An edge that
//!   closes a cycle is an implied equivalence and is not lowered.
//! - `PropertyDomain(p D)`: an optional member `p` on `D` whose target is
//!   `p`'s named range.
//! - A restriction `SubClassOf(C R)` on `p` constrains the whole member `p`
//!   on `C` only when it speaks of every `p`-value: `only F` (every value is
//!   an `F`), or a count (`some`, `min n`, `max n`, `exactly n`) that is
//!   unqualified or whose filler covers `p`'s established named range. Such a
//!   member targets the range (or the `only` filler); `exactly 1`/`max 1` →
//!   to one. A bound above one is kept as many with a degradation (Norn link
//!   cardinality is one or many).
//! - A qualified count whose filler does not cover the range counts only the
//!   values that are `F`: OWL lets separate values satisfy two such
//!   restrictions (`hasArgument some Principal` and `hasArgument some
//!   AppraisedValue`), and `max 1 F` leaves other values free. Narrowing the
//!   link's target or capping it would be wrong, so it is deferred
//!   (`format.owl.filler_scoped`).
//! - `min 0` constrains nothing: mapped to its class, adding no member.
//! - Existence (`some`, `min n ≥ 1`, `exactly n ≥ 1`) is open-world: the
//!   member stays optional with a `format.owl.open_world` degradation,
//!   unless the import's witness policy is `closed`, which requires it. Under
//!   `closed`, a filler-scoped existence claim lowers only its existence (a
//!   required member targeting the range), degraded. A data member holds one
//!   value, so for a datatype property that holds only where the property is
//!   already bounded to one value (`FunctionalProperty`, or a `max 1` or
//!   `exactly 1` over every value on the class or an ancestor); otherwise it
//!   is deferred.
//! - A member's or interface's description carries its `skos:definition`,
//!   else its `rdfs:label`; only the annotation it carries is mapped, and a
//!   label a definition displaces is deferred.
//! - `FunctionalProperty(p)` → every member `p` links to one;
//!   `InverseFunctionalProperty(p)` → the inverse multiplicity is one.
//! - Datatype members take the type the datatype table chooses.
//!
//! Interfaces only narrow what they inherit, so the members `C` declares are
//! the meet of its own axioms and its parents' effective members. When two
//! parents require one member with unrelated targets (or types), the edge to
//! the later parent is not lowered; when `C`'s own axiom cannot narrow what it
//! inherits, that axiom is not lowered. Both are recorded with the reason, and
//! the resolution repeats until it is stable, so the output always admits.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::codes;
use crate::contract::{Degradation, Disposition, NornHome, Target, TypeDecision};
use crate::owl::WitnessPolicy;
use crate::owl_model::{Axiom, Expr, Item, Model, OWL_THING, PropertyKind, Restriction};
use crate::type_tables::{TableInput, TypeTable};

/// One member requirement a class declares or inherits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shape {
    Link {
        target: String,
        to_one: bool,
        required: bool,
        from_one: bool,
    },
    Value {
        kind: ValueType,
        required: bool,
    },
}

/// A datatype member's Norn type and the table row that chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValueType {
    pub json: Value,
    pub decision: TypeDecision,
    pub degraded: Option<String>,
}

/// The Norn type the datatype table chooses for `datatype` (`""` when the
/// range is unknown or not one named datatype).
fn value_type(table: &mut TypeTable, datatype: &str) -> Result<ValueType, String> {
    let row = table.lookup(&[("datatype", TableInput::Text(datatype.to_owned()))])?;
    let mut fields = serde_json::Map::new();
    fields.insert("kind".into(), json!(row.text("kind")));
    for key in ["format", "timezone"] {
        if !row.text(key).is_empty() {
            fields.insert(key.into(), json!(row.text(key)));
        }
    }
    let json = Value::Object(fields);
    Ok(ValueType {
        json,
        decision: TypeDecision {
            table: row.table.clone(),
            row: row.row.clone(),
        },
        degraded: (!row.text("code").is_empty()).then(|| row.text("detail").to_owned()),
    })
}

/// What one axiom asks of one class's member.
#[derive(Debug, Clone)]
struct Own {
    item: usize,
    property: String,
    shape: Shape,
    degraded: Option<Degradation>,
    /// Whether the shape's target or type is the property's named range.
    uses_range: bool,
}

/// What one axiom asks of a class: a member, nothing at all (`min 0`), or a
/// deferral.
enum Ask {
    Member {
        shape: Shape,
        degraded: Option<Degradation>,
        uses_range: bool,
    },
    Vacuous,
    Deferred(Disposition),
}

/// `rdfs:Literal`: the unqualified filler of a datatype restriction.
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";

/// The lowered interfaces and the disposition of every item.
pub(crate) struct Lowered {
    /// Owned class IRI → interface name.
    pub names: BTreeMap<String, String>,
    /// Property IRI → member id.
    pub members: BTreeMap<String, String>,
    /// Interface payloads, by class IRI.
    pub interfaces: BTreeMap<String, Value>,
    /// Item index → disposition.
    pub dispositions: BTreeMap<usize, Disposition>,
}

fn deferred(code: &crate::codes::Code, home: NornHome, detail: String) -> Disposition {
    Disposition::Deferred {
        home,
        code: code.code.to_owned(),
        detail,
    }
}

/// The interface artifact id of an interface name.
pub(crate) fn artifact_id(name: &str) -> String {
    format!("{name}.interface")
}

/// `PascalCase` for a class local name; `None` when nothing usable remains.
fn pascal(local: &str) -> Option<String> {
    let mut out: String = local.chars().filter(char::is_ascii_alphanumeric).collect();
    let first = out.chars().next()?;
    if first.is_ascii_digit() {
        out.insert(0, 'C');
    } else if first.is_ascii_lowercase() {
        out.replace_range(..1, &first.to_ascii_uppercase().to_string());
    }
    Some(out)
}

/// `snake_case` for a property local name or a prefix.
pub(crate) fn snake(local: &str) -> String {
    let chars: Vec<char> = local.chars().collect();
    let mut out = String::new();
    for (index, c) in chars.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|i| chars.get(i)).copied();
        let next = chars.get(index.saturating_add(1)).copied();
        if c.is_ascii_uppercase() {
            let boundary = previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || (previous.is_some_and(|p| p.is_ascii_uppercase())
                    && next.is_some_and(|n| n.is_ascii_lowercase()));
            if boundary {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    match trimmed.chars().next() {
        None => "p".to_owned(),
        Some(c) if c.is_ascii_digit() => format!("p_{trimmed}"),
        Some(_) => trimmed.to_owned(),
    }
}

/// The Norn namespace of a prefix: `fibo-be-le-lp` → `fibo.be.le.lp`.
pub(crate) fn namespace_of(prefix: &str) -> String {
    prefix
        .split(['-', '.', '_'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let segment = snake(segment);
            if segment.starts_with("p_")
                && segment.chars().nth(2).is_some_and(|c| c.is_ascii_digit())
            {
                segment.replacen("p_", "n", 1)
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Name every owned class and member; refuse a collision.
/// Class IRI → interface name, and member IRI → member id.
type Names = (BTreeMap<String, String>, BTreeMap<String, String>);

fn name(model: &Model) -> Result<Names, String> {
    let mut names = BTreeMap::new();
    let mut taken = BTreeMap::new();
    for iri in &model.classes {
        let Some((namespace, local)) = model.split(iri) else {
            continue;
        };
        let local = pascal(local).ok_or_else(|| format!("class <{iri}> has no usable name"))?;
        let name = format!("{}.{local}", namespace_of(&namespace.prefix));
        if let Some(other) = taken.insert(name.clone(), iri.clone()) {
            return Err(format!("classes <{other}> and <{iri}> both name {name}"));
        }
        names.insert(iri.clone(), name);
    }
    let mut by_id: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for (iri, kind) in &model.properties {
        if *kind == PropertyKind::Annotation {
            continue;
        }
        if let Some((_, local)) = model.split(iri) {
            by_id.entry(snake(local)).or_default().push(iri);
        }
    }
    let mut members = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (id, iris) in by_id {
        for iri in iris.iter().copied() {
            let id = if iris.len() == 1 {
                id.clone()
            } else {
                let (namespace, _) = model.split(iri).ok_or("unowned property")?;
                format!("{}_{id}", snake(&namespace.prefix))
            };
            if !used.insert(id.clone()) {
                return Err(format!("two properties name member {id}"));
            }
            members.insert(iri.clone(), id);
        }
    }
    Ok((names, members))
}

/// Lower `model` with `table` choosing datatype member types.
#[allow(
    clippy::too_many_lines,
    clippy::result_large_err,
    reason = "one pass over the axioms; a deferral carries its disposition"
)]
pub(crate) fn lower(
    model: &Model,
    table: &mut TypeTable,
    witnesses: WitnessPolicy,
) -> Result<Lowered, String> {
    let (names, members) = name(model)?;
    let mut lowered = Lowered {
        names,
        members,
        interfaces: BTreeMap::new(),
        dispositions: BTreeMap::new(),
    };
    let owned = |iri: &str| lowered.names.contains_key(iri);
    let placeable = |iri: &str| lowered.members.contains_key(iri);

    let mut functional = BTreeSet::new();
    let mut inverse_functional = BTreeSet::new();
    let mut ranges: BTreeMap<&str, Vec<(usize, &Expr)>> = BTreeMap::new();
    for (index, item) in model.items.iter().enumerate() {
        if let Item::Axiom(Axiom::Characteristic {
            property,
            characteristic,
        }) = item
        {
            match characteristic.as_str() {
                "FunctionalProperty" => {
                    functional.insert(property.as_str());
                }
                "InverseFunctionalProperty" => {
                    inverse_functional.insert(property.as_str());
                }
                _ => {}
            }
        } else if let Item::Axiom(Axiom::Range { property, range }) = item {
            ranges
                .entry(property.as_str())
                .or_default()
                .push((index, range));
        }
    }
    // The single named range of each placeable property, when it has one.
    let mut range_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (property, list) in &ranges {
        let named_ranges: BTreeSet<&str> = list
            .iter()
            .filter_map(|(_, expr)| {
                if let Expr::Named(iri) = expr {
                    Some(iri.as_str())
                } else {
                    None
                }
            })
            .collect();
        if let (true, [only]) = (
            placeable(property),
            named_ranges.iter().copied().collect::<Vec<_>>().as_slice(),
        ) {
            range_of.insert(property, only);
        }
    }

    // Every asserted named superclass, for whether a filler covers a range.
    let mut named_sups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for item in &model.items {
        if let Item::Axiom(Axiom::SubClassOf {
            sub,
            sup: Expr::Named(sup),
        }) = item
        {
            named_sups
                .entry(sub.as_str())
                .or_default()
                .push(sup.as_str());
        }
    }
    // Classes on which a datatype property holds at most one value, by a
    // `max 1` or `exactly 1` over every value of it.
    let mut capped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for item in &model.items {
        if let Item::Axiom(Axiom::SubClassOf {
            sub,
            sup:
                Expr::Restriction {
                    property,
                    kind: Restriction::Max(1) | Restriction::Exact(1),
                    filler,
                },
        }) = item
            && model.properties.get(property) == Some(&PropertyKind::Datatype)
        {
            let range = range_of.get(property.as_str()).copied();
            let whole = match read_filler(filler.as_deref()) {
                Filler::Unqualified => true,
                Filler::Named(iri) => range == Some(iri),
                Filler::Expression => false,
            };
            if whole {
                capped
                    .entry(property.as_str())
                    .or_default()
                    .push(sub.as_str());
            }
        }
    }
    let context = Context {
        model,
        names: &lowered.names,
        members: &lowered.members,
        range_of: &range_of,
        named_sups: &named_sups,
        functional: &functional,
        capped: &capped,
        witnesses,
    };

    // Every axiom that asks a class for a member.
    let mut own: Vec<(String, Own)> = Vec::new();
    for (index, item) in model.items.iter().enumerate() {
        let Item::Axiom(axiom) = item else { continue };
        let ask = match axiom {
            Axiom::SubClassOf {
                sub,
                sup: Expr::Restriction {
                    property,
                    kind,
                    filler,
                },
            } if owned(sub) => {
                restriction_shape(&context, table, sub, property, kind, filler.as_deref())
                    .map(|ask| (sub.clone(), property.clone(), ask))
            }
            Axiom::Domain {
                property,
                domain: Expr::Named(domain),
            } if owned(domain) && placeable(property) => {
                match model.properties.get(property) {
                    Some(PropertyKind::Object) => match range_of.get(property.as_str()) {
                        Some(range) if owned(range) => Ok((
                            domain.clone(),
                            property.clone(),
                            Ask::Member {
                                shape: Shape::Link {
                                    target: (*range).to_owned(),
                                    to_one: false,
                                    required: false,
                                    from_one: false,
                                },
                                degraded: None,
                                uses_range: true,
                            },
                        )),
                        _ => Ok((
                            domain.clone(),
                            property.clone(),
                            Ask::Deferred(deferred(
                                &codes::OWL_UNPLACED_PROPERTY,
                                NornHome::EntityIr,
                                "the property has no single named range in the import set, so a domain member has no link target".into(),
                            )),
                        )),
                    },
                    Some(PropertyKind::Datatype) => {
                        let datatype = range_of.get(property.as_str()).copied().unwrap_or("");
                        value_type(table, datatype).map(|kind| {
                            (
                                domain.clone(),
                                property.clone(),
                                Ask::Member {
                                    shape: Shape::Value {
                                        kind,
                                        required: false,
                                    },
                                    degraded: None,
                                    uses_range: !datatype.is_empty(),
                                },
                            )
                        })
                    }
                    Some(PropertyKind::Annotation) | None => Ok((
                        domain.clone(),
                        property.clone(),
                        Ask::Deferred(deferred(
                            &codes::OWL_UNPLACED_PROPERTY,
                            NornHome::EntityIr,
                            "an annotation property carries no member".into(),
                        )),
                    )),
                }
            }
            Axiom::SubClassOf { .. }
            | Axiom::EquivalentClasses { .. }
            | Axiom::DisjointClasses(_)
            | Axiom::DisjointUnion { .. }
            | Axiom::Domain { .. }
            | Axiom::Range { .. }
            | Axiom::Characteristic { .. }
            | Axiom::PropertyRelation { .. }
            | Axiom::ClassAssertion { .. } => continue,
        };
        let (class, property, ask) = ask?;
        match ask {
            Ask::Member {
                shape,
                degraded,
                uses_range,
            } => {
                own.push((
                    class,
                    Own {
                        item: index,
                        property,
                        shape,
                        degraded,
                        uses_range,
                    },
                ));
            }
            Ask::Vacuous => {
                let artifact = artifact_id(lowered.names.get(&class).map_or("", String::as_str));
                lowered
                    .dispositions
                    .insert(index, mapped(Target::artifact(&artifact)));
            }
            Ask::Deferred(disposition) => {
                lowered.dispositions.insert(index, disposition);
            }
        }
    }
    for (_, ask) in &mut own {
        if let Shape::Link {
            to_one, from_one, ..
        } = &mut ask.shape
        {
            *to_one |= functional.contains(ask.property.as_str());
            *from_one |= inverse_functional.contains(ask.property.as_str());
        }
        if let Shape::Value { kind, .. } = &ask.shape
            && let Some(detail) = &kind.degraded
        {
            match &mut ask.degraded {
                Some(existing) => {
                    existing.detail.push_str("; ");
                    existing.detail.push_str(detail);
                }
                None => {
                    ask.degraded = Some(Degradation {
                        code: codes::OWL_DEGRADED.code.to_owned(),
                        detail: detail.clone(),
                    });
                }
            }
        }
    }

    // Superclass edges between owned classes; an edge closing a cycle is an
    // implied equivalence.
    let mut edges: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for (index, item) in model.items.iter().enumerate() {
        if let Item::Axiom(Axiom::SubClassOf {
            sub,
            sup: Expr::Named(sup),
        }) = item
            && owned(sub)
            && owned(sup)
            && sub != sup
        {
            edges
                .entry(sub.clone())
                .or_default()
                .insert(sup.clone(), index);
        }
    }
    let mut dropped_edges: BTreeMap<usize, Disposition> = BTreeMap::new();
    let pairs: Vec<(String, String, usize)> = edges
        .iter()
        .flat_map(|(sub, sups)| {
            sups.iter()
                .map(move |(sup, index)| (sub.clone(), sup.clone(), *index))
        })
        .collect();
    for (sub, sup, index) in pairs {
        if reaches(&edges, &sup, &sub) {
            if let Some(sups) = edges.get_mut(&sub) {
                sups.remove(&sup);
            }
            dropped_edges.insert(
                index,
                deferred(
                    &codes::OWL_EQUIVALENCE,
                    NornHome::EntityIr,
                    format!(
                        "{} and {} are subclasses of each other: an equivalence, and interface inheritance is acyclic",
                        model.compact(&sub),
                        model.compact(&sup)
                    ),
                ),
            );
        }
    }

    let (declared, rejected) = loop {
        match resolve(model, &lowered.names, &edges, &own) {
            Resolution::Stable { declared, rejected } => break (declared, rejected),
            Resolution::DropEdge { sub, sup, detail } => {
                if let Some(index) = edges.get_mut(&sub).and_then(|sups| sups.remove(&sup)) {
                    dropped_edges.insert(
                        index,
                        deferred(&codes::OWL_INHERITANCE_CONFLICT, NornHome::EntityIr, detail),
                    );
                } else {
                    return Err(format!("inheritance resolution did not converge at {sub}"));
                }
            }
        }
    };

    // Interfaces. `described` records, per documented IRI, which annotation
    // its emitted description carries (a definition before a label) and
    // where it was first emitted: only that annotation is mapped.
    let mut described: BTreeMap<String, (&'static str, Target)> = BTreeMap::new();
    for (iri, name) in &lowered.names {
        let artifact = artifact_id(name);
        let parents: Vec<&String> = edges
            .get(iri)
            .into_iter()
            .flat_map(BTreeMap::keys)
            .filter_map(|parent| lowered.names.get(parent))
            .collect();
        let mut properties = Vec::new();
        let mut links = Vec::new();
        for (property, shape) in declared.get(iri).into_iter().flatten() {
            let Some(id) = lowered.members.get(property) else {
                continue;
            };
            let documentation = documentation(model, property);
            let description = documentation.map_or_else(
                || format!("<{property}>"),
                |(_, text)| format!("{} <{property}>", text.trim()),
            );
            let emitted = |described: &mut BTreeMap<String, (&'static str, Target)>| {
                if let Some((predicate, _)) = documentation {
                    described
                        .entry(property.clone())
                        .or_insert_with(|| (predicate, Target::member(&artifact, id)));
                }
            };
            match shape {
                Shape::Link {
                    target,
                    to_one,
                    required,
                    from_one,
                } => {
                    let Some(target) = lowered.names.get(target) else {
                        continue;
                    };
                    let mut cardinality = json!({
                        "to": if *to_one { "one" } else { "many" },
                        "from": if *from_one { "one" } else { "many" },
                    });
                    if *required && let Some(fields) = cardinality.as_object_mut() {
                        fields.insert("required".into(), json!(true));
                    }
                    links.push(json!({
                        "id": id, "description": description,
                        "target": target, "cardinality": cardinality,
                    }));
                    emitted(&mut described);
                }
                Shape::Value { kind, required } => {
                    let mut property = json!({
                        "id": id, "description": description, "type": kind.json,
                    });
                    if !required && let Some(fields) = property.as_object_mut() {
                        fields.insert("constraints".into(), json!({ "nullable": true }));
                    }
                    properties.push(property);
                    emitted(&mut described);
                }
            }
        }
        let documentation = documentation(model, iri);
        if let Some((predicate, _)) = documentation {
            described.insert(iri.clone(), (predicate, Target::artifact(&artifact)));
        }
        let text = documentation.map_or_else(String::new, |(_, text)| format!("{} ", text.trim()));
        let mut interface = serde_json::Map::new();
        interface.insert("name".into(), json!(name));
        interface.insert("description".into(), json!(format!("{text}<{iri}>")));
        if !parents.is_empty() {
            interface.insert("extends".into(), json!(parents));
        }
        if !properties.is_empty() {
            interface.insert("properties".into(), Value::Array(properties));
        }
        if !links.is_empty() {
            interface.insert("links".into(), Value::Array(links));
        }
        lowered
            .interfaces
            .insert(iri.clone(), Value::Object(interface));
    }

    dispose(
        model,
        &mut lowered,
        &Accounted {
            own: &own,
            rejected: &rejected,
            edges: &edges,
            range_of: &range_of,
            described: &described,
        },
        dropped_edges,
    );
    Ok(lowered)
}

/// The annotation an IRI's description carries, and its text: its
/// `skos:definition` when it has one, else its `rdfs:label`.
fn documentation<'a>(model: &'a Model, iri: &str) -> Option<(&'static str, &'a String)> {
    model
        .definitions
        .get(iri)
        .map(|text| (crate::owl_model::SKOS_DEFINITION, text))
        .or_else(|| {
            model
                .labels
                .get(iri)
                .map(|text| (crate::owl_model::RDFS_LABEL, text))
        })
}

/// What the lowering decided, for the disposition of every item.
struct Accounted<'a> {
    own: &'a [(String, Own)],
    /// Own axioms not lowered: item → detail.
    rejected: &'a BTreeMap<usize, String>,
    edges: &'a BTreeMap<String, BTreeMap<String, usize>>,
    range_of: &'a BTreeMap<&'a str, &'a str>,
    /// Documented IRI → the annotation its description carries, and where.
    described: &'a BTreeMap<String, (&'static str, Target)>,
}

/// What the restriction lowering reads besides the restriction itself.
struct Context<'a> {
    model: &'a Model,
    names: &'a BTreeMap<String, String>,
    members: &'a BTreeMap<String, String>,
    range_of: &'a BTreeMap<&'a str, &'a str>,
    /// Class → its asserted named superclasses.
    named_sups: &'a BTreeMap<&'a str, Vec<&'a str>>,
    /// Functional properties: one value at most, everywhere.
    functional: &'a BTreeSet<&'a str>,
    /// Datatype property → the classes a whole-property `max 1` or
    /// `exactly 1` caps it on.
    capped: &'a BTreeMap<&'a str, Vec<&'a str>>,
    witnesses: WitnessPolicy,
}

impl Context<'_> {
    /// Whether `class` is `ancestor` or reaches it through asserted named
    /// superclasses.
    fn subsumed(&self, class: &str, ancestor: &str) -> bool {
        let mut stack = vec![class];
        let mut seen = BTreeSet::new();
        while let Some(node) = stack.pop() {
            if node == ancestor {
                return true;
            }
            if seen.insert(node) {
                stack.extend(self.named_sups.get(node).into_iter().flatten().copied());
            }
        }
        false
    }

    /// Whether every `class` holds at most one `property` value, so a
    /// single-valued data member imposes no bound OWL does not state.
    fn at_most_one(&self, class: &str, property: &str) -> bool {
        self.functional.contains(property)
            || self
                .capped
                .get(property)
                .into_iter()
                .flatten()
                .any(|capping| self.subsumed(class, capping))
    }
}

/// A restriction's filler, as the lowering reads it.
enum Filler<'a> {
    /// No filler, `owl:Thing` or `rdfs:Literal`: the restriction speaks of
    /// every value.
    Unqualified,
    /// One named class or datatype.
    Named(&'a str),
    /// A class expression or data range.
    Expression,
}

/// How the lowering reads a restriction's filler.
fn read_filler(filler: Option<&Expr>) -> Filler<'_> {
    match filler {
        None => Filler::Unqualified,
        Some(Expr::Named(iri)) if iri == OWL_THING || iri == RDFS_LITERAL => Filler::Unqualified,
        Some(Expr::Named(iri)) => Filler::Named(iri.as_str()),
        Some(_) => Filler::Expression,
    }
}

/// What a restriction asks of its class's member, or why it asks nothing the
/// member can carry.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per restriction kind and filler"
)]
fn restriction_shape(
    context: &Context<'_>,
    table: &mut TypeTable,
    class: &str,
    property: &str,
    kind: &Restriction,
    filler: Option<&Expr>,
) -> Result<Ask, String> {
    let model = context.model;
    let defer = |code: &codes::Code, home: NornHome, detail: String| {
        Ok(Ask::Deferred(deferred(code, home, detail)))
    };
    let Some(property_kind) = model.properties.get(property).copied() else {
        return defer(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            format!(
                "{} is not declared in the import set",
                model.compact(property)
            ),
        );
    };
    let is_object = match property_kind {
        PropertyKind::Object if context.members.contains_key(property) => true,
        PropertyKind::Datatype if context.members.contains_key(property) => false,
        PropertyKind::Object | PropertyKind::Datatype | PropertyKind::Annotation => {
            return defer(
                &codes::OWL_EXTERNAL_REFERENCE,
                NornHome::MappingIr,
                format!(
                    "{} is not an owned object or datatype property",
                    model.compact(property)
                ),
            );
        }
    };
    let filler = read_filler(filler);
    let range = context.range_of.get(property).copied();
    // (at least, at most) values the restriction counts.
    let (least, most) = match kind {
        Restriction::All => {
            // Every value is a filler: the whole link targets it.
            return if is_object {
                let (target, uses_range) = match filler {
                    Filler::Unqualified => (range, true),
                    Filler::Named(iri) => (Some(iri), false),
                    Filler::Expression => {
                        return defer(
                            &codes::OWL_CLASS_EXPRESSION,
                            NornHome::EntityIr,
                            "the filler is a class expression; a link target is one named type"
                                .into(),
                        );
                    }
                };
                Ok(link(context, target, false, false, None, uses_range))
            } else {
                let (datatype, uses_range) = match filler {
                    Filler::Unqualified => (range.unwrap_or(""), range.is_some()),
                    Filler::Named(iri) => (iri, false),
                    Filler::Expression => ("", false),
                };
                Ok(Ask::Member {
                    shape: Shape::Value {
                        kind: value_type(table, datatype)?,
                        required: false,
                    },
                    degraded: None,
                    uses_range,
                })
            };
        }
        Restriction::Min(0) => return Ok(Ask::Vacuous),
        Restriction::Max(0) | Restriction::Exact(0) => {
            return defer(
                &codes::OWL_CARDINALITY_BOUND,
                NornHome::RuleIr,
                "a prohibition (no values) needs a law, not a member".into(),
            );
        }
        Restriction::HasValue(_) | Restriction::HasSelf => {
            return defer(
                &codes::OWL_VALUE_RESTRICTION,
                NornHome::RuleIr,
                "a fixed value needs an invariant over the member; individuals are not imported"
                    .into(),
            );
        }
        Restriction::Some => (1, None),
        Restriction::Min(n) => (*n, None),
        Restriction::Exact(n) => (*n, Some(*n)),
        Restriction::Max(n) => (0, Some(*n)),
    };
    let closed = context.witnesses == WitnessPolicy::Closed;
    // Does the restriction speak of every value? Only when its filler
    // covers the property's established range.
    let covered = match (&filler, is_object) {
        (Filler::Unqualified, _) => true,
        (Filler::Named(iri), true) => range.is_some_and(|range| context.subsumed(range, iri)),
        (Filler::Named(iri), false) => range == Some(*iri),
        (Filler::Expression, _) => false,
    };
    if !covered {
        let filler_text = match filler {
            Filler::Named(iri) => model.compact(iri),
            Filler::Unqualified | Filler::Expression => "the filler expression".to_owned(),
        };
        let scope = format!(
            "{} counts only the {} values that are {filler_text}: other values are unconstrained and separate values may satisfy other restrictions on it, so neither its target nor its bound holds for the whole link{}",
            describe_restriction(kind),
            model.compact(property),
            range.map_or_else(
                || " (the property has no single named range)".to_owned(),
                |range| format!(
                    " ({filler_text} does not cover the range {})",
                    model.compact(range)
                ),
            ),
        );
        // Under the closed-world policy, the existence of some value still
        // holds for the whole link.
        if closed && least >= 1 && range.is_some() {
            let degraded = Some(Degradation {
                code: codes::OWL_FILLER_SCOPED.code.to_owned(),
                detail: format!(
                    "only the existence of a value is lowered (witnesses: closed); {scope}"
                ),
            });
            return if is_object {
                Ok(link(context, range, false, true, degraded, true))
            } else if !context.at_most_one(class, property) {
                defer(
                    &codes::OWL_FILLER_SCOPED,
                    NornHome::EntityIr,
                    format!(
                        "{scope}; under witnesses: closed only its existence could be lowered, but a data member holds one value and nothing bounds {} to one value here (no FunctionalProperty and no max 1 over every value), so a required member would add a bound OWL does not state",
                        model.compact(property)
                    ),
                )
            } else {
                Ok(Ask::Member {
                    shape: Shape::Value {
                        kind: value_type(table, range.unwrap_or(""))?,
                        required: true,
                    },
                    degraded,
                    uses_range: true,
                })
            };
        }
        let open = if least >= 1 && !closed {
            "; the value it asserts exists is an open-world claim (witnesses: open)"
        } else {
            ""
        };
        return defer(
            &codes::OWL_FILLER_SCOPED,
            NornHome::EntityIr,
            format!("{scope}{open}"),
        );
    }
    let required = least >= 1 && closed;
    if !is_object && required && !context.at_most_one(class, property) && most != Some(1) {
        return defer(
            &codes::OWL_CARDINALITY_BOUND,
            NornHome::EntityIr,
            format!(
                "a data member holds one value and nothing bounds {} to one value here (no FunctionalProperty and no max 1 over every value), so a required member would add a bound OWL does not state",
                model.compact(property)
            ),
        );
    }
    let shape_word = if is_object {
        "many-member"
    } else {
        "single-valued member"
    };
    let mut notes: Vec<(&codes::Code, String)> = Vec::new();
    if least >= 1 && !closed {
        notes.push((
            &codes::OWL_OPEN_WORLD,
            format!(
                "at least {least} value{} asserted to exist: an open-world claim, kept optional (witnesses: open)",
                if least == 1 { "" } else { "s" }
            ),
        ));
    } else if least > 1 {
        notes.push((
            &codes::OWL_CARDINALITY_BOUND,
            format!(
                "at least {least} values: kept as a required {shape_word}; the count bound needs a counting law"
            ),
        ));
    }
    if let Some(most) = most
        && most > 1
    {
        notes.push((
            &codes::OWL_CARDINALITY_BOUND,
            format!(
                "at most {most} values: kept as a {shape_word}; the count bound needs a counting law"
            ),
        ));
    }
    let degraded = notes.first().map(|(code, _)| Degradation {
        code: code.code.to_owned(),
        detail: notes
            .iter()
            .map(|(_, detail)| detail.as_str())
            .collect::<Vec<_>>()
            .join("; "),
    });
    let to_one = most == Some(1);
    if is_object {
        Ok(link(context, range, to_one, required, degraded, true))
    } else {
        Ok(Ask::Member {
            shape: Shape::Value {
                kind: value_type(table, range.unwrap_or(""))?,
                required,
            },
            degraded,
            uses_range: range.is_some(),
        })
    }
}

/// The functional-syntax keyword of a counting restriction.
fn describe_restriction(kind: &Restriction) -> String {
    match kind {
        Restriction::Some => "a someValuesFrom restriction".to_owned(),
        Restriction::Min(n) => format!("a qualified min {n}"),
        Restriction::Max(n) => format!("a qualified max {n}"),
        Restriction::Exact(n) => format!("a qualified exactly {n}"),
        Restriction::All | Restriction::HasValue(_) | Restriction::HasSelf => {
            "the restriction".to_owned()
        }
    }
}

/// A link member to `target`, or why there is none.
fn link(
    context: &Context<'_>,
    target: Option<&str>,
    to_one: bool,
    required: bool,
    degraded: Option<Degradation>,
    uses_range: bool,
) -> Ask {
    match target {
        Some(target) if context.names.contains_key(target) => Ask::Member {
            shape: Shape::Link {
                target: target.to_owned(),
                to_one,
                required,
                from_one: false,
            },
            degraded,
            uses_range,
        },
        Some(target) => Ask::Deferred(deferred(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            format!(
                "the target {} is not an owned class",
                context.model.compact(target)
            ),
        )),
        None => Ask::Deferred(deferred(
            &codes::OWL_UNPLACED_PROPERTY,
            NornHome::EntityIr,
            "no named filler and no single named range: the link has no target".into(),
        )),
    }
}

fn reaches(edges: &BTreeMap<String, BTreeMap<String, usize>>, from: &str, to: &str) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if seen.insert(node) {
            stack.extend(
                edges
                    .get(node)
                    .into_iter()
                    .flat_map(BTreeMap::keys)
                    .map(String::as_str),
            );
        }
    }
    false
}

enum Resolution {
    Stable {
        /// Class → property → the member it declares.
        declared: BTreeMap<String, BTreeMap<String, Shape>>,
        /// Own axioms not lowered: item → detail.
        rejected: BTreeMap<usize, String>,
    },
    DropEdge {
        sub: String,
        sup: String,
        detail: String,
    },
}

/// The narrower of two shapes, or `None` when neither narrows the other.
fn meet(a: &Shape, b: &Shape, ancestors: &BTreeMap<String, BTreeSet<String>>) -> Option<Shape> {
    let under = |x: &str, y: &str| x == y || ancestors.get(x).is_some_and(|set| set.contains(y));
    match (a, b) {
        (
            Shape::Link {
                target: ta,
                to_one: oa,
                required: ra,
                from_one: fa,
            },
            Shape::Link {
                target: tb,
                to_one: ob,
                required: rb,
                from_one: fb,
            },
        ) => {
            let target = if under(ta, tb) {
                ta
            } else if under(tb, ta) {
                tb
            } else {
                return None;
            };
            Some(Shape::Link {
                target: target.clone(),
                to_one: *oa || *ob,
                required: *ra || *rb,
                from_one: *fa || *fb,
            })
        }
        (
            Shape::Value {
                kind: ka,
                required: ra,
            },
            Shape::Value {
                kind: kb,
                required: rb,
            },
        ) if ka.json == kb.json => Some(Shape::Value {
            kind: ka.clone(),
            required: *ra || *rb,
        }),
        (Shape::Link { .. } | Shape::Value { .. }, _) => None,
    }
}

fn describe(model: &Model, shape: &Shape) -> String {
    match shape {
        Shape::Link { target, .. } => model.compact(target),
        Shape::Value { kind, .. } => kind.json.to_string(),
    }
}

fn resolve(
    model: &Model,
    names: &BTreeMap<String, String>,
    edges: &BTreeMap<String, BTreeMap<String, usize>>,
    own: &[(String, Own)],
) -> Resolution {
    let mut ancestors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for class in names.keys() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = edges
            .get(class)
            .into_iter()
            .flat_map(BTreeMap::keys)
            .map(String::as_str)
            .collect();
        while let Some(node) = stack.pop() {
            if seen.insert(node) {
                stack.extend(
                    edges
                        .get(node)
                        .into_iter()
                        .flat_map(BTreeMap::keys)
                        .map(String::as_str),
                );
            }
        }
        ancestors.insert(class.clone(), seen.into_iter().map(str::to_owned).collect());
    }
    let mut order: Vec<&String> = names.keys().collect();
    order.sort_by_key(|class| (ancestors.get(*class).map_or(0, BTreeSet::len), *class));
    let mut by_class: BTreeMap<&str, Vec<&Own>> = BTreeMap::new();
    for (class, ask) in own {
        by_class.entry(class.as_str()).or_default().push(ask);
    }
    let mut effective: BTreeMap<String, BTreeMap<String, Shape>> = BTreeMap::new();
    let mut declared: BTreeMap<String, BTreeMap<String, Shape>> = BTreeMap::new();
    let mut rejected = BTreeMap::new();
    for class in order {
        let mut members: BTreeMap<String, (Shape, String)> = BTreeMap::new();
        for parent in edges.get(class).into_iter().flat_map(BTreeMap::keys) {
            for (property, shape) in effective.get(parent).into_iter().flatten() {
                match members.get(property) {
                    None => {
                        members.insert(property.clone(), (shape.clone(), parent.clone()));
                    }
                    Some((current, from)) => match meet(current, shape, &ancestors) {
                        Some(narrower) => {
                            let source = if narrower != *current && narrower == *shape {
                                parent.clone()
                            } else {
                                from.clone()
                            };
                            members.insert(property.clone(), (narrower, source));
                        }
                        None => {
                            return Resolution::DropEdge {
                                sub: class.clone(),
                                sup: parent.clone(),
                                detail: format!(
                                    "{} and {} both pass member {} on, as {} and {}: no member narrows both, so this edge is not lowered",
                                    model.compact(from),
                                    model.compact(parent),
                                    model.compact(property),
                                    describe(model, current),
                                    describe(model, shape),
                                ),
                            };
                        }
                    },
                }
            }
        }
        let mut mine: BTreeMap<String, Shape> = BTreeMap::new();
        for ask in by_class.get(class.as_str()).into_iter().flatten() {
            let base = mine
                .get(&ask.property)
                .or_else(|| members.get(&ask.property).map(|(shape, _)| shape));
            let combined = match base {
                None => Some(ask.shape.clone()),
                Some(base) => meet(base, &ask.shape, &ancestors),
            };
            match combined {
                Some(shape) => {
                    mine.insert(ask.property.clone(), shape);
                }
                None => {
                    rejected.insert(
                        ask.item,
                        format!(
                            "the member is already {} here, which {} does not narrow",
                            base.map_or_default(|shape| describe(model, shape)),
                            describe(model, &ask.shape)
                        ),
                    );
                }
            }
        }
        let mut all: BTreeMap<String, Shape> = members
            .into_iter()
            .map(|(property, (shape, _))| (property, shape))
            .collect();
        all.extend(mine.clone());
        effective.insert(class.clone(), all);
        declared.insert(class.clone(), mine);
    }
    Resolution::Stable { declared, rejected }
}

fn mapped(target: Target) -> Disposition {
    Disposition::Mapped {
        target,
        degraded: None,
        decision: None,
    }
}

/// The disposition of every item.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per item kind keeps the accounting in one place"
)]
fn dispose(
    model: &Model,
    lowered: &mut Lowered,
    accounted: &Accounted<'_>,
    dropped_edges: BTreeMap<usize, Disposition>,
) {
    let Accounted {
        own,
        rejected,
        edges,
        range_of,
        described,
    } = *accounted;
    let target_of = |class: &str, member: Option<&str>| {
        let artifact = artifact_id(lowered.names.get(class).map_or("", String::as_str));
        member.map_or_else(
            || Target::artifact(&artifact),
            |member| Target::member(&artifact, member),
        )
    };
    // Where each property landed first (by class IRI), for the property's
    // own declaration, range, characteristics and annotations.
    let mut placed: BTreeMap<&str, &str> = BTreeMap::new();
    // Where each property's named range typed a lowered member first.
    let mut ranged: BTreeMap<&str, &str> = BTreeMap::new();
    let mut dispositions = dropped_edges;
    for (class, ask) in own {
        if let Some(detail) = rejected.get(&ask.item) {
            dispositions.insert(
                ask.item,
                deferred(
                    &codes::OWL_INHERITANCE_CONFLICT,
                    NornHome::EntityIr,
                    detail.clone(),
                ),
            );
            continue;
        }
        placed
            .entry(ask.property.as_str())
            .or_insert(class.as_str());
        if ask.uses_range {
            ranged
                .entry(ask.property.as_str())
                .or_insert(class.as_str());
        }
        let member = lowered.members.get(&ask.property).map(String::as_str);
        let decision = match &ask.shape {
            Shape::Value { kind, .. } => Some(kind.decision.clone()),
            Shape::Link { .. } => None,
        };
        dispositions.insert(
            ask.item,
            Disposition::Mapped {
                target: target_of(class, member),
                degraded: ask.degraded.clone(),
                decision,
            },
        );
    }
    let mapped_in = |map: &BTreeMap<&str, &str>, property: &str| {
        map.get(property).map(|class| {
            mapped(target_of(
                class,
                lowered.members.get(property).map(String::as_str),
            ))
        })
    };
    let mapped_property = |property: &str| mapped_in(&placed, property);
    // A range axiom is mapped only where it typed a lowered member.
    let mapped_range = |property: &str| mapped_in(&ranged, property);
    let unplaced = || {
        deferred(
            &codes::OWL_UNPLACED_PROPERTY,
            NornHome::EntityIr,
            "no domain or restriction gives the property an owner type, so no member carries it"
                .into(),
        )
    };
    for (index, item) in model.items.iter().enumerate() {
        if dispositions.contains_key(&index) || lowered.dispositions.contains_key(&index) {
            continue;
        }
        let disposition = match item {
            Item::Class(iri) if lowered.names.contains_key(iri) => mapped(target_of(iri, None)),
            Item::Class(iri) | Item::ExternalClass(iri) => deferred(
                &codes::OWL_EXTERNAL_REFERENCE,
                NornHome::MappingIr,
                format!("{} is outside every owned namespace", model.compact(iri)),
            ),
            Item::Ontology(_) => deferred(
                &codes::OWL_ANNOTATION,
                NornHome::MappingIr,
                "the ontology header: its release, licence and imports are the pack's pinned source admission".into(),
            ),
            Item::Property(iri, PropertyKind::Annotation) => deferred(
                &codes::OWL_ANNOTATION,
                NornHome::WidgetIr,
                format!("annotation property {}: documentation, not structure", model.compact(iri)),
            ),
            Item::Property(iri, _) => mapped_property(iri).unwrap_or_else(unplaced),
            Item::Datatype(iri) => deferred(
                &codes::OWL_CLASS_EXPRESSION,
                NornHome::EntityIr,
                format!("datatype {} is a data range; members over it are typed by the datatype table", model.compact(iri)),
            ),
            Item::Individual(iri, count) => deferred(
                &codes::OWL_INDIVIDUAL,
                NornHome::MappingIr,
                format!("{} with {count} statements is reference data: rows through a mapping, not a type", model.compact(iri)),
            ),
            Item::Annotation { subject, predicate } => {
                let documented = predicate == crate::owl_model::RDFS_LABEL
                    || predicate == crate::owl_model::SKOS_DEFINITION;
                match described.get(subject) {
                    Some((selected, target)) if documented && selected == predicate => {
                        mapped(target.clone())
                    }
                    Some((selected, _)) if documented => deferred(
                        &codes::OWL_ANNOTATION,
                        NornHome::WidgetIr,
                        format!(
                            "{} is not in the description: its {} takes precedence, so this text is kept in the source graph",
                            model.compact(predicate),
                            model.compact(selected)
                        ),
                    ),
                    _ if documented => deferred(
                        &codes::OWL_ANNOTATION,
                        NornHome::WidgetIr,
                        format!(
                            "{} is documentation of a term no emitted description carries, kept in the source graph",
                            model.compact(predicate)
                        ),
                    ),
                    _ => deferred(
                        &codes::OWL_ANNOTATION,
                        NornHome::WidgetIr,
                        format!("{} is documentation, kept in the source graph", model.compact(predicate)),
                    ),
                }
            }
            Item::AxiomAnnotation(_) => deferred(
                &codes::OWL_ANNOTATION,
                NornHome::WidgetIr,
                "an annotation on an axiom or an all-different assertion over individuals".into(),
            ),
            Item::Statement(_) => deferred(
                &codes::OWL_DEFERRED,
                NornHome::EntityIr,
                "a statement outside the OWL 2 constructs this reader lowers".into(),
            ),
            Item::Axiom(axiom) => axiom_disposition(model, lowered, axiom, edges, range_of, &Placement {
                property: &mapped_property,
                range: &mapped_range,
                unplaced: &unplaced,
                target_of: &target_of,
            }),
        };
        dispositions.insert(index, disposition);
    }
    lowered.dispositions.extend(dispositions);
}

/// Where an axiom's property landed, for the axioms that only qualify it.
struct Placement<'a> {
    /// The first member the property became.
    property: &'a dyn Fn(&str) -> Option<Disposition>,
    /// The first member the property's named range typed.
    range: &'a dyn Fn(&str) -> Option<Disposition>,
    unplaced: &'a dyn Fn() -> Disposition,
    target_of: &'a dyn Fn(&str, Option<&str>) -> Target,
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per axiom kind keeps the accounting in one place"
)]
fn axiom_disposition(
    model: &Model,
    lowered: &Lowered,
    axiom: &Axiom,
    edges: &BTreeMap<String, BTreeMap<String, usize>>,
    range_of: &BTreeMap<&str, &str>,
    placement: &Placement<'_>,
) -> Disposition {
    let Placement {
        property: mapped_property,
        range: mapped_range,
        unplaced,
        target_of,
    } = *placement;
    let owned = |iri: &str| lowered.names.contains_key(iri);
    match axiom {
        Axiom::SubClassOf { sub, sup: Expr::Named(sup) }
            if owned(sub) && (sup == OWL_THING || sub == sup) =>
        {
            mapped(target_of(sub, None))
        }
        Axiom::SubClassOf { sub, sup: Expr::Named(sup) }
            if owned(sub) && edges.get(sub).is_some_and(|sups| sups.contains_key(sup)) =>
        {
            mapped(target_of(sub, None))
        }
        Axiom::SubClassOf { sub, sup: Expr::Named(_) } if !owned(sub) => deferred(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            format!("the subclass {} is not an owned class", model.compact(sub)),
        ),
        Axiom::SubClassOf { sup: Expr::Named(sup), .. } => deferred(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            format!("the superclass {} is not an owned class", model.compact(sup)),
        ),
        Axiom::SubClassOf { sup: Expr::Union(_) | Expr::Complement(_) | Expr::OneOf(_) | Expr::Other(_), .. } => deferred(
            &codes::OWL_CLASS_EXPRESSION,
            NornHome::EntityIr,
            "a union, complement or enumeration as a superclass: a type extends named interfaces only".into(),
        ),
        Axiom::SubClassOf { sup: Expr::Intersection(_), .. } => deferred(
            &codes::OWL_CLASS_EXPRESSION,
            NornHome::EntityIr,
            "an intersection as a superclass: its named members would each be an edge; not lowered yet".into(),
        ),
        Axiom::SubClassOf { sup: Expr::Restriction { .. }, .. } => deferred(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            "a restriction on a class outside every owned namespace".into(),
        ),
        Axiom::EquivalentClasses { .. } => deferred(
            &codes::OWL_EQUIVALENCE,
            NornHome::EntityIr,
            "an equivalence is a sufficient condition: Norn types are asserted, never classified, so membership by definition needs defined concepts".into(),
        ),
        Axiom::DisjointClasses(_) | Axiom::DisjointUnion { .. } => deferred(
            &codes::OWL_DISJOINTNESS,
            NornHome::RuleIr,
            "disjointness of interfaces (no type implements both) has no construct; it needs a law".into(),
        ),
        // A domain on an owned class with a placeable property became a
        // member (or was deferred) when the members were asked for; here
        // the domain itself contributes nothing.
        Axiom::Domain { domain: Expr::Named(domain), .. } if !owned(domain) => deferred(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            format!("the domain {} is not an owned class", model.compact(domain)),
        ),
        Axiom::Domain { domain: Expr::Named(_), property } => deferred(
            &codes::OWL_EXTERNAL_REFERENCE,
            NornHome::MappingIr,
            format!("{} is not an owned object or datatype property", model.compact(property)),
        ),
        Axiom::Domain { .. } => deferred(
            &codes::OWL_CLASS_EXPRESSION,
            NornHome::EntityIr,
            "a class-expression domain (a union, intersection or restriction): a member lives on one named type".into(),
        ),
        Axiom::Range { property, range } => match range {
            Expr::Named(_) if range_of.contains_key(property.as_str()) => {
                mapped_range(property).unwrap_or_else(|| {
                    if mapped_property(property).is_some() {
                        deferred(
                            &codes::OWL_UNPLACED_PROPERTY,
                            NornHome::EntityIr,
                            "the range types no lowered member: every member of the property takes its target or type from a restriction's filler".into(),
                        )
                    } else {
                        unplaced()
                    }
                })
            }
            Expr::Named(_) => deferred(
                &codes::OWL_CLASS_EXPRESSION,
                NornHome::EntityIr,
                "one of several named ranges (their intersection): a link has one target".into(),
            ),
            Expr::Restriction { .. }
            | Expr::Union(_)
            | Expr::Intersection(_)
            | Expr::Complement(_)
            | Expr::OneOf(_)
            | Expr::Other(_) => deferred(
                &codes::OWL_CLASS_EXPRESSION,
                NornHome::EntityIr,
                "a class-expression range: a link target is one named type".into(),
            ),
        },
        Axiom::Characteristic { property, characteristic }
            if matches!(characteristic.as_str(), "FunctionalProperty" | "InverseFunctionalProperty") =>
        {
            mapped_property(property).unwrap_or_else(unplaced)
        }
        Axiom::Characteristic { .. } | Axiom::PropertyRelation { .. } => deferred(
            &codes::OWL_PROPERTY_AXIOM,
            NornHome::RuleIr,
            "sub-, inverse, equivalent and chained properties and transitive, symmetric or reflexive characteristics are inference rules; a derived link is a question".into(),
        ),
        Axiom::ClassAssertion { .. } => deferred(
            &codes::OWL_INDIVIDUAL,
            NornHome::MappingIr,
            "a class assertion on a declared term (punning) is data about it".into(),
        ),
    }
}
