use norn_api::{
    mcp::{Backend, CallOutput},
    refusal::Refusal,
    mcp_host::Host,
    scope::CellScope,
};
use norn_ir::{McpTool, RuntimeObject, RuntimeValue};
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckInput {
    artifacts: String,
    context: String,
}
pub struct Checker;
impl Backend for Checker {
    fn call(
        &mut self,
        _: &str,
        arguments: RuntimeObject,
        _: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        let input: CheckInput =
            norn_ir::runtime::decode(&RuntimeValue::Object(arguments)).map_err(Refusal::invalid)?;
        let report = norn_checker_wasm::check_artifact_set_json(&input.artifacts, &input.context)
            .map_err(|e| Refusal::invalid(e.to_string()))?;
        let value = RuntimeValue::from_json_text(&report).map_err(Refusal::host)?;
        let admitted =
            value.as_object().and_then(|v| v.get("admitted")) == Some(&RuntimeValue::Bool(true));
        Ok(CallOutput {
            value,
            is_error: !admitted,
        })
    }
}
pub fn host(org_id: String, environment_id: String, origin: String) -> Result<Host<Checker>, Refusal> {
    let input_schema = serde_json::to_value(schemars::schema_for!(CheckInput)).map_err(Refusal::host)?;
    let tool: McpTool = serde_json::from_value(serde_json::json!({"name":"norn.check", "description":"Check artifact-set content through the shared checker; no storage access", "inputSchema":input_schema})).map_err(Refusal::host)?;
    Ok(Host::new(CellScope { org_id, environment_id }, origin, vec![tool], Checker))
}
