//! SURFACE-UI-1: render an authorized server document; every submission uses the governed action route.

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
        if artifact["kind"] != "action" {
            continue;
        }
        let id = artifact["payload"]["actionId"]
            .as_str()
            .ok_or_else(|| Refusal::host("missing action identity"))?;
        let title = artifact["name"].as_str().unwrap_or(id);
        forms.push_str(&format!(
            "<form data-action-id=\"{}\"><h2>{}</h2><p><code>{}</code></p><label>Action input<textarea required spellcheck=\"false\">{{}}</textarea></label><button type=\"submit\">{}</button><pre role=\"status\" aria-live=\"polite\"></pre></form>",
            escape(id), escape(title), escape(id), escape(title),
        ));
    }
    Ok(format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Norn · Governed actions</title>
<style>body{{margin:0;background:#f4f1e8;color:#18332d;font:18px Georgia,serif}}main{{max-width:860px;margin:64px auto;padding:0 24px}}header{{border-top:6px solid #18332d;padding-top:24px;margin-bottom:40px}}h1{{font-size:44px;font-weight:400}}h2{{font-weight:400}}form{{border-top:1px solid #8d9b8e;padding:24px 0}}code,textarea,pre{{font:14px monospace}}label{{display:block}}textarea{{display:block;box-sizing:border-box;width:100%;min-height:150px;padding:16px;margin:12px 0;background:#fffdf6;border:1px solid #8d9b8e;color:inherit}}button{{padding:12px 20px;background:#18332d;color:#fffdf6;border:0;font:inherit;cursor:pointer}}button:disabled{{opacity:.6}}pre{{white-space:pre-wrap;overflow-wrap:anywhere;line-height:1.6}}:focus-visible{{outline:3px solid #c66935;outline-offset:3px}}</style>
<main><header><p>NORN / WORKSPACE</p><h1>Governed actions</h1><p>Submit an action from the admitted definitions. The server returns its receipt or explains the refusal.</p></header>{forms}</main>
<script type="application/json" id="context">{context}</script><script>
const context = JSON.parse(document.getElementById('context').textContent);
for (const form of document.querySelectorAll('form')) {{
  const input = form.querySelector('textarea'), button = form.querySelector('button'), output = form.querySelector('pre');
  input.addEventListener('input', () => {{ delete form.dataset.key; }});
  form.addEventListener('submit', async event => {{
    event.preventDefault(); button.disabled = true;
    try {{
      const key = form.dataset.key ||= crypto.randomUUID();
      const response = await fetch('/v1/actions', {{method:'POST', headers:{{'content-type':'application/json','x-norn-org-id':context.org,'x-norn-environment-id':context.environment}},
        body:'{{"actionId":'+JSON.stringify(form.dataset.actionId)+',"version":'+context.version+',"idempotencyKey":'+JSON.stringify(key)+',"input":'+input.value+'}}'}});
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
