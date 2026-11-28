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
//!    writes), full Cedar & `SoD` matrix (`PermissionDenied` + zero ledger
//!    events), and `LAW-EXAMPLE-1` runtime parity against a real [`PackHost`].
//! 4. **Seeded Multi-Entity Stateful DST** ([`dst`], `sampled`): seeded,
//!    shrinkable `proptest` operation sequences with virtual clock steps
//!    (`#176`), random crash/reopen recovery, and `verify_ledger()` integrity
//!    checks (generalizing `#181`).

pub(crate) mod cedar;
pub mod coverage;
pub mod driver;
pub mod dst;
pub(crate) mod law_parity;
pub(crate) mod seed;
mod seed_precond;
pub mod static_gaps;
pub mod synth;

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{FactKey, JsonObject, JsonValue};
use norn_kernel::pack_mbt_support::MappingIr;
use norn_kernel::pack_mbt_support::{
    AccessIr, ActionRoot, ArtifactEnvelope, ArtifactPayload, DecisionOutput, EntityRoot,
    FactChange, InterfaceDef, LawExampleVerdict as Verdict, LawIR, Lifecycle, ObjectTypeDef,
    ObjectTypeName, PolicyRoot, Property, PropertyConstraints, PropertyId, PropertyType,
    PropertyTypeDef, ResourceStatus, TransitionId, ZenExpression,
};
use norn_kernel::{
    CellHost, DecisionRefusal, DecisionTurn, FactEvent, LogicalTime, TransitionGuard, TypeCatalog,
    World, commit_decision, load_slice, transition_guards,
};

/// Honest evidence classification for verification outputs (`AGENTS.md`, `#1656`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceLevel {
    /// Derived purely from static IR structure without executing runtime paths.
    Structural,
    /// Exhaustively covers a finite declared graph or policy/role matrix.
    ExhaustiveFinite,
    /// Bounded exploration where some declared targets could not be witnessed.
    Bounded,
    /// Verified by executing the real runtime commit path.
    RuntimeEnforced,
    /// Explored through seeded randomized multi-entity walks.
    Sampled,
    /// No runtime or static checks ran for this surface.
    Unknown,
}

impl EvidenceLevel {
    /// Canonical lowercase label matching `AGENTS.md`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::ExhaustiveFinite => "exhaustive-finite",
            Self::Bounded => "bounded",
            Self::RuntimeEnforced => "runtime-enforced",
            Self::Sampled => "sampled",
            Self::Unknown => "unknown",
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
    /// Admitted `EntityIR` v2 object types.
    pub object_types: Vec<ObjectTypeDef>,
    /// Admitted `EntityIR` v2 interfaces.
    pub interfaces: Vec<InterfaceDef>,
    /// Shared named property types (`ref` targets).
    pub property_types: Vec<PropertyTypeDef>,
    /// Admitted `ActionIR` definitions with their artifact pins `(artifact_id, version, root)`.
    pub actions: Vec<AdmittedAction>,
    /// Retained governing law envelopes (passed directly to `TypeCatalog` / `Model`).
    pub law_envelopes: Vec<ArtifactEnvelope>,
    /// Decoded `LawIR` documents matching `law_envelopes`.
    pub laws: Vec<LawIR>,
    /// Admitted `AccessIR` policies lowered to Cedar.
    pub access_policies: Vec<AccessIr>,
    /// Optional inline/registered `LAW-EXAMPLE-1` cases to replay for runtime parity.
    pub law_examples: Vec<LawExampleCase>,
    /// ACTOR-INDEPENDENT-1: the transition guards of `actions`, bound to every
    /// catalog [`Self::type_catalog`] builds, as a host binds them.
    pub guards: Vec<TransitionGuard>,
    /// Admitted `Mapping` writers: each target object type and the properties
    /// (and lifecycles) its object maps write from source evidence.
    pub mapping_writes: BTreeMap<String, BTreeSet<String>>,
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
        // A map names its target by entity artifact id; resolved to the type below.
        let mut entity_types = BTreeMap::new();
        let mut mapped: Vec<(String, BTreeSet<String>)> = Vec::new();
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
                            entity_types.insert(
                                envelope.artifact_id.clone(),
                                v2.object_type.name.as_str().to_owned(),
                            );
                            pack.object_types.push(v2.object_type);
                        }
                        EntityRoot::V2Interface(iface) => {
                            pack.interfaces.push(iface.interface);
                        }
                        EntityRoot::V2PropertyTypes(pt) => {
                            pack.property_types.extend(pt.property_types);
                        }
                        EntityRoot::V2Extension(_)
                        | EntityRoot::V2Markings(_)
                        | EntityRoot::V1(_) => {}
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
                ArtifactPayload::Mapping(mapping) => {
                    mapped.extend(mapped_fields(&mapping.to_json_value()));
                }
                ArtifactPayload::Relation(_)
                | ArtifactPayload::Rule(_)
                | ArtifactPayload::Connector(_)
                | ArtifactPayload::Widget(_)
                | ArtifactPayload::Customization(_)
                | ArtifactPayload::Setting(_)
                | ArtifactPayload::BusinessPack(_)
                | ArtifactPayload::Derived(_)
                | ArtifactPayload::SourceSchema(_)
                | ArtifactPayload::DefinedConcept(_)
                | ArtifactPayload::Labels(_)
                | ArtifactPayload::Model(_) => {}
            }
        }
        for (entity, properties) in mapped {
            let object_type = entity_types.get(&entity).cloned().unwrap_or(entity);
            pack.mapping_writes
                .entry(object_type)
                .or_default()
                .extend(properties);
        }
        pack.guards = transition_guards(pack.actions.iter().map(|action| &action.root));
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
    /// interfaces, property types, retained governing laws and transition
    /// guards (ACTOR-INDEPENDENT-1), so no caller forgets to bind them.
    ///
    /// Pass `action_call_type` if the caller also registers `norn.ActionCall`
    /// for governed `commit_action` execution.
    #[must_use]
    pub fn type_catalog<'a>(&'a self, extra_types: &'a [ObjectTypeDef]) -> TypeCatalog<'a> {
        TypeCatalog::new(self.object_types.iter().chain(extra_types.iter()))
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types)
            .with_governing_laws(&self.law_envelopes)
            .with_transition_guards(&self.guards)
    }
}

/// Each object map of an admitted mapping: its target entity artifact and the
/// properties and lifecycles it writes. A law-interpretation document (the
/// other `mapping` format) maps no source rows, so it writes nothing.
fn mapped_fields(
    payload: &norn_kernel::pack_mbt_support::IrJsonValue,
) -> Vec<(String, BTreeSet<String>)> {
    let Ok(decoded) = payload.decode::<MappingIr>() else {
        return Vec::new();
    };
    decoded
        .maps
        .into_iter()
        .map(|map| {
            let written = map
                .properties
                .into_iter()
                .map(|property| property.property.as_str().to_owned())
                .chain(
                    map.lifecycles
                        .into_iter()
                        .map(|lifecycle| lifecycle.property.as_str().to_owned()),
                )
                .collect();
            (map.target.entity, written)
        })
        .collect()
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

    /// Performs a clean close and reopen of durable storage, then verifies the
    /// ledger hash chain (clean-reopen persistence + integrity check).
    ///
    /// # Errors
    ///
    /// Returns a description if reopening or ledger verification fails.
    fn reopen(&mut self) -> Result<(), String>;

    /// Feeds a typed pre-commit [`DecisionRefusal`] (such as
    /// `ImmutableStateWrite` or `NotFromState`) into the host's domain-coverage
    /// recorder when attached.
    fn record_decision_refusal(&self, _refusal: &DecisionRefusal<Self::Error>) {}

    /// Reads the host's cell-level observed domain coverage when attached, so
    /// MBT reports consume one unified observer rather than a disconnected tally.
    fn read_observed_coverage(&self) -> Option<coverage::DomainCoverage> {
        None
    }

    /// Returns the canonical `sha256:...` content pin of the governing law
    /// artifact `artifact_id` attached to `object_type` in this host's own
    /// runtime model, or `None` if absent (fail-closed default: `None`).
    fn governing_law_pin(&self, object_type: &str, artifact_id: &str) -> Option<String> {
        let _ = (object_type, artifact_id);
        None
    }

    /// Seeds a host-authenticated, retained `FunctionReceipt` for `target_key`
    /// matching `manifest_digest` and carrying `summary`, returning its receipt
    /// digest (`sha256:...`) to supply to an `acceptReceipt` action.
    ///
    /// # Errors
    ///
    /// Returns a description if receipt seeding fails.
    fn seed_accepted_receipt(
        &mut self,
        _target_key: &str,
        _manifest_digest: &str,
        _summary: JsonObject,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// Builds the platform's synthetic `norn.FunctionCommand` object type required
/// to persist Function command reconciliation state (`RECEIPT-CONSUME-1`).
#[must_use]
pub fn function_command_object_type() -> ObjectTypeDef {
    ObjectTypeDef {
        immutable: false,
        name: ObjectTypeName("norn.FunctionCommand".to_owned()),
        role: None,
        description: None,
        status: ResourceStatus::default(),
        extends: None,
        implements: Vec::new(),
        properties: vec![Property {
            protected: false,
            id: PropertyId("state".to_owned()),
            description: None,
            property_type: PropertyType::JsonExt { schema: None },
            constraints: PropertyConstraints {
                nullable: true,
                ..PropertyConstraints::default()
            },
            default: None,
            markings: Vec::new(),
            computed: None,
        }],
        links: Vec::new(),
        owned_collections: Vec::new(),
        identity: Vec::new(),
        invariants: Vec::new(),
        lifecycles: Vec::new(),
        markings: Vec::new(),
        accounting_leg: None,
    }
}

/// Builds the platform's synthetic `norn.ActionCall` object type required by
/// `commit_action` idempotency records.
#[must_use]
pub fn action_call_object_type() -> ObjectTypeDef {
    let text = |id: &str, nullable: bool| Property {
        protected: false,
        id: PropertyId(id.to_owned()),
        description: None,
        property_type: PropertyType::Text { format: None },
        constraints: PropertyConstraints {
            nullable,
            ..PropertyConstraints::default()
        },
        default: None,
        markings: Vec::new(),
        computed: None,
    };
    let json_ext = |id: &str| Property {
        protected: false,
        id: PropertyId(id.to_owned()),
        description: None,
        property_type: PropertyType::JsonExt { schema: None },
        constraints: PropertyConstraints {
            nullable: true,
            ..PropertyConstraints::default()
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
        name: ObjectTypeName(norn_kernel::ACTION_CALL_TYPE.to_owned()),
        role: None,
        description: None,
        status: ResourceStatus::Active {},
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

/// Returns `object_type` followed by its `extends` ancestors in order,
/// breaking safely on any `extends` cycle.
pub(crate) fn object_type_ancestors<'a>(
    pack: &'a AdmittedPack,
    object_type: &'a ObjectTypeDef,
) -> Vec<&'a ObjectTypeDef> {
    let mut visited = BTreeSet::new();
    let mut chain = Vec::new();
    let mut current = Some(object_type);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        chain.push(def);
        current = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    chain
}

/// Returns every interface implemented by any type in `chain` together with
/// all transitive super-interfaces (`contract_owners`), deduplicated in
/// deterministic traversal order.
pub(crate) fn object_type_interfaces<'a>(
    pack: &'a AdmittedPack,
    chain: &[&'a ObjectTypeDef],
) -> Vec<&'a InterfaceDef> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for def in chain {
        for iface_name in &def.implements {
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
            {
                for owner in
                    iface.contract_owners(|name| pack.interfaces.iter().find(|i| i.name == *name))
                {
                    if seen.insert(owner.name.as_str()) {
                        out.push(owner);
                    }
                }
            }
        }
    }
    out
}

/// Dereferences a chain of `PropertyType::Ref` aliases through `shared`,
/// stopping safely on missing targets or alias cycles.
pub(crate) fn resolve_property_type_chain<'a>(
    mut ty: &'a PropertyType,
    shared: &'a BTreeMap<&'a str, &'a PropertyTypeDef>,
) -> &'a PropertyType {
    let mut visited = BTreeSet::new();
    while let PropertyType::Ref { name, .. } = ty {
        if !visited.insert(name.as_str()) {
            break;
        }
        let Some(def) = shared.get(name.as_str()) else {
            break;
        };
        ty = &def.property_type;
    }
    ty
}

pub(crate) fn effective_lifecycles<'a>(
    pack: &'a AdmittedPack,
    object_type: &'a ObjectTypeDef,
) -> Vec<&'a Lifecycle> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let chain = object_type_ancestors(pack, object_type);
    for def in &chain {
        for lc in &def.lifecycles {
            if seen.insert(lc.property.as_str()) {
                out.push(lc);
            }
        }
    }
    for owner in object_type_interfaces(pack, &chain) {
        for lc in &owner.lifecycles {
            if seen.insert(lc.property.as_str()) {
                out.push(lc);
            }
        }
    }
    out
}

pub(crate) fn effective_invariants(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
) -> Vec<ZenExpression> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let chain = object_type_ancestors(pack, object_type);
    for def in &chain {
        for inv in &def.invariants {
            if seen.insert(inv.id.as_str()) {
                out.push(inv.expression.clone());
            }
        }
    }
    for owner in object_type_interfaces(pack, &chain) {
        for inv in &owner.invariants {
            if seen.insert(inv.id.as_str()) {
                out.push(inv.expression.clone());
            }
        }
    }
    out
}

pub(crate) fn type_matches_target(
    pack: &AdmittedPack,
    candidate: &ObjectTypeDef,
    target_name: &str,
) -> bool {
    let chain = object_type_ancestors(pack, candidate);
    if chain.iter().any(|def| def.name.as_str() == target_name) {
        return true;
    }
    if chain.iter().any(|def| {
        def.implements
            .iter()
            .any(|name| name.as_str() == target_name)
    }) {
        return true;
    }
    object_type_interfaces(pack, &chain)
        .into_iter()
        .any(|owner| owner.name.as_str() == target_name)
}

pub(crate) fn resolve_concrete_link_target<'a>(
    pack: &'a AdmittedPack,
    target_name: &str,
) -> Option<&'a ObjectTypeDef> {
    pack.object_types
        .iter()
        .find(|ty| ty.name.as_str() == target_name)
        .or_else(|| {
            pack.object_types
                .iter()
                .find(|ty| type_matches_target(pack, ty, target_name))
        })
}

pub(crate) fn format_zero_decimal(scale: u64) -> String {
    if scale == 0 {
        return "0".to_owned();
    }
    let digits = usize::try_from(scale.min(28)).unwrap_or(2);
    format!("0.{}", "0".repeat(digits))
}

pub(crate) fn format_decimal(whole: i64, scale: u64, with_fraction: bool) -> String {
    if scale == 0 {
        return whole.to_string();
    }
    let digits = usize::try_from(scale.min(28)).unwrap_or(2);
    if !with_fraction {
        return format!("{whole}.{}", "0".repeat(digits));
    }
    let mut frac = String::with_capacity(digits);
    frac.push('5');
    for _ in 1..digits {
        frac.push('0');
    }
    format!("{whole}.{frac}")
}

fn commit_fact_decision<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    ty_name: &str,
    id: &str,
    change: FactChange,
    turn_id: String,
) -> Result<(), DecisionRefusal<H::Error>> {
    let slice = load_slice(host, [FactKey::new(ty_name, id)]).map_err(DecisionRefusal::Host)?;
    let output = DecisionOutput {
        facts: vec![change],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn = DecisionTurn {
        id: turn_id,
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    commit_decision(host, catalog, &slice, &turn, &output).map(|_| ())
}

pub(crate) fn fire_transition_decision<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    ty_name: &str,
    prop: &str,
    id: &str,
    transition_id: &str,
    turn_nonce: u64,
) -> Result<(), DecisionRefusal<H::Error>> {
    commit_fact_decision(
        host,
        catalog,
        ty_name,
        id,
        FactChange::Transition {
            object_type: ObjectTypeName(ty_name.to_owned()),
            key: id.to_owned(),
            lifecycle: PropertyId(prop.to_owned()),
            transition: TransitionId(transition_id.to_owned()),
        },
        format!("tr-{id}-{transition_id}-{turn_nonce}"),
    )
}

pub(crate) fn set_property_decision<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    ty_name: &str,
    id: &str,
    prop: &str,
    value: JsonValue,
    turn_nonce: u64,
) -> Result<(), DecisionRefusal<H::Error>> {
    commit_fact_decision(
        host,
        catalog,
        ty_name,
        id,
        FactChange::Set {
            object_type: ObjectTypeName(ty_name.to_owned()),
            key: id.to_owned(),
            property: PropertyId(prop.to_owned()),
            value,
        },
        format!("set-{id}-{prop}-{turn_nonce}"),
    )
}

fn take_self_ident(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix("self.")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then(|| (name.clone(), rest.get(name.len()..).unwrap_or_default()))
}

fn unwrap_self_group(group: &str) -> Option<String> {
    let (name, tail) = take_self_ident(group.trim())?;
    let rest = tail.trim_start();
    (rest.is_empty() || rest.starts_with("??")).then_some(name)
}

fn is_op_boundary(s: &str, lhs: bool) -> bool {
    let t = if lhs { s.trim_end() } else { s.trim_start() };
    let toks = if lhs {
        ["&&", "||", "(", ","]
    } else {
        ["&&", "||", ")", ","]
    };
    t.is_empty()
        || toks.iter().any(|k| {
            if lhs {
                t.ends_with(k)
            } else {
                t.starts_with(k)
            }
        })
}

fn find_matching_open(inner: &str) -> Option<usize> {
    let mut stack = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in inner.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => stack.push(idx),
            ')' => {
                stack.pop()?;
            }
            _ => {}
        }
    }
    quote.is_none().then(|| stack.pop()).flatten()
}

fn find_matching_close(inner: &str) -> Option<usize> {
    let mut depth: usize = 1;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in inner.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth = depth.saturating_add(1),
            ')' if depth == 1 => return Some(idx),
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn extract_lhs_self_prop(lhs_part: &str) -> Option<String> {
    let trimmed = lhs_part.trim_end();
    if let Some(inner) = trimmed.strip_suffix(')') {
        let open = find_matching_open(inner)?;
        return is_op_boundary(inner.get(..open).unwrap_or_default(), true)
            .then(|| unwrap_self_group(inner.get(open.saturating_add(1)..).unwrap_or_default()))
            .flatten();
    }
    let rev: String = trimmed
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let name: String = rev.chars().rev().collect();
    let before = trimmed.strip_suffix(name.as_str())?.strip_suffix("self.")?;
    (!name.is_empty() && is_op_boundary(before, true)).then_some(name)
}

fn extract_rhs_self_prop(rhs_part: &str) -> Option<String> {
    let trimmed = rhs_part.trim_start();
    if let Some(inner) = trimmed.strip_prefix('(') {
        let close = find_matching_close(inner)?;
        let after = inner.get(close.saturating_add(1)..).unwrap_or_default();
        return is_op_boundary(after, false)
            .then(|| unwrap_self_group(inner.get(..close).unwrap_or_default()))
            .flatten();
    }
    let (name, tail) = take_self_ident(trimmed)?;
    is_op_boundary(tail, false).then_some(name)
}

pub(crate) fn extract_self_equality_pairs(expr: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut offset = 0;
    while let Some(rel) = expr.get(offset..).and_then(|tail| tail.find("==")) {
        let eq_pos = offset.saturating_add(rel);
        let lhs = expr.get(..eq_pos).unwrap_or_default();
        let rhs = expr.get(eq_pos.saturating_add(2)..).unwrap_or_default();
        if !lhs.ends_with(['!', '='])
            && !rhs.starts_with('=')
            && let (Some(l), Some(r)) = (extract_lhs_self_prop(lhs), extract_rhs_self_prop(rhs))
        {
            pairs.push((l, r));
        }
        offset = eq_pos.saturating_add(2);
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cycle_safe_walkers_and_multihop_refs_terminate() {
        let one: ObjectTypeDef = serde_json::from_value(json!({
            "name": "t.One",
            "extends": "t.Two",
            "properties": [
                { "id": "creator", "protected": true, "type": { "kind": "text" } },
                { "id": "node", "type": { "kind": "ref", "name": "t.RecNode" } }
            ]
        }))
        .expect("t.One decodes");
        let two: ObjectTypeDef = serde_json::from_value(json!({
            "name": "t.Two",
            "extends": "t.One",
            "properties": []
        }))
        .expect("t.Two decodes");
        let prop_types: Vec<PropertyTypeDef> = serde_json::from_value(json!([
            { "name": "t.Alias1", "type": { "kind": "ref", "name": "t.Alias2" } },
            { "name": "t.Alias2", "type": { "kind": "enum", "values": [{ "value": "open" }] } },
            {
                "name": "t.RecNode",
                "type": {
                    "kind": "record",
                    "members": [{ "id": "next", "type": { "kind": "ref", "name": "t.RecNode" } }]
                }
            }
        ]))
        .expect("property types decode");
        let pack = AdmittedPack {
            name: "cycle".to_owned(),
            object_types: vec![one, two],
            property_types: prop_types,
            ..AdmittedPack::default()
        };
        let first = pack.object_types.first().expect("first type");
        assert_eq!(object_type_ancestors(&pack, first).len(), 2);
        assert_eq!(seed::protected_properties(&pack, first), vec!["creator"]);
        let shared: BTreeMap<&str, &PropertyTypeDef> = pack
            .property_types
            .iter()
            .map(|def| (def.name.as_str(), def))
            .collect();
        let alias1: PropertyType =
            serde_json::from_value(json!({ "kind": "ref", "name": "t.Alias1" }))
                .expect("ref decodes");
        let resolved = resolve_property_type_chain(&alias1, &shared);
        assert!(matches!(resolved, PropertyType::Enum { .. }));
        let synthesized = synth::synthesize_object(&pack, first, "k1", 1);
        assert!(synthesized.contains_key("node"));
    }

    #[test]
    fn immutable_entity_coverage_plan_skips_unreachable_lifecycle_targets() {
        let locked: ObjectTypeDef = serde_json::from_value(json!({
            "name": "t.Locked",
            "immutable": true,
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] } }
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["open"],
                "terminal": ["closed"],
                "immutable": ["open"],
                "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
            }]
        }))
        .expect("t.Locked decodes");
        let pack = AdmittedPack {
            name: "imm".to_owned(),
            object_types: vec![locked],
            ..AdmittedPack::default()
        };
        let plan = coverage::DomainCoveragePlan::from_pack(&pack);
        assert!(plan.transitions.is_empty());
        assert_eq!(plan.immutable_targets.len(), 1);
    }

    #[test]
    fn synthesize_object_satisfies_interface_invariants_and_skips_inherited_lifecycle() {
        let iface: InterfaceDef = serde_json::from_value(json!({
            "name": "t.MeasuredCommitment",
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "requested" }, { "value": "fulfilled" }] } },
                { "id": "unit", "constraints": { "nullable": true }, "type": { "kind": "text" } },
                { "id": "currency", "constraints": { "nullable": true }, "type": { "kind": "text" } }
            ],
            "invariants": [{
                "id": "single_measure",
                "expression": "((self.unit != null) and (self.currency == null)) or ((self.unit == null) and (self.currency != null))",
                "message": "either unit or currency"
            }],
            "lifecycles": [{
                "property": "status",
                "initial": ["requested"],
                "terminal": ["fulfilled"],
                "immutable": ["fulfilled"],
                "transitions": [{ "id": "fulfil", "from": ["requested"], "to": "fulfilled" }]
            }]
        }))
        .expect("interface decodes");
        let obj: ObjectTypeDef = serde_json::from_value(json!({
            "name": "t.Line",
            "implements": ["t.MeasuredCommitment"],
            "properties": [
                { "id": "status", "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "requested" }, { "value": "fulfilled" }] } },
                { "id": "note", "type": { "kind": "text" } },
                { "id": "unit", "constraints": { "nullable": true }, "type": { "kind": "text" } },
                { "id": "currency", "constraints": { "nullable": true }, "type": { "kind": "text" } }
            ]
        }))
        .expect("object decodes");
        let pack = AdmittedPack {
            name: "inv".to_owned(),
            object_types: vec![obj],
            interfaces: vec![iface],
            ..AdmittedPack::default()
        };
        let line = pack.object_types.first().expect("line type");
        let values = synth::synthesize_object(&pack, line, "l1", 1);
        assert_eq!(
            values.get("status").and_then(JsonValue::as_str),
            Some("requested")
        );
        let unit_null = matches!(values.get("unit"), Some(JsonValue::Null));
        let currency_null = matches!(values.get("currency"), Some(JsonValue::Null));
        assert_ne!(unit_null, currency_null);
        let mutation = synth::pick_non_lifecycle_mutation(&pack, line, &values, 2);
        assert_eq!(mutation.as_ref().map(|(id, _)| id.as_str()), Some("note"));
    }

    #[test]
    fn extract_self_equality_pairs_handles_coalesce_and_rejects_compound_exprs() {
        assert_eq!(
            extract_self_equality_pairs(
                "self.evaluation_revision !== null && self.pinned_dataset_revision == (self.evaluation_revision ?? '') && self.evaluated_artifact_sha256 !== null && self.artifact_sha256 == (self.evaluated_artifact_sha256 ?? '')"
            ),
            vec![
                (
                    "pinned_dataset_revision".to_owned(),
                    "evaluation_revision".to_owned()
                ),
                (
                    "artifact_sha256".to_owned(),
                    "evaluated_artifact_sha256".to_owned()
                ),
            ]
        );
        assert_eq!(
            extract_self_equality_pairs("(self.a ?? '') == self.b"),
            vec![("a".to_owned(), "b".to_owned())]
        );
        assert_eq!(
            extract_self_equality_pairs("self.a == (self.b ?? ')')"),
            vec![("a".to_owned(), "b".to_owned())]
        );
        assert_eq!(
            extract_self_equality_pairs("(self.a ?? '(') == self.b"),
            vec![("a".to_owned(), "b".to_owned())]
        );
        assert_eq!(
            extract_self_equality_pairs(
                r#"self.x !== ')' && (self.a ?? '\')') == (self.b ?? "(\"")"#
            ),
            vec![("a".to_owned(), "b".to_owned())]
        );
        for expr in [
            "self.a == (self.b ?? ')') + 1",
            "self.a == (self.b + 1)",
            "self.a == (self.b ?? 0) + 1",
            "self.a == self.b + 1",
            "1 + self.a == self.b",
            "(self.a + 1) == self.b",
            "self.a !== (self.b ?? '')",
            "self.a == (self.b ?? f(1)) + 1",
        ] {
            assert_eq!(extract_self_equality_pairs(expr), Vec::new(), "{expr}");
        }
    }
}
