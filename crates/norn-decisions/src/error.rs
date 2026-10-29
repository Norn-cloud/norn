//! Norn-owned admission and evaluation errors. zen errors are mapped here and
//! never exposed.

use thiserror::Error;

/// Why a `RuleIR` v2 rule (and its sub-decision closure) cannot be loaded as
/// an [`AdmittedDecision`](crate::AdmittedDecision).
///
/// `norn-checker` owns admission and reports these as stable diagnostics
/// first; the evaluator re-checks what it relies on and refuses to load
/// anything else (fail closed).
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AdmissionError {
    /// The rule is not a `RuleIR` v2 rule with a JDM graph.
    #[error("rule `{rule_id}` is not a RuleIR v2 decision graph (ruleVersion `{rule_version}`)")]
    NotDecisionGraph {
        /// The rule.
        rule_id: String,
        /// Its declared `ruleVersion`.
        rule_version: String,
    },
    /// The rule declares a contract other than the one its position needs: the
    /// root is a `decide` graph, every other graph a `sub_decision`.
    #[error("rule `{rule_id}` must implement the `{expected}` contract")]
    WrongContract {
        /// The rule.
        rule_id: String,
        /// The contract its position in the closure requires.
        expected: &'static str,
    },
    /// A node type outside the Norn profile (`functionNode`, `customNode`).
    #[error("rule `{rule_id}` node `{node_id}`: `{node_type}` is not admitted by the Norn profile")]
    NodeNotAdmitted {
        /// The rule.
        rule_id: String,
        /// The node.
        node_id: String,
        /// Its JDM `type`.
        node_type: &'static str,
    },
    /// An input or output node carries a JDM `schema`. Decision input and
    /// output are validated against the rule's own closed schemas, exactly;
    /// a second, node-local schema is refused rather than ignored.
    #[error(
        "rule `{rule_id}` node `{node_id}`: node-level JDM schemas are not supported; declare inputSchema/outputSchema on the rule"
    )]
    NodeSchema {
        /// The rule.
        rule_id: String,
        /// The input or output node.
        node_id: String,
    },
    /// The graph imports a policy document; the Norn profile has none.
    #[error("rule `{rule_id}`: graph imports are not admitted by the Norn profile")]
    Import {
        /// The rule.
        rule_id: String,
    },
    /// A `decisionNode` key with no graph in the supplied closure.
    #[error("rule `{rule_id}`: sub-decision `{key}` is not in the admitted closure")]
    UnresolvedSubDecision {
        /// The calling rule.
        rule_id: String,
        /// The missing key.
        key: String,
    },
    /// A supplied sub-decision no graph of the closure calls: the closure is
    /// exact, so its identity does not depend on unused graphs.
    #[error("sub-decision `{key}` is not reachable from the root decision")]
    UnusedSubDecision {
        /// The unreachable key.
        key: String,
    },
    /// The closure is deeper or larger than the Norn profile admits (a cycle
    /// is reported as too deep).
    #[error(
        "the sub-decision closure exceeds the Norn profile bound (reached depth {depth} and {size} graphs)"
    )]
    SubDecisionBound {
        /// Deepest nesting reached.
        depth: usize,
        /// Distinct sub-decision graphs reached.
        size: usize,
    },
    /// A rule schema is not the closed dialect or not the decision contract.
    #[error("rule `{rule_id}` {path}: {reason}")]
    InvalidSchema {
        /// The rule.
        rule_id: String,
        /// Path of the offending schema member.
        path: String,
        /// What is wrong.
        reason: String,
    },
    /// An output default has no exact Norn value or violates the output schema.
    #[error("rule `{rule_id}` default `{member}`: {reason}")]
    InvalidDefault {
        /// The rule.
        rule_id: String,
        /// The `defaults` member.
        member: String,
        /// What is wrong.
        reason: String,
    },
    /// An executable JDM source violates the fail-closed syntax bounds used
    /// by the evaluator, including bounded-string builtin literal controls.
    #[error("rule `{rule_id}` {path}: expression admission failed ({code}): {message}")]
    InvalidExpression {
        /// The rule containing the expression.
        rule_id: String,
        /// Path to the JDM expression source.
        path: String,
        /// Stable diagnostic code from `norn-expr`.
        code: String,
        /// Human-readable detail from `norn-expr`.
        message: String,
    },
}

/// Where a strict evaluation error was raised (the vendored engine's strict errors).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StrictSite {
    /// A decision-table input (predicate) cell failed or was not a boolean.
    DecisionTableInput,
    /// An output cell of a matching decision-table row failed.
    DecisionTableOutput,
    /// A switch condition failed or was not a boolean.
    SwitchCondition,
}

impl StrictSite {
    /// Stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DecisionTableInput => "decision_table_input",
            Self::DecisionTableOutput => "decision_table_output",
            Self::SwitchCondition => "switch_condition",
        }
    }
}

impl std::fmt::Display for StrictSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an evaluation produced no [`Evaluated`](crate::Evaluated) output.
///
/// Every variant means **zero effects**: no fact, command, or timer of a
/// failed evaluation is returned. Variants and their fields are deterministic
/// for a given decision and input; engine diagnostic `message`s are not a
/// stable contract.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DecisionError {
    /// A [`ValidatedSlice`](crate::ValidatedSlice) or
    /// [`TypedEvent`](crate::TypedEvent) validated for another decision, or
    /// for another version of it (a different input schema).
    #[error("input was validated for decision `{validated_for}`, not `{decision}`")]
    InputBinding {
        /// The evaluated decision's rule id.
        decision: String,
        /// The rule id the input was validated for.
        validated_for: String,
    },
    /// The slice or event does not conform to the decide input schema.
    #[error("{member} violates the decision input schema at `{path}`: {reason}")]
    InputSchema {
        /// `slice` or `event`.
        member: &'static str,
        /// Path inside the value.
        path: String,
        /// What is wrong.
        reason: String,
    },
    /// A number has no exact engine representation (never rounded).
    #[error("number `{input}` is not an exact Norn decimal: {reason}")]
    InvalidNumber {
        /// The rejected text.
        input: String,
        /// Why.
        reason: String,
    },
    /// A decision-table cell or switch condition failed (strict errors): the
    /// evaluation aborts instead of falling through to another row or branch.
    #[error("strict {site} error at node `{node_id}` ({id}: `{expression}`): {message}")]
    Strict {
        /// The site.
        site: StrictSite,
        /// The node of the root graph the error surfaced at (a
        /// `decisionNode` when it came from a sub-decision).
        node_id: String,
        /// The column (table) or statement (switch) id.
        id: String,
        /// The failing expression source.
        expression: String,
        /// Engine diagnostic (not a stable contract).
        message: String,
    },
    /// The evaluation needed more operation units than its budget.
    #[error("decision evaluation budget exhausted: {used} units needed, limit {limit}")]
    BudgetExhausted {
        /// The budget's unit limit.
        limit: u64,
        /// Units the failing charge would have reached (`used > limit`).
        used: u64,
    },
    /// Sub-decision nesting exceeded the engine depth limit.
    #[error("sub-decision depth limit exceeded")]
    DepthLimitExceeded,
    /// The engine suspended. Every admitted node is synchronous, so this is
    /// an engine defect; it is reported, never retried or spun on.
    #[error("the decision engine suspended; evaluation must complete in one poll")]
    UnexpectedPending,
    /// Any other engine failure (for example an expression type error).
    #[error("decision evaluation failed at node `{node_id}`: {message}")]
    Engine {
        /// The node of the root graph, or empty when the graph itself failed.
        node_id: String,
        /// Engine diagnostic (not a stable contract).
        message: String,
    },
    /// The engine produced a value with no Norn value representation (for
    /// example a date).
    #[error("the decision produced a `{type_name}` value, which has no Norn value representation")]
    UnsupportedValue {
        /// Engine type name.
        type_name: String,
    },
    /// An exact fact violates the host's effective stored-property contract.
    #[error("decision fact violates the effective property contract at `{path}`: {reason}")]
    FactContract {
        /// Path of the fact inside the output.
        path: String,
        /// Why the effective contract refused it.
        reason: String,
    },
    /// The decision's output does not conform to its declared output schema.
    #[error("decision output violates the output schema at `{path}`: {reason}")]
    OutputSchema {
        /// Path inside the output.
        path: String,
        /// What is wrong.
        reason: String,
    },
}
