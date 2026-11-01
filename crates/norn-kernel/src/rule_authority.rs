//! Correspondence boundary for `field_authority.ruleAuthorityIsKernelSealed`.

use std::collections::{BTreeMap, BTreeSet};

use norn_decisions::{
    AdmittedDecision, DecisionBinding, DecisionError, TypedEvent, ValidatedSlice,
};
use norn_expr::EvaluationBudget;
use norn_ir::{DecisionOutput, RuleRoot, RuntimeObject};

use crate::cell_host::{CommitReceipt, JsonObject};
use crate::decision_commit::{
    AdmissionOrigin, commit_prepared_decision, prepare_decision_with_limits_for,
};
use crate::envelope::EnvelopeCounts;
use crate::{CellHost, FactEvent};
use crate::{
    DecisionRefusal, DecisionTurn, EnvelopeLimits, FactKey, FactSlice, PreparedDecision,
    TypeCatalog,
};

/// A rule and exact closure admitted for a cell's pinned definition set.
#[derive(Debug, Clone)]
pub struct AdmittedRule {
    decision: AdmittedDecision,
    pin: String,
}

/// Rule loading or canonical definition commitment failed.
#[derive(Debug, thiserror::Error)]
pub enum RuleAdmissionError {
    /// Stock admission requires a valid, complete typed definition set.
    #[error("invalid stock rule definition set")]
    StockDefinition,
    /// The evaluator refused the rule or its closure.
    #[error(transparent)]
    Admission(#[from] norn_decisions::AdmissionError),
    /// The exact definition could not be committed.
    #[error(transparent)]
    Canonical(#[from] norn_ir::canonical::CanonicalJsonError),
}

impl AdmittedRule {
    /// Admit and pin the root, closure, exact defaults and checker binding.
    ///
    /// # Errors
    /// Refuses invalid rules and non-canonical definitions.
    pub fn admit(
        root: &RuleRoot,
        closure: &BTreeMap<String, RuleRoot>,
        binding: DecisionBinding,
        exact_defaults: Option<&RuntimeObject>,
    ) -> Result<Self, RuleAdmissionError> {
        let pin = norn_ir::canonical::domain_hash_hex(
            b"norn.rule-authority.v1\0",
            &(
                root,
                closure,
                &binding.profile_id,
                &binding.environment_hash,
                exact_defaults,
            ),
        )?;
        let decision =
            AdmittedDecision::admit_with_defaults(root, closure, binding, exact_defaults)?;
        Ok(Self { decision, pin })
    }

    /// The admitted evaluator, for typed input validation.
    #[must_use]
    pub const fn decision(&self) -> &AdmittedDecision {
        &self.decision
    }
}

/// Exact evaluated effects and their kernel-sealed rule identity and pin.
/// No constructor, deserializer or mutable output accessor is exposed.
///
/// ```compile_fail
/// let forged = norn_kernel::RuleDecision {
///     rule_id: "declared.rule".to_owned(), pin: "claimed-pin".to_owned(),
///     output: norn_ir::DecisionOutput::default(),
/// };
/// ```
#[derive(Debug)]
pub struct RuleDecision {
    rule_id: String,
    pin: String,
    output: DecisionOutput,
}

/// Evaluate an admitted rule; only this path mints rule authority.
///
/// # Errors
/// Returns the evaluator's typed input, budget or output refusal.
pub fn evaluate_rule(
    rule: &AdmittedRule,
    slice: &ValidatedSlice,
    event: &TypedEvent,
    budget: EvaluationBudget,
) -> Result<RuleDecision, DecisionError> {
    let evaluated = norn_decisions::evaluate(rule.decision(), slice, event, budget)?;
    Ok(RuleDecision {
        rule_id: rule.decision().rule_id().to_owned(),
        pin: rule.pin.clone(),
        output: evaluated.output,
    })
}

/// Prepare sealed effects only under the exact admitted rule definition.
///
/// # Errors
/// `decision.invalid_output` for absent or changed rule authority, otherwise
/// the shared decision admission refusal. Caller-supplied `DecisionOutput`
/// continues through `prepare_decision` without rule authority.
pub fn prepare_rule_decision<X>(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    decision: &RuleDecision,
    source_owned_targets: &BTreeSet<FactKey>,
) -> Result<PreparedDecision, DecisionRefusal<X>> {
    if catalog
        .rules
        .get(decision.rule_id.as_str())
        .is_none_or(|rule| rule.pin != decision.pin)
        && catalog
            .stock_rules
            .get(decision.rule_id.as_str())
            .is_none_or(|rule| {
                rule.pin != decision.pin
                    || catalog.stock_schema_pin().ok().as_ref() != Some(&rule.schema_pin)
            })
    {
        return Err(DecisionRefusal::InvalidOutput {
            reason: "rule result does not match the admitted rule identity and definition pin"
                .to_owned(),
        });
    }
    prepare_decision_with_limits_for(
        catalog,
        slice,
        turn,
        &decision.output,
        &EnvelopeLimits::V1,
        AdmissionOrigin::Rule,
        source_owned_targets,
    )
}

/// Atomically commit a sealed rule result through the shared commit boundary.
///
/// # Errors
/// Returns rule binding, admission or host commit refusals without applying effects.
pub fn commit_rule_decision<H>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    decision: &RuleDecision,
) -> Result<CommitReceipt, DecisionRefusal<H::Error>>
where
    H: CellHost + ?Sized,
    H::Event: From<FactEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject>,
{
    let prepared = prepare_rule_decision(catalog, slice, turn, decision, &BTreeSet::new())?;
    let counts = EnvelopeCounts::new(
        prepared.events.len(),
        decision.output.commands.len(),
        decision.output.timers.len(),
    );
    commit_prepared_decision(host, turn, prepared, counts, None)
}

/// A checker-admitted closed stock rule, pinned to its complete definition set.
#[derive(Debug, Clone)]
pub struct AdmittedStockRule {
    rule_id: String,
    pin: String,
    schema_pin: String,
    artifact_id: String,
    version: String,
    scope: norn_ir::ScopeRef,
    action: norn_ir::ActionRoot,
}

impl AdmittedStockRule {
    pub(crate) fn receipt_binding<X>(
        &self,
        scope: &BTreeMap<String, String>,
    ) -> Result<String, crate::ActionRefusal<X>> {
        norn_ir::canonical::domain_hash_hex(b"stock.receipt/1\0", &(&self.pin, scope))
            .map(|pin| format!("stock.post/1:{pin}"))
            .map_err(|_error| crate::ActionRefusal::InvalidInput {
                field: "scope".into(),
            })
    }

    pub(crate) fn seal(&self, output: DecisionOutput) -> RuleDecision {
        RuleDecision {
            rule_id: self.rule_id.clone(),
            pin: self.pin.clone(),
            output,
        }
    }

    /// The admitted action; this immutable view grants no rule seal.
    #[must_use]
    pub const fn action(&self) -> &norn_ir::ActionRoot {
        &self.action
    }
}

fn stock_decode<T: serde::de::DeserializeOwned>(
    value: norn_ir::JsonValue,
) -> Result<T, RuleAdmissionError> {
    use serde::de::IntoDeserializer;
    T::deserialize(value.into_deserializer())
        .map_err(|_decode: serde::de::value::Error| RuleAdmissionError::StockDefinition)
}

/// Admit a complete, self-contained typed stock definition set before activation.
/// The checker remains the admission authority; stockPost's wire shape is unchanged.
///
/// # Errors
/// Refuses invalid definitions or non-canonical pins.
pub fn admit_stock_rules(
    artifacts: &[norn_ir::ArtifactEnvelope],
) -> Result<Vec<AdmittedStockRule>, RuleAdmissionError> {
    use norn_ir::{ArtifactPayload, EntityRoot};
    let registry = BTreeSet::new();
    let context = norn_checker::EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    if norn_checker::check_artifact_set(artifacts, &context)
        .iter()
        .any(|d| d.severity == norn_ir::DiagnosticSeverity::Error)
    {
        return Err(RuleAdmissionError::StockDefinition);
    }
    let (mut types, mut interfaces, mut extensions, mut properties) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for artifact in artifacts {
        if let ArtifactPayload::Entity(payload) = &artifact.payload {
            match stock_decode::<EntityRoot>(payload.to_json_value())? {
                EntityRoot::V2(root) => types.push(root.object_type),
                EntityRoot::V2Interface(root) => interfaces.push(root.interface),
                EntityRoot::V2Extension(root) => extensions.push(root.object_type_extension),
                EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
                EntityRoot::V2Markings(_) => {}
                EntityRoot::V1(_) => return Err(RuleAdmissionError::StockDefinition),
            }
        }
    }
    let schema_pin = TypeCatalog::new(&types)
        .with_interfaces(&interfaces)
        .with_extensions(&extensions)
        .with_property_types(&properties)
        .stock_schema_pin()?;
    let mut ordered: Vec<_> = artifacts.iter().collect();
    ordered.sort_by(|a, b| (&a.artifact_id, &a.version).cmp(&(&b.artifact_id, &b.version)));
    let definition_pin = norn_ir::canonical::domain_hash_hex(b"stock.post/1\0", &ordered)?;
    let mut rules = Vec::new();
    for artifact in artifacts {
        if let ArtifactPayload::Action(payload) = &artifact.payload {
            let action: norn_ir::ActionRoot = stock_decode(payload.to_json_value())?;
            if action.stock_post.is_some() {
                let rule_id = format!("{}#stockPost", action.action_id);
                let pin = norn_ir::canonical::domain_hash_hex(
                    b"norn.rule-authority.stock/1\0",
                    &(&definition_pin, &rule_id, &schema_pin),
                )?;
                rules.push(AdmittedStockRule {
                    rule_id,
                    pin,
                    schema_pin: schema_pin.clone(),
                    artifact_id: artifact.artifact_id.clone(),
                    version: artifact.version.clone(),
                    scope: artifact.scope.clone(),
                    action,
                });
            }
        }
    }
    Ok(rules)
}

impl<'a> TypeCatalog<'a> {
    /// Register only opaque checker-admitted stock rules for this catalog.
    #[must_use]
    pub fn with_stock_rules(
        mut self,
        rules: impl IntoIterator<Item = &'a AdmittedStockRule>,
    ) -> Self {
        self.stock_rules
            .extend(rules.into_iter().map(|r| (r.rule_id.as_str(), r)));
        self
    }
    /// Resolve the stable stock action used by an engine invoke adapter.
    #[must_use]
    pub fn stock_action(&self, action_id: &str) -> Option<&AdmittedStockRule> {
        self.stock_rules
            .get(format!("{action_id}#stockPost").as_str())
            .copied()
    }
    pub(crate) fn checked_stock_rule<X>(
        &self,
        call: &crate::ActionCall<'_>,
    ) -> Result<&AdmittedStockRule, crate::ActionRefusal<X>> {
        let rule = self.stock_action(&call.action.action_id).ok_or_else(|| {
            crate::ActionRefusal::UnsupportedEffect {
                action_id: call.action.action_id.clone(),
            }
        })?;
        let scoped = [
            ("orgId", &rule.scope.org_id),
            ("tenantId", &rule.scope.tenant_id),
            ("workspaceId", &rule.scope.workspace_id),
            ("environmentId", &rule.scope.environment_id),
            ("businessUnitId", &rule.scope.business_unit_id),
        ];
        if rule.action != *call.action
            || rule.artifact_id != call.action_artifact_id
            || rule.version != call.action_version
            || self.stock_schema_pin().ok().as_ref() != Some(&rule.schema_pin)
            || scoped.iter().any(|(key, value)| {
                value
                    .as_ref()
                    .is_some_and(|expected| call.scope.get(*key) != Some(expected))
            })
        {
            return Err(crate::ActionRefusal::Decision(
                DecisionRefusal::InvalidOutput {
                    reason:
                        "stock rule does not match the admitted action, scope and definition pin"
                            .into(),
                },
            ));
        }
        Ok(rule)
    }
}

#[cfg(test)]
mod stock_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "sealed-origin conformance fixture"
    )]
    use super::*;
    use crate::{LogicalTime, World};
    #[test]
    fn post_sealed_stock_refuses_forged_unpinned_or_changed_origins() {
        let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_str(include_str!(
            "../../../tools/norn-ts/fixtures/stock/stock.artifacts.json"
        ))
        .unwrap();
        let rules = admit_stock_rules(&artifacts).unwrap();
        assert_eq!(rules.len(), 2);
        let mut types: Vec<norn_ir::ObjectTypeDef> = artifacts
            .iter()
            .filter_map(|a| {
                if let norn_ir::ArtifactPayload::Entity(payload) = &a.payload {
                    payload
                        .object_type
                        .as_value()
                        .map(|v| stock_decode(v.clone()).unwrap())
                } else {
                    None
                }
            })
            .collect();
        let catalog = TypeCatalog::new(&types).with_stock_rules(&rules);
        let rule = &rules[0];
        let turn = DecisionTurn {
            id: "turn".into(),
            now: LogicalTime::from_millis(1),
            world: World::Main,
            consumes: None,
        };
        let scope = BTreeMap::from([("orgId".into(), "org_demo".into())]);
        let binding = rule
            .receipt_binding::<std::convert::Infallible>(&scope)
            .unwrap();
        assert_eq!(
            binding,
            rule.receipt_binding::<std::convert::Infallible>(&scope)
                .unwrap(),
            "POST-RETRY-PIN"
        );
        assert_ne!(
            binding,
            rule.receipt_binding::<std::convert::Infallible>(&BTreeMap::new())
                .unwrap(),
            "POST-RETRY-SCOPE"
        );
        let mut decision = RuleDecision {
            rule_id: rule.rule_id.clone(),
            pin: "forged".into(),
            output: DecisionOutput::default(),
        };
        let prepare = |catalog: &TypeCatalog<'_>, decision: &RuleDecision| {
            prepare_rule_decision::<std::convert::Infallible>(
                catalog,
                &FactSlice::new(),
                &turn,
                decision,
                &BTreeSet::new(),
            )
        };
        assert!(
            prepare(&catalog, &decision).is_err(),
            "POST-SEALED-STOCK forged pin"
        );
        decision.pin.clone_from(&rule.pin);
        assert!(
            prepare(&TypeCatalog::new(&types), &decision).is_err(),
            "POST-SEALED-STOCK absent admitted origin"
        );
        assert!(prepare(&catalog, &decision).is_ok());
        let mut extensions: Vec<norn_ir::ObjectTypeExtension> = serde_json::from_value(serde_json::json!([{"namespace":"a","target":"stock.Item"},{"namespace":"b","target":"stock.Item"}])).unwrap();
        let mut properties: Vec<norn_ir::PropertyTypeDef> = serde_json::from_value(serde_json::json!([{"name":"stock.a","type":{"kind":"text"}},{"name":"stock.b","type":{"kind":"text"}}])).unwrap();
        let before = TypeCatalog::new(&types)
            .with_extensions(&extensions)
            .with_property_types(&properties)
            .stock_schema_pin()
            .unwrap();
        extensions.reverse();
        properties.reverse();
        assert_eq!(
            before,
            TypeCatalog::new(&types)
                .with_extensions(&extensions)
                .with_property_types(&properties)
                .stock_schema_pin()
                .unwrap(),
            "POST-CATALOG-ORDER"
        );
        drop(catalog);
        types[0].description = Some("changed definition".into());
        assert!(
            prepare(
                &TypeCatalog::new(&types).with_stock_rules(&rules),
                &decision
            )
            .is_err(),
            "POST-SEALED-STOCK changed catalog"
        );
    }
}
