---
name: norn-ui
description: Build Norn Studio and tenant web applications over a Norn Cell following Norn's unified v6 Open Analytics + Polar visual contract — React 19, Base UI, Vercel UI SDK, Geist Variable/Mono, Tailwind v4 data-skin tokens, zero Radix, RTL + mobile hit targets, and honest cell action/redaction handling.
---

# Norn UI (`norn-ui`)

Use this skill whenever building or reviewing a frontend for Norn Studio or any tenant application backed by a Norn Cell.

## Hard Stack & Primitives Mandate

### Required packages (aligned with `apps/studio/package.json`)
- **Core:** `react@19.3.1`, `react-dom@19.3.1`, `tailwindcss@4.3.4`, `@tailwindcss/vite@4.3.4`, `tw-animate-css@1.4.0`
- **Primitives:** `@base-ui/react@1.9.0` (MIT — dialogs, popovers, menus, tabs, tooltips, toggles, direction provider)
- **Interaction Behaviors:** `@ui-sdk/core@0.1.0-beta.16`, `@ui-sdk/react@0.1.0-beta.16`, `@ui-sdk/web@0.1.0-beta.16` (Apache-2.0)
- **Typography:** `@fontsource-variable/geist@5.3.0`, `@fontsource-variable/geist-mono@5.3.0`
- **Icons & Dense Grids:** `lucide-react@1.67.0`, `@tanstack/react-table@8.21.3`, `@tanstack/react-virtual@3.14.22`
- **Loading Skeletons:** Use Studio's local `Skeleton` primitive (on `main`, `apps/studio/src/components/ui/skeleton.tsx`); do not assume `boneyard-js` is installed unless added by the repo.

### Strictly Banned
- **Zero Radix / cmdk:** Never install or reference `radix-ui`, `@radix-ui/*`, `cmdk`, or `--radix-*`.
- **Zero CSS-in-JS / Panda CSS:** Pure Tailwind v4 + native CSS custom properties only.
- **Zero CSS `zoom`:** Use `html { font-size: 90%; }` (`1rem = 14.4px`); never `zoom`.

### Base UI Gotcha
- Never coordinate with Base UI `CompositeRoot` focus from `onKeyDownCapture` + `queueMicrotask` (browsers drain microtasks between native capture and bubble listeners). Always hook bubble-phase `onKeyDown`.

## Visual Contract: Open v6 + Polar Craft

1. **Concentric double-shell framing vs. open streams (zero CRUD box walls):**
   - Studio currently defaults to `data-skin="open"` + `data-shell="sidebar"`.
   - **`open` skin:** Uses concentric double-shell framing for selected panels and KPI tiles (`ShellCard` / `StatWell` design patterns — `20px` outer card rim with `4px` padding wrapping a `15px` soft inner `.v6-well`, `#ffffff` / `#0d0d0e` canvas, single blue accent `#2e60e0` / `#5b8cff`).
   - **`polar` skin:** Removes outer padding and flattens KPI wells into strict monochrome ink (`#18181b` light / `#f4f4f5` dark primary, flat hairlines, zero chromatic buttons).
   - Keep streams and lists calm and open; never nest bordered cards inside bordered cards or ship heavy `700` uppercase admin table headers.
2. **Calm typography & quiet status semantics:**
   - `--font-sans: "Geist Variable", ...`; `--font-mono: "Geist Mono Variable", ...`; `font-variant-numeric: tabular-nums`.
   - Use **`font-normal` (`400`)** for card headers, list titles, and hero KPI figures; reserve **`font-medium` (`500`)** for `h1`, sheet titles, active pills, and primary CTAs. Lock a small semantic type scale rather than ad-hoc rem sizes.
   - Prefer **`6px` status dots + muted text** over screaming filled status badges. Reserve colour strictly for status dots, charts/allocation bars, tokenized soft avatar discs, the Norn knot (`--knot`), and (in `open`) one primary CTA.
3. **List -> Floating Inset `DetailSheet` (Design Pattern):**
   - Keep primary views uncluttered by opening row details, approval `Before -> After` diffs, and receipts in a floating inset right-side sheet (`12px` inset, `rounded-[24px]`, built on `@base-ui/react/dialog`) with one primary CTA in the footer.
   - *(Note: `ShellCard`, `StatWell`, and floating `DetailSheet` are design patterns to implement with local primitives — not pre-exported workspace components; Studio's existing generic `Sheet` is edge-flush on `@ui-sdk/react/dialog`.)*
4. **RTL & Mobile Ergonomics (`LABEL-LOCALE-1`):**
   - Under `[dir="rtl"]`, set `unicode-bidi: plaintext` on text elements, use CSS logical properties (`ms-*`, `me-*`, `ps-*`, `pe-*`, `start-*`, `end-*`), and mirror overlay slide directions and directional arrows.
   - Enforce **`>= 44px` (44 CSS-pixel) touch targets** on mobile/touch views; pair icon docks with clear labels on touch screens.

## CSS Layering & Skin Attributes

Line 1 of `index.css` **MUST** be `@layer ui-sdk;` before `@import "tailwindcss";`.

Set theme attributes on `<html>` (`:root`) **only** — never duplicate `data-skin` inside `<body>`:
- `data-skin`: `"open" | "polar" | "luma" | "glass"`
- `data-shell`: `"sidebar" | "dock"` (`dock` uses the dark floating bottom pill `--dock-surface`; `minimal` is a target for single-task embeds)
- `.dark` class on `<html>` (`:root.dark[data-skin="..."]`)
- `data-density="compact"` on dense builder/matrix surfaces

Core semantic CSS variables:
- Surfaces/ink: `--shell`, `--canvas`, `--background`, `--foreground`, `--card`, `--well`, `--well-border`, `--well-hover`, `--popover`, `--muted`, `--muted-foreground`, `--border`, `--panel-border`, `--table-border`
- Brand/interactive/status: `--knot`, `--primary`, `--primary-foreground`, `--action`, `--action-hover`, `--action-foreground`, `--ring`, `--destructive`, `--success`, `--warning`
- Floating dock: `--dock-surface`, `--dock-border`, `--dock-well`, `--dock-foreground`, `--dock-muted`, `--dock-active`, `--dock-shadow`

*(Use semantic CSS custom properties on `:root`; do not assume a runtime `StudioTheme` JSON API until it ships.)*

## Action Calls, Redaction & Local Development

- **Three public action outcomes (`CallResult`):** Every action dispatch must handle **`receipt | pending | refused`**:
  - `receipt`: committed mutation (or `outcome: "replayed"` on an identical retry) carrying the receipt hash or keyed handle, commit sequence, and logical time.
  - `pending`: HTTP `202` carrying a pending approval/decision — **no business mutation has committed yet**.
  - `refused`: authoritative cell refusal (`400` invalid input, `403` forbidden, `409` conflict/stale, `410` expired approval, `413` oversized request, `422` law/domain violation, `429` declared limit) — surface the structured refusal message honestly; never swallow failures or optimistically fake commits.
  - *(Note: Studio's current private client in `apps/studio/src/lib/actions.ts` also surfaces transport states `no-cell` and `unavailable`.)*
- **Idempotency key rule:** Mint **one opaque `idempotencyKey` per user intent** (e.g., per button click/submission), reuse it across retries of that same attempt, and mint a fresh key for a second user intent even with identical input. Send `{ input, idempotencyKey }` in the `POST /v1/actions/{actionId}` JSON body (a header alone is insufficient).
- **Zero mock mode in production UIs:** Use the real `norn dev` runtime for local integration — never ship mock data arrays, fake client-side business math, or dev role switchers in production bundles.
- **SDK target:** Extracting the action client into `packages/norn-sdk` and adding types-only `norn codegen` (emitting a single `.d.ts` `ActionMap`) is the **target**; today `packages/norn-sdk` exports generated OpenAPI types while Studio holds the private client.
- **Server-only redaction:** The cell strips or masks restricted fields on the wire **before serialization**; Studio renders masked wire values with a DOM `data-masked` attribute (`apps/studio/src/components/masked-value.tsx`). Never treat client-side hiding as authorization.
- **Honest status precedence:** Render truth status in strict order: `Quarantined > Withheld > Imported (unverified) > Verified fact`.
