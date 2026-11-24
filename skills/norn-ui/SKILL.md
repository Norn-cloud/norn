---
name: norn-ui
description: Build Open-Canvas tenant web applications over a Norn Cell strictly following Norn's DESIGN.md contract — React 19, Base UI, Vercel UI SDK, Geist Variable/Mono, Tailwind v4 data-skin tokens (Polar monochrome default + open/luma/glass), boneyard-js skeletons, zero Radix, and server-only PII redaction.
---

# Norn Open-Canvas UI (`norn-ui`)

Use this skill whenever building or reviewing a frontend for Norn Studio or any tenant application backed by a Norn Cell.

## Hard Stack & Primitives Mandate

### Required packages (pinned versions aligned with `apps/studio/package.json`)
- **Core:** `react@19.3.1`, `react-dom@19.3.1`, `tailwindcss@4.3.4`, `@tailwindcss/vite@4.3.4`, `tw-animate-css@1.4.0`
- **Primitives:** `@base-ui/react@1.9.0` (MIT — dialogs, popovers, menus, tabs, tooltips, toggles, sheets, direction provider)
- **Interaction & Agent Behaviors:** `@ui-sdk/core@0.1.0-beta.16`, `@ui-sdk/react@0.1.0-beta.16`, `@ui-sdk/web@0.1.0-beta.16` (Apache-2.0)
- **Typography:** `@fontsource-variable/geist@5.3.0`, `@fontsource-variable/geist-mono@5.3.0`
- **Icons & Dense Grids:** `lucide-react@1.67.0`, `@tanstack/react-table@8.21.3`, `@tanstack/react-virtual@3.14.22`
- **Skeleton Loading:** `boneyard-js@1.10.0` (`<Skeleton name="..." loading={...}>` from `boneyard-js/react`, themed via `--muted` / `--well`)

### Strictly Banned (Reject Immediately)
- **Zero Radix / cmdk:** Never install or reference `radix-ui`, `@radix-ui/*`, `cmdk`, or `--radix-*`.
- **Zero CSS-in-JS / Panda CSS:** Pure Tailwind v4 + native CSS custom properties only.
- **Zero CSS `zoom`:** Use `html { font-size: 90%; }`; never `zoom`.

### Base UI Gotcha
- Never coordinate with Base UI `CompositeRoot` focus from `onKeyDownCapture` + `queueMicrotask` (browsers drain microtasks between native capture and bubble listeners). Always hook bubble-phase `onKeyDown`.

## Aesthetic Contract: Open Canvas First

1. **No box-in-box CRUD admin panels.**
   - Lists, streams, schedules, and dossiers sit **directly on the open canvas** (`--canvas`).
   - Reserve borders/surfaces exclusively for: popovers/menus/tooltips, modals/drawers, active hover/selection states, the single floating `ControlDock`, and dashed Blueprint draft outlines.
   - **Banned:** >3 identical bordered white cards on one screen, heavy `700` uppercase admin headers, alphabetical schema-table sidebars, raw internal IR keys in operator headings.
2. **Strict Monochrome Surfaces (`data-skin="polar"` default):**
   - Large regions (canvas, shell, sidebars, tables, panels) stay monochrome black/grey/white.
   - Reserve color strictly for: charts/visualizations, status dots (`live`/`held`/`refused`), small semantic chips/badges, focused primary CTAs, and the brand mark (`--knot`).
3. **Typography & RTL (`LABEL-LOCALE-1`):**
   - `--font-sans: "Geist Variable", ...`; `--font-mono: "Geist Mono Variable", ...`; headings use tight tracking (`-0.02em`), medium (`500`–`600`) weight, and `font-variant-numeric: tabular-nums`.
   - Under `[dir="rtl"]`, set `unicode-bidi: plaintext` so mixed Arabic + Latin/numbers/codes render cleanly without scrambling.

## CSS Layering, Skins & `StudioTheme`

Line 1 of `index.css` **MUST** be `@layer ui-sdk;` before `@import "tailwindcss";`.

Set theme attributes on `<html>` (`:root`) **only** — never duplicate `data-skin` inside `<body>`:
- `data-skin`: `"polar" | "open" | "luma" | "glass"`
- `data-shell`: `"dock" | "sidebar" | "minimal"`
  - `dock`: open canvas + single floating dark pill `ControlDock` (`--dock-surface`)
  - `sidebar`: collapsible space-oriented navigation rail
  - `minimal`: chromeless single-task surface (ideal for store kiosks and embedded Telegram Mini Apps)
- `.dark` class on `<html>` for dark mode (`:root.dark[data-skin="..."]`)
- `data-density="compact"` for dense builder/matrix screens

Core semantic CSS variables:
- Surfaces/ink: `--shell`, `--canvas`, `--background`, `--foreground`, `--card`, `--well`, `--well-border`, `--popover`, `--muted`, `--muted-foreground`, `--border`, `--table-border`
- Brand/interactive/status: `--knot`, `--primary`, `--primary-foreground`, `--action`, `--ring`, `--destructive`, `--success`, `--warning`
- Floating dock: `--dock-surface`, `--dock-border`, `--dock-well`, `--dock-foreground`, `--dock-muted`, `--dock-active`, `--dock-shadow`
- Tenant JSON customization (`StudioTheme`): `{ version, name, baseSkin?, appearance?, accent?, canvas?, colors?, variants?: { light?, dark? } }` overriding only allowlisted semantic tokens while preserving WCAG AA (`>= 4.5:1` text/controls, `>= 3:1` focus/edges).

## Server-Only Authz, Redaction & Live Reads

- **Zero client-side authz authority:** The Norn Cell redacts marked fields (`pii.compensation`, `pii.gov_id`) **before serialization**. The UI renders explicit masked placeholders (`data-masked`) returned by the cell and surfaces authoritative cell refusals directly.
- **Honest status precedence:** Always render truth status in strict order: `Quarantined > Withheld > Imported (unverified) > Verified fact`.
- **Live updates:** Subscribe to the cell over `/v1/subscribe` (`norn.live.v1`) rather than polling; dispatch mutations only through checked Norn actions (`/v1/actions/{actionId}`).
- **Note on packaging:** Until `@norn/ui` (`packages/norn-ui`) is extracted as a standalone workspace package, tenant apps adopt `apps/studio/DESIGN.md`, the exact pinned packages, and the CSS token vocabulary directly.
