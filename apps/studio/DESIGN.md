# Norn Design System

This is the one design and UI engineering contract for every Norn UI: Studio (`apps/studio/`), the
apps compiled and rendered through `@norn/ui`, first-party wedge apps such as the Family Business HR
app, and custom or AI-generated Norn apps. Every new screen, primitive, skin or visual change must
conform. Where a rule here and a screen disagree, the screen is wrong; the gap list at the end
(section 18) tracks the known ones.

The spec merges two sources. Studio's contract supplies the architecture: server-driven UI with no
client authority, Base UI + Vercel UI SDK primitives, Geist, `boneyard-js` skeletons, the StudioTheme
JSON and the honesty rules. The HR wedge app (a private first-party app built on Norn, descended from the v6
mockups) supplies the look: the owner judged it better. **Where the
two conflict, the wedge's pattern wins, unless Studio's rule is stricter about authority, honesty,
accessibility or RTL.** Each such call is recorded where it is made, and in the token diff
(section 17).

Status words used below: **CURRENT** means shipped and enforced; **TARGET** means required for new
work and tracked as a gap until the code catches up.

---

## 1. Principles

1. **The server decides; the UI shows.** The cell host and its permission system (Cedar, Markings)
   are the only authority. No client-side permission logic, no per-screen endpoints, no hiding of
   values the browser already holds. Fix the renderer or the server document, never the screen.
2. **Calm, then clear.** Hierarchy comes from size, spacing and one tonal step between surfaces,
   not from weight, borders or colour. A screen reads in one breath: page title, a band of key
   figures, then lists that open into a sheet.
3. **One way to nest.** The concentric double shell (`ShellCard` around a well) is the only nesting
   pattern. Anything else that frames a frame is a bug.
4. **Colour means something.** Surfaces are neutral. Saturated colour is reserved for status dots,
   data marks, avatar discs, the focus ring and the single primary action on a view.
5. **Honest by construction.** Every state a screen can be in (loading, empty, unavailable,
   refused, withheld, committed) has a designed, truthful rendering. Nothing is inferred as
   "verified" or "zero" from silence.
6. **Both directions are first-class.** Arabic and English are designed together; RTL is not a
   mirror added later.
7. **Agents use the same controls as people.** Every control is a named, accessible primitive bound
   to a declared Norn action, so the Sidekick and automation drive exactly what a person drives.
8. **Domain-agnostic Studio.** `apps/studio/src/` never names a business domain. Domain words live
   in packs (`registry/packs/`) and fixtures; wedge apps carry their own vocabulary.

---

## 2. Stack (CURRENT)

| Layer | Choice | Rule |
| --- | --- | --- |
| Runtime | React 19 | React DOM is the renderer. Alternatives need measured Boat proof (LCP, INP, portals). |
| Behaviour | Vercel UI SDK (`@ui-sdk/core`, `@ui-sdk/react`, Apache-2.0) | Menu, Popover, Toolbar, Tooltip, SegmentedControl, ChromeDock; agent control (`createAgent`, TARGET). |
| Headless primitives | Base UI (`@base-ui/react`, MIT) | Dialog (sheets, modals), Popover, Tabs, Toggle, DirectionProvider. |
| Styling | Tailwind v4 + CSS custom properties | `@layer ui-sdk;` first, then Tailwind; tokens via `@theme inline`. No CSS-in-JS, no Panda. |
| Icons | `lucide-react` | 1.6–1.7 stroke; size from the type step they sit beside. |
| Fonts | Geist Variable, Geist Mono Variable, Cairo (Arabic) | Self-hosted via `@fontsource-variable/*`. |
| Skeletons | `boneyard-js` (`boneyard-js/react`, MIT) | Through `BoneSkeleton` and `LoadState bone="…"` only. |

**Banned:** `radix-ui`, `@radix-ui/*`, `cmdk`, `--radix-*` anywhere in first-party code, its
`package.json` or `bun.lock` (`apps/studio/test/ui-primitives-guard.test.ts`, CURRENT). Delete
legacy primitives instead of wrapping them in shims.

Screens are authored declaratively (`tools/norn-ts`) and compile to the canonical UI IR, which keeps a
TARGET path open to `@expo/ui` without rewriting tenant definitions or authorization rules.

---

## 3. Theming model

Everything visual is a token on `<html>`. Components read semantic tokens only; they never branch
on skin, shell or scheme.

| Attribute | Values | Owns |
| --- | --- | --- |
| `data-skin` | `open` (default), `polar`, `luma`, `glass` | Palette, radii, shadows. |
| `.dark` | present or absent | Light or dark scheme (OS preference unless the user or a theme forces one). |
| `data-shell` | `dock` (default), `sidebar` | Frame chrome and navigation geometry only (section 9). |
| `data-density` | `comfortable` (default), `compact` | Row heights (`--density-*`); `compact` for builder views. |
| `dir`, `lang` | `ltr` / `rtl`, BCP 47 | Direction and font pairing (section 14). |

**Default skin is `open`** (the wedge's default and the look the owner chose). `polar` is the
first-class monochrome alternative. `luma` and `glass` remain supported skins and follow the same
geometry rules. This replaces Studio's earlier `polar` default; see section 17 for why.

**StudioTheme JSON (CURRENT, `src/lib/studio-theme.ts`).** Operators and tenants may supply a
declarative theme (`version: 1`, `baseSkin`, `appearance`, `accent`, `canvas`, `colors`,
`variants.light` / `variants.dark`) that overrides only the allowlisted semantic tokens
(`STUDIO_THEME_TOKENS`). Themes carry zero logic: no queries, permission checks or layout forks, and
never geometry, z-index or arbitrary CSS. Every theme is sanitized against CSS injection and must
pass the contrast gate (section 16) before it applies. New tokens this spec adds that are colours
(`subtle-foreground`, `line`, `action-dot`, `action-soft`, the dot and avatar tokens) join the
allowlist; geometry tokens never do.

---

## 4. Colour tokens

Values are hex; `rgb(… / a)` where translucent. Text pairs are measured against the surface they sit
on, including the well, and must pass section 16.

### 4.1 Surfaces and text

| Token | open light | open dark | polar light | polar dark | Use |
| --- | --- | --- | --- | --- | --- |
| `--canvas` | #ffffff | #0d0d0e | #f6f6f8 | #0a0a0b | Page ground. |
| `--shell` | #f6f6f7 | #09090a | #f6f6f8 | #0a0a0b | Sidebar shell ground. |
| `--card` | #ffffff | #151517 | #ffffff | #151517 | Outer rim of a ShellCard, sheet, selected row. |
| `--panel-border` | #ebebeb | #232326 | #e5e5ea | #25252a | Hairline around a card or sheet. |
| `--well` | #f5f5f5 | #1c1c1f | #f7f7f9 | #1b1b1e | Inner well. |
| `--well-border` | #ededed | #27272b | #e8e8ec | #26262b | Well hairline. |
| `--well-hover` | #eeeeee | #232327 | #efefef | #222226 | Row hover inside a well. |
| `--popover` | #ffffff | #19191c | #ffffff | #1c1c20 | Menus, popovers. |
| `--line` | #e8e8e8 | #2a2a2e | #e5e5ea | #26262b | Dividers, SourceTag ring, sheet footer rule. |
| `--input` | #e4e4e4 | #2c2c30 | #dcdce2 | #323238 | Control borders, pill outline. |
| `--foreground` | #1b1b1d | #ececee | #18181b | #fafafa | Primary text. |
| `--muted-foreground` | #6e6e73 | #9b9ba3 | #5c5c66 | #9d9da6 | Secondary text, counts, status labels, metrics. |
| `--subtle-foreground` | #85858b | #6a6a72 | #85858b | #686870 | **Decorative only:** separators (`×`, `·`), info glyphs, chevrons. Never the only carrier of information. |

### 4.2 Action

| Token | open light | open dark | polar light | polar dark |
| --- | --- | --- | --- | --- |
| `--action` (CTA fill, text-on-surface accent) | #2e60e0 | #2e60e0 | #18181b | #f4f4f5 |
| `--action-hover` | #2451c7 | #2451c7 | #2c2c32 | #e4e4e7 |
| `--action-foreground` | #ffffff | #ffffff | #fafafa | #141416 |
| `--action-dot` | #7f9ef4 | #6f97ff | #71717a | #a1a1aa |
| `--action-soft` (selected wash) | rgb(58 109 240 / .10) | rgb(91 140 255 / .14) | rgb(24 24 27 / .08) | rgb(244 244 245 / .10) |
| `--action-fill` (active segment) | rgb(58 109 240 / .14) | rgb(91 140 255 / .20) | rgb(24 24 27 / .12) | rgb(244 244 245 / .16) |
| `--ring` | #2e60e0 | #5b8cff | #52525b | #a1a1aa |
| `--link` | #2451c7 | #7da4ff | #18181b | #e4e4e7 |

`--primary*` aliases `--action*`. `--knot` (#5b52f0 / #7c74ff) is reserved for the Norn mark.

### 4.3 Status: dots and text are separate tokens

Status colour comes in two strengths. **Dot** tokens are saturated and used only for 6px dots, data
bars and chart marks; a dot is always paired with a text label, so it is never the only carrier of
the state. **Text** tokens are the AA-safe versions used when a value itself must be coloured (a
signed delta, an "After" value, a page-head live line).

| State | Dot token | light | dark | Text token | light | dark |
| --- | --- | --- | --- | --- | --- | --- |
| live / ok | `--dot-live` | #1f9d61 | #3ccf8e | `--success` | #1f7a4d | #3ccf8e |
| held / pending | `--dot-held` | #e09a2b | #f0b04a | `--warning` | #8a5600 | #f0b04a |
| refused / failed | `--dot-bad` | #e5484d | #ff6b6f | `--destructive-text` | #b83228 | #ff6b6f |
| quiet / draft | `--dot-quiet` | `--subtle-foreground` @ 60% | same | — (use `--muted-foreground`) | | |
| accent / new | `--dot-accent` | `--action-dot` | same | — | | |

`--destructive` (fill for a destructive confirm button) is #b83228 light, #eb7168 dark with
`--destructive-foreground` #ffffff / #0b0b0e.

### 4.4 Data, kinds and avatars

- `--chart-1…5`: series colours. `chart-1` is the accent (#3a6df0 / #5b8cff in `open`, ink in
  `polar`); 2–5 are Studio's AA-tuned categorical set (#0f726a, #b45309, #9a1ca6, #7d8494 light).
- `--kind-*`, `--tint-*`, `--tone-*`: Studio's detail colours for artefact kinds and canvas node
  tints. They colour icons, dots, edges and chart marks, never surfaces or label fills.
- **Avatar palette (TARGET tokens).** Six disc pairs, `--avatar-1…6-bg` / `--avatar-1…6-fg`:

  | n | light bg / fg | dark bg / fg |
  | --- | --- | --- |
  | 1 | #fde4e1 / #9a3b2e | `color-mix(in oklab, #9a3b2e 30%, var(--card))` / #fde4e1 |
  | 2 | #e3ecff / #2d4fa8 | mix of #2d4fa8 @ 30% / #e3ecff |
  | 3 | #e2f5ea / #23744a | mix of #23744a @ 30% / #e2f5ea |
  | 4 | #fdf0d8 / #8a5a12 | mix of #8a5a12 @ 30% / #fdf0d8 |
  | 5 | #efe6fd / #6b3fae | mix of #6b3fae @ 30% / #efe6fd |
  | 6 | #dff3f6 / #1f6f7a | mix of #1f6f7a @ 30% / #dff3f6 |

  Every light pair measures ≥ 5.0:1. The disc index is a stable hash of the **principal or record
  id**, never the display name, so a rename or a switch between Arabic and English keeps the colour.
  Hard-coded hex in components (the wedge's `DISC_BG`/`DISC_FG`) is replaced by these tokens.

### 4.5 Dock

`--dock-surface` #1d1d20 / #222226, `--dock-well` #34343a / #3a3a41, `--dock-border`
rgb(255 255 255 / .08), `--dock-foreground` #ffffff, `--dock-muted` #a5a5ad (6.9:1 on the dock),
`--dock-ring` #93b4ff, `--dock-indicator` = `--action` dot. The dock is dark in every skin and
scheme; `glass` makes it translucent.

---

## 5. Radii and concentric geometry

Radii are **pixel** values, not rem: geometry must not change with the 90% root scale.

| Token | open | polar | luma | glass | Used by |
| --- | --- | --- | --- | --- | --- |
| `--radius-panel` | 20px | 16px | 16px | 20px | ShellCard, StatWell outer rim. |
| `--panel-inset` | 4px | 4px | 4px | 4px | Padding between rim and well. |
| `--radius-well` | 15px | 11px | 11px | 15px | Inner well. |
| `--radius-row` | 11px | 11px | 11px | 11px | ListRow, menu row (10px). |
| `--radius-sheet` | 24px | 24px | 24px | 24px | DetailSheet. |
| `--radius-search` | 18px | 18px | 18px | 18px | Page search field. |
| `--radius-dock` | 26px | 26px | 26px | 26px | Floating dock. |
| `--radius-popup` | 16px | 16px | 16px | 16px | Menus, popovers. |
| `--radius` | 8px | 8px | 8px | 8px | Form inputs, code blocks. |
| `--radius-chip` | 5px | 5px | 5px | 5px | SourceTag. |
| pills | full | full | full | full | Top-bar controls, every button, segmented controls. |

**Concentric rule:** `radius-panel = radius-well + panel-inset + 1px border`. 15 + 4 + 1 = 20;
11 + 4 + 1 = 16. A skin that changes one changes the other. Every skin keeps the double shell; no
skin flattens it (Studio's `polar` used to drop the inset; that is now a gap).

---

## 6. Elevation

| Token | light | dark |
| --- | --- | --- |
| `--panel-shadow` | `0 1px 2px rgb(0 0 0/.025), 0 2px 6px -2px rgb(0 0 0/.02)` (`polar`: `0 1px 2px rgb(0 0 0/.02)`) | `inset 0 1px 0 rgb(255 255 255/.02)` (`polar`: `.03`) |
| `--popup-shadow` | `0 16px 40px -12px rgb(0 0 0/.18), 0 2px 8px rgb(0 0 0/.04)` | `0 18px 44px -14px rgb(0 0 0/.72)` |
| `--sheet-shadow` | `0 24px 64px -16px rgb(0 0 0/.18), 0 4px 12px rgb(0 0 0/.04)` | `0 24px 64px -16px rgb(0 0 0/.8)` |
| `--dock-shadow` | `0 10px 30px -8px rgb(0 0 0/.35), 0 2px 6px rgb(0 0 0/.12)` | `0 10px 30px -8px rgb(0 0 0/.7)` |
| `--elevation-node`, `-node-hover`, `-float` | Studio's canvas values (CURRENT) | same |

Four levels only: flat (canvas, wells), card (`panel-shadow`), popup, and sheet/dock. Shadows are
whisper contact plus soft ambient; never a hard blob. Dark mode lifts with a 1px inner highlight
rather than a darker shadow.

---

## 7. Spacing and layout

- **Root scale:** `html { font-size: 90% }`, so `1rem = 14.4px`. Never CSS `zoom` (it breaks
  `@xyflow/react` pointer maths). Anything that must be an exact pixel size (touch targets, radii,
  dots) is written in px.
- **Canvas:** content `max-width: 1220px`, centred, `px-6 sm:px-8`. Top bar `max-width: 1260px`.
- **Vertical rhythm:** page head `pt-6 pb-5`; `gap-4` between cards in a band; `mt-6` between bands.
- **Dock clearance:** in dock shell, the scroll container ends with `pb-32` so the last row clears
  the dock.
- **Inside wells:** list wells `p-1.5`; prose wells `p-5`; Props rows `px-2.5 py-2.5`.
- **Grids:** KPI band is `grid gap-4 sm:auto-cols-fr sm:grid-flow-col` (2–4 StatWells); the list
  band is 2 columns ≥ 1024px (`lg:grid-cols-[1.4fr_1fr]` or equal), 1 below.
- **Density:** `--density-row-scan` 2rem / 1.75rem compact, `--density-row-read` 1.75rem / 1.5rem,
  `--density-cell` 2.375rem / 1.9375rem. ListRow stays `h-12` in comfortable density.

---

## 8. Type

Fonts: `--font-sans: "Geist Variable", "Cairo Variable", ui-sans-serif, system-ui, sans-serif`;
`--font-mono: "Geist Mono Variable", ui-monospace, SFMono-Regular, Menlo, monospace`. Body:
`letter-spacing: -0.003em`, `font-variant-numeric: tabular-nums`, antialiased.

### 8.1 The locked five-step scale (TARGET tokens)

| Step | Token | Size | px @ 90% | Line height | Tracking | Weight | Used for |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `--text-meta` | 0.84rem | 12.1 | 1.4 | 0 | 400 | Secondary runs, counts, timestamps, mono codes, table headers, SourceTag, footnotes, dock labels. |
| 2 | `--text-body` | 1rem | 14.4 | 1.45 | -0.003em | 400 | List titles, card titles, Props, buttons, menu rows, inputs, prose. |
| 3 | `--text-heading` | 1.35rem | 19.4 | 1.25 | -0.02em | 500 | Sheet titles, dialog titles. |
| 4 | `--text-title` | 1.85rem | 26.6 | 1.15 | -0.025em | 500 | Page `h1`. |
| 5 | `--text-figure` | 2rem | 28.8 | 1 | -0.025em | 400 | StatWell values, hero numbers. Unit at `0.54em`, muted, `tracking-normal`. |

**No other sizes.** No `text-[0.88rem]`, `text-[11px]`, `text-xs`/`text-sm` drift in first-party
screens. Mono runs use step 1 or 2 in the mono family with `letter-spacing: -0.01em`. Canvas edge
labels and minimap text are the one exception (they scale with the canvas zoom) and stay inside
`index.css`. Migration: Studio's `--text-panel` maps to `--text-body`, `--text-section` to
`--text-heading`; the old names are deleted once the restyle lane lands (no shims).

### 8.2 Weights

- **400** everywhere by default: card headers, list titles, KPIs, Props, body.
- **500** only for: page `h1`, sheet/dialog titles, the active pill or segment, the primary CTA.
- **600 and 700 are banned** in UI chrome and content, including canvas node cards and decision
  trees. Emphasis comes from size step, `--foreground` vs `--muted-foreground`, or position.
- No uppercase chrome (section banners, eyebrow labels). Codes keep their own case.

---

## 9. Shell modes

Shells own frame chrome and navigation only. A view never knows which shell it is in.

### 9.1 `dock` (default for operators and every app surface)

- **Top bar:** `h-[4.5rem]`, **no bottom border**, no background change from the canvas. Start
  side: Norn mark (32px), then a workspace pill (`rounded-full`, `h-9`, 24px org disc, name,
  chevron) whose menu holds workspace, access preview (where the server grants it) and skin. End
  side, in order: the guarantee tick bar pill (section 12.4), language toggle pill, scheme toggle
  (icon pill), the principal's avatar disc (36px).
- **Floating dock:** centred, `fixed bottom-6`, `rounded-[26px]`, `--dock-surface`, `p-1.5`,
  `gap-1`, ring `--dock-border`, `--dock-shadow`. Built on UI SDK `Toolbar` with
  `aria-label="<app> navigation"`. Contents: up to six destinations, a hairline separator, the
  assistant trigger, the appearance tray. More than six destinations go behind a "More" menu.
  - Destination: 40px round button, 18px icon, `aria-label` = its name, `aria-current="page"` when
    active, active fill `--dock-well`. Attention marker: 6px `--dock-indicator` dot at
    `top-[7px] end-[7px]` with a 2px `--dock-surface` ring.
  - ≥ 640px: icon-only with a top tooltip carrying the label.
  - **< 640px: every destination shows its label** under the icon (`--text-meta`) and every target
    is at least **44 × 44 CSS px** (written in px, since `size-10` is only 36px at the 90% root).
    The dock may scroll horizontally on its toolbar axis; it never wraps.
- `prefers-reduced-transparency` or `glass` fallbacks keep the dock opaque enough for 4.5:1 labels.

### 9.2 `sidebar` (opt-in, for builders)

Collapsible spaces rail grouped by workspace or lens, never an alphabetical dump of tables or raw
schema names. Pairs with `data-density="compact"`. The top bar keeps its controls; breadcrumbs live
in the bar, not in the page.

### 9.3 `minimal`

Chromeless canvas for embedded or single-task surfaces (kiosk, OAuth, login). No dock, no sidebar;
the page still has a title and an honest exit.

---

## 10. Canonical primitives

One shared implementation each, in `@norn/ui` (TARGET) or Studio's `src/components/` today. Screens
compose these; they do not restyle them with ad-hoc classes.

| Primitive | Anatomy | Do | Don't |
| --- | --- | --- | --- |
| **PageHead** | `min-h-12 pt-6 pb-5`; `h1` (step 4, 500); a status (`Live`: pulsing dot + step 2 text in the status's text token); end side: page controls, at most one primary action, overflow menu. | One line of purpose under the title only when the page is not obvious. | Breadcrumbs in the page; two primary buttons; a status as a filled pill. |
| **ShellCard** | `card` rim, `--radius-panel`, `--panel-inset` padding, `--panel-shadow`. Header `h-11 ps-3.5 pe-3`: icon (muted, 1.6 stroke), title (step 2, 400), count (muted), end: control or "See all ›" (muted, chevron `rtl:rotate-180`). Body: one well, `flex-1`. Optional footnote in the well (step 1, muted). | Use for every grouped list, table, chart or prose block. | A card inside a well; a well without a card; a bordered box inside a well. |
| **StatWell** | ShellCard shape; label row `h-9 px-3.5` (step 2, 400, optional info glyph with a tooltip); well `px-4 pt-3.5 pb-3`, value (step 5, 400) with unit prefix at 0.54em muted, sub at the end (step 1, muted). | 2–4 in a band at the top of a page; numbers from the server only. | Coloured values; trend arrows in saturated colour; a value the cell did not return shown as `0`. |
| **ListRow** | Button, `h-12`, `--radius-row`, `px-3`, `gap-3`. Lead: 6px dot, 28px Initials disc or 28px TypeDisc. Title (step 2, 400), mono code (step 1, muted), SourceTag, end: muted metric or StatusDot. Hover `--well-hover`; selected `--card` + 1px `--panel-border` ring. | Open the DetailSheet on activate; keep the row's own accessible name = its title. | Filled status pills; zebra stripes; more than one end slot. |
| **DataTable** | Inside a ShellCard well: header row step 1 muted 400, hairline row dividers, `--density-cell` rows, tabular numbers, hover wash. | Only for genuinely dense, column-compared data (TanStack Table + Virtual). | As the default layout when a list → sheet fits. |
| **StatusDot** | 6px dot (`--dot-*`) + step 1–2 text in `--muted-foreground`. | Every record status, lifecycle state, freshness. | Colour-only status; coloured label text; icon + dot + pill together. |
| **SourceTag** | Mono, step 1, `h-[1.2rem]`, `--radius-chip`, `ring-1 --line`, no fill, muted text. | Source systems, rails, kinds, short codes. | A tinted or filled background; more than two per row. |
| **Initials / TypeDisc** | 28px (rows), 36px (top bar), 24px (menus); Initials uses the avatar tokens; TypeDisc is `card` + `ring-1 --line` + muted 15px icon. | Hash by id. | Photos without consent; hard-coded hex. |
| **Pill / PillButton** | `rounded-full`, `h-9`, `ring-1 --input` or `--well` fill for passive info pills, step 2. | Secondary actions, filters, top-bar controls. | Bold; coloured fills. |
| **PrimaryButton** | `rounded-full`, `h-9 px-4.5`, `--action` fill, `--action-foreground`, step 2 at 500, inset top highlight. | Exactly one per view or sheet. | A second solid button next to it; icon-only primary. |
| **SegmentedControl** | UI SDK; `rounded-full` track in `--well`, active segment `--card` + 500. | Branch/lens/period switches of ≤ 6 options. | Tabs styled as pills elsewhere. |
| **Search** | `--radius-search`, `h-12`, `card` + `--panel-border`, leading icon, `/` key hint chip. | One per page, top of content. | A search inside every card. |
| **Props** | A well, `p-1.5`; rows `min-h-10 gap-4 px-2.5 py-2.5`; key column `w-32` muted, value foreground, step 2. | Key/value facts in a sheet or refusal. | Bold keys; a table for two columns of facts. |
| **Section** | Label (step 2, muted) with optional end slot, then a well. | Grouping inside a sheet. | Headings with rules or uppercase. |
| **Menu / Popover** | UI SDK; `--popover`, `--radius-popup`, `--popup-shadow`, ring `--panel-border`, rows `rounded-[10px] px-2.5 py-2`. | Grouped radio items with a muted group label. | Nested menus more than one level. |
| **EmptyState** | 40px muted icon tile, title (step 2, 400), one sentence why, next step. | Always say what fills it and offer the action if the principal may take it. | "No data" with nothing else; a bold title. |
| **Canvas nodes** (`@xyflow/react`) | Studio's node elevation, 12px radius, accent selection ring, destructive failing ring. | Keep as is; weights follow section 8.2. | Bold node titles. |

**Box rule (replaces "zero box-in-box").** Allowed frames are: ShellCard/StatWell (with exactly one
well inside), popovers/menus/tooltips, DetailSheet and dialogs, the dock, hover/selection states of
rows, and dashed Blueprint outlines for uncommitted proposals. A frame inside a well, three levels of
borders, or a card inside a card is a defect. Open streams, spatial maps and canvases may still sit
directly on the canvas with no card.

**Banned patterns** (reject in review): alphabetical table sidebars; 600/700 headers or uppercase
banners; duplicate summary/metadata boxes that repeat the primary stream; raw internal jargon in
headers (`commit 412`, raw IR enum keys: translate through `src/lib/guard-words.ts` and keep raw
payloads in evidence drawers); Retool-style CRUD tables as the default layout; ad-hoc font sizes;
filled status pills.

---

## 11. List → sheet pattern

The default way to work with records: a list in a ShellCard; activating a row opens a floating
DetailSheet; the sheet's one primary action commits through a declared Norn action.

**DetailSheet (TARGET single primitive):**

- Built on the Base UI `Dialog` (focus trap, `inert` background, Escape, return focus to the row).
- Position: `fixed top-3 bottom-3 end-3` (12px inset on three sides), `w-[33rem]`,
  `max-w-[calc(100vw-24px)]`, `--radius-sheet`, `card` + `--panel-border`, `--sheet-shadow`.
  Below 640px it insets 8px on all sides and fills the width.
- Backdrop: `bg-black/[0.08]` + 1px blur light; `bg-black/50` dark. The page behind stays visible.
- Header `px-6 pt-5 pb-4`: lead disc (Initials/TypeDisc, 48px), title (step 3, 500), sub line
  (step 1, muted: mono id, SourceTag, time), close button (36px, round, `aria-label`).
- Optional meta row `px-6 pb-4`: passive info pills (`--well` + ring), at most four.
- Body `px-5 pb-5 gap-5`, scrolls: Sections, each a well:
  - **What Norn found:** Props of the server's facts.
  - **What changes (if you approve):** Before → After rows: label, old value muted with a
    `→` (mirrored in RTL), new value in the matching text status token. Comes from the server's
    dry-run, never computed in the browser.
  - **Receipt:** receipt id (mono, end of the section label) and one line on what commit does.
- Footer `px-5 py-4`, top hairline `--line`: secondary action(s) as PillButtons on the start side,
  **one PrimaryButton** on the end side. If the principal may not take the action, the cell's
  refusal replaces the button (section 12.3); the button is not shown disabled with no reason.
- The open sheet is addressable (URL search param) so reload, share and agents land on it.

Dialogs (confirmations, theme editor) are centred, `--radius-popup` + 8px, same shadow, same single
primary rule.

---

## 12. Honest states

Truth status is a server-only authority. Never infer "verified" from absent findings, empty warning
arrays or a 200 response. Precedence when several apply to one record, field or roll-up:

```text
Quarantined  >  Withheld  >  Imported (unverified)  >  Verified fact
```

Each is shown dot-first (`--dot-bad`, `--dot-quiet`, `--dot-held`, `--dot-live`) with its plain
label; the description lives in the tooltip and the sheet.

### 12.1 Loading (CURRENT)

`LoadState` with a `boneyard-js` skeleton (`bone="…"`, captured `.bones.json` geometry in
`src/bones/`, coloured with `--muted`/`--well`) → after a delay, a quiet "Still loading" note →
after the bound, an honest timed-out state with Retry. Nothing spins forever. Skeletons respect
`prefers-reduced-motion`.

### 12.2 Unavailable (CURRENT)

`WorkspaceUnavailable`: says the workspace or cell could not be reached, distinguishes retryable
from non-retryable failures, and offers Retry only when retrying can help. Never shows stale
numbers as current; stale data carries its "as of" time.

### 12.3 Refused and withheld (mandatory)

When the cell refuses a read or withholds fields:

- **Whole view refused:** the PageHead keeps its title; its status is a held `Live` ("Withheld by
  the cell"). The body is one ShellCard with a lock icon, a title naming what was withheld, one
  plain sentence on why, and Props: principal, marking, policy, receipt id. No half-rendered shell,
  no placeholder zeros, no blurred data.
- **Fields withheld:** the cell sends redacted placeholders on the wire; the UI renders them as
  `MaskedValue` (`data-masked`) with the marking in the tooltip. The browser never holds the real
  value to hide.
- **Action refused at dispatch:** the cell's reason replaces the primary action, in the sheet
  footer, with the policy name. Denied calls never enter an approval queue (`APPROVAL-1`); only
  authorized calls that hit an approval gate park for sign-off, shown as held.

### 12.4 Receipts (mandatory)

Every committed action returns a receipt; the UI shows it. After commit the sheet shows the receipt
id (mono), the Before → After that actually happened (from the server), and a link to lineage. The
top bar's **guarantee tick bar** (10 hairline ticks, filled `--dot-live` per satisfied guarantee,
`n/m` count, popover listing each guarantee in plain words and the current receipt) shows the
workspace's live laws. Its contents come from the server; the UI hard-codes none.

### 12.5 Empty

EmptyState (section 10). Say what will fill it and offer the next step only when the server says
the principal may take it.

---

## 13. Status and colour discipline

- Saturated colour appears only in: status dots, data bars and chart marks, avatar discs, the focus
  ring, the dock's attention dot, and the single primary CTA.
- Coloured **text** appears only for: a PageHead live line, signed money or Before → After values,
  and inline links. Always the AA text tokens (`--success`, `--warning`, `--destructive-text`,
  `--link`), never the dot tokens.
- Status text is `--muted-foreground` next to its dot. No tinted or filled pills for status, kind
  or source (SourceTag is outline only).
- Proportional visualizers are the preferred way to show shares: `h-2.5` rounded segmented bars
  with a dot legend (name, amount, share), and the top-bar tick bar.
- Large structural regions (canvas, shell, cards, wells, tables, chrome) stay neutral in every skin.

---

## 14. RTL and Arabic

- `dir` and `lang` on `<html>` from the locale; Base UI `DirectionProvider` follows it.
- **Logical properties only** in first-party code: `ms/me/ps/pe`, `start/end`, `inset-inline`,
  `border-s/e`, `text-start/end`, `rounded-s/e`. Physical `ml/mr/pl/pr/left/right/border-l/r` are
  defects outside vendored primitives (and those get fixed when touched).
- Directional icons (chevrons, arrows, "See all ›", Before → After arrows) flip with
  `rtl:rotate-180`. Motion that slides in from an edge flips too (`rtl:-translate-x-4`).
- `[dir="rtl"] :where(p, h1…h6, li, label, button, a, dt, dd, figcaption, span):not([dir])
  { unicode-bidi: plaintext }` (CURRENT, `LABEL-LOCALE-1`): untranslated English keeps its order
  inside an RTL page.
- Codes, ids, money, times and mono runs render inside `<bdi>` / `dir="ltr"` so `EMP-005`,
  `RC-74ceaaad` and `11-9` never reorder.
- **Digits:** Western (latn) digits for all data (codes, amounts, times, dates, counts) in both
  locales so tables align and copy matches. Arabic-Indic digits only in prose when the pack's
  locale asks. Never mix both in one view.
- **Font pairing:** Cairo for Arabic, matched to Geist's x-height; same weights (400/500).
- Tables, schedules and dock order mirror. Charts with a time axis keep time running in the reading
  direction of the locale.
- Every screen ships with an Arabic Playwright row and an RTL screenshot (section 16).

---

## 15. Motion

- Popups and menus: 150ms opacity + scale 0.95 from `--transform-origin`.
- Sheet: 150ms opacity + 16px inline-axis slide from its edge.
- Hover and selection: 150–200ms colour transitions; canvas nodes lift 1.5px with `--ease-spring`.
- Live indicators: one slow ping (2.4s) on the PageHead live dot only.
- Active canvas edges: slow dash flow.
- `prefers-reduced-motion: reduce` turns every transform, ping and dash animation off.

---

## 16. Agent operability and the quality gates

### 16.1 Named controls (TARGET)

- Every interactive element is a Base UI or UI SDK primitive (never a `div` with `onClick`), with a
  stable accessible name: its visible label, or `aria-label` for icon-only controls. Names are
  unique within a view.
- Every state-changing control is an `ActionTrigger` bound to a declared Norn action or transition.
  The Sidekick drives those same named controls through UI SDK `createAgent`; it gets no private
  endpoints and no extra authority, and the cell authorizes its calls exactly as a person's.
- Primitives carry `data-slot` names (`panel`, `well`, `kpi`, `list-row`, `detail-sheet`,
  `floating-dock`) so tests, skins and agents target structure, not class strings.
- Selection and open sheets live in the URL, so an agent and a person share state by link.

### 16.2 Gates (every PR touching UI)

1. **Domain-agnostic Studio** (`apps/studio/test/domain-words.test.ts`, CURRENT). No allowlist.
2. **No Radix** (`apps/studio/test/ui-primitives-guard.test.ts`, CURRENT).
3. **Contrast** (`apps/studio/test/contrast.test.ts`, `studio-theme.test.ts`, CURRENT; extend to
   the new tokens): across every skin × scheme, text ≥ 4.5:1 on its real surface (card **and**
   well, resting, hover, selected); figures at step 5 ≥ 3:1; focus rings and graph edges ≥ 3:1;
   `--action-foreground` on `--action` ≥ 4.5:1; avatar pairs ≥ 4.5:1. Dots are exempt only
   because they are always paired with a label; the gate checks that pairing.
   `--subtle-foreground` may not carry information.
4. **Touch:** interactive targets ≥ 44 × 44 CSS px below 640px; ≥ 32px elsewhere.
5. **Focus:** every control shows `focus-visible` outline 2px in `--ring` (dock: `--dock-ring`).
6. **Type lock (TARGET lint):** no arbitrary `text-[…]` sizes and no `font-semibold`/`font-bold` in
   first-party screens; `font-medium` only on the allowed elements.
7. **Browser proof on Boat** (CURRENT): Playwright rows (`bun run e2e`) for every visual or
   interaction change, plus rendered screenshots in light, dark, the active skin and RTL. Static
   markup tests never substitute for rendered proof. Every primitive state also renders on the
   canvas contact sheet (`src/sheet.tsx`, `sheet-states.ts`).
8. **Performance:** `defaultPreload: "intent"`, lazy route chunks, heavy tools (`@xyflow/react`,
   `recharts`, `shiki`, WASM checker) loaded on intent; LCP ≤ 2.5s, INP ≤ 200ms, CLS ≤ 0.1.

---

## 17. Architecture notes kept from Studio (CURRENT and TARGET)

- **3-layer checked UI contract.** (1) Typed UI DSL primitives (`OpenCanvas`, `MediaStage`,
  `Timeline`, `OpenStream`, `ControlDock`, `PromptComposer`, `SplitPane`, `DataGrid`,
  `ActionTrigger`), serializable to IR. (2) Server-only authorization and redaction: compile-time
  `norn-ui-checker` (Ring-1 parity extracted in `#1638`; manifest and spatial DSL TARGET), runtime
  cell-host Cedar + Markings evaluated **before serialization**, whole-screen lock when core reads
  are refused, dispatch-time authorization with `APPROVAL-1`. (3) Thin presentation: skin, shell,
  density and direction only. Custom renderers (TARGET) are sandboxed and receive only redacted
  props plus checked action callbacks.
- **Semantic Map** (`/map`, CURRENT read-only catalog explorer; marking-filtered projection and
  Ripple TARGET `#1664`): monochrome Islands and Lenses over one System-of-Record graph; colour
  only for live attention and highlighted causal links; Blueprint dashed overlays and
  `Before / Delta / After` diffs for uncommitted changes; committed changes ripple through the
  server-side trigger outbox (`#1650`) as `Stale` tasks, never silent drift or marking leaks.
- **Visual references** (inspiration, not copies): `getopen.so` (open canvas, dark pill dock),
  `polar.sh` (monochrome craft, carbon dark), The Weave (source-coloured provenance threads),
  Floorplan (MIT; semantic zoom, Blueprint overlays), Archify (Apache-2.0; `SRC` badges, diffs),
  `bilalgursen/liquid-glass` (MIT; optional single WebGL2 shader behind `glass`, CSS fallback
  mandatory).

### 17.1 Token diff: Studio `index.css` vs the HR wedge

Same token, different values. "Pick" is what this spec adopts.

| Skin / scheme | Token | Studio | Wedge | Pick | Why |
| --- | --- | --- | --- | --- | --- |
| all | default skin | `polar` | `open` | `open` | Owner chose the wedge look; `open` is still neutral surfaces with one accent, so colour discipline holds. `polar` stays one click away. |
| all | default shell | `sidebar` | dock | `dock` | Non-negotiable 5; Studio's own spec already named the dock the target default. Builders opt into `sidebar`. |
| open L | `--radius-panel` / `--radius-well` | 1.25rem / 0.9375rem (18 / 13.5px at 90%) | 20px / 15px | 20px / 15px | Non-negotiable 1; px so geometry does not scale with the root. |
| open L | `--action`, `--primary`, `--ring` | #2e60e0 | #3a6df0 | #2e60e0 | White on #3a6df0 is 4.54:1 (knife-edge) and #3a6df0 as text on the well is 4.16:1 (fails); #2e60e0 is 5.43:1. Accessibility is stricter. #3a6df0 stays as `--chart-1`. |
| open L | `--action-hover` | #2451c7 | #2d5de0 | #2451c7 | Follows the action pick. |
| open L | `--muted-foreground` | #5c5c63 | #6e6e73 | #6e6e73 | Wedge's calmer grey still passes (5.07:1 card, 4.65:1 well). |
| open L | `--subtle-foreground` | — | #a0a0a6 | #85858b, decorative only | #a0a0a6 is 2.39:1 on the well; wedge used it for counts. Counts move to muted; subtle keeps ≥ 3:1 for glyphs. |
| open L | `--success` | #1f7a4d | #1f9d61 | text #1f7a4d, dot #1f9d61 | Wedge green is 3.47:1 as text (fails). Split into text and dot tokens. |
| open L | `--warning` | #8a5600 | #e09a2b | text #8a5600, dot #e09a2b | Wedge amber is 2.38:1 as text. |
| open L | `--destructive` | #b83228 | #e5484d | fill/text #b83228, dot #e5484d | Wedge red is 3.91:1 with white. |
| open L | `--chart-3`, `--chart-4` | #b45309, #9a1ca6 | #e09a2b, #7c5cff | Studio's | Studio's set is AA-tuned for chart labels; wedge's amber fails 3:1. |
| open L | `--line`, `--action-dot`, `--action-soft`, `--action-fill`, `--popup-shadow`, `--sheet-shadow` | — | defined | wedge | New tokens Studio lacks. |
| open L | `--font-sans` | Geist only | Geist + Cairo | Geist + Cairo | RTL first-class. |
| open D | `--action` / `--action-foreground` | #6b98ff / #ffffff | #5b8cff / #ffffff | #2e60e0 / #ffffff | Both shipped pairs fail as a CTA (2.78:1 and 3.16:1). #2e60e0 with white is 5.43:1 and matches light. `--ring` #5b8cff and `--link` #7da4ff keep the bright blue for focus and links (6.1:1 on canvas). |
| open D | `--primary` | #2e60e0 | #5b8cff | #2e60e0 | Alias of action. |
| open D | `--destructive-text` | #f07b73 | #ff6b6f | #ff6b6f | Wedge passes (6.14:1 on well) and matches its dot. |
| open D | `--panel-shadow` | `inset 0 1px 0 rgb(255 255 255/.02)` | same | same | — |
| polar L | `--radius-panel` / `--radius-well` | 0.75rem / 0.5rem (10.8 / 7.2px) | 16px / 11px | 16px / 11px | Non-negotiable 1. |
| polar L | panel inset | 0 (flattened KPI and panel wells) | 4px double shell | 4px | Non-negotiable 1: the double shell is the only nesting pattern in every skin. |
| polar L | `--panel-shadow` | none | `0 1px 2px rgb(0 0 0/.02)` | wedge | A whisper of lift separates the rim from `#f6f6f8` canvas. |
| polar L | `--muted-foreground` | #585861 | #5c5c66 | #5c5c66 | Passes (6.18:1 on well); wedge wins. |
| polar L | `--subtle-foreground` | — | #9696a0 | #85858b | #9696a0 is 2.74:1 on the well. |
| polar L | `--card-foreground`, `--foreground` | #18181b | #18181b | same | — |
| polar D | `--card` | #161618 | #151517 | #151517 | Wedge; matches `open` dark card. |
| polar D | `--panel-border` | #26262b | #25252a | #25252a | Wedge. |
| polar D | `--panel-shadow` | inset .04 | inset .03 | .03 | Wedge. |
| polar D | `--subtle-foreground` | — | #686870 | #686870 | 3.11:1 on well; decorative only. |
| luma, glass | radii | 0.6875/0.5rem, 1/0.75rem | 20/15px (inherited) | 16/11px, 20/15px | Concentric rule; Studio's luma and glass palettes are more complete and stay. |
| all | avatar colours | — | hard-coded hex in `Initials` | `--avatar-n-*` tokens, hashed by id | Non-negotiable 6. |
| all | type sizes | `--text-meta/panel/section/title/figure` = 0.8125/0.9375/1.0625/1.75/2rem | ad-hoc 0.65–2.1rem | 0.84/1/1.35/1.85/2rem (section 8.1) | Non-negotiable 6; the wedge's visual sizes, locked to five. |

---

## 18. Conformance gaps in Studio today

Measured on `origin/main` at `ff0f95bd5e`. **Not fixed here**; the restyle lane owns them. Counts are
`grep` occurrences in `apps/studio/src`.

**Tokens and theming (`src/index.css`, `src/lib/appearance.tsx`, `src/lib/studio-theme.ts`)**

- [ ] Default skin is `polar` (`storedSkin`) and default shell is `sidebar` (`storedShellChrome`);
  spec: `open` + `dock`.
- [ ] Radii in rem (scale with the 90% root); `polar`, `luma`, `glass` radii break the concentric
  rule; `[data-skin="polar"] [data-slot="panel"] { padding: 0 }` and the polar KPI well override
  flatten the double shell.
- [ ] Missing tokens: `--subtle-foreground`, `--line`, `--action-dot`, `--action-soft`,
  `--action-fill`, `--dot-*`, `--avatar-*`, `--popup-shadow`, `--sheet-shadow`, `--panel-inset`,
  `--radius-row/sheet/search/dock/popup`; StudioTheme allowlist needs the new colour tokens.
- [ ] Open dark `--action` #6b98ff with `--action-foreground` #ffffff is 2.78:1.
- [ ] `--font-sans` has no Arabic pairing (no Cairo).
- [ ] Type tokens are 0.8125/0.9375/1.0625/1.75/2rem; body is 0.875rem.

**Weights and sizes**

- [ ] `font-semibold`/`font-bold`: 41 uses in 23 files. Most: `shell.tsx` (7),
  `decision-tree.tsx` (5), `imports/split-pane-review.tsx` (3), `components/canvas/node-card.tsx`
  (3), `views.tsx`, `sources.tsx`, `dashboards.tsx`, `canvas.tsx` (2 each), and
  `components/panel.tsx` (`EmptyState` title).
- [ ] `font-medium`: 346 uses; most are outside the allowed four. Heaviest:
  `imports/split-pane-review.tsx` (20), `shell.tsx` (18), `imports/supersession-diff.tsx` (18),
  `semantic-map.tsx` (17), `sources.tsx`, `objects.tsx`, `guarantees.tsx` (10 each);
  `components/chip.tsx` sets 500 on every chip.
- [ ] Arbitrary `text-[…]` sizes: 81 uses (`text-[0.6875rem]` ×42, `text-[11px]` ×14, `text-[10px]`,
  `text-[0.625rem]`, display sizes 2.75–4rem). Heaviest: `imports/split-pane-review.tsx` (14),
  `shell.tsx` (12), `semantic-map.tsx` (7), `imports/alias-matcher.tsx` (6),
  `imports/supersession-diff.tsx`, `decision-tree.tsx`, `components/canvas/node-card.tsx` (5 each).
- [ ] Uppercase chrome: `semantic-map.tsx` (5), `views.tsx`, `shell.tsx`, `guarantees.tsx`.

**Status and colour**

- [ ] Filled tinted status pills: `components/chip.tsx` (`Chip` fills every tone) used in 21 files;
  heaviest `widget/views.tsx` (18), `learning/proposals-page.tsx` (15), `laws.tsx` (9),
  `imports/split-pane-review.tsx` (9), `learning/cases-page.tsx` (8), `learning/misfit-page.tsx`,
  `objects.tsx`, `learning/episodes-page.tsx`, `imports/supersession-diff.tsx`,
  `evidence/evidence-crop.tsx`. Status → `StatusDot`; kinds and sources → outline `SourceTag`.
- [ ] `components/fact-status-badge.tsx` renders the honesty states as filled chips; needs dot-first
  with the same precedence.
- [ ] shadcn `Badge` as status/kind: `transforms.tsx` (8), `sheet.tsx` (8), `views.tsx` (6),
  `semantic-map.tsx` (6), `workflows.tsx` (5), `sources.tsx` (4), `dashboards.tsx` (3).

**Nesting and primitives**

- [ ] `components/panel.tsx`: `Well` is a flush `border-t` body, not an inset well; nested panels
  turn into ruled sections. Align to ShellCard (one inset well).
- [ ] `components/table-card.tsx`: adds its own rim + well when not inside a Panel (a second
  nesting pattern).
- [ ] `components/kpi-tile.tsx`: close to StatWell; label uses `text-sm`, no unit slot.
- [ ] shadcn `Card` instead of ShellCard: `login.tsx`, `oauth.tsx`, `restatement.tsx`,
  `widget/views.tsx`.
- [ ] No shared `ListRow`, `StatusDot`, `SourceTag`, `Initials`, `Props`, `Section` primitives; lists
  are built per screen.

**Sheets**

- [ ] `components/ui/sheet.tsx` is edge-attached, full height, `w-3/4`, physical
  `left/right/border-l/border-r`; no 12px inset, no 24px radius, no single-CTA footer. Used by
  `answer-view.tsx`, `assistant-slot.tsx`, `dashboards.tsx`, `actions/action-sheet.tsx`,
  `decisions/decision-sheet.tsx`, `components/provenance.tsx`, `questions.tsx`,
  `dashboards/tile-view.tsx`, `objects.tsx`, `grid/records-grid.tsx`, `learning/cases-page.tsx`,
  `lib/inspector.tsx`. Widths vary (`sm:max-w-lg` to `sm:max-w-4xl`). Replace with one DetailSheet.

**Shell and dock (`shell.tsx`)**

- [ ] `FloatingDock` is icon-only at every width; no labels below 640px; buttons are `size-10`
  (36px at the 90% root), under the 44px target.
- [ ] Dock attention dot uses physical `right-[7px]`.
- [ ] Dock-mode top bar (`data-slot="dock-topbar"`) is borderless but `h-[4.4rem]` on
  `bg-background`, and has no guarantee tick bar, language pill or scheme toggle.
- [ ] The top-bar Notifications popover renders a hard-coded "Nothing yet." instead of a
  server-backed list or an honest unavailable state.

**RTL**

- [ ] Physical spacing in first-party code: `actions/action-form.tsx` (3),
  `learning/episodes-page.tsx`, `learning/episode-page.tsx`; vendored `components/ui/*`
  (`dropdown-menu`, `button`, `toggle`, `input-group`, `sidebar`, `sheet`) when touched.
- [ ] No Arabic font; no `<bdi>` discipline for ids and money.

**Agent operability**

- [ ] No UI SDK `createAgent` wiring; the Sidekick (`sidekick/client.ts`) does not drive named
  controls. List rows have no `data-slot`.

**Gates**

- [ ] Contrast gate does not yet cover the new tokens (subtle, dot pairing, avatars,
  action-foreground on action in dark).
- [ ] No lint for arbitrary type sizes, banned weights or physical spacing.
