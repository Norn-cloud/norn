---
name: norn-ui
description: Build or review Norn UIs (declarative widgets, themes, @norn/react, apps/studio, vertical apps, and custom canvas islands) under the Norn design system, Ring-1 norn-ui-checker contract, and headless state-machine verification gates.
---

# Norn UI

## Read first

- [Norn design system](../../../apps/studio/DESIGN.md) — read the sections your task touches instead of skimming the full file:
  - Tokens, skins & monochrome chrome: §§3–4, §13, §19.6
  - Geometry, spacing, locked 5-step type scale & calm 400/500 weights: §§5–8
  - Shell modes, canonical primitives (`src/components/system/`), `List → DetailSheet`: §§9–11
  - Honest states (loading, unavailable, refused/withheld, 4-state call outcomes, empty): §12
  - Arabic/RTL & motion: §§14–15
  - Agent operability, named controls & PR gates: §16
- [`@norn/react` README](../../../packages/norn-react/README.md) and [`crates/norn-ui-checker/src/lib.rs`](../../../crates/norn-ui-checker/src/lib.rs): the checked Widget UX contract and framework-free page machine (`UI-MACHINE-1`).
- Pair with the global **`ui-sdk`** skill whenever composing or extending `@ui-sdk/core`, `@ui-sdk/react`, or `@ui-sdk/web` behaviors, and with [norn-authoring](../norn-authoring/SKILL.md) for actions, policies, lifecycles, and packs.

## Primitives & stack (CURRENT, `origin/main` `114450852a`)

| Layer | Where | What to use |
| --- | --- | --- |
| Declarative widget authoring | [`tools/norn-ts/src/builder/widget.ts`](../../../tools/norn-ts/src/builder/widget.ts) | `W.widget`, `W.uiSdk`, `W.fromUiSdk`, `W.outcome`, `W.lowerWidget` (admitted by Ring-1 `norn-ui-checker`) |
| Server-served themes (`#1908`) | `tools/norn-ts/src/builder/theme.ts` *(landing in `#1892`)* | `defineTheme`, `lowerTheme`, semantic theme tokens — never raw hex literals in components |
| Admitted widget renderer | [`packages/norn-react/src/index.ts`](../../../packages/norn-react/src/index.ts) | `<NornAppRenderer>`, `useWidgetMachine`, `createPageRuntime`, `packages/norn-react/src/machine/` (framework-free `update`/driver/walks), `capabilities.ts` (L6 whole-page refusal) |
| Interactive web primitives | `@ui-sdk/core`, `@ui-sdk/react`, `@base-ui/react` | All interactive widgets (dialogs, tabs, lists, forms, menus, popovers, toggles, keyboard/focus/ARIA/RTL) |
| Studio canonical system primitives | [`apps/studio/src/components/system/`](../../../apps/studio/src/components/system/) | `Panel`/`Well`/`EmptyState` (`ShellCard`), `KpiTile`/`StatStrip` (`StatWell`), `ListRow`/`RowList`, `Props`, `StatusDot`/`Dot`, `SourceTag`, `Chip`, `TableCard`, `TickBar`, `Initials`/`TypeDisc`, `LanguagePill`, `SchemeToggle`, `kit.tsx` |

## Hard rules

1. **Server-only authority.** Never evaluate roles, permissions, or business rules on the client; render from server-admitted capabilities, affordances, and refusals. Never infer "verified" from absent findings, empty warnings, or a 200 response. Truth precedence: `Quarantined > Withheld > Imported (unverified) > Verified fact`.
2. **Closed primitive stack.** Use `@ui-sdk/*`, `@base-ui/react`, and `src/components/system/`. **Zero `@radix-ui/*`**, **zero `cmdk`**, **zero sparkle icons** (`DESIGN.md` §19.2), and **zero `div` with `onClick`**.
3. **Studio is domain-agnostic.** Never mention tenant or vertical domain words (e.g. jewelry, store-specific nouns) in `apps/studio/` (`test/domain-words.test.ts`); domain meaning lives in packs.
4. **Monochrome chrome & locked type.** Saturated color belongs only in status dots (always paired with a label), chart/data marks, avatars, mascot, and search/diff highlights (`test/monochrome.test.ts`). Use only the locked 5-step type scale and weights 400/500 — no arbitrary `text-[…]`, no 600/700 headings, no uppercase tracked chrome (`test/type-lock.test.ts`).
5. **Concentric shell geometry only.** Allowed container nesting is `ShellCard`/`StatWell` with **one** well inside (`DESIGN.md` §10). Never put a card inside a card, a bordered box inside a well, or filled status pills (status is dot-first; `SourceTag` is outline-only). One solid `PrimaryButton` per view or sheet.
6. **Honest states & 4-state call outcomes.** Handle all four action outcomes (`pending`, `success`, `failure`, `unknown_outcome`), confirm guards, `unavailable` with a single-click `Retry`, and terminal HTTP 404 `not_served` cleanly (disposing live subscriptions and timers on `not_served`). Do not show a disabled primary button with no server refusal reason.
7. **Agent operability & keyboard safety.** Every interactive control has a unique accessible name and `data-slot`. Domain-mutating controls carry fail-closed `data-agent-confirm` (unknown effects require confirmation) and honor `data-agent-secret`/`-hidden`. On Base UI composites (`Toolbar`, `ToggleGroup`, `RadioGroup`, `Tabs`), attach keyboard handlers in bubble-phase `onKeyDown`, never `onKeyDownCapture`.
8. **First-class Arabic & RTL.** Logical CSS properties only (`ms/me/ps/pe`, `start/end`, `inset-inline`, `border-s/e`, `text-start/end`, `rounded-s/e` — zero physical `ml/mr/pl/pr/left/right`). Wrap codes, IDs, money, and times in `<bdi>` / `dir="ltr"`. Format data numerals with Western `latn` digits (`-u-nu-latn`) in both locales. Flip directional icons and inline slide motion with `rtl:rotate-180` / `rtl:-translate-x-*`, and honor `prefers-reduced-motion`.
9. **Custom spatial/chart/canvas islands stay read-only.** Bespoke canvases (maps, flow graphs, timelines, visualizers) take only redacted read-only projections + theme tokens and emit typed intents (`select`, time scrub, `openAction`). They never mutate cell state or decide permissions; governed mutations route through Norn actions and confirm dialogs.

## Verification order (state machines first, browsers last)

Run fast deterministic checks in the inner loop; run Playwright / real browser verification last (and heavy Rust/Playwright runs on CI or disposable Boat sandboxes, never on shared dev hosts):

1. **Ring-1 widget/theme admission:** `bun tools/norn-ts/src/cli.ts check <pack|file.ts> --org ID --env ID` and `bun test test/widget-builder.test.ts` (plus `test/theme-builder.test.ts` once `#1892` lands) in `tools/norn-ts`.
2. **Headless page-machine & contract suite (`packages/norn-react`):** `bun run typecheck && bun test` — covers `machine.test.ts` (manual clock, timeout/reply races, stale outcomes, `not_served`), `capabilities.test.ts`, `runtime.test.ts`, `dom-contract.test.tsx`, and `vocab-v2.test.tsx`.
3. **Studio static & design-system gates (`apps/studio`):** `bun run typecheck && bun test` — specifically enforces `domain-words.test.ts`, `ui-primitives-guard.test.ts`, `contrast.test.ts`, `studio-theme.test.ts`, `monochrome.test.ts`, `type-lock.test.ts`, `system-boundary.test.ts`, `honest-states.test.ts`, `locale.test.ts`, and `workspace-retry-lint.test.ts`.
4. **Final browser proof (only after gates 1–3 pass):** `bun run e2e` in `apps/studio` on Boat, covering light, dark, active skin, and Arabic/RTL screenshots for visual or interaction changes.
5. **Repo provenance & cheap gate:** `python3 scripts/record_new_files.py --check` and `bash scripts/ci/core.sh cheap`.
