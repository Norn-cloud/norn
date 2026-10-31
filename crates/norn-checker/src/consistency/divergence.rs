//! Divergence witnesses: two mapped records that are provably the same
//! object and give one property two values (norn#79, lane C's claim
//! schema).
//!
//! Identity is established, never assumed. Each mapped record's object is a
//! labelled null per `(type, object key)`, so two records with the same key
//! are one object; a declared canon key equates two objects whose key values
//! agree. Each property is functional, so a chase failure on it is a
//! divergence, and its minimal witness is exactly the records involved.

use std::collections::{BTreeMap, BTreeSet};

use super::chase::{ChaseBudget, ChaseError, ChaseOutcome, Fact, Value, chase};
use super::theory::{Atom, Dependency, Egd, RuleOrigin, Term, Theory};

/// One mapped record: what the runtime's own mapping produced for a source
/// record (`MappedRecord`), with property values as canonical JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedObservation {
    /// The produced object's type.
    pub object_type: String,
    /// The produced object's key.
    pub key: String,
    /// Observed property values (canonical JSON text), lifecycle states
    /// included.
    pub values: BTreeMap<String, String>,
}

/// A declared business key of a canon type: equal values on every one of
/// `properties` mean the same object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonKey {
    /// The type.
    pub type_name: String,
    /// The key name.
    pub key: String,
    /// The key's properties.
    pub properties: Vec<String>,
}

/// How the two observations of a divergence are the same object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessIdentity {
    /// Both records have the same object type and key.
    SameObjectKey,
    /// The keys differ, but both records agree on every property of this
    /// declared key (`type_name`, key name).
    SharedKey(String, String),
}

/// Two observations, by index, that are one object with two values for
/// `type_name.property`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The target type.
    pub type_name: String,
    /// The target property.
    pub property: String,
    /// How identity was established.
    pub identity: WitnessIdentity,
    /// The first observation and its value.
    pub left: (usize, String),
    /// The second observation and its value.
    pub right: (usize, String),
}

fn relation(type_name: &str, property: &str) -> String {
    format!("{type_name}#{property}")
}

fn origin(rule: &str) -> RuleOrigin {
    RuleOrigin {
        artifact_id: None,
        pointer: format!("/{rule}"),
    }
}

/// Every divergence among `observations` under the canon `keys`, at most one
/// per `(object, property)`.
///
/// # Errors
/// A budget refusal, or [`ChaseError::Internal`] for a conflict that has no
/// two-record explanation; nothing is claimed then.
pub fn find_divergences(
    observations: &[MappedObservation],
    keys: &[CanonKey],
    budget: ChaseBudget,
) -> Result<Vec<Divergence>, ChaseError> {
    let mut objects: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut facts = Vec::new();
    let mut owner = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        let next = objects.len();
        let object = *objects
            .entry((observation.object_type.as_str(), observation.key.as_str()))
            .or_insert(next);
        for (property, value) in &observation.values {
            facts.push(Fact {
                relation: relation(&observation.object_type, property),
                values: vec![Value::Null(object), Value::Const(value.clone())],
            });
            owner.push(index);
        }
    }
    let key_rules: Vec<Dependency> = keys.iter().map(key_egd).collect();
    let properties: BTreeSet<&str> = facts.iter().map(|fact| fact.relation.as_str()).collect();
    let mut found = Vec::new();
    for property in properties {
        let mut theory = Theory {
            dependencies: key_rules.clone(),
        };
        theory.dependencies.push(functional(property));
        let mut inputs = facts.clone();
        // Kept aligned with `inputs`, so a dropped fact drops its owner.
        let mut owners = owner.clone();
        while let ChaseOutcome::Conflict(conflict) = chase(&theory, &inputs, budget)? {
            let reported: Vec<&Fact> = conflict
                .witness
                .iter()
                .filter(|fact| fact.relation == property)
                .collect();
            let &[first, second] = reported.as_slice() else {
                return Err(unexplained(property));
            };
            // Identical facts can come from several records, so try every
            // record holding each reported value. Identity must rest on the
            // pair alone: every witness fact must be one the pair holds, so a
            // chain through a third record has no pairwise witness.
            let holders = |fact: &Fact| -> Vec<(usize, usize)> {
                inputs
                    .iter()
                    .zip(&owners)
                    .enumerate()
                    .filter(|(_, (input, _))| *input == fact)
                    .map(|(index, (_, owner))| (index, *owner))
                    .collect()
            };
            let explains = |pair: [usize; 2]| {
                conflict.witness.iter().all(|fact| {
                    inputs
                        .iter()
                        .zip(&owners)
                        .any(|(input, owner)| input == fact && pair.contains(owner))
                })
            };
            let chosen = holders(first).into_iter().find_map(|(left, left_owner)| {
                holders(second)
                    .into_iter()
                    .find_map(|(right, right_owner)| {
                        (left_owner != right_owner && explains([left_owner, right_owner]))
                            .then_some((left, left_owner, right, right_owner))
                    })
            });
            let Some((left, left_owner, right, right_owner)) = chosen else {
                return Err(unexplained(property));
            };
            let (Some(left_fact), Some(right_fact)) = (inputs.get(left), inputs.get(right)) else {
                return Err(unexplained(property));
            };
            let identity = if left_fact.values.first() == right_fact.values.first() {
                WitnessIdentity::SameObjectKey
            } else {
                let (Some(a), Some(b)) =
                    (observations.get(left_owner), observations.get(right_owner))
                else {
                    return Err(unexplained(property));
                };
                let Some(key) = keys.iter().find(|key| {
                    conflict
                        .rules
                        .contains(&format!("key:{}/{}", key.type_name, key.key))
                        && key.properties.iter().all(|p| {
                            a.values
                                .get(p)
                                .is_some_and(|value| b.values.get(p) == Some(value))
                        })
                }) else {
                    return Err(unexplained(property));
                };
                WitnessIdentity::SharedKey(key.type_name.clone(), key.key.clone())
            };
            let (type_name, name) = property.split_once('#').unwrap_or((property, ""));
            let value = |fact: &Fact| match fact.values.get(1) {
                Some(Value::Const(text)) => text.clone(),
                Some(Value::Null(_)) | None => String::new(),
            };
            found.push(Divergence {
                type_name: type_name.to_owned(),
                property: name.to_owned(),
                identity,
                left: (left_owner, value(left_fact)),
                right: (right_owner, value(right_fact)),
            });
            // Drop the second value and look for the next divergence.
            inputs.remove(right);
            owners.remove(right);
        }
    }
    Ok(found)
}

/// A conflict that two observations alone do not explain (an engine bug, or
/// identity through a chain of three or more records, which a pairwise
/// witness cannot carry) fails closed rather than being dropped.
fn unexplained(property: &str) -> ChaseError {
    ChaseError::Internal(format!(
        "a conflict on {property} has no two-record witness"
    ))
}

/// One value per object: `R(o, v1), R(o, v2) → v1 = v2`.
fn functional(relation: &str) -> Dependency {
    Dependency::Egd(Egd {
        id: format!("functional:{relation}"),
        origin: origin(relation),
        body: vec![atom(relation, "o", "v1"), atom(relation, "o", "v2")],
        left: Term::Var("v1".to_owned()),
        right: Term::Var("v2".to_owned()),
    })
}

/// Equal values on every key property mean one object.
fn key_egd(key: &CanonKey) -> Dependency {
    let mut body = Vec::new();
    for (index, property) in key.properties.iter().enumerate() {
        let relation = relation(&key.type_name, property);
        body.push(atom(&relation, "o1", &format!("k{index}")));
        body.push(atom(&relation, "o2", &format!("k{index}")));
    }
    let id = format!("key:{}/{}", key.type_name, key.key);
    Dependency::Egd(Egd {
        origin: origin(&id),
        id,
        body,
        left: Term::Var("o1".to_owned()),
        right: Term::Var("o2".to_owned()),
    })
}

fn atom(relation: &str, object: &str, value: &str) -> Atom {
    Atom {
        relation: relation.to_owned(),
        terms: vec![Term::Var(object.to_owned()), Term::Var(value.to_owned())],
    }
}
