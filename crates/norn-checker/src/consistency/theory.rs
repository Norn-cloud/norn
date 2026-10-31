//! Closed-world integrity constraints as data-exchange dependencies.
//!
//! A [`Theory`] is a list of tuple-generating dependencies ([`Tgd`]: "every
//! invoice line belongs to an invoice") and equality-generating dependencies
//! ([`Egd`]: keys, functional dependencies, "exactly one customer"). Theories
//! are generated from typed definitions and mappings, never hand-written.

use std::collections::{BTreeMap, BTreeSet};

/// A term of a dependency atom.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    /// A universally quantified variable (body) or, when it occurs only in a
    /// TGD head, an existentially quantified one.
    Var(String),
    /// A constant, compared by exact canonical text.
    Const(String),
}

/// `relation(terms…)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Atom {
    /// Relation name; one arity per name across the theory.
    pub relation: String,
    /// Positional terms.
    pub terms: Vec<Term>,
}

/// Where a dependency was lowered from, so findings point at its definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleOrigin {
    /// The artifact that declares the constraint or mapping.
    pub artifact_id: Option<String>,
    /// RFC 6901 pointer into that artifact.
    pub pointer: String,
}

/// `body → ∃ȳ. head`: whenever the body matches, the head must too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tgd {
    /// Stable rule id, unique within the theory.
    pub id: String,
    /// Declaring definition.
    pub origin: RuleOrigin,
    /// Non-empty conjunction.
    pub body: Vec<Atom>,
    /// Non-empty conjunction; head-only variables are existential.
    pub head: Vec<Atom>,
}

/// `body → left = right`: a failed equation between two constants is a
/// contradiction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egd {
    /// Stable rule id, unique within the theory.
    pub id: String,
    /// Declaring definition.
    pub origin: RuleOrigin,
    /// Non-empty conjunction.
    pub body: Vec<Atom>,
    /// Bound by the body when a variable.
    pub left: Term,
    /// Bound by the body when a variable.
    pub right: Term,
}

/// One dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// Tuple-generating.
    Tgd(Tgd),
    /// Equality-generating.
    Egd(Egd),
}

impl Dependency {
    /// The rule id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Tgd(tgd) => &tgd.id,
            Self::Egd(egd) => &egd.id,
        }
    }

    /// The declaring definition.
    #[must_use]
    pub fn origin(&self) -> &RuleOrigin {
        match self {
            Self::Tgd(tgd) => &tgd.origin,
            Self::Egd(egd) => &egd.origin,
        }
    }

    pub(crate) fn atoms(&self) -> impl Iterator<Item = &Atom> {
        let (body, head): (&[Atom], &[Atom]) = match self {
            Self::Tgd(tgd) => (&tgd.body, &tgd.head),
            Self::Egd(egd) => (&egd.body, &[]),
        };
        body.iter().chain(head)
    }
}

/// A set of dependencies, checked in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theory {
    /// Dependencies with unique ids.
    pub dependencies: Vec<Dependency>,
}

/// Why a theory cannot be chased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TheoryError {
    /// Two dependencies share an id.
    DuplicateRuleId(String),
    /// A dependency has an empty body, or a TGD an empty head.
    EmptyConjunction(String),
    /// A relation is used with two arities.
    ArityMismatch {
        /// The relation.
        relation: String,
        /// The first arity seen.
        expected: usize,
        /// The conflicting arity.
        found: usize,
    },
    /// A mapping contains an EGD; mappings are TGDs from source to target.
    EgdInMapping(String),
    /// A relation no dependency mentions.
    UnknownRelation(String),
    /// An EGD side names a variable its body does not bind.
    UnboundVariable {
        /// The EGD.
        rule: String,
        /// The variable.
        variable: String,
    },
}

impl Theory {
    /// Every relation with its arity, after [`Theory::validate`].
    ///
    /// # Errors
    /// See [`Theory::validate`].
    pub fn signature(&self) -> Result<BTreeMap<String, usize>, TheoryError> {
        let mut ids = BTreeSet::new();
        let mut arities: BTreeMap<String, usize> = BTreeMap::new();
        for dependency in &self.dependencies {
            let id = dependency.id();
            if !ids.insert(id) {
                return Err(TheoryError::DuplicateRuleId(id.to_owned()));
            }
            let empty = match dependency {
                Dependency::Tgd(tgd) => tgd.body.is_empty() || tgd.head.is_empty(),
                Dependency::Egd(egd) => egd.body.is_empty(),
            };
            if empty {
                return Err(TheoryError::EmptyConjunction(id.to_owned()));
            }
            for atom in dependency.atoms() {
                record_arity(&mut arities, &atom.relation, atom.terms.len())?;
            }
            if let Dependency::Egd(egd) = dependency {
                let bound = variables(&egd.body);
                for side in [&egd.left, &egd.right] {
                    if let Term::Var(variable) = side
                        && !bound.contains(variable.as_str())
                    {
                        return Err(TheoryError::UnboundVariable {
                            rule: id.to_owned(),
                            variable: variable.clone(),
                        });
                    }
                }
            }
        }
        Ok(arities)
    }

    /// Check rule ids, conjunctions, arities and EGD range restriction.
    ///
    /// # Errors
    /// The first [`TheoryError`] in declaration order.
    pub fn validate(&self) -> Result<(), TheoryError> {
        self.signature().map(drop)
    }

    /// The sub-theory of the dependencies whose ids are in `keep`, in order.
    #[must_use]
    pub fn restricted_to(&self, keep: &BTreeSet<&str>) -> Self {
        Self {
            dependencies: self
                .dependencies
                .iter()
                .filter(|dependency| keep.contains(dependency.id()))
                .cloned()
                .collect(),
        }
    }
}

pub(crate) fn record_arity(
    arities: &mut BTreeMap<String, usize>,
    relation: &str,
    arity: usize,
) -> Result<(), TheoryError> {
    match arities.get(relation) {
        Some(&expected) if expected != arity => Err(TheoryError::ArityMismatch {
            relation: relation.to_owned(),
            expected,
            found: arity,
        }),
        Some(_) => Ok(()),
        None => {
            arities.insert(relation.to_owned(), arity);
            Ok(())
        }
    }
}

pub(crate) fn variables(atoms: &[Atom]) -> BTreeSet<&str> {
    atoms
        .iter()
        .flat_map(|atom| &atom.terms)
        .filter_map(|term| match term {
            Term::Var(name) => Some(name.as_str()),
            Term::Const(_) => None,
        })
        .collect()
}
