//! The decision-result commit path (T2-KER-2b, ruling 2026-09-25 §3, §5).
//!
//! A `decide` graph returns a [`DecisionOutput`]: canonical
//! [`FactChange`]s, command records, and timer records. This module turns it
//! into **one** [`CellCommit`] — fact writes, ledger [`FactEvent`]s, command
//! enqueue, timer enqueue, and consumption of the triggering delivery — and
//! hands it to the [`CellHost`]. Nothing is applied unless all of it is.
//!
//! [`prepare_decision`] is pure: it resolves every change against the slice
//! the decision read, in output order, over a working copy:
//!
//! - **`create`** requires the object to be absent; every effective lifecycle
//!   property it writes (or the property's default, when omitted) must be an
//!   initial state.
//! - **`set`** requires the object to exist and never writes a lifecycle
//!   property (that is what `transition` is for): any property a lifecycle
//!   is declared over along the lineage, resolved or not.
//! - **Fail closed.** A lifecycle declared over a property whose states do
//!   not resolve here (properties come from the lineage and from
//!   [`TypeCatalog::with_extensions`]) refuses every `create` of the type and
//!   every `transition` of that property (`decision.unresolved_lifecycle`).
//! - **`delete`** requires the object to exist.
//! - **`transition`** resolves the type's effective lifecycle of the named
//!   property (`norn_ir::effective_lifecycles`, the machine the checker
//!   validates: inherited lifecycles restricted to the type's narrowed
//!   `enum`, redeclarations with conjoined guards), finds
//!   the declared transition, verifies the object's current state is one of
//!   its `from` states (`norn_ir::Lifecycle::fire`, the per-call semantics
//!   `norn-checker` uses for `fire_transition`, with the same codes: a repeat
//!   firing from the target state is refused too), and evaluates every guard with `norn-expr` against the
//!   object's typed state (`self`) immediately before the change. A guard that
//!   is false, errors, or is not boolean refuses the turn (fail closed).
//!
//! Every object the decision read and did not write becomes a
//! compare-and-swap read expectation, and every written object a write
//! guarded by the version the decision read, so [`commit_decision`] refuses a
//! decision whose slice changed underneath it (`decision.stale_state`).
//!
//! Every refusal is a [`DecisionRefusal`] with a stable [`code`] and a
//! plain-language explanation (its `Display`), and commits nothing.
//!
//! [`code`]: DecisionRefusal::code

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use norn_checker::entity_model::{EntityContext, EntityModel};
use norn_checker::property_codec::{self, PropertyCodecError};
use norn_checker::{
    EffectiveKind, EffectiveLink, EffectiveObjectType, EffectiveProperty, effective_object_types,
};
use norn_expr::{EvaluationBudget, Expression, Value as ExprValue};
use norn_ir::{
    ConstraintClass, DataQualityOutcome, DataQualityRuleRef, DecisionOutput, FactChange,
    FiringRefusal, InterfaceDef, Invariant, InvariantId, InvariantOwner, Lifecycle, ObjectTypeDef,
    ObjectTypeExtension, ObjectTypeName, PropertyId, PropertyKind, PropertyType, PropertyTypeDef,
    RuntimeValue, RuntimeValueError, TIMER_AFTER_MILLIS_MEMBER, TIMER_ID_MEMBER, Transition,
    TransitionId, declared_lifecycle_properties, effective_lifecycles, lifecycle_codes,
};
use serde::{Deserialize, Serialize};

use crate::cell_host::{
    CellCommit, CellHost, CommitError, CommitReceipt, Consumed, FactExpectation, FactKey,
    FactMutation, FactState, FactVersion, FactWrite, JsonObject, JsonValue,
};
use crate::effect::{
    Command, CommandId, Effect, FreshnessPolicy, Generation, IdempotencyKey, Timer, TimerId,
};
use crate::envelope::{EnvelopeCounts, TurnEnvelopeMeter, check_decision_envelope};
use crate::limits::EnvelopeLimits;
use crate::observations::{LogicalTime, World};

/// The object types a cell knows, for resolving effective lifecycles and
/// property defaults along `extends`.
#[derive(Debug, Clone, Default)]
pub struct TypeCatalog<'a> {
    types: BTreeMap<&'a str, &'a ObjectTypeDef>,
    interfaces: BTreeMap<&'a str, &'a InterfaceDef>,
    extensions: Vec<&'a ObjectTypeExtension>,
    property_types: Option<&'a [PropertyTypeDef]>,
    effective_model: OnceLock<EffectiveModel>,
}

/// The checker-derived effective object and link contracts for one catalog.
#[derive(Debug, Clone, Default)]
struct EffectiveModel {
    object_types: BTreeMap<ObjectTypeName, EffectiveObjectType>,
    links: BTreeMap<ObjectTypeName, Vec<EffectiveLink>>,
}

/// The server-resolved classification of a pinned data-quality rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataQualityRuleClassification {
    /// A matching invariant rejects violating commands.
    Integrity,
    /// A matching invariant records violations without rejecting commands.
    Quality,
    /// A supported, version-matched platform rule.
    Builtin,
    /// The pinned declaration or built-in version could not be verified.
    Unverified,
}

impl<'a> TypeCatalog<'a> {
    /// A catalog over `types` (an admitted artifact set's object types).
    #[must_use]
    pub fn new(types: impl IntoIterator<Item = &'a ObjectTypeDef>) -> Self {
        Self {
            types: types
                .into_iter()
                .map(|object_type| (object_type.name.as_str(), object_type))
                .collect(),
            interfaces: BTreeMap::new(),
            extensions: Vec::new(),
            property_types: None,
            effective_model: OnceLock::new(),
        }
    }

    /// The same catalog with the admitted interfaces implemented by its
    /// object types. Interface invariants participate in every effective
    /// object's invariant set.
    #[must_use]
    pub fn with_interfaces(
        mut self,
        interfaces: impl IntoIterator<Item = &'a InterfaceDef>,
    ) -> Self {
        self.interfaces.extend(
            interfaces
                .into_iter()
                .map(|interface| (interface.name.as_str(), interface)),
        );
        self.effective_model.take();
        self
    }

    /// The same catalog with `extensions` (the artifact set's object-type
    /// extensions): their properties apply to the target and its subtypes,
    /// so a lifecycle over an extension property is enforced too.
    #[must_use]
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = &'a ObjectTypeExtension>,
    ) -> Self {
        self.extensions.extend(extensions);
        self.effective_model.take();
        self
    }

    /// The same catalog with the admitted shared property-type definitions
    /// used to resolve object properties declared with `ref`.
    #[must_use]
    pub fn with_property_types(mut self, property_types: &'a [PropertyTypeDef]) -> Self {
        self.property_types = Some(property_types);
        self.effective_model.take();
        self
    }

    /// Build the complete stored value of an object after a put: preserve
    /// submitted values and fill each effective stored property with its
    /// declared default or `null`. Computed properties are not materialized.
    ///
    /// A property's nearest declaration on the type lineage is effective,
    /// while its inherited default remains effective when the override omits
    /// one. Submitted typed values and defaults are round-tripped through the
    /// same scalar codec as cell storage. Extensions add their namespaced
    /// properties to the target and its subtypes.
    ///
    /// # Errors
    ///
    /// Returns [`EffectiveValuesError::Runtime`] when a submitted typed value
    /// or effective default cannot be represented as an exact runtime value,
    /// and [`EffectiveValuesError::ValueType`] when its scalar encoding does
    /// not match the effective property. Valid timestamps are canonicalized
    /// here so guards and persistence see the same UTC string.
    pub fn effective_object_values(
        &self,
        object_type: &ObjectTypeName,
        submitted: &JsonObject,
    ) -> Result<JsonObject, EffectiveValuesError> {
        let mut values = submitted.clone();
        let Some(effective) = self.effective_model().object_types.get(object_type) else {
            return Ok(values);
        };
        for property in &effective.properties {
            if property.computed {
                continue;
            }
            let name = property.id.as_str();
            let value = match submitted.get(name) {
                Some(value) => value.clone(),
                None => property
                    .default
                    .as_ref()
                    .map(|default| property.runtime_default_literal(&default.0))
                    .transpose()?
                    .unwrap_or(RuntimeValue::Null),
            };
            let kind = effective_property_kind(property);
            let encoded = property_codec::encode(kind, &value)
                .map_err(|error| property_codec_error(name, error))?;
            let value = property_codec::decode(kind, &encoded)
                .map_err(|error| property_codec_error(name, error))?;
            let value = property.canonical_runtime_value(&value);
            values.insert(name.to_owned(), value);
        }
        Ok(values)
    }

    /// Materialize declared defaults without filling other absent properties
    /// with `null` or validating caller values. Owned-row expressions run
    /// before final object validation, so a required property produced by a
    /// derivation must remain absent until that derivation has run, and a
    /// derived property may replace an invalid caller-supplied value.
    pub(crate) fn materialize_object_defaults(
        &self,
        object_type: &ObjectTypeName,
        submitted: &JsonObject,
    ) -> Result<JsonObject, EffectiveValuesError> {
        let mut values = submitted.clone();
        let Some(effective) = self.effective_model().object_types.get(object_type) else {
            return Ok(values);
        };
        for property in &effective.properties {
            if property.computed || submitted.contains_key(property.id.as_str()) {
                continue;
            }
            let name = property.id.as_str();
            let Some(default) = &property.default else {
                continue;
            };
            let value = property.runtime_default_literal(&default.0)?;
            let kind = effective_property_kind(property);
            let encoded = property_codec::encode(kind, &value)
                .map_err(|error| property_codec_error(name, error))?;
            let value = property_codec::decode(kind, &encoded)
                .map_err(|error| property_codec_error(name, error))?;
            values.insert(name.to_owned(), property.canonical_runtime_value(&value));
        }
        Ok(values)
    }

    /// Build the complete stored values for one effective link instance:
    /// preserve submitted values and fill each effective stored property with
    /// its declared default or `null`. Computed properties are omitted.
    /// Temporal values are checked against their declared type and valid
    /// timestamps are canonicalized to UTC.
    ///
    /// An unknown source type or link returns `submitted` unchanged; callers
    /// that admit link facts must fail closed when the shared shape is absent.
    ///
    /// # Errors
    ///
    /// Returns [`EffectiveValuesError::Runtime`] when a submitted typed value
    /// or effective default cannot be represented as an exact runtime value,
    /// and [`EffectiveValuesError::ValueType`] when its scalar encoding or
    /// temporal form does not match the effective property.
    pub fn effective_link_values(
        &self,
        source: &ObjectTypeName,
        link: &str,
        submitted: &JsonObject,
    ) -> Result<JsonObject, EffectiveValuesError> {
        let Some(effective_link) = self
            .effective_model()
            .links
            .get(source)
            .and_then(|links| links.iter().find(|candidate| candidate.id.as_str() == link))
        else {
            return Ok(submitted.clone());
        };

        let mut values = submitted.clone();
        for property in &effective_link.properties {
            if property.computed {
                continue;
            }
            let name = property.id.as_str();
            let value = match submitted.get(name) {
                Some(value) => value.clone(),
                None => property
                    .default
                    .as_ref()
                    .map(|default| property.runtime_default_literal(&default.0))
                    .transpose()?
                    .unwrap_or(RuntimeValue::Null),
            };
            let kind = effective_property_kind(property);
            let encoded = property_codec::encode(kind, &value)
                .map_err(|error| property_codec_error(name, error))?;
            let value = property_codec::decode(kind, &encoded)
                .map_err(|error| property_codec_error(name, error))?;
            if let Some(expected) = temporal_property_expected(property) {
                property.check_runtime_value(&value).map_err(|_error| {
                    EffectiveValuesError::ValueType {
                        property: name.to_owned(),
                        expected,
                    }
                })?;
            }
            let value = property.canonical_runtime_value(&value);
            values.insert(name.to_owned(), value);
        }
        Ok(values)
    }

    /// Whether `property` has an effective integer or decimal type on
    /// `object_type`, including through a resolved shared-property reference.
    #[must_use]
    pub fn is_numeric_property(&self, object_type: &ObjectTypeName, property: &str) -> bool {
        self.effective_type(object_type)
            .and_then(|object_type| object_type.property(property))
            .is_some_and(|property| {
                matches!(
                    &property.kind,
                    EffectiveKind::Base(PropertyType::Integer {} | PropertyType::Decimal { .. })
                )
            })
    }

    fn effective_property(
        &self,
        object_type: &ObjectTypeName,
        property: &PropertyId,
    ) -> Option<&EffectiveProperty> {
        self.effective_type(object_type)
            .and_then(|object_type| object_type.property(property.as_str()))
    }

    /// The effective object type in this catalog, if it resolves.
    pub(crate) fn effective_type(
        &self,
        object_type: &ObjectTypeName,
    ) -> Option<&EffectiveObjectType> {
        self.effective_model().object_types.get(object_type)
    }

    fn effective_model(&self) -> &EffectiveModel {
        self.effective_model.get_or_init(|| {
            let object_types: Vec<_> = self
                .types
                .values()
                .map(|object_type| (**object_type).clone())
                .collect();
            let interfaces: Vec<_> = self
                .interfaces
                .values()
                .map(|interface| (**interface).clone())
                .collect();
            let extensions: Vec<_> = self
                .extensions
                .iter()
                .map(|extension| (**extension).clone())
                .collect();
            let property_types = self.property_types.unwrap_or_default().to_vec();
            let registry = BTreeSet::new();
            let model = EntityModel {
                interfaces: &interfaces,
                object_types: &object_types,
                extensions: &extensions,
            };
            let context = EntityContext {
                catalog: &property_types,
                registry: &registry,
                base: EntityModel::EMPTY,
                tenant: None,
            };
            let mut effective_model = EffectiveModel::default();
            for (_, (object_type, links)) in effective_object_types(&model, &context) {
                let name = object_type.name.clone();
                effective_model
                    .object_types
                    .insert(name.clone(), object_type);
                effective_model.links.insert(name, links);
            }
            effective_model
        })
    }

    /// Canonicalize one present temporal property value while preserving
    /// untyped or computed values and invalid temporal source strings.
    fn canonicalize_working_temporal_value(
        &self,
        object_type: &ObjectTypeName,
        property_name: &str,
        value: &JsonValue,
    ) -> JsonValue {
        self.effective_type(object_type)
            .and_then(|effective| effective.property(property_name))
            .filter(|property| !property.computed && is_temporal_property(property))
            .map_or_else(
                || value.clone(),
                |property| property.canonical_runtime_value(value),
            )
    }

    /// Whether `object_type` is known.
    #[must_use]
    pub fn contains(&self, object_type: &ObjectTypeName) -> bool {
        self.types.contains_key(object_type.as_str())
    }

    /// Whether `object_type` has effective invariants of its own, from a
    /// supertype, or from an implemented interface, or has stored temporal
    /// properties that require implicit validity checks.
    ///
    /// # Errors
    ///
    /// Returns the name of an interface referenced by the object's lineage
    /// that is absent from this catalog.
    pub fn has_invariants(&self, object_type: &ObjectTypeName) -> Result<bool, ObjectTypeName> {
        let declared = !self.invariants(object_type)?.is_empty();
        let temporal = self.effective_type(object_type).is_some_and(|object_type| {
            object_type
                .properties
                .iter()
                .any(|property| !property.computed && is_temporal_property(property))
        });
        Ok(declared || temporal)
    }

    /// Whether the type or any ancestor is append-only.
    pub(crate) fn immutable(&self, object_type: &ObjectTypeName) -> bool {
        self.ancestry(object_type)
            .iter()
            .any(|definition| definition.immutable)
    }

    /// `object_type` and then its supertypes, nearest first. Bounded by the
    /// catalog size, so a cyclic `extends` chain (which the checker rejects)
    /// cannot loop.
    pub(crate) fn ancestry(&self, object_type: &ObjectTypeName) -> Vec<&'a ObjectTypeDef> {
        let mut chain = Vec::new();
        let mut next = self.types.get(object_type.as_str()).copied();
        while let Some(current) = next {
            if chain.len() >= self.types.len() {
                break;
            }
            chain.push(current);
            next = current
                .extends
                .as_ref()
                .and_then(|parent| self.types.get(parent.as_str()).copied());
        }
        chain
    }

    /// All interface contracts held by this type and its object ancestors.
    pub(crate) fn implements(&self, object_type: &ObjectTypeName, interface: &str) -> bool {
        self.ancestry(object_type)
            .iter()
            .flat_map(|definition| &definition.implements)
            .any(|name| {
                name.as_str() == interface
                    || self
                        .interfaces
                        .get(name.as_str())
                        .is_some_and(|definition| {
                            definition
                                .contract_owners(|parent| {
                                    self.interfaces.get(parent.as_str()).copied()
                                })
                                .iter()
                                .any(|parent| parent.name.as_str() == interface)
                        })
            })
    }

    /// Every invariant that applies to `object_type`, including its own,
    /// inherited declarations, and implemented interfaces. Owner and id
    /// form the stable identity; ordering is independent of declaration
    /// artifact order.
    pub(crate) fn invariants(
        &self,
        object_type: &ObjectTypeName,
    ) -> Result<Vec<EffectiveInvariant<'a>>, ObjectTypeName> {
        let mut invariants = BTreeMap::new();
        for def in self.ancestry(object_type) {
            for invariant in &def.invariants {
                invariants.insert(
                    (
                        InvariantOwner::Object(def.name.clone()),
                        invariant.id.clone(),
                    ),
                    invariant,
                );
            }
            for interface_name in &def.implements {
                let Some(interface) = self.interfaces.get(interface_name.as_str()).copied() else {
                    return Err(ObjectTypeName(interface_name.as_str().to_owned()));
                };
                let lineage = norn_expr::env::interface_lineage(interface, |name| {
                    self.interfaces.get(name.as_str()).copied()
                })
                .map_err(|name| ObjectTypeName(name.0))?;
                for owner in lineage {
                    for invariant in &owner.invariants {
                        invariants.insert(
                            (
                                InvariantOwner::Interface(owner.name.clone()),
                                invariant.id.clone(),
                            ),
                            invariant,
                        );
                    }
                }
            }
        }
        Ok(invariants
            .into_iter()
            .map(|((owner, id), invariant)| EffectiveInvariant {
                owner,
                id,
                invariant,
            })
            .collect())
    }

    /// Pin an invariant owner to its canonical `EntityIR` declaration content.
    ///
    /// Artifact envelopes are not retained by this kernel catalog, so findings
    /// use a Norn1 digest of the owner definition as their rule version.
    ///
    /// # Errors
    /// Canonical serialization of the typed owner definition failed.
    pub fn invariant_owner_version(
        &self,
        owner: &InvariantOwner,
    ) -> Result<Option<String>, norn_ir::canonical::CanonicalJsonError> {
        let canonical = match owner {
            InvariantOwner::Object(name) => self
                .types
                .get(name.as_str())
                .map(|definition| norn_ir::canonical::canonical_json_of(*definition))
                .transpose()?,
            InvariantOwner::Interface(name) => self
                .interfaces
                .get(name.as_str())
                .map(|definition| norn_ir::canonical::canonical_json_of(*definition))
                .transpose()?,
        };
        Ok(canonical.map(|bytes| {
            format!(
                "sha256:{}",
                norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
            )
        }))
    }

    /// Resolve a finding's classification only when its pinned rule reference
    /// matches the typed owner definition in this catalog.
    ///
    /// Built-in identifiers are versioned contracts too: unsupported ids or
    /// versions remain unverified. Invariant references require both an exact
    /// owner digest match and an invariant with the referenced id.
    ///
    /// # Errors
    /// Canonical serialization of an owner definition failed.
    pub fn classify_data_quality_rule(
        &self,
        rule_ref: &DataQualityRuleRef,
    ) -> Result<DataQualityRuleClassification, norn_ir::canonical::CanonicalJsonError> {
        match rule_ref {
            DataQualityRuleRef::Builtin { id, version } => {
                Ok(if is_supported_builtin_rule(id, version) {
                    DataQualityRuleClassification::Builtin
                } else {
                    DataQualityRuleClassification::Unverified
                })
            }
            DataQualityRuleRef::Invariant {
                owner,
                invariant_id,
                version,
            } => {
                let Some(owner_version) = self.invariant_owner_version(owner)? else {
                    return Ok(DataQualityRuleClassification::Unverified);
                };
                if owner_version != *version {
                    return Ok(DataQualityRuleClassification::Unverified);
                }
                let invariant = match owner {
                    InvariantOwner::Object(name) => {
                        self.types.get(name.as_str()).and_then(|definition| {
                            definition
                                .invariants
                                .iter()
                                .find(|invariant| invariant.id == *invariant_id)
                        })
                    }
                    InvariantOwner::Interface(name) => {
                        self.interfaces.get(name.as_str()).and_then(|definition| {
                            definition
                                .invariants
                                .iter()
                                .find(|invariant| invariant.id == *invariant_id)
                        })
                    }
                };
                Ok(match invariant.map(|invariant| invariant.classification) {
                    Some(ConstraintClass::Integrity) => DataQualityRuleClassification::Integrity,
                    Some(ConstraintClass::Quality) => DataQualityRuleClassification::Quality,
                    None => DataQualityRuleClassification::Unverified,
                })
            }
        }
    }

    /// `object_type` and then its supertypes, nearest first (empty when the
    /// type is unknown).
    #[must_use]
    pub fn lineage(&self, object_type: &ObjectTypeName) -> Vec<ObjectTypeName> {
        self.ancestry(object_type)
            .into_iter()
            .map(|def| def.name.clone())
            .collect()
    }

    /// Effective owned collections declared by `object_type` or an ancestor,
    /// nearest declaration first with unique ids. Admission rejects redeclared
    /// ids; first-wins keeps malformed unadmitted catalogs deterministic.
    pub(crate) fn owned_collections(
        &self,
        object_type: &ObjectTypeName,
    ) -> Vec<&'a norn_ir::OwnedChildCollection> {
        let mut seen = BTreeSet::new();
        self.ancestry(object_type)
            .into_iter()
            .flat_map(|definition| &definition.owned_collections)
            .filter(|collection| seen.insert(collection.id.as_str().to_owned()))
            .collect()
    }

    /// Whether `object_type` is the child type of any declared owned collection.
    pub(crate) fn is_owned_child_type(&self, object_type: &ObjectTypeName) -> bool {
        self.types.values().any(|parent| {
            parent.owned_collections.iter().any(|collection| {
                self.ancestry(object_type)
                    .iter()
                    .any(|ancestor| ancestor.name == collection.child_type)
            })
        })
    }

    /// Every effective lifecycle of `object_type`, by property: the state
    /// machine `norn-checker` validates against
    /// ([`norn_ir::effective_lifecycles`]: inherited lifecycles restricted to
    /// the type's narrowed `enum`, redeclarations with conjoined guards).
    #[must_use]
    pub fn lifecycles(&self, object_type: &ObjectTypeName) -> BTreeMap<PropertyId, Lifecycle> {
        let lookup = |name: &str| self.types.get(name).copied();
        effective_lifecycles(&lookup, &self.extensions, object_type.as_str())
    }

    /// Every property a lifecycle is declared over along `object_type`'s
    /// lineage ([`norn_ir::declared_lifecycle_properties`]), resolved or not.
    /// Such a property is never written directly, and one without an
    /// effective lifecycle refuses every create and transition (fail closed).
    #[must_use]
    pub fn lifecycle_properties(&self, object_type: &ObjectTypeName) -> BTreeSet<PropertyId> {
        let lookup = |name: &str| self.types.get(name).copied();
        declared_lifecycle_properties(&lookup, object_type.as_str())
    }

    /// The effective lifecycle of `property` on `object_type`.
    #[must_use]
    pub fn lifecycle(
        &self,
        object_type: &ObjectTypeName,
        property: &PropertyId,
    ) -> Option<Lifecycle> {
        self.lifecycles(object_type).remove(property)
    }

    /// The first immutable lifecycle state held by this object, if any.
    pub(crate) fn immutable_state(
        &self,
        object_type: &ObjectTypeName,
        values: &JsonObject,
    ) -> Option<(PropertyId, String)> {
        self.lifecycles(object_type)
            .into_values()
            .find_map(|lifecycle| {
                let state = values
                    .get(lifecycle.property.as_str())
                    .and_then(JsonValue::as_str)?;
                lifecycle
                    .immutable
                    .iter()
                    .any(|immutable| immutable == state)
                    .then(|| (lifecycle.property, state.to_owned()))
            })
    }

    /// The effective default of `property` on `object_type`, if declared.
    fn default_of(
        &self,
        object_type: &ObjectTypeName,
        property: &PropertyId,
    ) -> Option<&norn_ir::JsonValue> {
        self.effective_property(object_type, property)
            .and_then(|property| property.default.as_ref())
            .map(|default| &default.0)
    }
}

fn is_supported_builtin_rule(id: &str, version: &str) -> bool {
    version == "1"
        && matches!(
            id,
            "norn.temporal.property_validity" | "norn.lifecycle.observed_out_of_band"
        )
}

fn effective_property_kind(property: &EffectiveProperty) -> PropertyKind {
    match &property.kind {
        EffectiveKind::Base(property_type) => property_type.kind(),
        EffectiveKind::Registry(_) | EffectiveKind::Unresolved => PropertyKind::Ref,
    }
}

fn property_codec_error(property: &str, error: PropertyCodecError) -> EffectiveValuesError {
    match error {
        PropertyCodecError::Runtime(error) => EffectiveValuesError::Runtime(error),
        PropertyCodecError::ValueType(expected) => EffectiveValuesError::ValueType {
            property: property.to_owned(),
            expected,
        },
        PropertyCodecError::Corrupt => {
            EffectiveValuesError::Runtime(RuntimeValueError::InvalidJson {
                message: format!(
                    "effective property {property:?} did not round-trip through storage"
                ),
            })
        }
    }
}

/// One invariant together with the declaration that owns its identity.
#[derive(Debug, Clone)]
pub(crate) struct EffectiveInvariant<'a> {
    pub(crate) owner: InvariantOwner,
    pub(crate) id: InvariantId,
    pub(crate) invariant: &'a Invariant,
}

/// The objects a decision read, with the versions it read them at. An object
/// it read as absent is present with [`FactVersion::ABSENT`] (or its tombstone
/// version) and no values.
pub type FactSlice = BTreeMap<FactKey, FactState>;

/// Reads `keys` from `host` into a [`FactSlice`].
///
/// # Errors
///
/// The host's read failure.
pub fn load_slice<H: CellHost + ?Sized>(
    host: &H,
    keys: impl IntoIterator<Item = FactKey>,
) -> Result<FactSlice, H::Error> {
    keys.into_iter()
        .map(|key| host.load(&key).map(|state| (key, state)))
        .collect()
}

/// The turn a decision result is committed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionTurn {
    /// Stable identity of the turn. Command ids, idempotency keys, and default
    /// timer ids derive from it, so re-preparing the same turn yields the same
    /// commit.
    pub id: String,
    /// The host's logical time for the turn; timer delays count from it.
    pub now: LogicalTime,
    /// The world the turn runs in.
    pub world: World,
    /// The delivery that triggered the turn, consumed by the commit.
    pub consumes: Option<Consumed>,
}

/// One ledger event of a committed decision: a fact change as it resolved.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FactEvent {
    /// An action committed an outbound command in the same transaction.
    /// The immutable payload is retained with the ledger event so `why` can
    /// explain the command after its outbox row has completed.
    CommandEnqueued {
        /// The command identity.
        command_id: String,
        /// The initial generation.
        generation: u64,
        /// The external idempotency key.
        idempotency_key: String,
        /// The action-call fact key that caused this command.
        action_call: String,
        /// The immutable command request.
        payload: JsonObject,
    },
    /// A create-only action allocated a business identifier and committed its
    /// counter in the same transaction as the object and action receipt.
    NamingSeriesAllocated {
        /// The newly created business object.
        key: FactKey,
        /// The internal counter fact changed by the allocation.
        counter: FactKey,
        /// The declared naming pattern.
        pattern: String,
        /// The IANA timezone used to resolve date tokens.
        timezone: String,
        /// The pinned logical clock sample used for date tokens.
        epoch_millis: u64,
        /// The timezone database release used for this allocation.
        tzdb_version: String,
        /// The declared scope tuple and its committed values.
        scope: BTreeMap<PropertyId, String>,
        /// The reset bucket (empty, `YYYY`, `YYYY-MM`, or `YYYY-MM-DD`).
        period: String,
        /// The number rendered into the generated key.
        value: u64,
        /// The declared reset policy.
        reset_policy: norn_ir::NamingResetPolicy,
        /// The declared collision behavior.
        gap_policy: norn_ir::NamingGapPolicy,
    },
    /// An imported naming-series counter was installed atomically.
    NamingSeriesCounterSeeded {
        /// The internal counter fact written.
        counter: FactKey,
        /// The declared entity type.
        object_type: ObjectTypeName,
        /// The declared key name.
        key_name: String,
        /// The declared scope values.
        scope: RuntimeValue,
        /// The reset bucket.
        period: String,
        /// The last number imported.
        current: u64,
    },
    /// The object was created with these values (defaults applied).
    Created {
        /// The object.
        key: FactKey,
        /// Its values.
        values: JsonObject,
    },
    /// One property was set.
    Set {
        /// The object.
        key: FactKey,
        /// The property.
        property: PropertyId,
        /// Its new value.
        value: JsonValue,
    },
    /// One deterministic aggregate event for a net change to an existing
    /// object's stored values. Granular `Set` and `Transitioned` events remain
    /// in the ledger for their existing consumers.
    Modified {
        /// The object.
        key: FactKey,
        /// The version before the commit.
        before_version: FactVersion,
        /// The version after the commit.
        after_version: FactVersion,
        /// The complete stored values before the commit.
        before: JsonObject,
        /// The complete stored values after the commit.
        after: JsonObject,
        /// Sorted property ids whose values differ between `before` and `after`.
        changed_properties: Vec<PropertyId>,
    },
    /// The object was deleted.
    Deleted {
        /// The object.
        key: FactKey,
    },
    /// A declared lifecycle transition fired.
    Transitioned {
        /// The object.
        key: FactKey,
        /// The lifecycle property.
        lifecycle: PropertyId,
        /// The declared transition.
        transition: TransitionId,
        /// The state it left.
        from: String,
        /// The state it entered.
        to: String,
    },
    /// A command-side mutation was refused by a declared invariant. This
    /// ledger-only event records the stable owner, id, and message; no
    /// business write, effect, or timer is part of this refusal commit.
    InvariantRefused {
        /// The object whose attempted final value failed.
        key: FactKey,
        /// The declaration that owns the invariant identity.
        owner: InvariantOwner,
        /// The invariant id within `owner`.
        id: InvariantId,
        /// The human-readable invariant message.
        message: String,
        /// Why the invariant did not evaluate to true.
        outcome: DataQualityOutcome,
        /// Stable identity of the attempted turn or action call.
        attempt_id: String,
    },
}

// Like FactChange, decode exact runtime values through a plain record. Serde's
// internally tagged enum buffering cannot deserialize their RawValue tokens.
enum FactEventOp {
    CommandEnqueued,
    NamingSeriesAllocated,
    NamingSeriesCounterSeeded,
    Created,
    Set,
    Modified,
    Deleted,
    Transitioned,
    InvariantRefused,
}

impl FactEventOp {
    fn parse<E: serde::de::Error>(op: &str) -> Result<Self, E> {
        match op {
            "command_enqueued" => Ok(Self::CommandEnqueued),
            "naming_series_allocated" => Ok(Self::NamingSeriesAllocated),
            "naming_series_counter_seeded" => Ok(Self::NamingSeriesCounterSeeded),
            "created" => Ok(Self::Created),
            "set" => Ok(Self::Set),
            "modified" => Ok(Self::Modified),
            "deleted" => Ok(Self::Deleted),
            "transitioned" => Ok(Self::Transitioned),
            "invariant_refused" => Ok(Self::InvariantRefused),
            _ => Err(E::unknown_variant(
                op,
                &[
                    "command_enqueued",
                    "naming_series_allocated",
                    "naming_series_counter_seeded",
                    "created",
                    "set",
                    "modified",
                    "deleted",
                    "transitioned",
                    "invariant_refused",
                ],
            )),
        }
    }

    const fn members(&self) -> &'static [&'static str] {
        match self {
            Self::CommandEnqueued => &[
                "op",
                "commandId",
                "generation",
                "idempotencyKey",
                "actionCall",
                "payload",
            ],
            Self::NamingSeriesAllocated => &[
                "op",
                "key",
                "counter",
                "pattern",
                "timezone",
                "epochMillis",
                "tzdbVersion",
                "scope",
                "period",
                "value",
                "resetPolicy",
                "gapPolicy",
            ],
            Self::NamingSeriesCounterSeeded => &[
                "op",
                "counter",
                "objectType",
                "keyName",
                "scope",
                "period",
                "current",
            ],
            Self::Created => &["op", "key", "values"],
            Self::Set => &["op", "key", "property", "value"],
            Self::Modified => &[
                "op",
                "key",
                "beforeVersion",
                "afterVersion",
                "before",
                "after",
                "changedProperties",
            ],
            Self::Deleted => &["op", "key"],
            Self::Transitioned => &["op", "key", "lifecycle", "transition", "from", "to"],
            Self::InvariantRefused => &[
                "op",
                "key",
                "owner",
                "id",
                "message",
                "outcome",
                "attemptId",
            ],
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FactEventWire {
    op: String,
    #[serde(default, deserialize_with = "present_event_field")]
    command_id: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    generation: Option<u64>,
    #[serde(default, deserialize_with = "present_event_field")]
    idempotency_key: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    action_call: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    payload: Option<JsonObject>,
    #[serde(default, deserialize_with = "present_event_field")]
    key: Option<FactKey>,
    #[serde(default, deserialize_with = "present_event_field")]
    values: Option<JsonObject>,
    #[serde(default, deserialize_with = "present_event_field")]
    property: Option<PropertyId>,
    #[serde(default, deserialize_with = "present_event_field")]
    value: Option<JsonValue>,
    #[serde(default, deserialize_with = "present_event_field")]
    before_version: Option<FactVersion>,
    #[serde(default, deserialize_with = "present_event_field")]
    after_version: Option<FactVersion>,
    #[serde(default, deserialize_with = "present_event_field")]
    before: Option<JsonObject>,
    #[serde(default, deserialize_with = "present_event_field")]
    after: Option<JsonObject>,
    #[serde(default, deserialize_with = "present_event_field")]
    changed_properties: Option<Vec<PropertyId>>,
    #[serde(default, deserialize_with = "present_event_field")]
    lifecycle: Option<PropertyId>,
    #[serde(default, deserialize_with = "present_event_field")]
    transition: Option<TransitionId>,
    #[serde(default, deserialize_with = "present_event_field")]
    from: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    to: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    owner: Option<InvariantOwner>,
    #[serde(default, deserialize_with = "present_event_field")]
    id: Option<InvariantId>,
    #[serde(default, deserialize_with = "present_event_field")]
    message: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    outcome: Option<DataQualityOutcome>,
    #[serde(default, deserialize_with = "present_event_field")]
    attempt_id: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    counter: Option<FactKey>,
    #[serde(default, deserialize_with = "present_event_field")]
    pattern: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    timezone: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    epoch_millis: Option<u64>,
    #[serde(default, deserialize_with = "present_event_field")]
    tzdb_version: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    scope: Option<JsonValue>,
    #[serde(default, deserialize_with = "present_event_field")]
    period: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    reset_policy: Option<norn_ir::NamingResetPolicy>,
    #[serde(default, deserialize_with = "present_event_field")]
    gap_policy: Option<norn_ir::NamingGapPolicy>,
    #[serde(default, deserialize_with = "present_event_field")]
    object_type: Option<ObjectTypeName>,
    #[serde(default, deserialize_with = "present_event_field")]
    key_name: Option<String>,
    #[serde(default, deserialize_with = "present_event_field")]
    current: Option<u64>,
}

// Deserialize T directly: only omission is None. In particular, a Set value
// of null is Some(JsonValue::Null), while null for a nonnullable field fails.
fn present_event_field<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn required_event_field<T, E: serde::de::Error>(
    value: Option<T>,
    name: &'static str,
) -> Result<T, E> {
    value.ok_or_else(|| E::missing_field(name))
}

impl FactEventWire {
    fn into_allocation<E: serde::de::Error>(self) -> Result<FactEvent, E> {
        let scope = required_event_field::<_, E>(self.scope, "scope")?;
        let value = required_event_field::<_, E>(self.value, "value")?;
        Ok(FactEvent::NamingSeriesAllocated {
            key: required_event_field::<_, E>(self.key, "key")?,
            counter: required_event_field::<_, E>(self.counter, "counter")?,
            pattern: required_event_field::<_, E>(self.pattern, "pattern")?,
            timezone: required_event_field::<_, E>(self.timezone, "timezone")?,
            epoch_millis: required_event_field::<_, E>(self.epoch_millis, "epochMillis")?,
            tzdb_version: required_event_field::<_, E>(self.tzdb_version, "tzdbVersion")?,
            // These names also occur with unrestricted runtime types in Set
            // and CounterSeeded. Apply this variant's types losslessly here.
            scope: norn_ir::runtime::decode(&scope).map_err(E::custom)?,
            period: required_event_field::<_, E>(self.period, "period")?,
            value: norn_ir::runtime::decode(&value).map_err(E::custom)?,
            reset_policy: required_event_field::<_, E>(self.reset_policy, "resetPolicy")?,
            gap_policy: required_event_field::<_, E>(self.gap_policy, "gapPolicy")?,
        })
    }

    fn into_counter_seed<E: serde::de::Error>(self) -> Result<FactEvent, E> {
        Ok(FactEvent::NamingSeriesCounterSeeded {
            counter: required_event_field::<_, E>(self.counter, "counter")?,
            object_type: required_event_field::<_, E>(self.object_type, "objectType")?,
            key_name: required_event_field::<_, E>(self.key_name, "keyName")?,
            scope: required_event_field::<_, E>(self.scope, "scope")?,
            period: required_event_field::<_, E>(self.period, "period")?,
            current: required_event_field::<_, E>(self.current, "current")?,
        })
    }

    fn checked_op<E: serde::de::Error>(&self) -> Result<FactEventOp, E> {
        let op = FactEventOp::parse::<E>(&self.op)?;
        let allowed = op.members();
        let present = [
            ("counter", self.counter.is_some()),
            ("pattern", self.pattern.is_some()),
            ("timezone", self.timezone.is_some()),
            ("epochMillis", self.epoch_millis.is_some()),
            ("tzdbVersion", self.tzdb_version.is_some()),
            ("scope", self.scope.is_some()),
            ("period", self.period.is_some()),
            ("resetPolicy", self.reset_policy.is_some()),
            ("gapPolicy", self.gap_policy.is_some()),
            ("objectType", self.object_type.is_some()),
            ("keyName", self.key_name.is_some()),
            ("current", self.current.is_some()),
            ("commandId", self.command_id.is_some()),
            ("generation", self.generation.is_some()),
            ("idempotencyKey", self.idempotency_key.is_some()),
            ("actionCall", self.action_call.is_some()),
            ("payload", self.payload.is_some()),
            ("key", self.key.is_some()),
            ("values", self.values.is_some()),
            ("property", self.property.is_some()),
            ("value", self.value.is_some()),
            ("beforeVersion", self.before_version.is_some()),
            ("afterVersion", self.after_version.is_some()),
            ("before", self.before.is_some()),
            ("after", self.after.is_some()),
            ("changedProperties", self.changed_properties.is_some()),
            ("lifecycle", self.lifecycle.is_some()),
            ("transition", self.transition.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("owner", self.owner.is_some()),
            ("id", self.id.is_some()),
            ("message", self.message.is_some()),
            ("outcome", self.outcome.is_some()),
            ("attemptId", self.attempt_id.is_some()),
        ];
        if let Some((name, _)) = present
            .into_iter()
            .find(|(name, present)| *present && !allowed.contains(name))
        {
            return Err(E::unknown_field(name, allowed));
        }
        Ok(op)
    }
}

// A plain derived struct also accepts sequences. Keep FactEvent's object-only
// contract, forwarding map values directly so exact JSON tokens are retained.
struct FactEventVisitor;

impl<'de> serde::de::Visitor<'de> for FactEventVisitor {
    type Value = FactEventWire;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a ledger event object")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
        FactEventWire::deserialize(serde::de::value::MapAccessDeserializer::new(map))
    }
}

impl<'de> Deserialize<'de> for FactEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = deserializer.deserialize_map(FactEventVisitor)?;
        Ok(match wire.checked_op::<D::Error>()? {
            FactEventOp::NamingSeriesAllocated => wire.into_allocation::<D::Error>()?,
            FactEventOp::NamingSeriesCounterSeeded => wire.into_counter_seed::<D::Error>()?,
            FactEventOp::CommandEnqueued => Self::CommandEnqueued {
                command_id: required_event_field::<_, D::Error>(wire.command_id, "commandId")?,
                generation: required_event_field::<_, D::Error>(wire.generation, "generation")?,
                idempotency_key: required_event_field::<_, D::Error>(
                    wire.idempotency_key,
                    "idempotencyKey",
                )?,
                action_call: required_event_field::<_, D::Error>(wire.action_call, "actionCall")?,
                payload: required_event_field::<_, D::Error>(wire.payload, "payload")?,
            },
            FactEventOp::Created => Self::Created {
                key: required_event_field::<_, D::Error>(wire.key, "key")?,
                values: required_event_field::<_, D::Error>(wire.values, "values")?,
            },
            FactEventOp::Set => Self::Set {
                key: required_event_field::<_, D::Error>(wire.key, "key")?,
                property: required_event_field::<_, D::Error>(wire.property, "property")?,
                value: required_event_field::<_, D::Error>(wire.value, "value")?,
            },
            FactEventOp::Modified => Self::Modified {
                key: required_event_field::<_, D::Error>(wire.key, "key")?,
                before_version: required_event_field::<_, D::Error>(
                    wire.before_version,
                    "beforeVersion",
                )?,
                after_version: required_event_field::<_, D::Error>(
                    wire.after_version,
                    "afterVersion",
                )?,
                before: required_event_field::<_, D::Error>(wire.before, "before")?,
                after: required_event_field::<_, D::Error>(wire.after, "after")?,
                changed_properties: required_event_field::<_, D::Error>(
                    wire.changed_properties,
                    "changedProperties",
                )?,
            },
            FactEventOp::Deleted => Self::Deleted {
                key: required_event_field::<_, D::Error>(wire.key, "key")?,
            },
            FactEventOp::Transitioned => Self::Transitioned {
                key: required_event_field::<_, D::Error>(wire.key, "key")?,
                lifecycle: required_event_field::<_, D::Error>(wire.lifecycle, "lifecycle")?,
                transition: required_event_field::<_, D::Error>(wire.transition, "transition")?,
                from: required_event_field::<_, D::Error>(wire.from, "from")?,
                to: required_event_field::<_, D::Error>(wire.to, "to")?,
            },
            FactEventOp::InvariantRefused => Self::InvariantRefused {
                key: required_event_field::<_, D::Error>(wire.key, "key")?,
                owner: required_event_field::<_, D::Error>(wire.owner, "owner")?,
                id: required_event_field::<_, D::Error>(wire.id, "id")?,
                message: required_event_field::<_, D::Error>(wire.message, "message")?,
                outcome: required_event_field::<_, D::Error>(wire.outcome, "outcome")?,
                attempt_id: required_event_field::<_, D::Error>(wire.attempt_id, "attemptId")?,
            },
        })
    }
}

/// Build one net modification event from the values a commit read and wrote.
/// `before` and `after` are complete, effective stored-value snapshots. The
/// helper keeps the changed-property order canonical for replay and filters.
/// Hosts combining multiple prepared observations in one atomic commit can
/// use the final values to emit one aggregate event per changed object.
#[must_use]
pub fn modified_event(
    key: FactKey,
    before_version: FactVersion,
    after_version: FactVersion,
    before: JsonObject,
    after: JsonObject,
) -> Option<FactEvent> {
    if before == after {
        return None;
    }
    let properties: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let changed_properties = properties
        .into_iter()
        .filter(|property| before.get(*property) != after.get(*property))
        .map(|property| PropertyId(property.clone()))
        .collect();
    Some(FactEvent::Modified {
        key,
        before_version,
        after_version,
        before,
        after,
        changed_properties,
    })
}

/// A decision result ready for [`CellHost::commit`]: commands and timers carry
/// their records as payloads.
pub type PreparedDecision = CellCommit<FactEvent, JsonObject, JsonObject>;

/// Why a complete effective object value could not be materialized.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectiveValuesError {
    /// A submitted value or declared default is not an exact runtime value.
    #[error(transparent)]
    Runtime(#[from] RuntimeValueError),
    /// A submitted value or declared default does not match its effective
    /// property's scalar encoding.
    #[error("effective property `{property}` expects {expected}")]
    ValueType {
        /// The effective property.
        property: String,
        /// The expected scalar encoding.
        expected: &'static str,
    },
}

fn is_temporal_property(property: &EffectiveProperty) -> bool {
    temporal_property_expected(property).is_some()
}

fn temporal_property_expected(property: &EffectiveProperty) -> Option<&'static str> {
    match &property.kind {
        EffectiveKind::Base(PropertyType::Date {}) => Some("valid date"),
        EffectiveKind::Base(PropertyType::Timestamp { .. }) => Some("valid timestamp"),
        EffectiveKind::Base(_) | EffectiveKind::Registry(_) | EffectiveKind::Unresolved => None,
    }
}

/// Why a decision result was refused. Nothing was committed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DecisionRefusal<X> {
    /// The turn targets a historical, read-only world.
    #[error("the turn targets a historical, read-only world; nothing can be committed there")]
    ReadOnlyWorld,
    /// A non-main world emitted commands.
    #[error("only the main world may perform; this fork emitted {count} command(s)")]
    PerformOutsideMain {
        /// How many commands.
        count: usize,
    },
    /// The decision exceeded a per-decision envelope count cap.
    #[error("decision emitted {actual} {kind}, limit is {limit}")]
    EnvelopeExceeded {
        /// Which cap.
        kind: crate::decider::EnvelopeKind,
        /// The cap.
        limit: u32,
        /// What the decision emitted.
        actual: usize,
    },
    /// A commit contained an identifier longer than its admitted byte limit.
    #[error("{field} is {actual_bytes} bytes; the limit is {max_bytes} bytes")]
    IdentifierTooLong {
        /// The identifier kind.
        field: &'static str,
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
        /// Maximum UTF-8 byte length.
        max_bytes: usize,
    },
    /// Successful commits in this turn would exceed an aggregate envelope cap.
    #[error("turn would emit {actual} {kind}, limit is {limit}")]
    TurnEnvelopeExceeded {
        /// Which cap.
        kind: crate::decider::EnvelopeKind,
        /// The aggregate cap.
        limit: u64,
        /// The aggregate count if this commit were accepted.
        actual: u64,
    },
    /// A change names an object type the cell does not know.
    #[error("object type {object_type} is not declared in this cell")]
    UnknownType {
        /// The type.
        object_type: String,
    },
    /// A change targets an existing object the decision did not read.
    #[error("{key} is not in the slice the decision read, so it cannot be changed")]
    NotInSlice {
        /// The object.
        key: FactKey,
    },
    /// A rule attempted to mutate an object owned by a source observation.
    #[error("{key} is owned by a source observation and cannot be changed by a decision")]
    SourceOwnedTarget {
        /// The source-owned object.
        key: FactKey,
    },
    /// `set`, `delete`, or `transition` of an absent object.
    #[error("{key} does not exist")]
    ObjectMissing {
        /// The object.
        key: FactKey,
    },
    /// `create` of an existing object.
    #[error("{key} already exists")]
    ObjectExists {
        /// The object.
        key: FactKey,
    },
    /// `set` of a lifecycle property.
    #[error(
        "{key}: {property} is a lifecycle property and changes only through a declared transition"
    )]
    LifecyclePropertySet {
        /// The object.
        key: FactKey,
        /// The property.
        property: String,
    },
    /// A property write to an object in a declared immutable lifecycle state.
    #[error("{key} is in immutable {lifecycle} state {state:?}; property writes are refused")]
    ImmutableStateWrite {
        /// The object.
        key: FactKey,
        /// The lifecycle property whose current state is immutable.
        lifecycle: String,
        /// The immutable state.
        state: String,
    },
    /// `create` writes a lifecycle property that is not an initial state.
    #[error("{key}: {lifecycle} must start in one of {initial:?}, not {state:?}")]
    InvalidInitialState {
        /// The object.
        key: FactKey,
        /// The lifecycle property.
        lifecycle: String,
        /// The state written (or `None` when there is none and no default).
        state: Option<String>,
        /// The declared initial states.
        initial: Vec<String>,
    },
    /// `transition` names a property with no lifecycle on the type.
    #[error("{key}: {lifecycle} has no declared lifecycle")]
    UnknownLifecycle {
        /// The object.
        key: FactKey,
        /// The property.
        lifecycle: String,
    },
    /// A lifecycle is declared over the property, but it does not resolve to
    /// an effective state machine here (for example its states come from a
    /// shared property type); refused fail-closed.
    #[error(
        "{key}: a lifecycle is declared over {lifecycle}, but its states do not resolve in this cell; refused fail-closed"
    )]
    UnresolvedLifecycle {
        /// The object.
        key: FactKey,
        /// The lifecycle property.
        lifecycle: String,
    },
    /// `transition` names a transition the lifecycle does not declare.
    #[error("{key}: the {lifecycle} lifecycle declares no transition {transition:?}")]
    UnknownTransition {
        /// The object.
        key: FactKey,
        /// The lifecycle property.
        lifecycle: String,
        /// The transition named.
        transition: String,
    },
    /// The object's current state is not a source state of the transition.
    #[error(
        "{key} is {current:?}; transition {transition:?} only leaves {from:?}, so it cannot fire now"
    )]
    NotFromState {
        /// The object.
        key: FactKey,
        /// The transition.
        transition: String,
        /// The object's current state, if it has one.
        current: Option<String>,
        /// The transition's source states.
        from: Vec<String>,
    },
    /// A guard of the transition is false.
    #[error("{key}: guard `{guard}` of transition {transition:?} does not hold")]
    GuardFalse {
        /// The object.
        key: FactKey,
        /// The transition.
        transition: String,
        /// The guard source.
        guard: String,
    },
    /// A guard could not be evaluated to a boolean; refused fail-closed.
    #[error(
        "{key}: guard `{guard}` of transition {transition:?} could not be evaluated ({reason}); refused fail-closed"
    )]
    GuardError {
        /// The object.
        key: FactKey,
        /// The transition.
        transition: String,
        /// The guard source.
        guard: String,
        /// Why evaluation failed.
        reason: String,
    },
    /// The output is malformed (for example a timer record without a valid
    /// `afterMillis`).
    #[error("invalid decision output: {reason}")]
    InvalidOutput {
        /// What is wrong.
        reason: String,
    },
    /// An object in the slice changed after the decision read it.
    #[error(
        "{key} changed since the decision read it (read {expected}, now {current}); decide again on fresh state"
    )]
    Stale {
        /// The object.
        key: FactKey,
        /// The version the decision read.
        expected: FactVersion,
        /// The version now committed.
        current: FactVersion,
    },
    /// The triggering delivery is no longer pending.
    #[error("{consumed} is no longer pending; the turn was already committed or superseded")]
    NotPending {
        /// The delivery.
        consumed: Consumed,
    },
    /// An accounting posting in the resolved commit violates its declaration.
    #[error("accounting posting for {key} was refused: {reason:?}")]
    AccountingPosting {
        /// The accounting leg.
        key: FactKey,
        /// The accounting invariant that failed.
        reason: norn_ir::posting::AccountingError,
    },
    /// The resolved final value failed a declared invariant.
    #[error("{key}: invariant {} refused the change: {message}", id.as_str())]
    InvariantViolation {
        /// The object whose attempted final value failed.
        key: Box<FactKey>,
        /// The declaration that owns the invariant identity.
        owner: InvariantOwner,
        /// The invariant id within `owner`.
        id: InvariantId,
        /// The invariant's message.
        message: String,
        /// Why the invariant did not evaluate to true.
        outcome: Box<DataQualityOutcome>,
    },
    /// The bounded runtime invariant budget was exhausted.
    #[error("invariant evaluation for {key} exceeded the {limit}-unit budget")]
    InvariantBudgetExceeded {
        /// The object being evaluated when the budget was exhausted.
        key: FactKey,
        /// The per-commit unit ceiling.
        limit: u64,
    },
    /// The complete relation traversal could not fit the commit budget.
    #[error("{key}: {reason}")]
    TraversalLimitExceeded {
        /// The source whose relation could not be proven.
        key: FactKey,
        /// The exhausted budget.
        reason: String,
    },
    /// A version, the ledger sequence, or a timer due time would overflow.
    #[error("a version, ledger position, or due time would overflow")]
    Overflow,
    /// The host failed; nothing was committed.
    #[error("cell host failure")]
    Host(#[source] X),
}

impl<X> DecisionRefusal<X> {
    /// Stable machine-readable code of the refusal. Lifecycle violations use
    /// `norn-checker`'s codes (`norn_ir::lifecycle_codes`), so a refused
    /// firing has the same code statically and at commit; the rest are
    /// `decision.*`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ReadOnlyWorld => "decision.read_only_world",
            Self::PerformOutsideMain { .. } => "decision.perform_outside_main",
            Self::EnvelopeExceeded { .. } => "decision.envelope_exceeded",
            Self::IdentifierTooLong { .. } => "commit.identifier_too_long",
            Self::TurnEnvelopeExceeded { .. } => "decision.turn_envelope_exceeded",
            Self::UnknownType { .. } => "decision.unknown_type",
            Self::NotInSlice { .. } => "decision.not_in_slice",
            Self::SourceOwnedTarget { .. } => "decision.source_owned_target",
            Self::ObjectMissing { .. } => "decision.object_missing",
            Self::ObjectExists { .. } => "decision.object_exists",
            Self::LifecyclePropertySet { .. } => lifecycle_codes::DIRECT_WRITE,
            Self::ImmutableStateWrite { .. } => lifecycle_codes::IMMUTABLE_STATE_WRITE,
            Self::InvalidInitialState { .. } | Self::NotFromState { .. } => {
                lifecycle_codes::UNDECLARED_WRITE
            }
            Self::UnknownLifecycle { .. } => lifecycle_codes::NOT_A_LIFECYCLE,
            Self::UnresolvedLifecycle { .. } => "decision.unresolved_lifecycle",
            Self::UnknownTransition { .. } => lifecycle_codes::UNKNOWN_TRANSITION,
            Self::GuardFalse { .. } => "decision.guard_false",
            Self::GuardError { .. } => "decision.guard_error",
            Self::InvalidOutput { .. } => "decision.invalid_output",
            Self::Stale { .. } => "decision.stale_state",
            Self::NotPending { .. } => "decision.not_pending",
            Self::AccountingPosting { reason, .. } => reason.code(),
            Self::InvariantViolation { .. } => "decision.invariant_violation",
            Self::InvariantBudgetExceeded { .. } => "decision.invariant_budget_exceeded",
            Self::TraversalLimitExceeded { .. } => "commit.traversal_limit_exceeded",
            Self::Overflow => "decision.overflow",
            Self::Host(_) => "decision.host",
        }
    }

    /// Stored object referenced by a diagnostic, when this variant binds one.
    /// A shell must also check the complete read set: expressions and final
    /// values can depend on other facts. Absence is never proof of public data.
    #[must_use]
    pub fn diagnostic_subject(&self) -> Option<&FactKey> {
        match self {
            Self::NotInSlice { key }
            | Self::SourceOwnedTarget { key }
            | Self::ObjectMissing { key }
            | Self::ObjectExists { key }
            | Self::LifecyclePropertySet { key, .. }
            | Self::ImmutableStateWrite { key, .. }
            | Self::InvalidInitialState { key, .. }
            | Self::UnknownLifecycle { key, .. }
            | Self::UnresolvedLifecycle { key, .. }
            | Self::UnknownTransition { key, .. }
            | Self::NotFromState { key, .. }
            | Self::GuardFalse { key, .. }
            | Self::GuardError { key, .. }
            | Self::Stale { key, .. }
            | Self::AccountingPosting { key, .. }
            | Self::InvariantBudgetExceeded { key, .. }
            | Self::TraversalLimitExceeded { key, .. } => Some(key),
            Self::InvariantViolation { key, .. } => Some(key.as_ref()),
            Self::ReadOnlyWorld
            | Self::PerformOutsideMain { .. }
            | Self::EnvelopeExceeded { .. }
            | Self::IdentifierTooLong { .. }
            | Self::TurnEnvelopeExceeded { .. }
            | Self::UnknownType { .. }
            | Self::InvalidOutput { .. }
            | Self::NotPending { .. }
            | Self::Overflow
            | Self::Host(_) => None,
        }
    }

    pub(crate) fn from_commit(error: CommitError<X>) -> Self {
        let invalid = |reason| Self::InvalidOutput { reason };
        match error {
            CommitError::DuplicateKey { key } => Self::InvalidOutput {
                reason: format!("{key} appears twice in one commit"),
            },
            CommitError::OwnedRowsProtected { key } => Self::InvalidOutput {
                reason: format!("{key} belongs to a protected owned-row transaction"),
            },
            CommitError::IdentifierTooLong {
                field,
                actual_bytes,
                max_bytes,
            } => Self::commit_identifier_refusal(field, actual_bytes, max_bytes),
            CommitError::ImmutableObject { key } => invalid(format!("{key} is immutable")),
            CommitError::NotPending { consumed } => Self::NotPending { consumed },
            CommitError::Conflict {
                key,
                expected,
                current,
            } => Self::Stale {
                key,
                expected,
                current,
            },
            // A decision writes no links; a link refusal is a malformed commit.
            CommitError::DuplicateLink { key } => {
                invalid(format!("link {key} appears twice in one commit"))
            }
            CommitError::LinkConflict { key, .. } => {
                invalid(format!("link {key} changed since the turn read it"))
            }
            CommitError::LinkReadConflict { source_key, link } => invalid(format!(
                "link targets for {source_key} -{link}-> changed since the turn read them"
            )),
            CommitError::UnsupportedLinkRead { source_key, link } => invalid(format!(
                "host cannot verify link targets for {source_key} -{link}->"
            )),
            CommitError::DuplicateLinkRead { source_key, link } => invalid(format!(
                "link targets for {source_key} -{link}-> appear more than once"
            )),
            CommitError::LinkReadTooLarge {
                source_key,
                link,
                actual,
                max,
            } => Self::link_read_limit_refusal(&source_key, &link, actual, max),
            CommitError::InvalidLinkRead { source_key, link } => Self::InvalidOutput {
                reason: format!("link targets for {source_key} -{link}-> are malformed"),
            },
            CommitError::DanglingLink { key, missing } => Self::InvalidOutput {
                reason: format!("link {key} would dangle: {missing} does not exist"),
            },
            CommitError::LinkTarget { key } => Self::InvalidOutput {
                reason: format!("link {key} is not a declared link to an admitted target"),
            },
            CommitError::LinkCardinality { key, endpoint } => Self::InvalidOutput {
                reason: format!("link {key} exceeds its cardinality at {endpoint}"),
            },
            CommitError::LinkRequired { parent, link } => Self::InvalidOutput {
                reason: format!("{parent} has no required link {link}"),
            },
            CommitError::TypeMismatch { key, concrete } => Self::InvalidOutput {
                reason: format!("{key} is live as {}", concrete.as_str()),
            },
            CommitError::AccountingPosting { key, reason } => {
                Self::AccountingPosting { key, reason }
            }
            CommitError::InvariantViolation {
                key,
                owner,
                id,
                message,
                outcome,
            } => Self::commit_invariant_refusal(key, owner, id, message, outcome),
            CommitError::InvariantBudgetExceeded { key, limit } => {
                Self::InvariantBudgetExceeded { key, limit }
            }
            CommitError::InvariantModel { key, interface } => Self::InvalidOutput {
                reason: format!("{key} references missing interface {interface}"),
            },
            CommitError::EffectiveValues { key, reason } => Self::InvalidOutput {
                reason: format!("{key} has invalid effective property values: {reason}"),
            },
            CommitError::TraversalLimitExceeded { key, reason } => {
                Self::TraversalLimitExceeded { key, reason }
            }
            CommitError::ObservationAuthorization => Self::InvalidOutput {
                reason: "observation authorization did not match its prepared commit".to_owned(),
            },
            CommitError::EventLimitExceeded { actual, limit } => Self::InvalidOutput {
                reason: format!("commit emitted {actual} events, limit is {limit}"),
            },
            CommitError::Overflow => Self::Overflow,
            CommitError::Host(error) => Self::Host(error),
        }
    }

    fn link_read_limit_refusal(
        source_key: &FactKey,
        link: &str,
        actual: usize,
        max: usize,
    ) -> Self {
        Self::InvalidOutput {
            reason: format!(
                "link targets for {source_key} -{link}-> exceed the {max} item/byte limit ({actual})"
            ),
        }
    }

    fn commit_identifier_refusal(
        field: &'static str,
        actual_bytes: usize,
        max_bytes: usize,
    ) -> Self {
        Self::IdentifierTooLong {
            field,
            actual_bytes,
            max_bytes,
        }
    }

    fn commit_invariant_refusal(
        key: Box<FactKey>,
        owner: InvariantOwner,
        id: InvariantId,
        message: String,
        outcome: Box<DataQualityOutcome>,
    ) -> Self {
        Self::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        }
    }
}

/// One object's state while the output is resolved.
struct Working {
    /// The version the decision read.
    expected: FactVersion,
    /// The values after the changes so far (`None` = absent).
    values: Option<JsonObject>,
    /// Whether any change touched it.
    written: bool,
}

/// Resolves `output` against `slice` into one commit. Pure; see the module
/// docs for the rules.
///
/// # Errors
///
/// The first refusal, in output order: world checks, then each fact change,
/// then commands, then timers.
pub fn prepare_decision<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    prepare_decision_with_source_owned_targets(catalog, slice, turn, output, &BTreeSet::new())
}

/// Prepares a rule decision while refusing direct fact changes to the
/// source-owned targets resolved by its host. The protected set is supplied
/// from persisted observation authority markers and trigger provenance.
/// Native objects of the same type remain writable.
///
/// # Errors
///
/// Refuses a direct change to a source-owned target, or returns the first
/// ordinary decision admission refusal.
pub fn prepare_decision_with_source_owned_targets<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    prepare_decision_with_limits_and_source_owned_targets(
        catalog,
        slice,
        turn,
        output,
        &EnvelopeLimits::V1,
        source_owned_targets,
    )
}

/// Prepares `output` under the supplied per-decision envelope limits.
///
/// Counts are checked before resolving fact changes or allocating the commit
/// vectors. Aggregate turn limits are enforced by [`commit_decision_with_meter`]
/// and [`crate::commit_action_with_meter`].
///
/// # Errors
///
/// The first refusal, in order: world checks, envelope caps, then each fact
/// change, commands, and timers.
pub fn prepare_decision_with_limits<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    prepare_decision_with_limits_and_source_owned_targets(
        catalog,
        slice,
        turn,
        output,
        limits,
        &BTreeSet::new(),
    )
}

fn prepare_decision_with_limits_and_source_owned_targets<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    prepare_decision_with_limits_for(
        catalog,
        slice,
        turn,
        output,
        limits,
        AdmissionOrigin::Rule,
        source_owned_targets,
    )
}

pub(crate) fn prepare_action_decision<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    prepare_decision_with_limits_for(
        catalog,
        slice,
        turn,
        output,
        limits,
        AdmissionOrigin::Action,
        &BTreeSet::new(),
    )
}

/// Prepare the object changes for a trusted parent-and-owned-rows action.
/// Internal manifest writes are assembled separately in its one commit;
/// this narrow path is the only place generic child writes and parent
/// cascades are admitted.
pub(crate) fn prepare_owned_rows<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    prepare_decision_with_limits_for(
        catalog,
        slice,
        turn,
        output,
        limits,
        AdmissionOrigin::OwnedRows,
        &BTreeSet::new(),
    )
}

/// Prepares observed ordinary property facts using the same admission rules
/// as decisions, except that source observations may revise immutable state.
///
/// Observation snapshots are admitted by their source and mapping. Their
/// property facts record what the source reported; immutable-state protection
/// remains in force for commands, actions, and rules.
pub(crate) fn prepare_observed_properties<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    facts: &[FactChange],
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    let output = DecisionOutput {
        facts: facts.to_vec(),
        ..DecisionOutput::default()
    };
    prepare_decision_with_limits_for(
        catalog,
        slice,
        turn,
        &output,
        &EnvelopeLimits::V1,
        AdmissionOrigin::Observation,
        &BTreeSet::new(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionOrigin {
    Rule,
    Action,
    Observation,
    OwnedRows,
}

fn prepare_decision_with_limits_for<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
    origin: AdmissionOrigin,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    if !turn.world.is_writable() {
        return Err(DecisionRefusal::ReadOnlyWorld);
    }
    if !output.commands.is_empty() && !turn.world.may_perform() {
        return Err(DecisionRefusal::PerformOutsideMain {
            count: output.commands.len(),
        });
    }
    let counts = output_envelope_counts(output);
    check_decision_envelope(limits, counts).map_err(|exceeded| {
        DecisionRefusal::EnvelopeExceeded {
            kind: exceeded.kind,
            limit: exceeded.limit,
            actual: exceeded.actual,
        }
    })?;
    let mut working: BTreeMap<FactKey, Working> = BTreeMap::new();
    let mut events = Vec::with_capacity(output.facts.len());
    for change in &output.facts {
        events.push(apply_change(
            catalog,
            slice,
            &mut working,
            change,
            origin,
            source_owned_targets,
        )?);
    }
    // Preserve the granular events above, then append one aggregate event per
    // existing object whose final stored values differ from the values read.
    // Iterating the BTreeMap gives stable object-key order, independent of the
    // order in which output changes named their targets.
    for (key, entry) in &working {
        if !entry.written {
            continue;
        }
        let Some(before_state) = slice.get(key) else {
            continue;
        };
        let (Some(before_values), Some(after_values)) =
            (before_state.values.as_ref(), entry.values.as_ref())
        else {
            continue;
        };
        let before = catalog
            .effective_object_values(&key.object_type, before_values)
            .map_err(|error| DecisionRefusal::InvalidOutput {
                reason: format!("the before snapshot for {key} is invalid: {error}"),
            })?;
        let after = catalog
            .effective_object_values(&key.object_type, after_values)
            .map_err(|error| DecisionRefusal::InvalidOutput {
                reason: format!("the after snapshot for {key} is invalid: {error}"),
            })?;
        let Some(after_version) = entry.expected.checked_next() else {
            return Err(DecisionRefusal::Overflow);
        };
        if let Some(event) =
            modified_event(key.clone(), entry.expected, after_version, before, after)
        {
            events.push(event);
        }
    }
    // Aggregate modification events consume the same per-decision event
    // budget as the granular events the output requested.
    let resolved_counts =
        EnvelopeCounts::new(events.len(), output.commands.len(), output.timers.len());
    check_decision_envelope(limits, resolved_counts).map_err(|exceeded| {
        DecisionRefusal::EnvelopeExceeded {
            kind: exceeded.kind,
            limit: exceeded.limit,
            actual: exceeded.actual,
        }
    })?;
    let mut effects = commands(turn, &output.commands);
    for (index, record) in output.timers.iter().enumerate() {
        effects.push(Effect::Schedule(timer(turn, index, record)?));
    }

    let reads = slice
        .iter()
        .filter(|(key, _)| working.get(*key).is_none_or(|entry| !entry.written))
        .map(|(key, state)| FactExpectation {
            key: key.clone(),
            expected: state.version,
        })
        .collect();
    let writes = working
        .into_iter()
        .filter(|(_, entry)| entry.written)
        .map(|(key, entry)| FactWrite {
            key,
            expected: entry.expected,
            mutation: entry
                .values
                .map_or(FactMutation::Delete, |values| FactMutation::Put { values }),
        })
        .collect();
    Ok(CellCommit {
        consumes: turn.consumes.clone(),
        reads,
        writes,
        events,
        effects,
        links: Vec::new(),
        link_reads: Vec::new(),
    })
}

fn output_envelope_counts(output: &DecisionOutput) -> EnvelopeCounts {
    EnvelopeCounts::new(
        output.facts.len(),
        output.commands.len(),
        output.timers.len(),
    )
}

/// Prepares `output` (see [`prepare_decision`]) and commits it through
/// `host` in one transaction.
///
/// # Errors
///
/// A [`DecisionRefusal`]; nothing was committed.
pub fn commit_decision<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
) -> Result<CommitReceipt, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_decision_with_limits(host, catalog, slice, turn, output, &EnvelopeLimits::V1)
}

/// Prepares and commits a decision under explicit per-decision limits.
///
/// # Errors
///
/// A [`DecisionRefusal`]; nothing was committed.
pub fn commit_decision_with_limits<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
) -> Result<CommitReceipt, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    commit_decision_inner(host, catalog, slice, turn, output, limits, None)
}

/// Prepares and commits a decision under the meter's per-decision and
/// aggregate turn limits.
///
/// The aggregate counter is checked before the commit and charged only after
/// the host reports success.
///
/// # Errors
///
/// A [`DecisionRefusal`]; nothing was committed and the meter is unchanged.
pub fn commit_decision_with_meter<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    meter: &mut TurnEnvelopeMeter,
) -> Result<CommitReceipt, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let limits = meter.decision_limits();
    commit_decision_inner(host, catalog, slice, turn, output, &limits, Some(meter))
}

fn commit_decision_inner<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    output: &DecisionOutput,
    limits: &EnvelopeLimits,
    meter: Option<&mut TurnEnvelopeMeter>,
) -> Result<CommitReceipt, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let prepared = prepare_decision_with_limits(catalog, slice, turn, output, limits)?;
    let counts = EnvelopeCounts::new(
        prepared.events.len(),
        output.commands.len(),
        output.timers.len(),
    );
    if let Some(meter) = meter.as_deref() {
        meter
            .check(counts)
            .map_err(|exceeded| DecisionRefusal::TurnEnvelopeExceeded {
                kind: exceeded.kind,
                limit: exceeded.limit,
                actual: exceeded.actual,
            })?;
    }
    let commit = CellCommit {
        consumes: prepared.consumes,
        reads: prepared.reads,
        writes: prepared.writes,
        events: prepared.events.into_iter().map(Into::into).collect(),
        effects: prepared.effects.into_iter().map(widen_effect).collect(),
        links: Vec::new(),
        link_reads: Vec::new(),
    };
    match host.commit(commit) {
        Ok(receipt) => {
            if let Some(meter) = meter {
                meter.charge(counts);
            }
            Ok(receipt)
        }
        Err(CommitError::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        }) => {
            let refusal = DecisionRefusal::InvariantViolation {
                key: key.clone(),
                owner: owner.clone(),
                id: id.clone(),
                message: message.clone(),
                outcome: outcome.clone(),
            };
            let mut refusal_commit = CellCommit::empty();
            refusal_commit.consumes.clone_from(&turn.consumes);
            refusal_commit.events.push(
                FactEvent::InvariantRefused {
                    key: *key,
                    owner,
                    id,
                    message,
                    outcome: *outcome,
                    attempt_id: turn.id.clone(),
                }
                .into(),
            );
            match host.commit(refusal_commit) {
                Ok(_) => Err(refusal),
                Err(error) => Err(DecisionRefusal::from_commit(error)),
            }
        }
        Err(error) => Err(DecisionRefusal::from_commit(error)),
    }
}

pub(crate) fn widen_effect<C: From<JsonObject>, T: From<JsonObject>>(
    effect: Effect<JsonObject, JsonObject>,
) -> Effect<C, T> {
    match effect {
        Effect::Perform(command) => Effect::Perform(Command {
            id: command.id,
            generation: command.generation,
            idempotency_key: command.idempotency_key,
            freshness: command.freshness,
            deadline: command.deadline,
            payload: command.payload.into(),
        }),
        Effect::Schedule(timer) => Effect::Schedule(Timer {
            id: timer.id,
            due: timer.due,
            payload: timer.payload.into(),
        }),
        Effect::CancelTimer(id) => Effect::CancelTimer(id),
    }
}

fn fact_key(change: &FactChange) -> FactKey {
    let (object_type, key) = match change {
        FactChange::Create {
            object_type, key, ..
        }
        | FactChange::Set {
            object_type, key, ..
        }
        | FactChange::Delete { object_type, key }
        | FactChange::Transition {
            object_type, key, ..
        } => (object_type, key),
    };
    FactKey {
        object_type: object_type.clone(),
        key: key.clone(),
    }
}

/// The identity a `set` or `transition` writes: `key` itself, or, when the
/// decision read no object under it, the one existing object in its read
/// slice with the same key whose concrete type is a subtype of `key`'s type
/// (#3079). Single-object and list-entry reads can both resolve to concrete
/// subtype identities while a decision names a declared supertype; only an
/// existing read identity authorizes that alias. Writes retain the version
/// read, so compare-and-swap is unchanged.
fn slice_identity(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    working: &BTreeMap<FactKey, Working>,
    key: FactKey,
) -> FactKey {
    if working.contains_key(&key) || slice.contains_key(&key) {
        return key;
    }
    // Only objects that exist: an absent read (an optional or missing entry)
    // is not an object a write could mean.
    let read: BTreeSet<&FactKey> = slice
        .iter()
        .filter(|(_, state)| state.values.is_some())
        .map(|(read, _)| read)
        .filter(|read| {
            read.key == key.key
                && read.object_type != key.object_type
                && catalog
                    .ancestry(&read.object_type)
                    .iter()
                    .any(|ancestor| ancestor.name == key.object_type)
        })
        .collect();
    let mut read = read.into_iter();
    match (read.next(), read.next()) {
        (Some(only), None) => only.clone(),
        _ => key,
    }
}

/// Applies one change to the working copy and returns its ledger event.
#[allow(
    clippy::too_many_lines,
    reason = "one change is resolved atomically across the declared fact variants"
)]
fn apply_change<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    working: &mut BTreeMap<FactKey, Working>,
    change: &FactChange,
    origin: AdmissionOrigin,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<FactEvent, DecisionRefusal<X>> {
    let key = match change {
        // Only a write to an object the decision read is resolved: a create
        // or delete names exactly the identity it means.
        FactChange::Set { .. } | FactChange::Transition { .. } => {
            slice_identity(catalog, slice, working, fact_key(change))
        }
        FactChange::Create { .. } | FactChange::Delete { .. } => fact_key(change),
    };
    if source_owned_targets.iter().any(|source_owned| {
        source_owned.key == key.key
            && (source_owned.object_type == key.object_type
                || catalog
                    .ancestry(&source_owned.object_type)
                    .iter()
                    .any(|ancestor| ancestor.name == key.object_type)
                || catalog
                    .ancestry(&key.object_type)
                    .iter()
                    .any(|ancestor| ancestor.name == source_owned.object_type))
    }) {
        return Err(DecisionRefusal::SourceOwnedTarget { key });
    }
    if !catalog.contains(&key.object_type) {
        return Err(DecisionRefusal::UnknownType {
            object_type: key.object_type.as_str().to_owned(),
        });
    }
    let protected_action_call = key.object_type.as_str() == crate::action_commit::ACTION_CALL_TYPE;
    let protected_manifest = key.object_type.as_str() == crate::action_commit::OWNED_ROWS_TYPE;
    let protected_child = catalog.is_owned_child_type(&key.object_type);
    let protected_parent_delete = matches!(change, FactChange::Delete { .. })
        && !catalog.owned_collections(&key.object_type).is_empty();
    if protected_action_call
        || protected_manifest
        || (origin != AdmissionOrigin::OwnedRows && protected_child)
        || (origin != AdmissionOrigin::OwnedRows && protected_parent_delete)
    {
        return Err(DecisionRefusal::InvalidOutput {
            reason: format!(
                "{key} belongs to a protected owned-row transaction and cannot be changed by this path"
            ),
        });
    }
    if !working.contains_key(&key) {
        let entry = match slice.get(&key) {
            Some(state) => Working {
                expected: state.version,
                values: state.values.clone(),
                written: false,
            },
            // An object the decision never read can only be created, and
            // only if it was never written (the CAS checks that at commit).
            None if matches!(change, FactChange::Create { .. }) => Working {
                expected: FactVersion::ABSENT,
                values: None,
                written: false,
            },
            None => return Err(DecisionRefusal::NotInSlice { key }),
        };
        working.insert(key.clone(), entry);
    }
    let Some(entry) = working.get_mut(&key) else {
        return Err(DecisionRefusal::NotInSlice { key });
    };
    if entry.values.is_some()
        && catalog.immutable(&key.object_type)
        && !matches!(change, FactChange::Create { .. })
    {
        return Err(DecisionRefusal::InvalidOutput {
            reason: format!("{key} is immutable"),
        });
    }
    if origin != AdmissionOrigin::Rule {
        let protected = |property: &str| {
            catalog
                .effective_type(&key.object_type)
                .and_then(|effective| effective.property(property))
                .is_some_and(|property| property.protected)
        };
        let forbidden = match change {
            FactChange::Create { values, .. } => {
                values.keys().any(|property| protected(property.as_str()))
            }
            FactChange::Set { property, .. } => protected(property.as_str()),
            FactChange::Transition { lifecycle, .. } => protected(lifecycle.as_str()),
            FactChange::Delete { .. } => false,
        };
        if forbidden {
            return Err(DecisionRefusal::InvalidOutput {
                reason: format!("{key} has protected properties writable only by rules"),
            });
        }
    }
    let event = match change {
        FactChange::Create { values, .. } => {
            if entry.values.is_some() {
                return Err(DecisionRefusal::ObjectExists { key });
            }
            let values = with_initial_states(catalog, &key, values)?;
            let values = catalog
                .effective_object_values(&key.object_type, &values)
                .map_err(|error| DecisionRefusal::InvalidOutput {
                    reason: format!("the effective values for {key}: {error}"),
                })?;
            entry.values = Some(values.clone());
            FactEvent::Created { key, values }
        }
        FactChange::Set {
            property, value, ..
        } => {
            let Some(values) = entry.values.as_mut() else {
                return Err(DecisionRefusal::ObjectMissing { key });
            };
            if catalog
                .lifecycle_properties(&key.object_type)
                .contains(property)
                || catalog.lifecycle(&key.object_type, property).is_some()
            {
                return Err(DecisionRefusal::LifecyclePropertySet {
                    key,
                    property: property.as_str().to_owned(),
                });
            }
            if origin != AdmissionOrigin::Observation
                && let Some((lifecycle, state)) = catalog.immutable_state(&key.object_type, values)
            {
                return Err(DecisionRefusal::ImmutableStateWrite {
                    key,
                    lifecycle: lifecycle.as_str().to_owned(),
                    state,
                });
            }
            let value = catalog.canonicalize_working_temporal_value(
                &key.object_type,
                property.as_str(),
                value,
            );
            values.insert(property.as_str().to_owned(), value.clone());
            FactEvent::Set {
                key,
                property: property.clone(),
                value,
            }
        }
        FactChange::Delete { .. } => {
            if entry.values.take().is_none() {
                return Err(DecisionRefusal::ObjectMissing { key });
            }
            FactEvent::Deleted { key }
        }
        FactChange::Transition {
            lifecycle,
            transition,
            ..
        } => {
            let Some(values) = entry.values.as_mut() else {
                return Err(DecisionRefusal::ObjectMissing { key });
            };
            let (declared, from) =
                resolve_transition(catalog, &key, values, lifecycle, transition)?;
            check_guards(&key, values, &declared)?;
            values.insert(
                lifecycle.as_str().to_owned(),
                JsonValue::String(declared.to.clone()),
            );
            FactEvent::Transitioned {
                key,
                lifecycle: lifecycle.clone(),
                transition: transition.clone(),
                from,
                to: declared.to,
            }
        }
    };
    entry.written = true;
    Ok(event)
}

/// `values` with every effective lifecycle property's default filled in,
/// after checking each lifecycle property holds an initial state.
fn with_initial_states<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
) -> Result<JsonObject, DecisionRefusal<X>> {
    materialize_lifecycle_defaults(catalog, key, values, true)
}

/// Fill declared defaults for lifecycle properties without requiring their
/// final values to already be initial states. Callers that prepare a create
/// still use [`with_initial_states`] for that full validation.
pub(crate) fn fill_lifecycle_defaults<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
) -> Result<JsonObject, DecisionRefusal<X>> {
    materialize_lifecycle_defaults(catalog, key, values, false)
}

fn materialize_lifecycle_defaults<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    validate_initial: bool,
) -> Result<JsonObject, DecisionRefusal<X>> {
    let mut values = values.clone();
    let effective = catalog.lifecycles(&key.object_type);
    if let Some(unresolved) = catalog
        .lifecycle_properties(&key.object_type)
        .into_iter()
        .find(|property| !effective.contains_key(property))
    {
        return Err(DecisionRefusal::UnresolvedLifecycle {
            key: key.clone(),
            lifecycle: unresolved.as_str().to_owned(),
        });
    }
    for lifecycle in effective.into_values() {
        let property = lifecycle.property.as_str();
        let default = catalog
            .default_of(&key.object_type, &lifecycle.property)
            .filter(|_| !values.contains_key(property));
        if let Some(default) = default {
            let default =
                JsonValue::from_ir(default).map_err(|error| DecisionRefusal::InvalidOutput {
                    reason: format!("the default of `{property}`: {error}"),
                })?;
            values.insert(property.to_owned(), default);
        }
        if validate_initial {
            let state = match values.get(property) {
                Some(JsonValue::String(state)) => Some(state),
                _ => None,
            };
            if !state.is_some_and(|state| lifecycle.initial.contains(state)) {
                return Err(DecisionRefusal::InvalidInitialState {
                    key: key.clone(),
                    lifecycle: property.to_owned(),
                    state: state.cloned(),
                    initial: lifecycle.initial.clone(),
                });
            }
        }
    }
    Ok(values)
}

/// The effective lifecycle of `lifecycle`, then the firing of `transition`
/// from the object's current state under `Lifecycle::fire` — the per-call
/// semantics `norn-checker` uses for `fire_transition` (same codes).
/// Returns the transition and the state it leaves.
fn resolve_transition<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> Result<(Transition, String), DecisionRefusal<X>> {
    let Some(declared) = catalog.lifecycle(&key.object_type, lifecycle) else {
        let lifecycle_name = lifecycle.as_str().to_owned();
        return Err(
            if catalog
                .lifecycle_properties(&key.object_type)
                .contains(lifecycle)
            {
                DecisionRefusal::UnresolvedLifecycle {
                    key: key.clone(),
                    lifecycle: lifecycle_name,
                }
            } else {
                DecisionRefusal::UnknownLifecycle {
                    key: key.clone(),
                    lifecycle: lifecycle_name,
                }
            },
        );
    };
    let current = match values.get(lifecycle.as_str()) {
        Some(JsonValue::String(state)) => Some(state.clone()),
        _ => None,
    };
    match declared.fire(transition.as_str(), current.as_deref()) {
        Ok(fired) => Ok((fired.clone(), current.unwrap_or_default())),
        Err(FiringRefusal::UnknownTransition) => Err(DecisionRefusal::UnknownTransition {
            key: key.clone(),
            lifecycle: lifecycle.as_str().to_owned(),
            transition: transition.as_str().to_owned(),
        }),
        Err(FiringRefusal::NotFromState) => Err(DecisionRefusal::NotFromState {
            key: key.clone(),
            transition: transition.as_str().to_owned(),
            current,
            from: declared
                .transitions
                .iter()
                .find(|candidate| candidate.id == *transition)
                .map(|candidate| candidate.from.clone())
                .unwrap_or_default(),
        }),
    }
}

/// Evaluates every guard of `declared` against the object's state before
/// the change (the checker does not: guards read the object).
fn check_guards<X>(
    key: &FactKey,
    values: &JsonObject,
    declared: &Transition,
) -> Result<(), DecisionRefusal<X>> {
    if declared.guards.is_empty() {
        return Ok(());
    }
    let guard_error = |guard: &str, reason: String| DecisionRefusal::GuardError {
        key: key.clone(),
        transition: declared.id.as_str().to_owned(),
        guard: guard.to_owned(),
        reason,
    };
    let object = values
        .iter()
        .map(|(name, value)| expr_value(value).map(|value| (name.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>();
    let context = match object {
        Ok(object) => ExprValue::object([("self", ExprValue::Object(object))]),
        Err(reason) => {
            let first = declared.guards.first().map_or("", |guard| guard.as_str());
            return Err(guard_error(first, reason));
        }
    };
    for guard in &declared.guards {
        match guard_holds(guard.as_str(), &context) {
            Ok(true) => {}
            Ok(false) => {
                return Err(DecisionRefusal::GuardFalse {
                    key: key.clone(),
                    transition: declared.id.as_str().to_owned(),
                    guard: guard.as_str().to_owned(),
                });
            }
            Err(reason) => return Err(guard_error(guard.as_str(), reason)),
        }
    }
    Ok(())
}

/// Evaluates one guard; `Ok` only for a boolean result.
pub(crate) fn guard_holds(source: &str, context: &ExprValue) -> Result<bool, String> {
    let expression = Expression::compile(source).map_err(|error| error.to_string())?;
    let evaluation = expression
        .evaluate(context, EvaluationBudget::default())
        .map_err(|error| error.to_string())?;
    match evaluation.value() {
        ExprValue::Bool(holds) => Ok(*holds),
        other @ (ExprValue::Null
        | ExprValue::Number(_)
        | ExprValue::String(_)
        | ExprValue::Array(_)
        | ExprValue::Object(_)) => Err(format!("the guard evaluated to {other:?}, not a boolean")),
    }
}

/// An object value as a typed `norn-expr` value. Numbers convert exactly or
/// not at all.
pub(crate) fn expr_value(value: &JsonValue) -> Result<ExprValue, String> {
    Ok(match value {
        JsonValue::Null => ExprValue::Null,
        JsonValue::Bool(flag) => ExprValue::Bool(*flag),
        JsonValue::Number(number) => ExprValue::Number(
            number
                .to_string()
                .parse()
                .map_err(|error: norn_expr::ExprError| error.to_string())?,
        ),
        JsonValue::String(text) => ExprValue::String(text.clone()),
        JsonValue::Array(items) => {
            ExprValue::Array(items.iter().map(expr_value).collect::<Result<_, _>>()?)
        }
        JsonValue::Object(entries) => ExprValue::Object(
            entries
                .iter()
                .map(|(name, item)| expr_value(item).map(|item| (name.clone(), item)))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// Each command record as one `perform` effect with turn-derived identity.
fn commands(turn: &DecisionTurn, records: &[JsonObject]) -> Vec<Effect<JsonObject, JsonObject>> {
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let id = format!("{}/command/{index}", turn.id);
            Effect::Perform(Command {
                id: CommandId(id.clone()),
                generation: Generation::default(),
                idempotency_key: IdempotencyKey(id),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: None,
                payload: record.clone(),
            })
        })
        .collect()
}

/// One timer record as a timer due `afterMillis` after the turn.
fn timer<X>(
    turn: &DecisionTurn,
    index: usize,
    record: &JsonObject,
) -> Result<Timer<JsonObject>, DecisionRefusal<X>> {
    let invalid = |reason: String| DecisionRefusal::InvalidOutput { reason };
    let Some(JsonValue::Number(millis)) = record.get(TIMER_AFTER_MILLIS_MEMBER) else {
        return Err(invalid(format!(
            "timer {index}: `{TIMER_AFTER_MILLIS_MEMBER}` is required and must be a number"
        )));
    };
    // Integral, non-negative, and within `u64`; anything else (a fraction, a
    // negative, NaN) fails to parse.
    let after: u64 = millis.to_string().parse().map_err(|error| {
        invalid(format!(
            "timer {index}: `{TIMER_AFTER_MILLIS_MEMBER}` must be a non-negative integer, not {millis} ({error})"
        ))
    })?;
    let due = turn
        .now
        .checked_add_millis(after)
        .ok_or(DecisionRefusal::Overflow)?;
    let id = match record.get(TIMER_ID_MEMBER) {
        None => format!("{}/timer/{index}", turn.id),
        Some(JsonValue::String(id)) if !id.trim().is_empty() => id.clone(),
        Some(_) => {
            return Err(invalid(format!(
                "timer {index}: `{TIMER_ID_MEMBER}` must be a non-empty string"
            )));
        }
    };
    Ok(Timer {
        id: TimerId(id),
        due,
        payload: record.clone(),
    })
}
