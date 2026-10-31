//! Lower ontology and link packs (`registry/packs`) into consistency
//! theories, and run the separate obligations on each system → canon link.
//!
//! The lowering is the trust boundary of every pack finding, so it is
//! explicit and total over what it accepts:
//!
//! - a `class`, `predicate` or `action` term `T` is a unary relation `T(x)`;
//!   a `property` term `P` is a binary relation `P(x, v)`;
//! - `parentTermId` gives `T → parent`; a property's `domainTermId` gives
//!   `P(x, v) → D(x)`;
//! - a shape field `f` of class `C` is the property term `C.f` when that term
//!   exists with domain `C` (identified by id), otherwise its own relation
//!   `C#f`; `minCount ≥ 1` gives `C(x) → ∃v f(x, v)`, `maxCount = 1` makes it
//!   functional, and a `term_ref` target `R` gives `f(x, v) → R(v)`;
//! - a correspondence `subsumed_by` gives `from → to`, `subsumes` `to → from`,
//!   `equivalent` both; `derived` is not lowered (it is checked against its
//!   transform, not as a rule) and is listed in [`PackLink::not_lowered`].
//!
//! Each link's exchange: the source is the system pack, the mapping is the
//! link's rules plus the target's hierarchy (a derivation: an invoice *is* a
//! claim, not a claim the data must also state), and the target is the rest
//! of the canon. Pack prose (such as a disjointness stated in a description)
//! has no rule and contributes nothing.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, CorrespondenceRelation, OntologyLinkRoot, OntologyPackRoot, OntologyTermKind,
    OntologyTermStatus, ShapeValueType,
};

use super::chase::ChaseBudget;
use super::check_non_vacuity;
use super::exchange::{Exchange, check_mapping_totality, check_schema_compatibility};
use super::theory::{Atom, Dependency, Egd, RuleOrigin, Term, Tgd, Theory};
use crate::Diagnostic;

/// One system → canon link, lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackLink {
    /// The link artifact id.
    pub link_id: String,
    /// The source pack's terms as relations, with their column names.
    pub source_schema: BTreeMap<String, Vec<String>>,
    /// The source pack.
    pub source: Theory,
    /// The link's rules and the target's hierarchy.
    pub mapping: Theory,
    /// The target pack without its hierarchy.
    pub target: Theory,
    /// Terms the link declares unmapped (residuals), maps only by a
    /// `derived` correspondence, or maps only canon → system (`subsumes`).
    pub gaps: BTreeSet<String>,
    /// What was not lowered, and why.
    pub not_lowered: Vec<String>,
}

struct Pack {
    id: String,
    root: OntologyPackRoot,
    arity: BTreeMap<String, usize>,
}

fn var(name: &str) -> Term {
    Term::Var(name.to_owned())
}

fn atom(relation: &str, arity: usize) -> Atom {
    let names = ["x", "v"];
    Atom {
        relation: relation.to_owned(),
        terms: names.iter().take(arity).map(|n| var(n)).collect(),
    }
}

fn origin(artifact: &str, pointer: String) -> RuleOrigin {
    RuleOrigin {
        artifact_id: Some(artifact.to_owned()),
        pointer,
    }
}

fn tgd(id: String, origin: RuleOrigin, body: Vec<Atom>, head: Vec<Atom>) -> Dependency {
    Dependency::Tgd(Tgd {
        id,
        origin,
        body,
        head,
    })
}

impl Pack {
    fn new(id: String, root: OntologyPackRoot) -> Self {
        let arity = root
            .terms
            .iter()
            .filter(|t| t.status == OntologyTermStatus::Active)
            .map(|t| {
                let arity = if t.term_kind == OntologyTermKind::Property {
                    2
                } else {
                    1
                };
                (t.term_id.clone(), arity)
            })
            .collect();
        Self { id, root, arity }
    }

    /// The pack's rules, split into its hierarchy and everything else.
    fn lower(&self) -> (Theory, Theory) {
        let (mut hierarchy, mut rules) = (Theory::default(), Theory::default());
        for (index, term) in self.root.terms.iter().enumerate() {
            let Some(&arity) = self.arity.get(&term.term_id) else {
                continue;
            };
            let at = || origin(&self.id, format!("/payload/terms/{index}"));
            if let Some(parent) = term.parent_term_id.as_deref()
                && self.arity.get(parent) == Some(&arity)
            {
                hierarchy.dependencies.push(tgd(
                    format!("parent:{}", term.term_id),
                    at(),
                    vec![atom(&term.term_id, arity)],
                    vec![atom(parent, arity)],
                ));
            }
            if arity == 2
                && let Some(domain) = term.domain_term_id.as_deref()
                && self.arity.get(domain) == Some(&1)
            {
                rules.dependencies.push(tgd(
                    format!("domain:{}", term.term_id),
                    at(),
                    vec![atom(&term.term_id, 2)],
                    vec![atom(domain, 1)],
                ));
            }
        }
        for (s, shape) in self.root.shapes.iter().enumerate() {
            let class = shape.term_id.as_str();
            if self.arity.get(class) != Some(&1) {
                continue;
            }
            for (f, field) in shape.fields.iter().enumerate() {
                let at = || origin(&self.id, format!("/payload/shapes/{s}/fields/{f}"));
                let named = format!("{class}.{}", field.field_id);
                let declared = self
                    .root
                    .terms
                    .iter()
                    .any(|t| t.term_id == named && t.domain_term_id.as_deref() == Some(class));
                let relation = if declared {
                    named
                } else {
                    format!("{class}#{}", field.field_id)
                };
                if !declared {
                    rules.dependencies.push(tgd(
                        format!("domain:{relation}"),
                        at(),
                        vec![atom(&relation, 2)],
                        vec![atom(class, 1)],
                    ));
                }
                if field.min_count.0 >= 1 {
                    rules.dependencies.push(tgd(
                        format!("required:{relation}"),
                        at(),
                        vec![atom(class, 1)],
                        vec![atom(&relation, 2)],
                    ));
                }
                if field.max_count.0 == 1 {
                    rules.dependencies.push(Dependency::Egd(Egd {
                        id: format!("functional:{relation}"),
                        origin: at(),
                        body: vec![
                            Atom {
                                relation: relation.clone(),
                                terms: vec![var("x"), var("v1")],
                            },
                            Atom {
                                relation: relation.clone(),
                                terms: vec![var("x"), var("v2")],
                            },
                        ],
                        left: var("v1"),
                        right: var("v2"),
                    }));
                }
                if field.value_type == ShapeValueType::TermRef
                    && let Some(target) = field.target_term_id.as_deref()
                    && self.arity.get(target) == Some(&1)
                {
                    rules.dependencies.push(tgd(
                        format!("range:{relation}"),
                        at(),
                        vec![atom(&relation, 2)],
                        vec![Atom {
                            relation: target.to_owned(),
                            terms: vec![var("v")],
                        }],
                    ));
                }
            }
        }
        (hierarchy, rules)
    }
}

fn payload_json(artifact: &ArtifactEnvelope) -> Result<serde_json::Value, String> {
    serde_json::to_value(artifact.payload.to_json_value())
        .map_err(|error| format!("{}: {error}", artifact.artifact_id))
}

/// Lower every link in `artifacts` against the ontology packs in the same
/// set, in artifact order.
///
/// # Errors
/// An artifact that does not decode as its pack kind, or a link whose packs
/// are not in the set.
pub fn lower_pack_links(artifacts: &[ArtifactEnvelope]) -> Result<Vec<PackLink>, String> {
    let mut packs = BTreeMap::new();
    let mut links = Vec::new();
    for artifact in artifacts {
        let payload = artifact.payload.to_json_value();
        let kind = payload
            .as_object()
            .and_then(|object| object.get("packKind"))
            .and_then(|kind| kind.as_string());
        match kind {
            Some("ontology") => {
                packs.insert(
                    artifact.artifact_id.clone(),
                    Pack::new(
                        artifact.artifact_id.clone(),
                        serde_json::from_value(payload_json(artifact)?)
                            .map_err(|error| format!("{}: {error}", artifact.artifact_id))?,
                    ),
                );
            }
            Some("link") => links.push((
                artifact.artifact_id.clone(),
                serde_json::from_value::<OntologyLinkRoot>(payload_json(artifact)?)
                    .map_err(|error| format!("{}: {error}", artifact.artifact_id))?,
            )),
            _ => {}
        }
    }
    links
        .into_iter()
        .map(|(id, link)| {
            let from = packs
                .get(&link.from_pack.pack_id)
                .ok_or_else(|| format!("{id}: unknown fromPack"))?;
            let to = packs
                .get(&link.to_pack.pack_id)
                .ok_or_else(|| format!("{id}: unknown toPack"))?;
            Ok(lower_link(id, &link, from, to))
        })
        .collect()
}

fn lower_link(id: String, link: &OntologyLinkRoot, from: &Pack, to: &Pack) -> PackLink {
    let (source_hierarchy, source_rules) = from.lower();
    let (target_hierarchy, target) = to.lower();
    let mut source = source_hierarchy;
    source.dependencies.extend(source_rules.dependencies);
    let mut mapping = target_hierarchy;
    let mut gaps: BTreeSet<String> = link.residuals.iter().map(|r| r.term_id.clone()).collect();
    let mut not_lowered = Vec::new();
    for (index, c) in link.correspondences.iter().enumerate() {
        let at = || origin(&id, format!("/payload/correspondences/{index}"));
        let arity = from.arity.get(&c.from_term_id).copied();
        let [to_term] = c.to_term_ids.as_slice() else {
            gaps.insert(c.from_term_id.clone());
            not_lowered.push(format!(
                "{}: maps onto {} terms",
                c.correspondence_id,
                c.to_term_ids.len()
            ));
            continue;
        };
        let reason = if c.relation == CorrespondenceRelation::Derived {
            Some("derived: checked against its transform, not as a rule")
        } else if arity.is_none() || arity != to.arity.get(to_term).copied() {
            Some("the two terms are not both classes or both properties")
        } else {
            None
        };
        if let Some(reason) = reason {
            gaps.insert(c.from_term_id.clone());
            not_lowered.push(format!("{}: {reason}", c.correspondence_id));
            continue;
        }
        let arity = arity.unwrap_or(1);
        let (a, b) = (atom(&c.from_term_id, arity), atom(to_term, arity));
        let forward = matches!(
            c.relation,
            CorrespondenceRelation::SubsumedBy | CorrespondenceRelation::Equivalent
        );
        let backward = matches!(
            c.relation,
            CorrespondenceRelation::Subsumes | CorrespondenceRelation::Equivalent
        );
        if forward {
            mapping.dependencies.push(tgd(
                format!("link:{}", c.correspondence_id),
                at(),
                vec![a.clone()],
                vec![b.clone()],
            ));
        }
        if backward {
            // Canon → system: the source term is accounted for, but no
            // source data flows to the canon through it.
            if !forward {
                gaps.insert(c.from_term_id.clone());
            }
            mapping.dependencies.push(tgd(
                format!("link:{}:back", c.correspondence_id),
                at(),
                vec![b],
                vec![a],
            ));
        }
    }
    let source_schema = from
        .arity
        .iter()
        .map(|(term, &arity)| {
            (
                term.clone(),
                ["x", "v"]
                    .iter()
                    .take(arity)
                    .map(|c| (*c).to_owned())
                    .collect(),
            )
        })
        .collect();
    PackLink {
        link_id: id,
        source_schema,
        source,
        mapping,
        target,
        gaps,
        not_lowered,
    }
}

/// Every obligation on one lowered link: non-vacuity of the merged theory,
/// schema compatibility (incompatible sources and unpreserved canon
/// constraints) and mapping totality against the link's declared gaps.
#[must_use]
pub fn check_pack_link(link: &PackLink, budget: ChaseBudget) -> Vec<Diagnostic> {
    let exchange = Exchange {
        source_schema: &link.source_schema,
        source: &link.source,
        mapping: &link.mapping,
        target: &link.target,
    };
    let mut merged = link.source.clone();
    merged
        .dependencies
        .extend(link.mapping.dependencies.iter().cloned());
    merged
        .dependencies
        .extend(link.target.dependencies.iter().cloned());
    let mut findings = check_non_vacuity(&merged, budget);
    findings.extend(check_schema_compatibility(&exchange, budget));
    findings.extend(check_mapping_totality(&exchange, &link.gaps));
    findings
}
