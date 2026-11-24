//! Static Structural Gap Detector over admitted pack IR (#1656).
//!
//! Operates strictly on IR (no IO, no runtime cell) and labels every finding
//! honestly as [`EvidenceLevel::Structural`] or [`EvidenceLevel::ExhaustiveFinite`].
//! Never invents business policy: high-impact actions lacking separation-of-duties
//! policies are emitted as explicit [`GapSeverity::Question`] items for pack
//! authors and operators.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use norn_kernel::norn_expr::expression_read_paths;
use norn_kernel::norn_ir::access::{AccessCondition, AccessIr, ResourceCompareOp, ResourceOperand};
use norn_kernel::norn_ir::{
    ActionEffectProgram, ActionUiEffect, LawResponse, Lifecycle, ObjectTypeDef, PropertyType,
    WriteChange,
};

use super::{AdmittedPack, EvidenceLevel};

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
    /// governed action in the pack ever writes.
    OrphanField {
        /// Owning object type.
        object_type: String,
        /// Property id.
        property: String,
        /// Declarations that reference this field.
        referenced_in: Vec<String>,
    },
    /// An immutable financial/ledger object type with no compensating or
    /// reversal action declared in the pack.
    ImmutableFinancialMissingReversal {
        /// Owning object type.
        object_type: String,
        /// Why the detector classified this type as immutable financial/ledger.
        reason: String,
    },
    /// A high-impact action whose `AccessIR` policy lacks an explicit
    /// separation-of-duties (`requester != caller`) resource predicate.
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
            Self::HighImpactActionMissingSod { .. } => GapSeverity::Question,
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

    /// Returns all explicit policy questions (such as missing SoD policies).
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

/// Runs all static structural gap detectors on `pack`.
#[must_use]
pub fn analyze_static_gaps(pack: &AdmittedPack) -> StaticGapReport {
    let mut gaps = Vec::new();
    let written_fields = collect_action_written_fields(pack);
    let transition_actions = collect_transition_actions(pack);
    let has_any_actions = !pack.actions.is_empty();

    for object_type in &pack.object_types {
        detect_lifecycle_gaps(object_type, &transition_actions, has_any_actions, &mut gaps);
        if has_any_actions {
            detect_orphan_fields(pack, object_type, &written_fields, &mut gaps);
        }
        detect_immutable_financial_reversal_gap(pack, object_type, &mut gaps);
    }
    detect_high_impact_sod_questions(pack, &mut gaps);

    StaticGapReport {
        pack_name: pack.name.clone(),
        gaps,
    }
}

fn enum_states_for(object_type: &ObjectTypeDef, property: &str) -> Vec<String> {
    object_type
        .properties
        .iter()
        .find(|prop| prop.id.as_str() == property)
        .and_then(|prop| match &prop.property_type {
            PropertyType::Enum { values } => {
                Some(values.iter().map(|item| item.value.clone()).collect())
            }
            PropertyType::Text {}
            | PropertyType::Integer {}
            | PropertyType::Decimal { .. }
            | PropertyType::Boolean {}
            | PropertyType::Date {}
            | PropertyType::Timestamp {}
            | PropertyType::JsonExt {}
            | PropertyType::Ref { .. } => None,
        })
        .unwrap_or_default()
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
    object_type: &ObjectTypeDef,
    transition_actions: &BTreeSet<(String, String, String)>,
    has_any_actions: bool,
    out: &mut Vec<StructuralGap>,
) {
    let type_name = object_type.name.as_str();
    for lifecycle in &object_type.lifecycles {
        let prop = lifecycle.property.as_str();
        let all_states = enum_states_for(object_type, prop);
        let reachable = reachable_states(lifecycle);
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

        if has_any_actions {
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
                    .and_then(norn_kernel::norn_ir::JsonValue::as_object)
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
                    .and_then(norn_kernel::norn_ir::JsonValue::as_object)
                    .and_then(|props| props.get(parent_values_field))
                    .and_then(norn_kernel::norn_ir::JsonValue::as_object)
                    .and_then(|schema| schema.get("properties"))
                    .and_then(norn_kernel::norn_ir::JsonValue::as_object)
                {
                    for field in parent_props.keys() {
                        entry.insert(field.clone());
                    }
                }
            }
        }
    }
    written
}

fn collect_referenced_fields(
    pack: &AdmittedPack,
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

    for lifecycle in &object_type.lifecycles {
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
    for invariant in &object_type.invariants {
        record(
            invariant.expression.as_str(),
            format!("invariant:{}", invariant.id.as_str()),
        );
    }
    for law in &pack.laws {
        if law.subject != object_type.name.as_str() {
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

fn detect_orphan_fields(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    written_fields: &BTreeMap<String, BTreeSet<String>>,
    out: &mut Vec<StructuralGap>,
) {
    let type_name = object_type.name.as_str();
    let Some(written) = written_fields.get(type_name) else {
        return;
    };
    let refs = collect_referenced_fields(pack, object_type);
    let lifecycle_props: BTreeSet<&str> = object_type
        .lifecycles
        .iter()
        .map(|lc| lc.property.as_str())
        .collect();

    for prop in &object_type.properties {
        let name = prop.id.as_str();
        if prop.is_read_only() || prop.default.is_some() || lifecycle_props.contains(name) {
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

fn is_financial_or_ledger_type(object_type: &ObjectTypeDef) -> Option<String> {
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
    for prop in &object_type.properties {
        if let PropertyType::Ref { name, .. } = &prop.property_type
            && name.as_str().contains("money")
        {
            return Some(format!("carries money property '{}'", prop.id.as_str()));
        }
    }
    None
}

fn has_reversal_or_compensation(pack: &AdmittedPack, object_type: &str) -> bool {
    for action in &pack.actions {
        let touches_type = action
            .root
            .writes
            .iter()
            .any(|write| write.entity_kind == object_type)
            || action
                .root
                .effect_program
                .as_ref()
                .is_some_and(|eff| eff.target().object_kind == object_type);
        if !touches_type {
            continue;
        }
        if action.root.compensation.is_some() {
            return true;
        }
        let lower_id = action.root.action_id.to_ascii_lowercase();
        if [
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
            return true;
        }
    }
    false
}

fn detect_immutable_financial_reversal_gap(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    out: &mut Vec<StructuralGap>,
) {
    let has_immutable_lifecycle_state = object_type
        .lifecycles
        .iter()
        .any(|lc| !lc.immutable.is_empty());
    if !object_type.immutable && !has_immutable_lifecycle_state {
        return;
    }
    let Some(financial_reason) = is_financial_or_ledger_type(object_type) else {
        return;
    };
    if !has_reversal_or_compensation(pack, object_type.name.as_str()) {
        out.push(StructuralGap::ImmutableFinancialMissingReversal {
            object_type: object_type.name.as_str().to_owned(),
            reason: format!(
                "immutable financial/ledger type ({financial_reason}) has no declared compensating or reversal action"
            ),
        });
    }
}

fn high_impact_reason(action: &norn_kernel::norn_ir::ActionRoot) -> Option<String> {
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
    let lower = action.action_id.to_ascii_lowercase();
    for keyword in [
        "approve", "promote", "retire", "post", "pay", "settle", "payout", "activate", "release",
        "void", "reverse",
    ] {
        if lower.contains(keyword) {
            return Some(format!("high-impact action verb '{keyword}'"));
        }
    }
    None
}

fn action_has_sod_policy(policies: &[AccessIr], action_id: &str) -> bool {
    policies.iter().any(|policy| {
        policy.rules.iter().any(|rule| {
            rule.actions.iter().any(|act| act == action_id)
                && rule.all_of.iter().any(|cond| {
                    matches!(
                        cond,
                        AccessCondition::Resource(res)
                            if res.op == ResourceCompareOp::Ne
                                && matches!(res.value, ResourceOperand::PrincipalAttribute(_))
                    )
                })
        })
    })
}

fn detect_high_impact_sod_questions(pack: &AdmittedPack, out: &mut Vec<StructuralGap>) {
    for action in &pack.actions {
        let Some(reason) = high_impact_reason(&action.root) else {
            continue;
        };
        if !action_has_sod_policy(&pack.access_policies, &action.root.action_id) {
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
