//! Wire document types for `GET /v1/decisions` (DECISIONS-1), extracted from
//! `decisions.rs` and re-exported there with identical paths.

use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// How much is at stake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRisk {
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
}

/// What choosing an option does: exactly one of an action or a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOptionEffect {
    /// The action the option starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The binding the option sets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
}

/// One option a decider may choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionOptionDocument {
    /// The option id `choice` names.
    pub id: String,
    /// What the decider reads.
    pub label: String,
    /// How far it reaches, in the decider's words.
    pub scope: String,
    /// What it does.
    pub effect: DecisionOptionEffect,
}

/// The run a decision belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRun {
    /// The run id.
    pub id: String,
    /// Its generation.
    pub generation: u64,
    /// Its lifecycle state.
    pub state: Option<String>,
}

/// The flow a decision belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionFlow {
    /// The flow's `decide` rule artifact id.
    pub rule_id: String,
    /// The rule version the run is pinned to.
    pub version: String,
    /// The flow's display title.
    pub title: Option<String>,
}

/// The step that asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionStep {
    /// The step id.
    pub id: String,
    /// Its display label.
    pub label: Option<String>,
}

/// What the decision is about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContext {
    /// The objects the decision concerns.
    pub subjects: Vec<DecisionSubject>,
}

/// One object a decision concerns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSubject {
    /// Its object type.
    pub object_type: String,
    /// Its id.
    pub id: String,
}

/// One open decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionDocument {
    /// The opaque decision id.
    pub id: String,
    /// The run waiting on it.
    pub run: DecisionRun,
    /// The run's flow.
    pub flow: DecisionFlow,
    /// The step that asks.
    pub step: DecisionStep,
    /// The question.
    pub question: String,
    /// How much is at stake.
    pub risk: DecisionRisk,
    /// The options.
    pub options: Vec<DecisionOptionDocument>,
    /// An agent's recommended option id; a recommendation, never a decision.
    pub agent_pick: Option<String>,
    /// When the run asked (logical milliseconds).
    pub requested_at_ms: u64,
    /// When the decision expires (logical milliseconds), at most seven days
    /// after it was asked.
    pub expires_at_ms: u64,
    /// What it is about.
    pub context: DecisionContext,
    /// Reserved; always `null` in this version.
    #[schema(value_type = Option<std::collections::BTreeMap<String, serde_json::Value>>)]
    pub input_schema: Option<serde_json::Value>,
    /// For the approval of a parked action call (APPROVAL-1) or an automation
    /// proposal (AUTOMATION-1), the action call it proposes; absent for a flow's ask.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<DecisionCall>,
}

/// The parked action call an approval or automation proposal decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCall {
    /// The action called.
    pub action: String,
    /// The approval or automation definition that created it.
    pub definition: String,
    /// The call's admitted input.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub input: RuntimeValue,
    /// Optional provenance trace context carried by the parked call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub trace: Option<crate::actions::TraceContext>,
}

/// One page of open decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionsPage {
    /// The decisions, soonest deadline first, ties to the oldest request.
    pub items: Vec<DecisionDocument>,
    /// How many open decisions the caller may decide.
    pub total: u64,
    /// `total` is a lower bound: more were not examined.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub total_capped: bool,
    /// Pass as `cursor` for the next page; `null` on the last page.
    pub next_cursor: Option<String>,
    /// The cell's clock when the page was read (logical milliseconds).
    pub server_now_ms: u64,
}

/// `GET /v1/decisions` query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct DecisionsQuery {
    /// Organization (required).
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment (required).
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// `pending` (required): only open decisions are listed.
    pub state: Option<String>,
    /// Only decisions of runs of this flow (its definition id).
    pub flow: Option<String>,
    /// The previous page's `nextCursor`.
    pub cursor: Option<String>,
    /// Page size, 0 to 200 (default 50); 0 returns only the count.
    pub limit: Option<u32>,
}
