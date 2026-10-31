//! The cell ABI (T4-CELL-4, #2678): the JSON-in/JSON-out surface
//! `apps/cell-shim` calls so the Durable Objects host runs the kernel's own
//! commit contract instead of re-implementing it in TypeScript.
//!
//! The shim owns storage (DO SQLite through `ctx.storage.sql` inside
//! `transactionSync`), the alarm, and the outside world. Everything about a
//! commit that is kernel semantics comes from here:
//!
//! - [`decode_commit_json`] decodes a `CellCommit` with the kernel's own
//!   types (`deny_unknown_fields` everywhere the kernel has it) and returns a
//!   **plan**: the delivery it consumes, the objects it reads and writes, and
//!   its enqueues, with every identifier pre-extracted so the shim never has
//!   to parse a commit itself.
//! - [`check_commit_json`] runs [`norn_kernel::check_commit`] (the order
//!   every host uses) against the cell's model (its object types and
//!   interfaces) and
//!   the state the shim read inside its transaction, and returns the verdict:
//!   the refusal, or the new ledger position, the written versions, the
//!   receipt, and the ledger entry.
//! - [`resolve_key_json`] gives the canonical identity of an object or link
//!   and `is_a` under the model, for the shim's reads.
//! - [`decode_observed_json`] and [`validate_input_json`] check the other two
//!   things that enter a cell: an external observation (provenance is
//!   mandatory) and a performed command's result.
//!
//! # Exact payloads (T3-NUM-1, #2770)
//!
//! Ledger events, command payloads, timer payloads, and kernel inputs are
//! **opaque JSON text** at this boundary. Fact values retain opaque members
//! while declared numeric properties and defaults use the kernel's canonical
//! stored values for both the storage plan and invariant admission. Payloads are carried as
//! [`RawValue`]s — the exact source bytes, validated as JSON but never
//! converted to `f64` — and returned to the shim as JSON *strings*, so a
//! `JSON.parse` of a plan or verdict never touches a payload number. Integers
//! the shim does handle as numbers (versions, ledger positions, generations,
//! due times, incarnations) are rejected above
//! [`MAX_SAFE_INTEGER`], the largest integer a JavaScript number and DO
//! SQLite's bindings represent exactly. The fallible decimal conversion
//! inside the kernel/decisions ABI is #2770; opaque payload bytes pass through
//! unchanged and typed numeric properties never pass through `f64`.
//!
//! On `wasm32` the `wasm` module exports each function to JavaScript under
//! its camel-case name. Invalid *input* (not JSON, a wrong shape, an unsafe
//! integer) is an [`AbiError`] (a thrown `Error`); a refused commit is a
//! verdict, never an error.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_kernel::cell_host::{
    InterfaceDef, JsonObject, JsonValue as RuntimeValue, LinkReadTarget, ObjectTypeDef,
    ObjectTypeName, PropertyTypeDef,
};
use norn_kernel::{
    CellCommit, Command, CommitError, CommitModel, CommitView, Consumed, FactExpectation, FactKey,
    FactMutation, FactVersion, FactWrite, LedgerSeq, LinkKey, LinkMutation, LinkWrite, Observed,
    Timer, TimerId, TypeCatalog, WrittenLinkVersion, WrittenVersion, check_commit,
};
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// The ABI version [`version_json`] reports. A change to a function's
/// arguments or to a document's fields is a new version.
pub const ABI_VERSION: &str = "norn-cell-abi/4";

/// Format tag of the [`decode_commit_json`] document.
pub const PLAN_FORMAT: &str = "norn-cell-abi/plan@4";

/// Format tag of the [`check_commit_json`] document.
pub const VERDICT_FORMAT: &str = "norn-cell-abi/verdict@4";

/// The largest integer a JavaScript number represents exactly (2^53 − 1).
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Invalid input to an ABI function.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AbiError {
    /// An argument is not JSON of the expected shape.
    InvalidInput {
        /// The argument's name.
        argument: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// An integer the shim handles as a JavaScript number exceeds
    /// [`MAX_SAFE_INTEGER`].
    UnsafeInteger {
        /// Where the integer appears.
        field: &'static str,
        /// Its value.
        value: u64,
    },
    /// A declared property cannot be represented by the runtime.
    UnrepresentableProperty {
        /// The declared property name.
        property: String,
    },
    /// A result document could not be encoded.
    Encode(String),
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { argument, detail } => write!(f, "invalid {argument}: {detail}"),
            Self::UnsafeInteger { field, value } => write!(
                f,
                "{field} {value} exceeds the largest exactly representable integer {MAX_SAFE_INTEGER}"
            ),
            Self::UnrepresentableProperty { property } => {
                write!(f, "commit.unrepresentable_property: {property}")
            }
            Self::Encode(detail) => write!(f, "result could not be encoded: {detail}"),
        }
    }
}

impl std::error::Error for AbiError {}

fn invalid<E: fmt::Display>(argument: &'static str) -> impl Fn(E) -> AbiError {
    move |error| AbiError::InvalidInput {
        argument,
        detail: error.to_string(),
    }
}

fn safe(field: &'static str, value: u64) -> Result<u64, AbiError> {
    if value > MAX_SAFE_INTEGER {
        Err(AbiError::UnsafeInteger { field, value })
    } else {
        Ok(value)
    }
}

fn to_text(value: &impl Serialize) -> Result<String, AbiError> {
    serde_json::to_string(value).map_err(|error| AbiError::Encode(error.to_string()))
}

// ------------------------------------------------------------------ wire

/// Property values: a JSON object, carried as its exact bytes.
#[derive(Debug)]
struct ObjectText(Box<RawValue>);

/// Checks that a raw value is a JSON object with unique member names,
/// without converting any number.
struct UniqueMembers;

impl<'de> Visitor<'de> for UniqueMembers {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object of property values")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = BTreeSet::new();
        while let Some(name) = map.next_key::<String>()? {
            map.next_value::<IgnoredAny>()?;
            if !seen.insert(name.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate property `{name}` in values"
                )));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ObjectText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let mut inner = serde_json::Deserializer::from_str(raw.get());
        inner
            .deserialize_map(UniqueMembers)
            .and_then(|()| inner.end())
            .map_err(de::Error::custom)?;
        Ok(Self(raw))
    }
}

/// `FactMutation` with the values kept as their exact bytes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum WireMutation {
    Put { values: ObjectText },
    Delete,
}

/// `FactWrite` with the values kept as their exact bytes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWrite {
    key: FactKey,
    expected: FactVersion,
    mutation: WireMutation,
}

/// `LinkMutation` with the values kept as their exact bytes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum WireLinkMutation {
    Add { values: ObjectText },
    Remove,
}

/// `LinkWrite` with the values kept as their exact bytes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLink {
    key: LinkKey,
    expected: FactVersion,
    mutation: WireLinkMutation,
}

/// `Effect` with the command and timer kept as their exact bytes; each is
/// then decoded with the kernel's `Command` / `Timer` types.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireEffect {
    Perform(Box<RawValue>),
    Schedule(Box<RawValue>),
    CancelTimer(TimerId),
}

/// `CellCommit` with every payload kept as its exact bytes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCommit {
    consumes: Option<Consumed>,
    reads: Vec<FactExpectation>,
    writes: Vec<WireWrite>,
    events: Vec<Box<RawValue>>,
    effects: Vec<WireEffect>,
    #[serde(default)]
    links: Vec<WireLink>,
    #[serde(default)]
    link_reads: Vec<norn_kernel::LinkReadExpectation>,
}

impl WireCommit {
    fn decode(commit: &str) -> Result<Self, AbiError> {
        serde_json::from_str(commit).map_err(invalid("commit"))
    }

    /// Every canonical object the check reads state of, deduplicated, in
    /// first-mention order: reads, writes, and link endpoints.
    fn objects(&self, model: CommitModel<'_, '_>) -> Vec<FactKey> {
        let mut objects: Vec<FactKey> = Vec::new();
        let keys = self
            .reads
            .iter()
            .map(|read| &read.key)
            .chain(self.writes.iter().map(|write| &write.key))
            .chain(
                self.links
                    .iter()
                    .flat_map(|link| [&link.key.source, &link.key.target]),
            );
        for key in keys {
            let canonical = model.canonical(key);
            if !objects.contains(&canonical) {
                objects.push(canonical);
            }
        }
        objects
    }

    /// Admit declared fact and link fields through the native kernel checks.
    /// Undeclared payload fields, events and effects stay opaque.
    fn admission(
        &self,
        catalog: &TypeCatalog<'_>,
        lookup: &ViewLookup,
    ) -> Result<CellCommit<(), (), ()>, AbiError> {
        Ok(CellCommit {
            consumes: self.consumes.clone(),
            reads: self.reads.clone(),
            writes: self
                .writes
                .iter()
                .map(|write| {
                    Ok(FactWrite {
                        key: write.key.clone(),
                        expected: write.expected,
                        mutation: match &write.mutation {
                            WireMutation::Put { values } => {
                                let declared = catalog
                                    .effective_object_values(
                                        &write.key.object_type,
                                        &JsonObject::new(),
                                    )
                                    .map_err(invalid("model"))?;
                                let values = values.project(&declared)?;
                                FactMutation::Put { values }
                            }
                            WireMutation::Delete => FactMutation::Delete,
                        },
                    })
                })
                .collect::<Result<_, AbiError>>()?,
            events: Vec::new(),
            effects: Vec::new(),
            links: self
                .links
                .iter()
                .map(|link| {
                    Ok(LinkWrite {
                        key: link.key.clone(),
                        expected: link.expected,
                        mutation: match &link.mutation {
                            WireLinkMutation::Add { values } => {
                                let model = CommitModel::new(catalog);
                                let canonical = model.canonical(&link.key.source);
                                let source = self
                                    .writes
                                    .iter()
                                    .find(|write| model.canonical(&write.key) == canonical)
                                    .and_then(|write| match write.mutation {
                                        WireMutation::Put { .. } => Some(&write.key.object_type),
                                        WireMutation::Delete => None,
                                    })
                                    .or_else(|| {
                                        lookup
                                            .objects
                                            .get(&canonical)
                                            .and_then(|object| object.concrete.as_ref())
                                    })
                                    .unwrap_or(&link.key.source.object_type);
                                let declared = catalog
                                    .effective_link_values(
                                        source,
                                        link.key.link.as_str(),
                                        &JsonObject::new(),
                                    )
                                    .map_err(invalid("model"))?;
                                LinkMutation::Add {
                                    values: values.project(&declared)?,
                                }
                            }
                            WireLinkMutation::Remove => LinkMutation::Remove,
                        },
                    })
                })
                .collect::<Result<_, AbiError>>()?,
            link_reads: self.link_reads.clone(),
        })
    }
}

impl ObjectText {
    /// Build the kernel's canonical stored numeric values and defaults while
    /// retaining opaque payloads as raw JSON, including numbers outside the
    /// runtime decimal range on unconstrained properties.
    fn effective_values_text(
        &self,
        catalog: &TypeCatalog<'_>,
        object_type: &ObjectTypeName,
    ) -> Result<Cow<'_, str>, AbiError> {
        let mut raw: BTreeMap<String, Box<RawValue>> =
            serde_json::from_str(self.0.get()).map_err(invalid("commit"))?;
        let submitted = raw
            .iter()
            .map(|(name, value)| {
                let value = if catalog.is_numeric_property(object_type, name) {
                    RuntimeValue::from_json_text(value.get()).map_err(invalid("commit"))?
                } else {
                    RuntimeValue::Null
                };
                Ok((name.clone(), value))
            })
            .collect::<Result<JsonObject, AbiError>>()?;
        let effective = catalog
            .effective_object_values(object_type, &submitted)
            .map_err(invalid("model"))?;
        let mut changed = false;
        for (name, value) in effective {
            if !raw.contains_key(&name) || catalog.is_numeric_property(object_type, &name) {
                let text = String::from_utf8(value.canonical_json().map_err(invalid("model"))?)
                    .map_err(invalid("model"))?;
                if raw.get(&name).is_none_or(|original| original.get() != text) {
                    raw.insert(name, RawValue::from_string(text).map_err(invalid("model"))?);
                    changed = true;
                }
            }
        }
        if changed {
            to_text(&raw).map(Cow::Owned)
        } else {
            Ok(Cow::Borrowed(self.0.get()))
        }
    }

    /// Decode only declared stored fields; never parse opaque numeric payloads.
    fn project(&self, declared: &JsonObject) -> Result<JsonObject, AbiError> {
        let raw: BTreeMap<String, Box<RawValue>> =
            serde_json::from_str(self.0.get()).map_err(invalid("commit"))?;
        raw.into_iter()
            .filter(|(name, _)| declared.contains_key(name))
            .map(|(name, value)| {
                RuntimeValue::from_json_text(value.get())
                    .map(|value| (name.clone(), value))
                    .map_err(|_| AbiError::UnrepresentableProperty { property: name })
            })
            .collect()
    }
}

// ------------------------------------------------------------------ model

/// The cell's object types and interfaces, the model every commit is checked
/// against. The object-type array form remains accepted for ABI 3 callers
/// whose models have no interface definitions.
struct DecodedModel {
    object_types: Vec<ObjectTypeDef>,
    interfaces: Vec<InterfaceDef>,
    property_types: Vec<PropertyTypeDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StructuredModel {
    object_types: Vec<ObjectTypeDef>,
    #[serde(default)]
    interfaces: Vec<InterfaceDef>,
    #[serde(default)]
    property_types: Vec<PropertyTypeDef>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ModelInput {
    Legacy(Vec<ObjectTypeDef>),
    Structured(StructuredModel),
}

fn decode_model(model: &str) -> Result<DecodedModel, AbiError> {
    let decoded: ModelInput = serde_json::from_str(model).map_err(invalid("model"))?;
    Ok(match decoded {
        ModelInput::Legacy(object_types) => DecodedModel {
            object_types,
            interfaces: Vec::new(),
            property_types: Vec::new(),
        },
        ModelInput::Structured(model) => DecodedModel {
            object_types: model.object_types,
            interfaces: model.interfaces,
            property_types: model.property_types,
        },
    })
}

// ------------------------------------------------------------------ plan

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PlanConsumed<'a> {
    Timer { id: &'a str, incarnation: u64 },
    Result { command: &'a str, generation: u64 },
    Observation { id: &'a str },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanWrite<'a> {
    /// The canonical object (hierarchy root and key).
    object: FactKey,
    /// The type the object is written as (its concrete type after a put).
    object_type: &'a str,
    /// The exact property-values text; `None` for a delete.
    values: Option<Cow<'a, str>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanLink<'a> {
    /// The canonical link.
    link: LinkKey,
    /// The exact link-values text; `None` for a remove.
    values: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanLinkRead {
    /// The canonical source object whose adjacency set was read.
    source: FactKey,
    /// The declared link name.
    link: String,
    /// Maximum complete target count admitted by the current slice profile.
    max_records: u64,
    /// Maximum aggregate UTF-8 target-key bytes admitted by the slice profile.
    max_canonical_bytes: u64,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PlanEffect<'a> {
    #[serde(rename_all = "camelCase")]
    Perform {
        id: String,
        generation: u64,
        idempotency_key: String,
        deadline: Option<u64>,
        /// The exact `Command` text.
        command: &'a str,
    },
    #[serde(rename_all = "camelCase")]
    Schedule {
        id: String,
        due: u64,
        /// The exact `Timer` text.
        timer: &'a str,
    },
    #[serde(rename_all = "camelCase")]
    CancelTimer { id: &'a str },
}

#[derive(Serialize)]
struct Plan<'a> {
    format: &'static str,
    consumes: Option<PlanConsumed<'a>>,
    objects: Vec<FactKey>,
    writes: Vec<PlanWrite<'a>>,
    links: Vec<PlanLink<'a>>,
    #[serde(rename = "linkReads")]
    link_reads: Vec<PlanLinkRead>,
    effects: Vec<PlanEffect<'a>>,
}

fn plan_consumed(consumed: &Consumed) -> Result<PlanConsumed<'_>, AbiError> {
    Ok(match consumed {
        Consumed::Timer { id, incarnation } => PlanConsumed::Timer {
            id: &id.0,
            incarnation: safe("consumes.incarnation", incarnation.0)?,
        },
        Consumed::Result {
            command,
            generation,
        } => PlanConsumed::Result {
            command: &command.0,
            generation: safe("consumes.generation", generation.0)?,
        },
        Consumed::Observation { id } => PlanConsumed::Observation { id: &id.0 },
    })
}

fn plan_effect(effect: &WireEffect) -> Result<PlanEffect<'_>, AbiError> {
    Ok(match effect {
        WireEffect::Perform(raw) => {
            let command: Command<Box<RawValue>> =
                serde_json::from_str(raw.get()).map_err(invalid("commit.effects.perform"))?;
            PlanEffect::Perform {
                id: command.id.0,
                generation: safe("perform.generation", command.generation.0)?,
                idempotency_key: command.idempotency_key.0,
                deadline: command
                    .deadline
                    .map(|deadline| safe("perform.deadline", deadline.as_millis()))
                    .transpose()?,
                command: raw.get(),
            }
        }
        WireEffect::Schedule(raw) => {
            let timer: Timer<Box<RawValue>> =
                serde_json::from_str(raw.get()).map_err(invalid("commit.effects.schedule"))?;
            PlanEffect::Schedule {
                id: timer.id.0,
                due: safe("schedule.due", timer.due.as_millis())?,
                timer: raw.get(),
            }
        }
        WireEffect::CancelTimer(id) => PlanEffect::CancelTimer { id: &id.0 },
    })
}

/// Decodes a `CellCommit` (JSON text) into the shim's plan, against the
/// cell's model (an `ObjectTypeDef` array or an object containing
/// `objectTypes`, `interfaces`, and optional `propertyTypes`).
///
/// Result: `{format: "norn-cell-abi/plan@4", consumes, objects, writes,
/// links, linkReads, effects}` where
///
/// - `consumes` is `null` or `{kind: "timer", id, incarnation}`,
///   `{kind: "result", command, generation}`, `{kind: "observation", id}`;
/// - `objects` is every canonical `{objectType, key}` (hierarchy root and
///   key) the check reads state of, in order: [`check_commit_json`]'s view
///   answers each;
/// - `writes` is `{object, objectType, values}` per object write: the
///   canonical object, the type it is written as, and the exact
///   property-values text or `null` for a delete;
/// - `links` is `{link, values}` per link write: the canonical `LinkKey` and
///   the exact link-values text or `null` for a remove;
/// - `linkReads` is `{source, link, maxRecords, maxCanonicalBytes}` per
///   complete adjacency read, with canonical source keys and the slice
///   profile bounds the shim enforces while reading its transaction;
/// - `effects` is, in order, `{kind: "perform", id, generation,
///   idempotencyKey, deadline, command}`, `{kind: "schedule", id, due,
///   timer}`, or `{kind: "cancel_timer", id}`, with `command` / `timer` the
///   exact text of the kernel `Command` / `Timer`.
///
/// # Errors
///
/// [`AbiError`] if the commit is not a well-formed `CellCommit`, the model
/// has an invalid model shape, or an integer exceeds
/// [`MAX_SAFE_INTEGER`].
pub fn decode_commit_json(commit: &str, model: &str) -> Result<String, AbiError> {
    let wire = WireCommit::decode(commit)?;
    let model = decode_model(model)?;
    let catalog = TypeCatalog::new(&model.object_types)
        .with_interfaces(&model.interfaces)
        .with_property_types(&model.property_types);
    let model = CommitModel::new(&catalog);
    let slice_limits = norn_kernel::ResourceLimitProfile::norn_limits_v1().slice;
    for read in &wire.reads {
        safe("reads.expected", read.expected.0)?;
    }
    for write in &wire.writes {
        safe("writes.expected", write.expected.0)?;
    }
    for link in &wire.links {
        safe("links.expected", link.expected.0)?;
    }
    for read in &wire.link_reads {
        for target in &read.targets {
            safe("link_reads.targets.version", target.version.0)?;
        }
    }
    let plan = Plan {
        format: PLAN_FORMAT,
        consumes: wire.consumes.as_ref().map(plan_consumed).transpose()?,
        objects: wire.objects(model),
        writes: wire
            .writes
            .iter()
            .map(|write| {
                Ok(PlanWrite {
                    object: model.canonical(&write.key),
                    object_type: write.key.object_type.as_str(),
                    values: match &write.mutation {
                        WireMutation::Put { values } => {
                            Some(values.effective_values_text(&catalog, &write.key.object_type)?)
                        }
                        WireMutation::Delete => None,
                    },
                })
            })
            .collect::<Result<_, AbiError>>()?,
        links: wire
            .links
            .iter()
            .map(|link| PlanLink {
                link: model.canonical_link(&link.key),
                values: match &link.mutation {
                    WireLinkMutation::Add { values } => Some(values.0.get()),
                    WireLinkMutation::Remove => None,
                },
            })
            .collect(),
        link_reads: wire
            .link_reads
            .iter()
            .map(|read| PlanLinkRead {
                source: model.canonical(&read.source),
                link: read.link.clone(),
                max_records: u64::from(slice_limits.max_records),
                max_canonical_bytes: slice_limits.max_canonical_bytes,
            })
            .collect(),
        effects: wire
            .effects
            .iter()
            .map(plan_effect)
            .collect::<Result<_, _>>()?,
    };
    to_text(&plan)
}

// ------------------------------------------------------------------ verdict

/// What the shim read about one canonical object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectView {
    /// Its version (`0` if never written).
    version: u64,
    /// Its live concrete type; `null` when it does not exist.
    concrete: Option<ObjectTypeName>,
    /// Its live links (canonical), either direction.
    links: Vec<LinkKey>,
}

/// What the shim read inside its commit transaction.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct View {
    /// The cell's current ledger position (`0` before the first commit).
    seq: u64,
    /// Whether the consumed delivery is pending (ignored without one).
    pending: bool,
    /// Each plan object's state, in plan order.
    objects: Vec<ObjectView>,
    /// The current version of each plan link, in plan order.
    link_versions: Vec<u64>,
    /// Complete current adjacency sets in plan link-read order; `null` means
    /// the host could not provide a complete bounded set.
    link_reads: Vec<Option<Vec<LinkReadTarget>>>,
    #[serde(default)]
    implicit_objects: Vec<ImplicitObject>,
    #[serde(default)]
    implicit_link_reads: Vec<ImplicitLinkRead>,
    #[serde(default)]
    implicit_presence_reads: Vec<ImplicitPresenceRead>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImplicitObject {
    key: FactKey,
    state: ObjectView,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImplicitLinkRead {
    source: FactKey,
    link: String,
    targets: Option<Vec<LinkReadTarget>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImplicitPresenceRead {
    source: FactKey,
    link: String,
    excluded: Vec<FactKey>,
    present: Option<bool>,
}
type PresenceReads = BTreeMap<(FactKey, String, Vec<FactKey>), Option<bool>>;

/// A missing host read discovered by the kernel, before any mutation.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReadRequest {
    Object {
        key: FactKey,
    },
    LinkTargets {
        source: FactKey,
        link: String,
    },
    LinkPresence {
        source: FactKey,
        link: String,
        excluded: Vec<FactKey>,
    },
}

impl fmt::Display for ReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kernel admission requires an additional host read")
    }
}

/// The view as the kernel's [`CommitView`], by canonical key.
struct ViewLookup {
    pending: bool,
    objects: BTreeMap<FactKey, ObjectView>,
    links: BTreeMap<LinkKey, FactVersion>,
    link_reads: BTreeMap<(FactKey, String), Option<Vec<LinkReadTarget>>>,
    presence_reads: PresenceReads,
}

impl CommitView for ViewLookup {
    type Error = ReadRequest;

    fn pending(&self, _: &Consumed) -> Result<bool, ReadRequest> {
        Ok(self.pending)
    }

    fn version(&self, key: &FactKey) -> Result<FactVersion, ReadRequest> {
        Ok(self
            .objects
            .get(key)
            .map_or(FactVersion::ABSENT, |object| FactVersion(object.version)))
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, ReadRequest> {
        self.objects
            .get(key)
            .map(|object| object.concrete.clone())
            .ok_or_else(|| ReadRequest::Object { key: key.clone() })
    }

    fn link_version(&self, key: &LinkKey) -> Result<FactVersion, ReadRequest> {
        Ok(self.links.get(key).copied().unwrap_or(FactVersion::ABSENT))
    }

    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, ReadRequest> {
        Ok(self
            .objects
            .get(key)
            .map(|object| object.links.clone())
            .unwrap_or_default())
    }

    fn has_link_target(
        &self,
        source: &FactKey,
        link: &str,
        excluded: &BTreeSet<FactKey>,
    ) -> Result<Option<bool>, ReadRequest> {
        if let Some(targets) = self.link_reads.get(&(source.clone(), link.to_owned())) {
            return Ok(targets.as_ref().map(|targets| {
                targets
                    .iter()
                    .any(|target| !excluded.contains(&target.key.target))
            }));
        }
        let cap = usize::try_from(
            norn_kernel::ResourceLimitProfile::norn_limits_v1()
                .slice
                .max_records,
        )
        .unwrap_or(usize::MAX);
        if excluded.len() > cap {
            return Ok(None);
        }
        let excluded: Vec<_> = excluded.iter().cloned().collect();
        self.presence_reads
            .get(&(source.clone(), link.to_owned(), excluded.clone()))
            .copied()
            .ok_or_else(|| ReadRequest::LinkPresence {
                source: source.clone(),
                link: link.to_owned(),
                excluded,
            })
    }

    fn link_targets(
        &self,
        source: &FactKey,
        link: &str,
    ) -> Result<Option<Vec<LinkReadTarget>>, ReadRequest> {
        self.link_reads
            .get(&(source.clone(), link.to_owned()))
            .cloned()
            .ok_or_else(|| ReadRequest::LinkTargets {
                source: source.clone(),
                link: link.to_owned(),
            })
    }
}

/// One committed ledger entry, as the shim stores it.
#[derive(Serialize)]
struct Entry<'a> {
    seq: LedgerSeq,
    consumed: Option<&'a Consumed>,
    written: &'a [WrittenVersion],
    linked: &'a [WrittenLinkVersion],
    events: &'a [Box<RawValue>],
}

/// The receipt, as the kernel serializes `CommitReceipt`.
#[derive(Serialize)]
struct Receipt<'a> {
    seq: LedgerSeq,
    written: &'a [WrittenVersion],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    linked: &'a [WrittenLinkVersion],
}

/// The refusal document of a commit error: `{code, message}` plus the
/// error's fields (`key`, `link`, `missing`, `endpoint`, `concrete`,
/// `expected`, `current`, `consumed`, `detail`, and typed invariant details).
///
/// Every host reports a refused commit with this document, so the shim's
/// refusals can be compared with the reference host's.
#[must_use]
pub fn refusal_value<X: fmt::Display>(error: &CommitError<X>) -> Value {
    let mut doc = Map::new();
    doc.insert("code".to_owned(), Value::from(error.code()));
    doc.insert("message".to_owned(), Value::from(error.to_string()));
    let fields = match error {
        CommitError::OwnedRowsProtected { key } | CommitError::DuplicateKey { key } => {
            json!({ "key": key })
        }
        CommitError::NotPending { consumed } => json!({ "consumed": consumed }),
        CommitError::Conflict {
            key,
            expected,
            current,
        } => json!({ "key": key, "expected": expected, "current": current }),
        CommitError::DuplicateLink { key } | CommitError::LinkTarget { key } => {
            json!({ "link": key })
        }
        CommitError::LinkConflict {
            key,
            expected,
            current,
        } => json!({ "link": key, "expected": expected, "current": current }),
        CommitError::LinkReadConflict { source_key, link }
        | CommitError::UnsupportedLinkRead { source_key, link }
        | CommitError::DuplicateLinkRead { source_key, link }
        | CommitError::InvalidLinkRead { source_key, link } => {
            json!({ "sourceKey": source_key, "link": link })
        }
        CommitError::LinkReadTooLarge {
            source_key,
            link,
            actual,
            max,
        } => json!({ "sourceKey": source_key, "link": link, "actual": actual, "max": max }),
        CommitError::DanglingLink { key, missing } => json!({ "link": key, "missing": missing }),
        CommitError::LinkCardinality { key, endpoint } => {
            json!({ "link": key, "endpoint": endpoint })
        }
        CommitError::LinkRequired { parent, link } => json!({ "parent": parent, "link": link }),
        CommitError::TypeMismatch { key, concrete } => {
            json!({ "key": key, "concrete": concrete })
        }
        CommitError::TraversalLimitExceeded { key, reason }
        | CommitError::EffectiveValues { key, reason } => json!({ "key": key, "reason": reason }),
        CommitError::IdentifierTooLong {
            field,
            actual_bytes,
            max_bytes,
        } => json!({ "field": field, "actualBytes": actual_bytes, "maxBytes": max_bytes }),
        CommitError::AccountingPosting { key, reason } => {
            json!({ "key": key, "reason": reason })
        }
        CommitError::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        } => json!({
            "key": key,
            "invariantOwner": owner,
            "invariantId": id,
            "invariantMessage": message,
            "outcome": outcome,
        }),
        CommitError::InvariantBudgetExceeded { key, limit } => {
            json!({ "key": key, "limit": limit })
        }
        CommitError::InvariantModel { key, interface } => {
            json!({ "key": key, "interface": interface })
        }
        CommitError::ObservationAuthorization | CommitError::Overflow => json!({}),
        CommitError::EventLimitExceeded { limit, actual } => {
            json!({ "limit": limit, "actual": actual })
        }
        CommitError::Host(detail) => json!({ "detail": detail.to_string() }),
    };
    if let Value::Object(fields) = fields {
        doc.extend(fields);
    }
    Value::Object(doc)
}

fn refused<X: fmt::Display>(error: &CommitError<X>) -> Result<String, AbiError> {
    to_text(&json!({
        "format": VERDICT_FORMAT,
        "accepted": false,
        "refusal": refusal_value(error),
    }))
}

/// The view as the kernel's [`CommitView`], checked against the plan (one
/// object state per plan object, one version per plan link, and one bounded
/// adjacency snapshot per plan link read).
fn lookup(
    wire: &WireCommit,
    model: CommitModel<'_, '_>,
    view: View,
) -> Result<ViewLookup, AbiError> {
    let objects = wire.objects(model);
    if objects.len() != view.objects.len()
        || wire.links.len() != view.link_versions.len()
        || wire.link_reads.len() != view.link_reads.len()
    {
        return Err(AbiError::InvalidInput {
            argument: "view",
            detail: format!(
                "{} object states for {} plan objects, {} link versions for {} plan links, {} link-read snapshots for {} plan link reads",
                view.objects.len(),
                objects.len(),
                view.link_versions.len(),
                wire.links.len(),
                view.link_reads.len(),
                wire.link_reads.len()
            ),
        });
    }
    for object in &view.objects {
        safe("view.objects.version", object.version)?;
    }
    let mut links = BTreeMap::new();
    for (link, version) in wire.links.iter().zip(&view.link_versions) {
        links
            .entry(model.canonical_link(&link.key))
            .or_insert(FactVersion(safe("view.linkVersions", *version)?));
    }
    let slice_limits = norn_kernel::ResourceLimitProfile::norn_limits_v1().slice;
    let mut link_reads = BTreeMap::new();
    for (read, targets) in wire.link_reads.iter().zip(view.link_reads) {
        let source = model.canonical(&read.source);
        let targets = targets
            .map(|targets| checked_link_read_targets(read, &source, targets, model, slice_limits))
            .transpose()?;
        link_reads.insert((source, read.link.clone()), targets);
    }
    let mut objects: BTreeMap<_, _> = objects.into_iter().zip(view.objects).collect();
    add_implicit_reads(
        model,
        view.implicit_objects,
        view.implicit_link_reads,
        &mut objects,
        &mut link_reads,
    )?;
    let presence_reads = checked_presence_reads(model, view.implicit_presence_reads)?;
    Ok(ViewLookup {
        pending: view.pending,
        objects,
        links,
        link_reads,
        presence_reads,
    })
}

fn checked_presence_reads(
    model: CommitModel<'_, '_>,
    reads: Vec<ImplicitPresenceRead>,
) -> Result<PresenceReads, AbiError> {
    let limits = norn_kernel::ResourceLimitProfile::norn_limits_v1().slice;
    let cap = usize::try_from(limits.max_records).unwrap_or(usize::MAX);
    let mut checked = PresenceReads::new();
    if reads.len() > cap {
        return Err(invalid("view")(
            "implicit presence reads exceed the record budget",
        ));
    }
    for read in reads {
        let source = model.canonical(&read.source);
        let excluded: Vec<_> = read
            .excluded
            .iter()
            .map(|key| model.canonical(key))
            .collect();
        let bytes = excluded.iter().fold(source.key.len(), |total, key| {
            total.saturating_add(key.key.len())
        });
        if excluded.len() > cap
            || u64::try_from(bytes).unwrap_or(u64::MAX) > limits.max_canonical_bytes
            || excluded
                .windows(2)
                .any(|keys| matches!(keys, [left, right] if left >= right))
        {
            return Err(invalid("view")(
                "implicit presence exclusions are unordered or over budget",
            ));
        }
        if checked
            .insert((source, read.link, excluded), read.present)
            .is_some()
        {
            return Err(invalid("view")("duplicate implicit presence read"));
        }
    }
    Ok(checked)
}

fn add_implicit_reads(
    model: CommitModel<'_, '_>,
    extra_objects: Vec<ImplicitObject>,
    extra_links: Vec<ImplicitLinkRead>,
    objects: &mut BTreeMap<FactKey, ObjectView>,
    links: &mut BTreeMap<(FactKey, String), Option<Vec<LinkReadTarget>>>,
) -> Result<(), AbiError> {
    let limits = norn_kernel::ResourceLimitProfile::norn_limits_v1().slice;
    let max = usize::try_from(limits.max_records).unwrap_or(usize::MAX);
    if extra_objects.len() > max || extra_links.len() > max {
        return Err(invalid("view")(
            "implicit reads exceed the slice record limit",
        ));
    }
    let mut bytes = 0_usize;
    for object in extra_objects {
        safe("view.implicitObjects.version", object.state.version)?;
        let key = model.canonical(&object.key);
        bytes = bytes.saturating_add(key.key.len());
        if objects.insert(key, object.state).is_some() {
            return Err(invalid("view")("duplicate implicit object read"));
        }
    }
    for read in extra_links {
        let source = model.canonical(&read.source);
        bytes = bytes.saturating_add(source.key.len());
        let expected = norn_kernel::LinkReadExpectation {
            source: source.clone(),
            link: read.link.clone(),
            targets: Vec::new(),
        };
        let targets = read
            .targets
            .map(|targets| checked_link_read_targets(&expected, &source, targets, model, limits))
            .transpose()?;
        bytes = bytes.saturating_add(targets.as_ref().map_or(0, |targets| {
            targets
                .iter()
                .map(|target| target.key.target.key.len())
                .sum()
        }));
        if links.insert((source, read.link), targets).is_some() {
            return Err(invalid("view")("duplicate implicit adjacency read"));
        }
    }
    if u64::try_from(bytes).unwrap_or(u64::MAX) > limits.max_canonical_bytes {
        return Err(invalid("view")(
            "implicit reads exceed the slice byte limit",
        ));
    }
    Ok(())
}

fn checked_link_read_targets(
    read: &norn_kernel::LinkReadExpectation,
    source: &FactKey,
    targets: Vec<LinkReadTarget>,
    model: CommitModel<'_, '_>,
    limits: norn_kernel::SliceLimits,
) -> Result<Vec<LinkReadTarget>, AbiError> {
    let max_records = usize::try_from(limits.max_records).unwrap_or(usize::MAX);
    if targets.len() > max_records {
        return Err(AbiError::InvalidInput {
            argument: "view",
            detail: format!(
                "link-read snapshot has {} targets; slice profile allows at most {}",
                targets.len(),
                limits.max_records
            ),
        });
    }

    let mut checked = Vec::with_capacity(targets.len());
    let mut previous: Option<LinkKey> = None;
    let mut target_key_bytes = 0_usize;
    for target in targets {
        safe("view.linkReads.version", target.version.0)?;
        if target.key.source != *source || target.key.link != read.link {
            return Err(AbiError::InvalidInput {
                argument: "view",
                detail: "link-read target does not belong to the planned source and link"
                    .to_owned(),
            });
        }
        let key = LinkKey::new(
            source.clone(),
            read.link.clone(),
            model.canonical(&target.key.target),
        );
        if previous.as_ref().is_some_and(|prior| prior >= &key) {
            return Err(AbiError::InvalidInput {
                argument: "view",
                detail: "link-read targets must be unique and in canonical order".to_owned(),
            });
        }
        target_key_bytes = target_key_bytes.saturating_add(key.target.key.len());
        let target_key_bytes_u64 = u64::try_from(target_key_bytes).unwrap_or(u64::MAX);
        if target_key_bytes_u64 > limits.max_canonical_bytes {
            return Err(AbiError::InvalidInput {
                argument: "view",
                detail: format!(
                    "link-read target keys exceed the slice profile's {} byte limit",
                    limits.max_canonical_bytes
                ),
            });
        }
        previous = Some(key.clone());
        checked.push(LinkReadTarget {
            key,
            version: target.version,
        });
    }
    Ok(checked)
}

/// Runs the kernel's admission check on a commit against what the shim read
/// and the cell's model.
///
/// `view` is `{seq, pending, objects, linkVersions, linkReads}`: the cell's current
/// ledger position, whether the consumed delivery is pending, for each of
/// [`decode_commit_json`]'s `objects` its state `{version, concrete,
/// links}` (`concrete` `null` when it does not exist, `links` its live
/// canonical `LinkKey`s), the current version of each of its `links`, in
/// plan order, and each `linkReads` entry as a complete ordered array of
/// `{key, version}` targets or `null` when unavailable. `model` is the same
/// object-type list.
///
/// Result, refused: `{format: "norn-cell-abi/verdict@4", accepted: false,
/// refusal}` with [`refusal_value`]'s document. Accepted: `{format,
/// accepted: true, seq, written: [{version}], linked: [{version}], receipt,
/// entry}` where `seq` is the commit's ledger position, `written` / `linked`
/// the new version of each object / link write in order, `receipt` the
/// `CommitReceipt` text and `entry` the ledger-entry text `{seq, consumed,
/// written, linked, events}` (events as their exact bytes).
///
/// # Errors
///
/// [`AbiError`] if an argument is malformed, the view does not match the
/// plan, or an integer exceeds [`MAX_SAFE_INTEGER`].
pub fn check_commit_json(commit: &str, view: &str, model: &str) -> Result<String, AbiError> {
    let wire = WireCommit::decode(commit)?;
    let view: View = serde_json::from_str(view).map_err(invalid("view"))?;
    let model = decode_model(model)?;
    let catalog = TypeCatalog::new(&model.object_types)
        .with_interfaces(&model.interfaces)
        .with_property_types(&model.property_types);
    let model = CommitModel::new(&catalog);
    let seq_now = safe("view.seq", view.seq)?;
    let lookup = lookup(&wire, model, view)?;
    let admission = match wire.admission(&catalog, &lookup) {
        Ok(admission) => admission,
        Err(AbiError::UnrepresentableProperty { property }) => {
            return to_text(&json!({ "format": VERDICT_FORMAT, "accepted": false,
                "refusal": { "code": "commit.unrepresentable_property", "property": property } }));
        }
        Err(error) => return Err(error),
    };
    let admitted = match check_commit(&admission, &model, &lookup) {
        Ok(admitted) => admitted,
        Err(CommitError::Host(request)) => {
            let slice = norn_kernel::ResourceLimitProfile::norn_limits_v1().slice;
            return to_text(&json!({ "format": VERDICT_FORMAT, "accepted": false,
                "refusal": refusal_value(&CommitError::Host(&request)), "readRequest": request,
                "readLimits": { "maxRecords": slice.max_records, "maxCanonicalBytes": slice.max_canonical_bytes } }));
        }
        Err(error) => return refused(&error),
    };
    let written = admitted.written;
    let linked = admitted.linked;
    let Some(seq) = LedgerSeq(seq_now).checked_next() else {
        return refused(&CommitError::<ReadRequest>::Overflow);
    };
    safe("seq", seq.0)?;
    let mut versions = Vec::with_capacity(written.len());
    for row in &written {
        versions.push(json!({ "version": safe("written.version", row.version.0)? }));
    }
    let mut link_rows = Vec::with_capacity(linked.len());
    for row in &linked {
        link_rows.push(json!({ "version": safe("linked.version", row.version.0)? }));
    }
    let receipt = to_text(&Receipt {
        seq,
        written: &written,
        linked: &linked,
    })?;
    let entry = to_text(&Entry {
        seq,
        consumed: wire.consumes.as_ref(),
        written: &written,
        linked: &linked,
        events: &wire.events,
    })?;
    to_text(&json!({
        "format": VERDICT_FORMAT,
        "accepted": true,
        "seq": seq.0,
        "written": versions,
        "linked": link_rows,
        "receipt": receipt,
        "entry": entry,
    }))
}

/// The canonical identity of an object or link under the cell's model, and
/// whether an object of a stored concrete type is the requested type
/// (#3018): `{key, concrete}` → `{canonical, isA}`; `{link}` →
/// `{canonicalLink}`. The shim reads objects and links through it.
///
/// # Errors
///
/// [`AbiError`] if an argument is malformed.
pub fn resolve_key_json(request: &str, model: &str) -> Result<String, AbiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        key: Option<FactKey>,
        link: Option<LinkKey>,
        concrete: Option<ObjectTypeName>,
    }
    let request: Request = serde_json::from_str(request).map_err(invalid("request"))?;
    let model = decode_model(model)?;
    let catalog = TypeCatalog::new(&model.object_types)
        .with_interfaces(&model.interfaces)
        .with_property_types(&model.property_types);
    let model = CommitModel::new(&catalog);
    let mut answer = Map::new();
    if let Some(key) = &request.key {
        answer.insert("canonical".to_owned(), json!(model.canonical(key)));
        answer.insert(
            "isA".to_owned(),
            Value::Bool(
                request
                    .concrete
                    .as_ref()
                    .is_some_and(|concrete| model.is_a(concrete, &key.object_type)),
            ),
        );
    }
    if let Some(link) = &request.link {
        answer.insert(
            "canonicalLink".to_owned(),
            json!(model.canonical_link(link)),
        );
    }
    to_text(&Value::Object(answer))
}

// ------------------------------------------------------------------ inputs

/// Decodes an external observation (`Observed`, JSON text): the kernel shape
/// with mandatory provenance; the input stays opaque.
///
/// Result: `{id}`, the observation id the shim deduplicates on.
///
/// # Errors
///
/// [`AbiError`] if the text is not a well-formed `Observed`.
pub fn decode_observed_json(observed: &str) -> Result<String, AbiError> {
    let observed: Observed<Box<RawValue>> =
        serde_json::from_str(observed).map_err(invalid("observed"))?;
    to_text(&json!({ "id": observed.id.0 }))
}

/// Checks that a performed command's result is one JSON value (the kernel
/// input it maps to), without converting any number.
///
/// # Errors
///
/// [`AbiError`] if the text is not exactly one JSON value.
pub fn validate_input_json(input: &str) -> Result<(), AbiError> {
    serde_json::from_str::<Box<RawValue>>(input)
        .map(drop)
        .map_err(invalid("input"))
}

/// The ABI and crate versions:
/// `{abi, crateVersion, planFormat, verdictFormat, maxSafeInteger}`.
#[must_use]
pub fn version_json() -> String {
    json!({
        "abi": ABI_VERSION,
        "crateVersion": env!("CARGO_PKG_VERSION"),
        "planFormat": PLAN_FORMAT,
        "verdictFormat": VERDICT_FORMAT,
        "maxSafeInteger": MAX_SAFE_INTEGER,
    })
    .to_string()
}
