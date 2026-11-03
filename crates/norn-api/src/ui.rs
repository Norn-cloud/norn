//! SURFACE-UI-1: render an authorized server document; every submission uses the governed action route.

use std::fmt::Write as _;

use crate::{
    refusal::Refusal,
    routes::{DefinitionsQuery, DefsResponse},
};

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn fields(schema: &serde_json::Value) -> Option<String> {
    let mut fields = String::new();
    for (name, property) in schema.get("properties")?.as_object()? {
        let kind = if property.pointer("/nornType/kind").and_then(serde_json::Value::as_str) == Some("decimal") {
            "decimal"
        } else {
            property.get("type")?.as_str()?
        };
        let input_type = match kind {
            "string" | "decimal" => "text",
            "integer" | "number" => "number",
            "boolean" => "checkbox",
            _ => return None,
        };
        let required = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|names| names.iter().any(|value| value.as_str() == Some(name)));
        write!(&mut fields,
            "<label>{}<input data-field-name=\"{}\" data-json-kind=\"{kind}\" type=\"{input_type}\" step=\"any\" {}></label>",
            escape(&name.replace('_', " ")), escape(name),
            if required && kind != "boolean" { "required" } else { "" }).ok()?;
    }
    Some(fields)
}

pub(crate) fn render(document: &DefsResponse, scope: &DefinitionsQuery) -> Result<String, Refusal> {
    let context = serde_json::to_string(&serde_json::json!({
        "org": scope.org_id, "environment": scope.environment_id, "version": document.version,
    }))
    .map_err(Refusal::host)?
    .replace('<', "\\u003c");
    let mut forms = String::new();
    for artifact in &document.artifacts {
        let artifact: serde_json::Value =
            serde_json::from_str(artifact.get()).map_err(Refusal::host)?;
        if artifact.get("kind").and_then(serde_json::Value::as_str) != Some("action") {
            continue;
        }
        let payload = artifact
            .get("payload")
            .ok_or_else(|| Refusal::host("missing action payload"))?;
        let id = payload
            .get("actionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Refusal::host("missing action identity"))?;
        let title = artifact
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(id);
        let controls = payload
            .get("inputSchema")
            .and_then(fields)
            .unwrap_or_else(|| {
                "<label>Action input<textarea required spellcheck=\"false\">{}</textarea></label>"
                    .into()
            });
        write!(
            &mut forms,
            "<form data-action-id=\"{}\"><h2>{}</h2><p><code>{}</code></p>{controls}<button type=\"submit\">{}</button><pre role=\"status\" aria-live=\"polite\"></pre></form>",
            escape(id), escape(title), escape(id), escape(title),
        ).map_err(Refusal::host)?;
    }
    Ok(format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Norn · Governed actions</title>
<style>body{{margin:0;background:#f4f1e8;color:#18332d;font:18px Georgia,serif}}main{{max-width:860px;margin:64px auto;padding:0 24px}}header{{border-top:6px solid #18332d;padding-top:24px;margin-bottom:40px}}h1{{font-size:44px;font-weight:400}}h2{{font-weight:400}}form{{border-top:1px solid #8d9b8e;padding:24px 0}}code,input,textarea,pre{{font:14px monospace}}label{{display:block;margin:16px 0}}input{{display:block;width:100%;box-sizing:border-box;padding:12px;margin-top:6px;background:#fffdf6;border:1px solid #8d9b8e;font:18px Georgia,serif}}input[type=checkbox]{{width:auto}}textarea{{display:block;box-sizing:border-box;width:100%;min-height:150px;padding:16px;margin:12px 0;background:#fffdf6;border:1px solid #8d9b8e;color:inherit}}button{{padding:12px 20px;background:#18332d;color:#fffdf6;border:0;font:inherit;cursor:pointer}}button:disabled{{opacity:.6}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;line-height:1.6}}:focus-visible{{outline:3px solid #c66935;outline-offset:3px}}</style>
<main><header><p>NORN / WORKSPACE</p><h1>Governed actions</h1><p>Submit an action from the admitted definitions. The server returns its receipt or explains the refusal.</p></header>{forms}</main>
<script type="application/json" id="context">{context}</script><script>
const context = JSON.parse(document.getElementById('context').textContent);
for (const form of document.querySelectorAll('form')) {{
  const button = form.querySelector('button'), output = form.querySelector('pre');
  form.addEventListener('input', () => {{ delete form.dataset.key; }});
  form.addEventListener('submit', async event => {{
    event.preventDefault(); button.disabled = true;
    try {{
      const editor = form.querySelector('textarea');
      const fields = [...form.querySelectorAll('[data-field-name]')].filter(field => field.required || field.value !== '');
      const input = editor ? editor.value : '{{' + fields.map(field => {{
        const value = field.dataset.jsonKind === 'boolean' ? String(field.checked)
          : ['number','integer'].includes(field.dataset.jsonKind) ? field.value : JSON.stringify(field.value);
        return JSON.stringify(field.dataset.fieldName) + ':' + value;
      }}).join(',') + '}}';
      const key = form.dataset.key ||= crypto.randomUUID();
      const response = await fetch('/v1/actions', {{method:'POST', headers:{{'content-type':'application/json','x-norn-org-id':context.org,'x-norn-environment-id':context.environment}},
        body:'{{"actionId":'+JSON.stringify(form.dataset.actionId)+',"version":'+context.version+',"idempotencyKey":'+JSON.stringify(key)+',"input":'+input+'}}'}});
      output.textContent = await response.text();
    }} catch (error) {{ output.textContent = 'The action response is unavailable. Retry the same input to recover its receipt.'; }}
    finally {{ button.disabled = false; }}
  }});
}}
</script></html>"#
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn server_labels_cannot_create_markup() {
        assert_eq!(
            super::escape("</textarea><script>\"&'"),
            "&lt;/textarea&gt;&lt;script&gt;&quot;&amp;&#39;"
        );
    }
}
