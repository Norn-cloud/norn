//! Domain Model Coverage Observer (José Valim style, #1656).
//!
//! Computes the exact expected coverage surface directly from an
//! [`AdmittedPack`] — every declared `(object_type, lifecycle, from -> to)`
//! transition, every `immutable` state and `immutable: true` entity refusal,
//! every governing `LawIR` (`holds` and `violated`), and every `AccessIR` Cedar
//! rule (`allow` and `deny`) — and compares it against hits recorded during a
//! test run.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use norn_kernel::authz::{AccessRequest, DenyReason, Pdp, PdpDecision};
use norn_kernel::pack_mbt_support::{LawResponse, Lifecycle, ObjectTypeDef, PdpExplanation};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest,
};

use super::{AdmittedPack, action_call_object_type};

/// Key for one expected or observed lifecycle transition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DomainTransition {
    /// Owning object type name.
    pub object_type: String,
    /// Lifecycle property name.
    pub lifecycle: String,
    /// Declared transition id.
    pub transition: String,
    /// Source state.
    pub from: String,
    /// Target state.
    pub to: String,
}

/// Key for one immutable target `(object_type, lifecycle, state)`.
///
/// For entity-level `immutable: true` types, `lifecycle` is `"<object>"` and
/// `state` is `"<immutable_object>"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImmutableTarget {
    /// Owning object type name.
    pub object_type: String,
    /// Lifecycle property name, or `"<object>"` for entity-level immutability.
    pub lifecycle: String,
    /// Immutable lifecycle state name, or `"<immutable_object>"` for entity-level immutability.
    pub state: String,
}

/// Key for one `AccessIR` rule `(policy_id, rule_id)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PolicyRuleTarget {
    /// Policy artifact id.
    pub policy_id: String,
    /// Rule id within the policy.
    pub rule_id: String,
}

/// Expected coverage targets derived mechanically from an [`AdmittedPack`]'s
/// resolved [`norn_kernel::TypeCatalog`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainCoveragePlan {
    /// All effective `(object_type, lifecycle, transition, from -> to)` edges.
    pub transitions: BTreeSet<DomainTransition>,
    /// All effective immutable lifecycle states and immutable entity types.
    pub immutable_targets: BTreeSet<ImmutableTarget>,
    /// All governing predicate/invariant law ids.
    pub laws: BTreeSet<String>,
    /// All `AccessIR` `(policy_id, rule_id)` entries.
    pub cedar_rules: BTreeSet<PolicyRuleTarget>,
}

impl DomainCoveragePlan {
    /// Builds the expected coverage plan from `pack`'s resolved effective catalog.
    #[must_use]
    pub fn from_pack(pack: &AdmittedPack) -> Self {
        let mut plan = Self::default();
        let extra = [action_call_object_type()];
        let catalog = pack.type_catalog(&extra);
        for object_type in &pack.object_types {
            let ty = object_type.name.as_str();
            if is_effectively_immutable(pack, object_type) {
                plan.immutable_targets.insert(ImmutableTarget {
                    object_type: ty.to_owned(),
                    lifecycle: "<object>".to_owned(),
                    state: "<immutable_object>".to_owned(),
                });
                continue;
            }
            for lifecycle in catalog.lifecycles(&object_type.name).into_values() {
                let prop = lifecycle.property.as_str();
                for state in &lifecycle.immutable {
                    plan.immutable_targets.insert(ImmutableTarget {
                        object_type: ty.to_owned(),
                        lifecycle: prop.to_owned(),
                        state: state.clone(),
                    });
                }
                for transition in &lifecycle.transitions {
                    for from in &transition.from {
                        plan.transitions.insert(DomainTransition {
                            object_type: ty.to_owned(),
                            lifecycle: prop.to_owned(),
                            transition: transition.id.as_str().to_owned(),
                            from: from.clone(),
                            to: transition.to.clone(),
                        });
                    }
                }
            }
        }
        for law in &pack.laws {
            if matches!(
                law.response,
                LawResponse::Predicate { .. } | LawResponse::Invariant { .. }
            ) {
                plan.laws.insert(law.id.clone());
            }
        }
        for policy in &pack.access_policies {
            for rule in &policy.rules {
                plan.cedar_rules.insert(PolicyRuleTarget {
                    policy_id: policy.policy_id.clone(),
                    rule_id: rule.rule_id.clone(),
                });
            }
        }
        plan
    }
}

/// Live domain-model coverage tally collected across an MBT/DST run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainCoverage {
    /// Valid lifecycle transitions hit.
    pub transitions: BTreeMap<DomainTransition, u64>,
    /// Disallowed transitions refused.
    pub disallowed_transitions: BTreeMap<DomainTransition, u64>,
    /// Immutable state or entity mutation refusals hit.
    pub immutable_refusals: BTreeMap<ImmutableTarget, u64>,
    /// Governing laws that held.
    pub law_holds: BTreeMap<String, u64>,
    /// Governing laws that refused a write.
    pub law_violations: BTreeMap<String, u64>,
    /// Cedar rules that allowed a call.
    pub cedar_allows: BTreeMap<PolicyRuleTarget, u64>,
    /// Cedar denies by `(policy_id_or_action, rule_id_or_reason)`.
    pub cedar_denies: BTreeMap<PolicyRuleTarget, u64>,
}

impl DomainCoverage {
    /// Increments a valid transition counter.
    pub fn hit_transition(&mut self, key: DomainTransition) {
        bump(&mut self.transitions, key);
    }

    /// Increments a disallowed transition refusal counter.
    pub fn hit_disallowed_transition(&mut self, key: DomainTransition) {
        bump(&mut self.disallowed_transitions, key);
    }

    /// Increments an immutable refusal counter.
    pub fn hit_immutable_refusal(
        &mut self,
        object_type: impl Into<String>,
        lifecycle: impl Into<String>,
        state: impl Into<String>,
    ) {
        bump(
            &mut self.immutable_refusals,
            ImmutableTarget {
                object_type: object_type.into(),
                lifecycle: lifecycle.into(),
                state: state.into(),
            },
        );
    }

    /// Increments a governing law `holds` counter.
    pub fn hit_law_hold(&mut self, law_id: impl Into<String>) {
        bump(&mut self.law_holds, law_id.into());
    }

    /// Increments a governing law `violated` counter.
    pub fn hit_law_violation(&mut self, law_id: impl Into<String>) {
        bump(&mut self.law_violations, law_id.into());
    }

    /// Increments a Cedar `allow` counter.
    pub fn hit_cedar_allow(&mut self, policy_id: impl Into<String>, rule_id: impl Into<String>) {
        bump(
            &mut self.cedar_allows,
            PolicyRuleTarget {
                policy_id: policy_id.into(),
                rule_id: rule_id.into(),
            },
        );
    }

    /// Increments a Cedar `deny` counter.
    pub fn hit_cedar_deny(&mut self, policy_id: impl Into<String>, rule_id: impl Into<String>) {
        bump(
            &mut self.cedar_denies,
            PolicyRuleTarget {
                policy_id: policy_id.into(),
                rule_id: rule_id.into(),
            },
        );
    }

    /// Merges another coverage tally into `self`.
    pub fn merge(&mut self, other: &Self) {
        merge_counts(&mut self.transitions, &other.transitions);
        merge_counts(
            &mut self.disallowed_transitions,
            &other.disallowed_transitions,
        );
        merge_counts(&mut self.immutable_refusals, &other.immutable_refusals);
        merge_counts(&mut self.law_holds, &other.law_holds);
        merge_counts(&mut self.law_violations, &other.law_violations);
        merge_counts(&mut self.cedar_allows, &other.cedar_allows);
        merge_counts(&mut self.cedar_denies, &other.cedar_denies);
    }

    /// Produces a structured gap report against `plan`.
    #[must_use]
    pub fn report(
        &self,
        pack_name: impl Into<String>,
        plan: &DomainCoveragePlan,
    ) -> DomainCoverageReport {
        let uncovered_transitions = plan
            .transitions
            .iter()
            .filter(|tr| !self.transitions.contains_key(*tr))
            .cloned()
            .collect();
        let uncovered_immutable_refusals = plan
            .immutable_targets
            .iter()
            .filter(|target| !self.immutable_refusals.contains_key(*target))
            .cloned()
            .collect();
        let uncovered_law_holds = plan
            .laws
            .iter()
            .filter(|law| !self.law_holds.contains_key(*law))
            .cloned()
            .collect();
        let uncovered_law_violations = plan
            .laws
            .iter()
            .filter(|law| !self.law_violations.contains_key(*law))
            .cloned()
            .collect();
        let uncovered_cedar_allows = plan
            .cedar_rules
            .iter()
            .filter(|rule| !self.cedar_allows.contains_key(*rule))
            .cloned()
            .collect();
        let uncovered_cedar_denies = plan
            .cedar_rules
            .iter()
            .filter(|rule| !self.cedar_denies.contains_key(*rule))
            .cloned()
            .collect();

        DomainCoverageReport {
            pack_name: pack_name.into(),
            plan: plan.clone(),
            observed: self.clone(),
            uncovered_transitions,
            uncovered_immutable_refusals,
            uncovered_law_holds,
            uncovered_law_violations,
            uncovered_cedar_allows,
            uncovered_cedar_denies,
        }
    }
}

/// Structured coverage report highlighting hit vs uncovered domain elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainCoverageReport {
    /// Pack identifier.
    pub pack_name: String,
    /// Expected coverage plan derived from the pack IR.
    pub plan: DomainCoveragePlan,
    /// Observed hits.
    pub observed: DomainCoverage,
    /// Declared lifecycle transitions never hit.
    pub uncovered_transitions: Vec<DomainTransition>,
    /// Declared immutable states/entities whose mutation refusal was never hit.
    pub uncovered_immutable_refusals: Vec<ImmutableTarget>,
    /// Governing laws whose `holds` path was never hit.
    pub uncovered_law_holds: Vec<String>,
    /// Governing laws whose `violated` path was never hit.
    pub uncovered_law_violations: Vec<String>,
    /// Cedar rules whose `allow` path was never hit.
    pub uncovered_cedar_allows: Vec<PolicyRuleTarget>,
    /// Cedar rules whose `deny` path was never hit.
    pub uncovered_cedar_denies: Vec<PolicyRuleTarget>,
}

impl DomainCoverageReport {
    /// Returns `Ok(())` when every declared transition was exercised at least once.
    ///
    /// # Errors
    ///
    /// Returns a formatted diagnostic listing uncovered transitions.
    pub fn check_transitions_covered(&self) -> Result<(), String> {
        if self.uncovered_transitions.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "uncovered lifecycle transitions in {}: {:?}",
                self.pack_name, self.uncovered_transitions
            ))
        }
    }

    /// Returns `Ok(())` when every declared immutable state and immutable entity
    /// had its refusal exercised at least once.
    ///
    /// # Errors
    ///
    /// Returns a formatted diagnostic listing uncovered immutable targets.
    pub fn check_immutable_refusals_covered(&self) -> Result<(), String> {
        if self.uncovered_immutable_refusals.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "uncovered immutable refusals in {}: {:?}",
                self.pack_name, self.uncovered_immutable_refusals
            ))
        }
    }

    /// Returns `Ok(())` when every Cedar rule had both an `allow` and a `deny` hit.
    ///
    /// # Errors
    ///
    /// Returns a formatted diagnostic listing uncovered Cedar rules.
    pub fn check_cedar_covered(&self) -> Result<(), String> {
        if self.uncovered_cedar_allows.is_empty() && self.uncovered_cedar_denies.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "uncovered Cedar rules in {}: allow={:?}, deny={:?}",
                self.pack_name, self.uncovered_cedar_allows, self.uncovered_cedar_denies
            ))
        }
    }

    /// Returns `Ok(())` when every governing law had both `holds` and `violated` exercised.
    ///
    /// # Errors
    ///
    /// Returns a formatted diagnostic listing uncovered laws.
    pub fn check_laws_covered(&self) -> Result<(), String> {
        if self.uncovered_law_holds.is_empty() && self.uncovered_law_violations.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "uncovered laws in {}: holds={:?}, violated={:?}",
                self.pack_name, self.uncovered_law_holds, self.uncovered_law_violations
            ))
        }
    }
}

impl fmt::Display for DomainCoverageReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hit_tr = self
            .plan
            .transitions
            .len()
            .saturating_sub(self.uncovered_transitions.len());
        let hit_imm = self
            .plan
            .immutable_targets
            .len()
            .saturating_sub(self.uncovered_immutable_refusals.len());
        writeln!(f, "DomainCoverageReport({})", self.pack_name)?;
        writeln!(
            f,
            "  transitions: {hit_tr}/{} (disallowed refused: {})",
            self.plan.transitions.len(),
            self.observed.disallowed_transitions.len()
        )?;
        writeln!(
            f,
            "  immutable refusals: {hit_imm}/{}",
            self.plan.immutable_targets.len()
        )?;
        writeln!(
            f,
            "  laws: holds={}/{}, violated={}/{}",
            self.plan
                .laws
                .len()
                .saturating_sub(self.uncovered_law_holds.len()),
            self.plan.laws.len(),
            self.plan
                .laws
                .len()
                .saturating_sub(self.uncovered_law_violations.len()),
            self.plan.laws.len()
        )?;
        writeln!(
            f,
            "  cedar rules: allow={}/{}, deny={}/{}",
            self.plan
                .cedar_rules
                .len()
                .saturating_sub(self.uncovered_cedar_allows.len()),
            self.plan.cedar_rules.len(),
            self.plan
                .cedar_rules
                .len()
                .saturating_sub(self.uncovered_cedar_denies.len()),
            self.plan.cedar_rules.len()
        )?;
        Ok(())
    }
}

/// Returns whether `object_type` is immutable directly or via an `extends`
/// ancestor in `pack`.
#[must_use]
pub(crate) fn is_effectively_immutable(pack: &AdmittedPack, object_type: &ObjectTypeDef) -> bool {
    super::object_type_ancestors(pack, object_type)
        .into_iter()
        .any(|def| def.immutable)
}

/// [`ActionAuthorizer`] backed by a real Cedar [`Pdp`] via [`Pdp::explain`]
/// that records every `allow` and policy `Access` `deny` decision into a
/// shared [`DomainCoverage`] and retains the last [`PdpExplanation`].
type ReadRequestBuilder = Box<
    dyn for<'a> Fn(&ActionReadAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>
        + Send
        + Sync,
>;

type ReadTypeRequestBuilder = Box<
    dyn for<'a> Fn(&ActionReadTypeAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>
        + Send
        + Sync,
>;

enum ReadAuthzMode {
    DenyByDefault,
    AllowActionOnlySliceReads,
    Pdp {
        build_read: ReadRequestBuilder,
        build_read_type: ReadTypeRequestBuilder,
    },
}

/// An [`ActionAuthorizer`] backed by a real `norn-authz` [`Pdp`] that records
/// every determining policy rule allow and access denial directly into a
/// [`DomainCoverage`] collector.
///
/// Slice reads (`authorize_read` and `authorize_read_type`) fail closed by
/// default (`Err(ActionDenial::Invisible)`), matching `norn-kernel`'s own
/// default. Call [`Self::with_read_authorizers`] to evaluate slice reads
/// through the same [`Pdp`], or [`Self::allow_action_only_slice_reads`] when
/// driving an action-only `AccessIR` policy that declares no read rules.
pub struct ObservedPdpAuthorizer<F> {
    pdp: Arc<Pdp>,
    build_request: F,
    coverage: Arc<Mutex<DomainCoverage>>,
    deny_rule_hint: Option<PolicyRuleTarget>,
    last_explanation: Mutex<Option<PdpExplanation>>,
    read_mode: ReadAuthzMode,
}

impl<F> ObservedPdpAuthorizer<F>
where
    F: for<'a> Fn(&ActionAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>,
{
    /// Builds an observed Cedar authorizer whose slice reads fail closed until
    /// configured with [`Self::with_read_authorizers`] or
    /// [`Self::allow_action_only_slice_reads`].
    #[must_use]
    pub fn new(
        pdp: Arc<Pdp>,
        coverage: Arc<Mutex<DomainCoverage>>,
        deny_rule_hint: Option<PolicyRuleTarget>,
        build_request: F,
    ) -> Self {
        Self {
            pdp,
            build_request,
            coverage,
            deny_rule_hint,
            last_explanation: Mutex::new(None),
            read_mode: ReadAuthzMode::DenyByDefault,
        }
    }

    /// Evaluates `authorize_read` and `authorize_read_type` through the same
    /// [`Pdp`], recording allow/deny hits and returning [`ActionDenial::Invisible`]
    /// on any denial.
    #[must_use]
    pub fn with_read_authorizers<R, T>(mut self, build_read: R, build_read_type: T) -> Self
    where
        R: for<'a> Fn(&ActionReadAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>
            + Send
            + Sync
            + 'static,
        T: for<'a> Fn(&ActionReadTypeAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>
            + Send
            + Sync
            + 'static,
    {
        self.read_mode = ReadAuthzMode::Pdp {
            build_read: Box::new(build_read),
            build_read_type: Box::new(build_read_type),
        };
        self
    }

    /// Explicitly permits slice reads when verifying action-only `AccessIR`
    /// policies that declare no `norn.read` rules, so `commit_action` reaches
    /// the action PDP decision under test.
    #[must_use]
    pub fn allow_action_only_slice_reads(mut self) -> Self {
        self.read_mode = ReadAuthzMode::AllowActionOnlySliceReads;
        self
    }

    /// Returns the most recent [`PdpExplanation`] produced by this authorizer.
    #[must_use]
    pub fn last_explanation(&self) -> Option<PdpExplanation> {
        let guard = match self.last_explanation.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clone()
    }

    fn evaluate_pdp(&self, access_request: &AccessRequest, fallback_action: &str) -> PdpDecision {
        let explanation = self.pdp.explain(access_request);
        {
            let mut last = match self.last_explanation.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *last = Some(explanation.clone());
        }
        let mut cov = match self.coverage.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match explanation.decision {
            PdpDecision::Allow => {
                for determining in &explanation.determining_policies {
                    if let (Some(policy_id), Some(rule_id)) =
                        (&determining.policy_id, &determining.rule_id)
                    {
                        cov.hit_cedar_allow(policy_id.clone(), rule_id.clone());
                    }
                }
            }
            PdpDecision::Deny(reason) => {
                if reason == DenyReason::Access {
                    if let Some(hint) = &self.deny_rule_hint {
                        cov.hit_cedar_deny(hint.policy_id.clone(), hint.rule_id.clone());
                    } else {
                        cov.hit_cedar_deny(fallback_action.to_owned(), reason.code());
                    }
                }
            }
        }
        explanation.decision
    }
}

impl<F> ActionAuthorizer for ObservedPdpAuthorizer<F>
where
    F: for<'a> Fn(&ActionAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>,
{
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let access_request = (self.build_request)(request)?;
        match self.evaluate_pdp(&access_request, &request.action.action_id) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(reason) => Err(ActionDenial::Forbidden {
                reason: reason.code().to_owned(),
            }),
        }
    }

    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        match &self.read_mode {
            ReadAuthzMode::DenyByDefault => Err(ActionDenial::Invisible),
            ReadAuthzMode::AllowActionOnlySliceReads => Ok(()),
            ReadAuthzMode::Pdp { build_read, .. } => {
                let access_request = build_read(request)?;
                match self.evaluate_pdp(&access_request, access_request.action_id()) {
                    PdpDecision::Allow => Ok(()),
                    PdpDecision::Deny(_) => Err(ActionDenial::Invisible),
                }
            }
        }
    }

    fn authorize_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        match &self.read_mode {
            ReadAuthzMode::DenyByDefault => Err(ActionDenial::Invisible),
            ReadAuthzMode::AllowActionOnlySliceReads => Ok(()),
            ReadAuthzMode::Pdp {
                build_read_type, ..
            } => {
                let access_request = build_read_type(request)?;
                match self.evaluate_pdp(&access_request, access_request.action_id()) {
                    PdpDecision::Allow => Ok(()),
                    PdpDecision::Deny(_) => Err(ActionDenial::Invisible),
                }
            }
        }
    }
}

/// Selects a lifecycle state distinct from `current` so direct lifecycle write
/// refusal checks exercise a real state change instead of a same-state no-op.
pub(crate) fn distinct_lifecycle_state(lifecycle: &Lifecycle, current: &str) -> String {
    lifecycle
        .initial
        .iter()
        .chain(lifecycle.terminal.iter())
        .chain(lifecycle.transitions.iter().map(|tr| &tr.to))
        .chain(lifecycle.transitions.iter().flat_map(|tr| tr.from.iter()))
        .find(|candidate| candidate.as_str() != current)
        .cloned()
        .unwrap_or_else(|| format!("{current}_illegal"))
}

fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, key: K) {
    let slot = map.entry(key).or_insert(0);
    *slot = slot.saturating_add(1);
}

fn merge_counts<K: Ord + Clone>(dst: &mut BTreeMap<K, u64>, src: &BTreeMap<K, u64>) {
    for (k, v) in src {
        let slot = dst.entry(k.clone()).or_insert(0);
        *slot = slot.saturating_add(*v);
    }
}
