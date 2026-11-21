//! The OWL 2 structural view of an RDF graph (OWL 2 Mapping to RDF Graphs,
//! the subset ontologies such as FIBO use), and the partition of every
//! statement into exactly one accounted [`Item`].
//!
//! Every statement whose subject is an IRI belongs to one item: a
//! declaration, an axiom, an annotation facet, an individual, or (when none
//! of those fits) a plain statement. Blank-node structure (restrictions,
//! lists, class expressions) belongs to the item that references it; a
//! blank node nothing references is an item of its own. The partition is the
//! loss report's inventory, so a statement cannot fall out of the count.

use std::collections::{BTreeMap, BTreeSet};

use crate::ntriples::{Term, Triple};

pub(crate) const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub(crate) const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub(crate) const OWL: &str = "http://www.w3.org/2002/07/owl#";
pub(crate) const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

pub(crate) const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
pub(crate) const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
pub(crate) const SKOS_DEFINITION: &str = "http://www.w3.org/2004/02/skos/core#definition";
pub(crate) const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

/// The deepest class expression rendered; deeper (or cyclic) blank-node
/// structure is refused.
const MAX_EXPRESSION_DEPTH: usize = 32;

/// What an IRI is declared as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PropertyKind {
    Object,
    Datatype,
    Annotation,
}

/// A class expression (or data range), structurally.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Expr {
    /// A named class or datatype.
    Named(String),
    /// An `owl:Restriction`.
    Restriction {
        property: String,
        kind: Restriction,
        /// The qualifying class or data range (`someValuesFrom`, `onClass`, …).
        filler: Option<Box<Expr>>,
    },
    Union(Vec<Expr>),
    Intersection(Vec<Expr>),
    Complement(Box<Expr>),
    OneOf(Vec<Term>),
    /// Any other structure, kept as its rendering.
    Other(String),
}

/// The restriction constructor and its number or value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Restriction {
    Some,
    All,
    HasValue(Term),
    HasSelf,
    Min(u64),
    Max(u64),
    Exact(u64),
}

/// One logical axiom, structurally.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Axiom {
    SubClassOf {
        sub: String,
        sup: Expr,
    },
    EquivalentClasses {
        class: String,
        other: Expr,
    },
    DisjointClasses(Vec<Expr>),
    DisjointUnion {
        class: String,
        members: Vec<Expr>,
    },
    Domain {
        property: String,
        domain: Expr,
    },
    Range {
        property: String,
        range: Expr,
    },
    /// `owl:FunctionalProperty`, `owl:TransitiveProperty`, … by local name.
    Characteristic {
        property: String,
        characteristic: String,
    },
    /// `rdfs:subPropertyOf`, `owl:inverseOf`, `owl:equivalentProperty`,
    /// `owl:propertyDisjointWith`, `owl:propertyChainAxiom`.
    PropertyRelation {
        property: String,
        relation: String,
        other: Expr,
    },
    /// An IRI typed by a class while also declared as something else (punning).
    ClassAssertion {
        class: Expr,
        individual: String,
    },
}

/// One accounted unit of the source graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Item {
    Ontology(String),
    /// A class declared in an owned namespace.
    Class(String),
    /// A class declared outside every owned namespace.
    ExternalClass(String),
    Property(String, PropertyKind),
    Datatype(String),
    /// A named individual and the number of statements about it.
    Individual(String, usize),
    Axiom(Axiom),
    /// One annotation predicate on a declared IRI (all its values).
    Annotation {
        subject: String,
        predicate: String,
    },
    /// An annotated-axiom or `owl:AllDifferent` blank node, by its rendering.
    AxiomAnnotation(String),
    /// A statement nothing else accounts for, by its rendering.
    Statement(String),
}

/// An owned namespace and its preferred prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Namespace {
    pub iri: String,
    pub prefix: String,
}

/// The indexed graph.
pub(crate) struct Model {
    subjects: BTreeMap<Term, Vec<(String, Term)>>,
    namespaces: Vec<Namespace>,
    pub classes: BTreeSet<String>,
    pub properties: BTreeMap<String, PropertyKind>,
    pub datatypes: BTreeSet<String>,
    pub items: Vec<Item>,
    /// Labels and definitions per IRI (`rdfs:label`, `skos:definition`).
    pub labels: BTreeMap<String, String>,
    pub definitions: BTreeMap<String, String>,
}

impl Model {
    /// Index `triples` and partition them into items.
    pub(crate) fn build(triples: Vec<Triple>, namespaces: Vec<Namespace>) -> Result<Self, String> {
        let mut subjects: BTreeMap<Term, Vec<(String, Term)>> = BTreeMap::new();
        for triple in triples {
            subjects
                .entry(triple.subject)
                .or_default()
                .push((triple.predicate, triple.object));
        }
        let mut model = Self {
            subjects,
            namespaces,
            classes: BTreeSet::new(),
            properties: BTreeMap::new(),
            datatypes: BTreeSet::new(),
            items: Vec::new(),
            labels: BTreeMap::new(),
            definitions: BTreeMap::new(),
        };
        model.declare();
        model.partition()?;
        model.items.sort();
        model.items.dedup();
        Ok(model)
    }

    /// The namespace and local name of an owned IRI.
    pub(crate) fn split<'a>(&'a self, iri: &'a str) -> Option<(&'a Namespace, &'a str)> {
        self.namespaces
            .iter()
            .filter_map(|namespace| {
                iri.strip_prefix(namespace.iri.as_str())
                    .filter(|local| !local.is_empty() && !local.contains(['/', '#']))
                    .map(|local| (namespace, local))
            })
            .max_by_key(|(namespace, _)| namespace.iri.len())
    }

    /// `prefix:Local` for an owned IRI, `<iri>` otherwise.
    pub(crate) fn compact(&self, iri: &str) -> String {
        if let Some((namespace, local)) = self.split(iri) {
            return format!("{}:{local}", namespace.prefix);
        }
        for (vocabulary, prefix) in [(RDF, "rdf"), (RDFS, "rdfs"), (OWL, "owl"), (XSD, "xsd")] {
            if let Some(local) = iri.strip_prefix(vocabulary) {
                return format!("{prefix}:{local}");
            }
        }
        format!("<{iri}>")
    }

    fn objects<'a>(
        &'a self,
        subject: &Term,
        predicate: &str,
    ) -> impl Iterator<Item = &'a Term> + use<'a> {
        let predicate = predicate.to_owned();
        self.subjects
            .get(subject)
            .into_iter()
            .flatten()
            .filter(move |(p, _)| *p == predicate)
            .map(|(_, object)| object)
    }

    fn types(&self, subject: &Term) -> BTreeSet<String> {
        self.objects(subject, RDF_TYPE)
            .filter_map(Term::iri)
            .map(str::to_owned)
            .collect()
    }

    /// Record what every IRI subject is declared as.
    fn declare(&mut self) {
        let mut classes = BTreeSet::new();
        let mut properties = BTreeMap::new();
        let mut datatypes = BTreeSet::new();
        for subject in self.subjects.keys() {
            let Term::Iri(iri) = subject else { continue };
            let types = self.types(subject);
            if types.contains(&format!("{OWL}Class")) || types.contains(&format!("{RDFS}Class")) {
                classes.insert(iri.clone());
            }
            if types.contains(&format!("{RDFS}Datatype")) {
                datatypes.insert(iri.clone());
            }
            let kind = if types.contains(&format!("{OWL}ObjectProperty")) {
                Some(PropertyKind::Object)
            } else if types.contains(&format!("{OWL}DatatypeProperty")) {
                Some(PropertyKind::Datatype)
            } else if types.contains(&format!("{OWL}AnnotationProperty")) {
                Some(PropertyKind::Annotation)
            } else {
                None
            };
            if let Some(kind) = kind {
                properties.insert(iri.clone(), kind);
            }
        }
        self.classes = classes;
        self.properties = properties;
        self.datatypes = datatypes;
    }

    fn is_declared(&self, iri: &str) -> bool {
        self.classes.contains(iri)
            || self.properties.contains_key(iri)
            || self.datatypes.contains(iri)
            || self
                .types(&Term::Iri(iri.to_owned()))
                .contains(&format!("{OWL}Ontology"))
    }

    fn partition(&mut self) -> Result<(), String> {
        let mut items = Vec::new();
        let mut referenced = BTreeSet::new();
        for (subject, statements) in &self.subjects {
            let Term::Iri(iri) = subject else { continue };
            if !self.is_declared(iri) {
                items.push(Item::Individual(iri.clone(), statements.len()));
                for (_, object) in statements {
                    self.reach(object, &mut referenced, 0)?;
                }
                continue;
            }
            for (predicate, object) in statements {
                self.reach(object, &mut referenced, 0)?;
                items.push(self.classify(iri, predicate, object)?);
            }
        }
        for subject in self.subjects.keys() {
            if subject.is_blank() && !referenced.contains(subject) {
                items.push(self.top_level_blank(subject)?);
            }
        }
        self.items = items;
        let mut labels = BTreeMap::new();
        let mut definitions = BTreeMap::new();
        for subject in self.subjects.keys() {
            let Term::Iri(iri) = subject else { continue };
            if let Some(text) = self.preferred_text(subject, RDFS_LABEL) {
                labels.insert(iri.clone(), text);
            }
            if let Some(text) = self.preferred_text(subject, SKOS_DEFINITION) {
                definitions.insert(iri.clone(), text);
            }
        }
        self.labels = labels;
        self.definitions = definitions;
        Ok(())
    }

    /// The English (or untagged) text value of `predicate`, smallest first.
    fn preferred_text(&self, subject: &Term, predicate: &str) -> Option<String> {
        let mut candidates: Vec<(u8, &str)> = self
            .objects(subject, predicate)
            .filter_map(|object| match object {
                Term::Literal {
                    value, language, ..
                } => {
                    let rank = match language.as_deref() {
                        Some("en") => 0,
                        None => 1,
                        Some(_) => 2,
                    };
                    Some((rank, value.as_str()))
                }
                Term::Iri(_) | Term::Blank(_) => None,
            })
            .collect();
        candidates.sort_unstable();
        candidates.first().map(|(_, text)| (*text).to_owned())
    }

    /// Mark every blank node reachable from `term` as referenced.
    fn reach(
        &self,
        term: &Term,
        referenced: &mut BTreeSet<Term>,
        depth: usize,
    ) -> Result<(), String> {
        if !term.is_blank() || !referenced.insert(term.clone()) {
            return Ok(());
        }
        if depth > MAX_EXPRESSION_DEPTH {
            return Err("blank-node structure deeper than 32 levels".into());
        }
        for (_, object) in self.subjects.get(term).into_iter().flatten() {
            self.reach(object, referenced, depth.saturating_add(1))?;
        }
        Ok(())
    }

    fn classify(&self, iri: &str, predicate: &str, object: &Term) -> Result<Item, String> {
        let named = || Expr::Named(iri.to_owned());
        let property = || iri.to_owned();
        let item = match predicate
            .strip_prefix(RDFS)
            .or_else(|| predicate.strip_prefix(OWL))
        {
            Some("subClassOf") => Item::Axiom(Axiom::SubClassOf {
                sub: iri.to_owned(),
                sup: self.expr(object, 0)?,
            }),
            Some("equivalentClass") if self.datatypes.contains(iri) => {
                Item::Datatype(iri.to_owned())
            }
            Some("equivalentClass") => Item::Axiom(Axiom::EquivalentClasses {
                class: iri.to_owned(),
                other: self.expr(object, 0)?,
            }),
            Some("disjointWith") => {
                let mut members = vec![named(), self.expr(object, 0)?];
                members.sort();
                Item::Axiom(Axiom::DisjointClasses(members))
            }
            Some("disjointUnionOf") => Item::Axiom(Axiom::DisjointUnion {
                class: iri.to_owned(),
                members: self.list(object, 0)?,
            }),
            Some("domain") => Item::Axiom(Axiom::Domain {
                property: property(),
                domain: self.expr(object, 0)?,
            }),
            Some("range") => Item::Axiom(Axiom::Range {
                property: property(),
                range: self.expr(object, 0)?,
            }),
            Some(
                relation @ ("subPropertyOf"
                | "inverseOf"
                | "equivalentProperty"
                | "propertyDisjointWith"),
            ) => Item::Axiom(Axiom::PropertyRelation {
                property: property(),
                relation: relation.to_owned(),
                other: self.expr(object, 0)?,
            }),
            Some("propertyChainAxiom") => Item::Axiom(Axiom::PropertyRelation {
                property: property(),
                relation: "propertyChainAxiom".to_owned(),
                other: Expr::Intersection(self.list(object, 0)?),
            }),
            _ if predicate == RDF_TYPE => self.typing(iri, object)?,
            _ => Item::Annotation {
                subject: iri.to_owned(),
                predicate: predicate.to_owned(),
            },
        };
        Ok(item)
    }

    fn typing(&self, iri: &str, object: &Term) -> Result<Item, String> {
        let Some(class) = object.iri() else {
            return Ok(Item::Axiom(Axiom::ClassAssertion {
                class: self.expr(object, 0)?,
                individual: iri.to_owned(),
            }));
        };
        let item = match class.strip_prefix(OWL).or_else(|| class.strip_prefix(RDFS)) {
            Some("Ontology") => Item::Ontology(iri.to_owned()),
            Some("Class") if self.split(iri).is_some() => Item::Class(iri.to_owned()),
            Some("Class") => Item::ExternalClass(iri.to_owned()),
            Some("Datatype") => Item::Datatype(iri.to_owned()),
            Some("ObjectProperty") => Item::Property(iri.to_owned(), PropertyKind::Object),
            Some("DatatypeProperty") => Item::Property(iri.to_owned(), PropertyKind::Datatype),
            Some("AnnotationProperty") => Item::Property(iri.to_owned(), PropertyKind::Annotation),
            Some(
                characteristic @ ("FunctionalProperty"
                | "InverseFunctionalProperty"
                | "TransitiveProperty"
                | "SymmetricProperty"
                | "AsymmetricProperty"
                | "ReflexiveProperty"
                | "IrreflexiveProperty"),
            ) => Item::Axiom(Axiom::Characteristic {
                property: iri.to_owned(),
                characteristic: characteristic.to_owned(),
            }),
            _ => Item::Axiom(Axiom::ClassAssertion {
                class: Expr::Named(class.to_owned()),
                individual: iri.to_owned(),
            }),
        };
        Ok(item)
    }

    fn top_level_blank(&self, subject: &Term) -> Result<Item, String> {
        let types = self.types(subject);
        if types.contains(&format!("{OWL}AllDisjointClasses")) {
            let members = self
                .objects(subject, &format!("{OWL}members"))
                .next()
                .map(|list| self.list(list, 0))
                .transpose()?
                .unwrap_or_default();
            return Ok(Item::Axiom(Axiom::DisjointClasses(members)));
        }
        let rendering = self.render_blank(subject, 0)?;
        if types.contains(&format!("{OWL}Axiom")) || types.contains(&format!("{OWL}AllDifferent")) {
            Ok(Item::AxiomAnnotation(rendering))
        } else {
            Ok(Item::Statement(rendering))
        }
    }

    /// The members of an RDF list, in list order.
    fn list(&self, head: &Term, depth: usize) -> Result<Vec<Expr>, String> {
        let mut members = Vec::new();
        let mut node = head.clone();
        let mut steps = 0_usize;
        while node != Term::Iri(RDF_NIL.to_owned()) {
            steps = steps.saturating_add(1);
            if steps > 4096 {
                return Err("an RDF list longer than 4,096 members, or cyclic".into());
            }
            let first = self
                .objects(&node, RDF_FIRST)
                .next()
                .ok_or("an RDF list node without rdf:first")?;
            members.push(self.expr(first, depth.saturating_add(1))?);
            node = self
                .objects(&node, RDF_REST)
                .next()
                .cloned()
                .ok_or("an RDF list node without rdf:rest")?;
        }
        Ok(members)
    }

    /// The class expression (or data range) `term` denotes.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per OWL class-expression form"
    )]
    pub(crate) fn expr(&self, term: &Term, depth: usize) -> Result<Expr, String> {
        if depth > MAX_EXPRESSION_DEPTH {
            return Err("a class expression deeper than 32 levels, or cyclic".into());
        }
        let blank = match term {
            Term::Iri(iri) => return Ok(Expr::Named(iri.clone())),
            Term::Literal { .. } => return Ok(Expr::OneOf(vec![term.clone()])),
            Term::Blank(_) => term,
        };
        let next = depth.saturating_add(1);
        let one = |name: &str| self.objects(blank, &format!("{OWL}{name}")).next();
        if let Some(property) = one("onProperty").and_then(Term::iri) {
            let filler = ["someValuesFrom", "allValuesFrom", "onClass", "onDataRange"]
                .into_iter()
                .find_map(&one)
                .map(|filler| self.expr(filler, next).map(Box::new))
                .transpose()?;
            let number = |name: &str| -> Option<Result<u64, String>> {
                one(name).map(|value| match value {
                    Term::Literal { value, .. } => value
                        .parse::<u64>()
                        .map_err(|_parse| format!("cardinality {value:?} is not a whole number")),
                    Term::Iri(_) | Term::Blank(_) => Err("a cardinality is a literal".into()),
                })
            };
            let kind = if one("someValuesFrom").is_some() {
                Restriction::Some
            } else if one("allValuesFrom").is_some() {
                Restriction::All
            } else if let Some(value) = one("hasValue") {
                Restriction::HasValue(value.clone())
            } else if one("hasSelf").is_some() {
                Restriction::HasSelf
            } else if let Some(n) = number("qualifiedCardinality").or_else(|| number("cardinality"))
            {
                Restriction::Exact(n?)
            } else if let Some(n) =
                number("minQualifiedCardinality").or_else(|| number("minCardinality"))
            {
                Restriction::Min(n?)
            } else if let Some(n) =
                number("maxQualifiedCardinality").or_else(|| number("maxCardinality"))
            {
                Restriction::Max(n?)
            } else {
                return Ok(Expr::Other(self.render_blank(blank, depth)?));
            };
            // A restriction with more than one constructor is not one OWL
            // restriction; keep it whole rather than pick one reading.
            let constructors = self
                .subjects
                .get(blank)
                .into_iter()
                .flatten()
                .filter(|(p, _)| p.starts_with(OWL) && p.as_str() != format!("{OWL}onProperty"))
                .count();
            let expected =
                if filler.is_some() && !matches!(kind, Restriction::Some | Restriction::All) {
                    2
                } else {
                    1
                };
            if constructors != expected {
                return Ok(Expr::Other(self.render_blank(blank, depth)?));
            }
            return Ok(Expr::Restriction {
                property: property.to_owned(),
                kind,
                filler,
            });
        }
        if let Some(list) = one("unionOf") {
            return Ok(Expr::Union(self.list(list, next)?));
        }
        if let Some(list) = one("intersectionOf") {
            return Ok(Expr::Intersection(self.list(list, next)?));
        }
        if let Some(inner) = one("complementOf") {
            return Ok(Expr::Complement(Box::new(self.expr(inner, next)?)));
        }
        if let Some(list) = one("oneOf") {
            let members = self
                .list(list, next)?
                .into_iter()
                .map(|member| match member {
                    Expr::Named(iri) => Term::Iri(iri),
                    Expr::OneOf(mut terms) if terms.len() == 1 => {
                        terms.pop().unwrap_or(Term::Iri(String::new()))
                    }
                    other @ (Expr::Restriction { .. }
                    | Expr::Union(_)
                    | Expr::Intersection(_)
                    | Expr::Complement(_)
                    | Expr::OneOf(_)
                    | Expr::Other(_)) => Term::Literal {
                        value: format!("{other:?}"),
                        datatype: crate::ntriples::XSD_STRING.into(),
                        language: None,
                    },
                })
                .collect();
            return Ok(Expr::OneOf(members));
        }
        Ok(Expr::Other(self.render_blank(blank, depth)?))
    }

    /// A blank node's statements, rendered with its blank children inline:
    /// `[p o; p2 [..]]`. Labels never appear, so the rendering depends only
    /// on the graph's structure.
    fn render_blank(&self, blank: &Term, depth: usize) -> Result<String, String> {
        if depth > MAX_EXPRESSION_DEPTH {
            return Err("blank-node structure deeper than 32 levels, or cyclic".into());
        }
        let mut parts = Vec::new();
        for (predicate, object) in self.subjects.get(blank).into_iter().flatten() {
            let object = match object {
                Term::Blank(_) => self.render_blank(object, depth.saturating_add(1))?,
                Term::Iri(iri) => self.compact(iri),
                Term::Literal { .. } => object.to_string(),
            };
            parts.push(format!("{} {object}", self.compact(predicate)));
        }
        parts.sort();
        Ok(format!("[{}]", parts.join("; ")))
    }

    /// Functional-syntax rendering of an expression.
    pub(crate) fn render(&self, expr: &Expr) -> String {
        let join = |items: &[Expr]| {
            items
                .iter()
                .map(|item| self.render(item))
                .collect::<Vec<_>>()
                .join(" ")
        };
        match expr {
            Expr::Named(iri) => self.compact(iri),
            Expr::Restriction {
                property,
                kind,
                filler,
            } => {
                let data = self.properties.get(property) == Some(&PropertyKind::Datatype);
                let family = if data { "Data" } else { "Object" };
                let property = self.compact(property);
                let filler = filler
                    .as_deref()
                    .map(|filler| format!(" {}", self.render(filler)))
                    .unwrap_or_default();
                match kind {
                    Restriction::Some => format!("{family}SomeValuesFrom({property}{filler})"),
                    Restriction::All => format!("{family}AllValuesFrom({property}{filler})"),
                    Restriction::HasValue(value) => {
                        let value = match value {
                            Term::Iri(iri) => self.compact(iri),
                            Term::Blank(_) | Term::Literal { .. } => value.to_string(),
                        };
                        format!("{family}HasValue({property} {value})")
                    }
                    Restriction::HasSelf => format!("ObjectHasSelf({property})"),
                    Restriction::Min(n) => {
                        format!("{family}MinCardinality({n} {property}{filler})")
                    }
                    Restriction::Max(n) => {
                        format!("{family}MaxCardinality({n} {property}{filler})")
                    }
                    Restriction::Exact(n) => {
                        format!("{family}ExactCardinality({n} {property}{filler})")
                    }
                }
            }
            Expr::Union(items) => format!("ObjectUnionOf({})", join(items)),
            Expr::Intersection(items) => format!("ObjectIntersectionOf({})", join(items)),
            Expr::Complement(inner) => format!("ObjectComplementOf({})", self.render(inner)),
            Expr::OneOf(terms) => format!(
                "ObjectOneOf({})",
                terms
                    .iter()
                    .map(|term| match term {
                        Term::Iri(iri) => self.compact(iri),
                        Term::Blank(_) | Term::Literal { .. } => term.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            Expr::Other(rendering) => rendering.clone(),
        }
    }

    /// Functional-syntax rendering of an axiom: its stable identity.
    pub(crate) fn render_axiom(&self, axiom: &Axiom) -> String {
        let all = |items: &[Expr]| {
            items
                .iter()
                .map(|item| self.render(item))
                .collect::<Vec<_>>()
                .join(" ")
        };
        match axiom {
            Axiom::SubClassOf { sub, sup } => {
                format!("SubClassOf({} {})", self.compact(sub), self.render(sup))
            }
            Axiom::EquivalentClasses { class, other } => format!(
                "EquivalentClasses({} {})",
                self.compact(class),
                self.render(other)
            ),
            Axiom::DisjointClasses(members) => format!("DisjointClasses({})", all(members)),
            Axiom::DisjointUnion { class, members } => {
                format!("DisjointUnion({} {})", self.compact(class), all(members))
            }
            Axiom::Domain { property, domain } => format!(
                "PropertyDomain({} {})",
                self.compact(property),
                self.render(domain)
            ),
            Axiom::Range { property, range } => format!(
                "PropertyRange({} {})",
                self.compact(property),
                self.render(range)
            ),
            Axiom::Characteristic {
                property,
                characteristic,
            } => format!("{characteristic}({})", self.compact(property)),
            Axiom::PropertyRelation {
                property,
                relation,
                other,
            } => format!(
                "{relation}({} {})",
                self.compact(property),
                self.render(other)
            ),
            Axiom::ClassAssertion { class, individual } => format!(
                "ClassAssertion({} {})",
                self.render(class),
                self.compact(individual)
            ),
        }
    }
}
