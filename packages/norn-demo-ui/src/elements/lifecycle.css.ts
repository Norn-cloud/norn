// The lifecycle view's styles: semantic classes over the host's design tokens, with neutral
// fallbacks. Soft depth: cards on the page's surface, a light shadow, arrows in the border tone.

export const LIFECYCLE_CSS = `
.norn-life { display: grid; gap: 1rem; color: var(--foreground, #111); font: 14px/1.5 var(--font-sans, system-ui, sans-serif); }
.norn-life-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: .5rem 1rem; }
.norn-life-title { margin: 0; font-size: .9375rem; font-weight: 600; }
.norn-life-meta { margin: 0; font-size: .8125rem; color: var(--muted-foreground, #666); }
.norn-life-board { position: relative; display: grid; grid-auto-flow: column; grid-auto-columns: minmax(9.5rem, 1fr); gap: 1rem 2.75rem; align-items: start;
  padding: 1rem; border-radius: var(--radius-node, .75rem); background: color-mix(in oklab, var(--muted, #f4f4f5) 45%, transparent); overflow-x: auto; }
.norn-life-col { display: grid; gap: .75rem; align-content: start; position: relative; z-index: 1; list-style: none; margin: 0; padding: 0; }
.norn-life-state { display: grid; gap: .125rem; padding: .625rem .75rem; border-radius: var(--radius-node, .75rem); background: var(--card, #fff);
  border: 1px solid var(--node-border, var(--border, #e5e5e5)); box-shadow: var(--elevation-node, 0 1px 2px rgb(0 0 0 / .06)); transition: border-color .15s, box-shadow .15s; }
.norn-life-state[data-ending] { border-style: double; border-width: 3px; }
.norn-life-state[data-current] { border-color: var(--primary, #2563eb); box-shadow: 0 0 0 3px color-mix(in oklab, var(--primary, #2563eb) 25%, transparent); }
.norn-life-name { font-weight: 500; }
.norn-life-count { font-variant-numeric: tabular-nums; font-size: 1.25rem; font-weight: 600; line-height: 1.2; }
.norn-life-count[data-pending] { color: var(--muted-foreground, #666); font-weight: 400; }
.norn-life-role { font-size: .75rem; color: var(--muted-foreground, #666); }
.norn-life-arrows { position: absolute; inset: 0; pointer-events: none; z-index: 0; overflow: visible; }
.norn-life-arrows path { fill: none; stroke: color-mix(in oklab, var(--foreground, #111) 28%, transparent); stroke-width: 1.25; }
.norn-life-arrows path[data-active] { stroke: var(--primary, #2563eb); stroke-width: 2.25; }
.norn-life-arrows text { font-size: 11px; fill: var(--muted-foreground, #666); paint-order: stroke; stroke: var(--background, #fff); stroke-width: 3px; }
.norn-life-arrows text[data-active] { fill: var(--primary, #2563eb); font-weight: 600; }
.norn-life-foot { margin: 0; font-size: .8125rem; color: var(--muted-foreground, #666); }
.norn-life-refused { margin: 0; font-size: .875rem; border-radius: .5rem; padding: .5rem .75rem; background: color-mix(in oklab, var(--muted, #f4f4f5) 60%, transparent); }
.norn-life-findings { display: grid; gap: .5rem; }
.norn-life-findings h3 { margin: 0; font-size: .875rem; font-weight: 600; }
.norn-life-finding { display: grid; gap: .5rem; padding: .625rem .75rem; border-radius: .625rem; border: 1px solid var(--node-border, var(--border, #e5e5e5)); background: var(--card, #fff); }
.norn-life-finding[aria-current] { border-color: var(--primary, #2563eb); }
.norn-life-finding button { font: inherit; text-align: start; border: 0; padding: 0; background: none; color: inherit; cursor: pointer; }
.norn-life-finding button:focus-visible, .norn-life-step-controls button:focus-visible { outline: 2px solid var(--ring, #2563eb); outline-offset: 2px; border-radius: .25rem; }
.norn-life-steps { margin: 0; padding-inline-start: 1.25rem; display: grid; gap: .25rem; font-size: .8125rem; }
.norn-life-steps li[aria-current] { font-weight: 600; color: var(--primary, #2563eb); }
.norn-life-side { display: inline-block; min-width: 3.25rem; font-size: .6875rem; text-transform: uppercase; letter-spacing: .04em; color: var(--muted-foreground, #666); }
.norn-life-step-controls { display: flex; gap: .5rem; align-items: center; }
.norn-life-step-controls button { font: inherit; font-size: .8125rem; border-radius: .5rem; padding: .25rem .625rem; border: 1px solid var(--border, #d4d4d8); background: var(--background, #fff); color: inherit; cursor: pointer; }
.norn-life-step-controls button:disabled { opacity: .5; cursor: not-allowed; }
`

/** Add the view's styles to `document` once. */
export function adoptLifecycleStyles(document: Document): void {
  if (document.getElementById("norn-lifecycle-styles")) return
  const style = document.createElement("style")
  style.id = "norn-lifecycle-styles"
  style.textContent = LIFECYCLE_CSS
  document.head.append(style)
}
