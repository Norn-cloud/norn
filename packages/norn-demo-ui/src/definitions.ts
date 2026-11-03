import type { Schemas } from "../../norn-sdk/src/index.ts"
import { escape, renderShell, UNLABELLED_READ } from "./shell.ts"

type Artifact = Schemas["NornArtifactEnvelope"]
type Metadata = Pick<Artifact, "artifactId" | "kind" | "name" | "version">

/** Metadata and exact JSON text from an already-authorized definition read. */
export interface DefinitionEntry {
  readonly metadata: Metadata
  readonly source: string
}

/**
 * A read-only inspector. Authorization and admission happen before calling it.
 * Sources remain opaque text: numeric literals and definitions are never rewritten.
 */
export function renderDefinitionsDocument({ entries, title = "Definitions" }: {
  readonly entries: readonly DefinitionEntry[]
  readonly title?: string
}): string {
  const cards = entries.map(({ metadata, source }, index) => `
<article class="card" aria-labelledby="definition-${index}">
  <details>
    <summary id="definition-${index}">
      <h3>${escape(metadata.name)}</h3>
      <span class="sub"><code>${escape(metadata.artifactId)}</code> · ${escape(metadata.kind)} · ${escape(metadata.version)}</span>
    </summary>
    <pre tabindex="0" aria-label="Definition source"><code>${escape(source)}</code></pre>
  </details>
</article>`).join("")
  const count = `<p class="lede">${entries.length} ${entries.length === 1 ? "definition" : "definitions"}</p>`
  return renderShell({
    title,
    eyebrow: "What built this application",
    intro: "Entities, relationships, actions, lifecycle and rules, as returned by the server. Open a definition to inspect its source.",
    observation: UNLABELLED_READ,
    sections: [],
    body: count + (entries.length ? cards : '<p class="empty">This document contains no definitions.</p>'),
  })
}
