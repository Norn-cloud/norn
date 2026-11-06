//! Agent definition admission (decision D58, #772).
//!
//! An agent is a definition of `kind: "agent"` in the generic definitions
//! envelope. It is not an IR artifact kind: [`check_agent`] is a pure check
//! of the payload JSON text against an [`AgentContext`] (the model allowlist,
//! the tool bindings bound for the org, and the admitted laws), so the Studio
//! canvas (through the checker ABI) and the server (on admit and install) run
//! the same rules. Every finding carries an RFC 6901 pointer into the payload.
//!
//! The payload is
//! `{name, title, model, prompt, tools: [toolBindingId], budgets: {maxRuns,
//! maxToolCalls, maxCostUsd}, laws: [lawId], decisionPolicy: {approve:
//! "always" | "never" | "laws", laws?: [lawId], deciderRoles: [role]}}`.
//! Members are closed: an unknown member is a finding. Numbers are read from
//! their exact JSON token text and compared as exact decimals, never `f64`.

use std::collections::{BTreeMap, BTreeSet};

use norn_canonical::{CanonicalJsonError, JsonValue};
use norn_ir::function::Digest;
use norn_ir::{Decimal, ResourceLimitProfile};

use crate::property_types::error;
use crate::{Diagnostic, Span};

/// Stable `agent.*` codes.
pub mod codes {
    /// The payload does not decode: not JSON, a wrong or unknown member, a
    /// blank `name`, `title`, or `prompt`, a malformed `name`, or an
    /// oversized prompt or payload.
    pub const INVALID_PAYLOAD: &str = "agent.validation.invalid_payload";
    /// `model` is not in the model allowlist.
    pub const UNKNOWN_MODEL: &str = "agent.validation.unknown_model";
    /// A tool is not a binding bound for this org, or is listed twice.
    pub const UNBOUND_TOOL: &str = "agent.validation.unbound_tool";
    /// A law in `laws` or `decisionPolicy.laws` is not admitted.
    pub const UNKNOWN_LAW: &str = "agent.validation.unknown_law";
    /// A budget is missing, not an exact positive number, or above its cap.
    pub const BUDGET_UNBOUNDED: &str = "agent.validation.budget_unbounded";
    /// The decision policy cannot decide: `approve: "laws"` without laws, or
    /// no decider roles unless approval is `"never"`.
    pub const INVALID_DECISION_POLICY: &str = "agent.validation.invalid_decision_policy";
}

/// The largest prompt, in UTF-8 bytes (32 KiB). `norn.limits/1` has no prompt
/// cap; this bounds what a single definition may carry into every run.
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;

/// The largest `budgets.maxRuns`. `norn.limits/1` has no agent caps, so the
/// admission ceiling is named here; an org may only choose a lower budget.
pub const MAX_AGENT_RUNS: u64 = 10_000;

/// The largest `budgets.maxToolCalls` (see [`MAX_AGENT_RUNS`]).
pub const MAX_AGENT_TOOL_CALLS: u64 = 1_000;

/// The largest `budgets.maxCostUsd`, in whole US dollars (see
/// [`MAX_AGENT_RUNS`]).
pub const MAX_AGENT_COST_USD: u64 = 1_000;

/// A tool binding the host resolved to an installed connector and its
/// manifest, by digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBinding {
    /// The tool binding id an agent's `tools` names.
    pub id: String,
    /// The installed connector the binding resolves to.
    pub connector: String,
    /// The connector manifest's content digest, `sha256:<64 hex>`.
    pub manifest_digest: Digest,
}

/// What an agent is admitted against: host-supplied context only. Build it
/// with [`AgentContext::new`], which refuses a malformed binding, so a
/// context in hand is well formed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    models: BTreeSet<String>,
    bindings: BTreeMap<String, ToolBinding>,
    laws: BTreeSet<String>,
    limits: ResourceLimitProfile,
}

impl AgentContext {
    /// A context of the model allowlist `models`, the org's resolved tool
    /// `bindings`, the admitted `laws`, and the resource-limit profile
    /// `limits`. The profile's slice byte bound caps the payload text;
    /// `norn.limits/1` has no prompt, run, tool-call, or cost fields, so those
    /// caps are this module's named constants ([`MAX_PROMPT_BYTES`],
    /// [`MAX_AGENT_RUNS`], [`MAX_AGENT_TOOL_CALLS`], [`MAX_AGENT_COST_USD`]).
    ///
    /// # Errors
    ///
    /// A binding with a blank id or connector, a `manifestDigest` that is not
    /// `sha256:<64 lower-case hex>`, or an id bound twice.
    pub fn new(
        models: BTreeSet<String>,
        bindings: Vec<ToolBinding>,
        laws: BTreeSet<String>,
        limits: ResourceLimitProfile,
    ) -> Result<Self, String> {
        let mut by_id = BTreeMap::new();
        for binding in bindings {
            if binding.id.trim().is_empty() {
                return Err("a tool binding has a blank id".to_owned());
            }
            if binding.connector.trim().is_empty() {
                return Err(format!(
                    "tool binding {:?} has a blank connector",
                    binding.id
                ));
            }
            if !binding.manifest_digest.is_well_formed() {
                return Err(format!(
                    "tool binding {:?} has manifestDigest {:?}, not sha256:<64 lower-case hex>",
                    binding.id,
                    binding.manifest_digest.as_str()
                ));
            }
            let id = binding.id.clone();
            if by_id.insert(id, binding).is_some() {
                return Err("a tool binding id is bound twice".to_owned());
            }
        }
        Ok(Self {
            models,
            bindings: by_id,
            laws,
            limits,
        })
    }

    /// The resolved binding of tool `id`, when the org binds it.
    #[must_use]
    pub fn binding(&self, id: &str) -> Option<&ToolBinding> {
        self.bindings.get(id)
    }
}

/// When an agent's proposals are approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approve {
    /// Always approved.
    Always,
    /// Never approved.
    Never,
    /// Approved when the policy's laws hold.
    Laws,
}

/// The decoded `decisionPolicy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionPolicy {
    /// The approval mode.
    pub approve: Approve,
    /// The laws an `approve: "laws"` policy decides by.
    pub laws: Option<Vec<String>>,
    /// The roles that may decide.
    pub decider_roles: Vec<String>,
}

/// The decoded `budgets`. Each member is kept as its exact JSON value (absent
/// when missing) so admission, not the decoder, judges its bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBudgets {
    /// `maxRuns`.
    pub max_runs: Option<JsonValue>,
    /// `maxToolCalls`.
    pub max_tool_calls: Option<JsonValue>,
    /// `maxCostUsd`.
    pub max_cost_usd: Option<JsonValue>,
}

/// A decoded agent payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    /// The agent name, `[a-z][a-z0-9_.]*`.
    pub name: String,
    /// The display title.
    pub title: String,
    /// The model id.
    pub model: String,
    /// The system prompt.
    pub prompt: String,
    /// Tool binding ids, in order.
    pub tools: Vec<String>,
    /// Run, tool-call, and cost budgets.
    pub budgets: AgentBudgets,
    /// Law ids the agent is bound by.
    pub laws: Vec<String>,
    /// The decision policy.
    pub decision_policy: DecisionPolicy,
}

const AGENT_MEMBERS: &[&str] = &[
    "name",
    "title",
    "model",
    "prompt",
    "tools",
    "budgets",
    "laws",
    "decisionPolicy",
];
const BUDGET_MEMBERS: &[&str] = &["maxRuns", "maxToolCalls", "maxCostUsd"];
const POLICY_MEMBERS: &[&str] = &["approve", "laws", "deciderRoles"];

fn at(segments: &[&str]) -> Span {
    Span::from_segments(segments)
}

fn invalid(message: String, segments: &[&str]) -> Diagnostic {
    error(codes::INVALID_PAYLOAD, message, at(segments))
}

/// The members of `value` as an object; unknown members are findings.
fn object<'a>(
    value: &'a JsonValue,
    members: &[&str],
    path: &[&str],
    findings: &mut Vec<Diagnostic>,
) -> Option<&'a BTreeMap<String, JsonValue>> {
    let JsonValue::Object(map) = value else {
        findings.push(invalid("expected an object".to_owned(), path));
        return None;
    };
    for key in map.keys().filter(|key| !members.contains(&key.as_str())) {
        let mut segments = path.to_vec();
        segments.push(key);
        findings.push(invalid(format!("unknown member {key:?}"), &segments));
    }
    Some(map)
}

fn text(
    map: &BTreeMap<String, JsonValue>,
    key: &str,
    path: &[&str],
    findings: &mut Vec<Diagnostic>,
) -> Option<String> {
    let mut segments = path.to_vec();
    segments.push(key);
    match map.get(key) {
        Some(JsonValue::String(value)) => Some(value.clone()),
        Some(_) => {
            findings.push(invalid(format!("{key} must be a string"), &segments));
            None
        }
        None => {
            findings.push(invalid(format!("{key} is required"), &segments));
            None
        }
    }
}

fn texts(
    map: &BTreeMap<String, JsonValue>,
    key: &str,
    path: &[&str],
    findings: &mut Vec<Diagnostic>,
) -> Option<Vec<String>> {
    let mut segments = path.to_vec();
    segments.push(key);
    let Some(JsonValue::Array(items)) = map.get(key) else {
        let problem = if map.contains_key(key) {
            "must be an array of strings"
        } else {
            "is required"
        };
        findings.push(invalid(format!("{key} {problem}"), &segments));
        return None;
    };
    let mut values = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        if let JsonValue::String(value) = item {
            values.push(value.clone());
        } else {
            let index = index.to_string();
            let mut item_segments = segments.clone();
            item_segments.push(&index);
            findings.push(invalid(format!("{key} must hold strings"), &item_segments));
        }
    }
    (values.len() == items.len()).then_some(values)
}

fn nonblank(value: Option<&str>, key: &str, findings: &mut Vec<Diagnostic>) {
    if value.is_some_and(|value| value.trim().is_empty()) {
        findings.push(invalid(format!("{key} must not be blank"), &[key]));
    }
}

/// Whether `name` matches `[a-z][a-z0-9_.]*`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// The required member `key` of the payload.
fn required<'a>(
    map: &'a BTreeMap<String, JsonValue>,
    key: &str,
    findings: &mut Vec<Diagnostic>,
) -> Option<&'a JsonValue> {
    let value = map.get(key);
    if value.is_none() {
        findings.push(invalid(format!("{key} is required"), &[key]));
    }
    value
}

fn decode_policy(value: &JsonValue, findings: &mut Vec<Diagnostic>) -> Option<DecisionPolicy> {
    let path = ["decisionPolicy"];
    let map = object(value, POLICY_MEMBERS, &path, findings)?;
    let approve = match text(map, "approve", &path, findings).as_deref() {
        Some("always") => Some(Approve::Always),
        Some("never") => Some(Approve::Never),
        Some("laws") => Some(Approve::Laws),
        Some(other) => {
            findings.push(invalid(
                format!("approve is {other:?}, not \"always\", \"never\", or \"laws\""),
                &["decisionPolicy", "approve"],
            ));
            None
        }
        None => None,
    };
    let laws = if map.contains_key("laws") {
        Some(texts(map, "laws", &path, findings)?)
    } else {
        None
    };
    let decider_roles = texts(map, "deciderRoles", &path, findings)?;
    Some(DecisionPolicy {
        approve: approve?,
        laws,
        decider_roles,
    })
}

/// Whether `payload` is JSON text at all, for callers that treat text that is
/// not JSON as invalid input rather than a finding. Only UTF-8 and grammar
/// errors count; duplicate keys, nesting depth, and number range are
/// [`codes::INVALID_PAYLOAD`] or budget findings of [`check_agent`].
///
/// # Errors
///
/// The parser's message when the text is not JSON.
pub fn agent_payload_syntax(payload: &str) -> Result<(), String> {
    match norn_canonical::parse_json_verbatim(payload) {
        Ok(_)
        | Err(
            CanonicalJsonError::DuplicateKey { .. }
            | CanonicalJsonError::NumberTooLarge { .. }
            | CanonicalJsonError::NestingTooDeep { .. },
        ) => Ok(()),
        Err(
            error @ (CanonicalJsonError::InvalidUtf8 { .. }
            | CanonicalJsonError::InvalidJson { .. }
            | CanonicalJsonError::NonFiniteNumber { .. }
            | CanonicalJsonError::InvalidDecimal { .. }
            | CanonicalJsonError::LegacyParse { .. }
            | CanonicalJsonError::LegacySerialize { .. }
            | CanonicalJsonError::Serialize { .. }),
        ) => Err(error.to_string()),
    }
}

/// Decode an agent payload from its JSON text, at most
/// `limits.slice.max_canonical_bytes` long.
///
/// # Errors
///
/// Every [`codes::INVALID_PAYLOAD`] finding of the text.
pub fn decode_agent(
    payload: &str,
    limits: &ResourceLimitProfile,
) -> Result<AgentDefinition, Vec<Diagnostic>> {
    let max_bytes = usize::try_from(limits.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    if payload.len() > max_bytes {
        return Err(vec![invalid(
            format!(
                "the payload is {} bytes, more than {max_bytes}",
                payload.len()
            ),
            &[],
        )]);
    }
    let document = norn_canonical::parse_json_verbatim(payload)
        .map_err(|decode| vec![invalid(format!("the payload is not JSON: {decode}"), &[])])?;
    let mut findings = Vec::new();
    let Some(map) = object(&document, AGENT_MEMBERS, &[], &mut findings) else {
        return Err(findings);
    };
    let name = text(map, "name", &[], &mut findings);
    let title = text(map, "title", &[], &mut findings);
    let model = text(map, "model", &[], &mut findings);
    let prompt = text(map, "prompt", &[], &mut findings);
    nonblank(name.as_deref(), "name", &mut findings);
    nonblank(title.as_deref(), "title", &mut findings);
    nonblank(prompt.as_deref(), "prompt", &mut findings);
    if let Some(name) = name.as_ref().filter(|name| !name.trim().is_empty())
        && !valid_name(name)
    {
        findings.push(invalid(
            format!("name {name:?} does not match [a-z][a-z0-9_.]*"),
            &["name"],
        ));
    }
    if let Some(prompt) = &prompt
        && prompt.len() > MAX_PROMPT_BYTES
    {
        findings.push(invalid(
            format!(
                "the prompt is {} bytes, more than {MAX_PROMPT_BYTES}",
                prompt.len()
            ),
            &["prompt"],
        ));
    }
    let tools = texts(map, "tools", &[], &mut findings);
    let laws = texts(map, "laws", &[], &mut findings);
    let budgets = required(map, "budgets", &mut findings)
        .and_then(|value| object(value, BUDGET_MEMBERS, &["budgets"], &mut findings))
        .map(|budgets| AgentBudgets {
            max_runs: budgets.get("maxRuns").cloned(),
            max_tool_calls: budgets.get("maxToolCalls").cloned(),
            max_cost_usd: budgets.get("maxCostUsd").cloned(),
        });
    let decision_policy = required(map, "decisionPolicy", &mut findings)
        .and_then(|value| decode_policy(value, &mut findings));
    match (
        name,
        title,
        model,
        prompt,
        tools,
        budgets,
        laws,
        decision_policy,
    ) {
        (
            Some(name),
            Some(title),
            Some(model),
            Some(prompt),
            Some(tools),
            Some(budgets),
            Some(laws),
            Some(decision_policy),
        ) if findings.is_empty() => Ok(AgentDefinition {
            name,
            title,
            model,
            prompt,
            tools,
            budgets,
            laws,
            decision_policy,
        }),
        _ => Err(findings),
    }
}

fn unbounded(message: String, member: &str) -> Diagnostic {
    error(codes::BUDGET_UNBOUNDED, message, at(&["budgets", member]))
}

/// A count budget: an integer in `1..=cap`.
fn check_count(value: Option<&JsonValue>, member: &str, cap: u64, findings: &mut Vec<Diagnostic>) {
    let count = match value {
        None => {
            findings.push(unbounded(format!("{member} is missing"), member));
            return;
        }
        // Norn1 number text has no exponent or trailing zeros, so an integer
        // is plain digits; an overflow is above every cap.
        Some(JsonValue::Number(digits)) if digits.bytes().all(|b| b.is_ascii_digit()) => {
            digits.parse::<u64>().ok()
        }
        Some(_) => {
            findings.push(unbounded(
                format!("{member} must be an integer in 1..={cap}"),
                member,
            ));
            return;
        }
    };
    if !count.is_some_and(|count| (1..=cap).contains(&count)) {
        findings.push(unbounded(
            format!("{member} must be an integer in 1..={cap}"),
            member,
        ));
    }
}

/// Whether `text` is a plain decimal: digits with an optional fraction, no
/// sign or exponent.
fn plain_decimal(text: &str) -> bool {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, "0"));
    !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && fraction.bytes().all(|b| b.is_ascii_digit())
}

/// The cost budget: an exact decimal (number or plain-decimal string) in
/// `(0, MAX_AGENT_COST_USD]`.
fn check_cost(value: Option<&JsonValue>, findings: &mut Vec<Diagnostic>) {
    let member = "maxCostUsd";
    let cost = match value {
        None => {
            findings.push(unbounded(format!("{member} is missing"), member));
            return;
        }
        Some(JsonValue::Number(text)) => Decimal::parse(text).ok(),
        Some(JsonValue::String(text)) if plain_decimal(text) => Decimal::parse(text).ok(),
        Some(_) => None,
    };
    let Some(cost) = cost else {
        findings.push(unbounded(
            format!("{member} must be an exact decimal number or string"),
            member,
        ));
        return;
    };
    if cost <= Decimal::ZERO || cost > Decimal::from(MAX_AGENT_COST_USD) {
        findings.push(unbounded(
            format!(
                "{member} is {}, not greater than 0 and at most {MAX_AGENT_COST_USD}",
                cost.canonical()
            ),
            member,
        ));
    }
}

fn check_laws(laws: &[String], path: &[&str], context: &AgentContext, out: &mut Vec<Diagnostic>) {
    for (index, law) in laws.iter().enumerate() {
        if !context.laws.contains(law) {
            let index = index.to_string();
            let mut segments = path.to_vec();
            segments.push(&index);
            out.push(error(
                codes::UNKNOWN_LAW,
                format!("law {law:?} is not admitted"),
                at(&segments),
            ));
        }
    }
}

/// Admit the agent payload `payload` (JSON text) against `context`.
///
/// Decode findings ([`codes::INVALID_PAYLOAD`]) are returned alone; a decoded
/// agent is then checked for an allowlisted model, bound and distinct tools,
/// admitted laws, bounded budgets, and a decidable decision policy. No
/// finding means the agent is admitted.
#[must_use]
pub fn check_agent(payload: &str, context: &AgentContext) -> Vec<Diagnostic> {
    let agent = match decode_agent(payload, &context.limits) {
        Ok(agent) => agent,
        Err(findings) => return findings,
    };
    let mut findings = Vec::new();
    if !context.models.contains(&agent.model) {
        findings.push(error(
            codes::UNKNOWN_MODEL,
            format!("model {:?} is not in the model allowlist", agent.model),
            at(&["model"]),
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, tool) in agent.tools.iter().enumerate() {
        let message = match (seen.insert(tool), context.bindings.contains_key(tool)) {
            (false, _) => format!("tool {tool:?} is listed more than once"),
            (true, true) => continue,
            (true, false) => format!("tool {tool:?} is not a binding bound for this org"),
        };
        findings.push(error(
            codes::UNBOUND_TOOL,
            message,
            at(&["tools", &index.to_string()]),
        ));
    }
    check_laws(&agent.laws, &["laws"], context, &mut findings);
    let budgets = &agent.budgets;
    check_count(
        budgets.max_runs.as_ref(),
        "maxRuns",
        MAX_AGENT_RUNS,
        &mut findings,
    );
    check_count(
        budgets.max_tool_calls.as_ref(),
        "maxToolCalls",
        MAX_AGENT_TOOL_CALLS,
        &mut findings,
    );
    check_cost(budgets.max_cost_usd.as_ref(), &mut findings);
    let policy = &agent.decision_policy;
    let policy_laws = policy.laws.as_deref().unwrap_or_default();
    check_laws(
        policy_laws,
        &["decisionPolicy", "laws"],
        context,
        &mut findings,
    );
    if policy.approve == Approve::Laws && policy_laws.is_empty() {
        let segments: &[&str] = if policy.laws.is_some() {
            &["decisionPolicy", "laws"]
        } else {
            &["decisionPolicy"]
        };
        findings.push(error(
            codes::INVALID_DECISION_POLICY,
            "approve \"laws\" needs at least one law".to_owned(),
            at(segments),
        ));
    }
    if policy.approve != Approve::Never && policy.decider_roles.is_empty() {
        findings.push(error(
            codes::INVALID_DECISION_POLICY,
            "deciderRoles must name a role unless approve is \"never\"".to_owned(),
            at(&["decisionPolicy", "deciderRoles"]),
        ));
    }
    for (index, role) in policy.decider_roles.iter().enumerate() {
        if role.trim().is_empty() {
            findings.push(error(
                codes::INVALID_DECISION_POLICY,
                "a decider role must not be blank".to_owned(),
                at(&["decisionPolicy", "deciderRoles", &index.to_string()]),
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn context() -> AgentContext {
        let set = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        AgentContext::new(
            set(&["claude-opus-5-5"]),
            vec![binding("crm.lookup"), binding("mail.send")],
            set(&["finance.no_overspend", "privacy.no_pii"]),
            ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("well-formed context")
    }

    fn binding(id: &str) -> ToolBinding {
        ToolBinding {
            id: id.to_owned(),
            connector: "crm".to_owned(),
            manifest_digest: Digest::from_hex(&"a".repeat(64)),
        }
    }

    #[test]
    fn a_malformed_binding_is_refused() {
        let context = |bindings| {
            AgentContext::new(
                BTreeSet::new(),
                bindings,
                BTreeSet::new(),
                ResourceLimitProfile::norn_limits_v1(),
            )
        };
        assert!(context(vec![binding("crm.lookup")]).is_ok());
        let mut blank = binding("crm.lookup");
        blank.connector = " ".to_owned();
        assert!(context(vec![blank]).is_err());
        let mut tag = binding("crm.lookup");
        tag.manifest_digest = Digest("sha256:latest".to_owned());
        assert!(context(vec![tag]).is_err());
        assert!(context(vec![binding(""), binding("x")]).is_err());
        assert!(context(vec![binding("x"), binding("x")]).is_err());
    }

    fn agent() -> Value {
        json!({
            "name": "sales.followup",
            "title": "Sales follow-up",
            "model": "claude-opus-5-5",
            "prompt": "Follow up on open quotes.",
            "tools": ["crm.lookup", "mail.send"],
            "budgets": {"maxRuns": 100, "maxToolCalls": 20, "maxCostUsd": "12.50"},
            "laws": ["privacy.no_pii"],
            "decisionPolicy": {
                "approve": "laws",
                "laws": ["finance.no_overspend"],
                "deciderRoles": ["sales.manager"]
            }
        })
    }

    fn findings_of(payload: &str) -> Vec<(String, String)> {
        check_agent(payload, &context())
            .into_iter()
            .map(|finding| {
                assert!(
                    finding.registration().is_some(),
                    "{} unregistered",
                    finding.code
                );
                (
                    finding.code.as_str().to_owned(),
                    finding.span.map(|span| span.pointer).unwrap_or_default(),
                )
            })
            .collect()
    }

    fn with(edit: impl FnOnce(&mut Value)) -> Vec<(String, String)> {
        let mut payload = agent();
        edit(&mut payload);
        findings_of(&payload.to_string())
    }

    fn remove(value: &mut Value, key: &str) {
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
    }

    fn one(code: &str, pointer: &str) -> Vec<(String, String)> {
        vec![(code.to_owned(), pointer.to_owned())]
    }

    #[test]
    fn a_clean_agent_is_admitted() {
        assert_eq!(with(|_| {}), Vec::new());
        // `never` needs neither laws nor decider roles.
        assert_eq!(
            with(|agent| agent["decisionPolicy"] = json!({"approve": "never", "deciderRoles": []})),
            Vec::new()
        );
    }

    #[test]
    fn malformed_payloads_are_invalid() {
        let code = codes::INVALID_PAYLOAD;
        assert_eq!(findings_of("not json"), one(code, ""));
        assert_eq!(findings_of("[]"), one(code, ""));
        assert_eq!(findings_of(r#"{"name":"a","name":"b"}"#), one(code, ""));
        assert_eq!(with(|a| a["extra"] = json!(1)), one(code, "/extra"));
        assert_eq!(
            with(|a| a["budgets"]["maxDays"] = json!(1)),
            one(code, "/budgets/maxDays")
        );
        assert_eq!(
            with(|a| a["decisionPolicy"]["approve"] = json!("sometimes")),
            one(code, "/decisionPolicy/approve")
        );
        assert_eq!(with(|a| a["title"] = json!(" ")), one(code, "/title"));
        assert_eq!(with(|a| a["prompt"] = json!("")), one(code, "/prompt"));
        assert_eq!(with(|a| a["name"] = json!("")), one(code, "/name"));
        assert_eq!(with(|a| a["name"] = json!("Sales")), one(code, "/name"));
        assert_eq!(with(|a| a["name"] = json!("9lives")), one(code, "/name"));
        assert_eq!(with(|a| a["tools"] = json!([1])), one(code, "/tools/0"));
        assert_eq!(with(|a| remove(a, "model")), one(code, "/model"));
        let long = format!("{}p", "p".repeat(MAX_PROMPT_BYTES));
        assert_eq!(with(|a| a["prompt"] = json!(long)), one(code, "/prompt"));
        assert_eq!(
            with(|a| a["prompt"] = json!("p".repeat(MAX_PROMPT_BYTES))),
            Vec::new()
        );
    }

    #[test]
    fn only_text_that_is_not_json_is_a_syntax_error() {
        assert!(agent_payload_syntax("{").is_err());
        assert!(agent_payload_syntax("not json").is_err());
        let deep = format!("{}1{}", "[".repeat(300), "]".repeat(300));
        assert!(agent_payload_syntax(&deep).is_ok());
        assert_eq!(findings_of(&deep), one(codes::INVALID_PAYLOAD, ""));
        let deep_budget = agent().to_string().replace(
            "\"maxRuns\":100",
            &format!("\"maxRuns\":{}1{}", "[".repeat(200), "]".repeat(200)),
        );
        assert!(agent_payload_syntax(&deep_budget).is_ok());
        assert_eq!(
            findings_of(&deep_budget),
            one(codes::BUDGET_UNBOUNDED, "/budgets/maxRuns")
        );
    }

    #[test]
    fn the_model_must_be_allowlisted() {
        assert_eq!(
            with(|a| a["model"] = json!("gpt-4")),
            one(codes::UNKNOWN_MODEL, "/model")
        );
    }

    #[test]
    fn tools_must_be_bound_and_distinct() {
        assert_eq!(
            with(|a| a["tools"] = json!(["crm.lookup", "shell.exec", "crm.lookup"])),
            vec![
                (codes::UNBOUND_TOOL.to_owned(), "/tools/1".to_owned()),
                (codes::UNBOUND_TOOL.to_owned(), "/tools/2".to_owned()),
            ]
        );
    }

    #[test]
    fn laws_must_be_admitted() {
        assert_eq!(
            with(|a| {
                a["laws"] = json!(["privacy.no_pii", "made.up"]);
                a["decisionPolicy"]["laws"] = json!(["also.made_up"]);
            }),
            vec![
                (codes::UNKNOWN_LAW.to_owned(), "/laws/1".to_owned()),
                (
                    codes::UNKNOWN_LAW.to_owned(),
                    "/decisionPolicy/laws/0".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn budgets_must_be_exact_and_bounded() {
        let code = codes::BUDGET_UNBOUNDED;
        for runs in [json!(0), json!(-1), json!(1.5), json!("5"), json!(10_001)] {
            assert_eq!(
                with(|a| a["budgets"]["maxRuns"] = runs.clone()),
                one(code, "/budgets/maxRuns"),
                "{runs}"
            );
        }
        assert_eq!(
            findings_of(
                &agent()
                    .to_string()
                    .replace("\"maxRuns\":100", "\"maxRuns\":1e40")
            ),
            one(code, "/budgets/maxRuns")
        );
        assert_eq!(
            with(|a| a["budgets"]["maxToolCalls"] = json!(1_001)),
            one(code, "/budgets/maxToolCalls")
        );
        assert_eq!(
            with(|a| remove(&mut a["budgets"], "maxToolCalls")),
            one(code, "/budgets/maxToolCalls")
        );
        for cost in [
            json!("0"),
            json!(0),
            json!("-1"),
            json!("1e2"),
            json!(" 1"),
            json!("1000.0000000000000000000000001"),
            json!(true),
        ] {
            assert_eq!(
                with(|a| a["budgets"]["maxCostUsd"] = cost.clone()),
                one(code, "/budgets/maxCostUsd"),
                "{cost}"
            );
        }
        for cost in ["1000", "0.0000000000000000000000000001"] {
            assert_eq!(
                with(|a| a["budgets"]["maxCostUsd"] = json!(cost)),
                Vec::new(),
                "{cost}"
            );
        }
        // A number token is read exactly, never through f64.
        let exact = agent()
            .to_string()
            .replace("\"12.50\"", "999.99999999999999999999999");
        assert_eq!(findings_of(&exact), Vec::new());
        let over = agent()
            .to_string()
            .replace("\"12.50\"", "1000.00000000000000000000001");
        assert_eq!(findings_of(&over), one(code, "/budgets/maxCostUsd"));
    }

    #[test]
    fn the_decision_policy_must_decide() {
        let code = codes::INVALID_DECISION_POLICY;
        assert_eq!(
            with(|a| a["decisionPolicy"]["laws"] = json!([])),
            one(code, "/decisionPolicy/laws")
        );
        assert_eq!(
            with(|a| remove(&mut a["decisionPolicy"], "laws")),
            one(code, "/decisionPolicy")
        );
        assert_eq!(
            with(|a| a["decisionPolicy"] = json!({"approve": "always", "deciderRoles": []})),
            one(code, "/decisionPolicy/deciderRoles")
        );
        assert_eq!(
            with(|a| a["decisionPolicy"]["deciderRoles"] = json!([""])),
            one(code, "/decisionPolicy/deciderRoles/0")
        );
    }
}
