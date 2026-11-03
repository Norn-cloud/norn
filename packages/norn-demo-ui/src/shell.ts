/** Escape server text for HTML text and attribute positions. */
export const escape = (value: string): string => value.replace(/[&<>"'\r]/g, (character) => {
  switch (character) {
    case "\r": return "&#13;"
    case "&": return "&amp;"
    case "<": return "&lt;"
    case ">": return "&gt;"
    case '"': return "&quot;"
    case "'": return "&#39;"
    default: return character
  }
})

const styles = `
:root {
  color-scheme: light dark;
  --bg: #f5f5f7; --surface: #ffffff; --raised: #fbfbfd; --text: #1d1d1f; --muted: #6e6e73;
  --line: #d2d2d7; --hairline: #e8e8ed; --accent: #0066cc; --focus: #0071e3;
  --thing: #0066cc; --thing-bg: #e8f1fb; --happening: #b4530b; --happening-bg: #fdf0e6;
  --neutral-bg: #f0f0f3; --warn: #b4530b; --ok: #1d7d3a; --ok-bg: #e7f5ec;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, "Noto Sans", "Noto Sans Arabic", sans-serif;
  font-size: 16px; line-height: 1.5; color: var(--text); background: var(--bg);
  -webkit-font-smoothing: antialiased;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #000000; --surface: #1c1c1e; --raised: #242426; --text: #f5f5f7; --muted: #a1a1a6;
    --line: #3a3a3c; --hairline: #2c2c2e; --accent: #2997ff; --focus: #2997ff;
    --thing: #64b5ff; --thing-bg: #10263d; --happening: #ffa95e; --happening-bg: #3a2414;
    --neutral-bg: #2c2c2e; --warn: #ffa95e; --ok: #5fd38a; --ok-bg: #12301d;
  }
}
* { box-sizing: border-box; }
body { margin: 0; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; text-underline-offset: .2em; }
:focus-visible { outline: 3px solid var(--focus); outline-offset: 3px; border-radius: 4px; }
.skip { position: absolute; left: 1rem; top: -5rem; }
.skip:focus { top: 1rem; background: var(--surface); padding: .75rem 1rem; z-index: 2; }
.bar { position: sticky; top: 0; z-index: 1; display: flex; gap: 2rem; align-items: center;
  padding: 0 max(1.25rem, calc((100vw - 1120px) / 2)); min-height: 52px;
  background: color-mix(in srgb, var(--bg) 82%, transparent); backdrop-filter: saturate(180%) blur(20px);
  -webkit-backdrop-filter: saturate(180%) blur(20px); border-bottom: 1px solid var(--hairline); }
.brand { font-weight: 600; letter-spacing: -.01em; font-size: 1.05rem; }
.bar nav { display: flex; gap: 1.5rem; overflow-x: auto; scrollbar-width: none; font-size: .875rem; }
.bar nav a { color: var(--muted); white-space: nowrap; padding: .9rem 0; }
.bar nav a:hover { color: var(--text); text-decoration: none; }
main { max-width: 1120px; margin: 0 auto; padding: 3.5rem 1.25rem 6rem; }
.eyebrow { color: var(--muted); font-size: .875rem; font-weight: 600; margin: 0; }
h1 { font-size: clamp(2.25rem, 6vw, 3.5rem); line-height: 1.07; letter-spacing: -.025em; font-weight: 700; margin: .35rem 0 1rem; overflow-wrap: anywhere; }
h2 { font-size: clamp(1.5rem, 3.5vw, 2rem); letter-spacing: -.02em; font-weight: 650; margin: 4rem 0 .5rem; scroll-margin-top: 72px; }
h3 { font-size: 1.125rem; letter-spacing: -.01em; font-weight: 600; margin: 0; overflow-wrap: anywhere; }
h4 { font-size: .75rem; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); font-weight: 600; margin: 1.5rem 0 .5rem; }
.intro { max-width: 62ch; color: var(--muted); font-size: 1.1875rem; line-height: 1.55; margin: 0 0 2rem; }
.observed { display: inline-flex; gap: .5rem; align-items: center; color: var(--muted); font-size: .8125rem; margin: -1rem 0 2rem; }
.observed::before { content: ""; width: 8px; height: 8px; border-radius: 50%; background: var(--line); }
.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
.lede { color: var(--muted); max-width: 70ch; margin: 0 0 1.5rem; }
.stats { display: flex; flex-wrap: wrap; gap: .75rem; margin: 0; padding: 0; list-style: none; }
.stats li { background: var(--surface); border: 1px solid var(--hairline); border-radius: 14px; padding: .9rem 1.25rem; min-width: 9rem; }
.stats b { display: block; font-size: 1.75rem; letter-spacing: -.02em; font-variant-numeric: tabular-nums; }
.stats span { color: var(--muted); font-size: .875rem; }
.card { background: var(--surface); border: 1px solid var(--hairline); border-radius: 18px; padding: 1.5rem; margin-bottom: 1rem; scroll-margin-top: 72px; }
.card:target { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent); }
.card > header { display: flex; flex-wrap: wrap; gap: .5rem 1rem; align-items: baseline; justify-content: space-between; }
.sub { color: var(--muted); font-size: .875rem; overflow-wrap: anywhere; }
.desc { margin: .75rem 0 0; max-width: 75ch; }
.chips { display: flex; flex-wrap: wrap; gap: .375rem; }
.chips + .scroll { margin-top: .75rem; }
.chip { display: inline-block; font-size: .75rem; font-weight: 600; border-radius: 999px; padding: .15rem .6rem; background: var(--neutral-bg); color: var(--muted); }
.chip.thing { background: var(--thing-bg); color: var(--thing); }
.chip.happening { background: var(--happening-bg); color: var(--happening); }
.chip.error { background: var(--happening-bg); color: var(--warn); }
table { width: 100%; border-collapse: collapse; font-size: .9375rem; }
th { text-align: start; font-weight: 500; color: var(--muted); font-size: .8125rem; padding: .4rem .75rem .4rem 0; border-bottom: 1px solid var(--hairline); }
td { padding: .55rem .75rem .55rem 0; border-bottom: 1px solid var(--hairline); vertical-align: top; overflow-wrap: anywhere; }
tr:last-child td { border-bottom: 0; }
.scroll { overflow-x: auto; }
code, pre { font-family: ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, monospace; font-size: .8125rem; }
.unit { color: var(--accent); font-weight: 600; }
dl { display: grid; grid-template-columns: minmax(7rem, max-content) 1fr; gap: .5rem 1.25rem; margin: 1rem 0 0; }
dt { color: var(--muted); font-size: .875rem; }
dd { margin: 0; overflow-wrap: anywhere; }
.wording { display: grid; grid-template-columns: 1fr 1fr; gap: .75rem; margin-top: 1rem; }
.wording > div { background: var(--raised); border: 1px solid var(--hairline); border-radius: 12px; padding: .9rem 1rem; }
.absent { color: var(--muted); font-size: .875rem; font-style: italic; margin: .25rem 0 0; }
.empty { padding: 2rem; border: 1px dashed var(--line); border-radius: 18px; color: var(--muted); }
.diagram { margin: 0; background: var(--surface); border: 1px solid var(--hairline); border-radius: 18px; padding: 1rem; overflow-x: auto; }
.diagram svg { display: block; margin: 0 auto; max-width: none; }
.diagram .node rect { fill: var(--neutral-bg); stroke: var(--line); }
.diagram .node.thing rect { fill: var(--thing-bg); stroke: var(--thing); }
.diagram .node.happening rect { fill: var(--happening-bg); stroke: var(--happening); }
.diagram .node.external rect { fill: none; stroke: var(--line); stroke-dasharray: 4 3; }
.diagram .node text { fill: var(--text); font-size: 12px; font-weight: 600; }
.diagram .node text.ns { fill: var(--muted); font-weight: 400; font-size: 10px; }
.diagram .edge { fill: none; stroke: var(--line); stroke-width: 1.25; }
.diagram .edge.implements { stroke-dasharray: 5 4; }
.diagram a:focus-visible rect { stroke: var(--focus); stroke-width: 3; }
.legend { display: flex; flex-wrap: wrap; gap: 1.25rem; color: var(--muted); font-size: .8125rem; margin: .75rem 0 0; padding: 0; list-style: none; }
.legend i { display: inline-block; width: 12px; height: 12px; border-radius: 3px; margin-inline-end: .4rem; vertical-align: -1px; border: 1px solid var(--line); background: var(--neutral-bg); }
.legend i.thing { background: var(--thing-bg); border-color: var(--thing); }
.legend i.happening { background: var(--happening-bg); border-color: var(--happening); }
.legend i.external { background: none; border-style: dashed; }
.legend i.line { height: 0; width: 20px; border: 0; border-top: 1.5px solid var(--muted); border-radius: 0; vertical-align: middle; background: none; }
.legend i.line.dashed { border-top-style: dashed; }
.states { display: flex; gap: .5rem; list-style: none; padding: 0; margin: 0 0 1.5rem; font-size: .875rem; font-weight: 600; }
.states li { padding: .35rem .9rem; border-radius: 999px; background: var(--neutral-bg); color: var(--muted); }
.states li.done::after { content: " ✓"; }
.states li.refused { background: var(--happening-bg); color: var(--warn); }
.assurance { border-inline-start: 4px solid var(--accent); }
.chip.freshness.exact, .chip.freshness.fresh { background: var(--ok-bg); color: var(--ok); }
.chip.freshness.stale, .chip.freshness.incomplete { background: var(--happening-bg); color: var(--warn); }
.facts { display: flex; flex-wrap: wrap; gap: .5rem 2rem; list-style: none; margin: 1.25rem 0 .25rem; padding: 0; }
.facts span { display: block; color: var(--muted); font-size: .75rem; }
.facts b { font-weight: 600; font-variant-numeric: tabular-nums; }
.callout { margin: 1rem 0 0; padding: .9rem 1.1rem; border-radius: 12px; background: var(--happening-bg); color: var(--text); }
.callout h4 { margin: 0 0 .25rem; color: var(--warn); }
.callout p { margin: 0 0 .5rem; }
.plain { list-style: none; margin: 0; padding: 0; }
th.num, td.num { text-align: end; font-variant-numeric: tabular-nums; }
.answer td.value a { font-weight: 650; }
.drill { display: none; margin: .75rem 0; padding: 1rem 1.1rem; border: 1px solid var(--accent); border-radius: 12px; scroll-margin-top: 72px; }
.drill:target { display: block; }
h5 { font-size: 1rem; font-weight: 600; margin: 0 0 .5rem; }
.exact { font-variant-numeric: tabular-nums; font-weight: 600; }
details > summary { cursor: pointer; }
pre { margin: 0; padding: 1.25rem; background: var(--raised); overflow: auto; max-height: 65vh; line-height: 1.6; tab-size: 2; }
@media (max-width: 640px) {
  main { padding-top: 2rem; }
  .card { padding: 1.125rem; border-radius: 14px; }
  .wording { grid-template-columns: 1fr; }
  dl { grid-template-columns: 1fr; gap: .125rem; }
  dd { margin-bottom: .5rem; }
  .bar { gap: 1rem; }
}
@media print { .bar { position: static; } body { background: white; } pre { max-height: none; white-space: pre-wrap; overflow-wrap: anywhere; } }
`

/** A section reachable from the shell's in-page navigation. */
export interface ShellSection {
  readonly id: string
  readonly label: string
}

/**
 * The shared product frame: no script, no remote resources, light and dark
 * themes from the platform preference. Callers pass already-escaped markup.
 * Pages render observed server documents; `observation` states what the read
 * says about its own frontier and freshness, and never more.
 */
export function renderShell({ title, eyebrow, intro, observation, sections, body }: {
  readonly title: string
  readonly eyebrow: string
  readonly intro: string
  readonly observation: string
  readonly sections: readonly ShellSection[]
  readonly body: string
}): string {
  const nav = sections.map(({ id, label }) => `<a href="#${escape(id)}">${escape(label)}</a>`).join("")
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'">
  <title>${escape(title)} · Norn</title>
  <style>${styles}</style>
</head>
<body>
  <a class="skip" href="#content">Skip to content</a>
  <div class="bar"><span class="brand">Norn</span>${nav ? `<nav aria-label="Sections">${nav}</nav>` : ""}</div>
  <main id="content">
    <p class="eyebrow">${escape(eyebrow)}</p>
    <h1>${escape(title)}</h1>
    <p class="intro">${escape(intro)}</p>
    <p class="observed">${escape(observation)}</p>
    ${body}
  </main>
</body>
</html>\n`
}

/** The definitions read carries no frontier or freshness label yet. */
export const UNLABELLED_READ = "Observed from the server. This read carries no frontier or freshness label."
