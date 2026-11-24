//! Generic IR-driven Model-Based Testing (MBT), Domain Model Coverage, and
//! Static Structural Gap Detection over any admitted Norn pack (#1656).
//!
//! This module takes any pack's admitted IR (`ObjectTypeDef`, `InterfaceDef`,
//! `PropertyTypeDef`, `ActionRoot`, `LawIR`, `AccessIr`, and optional
//! `LAW-EXAMPLE-1` examples) and executes four complementary verification
//! layers with honest evidence labels:
//!
//! 1. **Static Structural Gap Detection** ([`static_gaps`], `structural` /
//!    `exhaustive-finite`): dead-end non-terminal states, unreachable states,
//!    orphan fields, immutable financial/ledger types missing a compensating or
//!    reversal action, and high-impact actions lacking separation-of-duties
//!    (`requester != approver`) policies.
//! 2. **Domain Model Coverage Observation** ([`coverage`]): José Valim-style
//!    expected-vs-hit coverage tracking for every lifecycle transition,
//!    immutable refusal, governing law (`holds` + `violated`), and Cedar policy
//!    rule (`allow` + `deny`), plus gating assertions.
//! 3. **Deterministic IR-Driven MBT** ([`driver`], `exhaustive-finite` over
//!    declared lifecycles/policies + `runtime-enforced` `LAW-EXAMPLE-1` parity):
//!    all-states and all-transitions walks, disallowed transitions, mutations on
//!    every `immutable` state and entity (`IMMUTABLE_STATE_WRITE` + zero ledger
//!    writes), full Cedar & SoD matrix (`PermissionDenied` + zero ledger
//!    events), and `LAW-EXAMPLE-1` runtime parity against a real [`PackHost`].
//! 4. **Seeded Multi-Entity Stateful DST** ([`dst`], `sampled`): seeded,
//!    shrinkable `proptest` operation sequences with virtual clock steps
//!    (`#176`), random crash/reopen recovery, and `verify_ledger()` integrity
//!    checks (generalizing `#181`).

pub mod coverage;
pub mod driver;
pub mod dst;
pub mod static_gaps;
pub mod synth;

use std::collections::BTreeMap;

use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::norn_checker::law_examples::Verdict;
use norn_kernel::norn_ir::access::{AccessIr, PolicyRoot};
use norn_kernel::norn_ir::{
    ActionRoot, ArtifactEnvelope, ArtifactPayload, EntityRoot, InterfaceDef, LawIR, ObjectTypeDef,
    PropertyTypeDef,
};
use norn_kernel::{CellHost, FactEvent, LogicalTime, TypeCatalog};

/// Honest evidence classification for verification outputs (`AGENTS.md`, `#1656`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceLevel {
    /// Derived purely from static IR structure without executing runtime paths.
    Structural,
    /// Exhaustively covers a finite declared graph or policy/role matrix.
    ExhaustiveFinite,
    /// Verified by executing the real runtime commit path.
    RuntimeEnforced,
    /// Explored through seeded randomized multi-entity walks.
    Sampled,
}

impl EvidenceLevel {
    /// Canonical lowercase label matching `AGENTS.md`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::ExhaustiveFinite => "exhaustive-finite",
            Self::RuntimeEnforced => "runtime-enforced",
            Self::Sampled => "sampled",
        }
    }
}

/// One authored `LAW-EXAMPLE-1` case paired with its expected checker verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct LawExampleCase {
    /// Human-readable example name.
    pub name: String,
    /// Target `LawIR::id` inside the pack.
    pub law_id: String,
    /// Exact subject object values bound to `self`.
    pub subject: JsonObject,
    /// Verdict expected from both `norn-checker` and runtime replay.
    pub expected: Verdict,
}

/// Complete admitted pack IR consumed by static gap detection, MBT, and DST.
#[derive(Debug, Clone, Default)]
pub struct AdmittedPack {
    /// Pack identifier for diagnostics and coverage reports.
    pub name: String,
    /// Admitted EntityIR v2 object types.
    pub object_types: Vec<ObjectTypeDef>,
    /// Admitted EntityIR v2 interfaces.
    pub interfaces: Vec<InterfaceDef>,
    /// Shared named property types (`ref` targets).
    pub property_types: Vec<PropertyTypeDef>,
    /// Admitted ActionIR definitions with their artifact pins `(artifact_id, version, root)`.
    pub actions: Vec<AdmittedAction>,
    /// Retained governing law envelopes (passed directly to `TypeCatalog` / `Model`).
    pub law_envelopes: Vec<ArtifactEnvelope>,
    /// Decoded `LawIR` documents matching `law_envelopes`.
    pub laws: Vec<LawIR>,
    /// Admitted `AccessIR` policies lowered to Cedar.
    pub access_policies: Vec<AccessIr>,
    /// Optional inline/registered `LAW-EXAMPLE-1` cases to replay for runtime parity.
    pub law_examples: Vec<LawExampleCase>,
}

/// One admitted action pinned with its artifact identity and version.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedAction {
    /// Artifact id of the action envelope.
    pub artifact_id: String,
    /// Artifact version of the action envelope.
    pub version: String,
    /// Decoded `ActionRoot` payload.
    pub root: ActionRoot,
}

impl AdmittedPack {
    /// Extracts entities, interfaces, property types, actions, laws, and
    /// `AccessIR` policies from a slice of admitted [`ArtifactEnvelope`]s.
    ///
    /// # Errors
    ///
    /// Returns a human-readable decode error if a strict payload fails decoding.
    pub fn from_envelopes(
        name: impl Into<String>,
        envelopes: &[ArtifactEnvelope],
    ) -> Result<Self, String> {
        let mut pack = Self {
            name: name.into(),
            ..Self::default()
        };
        for envelope in envelopes {
            match &envelope.payload {
                ArtifactPayload::Entity(entity) => {
                    if !entity.is_entity_ir_v2() {
                        continue;
                    }
                    let root: EntityRoot = entity
                        .to_json_value()
                        .decode()
                        .map_err(|err| format!("{}: {err}", envelope.artifact_id))?;
                    match root {
                        EntityRoot::V2(v2) => {
                            if let Some(object_type) = v2.object_type {
                                pack.object_types.push(object_type);
                            }
                            if let Some(interface) = v2.interface {
                                pack.interfaces.push(interface);
                            }
                            if let Some(property_types) = v2.property_types {
                                pack.property_types.extend(property_types);
                            }
                        }
                        EntityRoot::V1(_) => {}
                    }
                }
                ArtifactPayload::Action(action) => {
                    let root: ActionRoot = action
                        .to_json_value()
                        .decode()
                        .map_err(|err| format!("{}: {err}", envelope.artifact_id))?;
                    pack.actions.push(AdmittedAction {
                        artifact_id: envelope.artifact_id.clone(),
                        version: envelope.version.clone(),
                        root,
                    });
                }
                ArtifactPayload::Law(law) => {
                    let decoded: LawIR = law
                        .to_json_value()
                        .decode()
                        .map_err(|err| format!("{}: {err}", envelope.artifact_id))?;
                    pack.law_envelopes.push(envelope.clone());
                    pack.laws.push(decoded);
                }
                ArtifactPayload::Policy(policy) => {
                    let root: PolicyRoot = policy
                        .to_json_value()
                        .decode()
                        .map_err(|err| format!("{}: {err}", envelope.artifact_id))?;
                    if let PolicyRoot::Access(access) = root {
                        pack.access_policies.push(access);
                    }
                }
                ArtifactPayload::Relation(_)
                | ArtifactPayload::Rule(_)
                | ArtifactPayload::Connector(_)
                | ArtifactPayload::Widget(_)
                | ArtifactPayload::Customization(_)
                | ArtifactPayload::Setting(_)
                | ArtifactPayload::BusinessPack(_)
                | ArtifactPayload::SourceSchema(_)
                | ArtifactPayload::Mapping(_)
                | ArtifactPayload::View(_)
                | ArtifactPayload::Metric(_)
                | ArtifactPayload::Function(_)
                | ArtifactPayload::Deontic(_)
                | ArtifactPayload::StructuredRequirement(_)
                | ArtifactPayload::Dataset(_)
                | ArtifactPayload::Source(_)
                | ArtifactPayload::Question(_)
                | ArtifactPayload::Transform(_)
                | ArtifactPayload::Flow(_)
                | ArtifactPayload::Plan(_)
                | ArtifactPayload::Workflow(_) => {}
            }
        }
        Ok(pack)
    }

    /// Attaches `LAW-EXAMPLE-1` cases to this pack.
    #[must_use]
    pub fn with_law_examples(mut self, examples: Vec<LawExampleCase>) -> Self {
        self.law_examples = examples;
        self
    }

    /// Returns object types indexed by canonical name.
    #[must_use]
    pub fn object_type_map(&self) -> BTreeMap<&str, &ObjectTypeDef> {
        self.object_types
            .iter()
            .map(|ty| (ty.name.as_str(), ty))
            .collect()
    }

    /// Returns a [`TypeCatalog`] wired with this pack's object types,
    /// interfaces, property types, and retained governing laws.
    ///
    /// Pass `action_call_type` if the caller also registers `norn.ActionCall`
    /// for governed `commit_action` execution.
    #[must_use]
    pub fn type_catalog<'a>(&'a self, extra_types: &'a [ObjectTypeDef]) -> TypeCatalog<'a> {
        TypeCatalog::new(self.object_types.iter().chain(extra_types.iter()))
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types)
            .with_governing_laws(&self.law_envelopes)
    }
}

/// Host abstraction driven by the generic pack MBT and DST harnesses.
///
/// Implemented for `MemoryCell` (in-memory reference host) and for file-backed
/// `SqliteCell` / `ObservedSqliteCell` (real SQLite durability, crash/reopen,
/// and `verify_ledger()`).
pub trait PackHost: CellHost<Event = FactEvent, Command = JsonObject, Input = JsonObject> {
    /// Current committed ledger entry count (used to prove zero ledger writes
    /// on every refused transition, immutable write, law violation, or Cedar
    /// denial).
    fn ledger_len(&self) -> usize;

    /// Current logical clock of the cell (`#176` virtual time).
    fn logical_now(&self) -> LogicalTime;

    /// Advances the cell's virtual clock by `delta_millis`.
    fn advance_clock(&mut self, delta_millis: u64);

    /// Verifies the full ledger hash chain and receipt integrity.
    ///
    /// # Errors
    ///
    /// Returns a description if the host's ledger integrity check fails.
    fn verify_ledger(&self) -> Result<(), String>;

    /// Simulates a process crash by closing and reopening durable storage,
    /// then verifying the ledger hash chain.
    ///
    /// # Errors
    ///
    /// Returns a description if reopening or ledger verification fails.
    fn crash_and_reopen(&mut self) -> Result<(), String>;
}

/// Builds the platform's synthetic `norn.ActionCall` object type required by
/// `commit_action` idempotency records.
#[must_use]
pub fn action_call_object_type() -> ObjectTypeDef {
    let text = |id: &str, nullable: bool| norn_kernel::norn_ir::Property {
        protected: false,
        id: norn_kernel::norn_ir::PropertyId(id.to_owned()),
        description: None,
        property_type: norn_kernel::norn_ir::PropertyType::Text {},
        constraints: norn_kernel::norn_ir::PropertyConstraints {
            nullable,
            ..norn_kernel::norn_ir::PropertyConstraints::default()
        },
        default: None,
        markings: Vec::new(),
        computed: None,
    };
    let json_ext = |id: &str| norn_kernel::norn_ir::Property {
        protected: false,
        id: norn_kernel::norn_ir::PropertyId(id.to_owned()),
        description: None,
        property_type: norn_kernel::norn_ir::PropertyType::JsonExt {},
        constraints: norn_kernel::norn_ir::PropertyConstraints {
            nullable: true,
            ..norn_kernel::norn_ir::PropertyConstraints::default()
        },
        default: None,
        markings: Vec::new(),
        computed: None,
    };
    let mut properties = vec![
        text("actionId", false),
        text("actor", false),
        text("turn", false),
        text("transition", false),
        text("from", false),
        text("to", false),
        text("actionArtifactId", true),
        text("actionVersion", true),
        text("effectKind", true),
        text("ruleBinding", true),
    ];
    for id in [
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "parameterPins",
        "rowOperations",
        "trace",
    ] {
        properties.push(json_ext(id));
    }
    ObjectTypeDef {
        immutable: false,
        name: norn_kernel::norn_ir::ObjectTypeName(norn_kernel::ACTION_CALL_TYPE.to_owned()),
        role: None,
        description: None,
        status: norn_kernel::norn_ir::ResourceStatus::Active,
        extends: None,
        implements: Vec::new(),
        properties,
        links: Vec::new(),
        owned_collections: Vec::new(),
        identity: Vec::new(),
        invariants: Vec::new(),
        lifecycles: Vec::new(),
        markings: Vec::new(),
        accounting_leg: None,
    }
}

/// Converts a `RuntimeValue` object into a plain `JsonObject` map for lookup.
#[must_use]
pub fn json_string(value: &str) -> JsonValue {
    JsonValue::String(value.to_owned())
}
