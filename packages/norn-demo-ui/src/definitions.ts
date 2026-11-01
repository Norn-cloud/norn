import type { Schemas } from "../../norn-sdk/src/index.ts"

type Artifact = Schemas["NornArtifactEnvelope"]
type Metadata = Pick<Artifact, "artifactId" | "kind" | "name" | "version">

/** Metadata and exact JSON text from an already-authorized definition read. */
export interface DefinitionEntry {
  readonly metadata: Metadata
  readonly source: string
}

const escape = (value: string): string => value.replace(/[&<>"']/g, (character) => {
  switch (character) {
    case "&": return "&amp;"
    case "<": return "&lt;"
    case ">": return "&gt;"
    case '"': return "&quot;"
    case "'": return "&#39;"
    default: return character
  }
})

const styles = `
:root { color-scheme: light; font-family: "Segoe UI", sans-serif; color: #202c29; background: #f4f5f1; }
* { box-sizing: border-box; }
body { margin: 0; }
a { color: inherit; text-underline-offset: .2em; }
a:focus-visible, summary:focus-visible { outline: 3px solid #23664c; outline-offset: 5px; }
.skip { position: absolute; left: 1rem; top: -5rem; }
.skip:focus { top: 1rem; background: white; padding: 1rem; }
header { border-bottom: 1px solid #d5dcd4; padding: 1.25rem max(1.5rem, calc((100vw - 1120px)/2)); background: #fff; }
.brand { font-weight: 650; letter-spacing: .06em; }
main { max-width: 1120px; margin: auto; padding: 3rem 1.5rem 5rem; }
h1 { font-family: Georgia, serif; font-weight: 400; font-size: clamp(2rem, 5vw, 3rem); margin: .4rem 0 1rem; }
.eyebrow, .meta { color: #52635b; font-size: .875rem; }
.eyebrow { text-transform: uppercase; letter-spacing: .12em; }
.intro { max-width: 65ch; line-height: 1.65; margin-bottom: 2.5rem; }
.count { font-variant-numeric: tabular-nums; margin-bottom: 1rem; }
article { background: #fff; border: 1px solid #d5dcd4; border-radius: 4px; margin-bottom: .75rem; }
summary { cursor: pointer; padding: 1.25rem; line-height: 1.6; overflow-wrap: anywhere; }
.name { font-weight: 600; }
.meta { display: block; margin-left: 1rem; }
code, pre { font-family: "Cascadia Code", "SFMono-Regular", Consolas, monospace; font-size: .85rem; }
pre { margin: 0; padding: 1.25rem; border-top: 1px solid #e0e5de; background: #fafbf8; overflow: auto; max-height: 65vh; line-height: 1.6; tab-size: 2; }
.empty { padding: 2rem; border: 1px dashed #a4b1a7; line-height: 1.6; }
@media (max-width: 600px) { main { padding-top: 2rem; } summary { padding: 1rem; } }
@media print { body { background: white; } pre { max-height: none; white-space: pre-wrap; overflow-wrap: anywhere; } }
`

/**
 * A read-only inspector. Authorization and admission happen before calling it.
 * Sources remain opaque text: numeric literals and definitions are never rewritten.
 */
export function renderDefinitionsDocument({ entries, title = "Definitions" }: {
  readonly entries: readonly DefinitionEntry[]
  readonly title?: string
}): string {
  const cards = entries.map(({ metadata, source }, index) => `
<article aria-labelledby="definition-${index}">
  <details>
    <summary id="definition-${index}">
      <span class="name">${escape(metadata.name)}</span>
      <span class="meta"><code>${escape(metadata.artifactId)}</code> · ${escape(metadata.kind)} · ${escape(metadata.version)}</span>
    </summary>
    <pre tabindex="0" aria-label="Definition source"><code>${escape(source)}</code></pre>
  </details>
</article>`).join("")
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'">
  <title>${escape(title)} · Norn</title>
  <style>${styles}</style>
</head>
<body>
  <a class="skip" href="#content">Skip to definitions</a>
  <header><span class="brand">NORN</span></header>
  <main id="content">
    <div class="eyebrow">What built this application</div>
    <h1>${escape(title)}</h1>
    <p class="intro">Entities, relationships, actions, lifecycle and rules, as returned by the server. Open a definition to inspect its source.</p>
    <p class="count">${entries.length} ${entries.length === 1 ? "definition" : "definitions"}</p>
    ${entries.length ? cards : '<p class="empty">This document contains no definitions.</p>'}
  </main>
</body>
</html>\n`
}
