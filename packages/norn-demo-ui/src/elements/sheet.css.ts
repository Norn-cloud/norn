// The sheet's styles: semantic classes over the host's design tokens (Studio's `--card`,
// `--border`, `--primary`, ...), with neutral fallbacks so the element renders anywhere it embeds.
// Light and dark follow the host's tokens; no remote resources.

export const SHEET_CSS = `
.norn-sheet { display: grid; gap: 1.25rem; padding: 1.5rem; color: var(--foreground, #111); font: 14px/1.5 var(--font-sans, system-ui, sans-serif); }
.norn-sheet-header { display: grid; gap: .375rem; }
.norn-title { margin: 0; font-size: 1rem; font-weight: 600; }
.norn-sheet-header .norn-title { padding-inline-end: 2rem; }
.norn-meta { margin: 0; font-size: .8125rem; color: var(--muted-foreground, #666); }
.norn-legend { font-size: .875rem; font-weight: 500; margin-bottom: .5rem; }
.norn-chip { justify-self: start; display: inline-flex; align-items: center; border-radius: .375rem; padding: .0625rem .375rem; font-size: .75rem;
  background: color-mix(in oklab, var(--norn-tone) 14%, transparent); color: color-mix(in oklab, var(--norn-tone) 80%, var(--foreground, #111));
  box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--norn-tone) 22%, transparent); }
.norn-chip[data-tone="risk-low"] { --norn-tone: var(--tint-route, #2f7d5b); }
.norn-chip[data-tone="risk-medium"] { --norn-tone: var(--tint-wait, #b7791f); }
.norn-chip[data-tone="risk-high"] { --norn-tone: var(--tint-law, #c53030); }
.norn-chip[data-tone="agent"] { --norn-tone: var(--tint-agent, #6b46c1); margin-inline-start: .375rem; }
.norn-fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: .5rem; }
.norn-fieldset ui-radio-group { display: grid; gap: .5rem; }
.norn-option { display: grid; grid-template-columns: auto 1fr; gap: .625rem; align-items: start; cursor: pointer; border-radius: var(--radius-node, .75rem);
  border: 1px solid var(--node-border, var(--border, #e5e5e5)); background: var(--card, #fff); padding: .75rem;
  box-shadow: var(--node-highlight, 0 0 0 0 transparent), var(--elevation-node, 0 1px 2px rgb(0 0 0 / .06)); transition: border-color .15s, box-shadow .15s; }
.norn-option:has([data-checked]) { border-color: var(--primary, #2563eb); box-shadow: 0 0 0 3px color-mix(in oklab, var(--primary, #2563eb) 25%, transparent); }
.norn-option:hover { background: color-mix(in oklab, var(--muted, #f4f4f5) 40%, var(--card, #fff)); }
.norn-option ui-radio { margin-top: .1875rem; display: inline-grid; place-items: center; width: 1rem; height: 1rem; border-radius: 999px;
  border: 1px solid var(--border, #d4d4d8); background: var(--background, #fff); }
.norn-option ui-radio:focus-visible { outline: 2px solid var(--ring, #2563eb); outline-offset: 2px; }
.norn-option ui-radio-indicator { width: .5rem; height: .5rem; border-radius: 999px; background: var(--primary, #2563eb); }
.norn-option-text { display: grid; gap: .125rem; }
.norn-option-label { font-weight: 500; }
.norn-field { display: grid; gap: .375rem; }
.norn-field textarea { width: 100%; box-sizing: border-box; min-height: 5rem; resize: vertical; border-radius: .5rem; padding: .5rem .625rem; font: inherit;
  color: inherit; background: var(--background, #fff); border: 1px solid var(--input, var(--border, #d4d4d8)); }
.norn-field textarea:focus-visible { outline: 2px solid var(--ring, #2563eb); outline-offset: 1px; }
.norn-status, .norn-confirm, .norn-outcome { border-radius: .5rem; padding: .5rem .75rem; font-size: .875rem; }
.norn-confirm[data-tone="neutral"] { border: 1px solid var(--node-border, var(--border, #e5e5e5)); background: color-mix(in oklab, var(--muted, #f4f4f5) 60%, transparent); color: inherit; }
.norn-confirm[data-tone="neutral"] .norn-confirm-title { color: var(--foreground, #111); }
.norn-option[data-refused] { opacity: .75; cursor: not-allowed; border-style: dashed; }
.norn-option[data-refused] .norn-option-label { text-decoration: line-through; text-decoration-color: color-mix(in oklab, var(--foreground, #111) 40%, transparent); }
.norn-hash { display: flex; align-items: center; gap: .5rem; }
.norn-hash .norn-button { height: 1.75rem; padding: 0 .5rem; font-size: .8125rem; }
.norn-value { white-space: normal; }
.norn-status[data-tone="danger"], .norn-confirm:not([data-tone="neutral"]) { border: 1px solid color-mix(in oklab, var(--destructive, #dc2626) 30%, transparent);
  background: color-mix(in oklab, var(--destructive, #dc2626) 6%, transparent); color: var(--destructive, #b91c1c); }
.norn-status[data-tone="info"] { color: var(--muted-foreground, #666); }
.norn-confirm p { margin: 0; }
.norn-confirm-title { font-weight: 500; }
.norn-outcome { border: 1px solid var(--node-border, var(--border, #e5e5e5)); background: var(--card, #fff); font-weight: 500;
  box-shadow: var(--elevation-node, 0 1px 2px rgb(0 0 0 / .06)); }
.norn-actions { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem; }
.norn-footer { position: sticky; bottom: 0; margin: 0 -1.5rem -1.5rem; padding: .75rem 1.5rem; border-top: 1px solid var(--node-border, var(--border, #e5e5e5));
  background: color-mix(in oklab, var(--card, #fff) 95%, transparent); backdrop-filter: blur(4px); }
.norn-spacer { flex: 1; }
.norn-button { display: inline-flex; align-items: center; height: 2.25rem; padding: 0 .875rem; border-radius: .5rem; font: inherit; font-weight: 500; cursor: pointer;
  border: 1px solid transparent; background: transparent; color: inherit; }
.norn-button[hidden] { display: none; }
.norn-button:focus-visible { outline: 2px solid var(--ring, #2563eb); outline-offset: 2px; }
.norn-button:disabled { opacity: .5; cursor: not-allowed; }
.norn-button[data-variant="solid"] { background: var(--primary, #2563eb); color: var(--primary-foreground, #fff); }
.norn-button[data-variant="danger"] { background: var(--destructive, #dc2626); color: #fff; }
.norn-button[data-variant="outline"] { border-color: var(--node-border, var(--border, #d4d4d8)); }
.norn-button[data-variant="ghost"]:hover:not(:disabled), .norn-button[data-variant="outline"]:hover:not(:disabled) { background: var(--muted, #f4f4f5); }
.norn-dialog { display: grid; gap: .75rem; max-width: 28rem; padding: 1.25rem; border-radius: var(--radius-node, .75rem); background: var(--card, #fff);
  color: var(--foreground, #111); border: 1px solid var(--node-border, var(--border, #e5e5e5)); box-shadow: var(--elevation-float, 0 12px 32px rgb(0 0 0 / .18)); }
ui-alert-dialog-backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / .35); }
.norn-form { display: grid; gap: 1.25rem; }
.norn-link { font-size: .875rem; color: inherit; text-underline-offset: 4px; }
.norn-link:hover, .norn-outcome-card .norn-link { text-decoration: underline; }
.norn-note { font-size: .8125rem; color: var(--muted-foreground, #555); border-inline-start: 2px solid var(--node-border, var(--border, #d4d4d8)); padding-inline-start: .5rem; }
.norn-problem { margin: 0; font-size: .8125rem; color: var(--destructive, #b91c1c); }
.norn-field textarea[aria-invalid="true"] { border-color: var(--destructive, #dc2626); }
[data-region="countdown"][data-urgent] { color: var(--destructive, #b91c1c); font-weight: 500; }
[data-region="countdown"] { font-variant-numeric: tabular-nums; }
.norn-outcome-card { display: grid; gap: .75rem; }
.norn-outcome-card .norn-outcome { margin: 0; }
.norn-details summary { cursor: pointer; }
.norn-dl { display: grid; grid-template-columns: minmax(7rem, auto) 1fr; gap: .375rem 1rem; margin: .75rem 0 0; font-size: .875rem; }
.norn-dl dt { color: var(--muted-foreground, #666); }
.norn-dl dd { margin: 0; min-width: 0; overflow-wrap: anywhere; }
.norn-sheet [hidden] { display: none !important; }
`

/** Add the sheet's styles to a document once. */
export function adoptSheetStyles(document: Document): void {
  if (document.getElementById("norn-sheet-styles")) return
  const style = document.createElement("style")
  style.id = "norn-sheet-styles"
  style.textContent = SHEET_CSS
  document.head.append(style)
}
