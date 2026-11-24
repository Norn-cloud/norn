# Norn Studio Design & UI Architecture Contract

This document is the binding design and UI engineering contract for first-party code in `apps/studio/` and compiled tenant applications rendered through `@norn/ui`. Every new screen, primitive, skin, or visual change must conform to these rules.

---

## 1. Product Aesthetic — Open Canvas First

Norn Studio is an open operational canvas, not a CRUD admin panel. Content sits directly on the surface; hierarchy comes from typography, spacing, and quiet tonal separation rather than stacked boxes.

### Primary visual references

| Reference | Licence / Notes | What we take |
| --- | --- | --- |
| `getopen.so` | Reference aesthetic | Pure open canvas, borderless floating streams, generous whitespace, single floating dark pill dock, minimal corner chrome. |
| `polar.sh` | Reference aesthetic | Deep carbon layered dark surfaces (never inverted light mode), crisp weight/tracking contrast, restrained accents. |
| **The Weave** | Visual language | Source-colored convergence threads and soft semantic washes that trace provenance without loud fills. |
| **Floorplan** | MIT | Multi-level semantic zoom, plain-English job cards, dashed **Blueprint** overlays for uncommitted proposals. |
| **Archify** | Apache-2.0 | `SRC` span evidence badges and `Before / Delta / After` architectural diffs. |
| **`bilalgursen/liquid-glass`** | MIT | Single WebGL2 refraction shader + reduced-motion CSS fallback layered purely over `@ui-sdk` behaviors. |

### Zero box-in-box rule

- Lists, tables, streams, dossiers, and spatial maps live **directly on the open canvas**. Never wrap lists inside bordered cards inside bordered panels.
- **Surfaces and borders are reserved exclusively for:**
  1. Popovers, menus, and tooltips.
  2. Modals, sheets, and inspection drawers.
  3. Active hover or keyboard/pointer selection states.
  4. A single floating dock (`ControlDock`).
  5. Dashed **Blueprint** outlines marking uncommitted draft proposals on the Semantic Map (`#1664`).

### Banned patterns

Reject and replace any of the following in code review:

- Alphabetical database-table sidebars dumping raw schema names.
- More than **3 identical bordered white cards** on a single screen.
- Heavy `700` bold admin box headers or uppercase-chrome section banners.
- Duplicate metadata, summary, or history boxes repeating facts already shown in the primary stream or dossier.
- Raw internal jargon (`commit 412`, `Observed link claim`, raw IR enum keys) in operator-facing headers. Translate guards and labels into plain language (`src/lib/guard-words.ts`); keep raw commits and claim payloads in expandable evidence/code drawers.
- Retool / Django-admin CRUD tables as the default layout when an open stream, spatial map, or living dossier fits the task.

---

## 2. Typography, Tokens & Swappable Skin / Shell Architecture

### Typography & layout invariants

- **Fonts:** Self-hosted **Geist Variable** (`@fontsource-variable/geist` → `--font-sans`) and **Geist Mono Variable** (`@fontsource-variable/geist-mono` → `--font-mono`).
- **Hierarchy without borders:** Medium (`500`) weights, size steps (`--text-meta`, `--text-panel`, `--text-section`, `--text-title`, `--text-figure`), and tight heading tracking (`letter-spacing: -0.02em` on `h1`–`h3`) carry structure.
- **Numerals:** `font-variant-numeric: tabular-nums` on body and data readouts so figures align vertically without jitter.
- **Root scale:** `html { font-size: 90%; }` in `src/index.css`. Never apply CSS `zoom`, which breaks `@xyflow/react` pointer coordinates.
- **Bidirectional prose (`LABEL-LOCALE-1`):** Mixed-direction text under `[dir="rtl"]` resolves per block via `unicode-bidi: plaintext`.
- **Contrast & focus:** Every theme and skin must maintain `>= 4.5:1` WCAG AA contrast for text (resting, hover, selected) and `>= 3:1` for graph edges and dedicated `focus-visible:outline-2` focus indicators (`apps/studio/test/contrast.test.ts`). Respect `prefers-reduced-motion`.

### Styling engine

- **Tailwind v4 + native CSS Custom Properties (`data-skin="..."`)** wired via `@theme inline` in `apps/studio/src/index.css`, with `@layer ui-sdk;` declared first so base SDK styles cascade cleanly underneath Tailwind utilities.
- **No CSS-in-JS and no Panda CSS.** Keep styling statically analyzable and zero-runtime.

### Define once, skin anywhere

Themes contain **zero business logic**—no conditional queries, permission checks, or layout forks. Every visual theme is a pure CSS token layer driven by `data-skin` (alongside `.dark` color-scheme tokens and `[data-density="compact"]` for dense builder views):

- **Strict first-party monochrome default (`polar`):** Large structural regions (canvas, sidebar, islands, panels, tables, chrome) stay strictly monochrome black, grey, and white (Polar / Linear craft). Colour is reserved exclusively for visualizations/charts/graph paths, status dots (`live`/`held`/`refused`), semantic badges/chips, small primary CTAs/focus rings, and the Norn knot (`--knot`).
- **VS Code / T3 Code-style `StudioTheme` JSON customization (`src/lib/studio-theme.ts`):** Operators and tenants may supply or edit declarative JSON themes (`version: 1`, `baseSkin`, `accent`, `canvas`, `colors`, `variants.light` / `variants.dark`) overriding allowlisted semantic CSS tokens on `:root`. Every custom theme is sanitized against CSS injection and gated by automated WCAG AA contrast checks (`>= 4.5:1` text/controls, `>= 3:1` focus rings) before applying.

| Built-in `data-skin` | Visual character |
| --- | --- |
| `polar` | **First-party default.** Strict Polar / Linear monochrome light and deep-carbon dark surfaces, crisp flat hairlines, colour reserved for attention and data. |
| `open` | Airy light/dark open canvas with double-shell cards and a single crisp blue accent. |
| `luma` | Soft neutral daylight/carbon surfaces with two-surface mist ground and knot-violet accent. |
| `glass` | Translucent liquid-glass overlays and floating docks over the canvas (CSS fallback mandatory; optional single-shader WebGL2 refraction layered over `@ui-sdk`, respecting `prefers-reduced-motion`). |

### First-party UI-shape skeleton loading (`boneyard-js`)

First-party Studio loading surfaces use **`boneyard-js`** (`0xGF/boneyard`, MIT, `boneyard-js/react`) via `BoneSkeleton` (`src/components/bone-skeleton.tsx`) and `LoadState` (`bone="..."`), rendering pre-captured responsive `.bones.json` geometry (`src/bones/`) styled with Studio's monochrome tokens (`var(--muted)` / `var(--well)`) and respecting `prefers-reduced-motion`. Every pending state remains bounded by `LoadState` (skeleton → slow notice → honest timed-out state with explicit Retry).

### Performance & loading discipline (TARGET)

- **Load on intent:** Use TanStack Router `defaultPreload: "intent"` (`defaultPreloadDelay: 50`) and lazy route chunks via `Route.lazy()` on the programmatic `createRoute` tree (or `autoCodeSplitting` if Studio adopts file-based routing); warm critical queries in route loaders and lazy-load heavy tools (`@xyflow/react`, `recharts`, `shiki`, WASM checker, optional WebGL shader) on hover/focus/activation.
- **Cold-entry budgets & profiling:** Gate first-view JS/CSS Brotli budgets in CI, enable React Compiler auto-memoization, and profile heavy filtering/typing interactions (`LCP <= 2.5s`, `INP <= 200ms`, `CLS <= 0.1`). Keep **React DOM 19** as the default renderer; alternative synchronous renderers (such as TanStack Redact) require measured Boat proof of both cold-entry wins and zero INP/portal regressions.

### Swappable shell presets

Shells own frame chrome and navigation geometry only; view components must never couple to a specific shell:

- `bottom-dock` — Pure open canvas with a single floating dark pill dock and minimal corner controls (TARGET default).
- `sidebar-spaces` — Collapsible space-oriented navigation rail (grouped by workspace/lens, never an alphabetical table dump).
- `minimal` — Chromeless canvas for embedded or focused single-task surfaces.

---

## 3. Hard Primitives Mandate — No Radix in First-Party Code

- **`radix-ui`, `@radix-ui/*`, `cmdk`, and `--radix-*` are strictly banned in first-party Studio code.** Automated CI enforcement (`apps/studio/test/ui-primitives-guard.test.ts` — **CURRENT**) blocks any direct or transitive Radix/cmdk import, CSS reference, or lockfile entry.
- **Delete instead of preserving (`AGENTS.md`).** Do not wrap legacy primitives behind compatibility shims.

### Approved web primitive stack

1. **React 19** — core rendering runtime.
2. **Vercel UI SDK (`@ui-sdk/core` + `@ui-sdk/react`, Apache-2.0)** — atomic interaction behaviors, controllable UI state primitives, and native `createAgent` Sidekick control.
3. **Base UI (`@base-ui/react`, MIT)** — accessible headless primitives for standard overlays and controls (dialogs, popovers, menus, tabs, tooltips, toggles, sheets, direction provider).

### Long-term universal runtime trajectory (TARGET)

Screens are authored declaratively in typed TypeScript (`tools/norn-ts`) and compile to canonical UI IR rather than binding domain semantics directly to DOM libraries. Keeping primitives declarative preserves a **TARGET** compilation path for **`@expo/ui`** (native mobile) and **Expo `Exact`** once it matures, without rewriting tenant definitions or authorization rules.

---

## 4. 3-Layer Arbitrary Checked UI Contract (TARGET, building on `#1638`)

Norn's **TARGET** UI runtime renders arbitrary tenant and AI-authored applications—creative studios (e.g., Fashn.ai, Higgsfield, Luma AI, Lovart), realtime customer globes, HR schedulers, lending desks—without collapsing every product into generic CRUD tables and without allowing unverified agentic UI drift. (`#1638` extracts the existing Ring-1 `norn-ui-checker` with behavioral parity; the runtime Screen Capability Manifest and spatial/media DSL below are **TARGET** extensions built on top of it.)

### Layer 1 — Typed UI DSL / Spatial & Media Primitives (`@norn/ui`, TARGET)

Every screen composes from declarative, IR-serializable primitives:

- `OpenCanvas` — pan/zoom spatial surface supporting multi-level semantic zoom.
- `MediaStage` — first-class media canvas supporting `image`, `video`, `compare` (before/after wipe), `mask` painting, and `bbox` annotations.
- `Timeline` / `Filmstrip` — time-ordered tracks, scrubbing, and visual frame strips.
- `OpenStream` — borderless chronological or semantic activity/record stream.
- `ControlDock` — single floating dark pill dock for tools, lenses, modes, and Sidekick triggers.
- `PromptComposer` — structured prompt, attachment, and parameter composer.
- `SplitPane` — resizable multi-region workspace layout.
- `DataGrid` — virtualized grid (`@tanstack/react-table` + `@tanstack/react-virtual`) reserved for genuinely dense tabular workflows.
- `ActionTrigger` — checked action invocation bound directly to a declared Norn action or transition.

### Layer 2 — Server-Only Authorization & Redaction (`norn-ui-checker` + Cell Host + Cedar + Markings)

Studio and `@norn/ui` hold **zero authorization authority** and **never receive unredacted restricted values** to hide in the browser:

- **Compile-time (`norn-ui-checker`, TARGET extension beyond `#1638`'s parity extraction):** Validates every UI binding, object/field read, marking flow, and action target against the canonical IR before a screen can publish.
- **Runtime (Cell Host enforcement — authoritative):**
  - **Pre-serialization redaction & withholding:** The cell host evaluates Cedar policies and Markings **before serialization**, withholding or replacing restricted fields with explicit redacted/masked placeholders on the wire. `@norn/ui` only renders those cell-returned placeholders (such as `data-masked` visual treatment) and never blurs raw restricted data in the browser.
  - **Whole-screen lock (TARGET Screen Capability Manifest):** When the cell refuses core screen reads for the active principal/scope, Studio renders a fail-closed locked state with the cell's reason—never a half-rendered shell.
  - **Dispatch-time action authorization & `APPROVAL-1` (`docs/spec/core/04-transactions-effects.md`):** The cell authorizes every action at dispatch. **Denied calls are refused immediately and never enter an approval queue.** Only an action call that **passes** Cedar authorization and matches an active approval gate parks under `APPROVAL-1` for human sign-off. Studio reflects capability hints and surfaces authoritative cell refusals without client-side authz guesses.

### Layer 3 — Thin Swappable Presentation & Skin Overlay

- Applies `[data-skin]`, shell preset, density, and locale direction on top of Layers 1 and 2 without altering data flow or security boundaries.
- **Sandboxed custom renderers (TARGET):** Arbitrary tenant visualizers (custom WebGL globes, bespoke stage canvases, interactive domain charts) run sandboxed and receive **only** already-authorized, cell-redacted props plus checked `<NornAction>` dispatch callbacks—never raw cell clients, unfiltered state stores, or unguarded network access.

---

## 5. Honest Truth & Semantic Map / Ripple UX (`#1664`)

### Honest status precedence

Truth status is a **server-only authority**. Never infer "verified" from the absence of findings, empty warning arrays, or a 200 HTTP response. Whenever multiple statuses apply to a record, field, or summary roll-up, render according to strict precedence:

```text
Quarantined  >  Withheld  >  Imported (unverified)  >  Verified fact
```

### Semantic Map (Islands / Lenses — Read-only Catalog Explorer CURRENT at `/map`; Cell-side Marking-Filtered Projection & Ripple TARGET `#1664`)

Underneath, the workspace is one flat System-of-Record (SoR) ontology (`norn-ir` + `#45` CodeGraph). On top, users navigate monochrome structural **Islands** through switchable **Lenses** without duplicating truth (reserving colour strictly for verified live attention status dots, badges, and highlighted causal links):

- **Zoom 0 — Landscape (CURRENT at `/map` over admitted `/v1/definitions`):** Monochrome structural Islands connected by bundled cross-island bridges with aggregate edge counts and honest live-inbox alert status (never claiming "no open alerts" when inbox reads are pending, unavailable, or recorded).
- **Zoom 1 — Island Expansion (CURRENT):** Plain-English object cards with compact dashed ghost stubs for cross-island neighbors and switchable lenses (`All`, `Data flow`, `Governance`, `Attention`).
- **Zoom 2 — Schema & Provenance Side Sheet (CURRENT for admitted definitions; `SRC` code-span evidence TARGET):** Canonical definition artifact link, source feed mapped/gap counts, governing laws in plain language, markings, connected upstream/downstream types, and direct jump links to Records and Definitions.
- **Cell-side marking-filtered map projection (TARGET `#1664`):** Today `/v1/definitions` checks `ReadDefinitions` at the catalog gate; fine-grained cell-side withholding across individual **nodes, edges, aggregate island/bridge counts, and `SRC` source spans** requires the server-side Semantic Map projection route tracked in `#1664`.

Switching Lenses re-projects the exact same underlying SoR graph (for example: operational flow, authority/markings, or lineage) rather than maintaining separate stale diagrams.

### SoR → Projection Ripple (TARGET `#1664`)

Changes to the System of Record ripple visibly and honestly into every dependent projection:

- **Pre-commit (TARGET, building on `#1371`):** Show `Before / Delta / After` architectural diffs (Archify) alongside dashed **Blueprint** overlays (Floorplan) on the canvas so operators see exact downstream impact before committing.
- **Post-commit (TARGET, building on `#1650`'s server-side trigger outbox):** Future projection-ripple handlers consume committed outbox triggers to refresh deterministic projections and emit Marking/IFC-safe **`Stale`** update tasks for anchored human or agent documents—never silently letting docs drift and never leaking restricted fields across marking boundaries.

---

## 6. Verification & Non-Negotiable Quality Gates

Every Studio PR must satisfy all applicable gates before merge:

1. **Domain-agnostic Studio (`apps/studio/test/domain-words.test.ts` — CURRENT):** `apps/studio/src/` must never name a business domain (identifiers or prose). Domain vocabulary belongs exclusively in packs (`registry/packs/`) and test fixtures. There is no allowlist.
2. **No-Radix CI guard (`apps/studio/test/ui-primitives-guard.test.ts` — CURRENT):** First-party Studio code, `apps/studio/package.json`, and `bun.lock` must contain zero `radix-ui`, `@radix-ui/*`, `cmdk`, or `--radix-*` references.
3. **Contrast & focus gate (`apps/studio/test/contrast.test.ts` & `apps/studio/test/studio-theme.test.ts` — CURRENT):** All built-in skins (`polar`, `open`, `luma`, `glass`), custom `StudioTheme` JSON overrides, buttons, chips, badges, tabs, and shell controls must pass automated WCAG AA contrast and focus-outline invariants across light and dark themes.
4. **Browser proof on Boat before merge (CURRENT):** Visual, layout, skin, shell, and interaction changes require Playwright E2E coverage (`bun run e2e`) plus real rendered **light, dark, and active skin screenshots** captured on Boat before merge. Static markup tests never substitute for rendered browser proof.
