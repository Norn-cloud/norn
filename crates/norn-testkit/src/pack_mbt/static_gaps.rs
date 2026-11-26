//! Static Structural Gap Detector over admitted pack IR (#1656).
//!
//! Operates strictly on IR (no IO, no runtime cell) and labels every finding
//! honestly as [`EvidenceLevel::Structural`] or [`EvidenceLevel::ExhaustiveFinite`].
//! Never invents business policy: high-impact actions lacking separation-of-duties
//! policies are emitted as explicit [`GapSeverity::Question`] items for pack
//! authors and operators.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use norn_kernel::TypeCatalog;
use norn_kernel::pack_mbt_support::{
    AccessCondition, AccessIr, ActionEffectProgram, ActionRoot, ActionUiEffect, IrJsonValue,
    LawResponse, Lifecycle, ObjectTypeDef, PropertyType, ResourceCompareOp, ResourceOperand,
    WriteChange, expression_read_paths,
};

use super::{AdmittedPack, EvidenceLevel, action_call_object_type};

/// Severity of a detected structural gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapSeverity {
    /// A structural defect in the declared state machine or schema.
    Defect,
    /// An explicit business-policy question for the pack author (never guessed).
    Question,
}

/// One typed finding emitted by [`analyze_static_gaps`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralGap {
    /// A non-terminal lifecycle state with zero outgoing transitions.
    DeadEndNonTerminalState {
        /// Owning object type.
        object_type: String,
        /// Lifecycle property name.
        lifecycle: String,
        /// Dead-end state name.
        state: String,
    },
    /// A lifecycle enum state unreachable from any `initial` state.
    UnreachableState {
        /// Owning object type.
        object_type: String,
        /// Lifecycle property name.
        lifecycle: String,
        /// Unreachable state name.
        state: String,
    },
    /// A stored non-computed field read in a guard, invariant, or law that no
    /// governed action or admitted mapping in the pack ever writes.
    OrphanField {
        /// Owning object type.
        object_type: String,
        /// Property id.
        property: String,
        /// Declarations that reference this field.
        referenced_in: Vec<String>,
    },
    /// An immutable financial/ledger object type with no resolved compensating
    /// action declared in the pack.
    ImmutableFinancialMissingReversal {
        /// Owning object type.
        object_type: String,
        /// Why the detector classified this type as immutable financial/ledger.
        reason: String,
    },
    /// An immutable financial/ledger object type with a reversal-like action
    /// name but no explicit `compensation` declaration resolving to it.
    ImmutableFinancialUnverifiedReversal {
        /// Owning object type.
        object_type: String,
        /// Candidate action id matched only by name heuristic.
        candidate_action_id: String,
        /// Why manual confirmation or explicit `compensation` metadata is needed.
        reason: String,
    },
    /// A high-impact action whose `AccessIR` policy lacks an explicit
    /// separation-of-duties (`requester != caller`) resource predicate across
    /// all permit paths.
    HighImpactActionMissingSod {
        /// Action id.
        action_id: String,
        /// Target object type, if known.
        object_type: Option<String>,
        /// Why the action was flagged as high-impact.
        reason: String,
    },
    /// A declared lifecycle transition with no governed `fire_transition` action.
    TransitionWithoutAction {
        /// Owning object type.
        object_type: String,
        /// Lifecycle property name.
        lifecycle: String,
        /// Declared transition id.
        transition_id: String,
    },
}

impl StructuralGap {
    /// Returns whether this finding is a defect or an explicit policy question.
    #[must_use]
    pub const fn severity(&self) -> GapSeverity {
        match self {
            Self::DeadEndNonTerminalState { .. }
            | Self::UnreachableState { .. }
            | Self::OrphanField { .. }
            | Self::ImmutableFinancialMissingReversal { .. }
            | Self::TransitionWithoutAction { .. } => GapSeverity::Defect,
            Self::HighImpactActionMissingSod { .. }
            | Self::ImmutableFinancialUnverifiedReversal { .. } => GapSeverity::Question,
        }
    }

    /// Honest evidence classification for this check.
    #[must_use]
    pub const fn evidence(&self) -> EvidenceLevel {
        match self {
            Self::DeadEndNonTerminalState { .. } | Self::UnreachableState { .. } => {
                EvidenceLevel::ExhaustiveFinite
            }
            Self::OrphanField { .. }
            | Self::ImmutableFinancialMissingReversal { .. }
            | Self::ImmutableFinancialUnverifiedReversal { .. }
            | Self::HighImpactActionMissingSod { .. }
            | Self::TransitionWithoutAction { .. } => EvidenceLevel::Structural,
        }
    }
}

/// Complete output of the Static Structural Gap Detector for one pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticGapReport {
    /// Pack identifier.
    pub pack_name: String,
    /// All findings in deterministic order.
    pub gaps: Vec<StructuralGap>,
}

impl StaticGapReport {
    /// Returns all structural defects (excluding open policy questions).
    #[must_use]
    pub fn defects(&self) -> Vec<&StructuralGap> {
        self.gaps
            .iter()
            .filter(|gap| gap.severity() == GapSeverity::Defect)
            .collect()
    }

    /// Returns all explicit policy questions (such as missing `SoD` policies).
    #[must_use]
    pub fn questions(&self) -> Vec<&StructuralGap> {
        self.gaps
            .iter()
            .filter(|gap| gap.severity() == GapSeverity::Question)
            .collect()
    }
}

impl fmt::Display for StaticGapReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "StaticGapReport({}): {} total ({} defects, {} questions)",
            self.pack_name,
            self.gaps.len(),
            self.defects().len(),
            self.questions().len()
        )?;
        for gap in &self.gaps {
            writeln!(
                f,
                "  [{:?} | {}] {:?}",
                gap.severity(),
                gap.evidence().as_str(),
                gap
            )?;
        }
        Ok(())
    }
}

/// Runs all static structural gap detectors on `pack`'s resolved effective catalog.
#[must_use]
pub fn analyze_static_gaps(pack: &AdmittedPack) -> StaticGapReport {
    let mut gaps = Vec::new();
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);
    let written_fields = collect_action_written_fields(pack);
    let transition_actions = collect_transition_actions(pack);

    for object_type in &pack.object_types {
        detect_lifecycle_gaps(pack, &catalog, object_type, &transition_actions, &mut gaps);
        detect_orphan_fields(pack, &catalog, object_type, &written_fields, &mut gaps);
        detect_immutable_financial_reversal_gap(pack, &catalog, object_type, &mut gaps);
    }
    detect_high_impact_sod_questions(pack, &mut gaps);

    StaticGapReport {
        pack_name: pack.name.clone(),
        gaps,
    }
}

fn enum_states_for(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    property: &str,
) -> Vec<String> {
    let resolve_enum = |ty: &PropertyType| -> Option<Vec<String>> {
        let resolved = match ty {
            PropertyType::Ref { name, .. } => pack
                .property_types
                .iter()
                .find(|def| def.name.as_str() == name.as_str())
                .map_or(ty, |def| &def.property_type),
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Decimal { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Boolean {}
            | PropertyType::Enum { .. }
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Timestamp { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. } => ty,
        };
        match resolved {
            PropertyType::Enum { values } => {
                Some(values.iter().map(|item| item.value.clone()).collect())
            }
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Decimal { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Boolean {}
            | PropertyType::Date {}
            | PropertyType::Duration {}
            | PropertyType::Timestamp { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::Ref { .. } => None,
        }
    };
    let mut visited = BTreeSet::new();
    let mut current = Some(object_type);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        if let Some(prop) = def.properties.iter().find(|p| p.id.as_str() == property)
            && let Some(states) = resolve_enum(&prop.property_type)
        {
            return states;
        }
        for iface_name in &def.implements {
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
                && let Some(prop) = iface.properties.iter().find(|p| p.id.as_str() == property)
                && let Some(states) = resolve_enum(&prop.property_type)
            {
                return states;
            }
        }
        current = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    Vec::new()
}

fn reachable_states(lifecycle: &Lifecycle) -> BTreeSet<String> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    for init in &lifecycle.initial {
        if visited.insert(init.clone()) {
            queue.push_back(init.clone());
        }
    }
    while let Some(state) = queue.pop_front() {
        for transition in &lifecycle.transitions {
            if transition.from.iter().any(|src| src == &state)
                && visited.insert(transition.to.clone())
            {
                queue.push_back(transition.to.clone());
            }
        }
    }
    visited
}

fn detect_lifecycle_gaps(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
    transition_actions: &BTreeSet<(String, String, String)>,
    out: &mut Vec<StructuralGap>,
) {
    let type_name = object_type.name.as_str();
    for lifecycle in catalog.lifecycles(&object_type.name).into_values() {
        let prop = lifecycle.property.as_str();
        let all_states = enum_states_for(pack, object_type, prop);
        let reachable = reachable_states(&lifecycle);
        let terminal: BTreeSet<&str> = lifecycle.terminal.iter().map(String::as_str).collect();

        for state in &all_states {
            if !reachable.contains(state) {
                out.push(StructuralGap::UnreachableState {
                    object_type: type_name.to_owned(),
                    lifecycle: prop.to_owned(),
                    state: state.clone(),
                });
            }
            let has_outgoing = lifecycle
                .transitions
                .iter()
                .any(|tr| tr.from.iter().any(|src| src == state));
            if !terminal.contains(state.as_str()) && !has_outgoing {
                out.push(StructuralGap::DeadEndNonTerminalState {
                    object_type: type_name.to_owned(),
                    lifecycle: prop.to_owned(),
                    state: state.clone(),
                });
            }
        }

        for transition in &lifecycle.transitions {
            let key = (
                type_name.to_owned(),
                prop.to_owned(),
                transition.id.as_str().to_owned(),
            );
            if transition.trigger.is_none() && !transition_actions.contains(&key) {
                out.push(StructuralGap::TransitionWithoutAction {
                    object_type: type_name.to_owned(),
                    lifecycle: prop.to_owned(),
                    transition_id: transition.id.as_str().to_owned(),
                });
            }
        }
    }
}

fn collect_transition_actions(pack: &AdmittedPack) -> BTreeSet<(String, String, String)> {
    let mut covered = BTreeSet::new();
    for action in &pack.actions {
        if let Some(ActionEffectProgram::FireTransition {
            object_kind,
            lifecycle,
            transition,
            ..
        }) = &action.root.effect_program
        {
            covered.insert((
                object_kind.clone(),
                lifecycle.as_str().to_owned(),
                transition.as_str().to_owned(),
            ));
        }
    }
    covered
}

fn collect_action_written_fields(pack: &AdmittedPack) -> BTreeMap<String, BTreeSet<String>> {
    let mut written: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for action in &pack.actions {
        let Some(effect) = &action.root.effect_program else {
            continue;
        };
        let target_kind = effect.target().object_kind.to_owned();
        let entry = written.entry(target_kind).or_default();
        match effect {
            ActionEffectProgram::UpsertInputObject { .. } => {
                if let Some(props) = action
                    .root
                    .input_schema
                    .get("properties")
                    .and_then(IrJsonValue::as_object)
                {
                    for field in props.keys() {
                        entry.insert(field.clone());
                    }
                }
            }
            ActionEffectProgram::FireTransition {
                lifecycle,
                accept_receipt,
                ..
            } => {
                entry.insert(lifecycle.as_str().to_owned());
                if let Some(receipt) = accept_receipt {
                    entry.insert(receipt.digest_property.as_str().to_owned());
                    for target_prop in receipt.result_bindings.keys() {
                        entry.insert(target_prop.as_str().to_owned());
                    }
                }
            }
            ActionEffectProgram::MutateParentWithRows {
                parent_values_field,
                ..
            } => {
                if let Some(parent_props) = action
                    .root
                    .input_schema
                    .get("properties")
                    .and_then(IrJsonValue::as_object)
                    .and_then(|props| props.get(parent_values_field))
                    .and_then(IrJsonValue::as_object)
                    .and_then(|schema| schema.get("properties"))
                    .and_then(IrJsonValue::as_object)
                {
                    for field in parent_props.keys() {
                        entry.insert(field.clone());
                    }
                }
            }
        }
    }
    // An admitted mapping writes its target's mapped fields from source evidence.
    for (object_type, fields) in &pack.mapping_writes {
        written
            .entry(object_type.clone())
            .or_default()
            .extend(fields.iter().cloned());
    }
    written
}

fn collect_referenced_fields(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
) -> BTreeMap<String, Vec<String>> {
    let mut refs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut record = |expr: &str, source: String| {
        if let Ok(paths) = expression_read_paths(expr, "self") {
            for path in paths {
                if let Some(first) = path.first() {
                    refs.entry(first.clone()).or_default().push(source.clone());
                }
            }
        }
    };

    for lifecycle in catalog.lifecycles(&object_type.name).into_values() {
        for transition in &lifecycle.transitions {
            for guard in &transition.guards {
                record(
                    guard.as_str(),
                    format!(
                        "guard:{}.{}",
                        lifecycle.property.as_str(),
                        transition.id.as_str()
                    ),
                );
            }
        }
    }
    let mut seen_invariants = BTreeSet::new();
    let mut walk_ty = Some(object_type);
    while let Some(def) = walk_ty {
        for invariant in &def.invariants {
            if seen_invariants.insert(invariant.id.as_str().to_owned()) {
                record(
                    invariant.expression.as_str(),
                    format!("invariant:{}", invariant.id.as_str()),
                );
            }
        }
        for iface_name in &def.implements {
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
            {
                for owner in
                    iface.contract_owners(|name| pack.interfaces.iter().find(|i| i.name == *name))
                {
                    for invariant in &owner.invariants {
                        if seen_invariants.insert(invariant.id.as_str().to_owned()) {
                            record(
                                invariant.expression.as_str(),
                                format!("invariant:{}", invariant.id.as_str()),
                            );
                        }
                    }
                }
            }
        }
        walk_ty = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    let governed_subjects = collect_governed_subjects(pack, object_type);
    let governed_law_ids: Option<BTreeSet<String>> = catalog
        .governing_laws(&object_type.name)
        .ok()
        .map(|envelopes| {
            envelopes
                .into_iter()
                .map(|env| env.artifact_id.clone())
                .collect()
        });
    for (idx, law) in pack.laws.iter().enumerate() {
        let applies = governed_law_ids.as_ref().map_or_else(
            || governed_subjects.contains(law.subject.as_str()),
            |ids| {
                pack.law_envelopes
                    .get(idx)
                    .is_some_and(|env| ids.contains(&env.artifact_id))
                    || governed_subjects.contains(law.subject.as_str())
            },
        );
        if !applies {
            continue;
        }
        if let Some(cond) = &law.condition {
            record(cond.as_str(), format!("law:{}:condition", law.id));
        }
        if let LawResponse::Predicate { expression } = &law.response {
            record(expression.as_str(), format!("law:{}:response", law.id));
        }
    }
    refs
}

fn collect_governed_subjects(pack: &AdmittedPack, object_type: &ObjectTypeDef) -> BTreeSet<String> {
    let mut governed_subjects = BTreeSet::new();
    let mut walk_subj = Some(object_type);
    while let Some(def) = walk_subj {
        if !governed_subjects.insert(def.name.as_str().to_owned()) {
            break;
        }
        for iface_name in &def.implements {
            governed_subjects.insert(iface_name.as_str().to_owned());
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
            {
                for owner in
                    iface.contract_owners(|name| pack.interfaces.iter().find(|i| i.name == *name))
                {
                    governed_subjects.insert(owner.name.as_str().to_owned());
                }
            }
        }
        walk_subj = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    governed_subjects
}

fn detect_orphan_fields(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
    written_fields: &BTreeMap<String, BTreeSet<String>>,
    out: &mut Vec<StructuralGap>,
) {
    let type_name = object_type.name.as_str();
    let empty = BTreeSet::new();
    let written = written_fields.get(type_name).unwrap_or(&empty);
    let refs = collect_referenced_fields(pack, catalog, object_type);
    let lifecycle_props: BTreeSet<String> = catalog
        .lifecycles(&object_type.name)
        .into_keys()
        .map(|prop| prop.as_str().to_owned())
        .collect();

    for prop in effective_properties(pack, object_type) {
        let name = prop.id;
        if prop.read_only_or_default || lifecycle_props.contains(name) {
            continue;
        }
        if let Some(sources) = refs.get(name)
            && !written.contains(name)
        {
            let mut deduped = sources.clone();
            deduped.sort();
            deduped.dedup();
            out.push(StructuralGap::OrphanField {
                object_type: type_name.to_owned(),
                property: name.to_owned(),
                referenced_in: deduped,
            });
        }
    }
}

struct EffectiveProp<'a> {
    id: &'a str,
    property_type: &'a PropertyType,
    read_only_or_default: bool,
}

fn effective_properties<'a>(
    pack: &'a AdmittedPack,
    object_type: &'a ObjectTypeDef,
) -> Vec<EffectiveProp<'a>> {
    let mut seen_props = BTreeSet::new();
    let mut effective_props = Vec::new();
    let mut concrete_chain = Vec::new();
    let mut visited_ty = BTreeSet::new();
    let mut walk_ty = Some(object_type);
    while let Some(def) = walk_ty {
        if !visited_ty.insert(def.name.as_str().to_owned()) {
            break;
        }
        concrete_chain.push(def);
        walk_ty = def.extends.as_ref().and_then(|parent| {
            pack.object_types
                .iter()
                .find(|ty| ty.name.as_str() == parent.as_str())
        });
    }
    // Pass 1: concrete `extends` chain preserves parent default / read-only metadata.
    for def in &concrete_chain {
        for prop in &def.properties {
            if seen_props.insert(prop.id.as_str().to_owned()) {
                effective_props.push(EffectiveProp {
                    id: prop.id.as_str(),
                    property_type: &prop.property_type,
                    read_only_or_default: prop.is_read_only() || prop.default.is_some(),
                });
            }
        }
    }
    // Pass 2: interface contracts add interface-only properties without shadowing concrete ones.
    for def in &concrete_chain {
        for iface_name in &def.implements {
            if let Some(iface) = pack
                .interfaces
                .iter()
                .find(|i| i.name.as_str() == iface_name.as_str())
            {
                for owner in
                    iface.contract_owners(|name| pack.interfaces.iter().find(|i| i.name == *name))
                {
                    for prop in &owner.properties {
                        if seen_props.insert(prop.id.as_str().to_owned()) {
                            effective_props.push(EffectiveProp {
                                id: prop.id.as_str(),
                                property_type: &prop.property_type,
                                read_only_or_default: false,
                            });
                        }
                    }
                }
            }
        }
    }
    effective_props
}

fn is_financial_or_ledger_type(pack: &AdmittedPack, object_type: &ObjectTypeDef) -> Option<String> {
    if object_type.accounting_leg.is_some() {
        return Some("declares accountingLeg".to_owned());
    }
    for iface in &object_type.implements {
        let name = iface.as_str();
        if matches!(
            name,
            "core.LedgerEffect" | "core.Claim" | "core.EconomicEvent" | "canon.Settlement"
        ) {
            return Some(format!("implements {name}"));
        }
    }
    let lower_name = object_type.name.local_name().to_ascii_lowercase();
    for keyword in [
        "posting", "journal", "ledger", "invoice", "payment", "payroll", "payout",
    ] {
        if lower_name.contains(keyword) {
            return Some(format!("financial/ledger entity name contains '{keyword}'"));
        }
    }
    for prop in effective_properties(pack, object_type) {
        if let PropertyType::Ref { name, .. } = prop.property_type
            && name.as_str().contains("money")
        {
            return Some(format!("carries money property '{}'", prop.id));
        }
    }
    None
}

enum ReversalCoverage {
    VerifiedCompensation,
    NameHeuristicOnly(String),
    Missing,
}

fn action_touches_type(action: &ActionRoot, object_type: &str) -> bool {
    action
        .writes
        .iter()
        .any(|write| write.entity_kind == object_type)
        || action
            .effect_program
            .as_ref()
            .is_some_and(|eff| eff.target().object_kind == object_type)
}

fn classify_reversal_coverage(pack: &AdmittedPack, object_type: &str) -> ReversalCoverage {
    let mut heuristic_candidate = None;
    for action in &pack.actions {
        if !action_touches_type(&action.root, object_type) {
            continue;
        }
        if let Some(comp) = &action.root.compensation
            && let Some(target_action) = pack
                .actions
                .iter()
                .find(|candidate| candidate.root.action_id == comp.action_id)
            && (action_touches_type(&target_action.root, object_type)
                || !target_action.root.writes.is_empty()
                || target_action.root.effect_program.is_some())
        {
            return ReversalCoverage::VerifiedCompensation;
        }
        let lower_id = action.root.action_id.to_ascii_lowercase();
        if heuristic_candidate.is_none()
            && [
                "reverse",
                "reversal",
                "void",
                "compensat",
                "refund",
                "credit_note",
                "storno",
            ]
            .iter()
            .any(|kw| lower_id.contains(kw))
        {
            heuristic_candidate = Some(action.root.action_id.clone());
        }
    }
    match heuristic_candidate {
        Some(action_id) => ReversalCoverage::NameHeuristicOnly(action_id),
        None => ReversalCoverage::Missing,
    }
}

fn detect_immutable_financial_reversal_gap(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
    out: &mut Vec<StructuralGap>,
) {
    let has_immutable_lifecycle_state = catalog
        .lifecycles(&object_type.name)
        .values()
        .any(|lc| !lc.immutable.is_empty());
    if !super::coverage::is_effectively_immutable(pack, object_type)
        && !has_immutable_lifecycle_state
    {
        return;
    }
    let Some(financial_reason) = is_financial_or_ledger_type(pack, object_type) else {
        return;
    };
    match classify_reversal_coverage(pack, object_type.name.as_str()) {
        ReversalCoverage::VerifiedCompensation => {}
        ReversalCoverage::NameHeuristicOnly(candidate_action_id) => {
            out.push(StructuralGap::ImmutableFinancialUnverifiedReversal {
                object_type: object_type.name.as_str().to_owned(),
                candidate_action_id,
                reason: format!(
                    "immutable financial/ledger type ({financial_reason}) has a reversal-named action but no explicit resolved `compensation` declaration; confirm reversal semantics"
                ),
            });
        }
        ReversalCoverage::Missing => {
            out.push(StructuralGap::ImmutableFinancialMissingReversal {
                object_type: object_type.name.as_str().to_owned(),
                reason: format!(
                    "immutable financial/ledger type ({financial_reason}) has no declared compensating action resolving in the pack"
                ),
            });
        }
    }
}

fn high_impact_reason(action: &ActionRoot) -> Option<String> {
    let ui_effects = action.effective_ui_effects();
    if ui_effects.contains(&ActionUiEffect::Irreversible) {
        return Some("declared irreversible uiEffect".to_owned());
    }
    if ui_effects.contains(&ActionUiEffect::Destructive)
        || action
            .writes
            .iter()
            .any(|write| write.change == WriteChange::Deleted)
    {
        return Some("destructive or deleting write".to_owned());
    }
    if ui_effects
        .iter()
        .any(|eff| matches!(eff, ActionUiEffect::Money { .. }))
    {
        return Some("declared monetary effect".to_owned());
    }
    // Draft creation and host-authenticated receipt confirmations are not human
    // approval decisions, so skip the name-based heuristic for them while still
    // honoring explicit irreversible/destructive/monetary declarations above (DX-9).
    if action.quantity_effect.is_none()
        && matches!(
            action.effect_program,
            Some(ActionEffectProgram::UpsertInputObject {
                write_mode: norn_kernel::pack_mbt_support::UpsertWriteMode::CreateOnly,
                ..
            })
        )
    {
        return None;
    }
    if matches!(
        action.effect_program,
        Some(ActionEffectProgram::FireTransition {
            accept_receipt: Some(_),
            ..
        })
    ) {
        return None;
    }
    // Match on tokens of the action verb segment (final `.` segment), never on
    // entity nouns such as `payroll`, `payslip`, or `payout` in earlier segments (DX-9).
    let verb = action
        .action_id
        .rsplit('.')
        .next()
        .unwrap_or(action.action_id.as_str())
        .to_ascii_lowercase();
    for token in verb.split(['_', '-']) {
        if matches!(
            token,
            "approve"
                | "promote"
                | "retire"
                | "post"
                | "pay"
                | "payout"
                | "dispatch"
                | "settle"
                | "activate"
                | "release"
                | "void"
                | "reverse"
        ) {
            return Some(format!("high-impact action verb '{token}'"));
        }
    }
    None
}

fn is_actor_resource_field(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "requester",
        "requestor",
        "creator",
        "created_by",
        "author",
        "authored_by",
        "submitter",
        "submitted_by",
        "initiator",
        "initiated_by",
        "owner",
        "actor",
        "preparer",
        "prepared_by",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn is_principal_identity_attr(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "id" | "subject_id"
            | "subjectid"
            | "actor_id"
            | "actorid"
            | "user_id"
            | "userid"
            | "principal_id"
    )
}

fn rule_enforces_sod(rule: &norn_kernel::pack_mbt_support::AccessRule) -> bool {
    rule.all_of.iter().any(|cond| {
        matches!(
            cond,
            AccessCondition::Resource(res)
                if res.op == ResourceCompareOp::Ne
                    && is_actor_resource_field(&res.name)
                    && matches!(
                        &res.value,
                        ResourceOperand::PrincipalAttribute(pa)
                            if is_principal_identity_attr(&pa.principal_attribute)
                    )
        )
    })
}

fn action_has_sod_policy(policies: &[AccessIr], action_id: &str) -> bool {
    let matching_rules: Vec<_> = policies
        .iter()
        .flat_map(|policy| policy.rules.iter())
        .filter(|rule| rule.actions.iter().any(|act| act == action_id))
        .collect();
    !matching_rules.is_empty() && matching_rules.iter().all(|rule| rule_enforces_sod(rule))
}

fn detect_high_impact_sod_questions(pack: &AdmittedPack, out: &mut Vec<StructuralGap>) {
    for action in &pack.actions {
        let Some(reason) = high_impact_reason(&action.root) else {
            continue;
        };
        // ACTOR-INDEPENDENT-1: `independentOf` separates the caller from a
        // recorded actor in the kernel, as an AccessIR SoD rule does in policy.
        if action.root.independent_of.is_empty()
            && !action_has_sod_policy(&pack.access_policies, &action.root.action_id)
        {
            let object_type = action
                .root
                .effect_program
                .as_ref()
                .map(|eff| eff.target().object_kind.to_owned());
            out.push(StructuralGap::HighImpactActionMissingSod {
                action_id: action.root.action_id.clone(),
                object_type,
                reason: format!(
                    "high-impact action ({reason}) has no separation-of-duties (requester != approver) AccessIR rule; confirm whether SoD is required"
                ),
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "unit test fixture decoding")]
mod tests {
    use norn_kernel::pack_mbt_support::{ActionEffectProgram, ActionRoot, UpsertWriteMode};

    use super::high_impact_reason;

    fn sample_action(action_id: &str, effect: Option<ActionEffectProgram>) -> ActionRoot {
        let mut root: ActionRoot = serde_json::from_value(serde_json::json!({
            "actionId": action_id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }))
        .expect("minimal action root");
        root.effect_program = effect;
        root
    }

    #[test]
    fn dx9_skips_create_only_and_noun_matches_and_receipt_confirmations() {
        let create_payroll = sample_action(
            "soliman.payroll.create",
            Some(ActionEffectProgram::UpsertInputObject {
                object_kind: "soliman.PayrollRun".to_owned(),
                object_id_field: "id".to_owned(),
                entity_artifact_id: "soliman.PayrollRun.entity".to_owned(),
                entity_artifact_version: "0.2.0".to_owned(),
                write_mode: UpsertWriteMode::CreateOnly,
            }),
        );
        assert!(high_impact_reason(&create_payroll).is_none());

        let noun_only = sample_action("soliman.payout.rename", None);
        assert!(high_impact_reason(&noun_only).is_none());

        let receipt_confirm: ActionEffectProgram = serde_json::from_value(serde_json::json!({
            "kind": "fire_transition",
            "objectKind": "soliman.PayoutInstruction",
            "objectIdField": "id",
            "entityArtifactId": "soliman.PayoutInstruction.entity",
            "entityArtifactVersion": "0.2.0",
            "lifecycle": "state",
            "transition": "confirm_succeeded",
            "acceptReceipt": {
                "referenceField": "receipt",
                "manifestDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "digestProperty": "last_evidence_digest",
                "resultBindings": {}
            }
        }))
        .expect("receipt transition effect");
        let confirm = sample_action("soliman.payout.confirm_succeeded", Some(receipt_confirm));
        assert!(high_impact_reason(&confirm).is_none());

        let approve = sample_action("soliman.payroll.approve", None);
        assert!(high_impact_reason(&approve).is_some());

        let dispatch = sample_action("soliman.payout.dispatch_now", None);
        assert!(high_impact_reason(&dispatch).is_some());
    }
}
