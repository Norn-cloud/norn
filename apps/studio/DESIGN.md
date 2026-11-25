# Norn Design System

This is the one design and UI engineering contract for every Norn UI: Studio (`apps/studio/`), the
apps compiled and rendered through `@norn/ui`, first-party apps built on Norn, and custom or
AI-generated Norn apps. Every new screen, primitive, skin or visual change must
conform. Where a rule here and a screen disagree, the screen is wrong; the gap list at the end
(section 18) tracks the known ones.

The spec merges two sources. Studio's contract supplies the architecture: server-driven UI with no
client authority, Base UI + Vercel UI SDK primitives, Geist, `boneyard-js` skeletons, the StudioTheme
JSON and the honesty rules. The reference first-party app (private, built on Norn, descended from
the v6 mockups) supplies the look: the owner judged it better. **Where the two conflict, the
first-party app's pattern wins, unless Studio's rule is stricter about authority, honesty,
accessibility or RTL.** Each such call is recorded where it is made, and in the token diff
(section 17).

Status words used below: **CURRENT** means shipped in `apps/studio` today (and, where a gate is
named, enforced by it); **TARGET** means required for new work but not yet built, and tracked as a
gap in section 18. A section marked CURRENT may contain TARGET items; those are marked inline. A
TARGET rule is never described as shipped.

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
7. **Agents use the same controls as people.** Every control is a named, accessible primitive, so
   the Sidekick and automation drive exactly what a person drives. Controls that change domain
   state go through a declared Norn action that the cell authorizes; local view controls (scheme,
   skin, navigation, opening a sheet) stay local state or URL state and are not cell actions
   (section 16.1).
8. **Domain-agnostic Studio.** `apps/studio/src/` never names a business domain. Domain words live
   in packs (`registry/packs/`) and fixtures; first-party apps carry their own vocabulary.

---

## 2. Stack (CURRENT unless marked)

| Layer | Choice | Rule |
| --- | --- | --- |
| Runtime | React 19 | React DOM is the renderer. Alternatives need measured Boat proof (LCP, INP, portals). |
| Behaviour | Vercel UI SDK (`@ui-sdk/core`, `@ui-sdk/react`, `@ui-sdk/web`, Apache-2.0; pinned exactly at `0.1.0-beta.17` in `apps/studio/package.json`) | Menu, Popover, Toolbar, Tooltip, SegmentedControl, ChromeDock; agent control (`createAgent`, TARGET). |
| Headless primitives | Base UI (`@base-ui/react`, MIT) | Dialog (sheets, modals), Popover, Tabs, Toggle, DirectionProvider. |
| Styling | Tailwind v4 + CSS custom properties | `@layer ui-sdk;` first, then Tailwind; tokens via `@theme inline`. No CSS-in-JS, no Panda. |
| Icons | `lucide-react` | 1.6–1.7 stroke; size from the type step they sit beside. |
| Fonts | Geist Variable, Geist Mono Variable (CURRENT); Cairo Variable for Arabic (**TARGET, not yet installed**) | Self-hosted via `@fontsource-variable/*`. |
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
| `data-shell` | `dock` (TARGET default; `sidebar` is today's default), `sidebar` | Frame chrome and navigation geometry only (section 9). A user preference, persisted per browser. |
| *(no attribute)* `minimal` | route-level, not a `data-shell` value | Chromeless routes (login, OAuth, embeds) render outside the shell (section 9.3). A stored preference can never select it. |
| `data-density` | `comfortable` (default), `compact` | Row heights (`--density-*`); `compact` for builder views. |
| `dir`, `lang` | `ltr` / `rtl`, BCP 47 | Direction and font pairing (section 14). |

**Default skin is `open`** (TARGET; the first-party app's default and the look the owner chose). `polar` is the
first-class monochrome alternative. `luma` and `glass` remain supported skins and follow the same
geometry rules. This replaces Studio's earlier `polar` default; see section 17 for why.

**StudioTheme JSON (CURRENT, `src/lib/studio-theme.ts`).** Operators and tenants may supply a
declarative theme (`version: 1`, `baseSkin`, `appearance`, `accent`, `canvas`, `colors`,
`variants.light` / `variants.dark`) that overrides only the allowlisted semantic tokens
(`STUDIO_THEME_TOKENS`). Themes carry zero logic: no queries, permission checks or layout forks, and
never geometry, z-index or arbitrary CSS. Every theme is sanitized against CSS injection and must
pass the contrast gate (section 16) before it applies. New tokens this spec adds that are colours
(`subtle-foreground`, `line`, `action-dot`, `action-soft`, the dot and avatar tokens) join the
allowlist (TARGET); geometry tokens never do.

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
| `--muted-foreground` | #6a6a6f | #9b9ba3 | #5c5c66 | #9d9da6 | Secondary text, counts, status labels, metrics. |
| `--subtle-foreground` | #85858b | #6a6a72 | #85858b | #686870 | **Decorative only:** separators (`×`, `·`), info glyphs, chevrons. Never the only carrier of information. |

### 4.2 Action: fill and text are separate roles

The CTA **fill** and the accent **text** are different tokens, because one blue cannot be both in a
dark scheme (open dark #2e60e0 carries white at 5.43:1 but is only 3.13:1 as text on the well). This
is the split Studio's CSS and the StudioTheme validator already use (`studio-theme.ts` checks
`action` as text on background, card and popover, and `primary-foreground` on `primary` and on
`action-hover`), so the spec follows the code rather than the reverse.

| Role | Token | open L | open D | polar L | polar D | luma L | luma D |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CTA fill | `--primary` | #2e60e0 | #2e60e0 | #18181b | #f4f4f5 | #554ce8 | #6258ec |
| CTA hover fill | `--action-hover` | #2451c7 | #2451c7 | #2c2c32 | #e4e4e7 | #4940dc | #695ff0 |
| CTA ink | `--primary-foreground` (= `--action-foreground`) | #ffffff | #ffffff | #fafafa | #141416 | #ffffff | #ffffff |
| Accent text, accent borders | `--action` | #2e60e0 | #6b98ff | #18181b | #f4f4f5 | #554ce8 | #7c74ff |
| Inline link | `--link` | #2451c7 | #7da4ff | #18181b | #e4e4e7 | #4f46e5 | #938cff |
| Focus ring | `--ring` | #2e60e0 | #5b8cff | #52525b | #a1a1aa | #554ce8 | #7c74ff |
| Accent dot | `--action-dot` | #7f9ef4 | #6f97ff | #71717a | #a1a1aa | `--action` | `--action` |
| Non-text wash | `--action-soft` | rgb(58 109 240 / .10) | rgb(91 140 255 / .14) | rgb(24 24 27 / .08) | rgb(244 244 245 / .10) | `--action` @ 10% | `--action` @ 14% |

- **Solid fills use `--primary`, never `--action`.** `bg-action` with `--action-foreground` is a
  defect in dark schemes (white on open-dark #6b98ff is 2.78:1; on luma-dark #7c74ff 3.62:1).
- **`--action` is text.** It must reach 4.5:1 on canvas, card, well, well-hover and popover.
- **`--action-soft` carries no text.** It is for drop targets, canvas selection halos and chart
  highlights only: muted text on it falls below 4.5:1 (4.35:1 on the open-light well). Selected
  rows use `--card` + ring (section 10); the active segment uses `--card` (section 10). There is
  no `--action-fill` token.
- **`glass`** sets no action tokens, so it inherits `open`'s row for its scheme. Its translucent
  surfaces are measured composited over its `--canvas` (#eef1f8 / #0b0d17); the decorative
  radial tint must not be used to lift a failing pair. A StudioTheme with `baseSkin: "glass"`
  starts from the same inherited palette and passes the same validator.
- `--knot` (#5b52f0 / #7c74ff) is reserved for the Norn mark.

Measured pairs, reproducible from the values in this section and in `src/index.css`:

- **Formula:** WCAG 2.x relative luminance and contrast ratio, on sRGB hex.
- **Compositing:** a translucent value (`rgb(… / a)`) is composited over the opaque colour behind
  it, rounding each channel to 8 bits at every step (the `mix` in `test/contrast.test.ts`). Layer
  order, back to front:
  - canvas: `--canvas` (opaque in every skin);
  - card: `--card` over canvas;
  - well: `--well` over card over canvas (a well is always inset in a card);
  - well-hover: `--well-hover` over well over card over canvas (a hovered row inside a well);
  - popover: `--popover` over canvas;
  - dock: `--dock-surface` over canvas.
- **Surface sets:** text roles (`--action`, `--link`, `--foreground` as figure, `--muted-foreground`
  as unit) take the minimum over canvas, card, well, well-hover and popover; `--ring` over canvas,
  card and well; `--dock-indicator` on the dock; CTA ink on its two fills.
- **Palettes:** `open` and `polar` use section 4.1. `luma` and `glass` use their `index.css`
  surfaces and inherit `open`'s text tokens from section 4.1 (`--foreground`, `--muted-foreground`)
  and, for `glass`, its action row. Their composited surfaces:

  | Skin / scheme | canvas | card | well | well-hover | popover | dock |
  | --- | --- | --- | --- | --- | --- | --- |
  | luma L | #f3f4f6 | #fafafb | #fafafb | #f1f2f5 | #ffffff | #1d1d20 |
  | luma D | #0b0b0e | #17171d | #17171d | #1d1d24 | #1f1f27 | #222226 |
  | glass L | #eef1f8 | #f9fafc | #f6f7fc | #eef2fa | #fdfdfe | #3f414a |
  | glass D | #0b0d17 | #131623 | #1a1e2f | #23283e | #181c2d | #111320 |

Floors: 4.5:1 text, 3:1 ring and indicator. Each minimum names the surface that sets it
(c canvas, k card, w well, h well-hover, p popover).

| Skin / scheme | CTA ink on fill | on hover fill | `--action` text | `--link` | `--ring` | `--dock-indicator` on dock | figure / unit |
| --- | --- | --- | --- | --- | --- | --- | --- |
| open L | 5.43 | 6.83 | 4.68 h | 5.88 h | 4.98 w | 6.05 | 14.82 h / 4.64 h |
| open D | 5.43 | 6.83 | 5.64 h | 6.43 h | 5.38 w | 5.71 | 13.27 h / 5.67 h |
| polar L | 16.97 | 13.29 | 15.41 h | 15.41 h | 7.16 c | 15.30 | 15.41 h / 5.75 h |
| polar D | 16.74 | 14.50 | 14.42 h | 12.49 h | 6.70 w | 14.42 | 15.19 h / 5.89 h |
| luma L | 5.86 | 6.88 | 5.23 h | 5.62 h | 5.32 c | 6.05 | 15.36 h / 4.81 h |
| luma D | 5.09 | 4.67 | 4.52 p | 5.79 p | 4.93 k | 5.71 | 13.87 p / 5.93 p |
| glass L | 5.43 | 6.83 | 4.80 c | 6.04 c | 4.80 c | 3.66 | 15.21 c / 4.76 c |
| glass D | 5.43 | 6.83 | 5.23 h | 5.97 h | 5.23 w | 6.64 | 12.32 h / 5.27 h |

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
  Hard-coded hex in components (the first-party app's `DISC_BG`/`DISC_FG`) is replaced by these tokens.

### 4.5 Dock

`--dock-surface` #1d1d20 / #222226, `--dock-well` #34343a / #3a3a41, `--dock-border`
rgb(255 255 255 / .08), `--dock-foreground` #ffffff, `--dock-muted` #b4b4bc (8.16:1 / 7.70:1 on the
light- and dark-scheme dock, 4.93:1 on the glass-light composite #3f414a, 8.96:1 on glass dark;
shipped `index.css` still uses #a5a5ad, which is 4.15:1 on glass light: see section 18), `--dock-ring` #93b4ff. The dock is dark in every skin and scheme;
`glass` makes it translucent (rgb(24 26 36 / .82) / rgb(18 20 34 / .80)).

`--dock-indicator` is its **own token**, never an alias of `--action`, because the dock is dark
even in light schemes (polar-light `--action` #18181b on the dock is 1.05:1). Values (CURRENT in
`index.css`): #6b98ff in `open`, `luma` and `glass`; #f4f4f5 in `polar`. It must reach 3:1 on the
actual dock surface (`apps/studio/test/contrast.test.ts`, CURRENT); see the table in 4.2.

---

## 5. Radii and concentric geometry

Radii are **pixel** values, not rem: geometry must not change with the 90% root scale.

| Token | open | polar | luma | glass | Used by |
| --- | --- | --- | --- | --- | --- |
| `--radius-panel` | 20px | 16px | 16px | 20px | ShellCard, StatWell outer rim. |
| `--panel-inset` | 4px | 4px | 4px | 4px | Padding between rim and well. |
| `--radius-well` | 15px | 11px | 11px | 15px | Inner well. |
| `--radius-row` | 11px | 11px | 11px | 11px | ListRow and menu rows (one value). |
| `--radius-sheet` | 24px | 24px | 24px | 24px | DetailSheet. |
| `--radius-search` | 18px | 18px | 18px | 18px | Page search field. |
| `--radius-dock` | 26px | 26px | 26px | 26px | Floating dock. |
| `--radius-popup` | 16px | 16px | 16px | 16px | Menus, popovers. |
| `--radius` | 8px | 8px | 8px | 8px | Form inputs, code blocks. |
| `--radius-chip` | 5px | 5px | 5px | 5px | SourceTag. |
| pills | full | full | full | full | Top-bar controls, every button, segmented controls. |

Radii, geometry tokens and every value in sections 5, 9 and 11 written in px are TARGET until the
restyle lane lands (section 18); today's radii are rem.

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
  `@xyflow/react` pointer maths). Tailwind's spacing scale is rem-based, so `3` is 10.8px and `4`
  is 14.4px here. Anything that must be an exact pixel size (touch targets, radii, dots, the sheet
  inset, slide distances) is written as an arbitrary px value (`top-[12px]`, `min-h-[44px]`).
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
| 5 | `--text-figure` | 2rem | 28.8 | 1 | -0.025em | 400 | StatWell values, hero numbers. The unit beside a figure is step 2 (`--text-body`), muted, `tracking-normal`. |

**No other sizes.** No `text-[0.88rem]`, `text-[11px]`, `text-xs`/`text-sm` drift in first-party
screens. Mono runs use step 1 or 2 in the mono family with `letter-spacing: -0.01em`. Canvas edge
labels and minimap text are the one exception (they scale with the canvas zoom) and stay inside
`index.css`. Figure units are not an exception: they use step 2. Migration: Studio's `--text-panel` maps to `--text-body`, `--text-section` to
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

### 9.1 `dock` (TARGET default for operators and every app surface)

- **Top bar:** `h-[4.5rem]`, **no bottom border**, no background change from the canvas. Start
  side: Norn mark (32px), then a workspace pill (`rounded-full`, `h-9`, 24px org disc, name,
  chevron) whose menu holds workspace, access preview (where the server grants it) and skin. End
  side, in order: the guarantee tick bar pill (section 12.4), language toggle pill, scheme toggle
  (icon pill), the principal's avatar disc (36px).
- **Floating dock:** centred, `fixed bottom-6`, `rounded-[26px]`, `--dock-surface`, `p-1.5`,
  `gap-1`, ring `--dock-border`, `--dock-shadow`. Built on UI SDK `Toolbar` with
  `aria-label="<app> navigation"`. Contents: up to six destinations, a hairline separator, the
  assistant trigger, the appearance tray. More than six destinations go behind a "More" menu.
  - Destination: 40px round button (`size-[40px]`), 18px icon, `aria-label` = its name,
    `aria-current="page"` when active, active fill `--dock-well`. Attention marker: 6px
    `--dock-indicator` dot at `top-[7px] end-[7px]` with a 2px `--dock-surface` ring. The dot is
    never the only signal: the destination's accessible name carries the state too (TARGET). The
    name is one complete `aria-label` that includes the state, for example
    `aria-label="Inbox, 3 need attention"` (or `"Inbox, new activity"` when there is no count),
    and the tooltip shows the same text. Do not extend an explicit `aria-label` with a hidden
    child (the label overrides it) or with `aria-description` (that is a description, not the
    name).
  - ≥ 640px: icon-only with a top tooltip carrying the label.
  - **< 640px: every destination shows its label** under the icon (`--text-meta`) and every target
    is at least **44 × 44 CSS px** (written in px, since `size-10` is only 36px at the 90% root).
    The dock may scroll horizontally on its toolbar axis; it never wraps.
- `prefers-reduced-transparency` or `glass` fallbacks keep the dock opaque enough for 4.5:1 labels; with
  `--dock-muted` #b4b4bc the glass-light composite (#3f414a) holds 4.93:1 (TARGET until `index.css` adopts it).

### 9.2 `sidebar` (opt-in, for builders)

Collapsible spaces rail grouped by workspace or lens, never an alphabetical dump of tables or raw
schema names. Pairs with `data-density="compact"`. The top bar keeps its controls; breadcrumbs live
in the bar, not in the page.

### 9.3 `minimal` (route-level, TARGET)

Chromeless canvas for embedded or single-task surfaces (kiosk, OAuth, login). It is **not** a
`data-shell` value: the route decides it and renders outside the shell, so a stored shell
preference can never strip the chrome from an operator view or add it to a login page. No dock, no
sidebar; the page still has a title and an honest exit.

---

## 10. Canonical primitives

One shared implementation each, in `@norn/ui` (TARGET) or Studio's `src/components/` today. Screens
compose these; they do not restyle them with ad-hoc classes.

| Primitive | Anatomy | Do | Don't |
| --- | --- | --- | --- |
| **PageHead** | `min-h-12 pt-6 pb-5`; `h1` (step 4, 500); a status (`Live`: pulsing dot + step 2 text in the status's text token); end side: page controls, at most one primary action, overflow menu. | One line of purpose under the title only when the page is not obvious. | Breadcrumbs in the page; two primary buttons; a status as a filled pill. |
| **ShellCard** | `card` rim, `--radius-panel`, `--panel-inset` padding, `--panel-shadow`. Header `h-11 ps-3.5 pe-3`: icon (muted, 1.6 stroke), title (step 2, 400), count (muted), end: control or "See all ›" (muted, chevron `rtl:rotate-180`). Body: one well, `flex-1`. Optional footnote in the well (step 1, muted). | Use for every grouped list, table, chart or prose block. | A card inside a well; a well without a card; a bordered box inside a well. |
| **StatWell** | ShellCard shape; label row `h-9 px-3.5` (step 2, 400, optional info glyph with a tooltip); well `px-4 pt-3.5 pb-3`, value (step 5, 400) with its unit at step 2 muted, sub at the end (step 1, muted). | 2–4 in a band at the top of a page; numbers from the server only. | Coloured values; trend arrows in saturated colour; a value the cell did not return shown as `0`. |
| **ListRow** | Button, `h-12` (`min-h-[44px]` below 640px), `--radius-row`, `px-3`, `gap-3`. Lead: 6px dot, 28px Initials disc or 28px TypeDisc. Title (step 2, 400), mono code (step 1, muted), SourceTag, end: muted metric or StatusDot. Hover `--well-hover`; selected `--card` + 1px `--panel-border` ring. | Open the DetailSheet on activate; keep the row's own accessible name = its title. | Filled status pills; zebra stripes; more than one end slot. |
| **DataTable** | Inside a ShellCard well: header row step 1 muted 400, hairline row dividers, `--density-cell` rows, tabular numbers, hover wash. | Only for genuinely dense, column-compared data (`@tanstack/react-virtual`). | As the default layout when a list → sheet fits. |
| **StatusDot** | 6px dot (`--dot-*`) + step 1–2 text in `--muted-foreground`. | Every record status, lifecycle state, freshness. | Colour-only status; coloured label text; icon + dot + pill together. |
| **SourceTag** | Mono, step 1, `h-[1.2rem]`, `--radius-chip`, `ring-1 --line`, no fill, muted text. | Source systems, rails, kinds, short codes. | A tinted or filled background; more than two per row. |
| **Initials / TypeDisc** | 28px (rows), 36px (top bar), 24px (menus); Initials uses the avatar tokens; TypeDisc is `card` + `ring-1 --line` + muted 15px icon. | Hash by id. | Photos without consent; hard-coded hex. |
| **Pill / PillButton** | `rounded-full`, `h-9` (`min-h-[44px]` below 640px when interactive), `ring-1 --input` or `--well` fill for passive info pills, step 2. | Secondary actions, filters, top-bar controls. | Bold; coloured fills. |
| **PrimaryButton** | `rounded-full`, `h-9 px-4.5` (`min-h-[44px]` below 640px), `--primary` fill, `--action-hover` on hover, `--primary-foreground` ink, step 2 at 500, inset top highlight. | Exactly one per view or sheet. | A second solid button next to it; icon-only primary. |
| **SegmentedControl** | UI SDK; `rounded-full` track in `--well`, active segment `--card` + 500. | Branch/lens/period switches of ≤ 6 options. | Tabs styled as pills elsewhere. |
| **Search** | `--radius-search`, `h-12`, `card` + `--panel-border`, leading icon, `/` key hint chip. | One per page, top of content. | A search inside every card. |
| **Props** | A well, `p-1.5`; rows `min-h-10 gap-4 px-2.5 py-2.5`; key column `w-32` muted, value foreground, step 2. | Key/value facts in a sheet or refusal. | Bold keys; a table for two columns of facts. |
| **Section** | Label (step 2, muted) with optional end slot, then a well. | Grouping inside a sheet. | Headings with rules or uppercase. |
| **Menu / Popover** | UI SDK; `--popover`, `--radius-popup`, `--popup-shadow`, ring `--panel-border`, rows `--radius-row` `px-2.5 py-2`. | Grouped radio items with a muted group label. | Nested menus more than one level. |
| **EmptyState** | 40px muted icon tile, title (step 2, 400), one sentence why, next step. | Always say what fills it and offer the action if the principal may take it. | "No data" with nothing else; a bold title. |
| **Form input** | `--radius`, `h-9` (`min-h-[44px]` below 640px), 1px `--input` border, `--card` fill, visible label above. | The `--input` hairline is 1.27–1.43:1, below the 3:1 non-text floor, so an essential input also carries a second boundary cue (a `--well` fill against `--card`, or its label and a 3:1 focus ring). | A hairline as the only way to find the field. |
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
- Position: `fixed top-[12px] bottom-[12px] end-[12px]` (exactly 12px on three sides; `top-3` would
  be 10.8px at this root), `w-[33rem]`, `max-w-[calc(100vw-24px)]`, `--radius-sheet`, `card` +
  `--panel-border`, `--sheet-shadow`. Below 640px it uses `inset-[8px]` and fills the width.
- Backdrop: `bg-black/[0.08]` + 1px blur light; `bg-black/50` dark. The page behind stays visible.
- Header `px-6 pt-5 pb-4`: lead disc (Initials/TypeDisc, 48px), title (step 3, 500), sub line
  (step 1, muted: mono id, SourceTag, time), close button (`size-[36px]`, round, `aria-label`;
  `min-h-[44px] min-w-[44px]` hit area below 640px).
- Optional meta row `px-6 pb-4`: passive info pills (`--well` + ring), at most four.
- Body `px-5 pb-5 gap-5`, scrolls: Sections, each a well:
  - **What Norn found:** Props of the server's facts.
  - **What changes (if you approve):** Before → After rows: label, old value muted with a
    `→` (mirrored in RTL), new value in the matching text status token. Rendered only when the
    server supplies a dry-run (TARGET); never computed in the browser. With no dry-run the section
    is omitted, not filled with a guess.
  - **Before dispatch** there is no receipt: the sheet states in one line what commit will do.
    The receipt section appears only after the server returns a `receipt` (section 12.4).
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

A refusal renders **only what the server sent**. The wire type is `Refusal`
(`crates/norn-api/src/refusal.rs`, `norn/refusal@2`; typed in the SDK by #1686). Studio's `refused`
arm of `CallResult` (`src/lib/actions.ts`) is a **lossy projection** of it today; see "Studio
today" below. The wire fields split into what every refusal carries and what only some disclose:

| Field | Always sent | Renders as |
| --- | --- | --- |
| `message` | yes | The card or footer title, verbatim. |
| `code` | yes | Mono, step 1, muted, after the title (`scope.denied`). |
| `category` | yes | (TARGET in Studio) Chooses the icon and next step: `visibility` / `permission` → lock, no retry; `conflict` / `precondition` → reload; `limit` → wait; `invalid` → fix the input; `internal` → incident. |
| `retryable` | yes | (TARGET in Studio) Shows Retry only when `true`. |
| `explanation` | yes (may be generic) | One plain sentence under the title. |
| `reason` (law, subject, values, pin) | optional (null when not disclosed) | A "What refused" Props section: the law's name, the subject, and the checked values by path. |
| `diagnostics` | optional (absent when empty) | A list of findings, each with its code and pointer. |
| `incidentId` | optional (internal refusals) | Mono, with copy, in the footer line. |
| `decidedAt` (Studio call path) | optional | "Refused at <time>" in the sub line. |

**Studio today (CURRENT; gap in section 18).** `refusalOf` in `src/lib/actions.ts` keeps `code`,
`message`, `diagnostics` and the optional `explanation`, `reason`, `decidedAt` and `incidentId`. It
**drops `category` and `retryable`**, and types `explanation` as optional although the wire always
sends it. `src/lib/screen/call.ts` copies the same fields into its `refused` phase. The catalog read
(`listActions`) keeps only `status`, `code` and `message`. So a renderer fed by `CallResult` cannot
draw the category icon or the server-directed Retry yet. **TARGET:** both arms and the `refused`
phase carry `category` and `retryable` verbatim and `explanation` as required. Until then a
renderer shows no category icon and no Retry on a refusal, and never infers either from the HTTP
status or the code.

**Absent sections render nothing:** no "Policy: —", no "unknown", no placeholder, no text the
browser composed to sound specific. A bare opaque denial (the one read denial in
`crates/norn-api/src/authz.rs`, the same document whatever was denied) is a complete, valid
rendering: title, code, explanation, and no Props. The refusal does not carry a principal, policy
name, marking or receipt id, so the UI never shows those as if it did. The signed-in principal may
appear in the top bar as usual, never inside the refusal as evidence.

- **Whole view refused:** the PageHead keeps its title; its status is a held dot with the label
  "Withheld by the cell". The body is one ShellCard with a lock icon and the refusal above. No
  half-rendered shell, no placeholder zeros, no blurred data.
- **Fields withheld (CURRENT, `components/masked-value.tsx`):** the cell sends projected
  placeholders on the wire; the UI renders them as `MaskedValue` with the projection word
  (`masked`, `pseudonymised`, `withheld`) and the markings **when the projection lists any**. The
  browser never holds the real value to hide.
- **Action refused at dispatch:** the refusal replaces the primary action in the sheet footer:
  `message`, then `reason` when sent. Denied calls never enter an approval queue (`APPROVAL-1`);
  only authorized calls that hit an approval gate park for sign-off, shown as held.

### 12.4 Call outcomes and receipts (mandatory)

A dispatched action resolves to exactly one arm of `CallResult` (CURRENT, `src/lib/actions.ts`,
reduced by `src/lib/screen/call.ts`). Each arm has one rendering:

| Arm | Wire | Renders as |
| --- | --- | --- |
| `receipt` | `ActionReceipt`: `outcome` (`committed` / `replayed`), object type, id, version, and `receipt` { `receiptHash`, `commitSeq`, `recordedAtMs` } | Live dot, "Recorded", the receipt hash (mono, shortened, full value in copy and tooltip), the commit sequence and time. `replayed` says "Already recorded" and shows the earlier receipt: nothing new was committed. Undo only when the server offers it. |
| `pending` | HTTP 202, `decision` { `id`, `expiresAtMs` } | Held dot, "Sent for approval", the decision id (mono) and its expiry. Nothing committed; no receipt is shown. |
| `refused` | `Refusal` (section 12.3) | The refusal replaces the primary action. |
| `no-cell` | no cell is connected | An unavailable state that says no cell is connected and how to start one (`norn dev`); no Retry loop. |
| `unavailable` | **every** HTTP 404 or 503 from the action route (CURRENT). `callAction` parses the body but the status decides the outcome, so a 404 or 503 that carries a typed `Refusal` is also reported as `unavailable` and its refusal is dropped; `listActions` does the same for the catalog. TARGET: a 404 or 503 whose body is a `Refusal` resolves to `refused` (section 18). | CURRENT: "This cell doesn't take actions from Studio yet.", no Retry, for both statuses. TARGET (section 18): an untyped **404** keeps that copy with no Retry (the route is absent); an untyped **503** is a temporary outage: "The cell is temporarily unavailable." with Retry (section 12.2), never the unsupported-route copy. |
| (thrown) | network failure, a malformed 202, or another non-2xx whose body is not a `Refusal` (`CellUnavailable`); reduced to `failed` with reason `error` | "Couldn't reach the cell", with Retry. |

- **One idempotency key per user intent.** The draft holds one `idempotencyKey`; a retry of the same
  intent resends the same key (so a lost response replays instead of committing twice); editing the
  draft mints a new key. Never one key per click.
- **Development runs against a real cell.** `norn dev` serves a project's own SQLite cell on
  loopback; screens are proven against it, never against hand-written mock responses.
- After commit the sheet shows the receipt, the Before → After that actually happened (when the
  server returns it), and a link to lineage.

The top bar's **guarantee tick bar** (TARGET: 10 hairline ticks, filled `--dot-live` per satisfied
guarantee, `n/m` count, popover listing each guarantee in plain words and the current receipt) shows
the workspace's live laws. Its contents come from the server; the UI hard-codes none.

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
- Chart **labels** (axis text, value labels, legends) use text tokens (`--foreground`,
  `--muted-foreground`), never a series colour: `--chart-3` light is 4.33:1 on well-hover and
  `--chart-5` 3.75:1 on white. Series marks themselves need 3:1 against their surface.
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
- Codes, ids, money, times and mono runs render inside `<bdi>` / `dir="ltr"` so `REC-005`,
  `RC-74ceaaad` and `11-9` never reorder.
- **Digits:** Western (latn) digits for all data (codes, amounts, times, dates, counts) in both
  locales so tables align and copy matches. Arabic-Indic digits only in prose when the pack's
  locale asks. Never mix both in one view.
- **Font pairing (TARGET):** Cairo for Arabic, matched to Geist's x-height; same weights (400/500).
- Tables, schedules and dock order mirror. Charts with a time axis keep time running in the reading
  direction of the locale.
- Every screen ships with an Arabic Playwright row and an RTL screenshot (section 16).

---

## 15. Motion

- Popups and menus: 150ms opacity + scale 0.95 from `--transform-origin`.
- Sheet: 150ms opacity + exactly 16px inline-axis slide from its edge (`translate-x-[16px]`, with
  `rtl:-translate-x-[16px]`; `translate-x-4` would be 14.4px at this root).
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
- Every control that changes **domain state** is an `ActionTrigger` bound to a declared Norn action
  or transition, dispatched through the typed, server-authorized call path (section 12.4). Local
  view controls are not cell actions and get no `ActionTrigger`: scheme and skin (local state,
  `src/lib/appearance.tsx`), navigation, opening or closing a sheet, filters and selection (URL
  state). They are still named, accessible primitives an agent can operate.
- The Sidekick drives those same named controls through UI SDK `createAgent` (TARGET); it gets no
  private endpoints and no extra authority, and the cell authorizes its calls exactly as a person's.
- **Keyboard handlers on Base UI composites** (Toolbar, ToggleGroup, RadioGroup, Tabs) run in the
  bubble-phase `onKeyDown`, alongside Base UI's `CompositeRoot`, never in `onKeyDownCapture`. Work
  that must run after Base UI's roving `focus()` (which it schedules with one `queueMicrotask`) is
  deferred inside that same handler, as `components/ui/toggle-group.tsx` does; capture-phase
  handlers race the roving focus and select the wrong item.
- Primitives carry `data-slot` names (`panel`, `well`, `kpi`, `list-row`, `detail-sheet`,
  `floating-dock`) so tests, skins and agents target structure, not class strings.
- Selection and open sheets live in the URL, so an agent and a person share state by link.

### 16.2 Gates (every PR touching UI)

1. **Domain-agnostic Studio** (`apps/studio/test/domain-words.test.ts`, CURRENT). No allowlist.
2. **No Radix** (`apps/studio/test/ui-primitives-guard.test.ts`, CURRENT).
3. **Contrast** (`apps/studio/test/contrast.test.ts`, `studio-theme.test.ts`, CURRENT; extend to
   the new tokens): across every skin × scheme, text ≥ 4.5:1 on its real surface (card **and**
   well, resting, hover, selected), including step-5 figures and their units; `--action` as text ≥
   4.5:1 on the same surfaces; `--primary-foreground` on `--primary` and on `--action-hover` ≥
   4.5:1; focus rings, graph edges, chart marks and `--dock-indicator` on the dock ≥ 3:1; avatar
   pairs ≥ 4.5:1. Dots are exempt only
   because they are always paired with a label; a gate check for that pairing is TARGET (see
   section 18).
   `--subtle-foreground` may not carry information.
4. **Touch:** interactive targets ≥ 44 × 44 CSS px below 640px (dock, ListRow, pills, buttons,
   sheet close, form inputs), written in px; ≥ 32px elsewhere.
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

### 17.1 Token diff: Studio `index.css` vs the first-party app

Same token, different values. "Pick" is what this spec adopts.

| Skin / scheme | Token | Studio | First-party app | Pick | Why |
| --- | --- | --- | --- | --- | --- |
| all | default skin | `polar` | `open` | `open` | Owner chose the first-party app's look; `open` is still neutral surfaces with one accent, so colour discipline holds. `polar` stays one click away. |
| all | default shell | `sidebar` | dock | `dock` | Non-negotiable 5; Studio's own spec already named the dock the target default. Builders opt into `sidebar`. |
| open L | `--radius-panel` / `--radius-well` | 1.25rem / 0.9375rem (18 / 13.5px at 90%) | 20px / 15px | 20px / 15px | Non-negotiable 1; px so geometry does not scale with the root. |
| open L | `--action`, `--primary`, `--ring` | #2e60e0 | #3a6df0 | #2e60e0 | White on #3a6df0 is 4.54:1 (knife-edge) and #3a6df0 as text on the well is 4.16:1 (fails); #2e60e0 is 5.43:1. Accessibility is stricter. #3a6df0 stays as `--chart-1`. |
| open L | `--action-hover` | #2451c7 | #2d5de0 | #2451c7 | Follows the action pick. |
| open L | `--muted-foreground` | #5c5c63 | #6e6e73 | #6a6a6f | The app's grey, one step darker: #6e6e73 is 4.37:1 on `--well-hover` #eeeeee (fails); #6a6a6f is 5.38 / 4.93 / 4.64:1 on card, well and hover. |
| open L | `--subtle-foreground` | — | #a0a0a6 | #85858b, decorative only | #a0a0a6 is 2.39:1 on the well; the app used it for counts. Counts move to muted; subtle keeps ≥ 3:1 for glyphs. |
| open L | `--success` | #1f7a4d | #1f9d61 | text #1f7a4d, dot #1f9d61 | The app's green is 3.47:1 as text (fails). Split into text and dot tokens. |
| open L | `--warning` | #8a5600 | #e09a2b | text #8a5600, dot #e09a2b | The app's amber is 2.38:1 as text. |
| open L | `--destructive` | #b83228 | #e5484d | fill/text #b83228, dot #e5484d | The app's red is 3.91:1 with white. |
| open L | `--chart-3`, `--chart-4` | #b45309, #9a1ca6 | #e09a2b, #7c5cff | Studio's | Studio's set is AA-tuned; the app's amber fails 3:1 as a mark. Labels still use text tokens (section 13). |
| open L | `--line`, `--action-dot`, `--action-soft`, `--popup-shadow`, `--sheet-shadow` | — | defined | app's | New tokens Studio lacks. `--action-fill` is dropped: muted text on it fails (section 4.2). |
| open L | `--font-sans` | Geist only | Geist + Cairo | Geist + Cairo | RTL first-class. |
| open D | `--primary` (CTA fill) / ink | #2e60e0 / #ffffff | #5b8cff / #ffffff | #2e60e0 / #ffffff | White on the app's #5b8cff is 3.16:1. #2e60e0 is 5.43:1 and matches light. |
| open D | `--action` (accent text) | #6b98ff | #5b8cff | #6b98ff | Studio's; ≥ 5.64:1 on every dark surface. #2e60e0 as text would be 3.58 / 3.36 / 3.13 / 2.88:1 on canvas, card, well, hover. `--link` #7da4ff is 7.98:1 on canvas; `--ring` #5b8cff is 6.14:1 on canvas and 5.38:1 on the well (its minimum). |
| open D | `--destructive-text` | #f07b73 | #ff6b6f | #ff6b6f | The app's passes (6.14:1 on well) and matches its dot. |
| open D | `--panel-shadow` | `inset 0 1px 0 rgb(255 255 255/.02)` | same | same | — |
| polar L | `--radius-panel` / `--radius-well` | 0.75rem / 0.5rem (10.8 / 7.2px) | 16px / 11px | 16px / 11px | Non-negotiable 1. |
| polar L | panel inset | 0 (flattened KPI and panel wells) | 4px double shell | 4px | Non-negotiable 1: the double shell is the only nesting pattern in every skin. |
| polar L | `--panel-shadow` | none | `0 1px 2px rgb(0 0 0/.02)` | app's | A whisper of lift separates the rim from `#f6f6f8` canvas. |
| polar L | `--muted-foreground` | #585861 | #5c5c66 | #5c5c66 | Passes (6.18:1 on well); the app's wins. |
| polar L | `--subtle-foreground` | — | #9696a0 | #85858b | #9696a0 is 2.74:1 on the well. |
| polar L | `--card-foreground`, `--foreground` | #18181b | #18181b | same | — |
| polar D | `--card` | #161618 | #151517 | #151517 | The app's; matches `open` dark card. |
| polar D | `--panel-border` | #26262b | #25252a | #25252a | The app's. |
| polar D | `--panel-shadow` | inset .04 | inset .03 | .03 | The app's. |
| polar D | `--subtle-foreground` | — | #686870 | #686870 | 3.11:1 on well; decorative only. |
| luma, glass | radii | 0.6875/0.5rem, 1/0.75rem | 20/15px (inherited) | 16/11px, 20/15px | Concentric rule; Studio's luma and glass palettes are more complete and stay. |
| luma D | CTA fill | `--primary` #6258ec | — | #6258ec | White on it is 5.09:1 (hover #695ff0 4.67:1). Luma's `--action` #7c74ff is text only: white on it is 3.62:1. |
| all | `--dock-indicator` | #6b98ff, polar #f4f4f5 | `--action` | Studio's | The dock is dark in every scheme; `--action` on it is 1.05:1 in polar light and 2.92:1 for #2e60e0 in open dark. |
| all | avatar colours | — | hard-coded hex in `Initials` | `--avatar-n-*` tokens, hashed by id | Non-negotiable 6. |
| all | type sizes | `--text-meta/panel/section/title/figure` = 0.8125/0.9375/1.0625/1.75/2rem | ad-hoc 0.65–2.1rem | 0.84/1/1.35/1.85/2rem (section 8.1) | Non-negotiable 6; the app's visual sizes, locked to five. The app's 0.54em figure unit becomes step 2. |

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
  `--dot-*`, `--avatar-*`, `--popup-shadow`, `--sheet-shadow`, `--panel-inset`,
  `--radius-row/sheet/search/dock/popup`; StudioTheme allowlist needs the new colour tokens.
- [ ] Solid `bg-action` fills (4 uses) put `--action-foreground` on the text-role `--action`: 2.78:1
  on open-dark #6b98ff and 3.62:1 on luma-dark #7c74ff. Solid fills move to `bg-primary`.
- [ ] `--font-sans` has no Arabic pairing (no Cairo; not in `package.json`).
- [ ] No `minimal` route-level chromeless frame; login and OAuth use shadcn `Card`.
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
- [ ] Dock attention dot uses physical `right-[7px]` and is colour-only (no accessible text).
- [ ] Dock-mode top bar (`data-slot="dock-topbar"`) is borderless but `h-[4.4rem]` on
  `bg-background`, and has no guarantee tick bar, language pill or scheme toggle.
- [ ] The top-bar Notifications popover renders a hard-coded "Nothing yet." instead of a
  server-backed list or an honest unavailable state.

**RTL**

- [ ] Physical spacing in first-party code: `actions/action-form.tsx` (3),
  `learning/episodes-page.tsx`, `learning/episode-page.tsx`; vendored `components/ui/*`
  (`dropdown-menu`, `button`, `toggle`, `input-group`, `sidebar`, `sheet`) when touched.
- [ ] No Arabic font; no `<bdi>` discipline for ids and money.

**Refusals and call outcomes (`src/lib/actions.ts`, `src/lib/screen/call.ts`)**

- [ ] `refusalOf` drops the wire's `category` and `retryable` and makes `explanation` optional; the
  `refused` arm of `CallResult` and the `refused` call phase carry neither, so no screen can show the
  category icon or a server-directed Retry (section 12.3).
- [ ] `listActions`' `refused` arm keeps only `status`, `code` and `message`.
- [ ] `callAction` and `listActions` map every 404 and 503 to `unavailable` without using the body,
  so a typed `Refusal` on those statuses is never shown (section 12.4).
- [ ] An untyped 503 renders the unsupported-route copy with no Retry; it should read as a temporary
  outage with Retry, and only an untyped 404 keeps "doesn't take actions" (section 12.4).
- [ ] `index.css` `--dock-muted` is #a5a5ad (4.15:1 on the glass-light dock composite #3f414a);
  the spec value is #b4b4bc (4.93:1) (section 9.1 tokens).

**Agent operability**

- [ ] No UI SDK `createAgent` wiring; the Sidekick (`sidekick/client.ts`) does not drive named
  controls. List rows have no `data-slot`.

**Gates**

- [ ] Contrast gate does not yet cover the new tokens (subtle, avatars, action-foreground on
  action in dark, muted on `--well-hover`), and nothing checks that every dot is paired with a
  text label.
- [ ] No 44px mobile minimum on rows, pills or buttons (`h-9` is 32.4px at the 90% root).
- [ ] No lint for arbitrary type sizes, banned weights or physical spacing.
