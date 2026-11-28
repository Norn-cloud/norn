//! Indexed `norn.reveal` rule lookup for marking migration preflight checks.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::CellError;

/// The normalized `norn.reveal` rules of every `AccessIR` policy in one
/// artifact set, indexed once by marking id (without their rule ids, so a
/// renamed rule is the same rule). Each serialized rule string is stored once
/// and referenced by integer index to avoid quadratic cloning across `anyOf`
/// marking lists.
pub(crate) struct RevealRuleIndex {
    /// Interned normalized serialized rule strings.
    rules: Vec<String>,
    /// Lookup from normalized serialized rule string to its interned index.
    rule_ids: BTreeMap<String, usize>,
    /// Indices of rules with no `Marking` condition; they apply to every declared marking.
    unscoped: BTreeSet<usize>,
    /// Indices of rules constrained by one or more `Marking` conditions, keyed by marking id.
    by_marking: BTreeMap<String, BTreeSet<usize>>,
}

impl RevealRuleIndex {
    pub(crate) fn from_artifacts(
        artifacts: &[norn_ir::ArtifactEnvelope],
    ) -> Result<Self, CellError> {
        let mut rules = Vec::new();
        let mut rule_ids = BTreeMap::<String, usize>::new();
        let mut unscoped = BTreeSet::new();
        let mut by_marking = BTreeMap::<String, BTreeSet<usize>>::new();
        for artifact in artifacts {
            let Ok(decoded) = norn_ir::access::decode_policy_reveal_rules(artifact) else {
                continue;
            };
            for mut rule in decoded {
                let mut scoped_markings: Option<BTreeSet<String>> = None;
                for condition in &rule.all_of {
                    match condition {
                        norn_ir::access::AccessCondition::Marking(marking_condition) => {
                            let ids: BTreeSet<String> = marking_condition
                                .any_of
                                .iter()
                                .map(|id| id.as_str().to_owned())
                                .collect();
                            scoped_markings = Some(match scoped_markings {
                                None => ids,
                                Some(existing) => existing.intersection(&ids).cloned().collect(),
                            });
                        }
                        norn_ir::access::AccessCondition::Role(_)
                        | norn_ir::access::AccessCondition::Rel(_)
                        | norn_ir::access::AccessCondition::Context(_)
                        | norn_ir::access::AccessCondition::Resource(_)
                        | norn_ir::access::AccessCondition::Entitlement(_) => {}
                    }
                }
                if matches!(&scoped_markings, Some(markings) if markings.is_empty()) {
                    continue;
                }
                rule.rule_id.clear();
                let serialized = serde_json::to_string(&rule).map_err(CellError::Json)?;
                let rule_id = match rule_ids.entry(serialized) {
                    std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let next_id = rules.len();
                        rules.push(entry.key().clone());
                        entry.insert(next_id);
                        next_id
                    }
                };
                match scoped_markings {
                    None => {
                        unscoped.insert(rule_id);
                    }
                    Some(markings) => {
                        for marking in markings {
                            by_marking.entry(marking).or_default().insert(rule_id);
                        }
                    }
                }
            }
        }
        Ok(Self {
            rules,
            rule_ids,
            unscoped,
            by_marking,
        })
    }

    fn contains(&self, marking: &str, rule: &str) -> bool {
        self.rule_ids.get(rule).is_some_and(|rule_id| {
            self.unscoped.contains(rule_id)
                || self
                    .by_marking
                    .get(marking)
                    .is_some_and(|ids| ids.contains(rule_id))
        })
    }

    /// The lexicographically smallest rule in `self` applicable to `marking`
    /// that is not present in `before` for `marking`.
    pub(crate) fn first_widened<'a>(&'a self, before: &Self, marking: &str) -> Option<&'a str> {
        self.unscoped
            .iter()
            .chain(self.by_marking.get(marking).into_iter().flatten())
            .filter_map(|&rule_id| self.rules.get(rule_id).map(String::as_str))
            .filter(|rule| !before.contains(marking, rule))
            .min()
    }
}
