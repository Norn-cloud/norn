//! Versioned, declared definition metadata, projected without semantic checking.
//!
//! These are not effective/admitted definitions: no inheritance, expression,
//! authorization, or law interpretation happens here. Missing rows never prove
//! absence. Consumers must account for [`Projection::incomplete`] and obtain
//! admission separately. Every tuple starts with its exact envelope scope.

use serde::{Deserialize, Serialize};

use crate::access::PolicyRoot;
use crate::action::WriteChange;
use crate::mapping::MappingIr;
use crate::verification::LawIR;
use crate::{ActionEffectProgram, ActionRoot, ObjectTypeDef, ScopeRef};

/// Projection schema version, independent of source artifact versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Version {
    /// Initial declared metadata schema.
    #[serde(rename = "norn.ir.facts.v1")]
    V1,
}

/// Finite constants shared with the Datalog AST. No executable expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Value {
    /// Verbatim declared identifier or metadata text.
    Text(String),
    /// A declared Boolean flag.
    Bool(bool),
    /// Exact envelope scope; absent dimensions remain absent, never wildcards.
    Scope(ScopeRef),
}

/// Closed predicate vocabulary; arities include the leading scope column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    /// `(scope, type_name)`.
    Type,
    /// `(scope, child, declared_parent)`; no transitive closure.
    Extends,
    /// `(scope, owner, member, declared_kind, nullable, computed)`.
    Field,
    /// `(scope, owner, member, target, to, from, required, acyclic)`.
    Link,
    /// `(scope, owner, property, transition_id, from, to)`; guards not evaluated.
    Transition,
    /// `(scope, action_id, entity_kind, change)`; entity-level declaration only.
    Writes,
    /// `(scope, action_id, object_kind, lifecycle_property)`; transition effect only.
    WritesField,
    /// `(scope, map_id, source_dataset, target_entity, target_interface)`.
    Maps,
    /// `(scope, policy_id)`; no derived permission facts.
    Policy,
    /// `(scope, law_id, subject)`; no derived prohibition facts.
    Law,
}

impl Predicate {
    /// Stable relation name consumed by shared rule packs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::Extends => "extends",
            Self::Field => "field",
            Self::Link => "link",
            Self::Transition => "transition",
            Self::Writes => "writes",
            Self::WritesField => "writes_field",
            Self::Maps => "maps",
            Self::Policy => "policy",
            Self::Law => "law",
        }
    }

    /// Tuple width in this version; data schema, not semantic validation.
    #[must_use]
    pub const fn arity(self) -> usize {
        match self {
            Self::Type | Self::Policy => 2,
            Self::Extends | Self::Law => 3,
            Self::Writes | Self::WritesField => 4,
            Self::Maps => 5,
            Self::Field | Self::Transition => 6,
            Self::Link => 8,
        }
    }
}

/// A definition tuple. Derivation witnesses are never part of this identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fact {
    /// Relation in the versioned schema.
    pub predicate: Predicate,
    /// Positional constants, with the exact envelope scope first.
    pub values: Vec<Value>,
}

/// Source identity retained alongside the tuples, without inventing admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    /// Exact source envelope IR version supplied by the caller.
    pub ir_version: String,
    /// Exact artifact identity supplied by the caller.
    pub artifact_id: String,
    /// Exact artifact version supplied by the caller.
    pub artifact_version: String,
    /// Exact envelope scope, including absent dimensions.
    pub scope: ScopeRef,
}

/// The only basis this projection can claim. Admission is external evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// Syntactic metadata only, possibly from an unadmitted definition.
    Declared,
}

/// Explicitly unsupported semantic coverage. Never interpret these as success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gap {
    /// No base registry, extensions, interfaces, or inherited effective closure.
    EffectiveDefinitions,
    /// No record counts or record graph edges, so no record cardinality checks.
    RecordCardinality,
    /// Typed IR has no general disjoint-type declaration in this projection.
    TypeDisjointness,
    /// `role_can` cannot be inferred from roles without all policy conjuncts.
    RoleCan,
    /// `law_forbids` requires scoped temporal/predicate interpretation.
    LawForbids,
    /// Guards and expression satisfiability are not interpreted.
    Expressions,
    /// Field write inventories are incomplete for general effects/commands.
    FieldWrites,
    /// Mapping conditions, transforms, joins, and exact/lossless claims.
    MappingSemantics,
}

/// One source's declared facts and explicit semantic coverage gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Projection {
    /// Wire schema.
    pub version: Version,
    /// This function never asserts admission or an effective model.
    pub basis: Basis,
    /// Unmodified source identity and scope.
    pub source: Source,
    /// Metadata in declaration order, preserving names and duplicates.
    pub facts: Vec<Fact>,
    /// Consumers must refuse conclusions requiring any missing coverage.
    pub incomplete: Vec<Gap>,
}

/// Already typed source; decoding/admission remains the caller's responsibility.
#[derive(Debug, Clone, Copy)]
pub enum Definition<'a> {
    /// An object declaration, without inherited members.
    ObjectType(&'a ObjectTypeDef),
    /// Declared action writes and the exact transition-effect field, if any.
    Action(&'a ActionRoot),
    /// Declared map endpoints, without evaluating transformations.
    Mapping(&'a MappingIr),
    /// Policy identity only; no authorization inference.
    Policy(&'a PolicyRoot),
    /// Law identity and subject only; no prohibition inference.
    Law(&'a LawIR),
}

/// Project available typed metadata. This is a structural transformation only:
/// invalid names, duplicate members, dangling references, and scopes survive.
#[must_use]
pub fn project(source: Source, definition: Definition<'_>) -> Projection {
    let mut projection = Projection {
        version: Version::V1,
        basis: Basis::Declared,
        source,
        facts: Vec::new(),
        incomplete: vec![
            Gap::EffectiveDefinitions,
            Gap::RecordCardinality,
            Gap::TypeDisjointness,
            Gap::RoleCan,
            Gap::LawForbids,
            Gap::Expressions,
            Gap::FieldWrites,
            Gap::MappingSemantics,
        ],
    };
    match definition {
        Definition::ObjectType(object) => projection.object(object),
        Definition::Action(action) => {
            for write in &action.writes {
                let change = match write.change {
                    WriteChange::Created => "created",
                    WriteChange::Modified => "modified",
                    WriteChange::Deleted => "deleted",
                };
                projection.push(
                    Predicate::Writes,
                    vec![
                        text(&action.action_id),
                        text(&write.entity_kind),
                        text(change),
                    ],
                );
            }
            if let Some(ActionEffectProgram::FireTransition {
                object_kind,
                lifecycle,
                ..
            }) = &action.effect_program
            {
                projection.push(
                    Predicate::WritesField,
                    vec![
                        text(&action.action_id),
                        text(object_kind),
                        text(lifecycle.as_str()),
                    ],
                );
            }
        }
        Definition::Mapping(mapping) => {
            for map in &mapping.maps {
                projection.push(
                    Predicate::Maps,
                    vec![
                        text(&map.id),
                        text(&map.source.dataset),
                        text(&map.target.entity),
                        text(&map.target.interface),
                    ],
                );
            }
        }
        Definition::Policy(policy) => {
            let id = match policy {
                PolicyRoot::Access(policy) => &policy.policy_id,
                PolicyRoot::Approval(policy) => &policy.policy_id,
                PolicyRoot::Obligations(policy) => &policy.policy_id,
            };
            projection.push(Predicate::Policy, vec![text(id)]);
        }
        Definition::Law(law) => {
            projection.push(Predicate::Law, vec![text(&law.id), text(&law.subject)])
        }
    }
    projection
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

impl Projection {
    fn push(&mut self, predicate: Predicate, values: Vec<Value>) {
        let mut scoped = vec![Value::Scope(self.source.scope.clone())];
        scoped.extend(values);
        self.facts.push(Fact {
            predicate,
            values: scoped,
        });
    }

    fn object(&mut self, object: &ObjectTypeDef) {
        let owner = object.name.as_str();
        self.push(Predicate::Type, vec![text(owner)]);
        if let Some(parent) = &object.extends {
            self.push(Predicate::Extends, vec![text(owner), text(parent.as_str())]);
        }
        for field in &object.properties {
            self.push(
                Predicate::Field,
                vec![
                    text(owner),
                    text(field.id.as_str()),
                    text(field.property_type.kind().as_str()),
                    Value::Bool(field.constraints.nullable),
                    Value::Bool(field.computed.is_some()),
                ],
            );
        }
        for link in &object.links {
            let multiplicity = |value| match value {
                crate::Multiplicity::One => text("one"),
                crate::Multiplicity::Many => text("many"),
            };
            self.push(
                Predicate::Link,
                vec![
                    text(owner),
                    text(link.id.as_str()),
                    text(link.target.as_str()),
                    multiplicity(link.cardinality.to),
                    multiplicity(link.cardinality.from),
                    Value::Bool(link.cardinality.required),
                    Value::Bool(link.acyclic),
                ],
            );
        }
        for lifecycle in &object.lifecycles {
            for transition in &lifecycle.transitions {
                for from in &transition.from {
                    self.push(
                        Predicate::Transition,
                        vec![
                            text(owner),
                            text(lifecycle.property.as_str()),
                            text(transition.id.as_str()),
                            text(from),
                            text(&transition.to),
                        ],
                    );
                }
            }
        }
    }
}
