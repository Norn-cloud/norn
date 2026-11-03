//! Closed action/policy projection for conservative finite control analysis.
use crate::duties::{DutyEdge, DutyGraph};
use norn_ir::access::{AccessCondition, PolicyRoot};
use norn_ir::ActionRoot;
use std::collections::{BTreeMap, BTreeSet};

/// CONTROL-REFS-1: every policy action must have exactly one declared root.
/// Returns `None` (unknown) for missing/duplicate actions or unsupported policy
/// forms. A caller must never turn an incomplete projection into proof evidence.
#[must_use]
pub fn project(actions: &[ActionRoot], policies: &[PolicyRoot]) -> Option<DutyGraph> {
    let mut grants: BTreeMap<String, BTreeSet<String>> = actions
        .iter()
        .map(|a| (a.action_id.clone(), BTreeSet::new()))
        .collect();
    if grants.len() != actions.len() {
        return None;
    }
    for policy in policies {
        let PolicyRoot::Access(policy) = policy else {
            return None;
        };
        if !policy.generic_rules.is_empty() {
            return None;
        }
        for rule in &policy.rules {
            let mut roles: Option<BTreeSet<String>> = None;
            for condition in &rule.all_of {
                if let AccessCondition::Role(role) = condition {
                    let next: BTreeSet<_> = role.any_of.iter().cloned().collect();
                    roles = Some(roles.map_or_else(
                        || next.clone(),
                        |prior| prior.intersection(&next).cloned().collect(),
                    ));
                }
            }
            let roles = roles?;
            for action in &rule.actions {
                let granted = grants.get_mut(action)?;
                granted.extend(roles.clone());
            }
        }
    }
    Some(DutyGraph {
        states: vec!["existing_object".into()],
        initial: BTreeSet::from([0]),
        edges: actions
            .iter()
            .map(|a| DutyEdge {
                action: a.action_id.clone(),
                from: 0,
                to: 0,
            })
            .collect(),
        grants,
    })
}


#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "closed-reference model assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    fn action() -> ActionRoot {
        let envelope: serde_json::Value = serde_json::from_str(include_str!("../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json")).expect("action fixture");
        let mut action: ActionRoot = serde_json::from_value(envelope["payload"].clone()).expect("typed action");
        action.action_id = "known.action".into();
        action
    }
    fn policy(actions: Vec<String>) -> PolicyRoot {
        serde_json::from_value(json!({"accessVersion":"0.1","policyId":"test.access","rules":[{
            "ruleId":"grant","actions":actions,"resourceType":"test.Order",
            "scope":{"dimensions":["org"]},"allOf":[{"role":{"anyOf":["reviewer"]}}]
        }]})).expect("typed policy")
    }
    #[test]
    fn declared_policy_actions_project_their_grants() {
        let graph = project(&[action()], &[policy(vec!["known.action".into()])]).expect("complete graph");
        assert_eq!(graph.grants["known.action"], BTreeSet::from(["reviewer".into()]));
        assert_eq!(graph.edges.len(), 1);
    }
    #[test]
    fn undeclared_policy_action_makes_the_projection_unknown() {
        assert!(project(&[action()], &[policy(vec!["known.action".into(), "missing.action".into()])]).is_none());
    }
    #[test]
    fn duplicate_action_roots_make_the_projection_unknown() {
        let known = action();
        assert!(project(&[known.clone(), known], &[policy(vec!["known.action".into()])]).is_none());
    }
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn any_undeclared_policy_action_closes_the_projection(suffix in "[a-z]{1,12}") {
            proptest::prop_assert!(project(&[action()], &[policy(vec!["known.action".into(), format!("missing.{suffix}")])]).is_none());
        }
    }
}
