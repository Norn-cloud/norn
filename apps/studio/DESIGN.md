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

**Owner decision (2026-11-25, later the same day): luma, on three glass islands.** Every Norn UI
(Studio, `@norn/ui`, first-party apps, generated apps) defaults to the `luma` skin and the
three-island shell (`data-skin` absent or `"luma"`, `data-shell="sidebar"`; sections 3, 4.7 and 9.1):
a thin applet rail on the start side, the open applet's submenu as a second floating island, and the
content on a third, main island, all liquid glass on one backdrop. In the owner's words: "use luma
theme, and i wanted a thin sidebar in the left, for applets, then submenus as a second island, then
main island, KINDA like Polar, or look up the new ChatGPT desktop app look, but replace threads with
submenu items", and, for the material, "can we kinda use the shadcn liquid glass or panelo thing?".
This supersedes the single thin sidebar and the `polar` default from #1734 and #1753 earlier that day;
`polar`, `open` and `glass` stay selectable. The floating dock stays opt-in: an app may choose it only
as a non-default mode for kiosk or mobile-first surfaces (section 9.2). The rest of the first-party
app's language still applies everywhere: the concentric double-shell `ShellCard`/`StatWell`, weights
of 400 and 500 only, a dot plus its word for status, the floating inset `DetailSheet` with one
primary action, and the five-step type scale.

**Owner decision (2026-11-25, third): the real shadcn Luma, no sparkle, a premade composer, a
mascot.** In the owner's words: "This still isn't really luma, theme, AND, look, i want the sparkly
'ai' icon, banned, its so cliche now, and get a premade ai composer and shit like that, ideally
official shadcn one, dont reinvent the wheel [...] see if u can get a grokbot/bevel ai, looking
mascot". So the `luma` skin is shadcn's own Luma style (`base-luma`: its radius scale, pill
controls and fully rounded cards, section 19.1) in the monochrome the owner set the same day, measured
from Polar's dashboard and the getopen.so demo (pure greys, opaque islands, no glass in the default;
section 19.6); the sparkle "AI" glyph is banned by lint (section 19.2); Sidekick and the Ask Norn bar
use shadcn's own chat primitives over the AI SDK's `useChat` (section 19.3); and Norn's assistant is
an original mascot (section 19.4).

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
   figures, then lists that open into a sheet. The frame stays quiet as well: by default it is three
   calm glass islands on a mist backdrop (section 9.1), so the content carries the colour.
3. **One way to nest.** The concentric double shell (`ShellCard` around a well) is the only nesting
   pattern. Anything else that frames a frame is a bug.
4. **Colour means something.** Chrome is monochrome in every skin: black, white and greys, like
   Polar's dashboard (section 19.6). Colour is reserved for charts and visualisations, status dots
   beside their word, avatar discs, the Norn mark and its mascot, and data highlights.
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
| shadcn style | `base-luma` in `components.json` (shadcn CLI 4.23.0, `rtl: true`) | `components/ui/*` are derived from its output (section 19.1). |
| AI chat | shadcn's chat primitives (`message`, `bubble`, `trace` from the base-luma registry, MIT, derived) and Norn's composer on shadcn's `input-group`, over the AI SDK's `useChat` (`ai` 7.0.169, `@ai-sdk/react` 4.0.173, Apache-2.0 npm packages, pinned exactly) | Sidekick and the Ask Norn bar only (section 19.3). The AI SDK loads with the Sidekick chunk, never with the shell. |

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
| `data-skin` | `luma` (default; bare `:root` and `.dark`: shadcn's Luma, section 19.1), `polar`, `open`, `glass` | Palette, radii (including Tailwind's `rounded-xl…4xl` steps, through `--radius-step-*`), shadows and the island material (section 4.7). |
| `.dark` | present or absent | Light or dark scheme (OS preference unless the user or a theme forces one). |
| `data-shell` | `sidebar` (default: the three-island shell), `dock` (opt-in) | Frame chrome and navigation geometry only (section 9). A user preference, persisted per browser. The value keeps its old name so stored choices survive; the appearance menu calls it "Islands". |
| *(no attribute)* `minimal` | route-level, not a `data-shell` value | Chromeless routes (login, OAuth, embeds) render outside the shell (section 9.3). A stored preference can never select it. |
| `data-density` | `comfortable` (default), `compact` | Row heights (`--density-*`); `compact` on pages of the Builder lens. The shell sets it from the current page's lens (`Section.perspective`), never from a reader's switch. |
| `dir`, `lang` | `ltr` / `rtl`, BCP 47 | Direction and font pairing (section 14). |

**Default skin is `luma`** (CURRENT: bare `:root` and `.dark` in `src/index.css`, so the default
holds before any script runs; `storedSkin` in `src/lib/appearance.tsx`; owner decision,
2026-11-25). It is shadcn's Luma style: the neutral theme and Luma's radius scale, pill controls and
fully rounded cards (section 19.1), and its islands are liquid glass (section 4.7). Knot violet now
paints only the Norn mark and the mascot. `polar` (monochrome ink on neutral surfaces, colour only where section 13
allows it) moved behind its own `:root[data-skin="polar"]` blocks and stays one choice away, with
`open` (the first-party app's white canvas with a blue accent) and `glass`. Every skin follows the
same geometry rules and passes the same gates; `polar` and `open` paint opaque islands. Section 17.1
records the decision.

**Default shell is `sidebar`, the three-island shell** (CURRENT: `storedShellChrome`; owner
decision, 2026-11-25). It is described in section 9.1, and it is always the default, in Studio and
in every app. `dock` is never a default: an app may opt into it only as a non-default mode for kiosk
or mobile-first surfaces, chosen explicitly by the app (section 9.2). A reader's stored choice wins
over the default, including an app's dock opt-in: that is only the surface's default, so a reader's
stored `sidebar` choice still wins there (CURRENT: `storedShellChrome(surfaceDefault)`; lead ruling,
2026-11-25). A view never branches on the shell.

**StudioTheme JSON (CURRENT, `src/lib/studio-theme.ts`).** Operators and tenants may supply a
declarative theme (`version: 1`, `baseSkin`, `appearance`, `accent`, `canvas`, `colors`,
`variants.light` / `variants.dark`) that overrides only the allowlisted semantic tokens
(`STUDIO_THEME_TOKENS`). Themes carry zero logic: no queries, permission checks or layout forks, and
never geometry, z-index or arbitrary CSS. Every theme is sanitized against CSS injection and must
pass the contrast gate (section 16) before it applies. New tokens this spec adds that are colours
(`subtle-foreground`, `line`, `action-dot`, `action-soft`, the dot and avatar tokens) are in the
allowlist, each with validator pairs (CURRENT since restyle 2: subtle glyphs 3:1 on canvas, card and
well; `line` a visible hairline, 1.1:1, on card and well; ink 4.5:1 on `action-soft`; each dot a
visible 1.5:1 mark beside its word; avatar ink 4.5:1 on its disc); geometry tokens never do. The
island colours (`backdrop`, `backdrop-glow-1`, `backdrop-glow-2`, `island`, with `sidebar`) are in the
allowlist too, and every island pair is validated over the worst-case backdrop (section 4.7; CURRENT
since shell v2). A theme that paints its own `canvas` but not the islands gets opaque islands in that
ground on a backdrop one step darker (`resolveCustomThemeTokens`), so its canvas is what the reader
sees; the blur, the edge and the shadows are geometry and stay the base skin's.

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
- **`glass`** sets no action tokens, so it inherits `open`'s row for its scheme. Its dark scheme lifts
  `--subtle-foreground` to #6e6e76: the islands' `saturate(165%)` leaves open's #6a6a72 at 2.97:1 on
  the main island under both glows, and #6e6e76 reads 3.15:1 there. Its translucent
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
  - dock: `--dock-surface` over canvas;
  - sidebar: `--sidebar` over canvas; sidebar hover: `--sidebar-accent` at 50% over sidebar; sidebar
    pill: `--sidebar-accent` over sidebar (measured in section 4.6).
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
the state. Navigation attention dots (the sidebar's and the dock's) are the exception: their state
lives only in the control's accessible name, so they use their own 3:1 tokens (`--ring` in the
sidebar, section 4.6; `--dock-indicator` on the dock, section 4.5). **Text** tokens are the
AA-safe versions used when a value itself must be coloured (a signed delta, an "After" value, a
page-head live line).

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
- **Avatar palette (CURRENT tokens, restyle 2).** Six disc pairs, `--avatar-1…6-bg` / `--avatar-1…6-fg`,
  the same in every skin:

  | n | light bg / fg | dark bg / fg |
  | --- | --- | --- |
  | 1 | #fde4e1 / #9a3b2e | #3d201e / #fde4e1 |
  | 2 | #e3ecff / #2d4fa8 | #1c2642 / #e3ecff |
  | 3 | #e2f5ea / #23744a | #193226 / #e2f5ea |
  | 4 | #fdf0d8 / #8a5a12 | #382a15 / #fdf0d8 |
  | 5 | #efe6fd / #6b3fae | #2f2244 / #efe6fd |
  | 6 | #dff3f6 / #1f6f7a | #183035 / #dff3f6 |

  The dark fills are the light ink at 30% over the dark card (#151517), precomputed in sRGB so the
  palette generator and the validator read plain hex. Every light pair measures ≥ 5.0:1 and every
  dark pair ≥ 12:1 (`test/system-primitives.test.tsx`, the StudioTheme validator). The disc index is
  a stable FNV-1a hash of the **principal, workspace or record id**, never the display name
  (`avatarIndex`, `components/system/list-row.tsx`), so a rename or a switch between Arabic and
  English keeps the colour. Components never hard-code disc hex.

### 4.5 Dock (opt-in shell)

`--dock-surface` #1d1d20 / #222226, `--dock-well` #34343a / #3a3a41, `--dock-border`
rgb(255 255 255 / .08), `--dock-foreground` #ffffff, `--dock-muted` #b4b4bc (8.16:1 / 7.70:1 on the
light- and dark-scheme dock, 4.93:1 on the glass-light composite #3f414a, 8.96:1 on glass dark;
shipped `index.css` still uses #a5a5ad, which is 4.15:1 on glass light: see section 18), `--dock-ring` #93b4ff. The dock is dark in every skin and scheme;
`glass` makes it translucent (rgb(24 26 36 / .82) / rgb(18 20 34 / .80)).

`--dock-indicator` is its **own token**, never an alias of `--action`, because the dock is dark
even in light schemes (polar-light `--action` #18181b on the dock is 1.05:1). Values (CURRENT in
`index.css`): #6b98ff in `open`, `luma` and `glass`; #f4f4f5 in `polar`. It must reach 3:1 on the
actual dock surface (`apps/studio/test/contrast.test.ts`, CURRENT); see the table in 4.2.

### 4.6 Nav islands: the applet rail and the submenu (default shell)

The applet rail and the submenu island (section 9.1) use the `--sidebar-*` family (CURRENT values in
`index.css`; all in the StudioTheme allowlist). `--sidebar` is their material: in `luma` the same
translucent mist as the main island, in `glass` its own frost, and opaque `--shell`-like ground in
`polar` and `open`.

| Token | luma L | luma D | polar L | polar D | open L | open D | glass L | glass D | Use |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `--sidebar` | rgb(247 248 249 / .80) | rgb(13 13 16 / .80) | #f6f6f8 | #0a0a0b | #f6f6f7 | #09090a | rgb(246 248 254 / .62) | rgb(15 18 30 / .60) | The nav islands' material, over the backdrop (section 4.7). Under reduced transparency, without `backdrop-filter`, or in forced colours it is `--shell`. |
| `--sidebar-foreground` | #1b1b1d | #ececee | #18181b | #fafafa | #1b1b1d | #ececee | #1b1b1d | #ececee | The workspace and principal names, the workspace's state line. |
| `--sidebar-accent` | #e8eaf0 | #18181f | #ebebef | #1d1d21 | #ececef | #18181b | rgb(255 255 255 / .72) | rgb(255 255 255 / .09) | The selection pill (current applet, current row). At 50% it is the hover wash, and the pressed wash. |
| `--sidebar-accent-foreground` | #1b1b1d | #ececee | #18181b | #fafafa | #1b1b1d | #ececee | #1b1b1d | #ececee | Label, icon and count of the current row and the current applet, drawn on the pill. |
| `--sidebar-border` | #e2e4ea | #1e1e26 | #e5e5ea | #232328 | #ebebeb | #202024 | rgb(58 109 240 / .14) | rgb(255 255 255 / .12) | The phone sheet's hairline between the applets and the menu, and the filter field's ring. Decorative only. |

Resting rows, the applet icons, group headers and counts use `--muted-foreground`. Focus uses
`--ring`. Chrome reads neither `--sidebar-primary` nor `--sidebar-ring`: it gets no accent fills,
and `--sidebar-ring` is not the focus ring.

Measured pairs use the formula and compositing of section 4.2, with the nav island composited over
the backdrop at its worst case (section 4.7: flat, under either glow at its peak, under both); each
figure is the minimum over the four. Floors: 4.5:1 for text, 3:1 for the ring and the attention dot.
Gated by `test/contrast.test.ts` ("nav islands' ground…") and by the StudioTheme validator.

| Skin / scheme | backdrop | ground (flat) | muted on ground | muted on hover | pill ink on pill | `--ring` (focus and dot) on ground / hover / pill |
| --- | --- | --- | --- | --- | --- | --- |
| luma L | #e7e8ed | #f4f5f7 | 5.92 | 5.72 | 14.30 | 5.23 / 5.05 / 4.87 |
| luma D | #050507 | #0b0b0e | 6.93 | 6.66 | 14.97 | 5.28 / 5.08 / 4.88 |
| polar L | #ebebee | #f6f6f8 | 6.52 | 6.24 | 14.90 | 7.16 / 6.85 / 6.50 |
| polar D | #000000 | #0a0a0b | 7.36 | 6.84 | 16.10 | 7.72 / 7.18 / 6.56 |
| open L | #f0f0f2 | #f6f6f7 | 6.14 | 5.88 | 14.59 | 5.03 / 4.82 / 4.61 |
| open D | #000000 | #09090a | 7.21 | 6.89 | 15.02 | 6.29 / 6.01 / 5.60 |
| glass L | #eef1f8 | #f2f5fe | 5.41 | 5.84 | 16.24 | 4.43 / 4.78 / 5.13 |
| glass D | #0b0d17 | #0d101e | 5.84 | 5.19 | 10.58 | 5.09 / 4.53 / 3.95 |

- **The attention dot uses `--ring`, not `--dot-accent`.** A rail applet's dot carries its state only
  through the applet's `aria-label` ("Inbox, 3 open"), so nothing visible sits beside it, and it
  must reach 3:1 on the ground, the hover wash and the pill. `--ring` does in every row above.
- **Muted text never sits on the pill.** In the current row, the count also takes the pill's ink.
- **The pill is not the only cue.** Its fill is only about 1.1:1 against the ground. The current row
  and the current applet are the only ones whose label and icon step up to
  `--sidebar-accent-foreground`, and the only ones with `aria-current="page"` (section 9.1).
- **The phone sheet is opaque.** It uses `--shell` in every skin (the material and the backdrop do
  not reach inside a modal), measured on that ground by the same test.

### 4.7 The backdrop and the glass material (default shell)

The three islands float on a **backdrop**: `--backdrop`, painted on the shell wrapper with two soft
radial pools of ambient light, `--backdrop-glow-1` and `--backdrop-glow-2` (decorative, part of the
backdrop). The main island's material is `--island`; the nav islands' is `--sidebar` (section 4.6).
Each island takes the backdrop blur and saturate `--island-blur`, a 1px specular edge
`--island-border`, and a depth shadow `--island-shadow` with a 1px inner highlight on its top edge,
at `--radius-island` (18px). It is CSS only, in the spirit of the shadcn liquid-glass registries
(glasscn, Glass UI) and Panelo: **no WebGL, no SVG displacement and no refraction library**
(`test/contrast.test.ts` refuses one in the shell).

| Token | luma L | luma D | polar L | polar D | open L | open D | glass L | glass D |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `--backdrop` | #e7e8ed | #050507 | #ebebee | #000000 | #f0f0f2 | #000000 | #eef1f8 | #0b0d17 |
| `--backdrop-glow-1` | rgb(85 76 232 / .07) | rgb(124 116 255 / .10) | none | none | none | none | rgb(58 109 240 / .16) | rgb(58 109 240 / .22) |
| `--backdrop-glow-2` | rgb(56 189 248 / .06) | rgb(56 189 248 / .05) | none | none | none | none | rgb(139 92 246 / .12) | rgb(139 92 246 / .18) |
| `--island` | rgb(247 248 249 / .80) | rgb(13 13 16 / .80) | #f6f6f8 | #0a0a0b | #ffffff | #0d0d0e | rgb(252 253 255 / .60) | rgb(18 21 34 / .62) |
| `--island-border` | rgb(20 22 48 / .07) | rgb(255 255 255 / .07) | rgb(24 24 27 / .06) | rgb(255 255 255 / .07) | rgb(0 0 0 / .06) | rgb(255 255 255 / .07) | rgb(255 255 255 / .72) | rgb(255 255 255 / .13) |
| `--island-blur` | blur(24px) saturate(160%) | same | none | none | none | none | blur(24px) saturate(165%) | same |

`--canvas` is the main island's opaque equivalent (luma #f3f4f6 / #0b0b0e, the composite of the
island over the flat backdrop), and `--shell` the nav islands'. Content measured "on canvas" is
therefore measured on the main island's fallback, and the island itself over the backdrop:

- **Worst-case backdrop.** A glow is brightest at its centre, so an island over a glow at its peak is
  the extreme of what the reader can see under it. Every island pair is measured four times: over
  the flat backdrop, under either glow at its peak, and under both (8-bit compositing per layer,
  section 4.2), each as the island's `saturate()` paints it (the Filter Effects saturate matrix on
  the sRGB values, clamped), and the minimum must clear the floor. The glows never lift a failing pair: each
  measurement includes the darkest of them.
- **Main island pairs** (`test/contrast.test.ts`, "the main island's text…", and the validator): page
  text, muted text, `--action` and `--link` at 4.5:1 and `--ring` at 3:1 on the island, and on a card,
  a well and a hovered well inside it; decorative glyphs 3:1 on the island. Status dots in the
  connection line sit beside their word (section 4.3), so they are kept visible (1.5:1), not held to 3:1.

  | Skin / scheme | island (flat) | figure | muted | `--action` | `--link` | `--ring` | muted on a well |
  | --- | --- | --- | --- | --- | --- | --- | --- |
  | luma L | #f4f5f7 | 15.35 | 5.92 | 5.88 | 5.61 | 5.23 | 6.36 |
  | luma D | #0b0b0e | 16.20 | 6.93 | 5.79 | 6.77 | 5.28 | 6.47 |
  | polar L | #f6f6f8 | 16.41 | 6.52 | 16.41 | 16.41 | 7.16 | 6.58 |
  | polar D | #0a0a0b | 18.96 | 7.36 | 18.00 | 15.60 | 7.72 | 6.39 |
  | open L | #ffffff | 17.20 | 6.63 | 6.37 | 6.83 | 5.43 | 6.08 |
  | open D | #0d0d0e | 16.47 | 7.04 | 6.99 | 7.98 | 6.14 | 6.16 |
  | glass L | #f6f8fe | 14.30 | 5.52 | 5.30 | 5.68 | 4.52 | 6.15 |
  | glass D | #0f1220 | 13.50 | 5.77 | 5.73 | 6.54 | 5.04 | 5.80 |

- **Opaque fallbacks** (CURRENT, `index.css`, gated by `test/contrast.test.ts` and the Playwright
  row "islands under reduced transparency and forced colours"). Under
  `prefers-reduced-transparency: reduce`, and in a browser without `backdrop-filter`, the nav islands
  paint `--shell` and the main island `--canvas`, with no blur (also on the frosted surfaces inside
  `glass`). Under `forced-colors: active` the islands drop the blur and the shadow and take a
  `CanvasText` edge. The opaque surfaces are the canvas and shell pairs already gated.
- **Luma's accent dot and wash follow its accent** (`--action-dot: var(--action)`,
  `--action-soft`: `--action` at 10% / 14%), no longer open's blue.

## 5. Radii and concentric geometry

Radii are **pixel** values, not rem: geometry must not change with the 90% root scale.

| Token | open | polar | luma | glass | Used by |
| --- | --- | --- | --- | --- | --- |
| `--radius-panel` | 20px | 16px | 26px (Luma's card, `rounded-4xl`) | 20px | ShellCard, StatWell outer rim. |
| `--panel-inset` | 4px | 4px | 4px | 4px | Padding between rim and well. |
| `--radius-well` | 15px | 11px | 21px | 15px | Inner well. |
| `--radius-row` | 11px | 11px | 16px | 11px | ListRow and menu rows (one value). |
| `--radius-sheet` | 24px | 24px | 26px (Luma's dialog) | 24px | DetailSheet, dialogs. |
| `--radius-search` | 18px | 18px | 14px (Open's input) | 18px | Page search field, the Ask Norn bar. |
| `--radius-composer` | 12px | 12px | 14px | 12px | Sidekick's composer, a multi-line field group. |
| `--radius-dock` | 26px | 26px | 26px | 26px | Floating dock. |
| `--radius-island` | 18px | 18px | 16px (Polar's island) | 18px | The three islands of the default shell (section 9.1); the phone main island has none. |
| `--radius-popup` | 16px | 16px | 22px (Luma's `rounded-3xl`) | 16px | Menus, popovers. |
| `--radius` | 8px | 8px | 12px | 8px | Form inputs (white, 1px `--input` hairline), code blocks; Tailwind's `rounded-lg`. Inputs stay 8–14px (section 19.6), never pills. |
| `--radius-chip` | 5px | 5px | full (Luma's pill badge) | 5px | SourceTag, Badge. |
| `--radius-step-xl/2xl/3xl/4xl` | panel/12/8/8px | panel/12/8/8px | 14/18/22/26px | panel/12/8/8px | Tailwind's `rounded-xl…4xl`, so shadcn's Luma classes (pill inputs and toggles, 4xl cards) draw Luma's geometry in luma and the crisp one elsewhere. |
| pills | full | full | full | full | Top-bar controls, every button, segmented controls, tabs (luma). |

Radii, geometry tokens and the px values in sections 5, 9 and 11 are CURRENT in `index.css` since
the Studio restyle (#1753), in every skin.

**Concentric rule:** `radius-panel = radius-well + panel-inset + 1px border`. 15 + 4 + 1 = 20;
11 + 4 + 1 = 16; 21 + 4 + 1 = 26 (luma). A skin that changes one changes the other. Every skin keeps the double shell; no
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

In `luma` the card, popup and sheet levels are Luma's own Tailwind shadows: `shadow-md`
(`0 4px 6px -1px rgb(0 0 0/.1), 0 2px 4px -2px rgb(0 0 0/.1)`), `shadow-lg` and `shadow-xl`, with a
`--foreground` 6% edge (10% dark) in place of a border, as Luma's `ring-foreground/5` does. Dark keeps
the inner highlight above.

Five levels only: flat (wells, the backdrop), island (`--island-shadow`: a top inner highlight,
whisper contact and a soft ambient drop, section 4.7), card (`panel-shadow`), popup, and sheet/dock
(the phone Navigation sheet takes `--sheet-shadow`). Shadows are
whisper contact plus soft ambient; never a hard blob. Dark mode lifts with a 1px inner highlight
rather than a darker shadow.

---

## 7. Spacing and layout

- **Root scale:** `html { font-size: 90% }`, so `1rem = 14.4px`. Never CSS `zoom` (it breaks
  `@xyflow/react` pointer maths). Tailwind's spacing scale is rem-based, so `3` is 10.8px and `4`
  is 14.4px here. Anything that must be an exact pixel size (touch targets, radii, dots, the sheet
  inset, slide distances) is written as an arbitrary px value (`top-[12px]`, `min-h-[44px]`).
- **Canvas:** content `max-width: 1220px`, centred, `px-6 sm:px-8`. Top bar `max-width: 1260px`.
  In the three-island shell both are centred inside the main island, not in the viewport.
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

### 8.1 The locked five-step scale (CURRENT tokens)

| Step | Token | Size | px @ 90% | Line height | Tracking | Weight | Used for |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `--text-meta` | 0.84rem | 12.1 | 1.4 | 0 | 400 | Secondary runs, counts, timestamps, mono codes, table headers, SourceTag, footnotes, sidebar group headers, dock labels. |
| 2 | `--text-body` | 1rem | 14.4 | 1.45 | -0.003em | 400 | List titles, card titles, Props, buttons, menu rows, inputs, prose. |
| 3 | `--text-heading` | 1.35rem | 19.4 | 1.25 | -0.02em | 500 | Sheet titles, dialog titles. |
| 4 | `--text-title` | 1.5rem | 21.6 | 1.15 | -0.025em | 500 | Page `h1` (between Open's 20px and Polar's 24px, section 19.6). |
| 5 | `--text-figure` | 2rem | 28.8 | 1 | -0.025em | 400 | StatWell values, hero numbers. The unit beside a figure is step 2 (`--text-body`), muted, `tracking-normal`. |

**No other sizes.** No `text-[0.88rem]`, `text-[11px]`, `text-xs`/`text-sm` drift in first-party
screens. Mono runs use step 1 or 2 in the mono family with `letter-spacing: -0.01em`. Canvas edge
labels and minimap text are the one exception (they scale with the canvas zoom) and stay inside
`index.css`. Figure units are not an exception: they use step 2. Migration: Studio's `--text-panel` maps to `--text-body`, `--text-section` to
`--text-heading`; the old names are deleted once the restyle lane lands (no shims).

### 8.2 Weights

- **400** everywhere by default: card headers, list titles, KPIs, Props, body.
- **500** only for: page `h1`, sheet/dialog titles, the active pill or segment, the primary CTA.
  The current sidebar row is not an active pill in this sense. It stays at 400, and the selection
  pill and ink step show it (section 9.1).
- **600 and 700 are banned** in UI chrome and content, including canvas node cards and decision
  trees. Emphasis comes from size step, `--foreground` vs `--muted-foreground`, or position.
- No uppercase chrome (section banners, eyebrow labels). Codes keep their own case.

---

## 9. Shell modes

Shells own frame chrome and navigation only. A view never knows which shell it is in.

### 9.1 `sidebar`: the three-island shell (default for every Norn UI)

Three floating islands on the backdrop, like Polar's dashboard (an icon rail beside a context
panel) and the ChatGPT desktop app, with the open applet's items where ChatGPT lists threads (owner
decision, 2026-11-25). CURRENT since shell v2 (`src/shell.tsx`, `src/shell-nav.tsx`,
`src/components/ui/sidebar.tsx`, the model in `src/lib/nav-model.ts`), except where an item is
marked TARGET.

**Frame**

- On the start side (the right in RTL), in order: the **applet rail** (64px, `--rail-width`), the
  **submenu island** (256px, `--sidebar-width`), then the **main island**, which fills the rest. Each
  is 8px from the window edge and 8px from the next, at `--radius-island` (18px), in the glass
  material of section 4.7 on the backdrop. Widths are px, so they do not scale with the 90% root.
- The rail and the submenu island are `sticky` and as tall as the window less 16px; the page scrolls
  as a document, inside the main island, which is at least that tall. The main island holds the top
  bar, the connection line, the page, and the inspector and assistant docked on its end side.
- **No card in a card.** The main island is the ground the ShellCards sit on, not a card around
  them: its material is the luma mist (`--island`, whose opaque equivalent is `--canvas`), its edge a
  1px specular hairline and its shadow a soft ambient drop, so the double-shell cards inside keep
  their one tonal step (#fafafb cards on the #f3f4f6 island in luma light). The old flat,
  unframed content column of #1753 is superseded by the owner decision.

**The applet model.** Applets come from the app's navigation model (`SECTIONS` in `main.tsx`), never
from words in the shell: each destination names its applet (`Section.applet`) and, inside it, the
quiet heading it sits under (`Section.group`); a destination without one is an applet of its own
(Home, Inbox). Studio's applets: Home, Inbox, Data (records, views), Analysis (metrics, dashboards),
Operations (workflows, import triage), Learning (where the cell serves it), Builder (sources, the
semantic map, transforms, the canvas, rules, markings) and Settings (connections). Which ones show is
the server's: the catalog and the mode decide, as before. The page's applet is the one whose item
claims its route (an item that is the page itself first, then the longest claim, then the model's
order); a page two applets list, such as a type's record page, opens in the applet whose rail button
or menu row the reader followed (kept in the history entry, so Back restores it), else stays in the
applet the reader came from (`appletOf`). An item the reader pinned (a saved view's space) goes to the applet of that name,
under "Pinned views", or under its own heading in its destination's applet (`placeOf`).

**Applet rail (first island)**

- Top: the workspace switcher, a 32px disc (avatar tokens, hashed by id) named
  "Workspace: <name>"; its menu holds the workspaces and examples. Bottom: the assistant (when the app
  has one; pressed while open, the 50% wash, never the pill) and the principal's 24px disc, whose menu
  holds the account, the view, language, scheme, skin and chrome, and Sign out.
- Between them, the **"Applets" landmark**: one 40px button per applet (an `<a href>` to where the
  applet opens: its first destination with a page, or the first item of one without), a 18px icon,
  muted at rest, the 50% wash on hover, and the neutral pill with its own ink when current. Its name
  is its label plus its state ("Inbox, 3 open"), shown in an end-side tooltip; an applet with a count
  adds a 6px `--ring` attention dot (section 4.6). **Exactly one applet** carries `aria-current="page"`.
- It is a UI SDK vertical `Toolbar`: one tab stop, Up and Down (and Home, End) move between applets,
  Enter follows the link.

**Submenu island (second island)**

- Header: the applet's name (step 2, `--foreground`), and under it one quiet step-1 line with the
  workspace and its connection state ("Analytical · Live · Owner", "Connecting…", "Workspace
  unavailable"), so that state is visible text, never only the rail disc's tooltip (section 12.2).
- A **filter field** when the applet lists items of its own ("Filter <applet>"): it narrows the rows
  as the reader types (case and accents fold), a matching destination keeps all its items, lists are
  shown whole while filtering, a filter that matches nothing says "Nothing matches “…”" in a status
  line, Escape empties it, and Down moves into the list. It is a view filter over what the server
  already listed; it never asks the server for anything or hides what the server allowed.
- The **"<applet> menu" landmark** around a UI SDK `Tree` (the APG navigation treeview: arrows,
  Right and Left, Home and End, type-ahead, Enter follows the link; `current` sets the one
  `aria-current="page"`). Rows are real `<a href>` links.
  - Headings (`Section.group`, pinned headings) are quiet step-1 labels at 400, sentence case, 32px,
    collapsible, their chevron on hover and focus (always where the reader cannot hover, and in the
    phone sheet), turned 90° when open and mirrored in RTL; 12px above each but the first.
  - A destination row: 32px, a 16px icon (1.6 stroke), the label at step 2 and 400, muted at rest,
    the 50% wash on hover with no text change, the pill with its own ink when current; at most one
    end slot, a count from the server (never a `0` it did not send) or a 6px `--ring` dot.
  - Its items indent to the label column (`ps-[32px]`), with no icon, under sub-headings of the same
    style when the destination splits them (with a count).
  - **Show all.** A list of more than nine rows shows its first eight, the current row too when it
    falls past them, and a quiet step-1 "Show all <n>" row (a tree item: pointer or Enter); whole, it
    reads "Show fewer". This keeps fifteen record types from pushing the rest of the menu below the
    fold. The choice is kept while Studio is open.
  - **Empty.** A destination with no page whose items were read and are none says "None yet" in its
    end slot; one whose items are not read yet (pending or failed) claims nothing. An applet whose
    only rows are its destinations lists them, with no filter field.
- Headings, lists and Show all keep each applet's own state while Studio is open; the current row's
  heading and list are never hidden.

**Collapse and persistence**

- The submenu island collapses **to nothing** (rail and main island stay). At 1024px and wider it
  starts shown, from 640 to 1023px collapsed, and the reader's own choice, kept per browser
  (`norn.studio.sidebar`), wins at every width from 640px up. Collapsing animates its width over
  200ms ease-out; under `prefers-reduced-motion: reduce` it is instant. While collapsed it is `inert`
  and hidden from assistive tech.
- **Toggle**: a visible `PanelLeft` button (mirrored in RTL) at the start of the main island's top
  bar, `size-[32px]` (`size-[44px]` below 640px), named "Sidebar" in both states, with
  `aria-expanded` and, from 640px, `aria-controls` on the island's id. Its tooltip reads "Hide
  sidebar ⌘B" or "Show sidebar ⌘B", and `⌘B` / `Ctrl+B` toggles it. In the dock shell the shortcut
  is off: it stores no choice and opens nothing.
- **Focus is never stranded.** Collapsing while focus is in the island hands focus to the current
  applet on the rail, else to the toggle; focus anywhere else is left alone.

**Top bar (inside the main island).** `h-[52px]`, no border, on the island's own material. Start:
the toggle, then the breadcrumbs. End, in order: search (`⌘K`), page tools, the rules tick bar pill
(section 12.4), the language pill and the scheme toggle (restyle 2's `TopBarExtras`, which give way
as the bar narrows), appearance, shortcuts and notifications. The rail replaced the perspective
switch: there is no second control for the same choice. The workspace switcher and the principal
live on the rail.

**Connection line.** Under the top bar, one line that never wraps into a column (`min-h-[44px]`,
`flex-nowrap`): connecting slowly, reconnecting, or Workspace unavailable. The page's own Workspace
unavailable state is the full one (what the server said, the incident, Last reachable, Retry), so
while the page shows it the line is gone; on a page that does not, the line is the compact status
(a dot and "Workspace unavailable", the server's words and the incident from 640px up, one Retry).
Exactly one Retry and one incident id are visible (Playwright row "Workspace unavailable shows
once"); every Retry is at least 44px below 640px.

**Below 640px: one Navigation sheet**

- The rail and the submenu island leave the layout; the main island fills the window with no
  margin, edge or radius. The top bar's toggle opens a modal sheet from the start edge (Base UI
  `Dialog`: it traps focus, makes the page `inert`, closes on Escape and returns focus to the
  toggle), named "Navigation", `top-[8px] bottom-[8px] start-[8px]`,
  `w-[min(360px,calc(100vw-56px))]`, `--radius-sheet`, opaque `--shell`, `--sheet-shadow`.
- It holds the same two landmarks side by side: a 64px applet column (the workspace switcher at the
  top, the assistant and the principal at the bottom) and the open applet's menu with its header
  and a close button. In the column, each applet is a button that shows its menu beside it
  (`aria-pressed`); the applet that holds the page keeps `aria-current="page"`. Choosing an applet
  never navigates; following a row does, and closes the sheet.
- **Why a sheet, not a bottom tab bar:** eight applets do not fit a five-slot tab bar without a
  "More" overflow; a permanent bar costs about 64px of every phone screen, collides with the home
  indicator and the DetailSheet's one-CTA footer; and the two-pane sheet mirrors the desktop rail
  and island exactly, behind one trigger, reusing the Base UI dialog, its focus trap and the
  double-Escape fix (a menu open inside the sheet closes first; `details.cancel()` with
  `allowPropagation()` lets the UI SDK close it).
- Every target is at least 44 × 44 CSS px: applet buttons, rows and headings, the switcher and
  account discs, the close button (`aria-label="Close navigation"`). Motion: 150ms opacity plus a
  16px inline slide from the start edge, mirrored in RTL.

**Keyboard and accessibility**

- **Two navigation landmarks with distinct names**: "Applets" and "<applet> menu". The workspace
  switcher, the assistant and the account sit on the rail outside the "Applets" landmark.
- Roving focus in the rail (UI SDK `Toolbar`), the APG tree in the menu (UI SDK `Tree`), `⌘B`, and
  type-ahead; every control is a named primitive (section 16.1).
- **RTL.** The islands sit on the right in the same order from the start edge; chevrons, the toggle
  icon and the sheet's slide mirror; tooltips open towards the content (section 14).

### 9.2 `dock` (opt-in: kiosk and mobile-first operational apps)

The dock is an alternative frame for apps whose readers work standing up or on a phone, such as
kiosks and floor or field operations. It is never the default (owner decision, 2026-11-25): the
three-island shell is always the default, and an app may opt into the dock only as a non-default mode for
kiosk or mobile-first surfaces, chosen explicitly by the app. A reader may also pick it as a stored
preference (section 3). When the dock is on, every rule below applies in full, including the
accessibility rules.

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
    The dock may scroll horizontally on its toolbar axis; it never wraps. (CURRENT since restyle 2:
    `DOCK_BUTTON` and `DockLabel` in `shell.tsx`; the label is the start of each tile's name.)
- `prefers-reduced-transparency` or `glass` fallbacks keep the dock opaque enough for 4.5:1 labels; with
  `--dock-muted` #b4b4bc the glass-light composite (#3f414a) holds 4.93:1 (CURRENT in `index.css`).

### 9.3 `minimal` (route-level, TARGET)

Chromeless canvas for embedded or single-task surfaces (kiosk, OAuth, login). It is **not** a
`data-shell` value: the route decides it and renders outside the shell, so a stored shell
preference can never strip the chrome from an operator view or add it to a login page. No dock, no
sidebar; the page still has a title and an honest exit.

---

## 10. Canonical primitives

One shared implementation each, in `@norn/ui` (TARGET) or Studio's `src/components/system/` today
(CURRENT since restyle 2: `ListRow`, `RowList`, `Initials`, `TypeDisc`, `StatusDot`, `Dot`,
`SourceTag`, `Chip`, `Panel`/`Well`/`EmptyState`, `KpiTile`/`StatStrip`, `TableCard`, `TickBar`,
`LanguagePill`, `SchemeToggle` and the UI SDK kit). That folder imports only tokens, `cn`, Base UI,
the UI SDK and its own files, and takes every value through props, so it moves into `@norn/ui` as
it is (`test/system-boundary.test.ts`). Screens compose these; they do not restyle them with ad-hoc
classes.

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
well inside), popovers/menus/tooltips, DetailSheet and dialogs, the sidebar and its mobile sheet,
the dock, hover/selection states of rows, and dashed Blueprint outlines for uncommitted proposals.
A frame inside a well, three levels of borders, or a card inside a card is a defect. A framed
content panel around the page's ShellCards is a card inside a card. Open streams, spatial maps and
canvases may still sit directly on the canvas with no card.

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

The top bar's **rules tick bar** (CURRENT since restyle 2, `TickBar` + `RulesTickBar`, which matches the
workspace and each read through `lib/state`'s `matchWorkspaceState` / `matchReadState`) shows the
workspace's live laws, from two server reads only: the laws the catalog lists, and the held-back
groups `GET /v1/inbox` returns. Ten hairline ticks fill in `--dot-held` for the share of rules the
inbox lists as holding records back (at least one tick for any share, never all ten while one rule is
not), with an `n/m` count and a popover that lists every rule in plain words with a held StatusDot and
its count where the inbox names it. A rule the inbox does not list makes no claim; "nothing held back"
appears only when the inbox's held-back section counted zero, exactly; a scan bound or a capped group
list reads as a lower bound ("at least", `+`). With no live cell or no laws it renders nothing.
**Server gaps (TARGET):** the cell serves no per-guarantee "satisfied" verdict and no current receipt
for a live workspace, so the bar never fills `--dot-live` and shows no receipt; both wait on a server
read, never a client inference.

### 12.5 Empty

EmptyState (section 10). Say what will fill it and offer the next step only when the server says
the principal may take it.

---

## 13. Status and colour discipline

- Saturated colour appears only in: status dots beside their word (success, warning, error, in muted
  tones), data bars and chart marks, avatar discs, the Norn mark and its mascot, and data highlights
  (search matches, diff marks). The primary CTA, the focus ring, links, tabs, inputs, chips, borders,
  icons and the navigation attention dots are neutral (section 19.6, gated by `test/monochrome.test.ts`).
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
- Tables, schedules and the dock's order mirror, and the three islands move to the right side. Charts
  with a time axis keep time running in the reading direction of the locale.
- Every screen ships with an Arabic Playwright row and an RTL screenshot (section 16).

---

## 15. Motion

- Popups and menus: 150ms opacity + scale 0.95 from `--transform-origin`.
- Submenu island: collapse and expand animate its width over 200ms, ease-out. The phone Navigation
  sheet moves like the DetailSheet below, from the start edge. Under reduced motion both are instant.
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
  `sidebar`, `floating-dock`) so tests, skins and agents target structure, not class strings.
- Selection and open sheets live in the URL, so an agent and a person share state by link.

### 16.2 Gates (every PR touching UI)

1. **Domain-agnostic Studio** (`apps/studio/test/domain-words.test.ts`, CURRENT). No allowlist.
2. **No Radix** (`apps/studio/test/ui-primitives-guard.test.ts`, CURRENT).
3. **Contrast** (`apps/studio/test/contrast.test.ts`, `studio-theme.test.ts`, CURRENT; extend to
   the new tokens): across every skin × scheme, text ≥ 4.5:1 on its real surface (card **and**
   well, resting, hover, selected), including step-5 figures and their units; `--action` as text ≥
   4.5:1 on the same surfaces; `--primary-foreground` on `--primary` and on `--action-hover` ≥
   4.5:1; focus rings, graph edges, chart marks, `--dock-indicator` on the dock and the sidebar's
   `--ring` attention dot on its ground, hover wash and pill ≥ 3:1; avatar pairs ≥ 4.5:1. Other
   dots are exempt only
   because they are always paired with a label; a gate check for that pairing is TARGET (see
   section 18).
   `--subtle-foreground` may not carry information.
4. **Touch:** interactive targets ≥ 44 × 44 CSS px below 640px (sidebar rows, group headers and
   toggle, dock, ListRow, pills, buttons, sheet close, form inputs), written in px; ≥ 32px
   elsewhere.
5. **Focus:** every control shows `focus-visible` outline 2px in `--ring` (dock: `--dock-ring`).
6. **Type lock (CURRENT lint, `test/type-lock.test.ts`):** across every file in `src/`: no Tailwind
   or arbitrary `text-[…]` sizes, no weight but 400 and 500, a bare `font-medium` only beside a step-3
   or step-4 title or on the primary fill (state variants such as `aria-pressed:font-medium` mark the
   active segment), no uppercase or tracked chrome, logical inline properties only (section 14), no
   translucent focus ring, and a `--dot-*` fill only inside the dot primitives, so every dot sits
   beside its word. `b`, `strong` and `th` inherit their weight (`index.css`).
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
- **Visual references** (inspiration, not copies): Polar's dashboard (an icon rail beside a context
  panel) and the ChatGPT desktop app's sidebar (the default shell's three islands), the shadcn
  liquid-glass registries (glasscn, Glass UI) and Panelo (the CSS glass material), macOS source
  lists in Finder, Notes and Mail (the menu rows), `polar.sh` (monochrome craft, carbon dark),
  `getopen.so` (open canvas, dark pill dock, now opt-in), The Weave (source-coloured provenance
  threads), Floorplan (MIT; semantic zoom, Blueprint overlays), Archify (Apache-2.0; `SRC` badges,
  diffs), `bilalgursen/liquid-glass` (MIT; optional single WebGL2 shader behind `glass`, CSS
  fallback mandatory).

### 17.1 Token diff: Studio `index.css` vs the first-party app

Same token, different values. "Pick" is what this spec adopts.

| Skin / scheme | Token | Studio | First-party app | Pick | Why |
| --- | --- | --- | --- | --- | --- |
| all | default skin | `luma` | `open` | `luma` | **Owner decision, 2026-11-25 (second):** "use luma theme". It supersedes the `polar` pick made earlier that day (#1734, #1753), which reversed the `open` pick from #1692. `polar`, `open` and `glass` stay one choice away. |
| all | default shell | `sidebar` (three islands) | dock | the three-island shell (section 9.1) | **Owner decision, 2026-11-25 (second):** a thin applet rail, the submenu as a second island, then the main island, "KINDA like Polar" or the ChatGPT desktop app with submenu items in place of threads, in liquid glass. It supersedes the single thin sidebar from #1753 and the `dock` pick from #1692. The dock is never a default (section 9.2). |
| all | shell geometry | 232px source list, 48px rail | — | 64px rail, 256px submenu island that collapses to nothing, main island; 8px apart, 18px radius | Px so it does not scale with the root. 64px fits a 40px applet button in 12px gutters; 256px keeps the 176px label column of the old list plus the filter field. |
| all | sidebar selection | `--background` fill, ring, `shadow-control`, 2.5px `--action` start bar, `--action` icon, weight 500 | — | `--sidebar-accent` pill, `--sidebar-accent-foreground` ink, weight 400 | A soft pill with no border or colour in chrome (sections 4.6 and 13). The ink step and `aria-current` back up a fill of 1.06–1.26:1. |
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

Measured on `origin/main` at `ff0f95bd5e`, except the shell gaps, which were measured on
`ui/shell-v2` (shell v2, the three-island shell). A checked item was closed by the Studio restyle (#1753) or restyle 2 (Part of #1705); an open item that one narrowed says what remains. Counts are `grep` occurrences in
`apps/studio/src`, and `file:line` references are relative to `apps/studio/src` at the stated sha.

**Tokens and theming (`src/index.css`, `src/lib/appearance.tsx`, `src/lib/studio-theme.ts`)**

- [x] Radii in rem (scale with the 90% root); `polar`, `luma`, `glass` radii break the concentric
  rule; `[data-skin="polar"] [data-slot="panel"] { padding: 0 }` and the polar KPI well override
  flatten the double shell.
  Closed in #1753: px radii in every skin, and the 4px inset double shell in every skin.
- [x] Missing tokens: `--avatar-*`; and the StudioTheme allowlist does not yet carry the colour
  tokens #1753 added (`--subtle-foreground`, `--line`, `--action-dot`, `--action-soft`, `--dot-*`).
  #1753 added those and `--popup-shadow`, `--sheet-shadow`, `--panel-inset` and
  `--radius-row/sheet/search/dock/popup`.
  Closed in restyle 2 (Part of #1705): `--avatar-1…6-bg/fg` (section 4.4), and every new colour token in the allowlist with its validator pairs (section 3); the "Paper Editorial" preset gained its own `line` and `subtle-foreground`, which the new pairs caught.
- [x] Solid `bg-action` fills (4 uses) put `--action-foreground` on the text-role `--action`: 2.78:1
  on open-dark #6b98ff and 3.62:1 on luma-dark #7c74ff. Solid fills move to `bg-primary`.
  Closed: no solid `--action` fill carries text (checked in #1753); the unapplied-filter marker is a `--ring` dot, since it has no word beside it and `--dot-accent` misses 3:1 in open, luma and glass light.
- [x] The `polar` default holds only once JS runs. Bare `:root` (`index.css:24`) and the
  `:root:not([data-skin])` rules (`index.css:487-502`) are the `open` palette and geometry.
  `polar` arrives only when `main.tsx:129` calls `applyAppearance`, which sets `data-skin`
  (`lib/appearance.tsx:88`), and `index.html` has no pre-paint script. So the first paint, and any
  page that loads `index.css` without that call, renders `open`. Spec: the bare-root palette is
  `polar`, so the default holds without JS.
  Closed in #1753: bare `:root` and `.dark` are the `polar` palette and geometry; `open` is `:root[data-skin="open"]`.
- [ ] `--font-sans` has no Arabic pairing (no Cairo; not in `package.json`).
- [ ] No `minimal` route-level chromeless frame; login and OAuth use shadcn `Card`.
  Narrowed in restyle 2: `Card` is the ShellCard shape (rim, `--panel-inset`, one inset well), so login, OAuth, the restatement card and the widget cards follow it; login's mode switch is a segmented control. The route-level `minimal` frame itself remains.
- [x] Type tokens are 0.8125/0.9375/1.0625/1.75/2rem; body is 0.875rem.
  Closed in #1753: the five steps of section 8.1 (with line heights and tracking), body at step 2; `--text-panel` and `--text-section` are deleted, and `cn` (tailwind-merge) knows the steps as their own class group, which conflicts only with font sizes, so `leading-*` and `tracking-*` survive beside a step.

**Weights and sizes**

- [x] `font-semibold`/`font-bold`: 41 uses in 23 files. Most: `shell.tsx` (7),
  `decision-tree.tsx` (5), `imports/split-pane-review.tsx` (3), `components/canvas/node-card.tsx`
  (3), `views.tsx`, `sources.tsx`, `dashboards.tsx`, `canvas.tsx` (2 each), and
  `components/panel.tsx` (`EmptyState` title).
  Closed in restyle 2 (Part of #1705): none remain anywhere in `src/` (`test/type-lock.test.ts` enforces it).
- [x] `font-medium`: 346 uses; most are outside the allowed four. Heaviest:
  `imports/split-pane-review.tsx` (20), `shell.tsx` (18), `imports/supersession-diff.tsx` (18),
  `semantic-map.tsx` (17), `sources.tsx`, `objects.tsx`, `guarantees.tsx` (10 each);
  `components/chip.tsx` sets 500 on every chip.
  Closed in restyle 2 (Part of #1705): none remain anywhere in `src/` (`test/type-lock.test.ts` enforces it).
- [x] Arbitrary `text-[…]` sizes: 81 uses (`text-[0.6875rem]` ×42, `text-[11px]` ×14, `text-[10px]`,
  `text-[0.625rem]`, display sizes 2.75–4rem). Heaviest: `imports/split-pane-review.tsx` (14),
  `shell.tsx` (12), `semantic-map.tsx` (7), `imports/alias-matcher.tsx` (6),
  `imports/supersession-diff.tsx`, `decision-tree.tsx`, `components/canvas/node-card.tsx` (5 each).
  Closed in restyle 2 (Part of #1705): none remain anywhere in `src/` (`test/type-lock.test.ts` enforces it).
- [x] Uppercase chrome: `semantic-map.tsx` (5), `views.tsx`, `shell.tsx`, `guarantees.tsx`.
  Closed in restyle 2 (Part of #1705): none remain anywhere in `src/` (`test/type-lock.test.ts` enforces it).

**Status and colour**

- [x] Filled tinted status pills: `components/chip.tsx` (`Chip` fills every tone) used in 21 files;
  heaviest `widget/views.tsx` (18), `learning/proposals-page.tsx` (15), `laws.tsx` (9),
  `imports/split-pane-review.tsx` (9), `learning/cases-page.tsx` (8), `learning/misfit-page.tsx`,
  `objects.tsx`, `learning/episodes-page.tsx`, `imports/supersession-diff.tsx`,
  `evidence/evidence-crop.tsx`. Status → `StatusDot`; kinds and sources → outline `SourceTag`.
  Closed in #1753: `Chip` renders a state as a 6px dot and its word, and a neutral chip as an outline SourceTag, in every file that uses it.
- [x] `components/fact-status-badge.tsx` renders the honesty states as filled chips; needs dot-first
  with the same precedence.
  Closed in #1753: dot-first, same precedence.
- [x] shadcn `Badge` as status/kind: `transforms.tsx` (8), `sheet.tsx` (8), `views.tsx` (6),
  `semantic-map.tsx` (6), `workflows.tsx` (5), `sources.tsx` (4), `dashboards.tsx` (3).
  Closed in #1753: `Badge` variants are outline tags or dot-and-word states; none fills.

**Nesting and primitives**

- [x] `components/panel.tsx`: `Well` is a flush `border-t` body, not an inset well; nested panels
  turn into ruled sections. Align to ShellCard (one inset well).
  Closed in #1753: every Panel is the rim around one inset well.
- [x] `components/table-card.tsx`: adds its own rim + well when not inside a Panel (a second
  nesting pattern).
  Closed in restyle 2 (Part of #1705): a `TableCard` is always a Panel's well; outside one it brings its own `Panel` (with a header when given a label), and inside a well it is a ruled part of it.
- [x] `components/kpi-tile.tsx`: close to StatWell; label uses `text-sm`, no unit slot.
  Closed in #1753: a StatWell with a step-2 label and a unit slot.
- [x] shadcn `Card` instead of ShellCard: `login.tsx`, `oauth.tsx`, `restatement.tsx`,
  `widget/views.tsx`.
  Closed in restyle 2 (Part of #1705): the `Card` primitive is the ShellCard shape for all four.
- [ ] No shared `ListRow`, `StatusDot`, `SourceTag`, `Initials`, `Props`, `Section` primitives; lists
  are built per screen.
  Narrowed in restyle 2: `ListRow`, `RowList`, `StatusDot`, `SourceTag`, `Initials` and `TypeDisc` are shared (`components/system/`), and the builder lists (imports, semantic map, laws, transforms, sources, the canvas problems list, saved questions) and Home's checklist use them; `SheetSection` is the sheet's Section. A shared `Props` remains.

**Sheets**

- [x] `components/ui/sheet.tsx` is edge-attached, full height, `w-3/4`, physical
  `left/right/border-l/border-r`; no 12px inset, no 24px radius, no single-CTA footer. Used by
  `answer-view.tsx`, `assistant-slot.tsx`, `dashboards.tsx`, `actions/action-sheet.tsx`,
  `decisions/decision-sheet.tsx`, `components/provenance.tsx`, `questions.tsx`,
  `dashboards/tile-view.tsx`, `objects.tsx`, `grid/records-grid.tsx`, `learning/cases-page.tsx`,
  `lib/inspector.tsx`. Widths vary (`sm:max-w-lg` to `sm:max-w-4xl`). Replace with one DetailSheet.
  Closed in #1753: `SheetContent` is the floating inset DetailSheet (12px inset, 24px radius, `--sheet-shadow`, logical sides) with `SheetBody`, `SheetSection` and a footer for one primary action; callers no longer set their own widths except the wide answer and dashboard sheets.

**Three-island shell, the default (`shell.tsx`, `shell-nav.tsx`, `components/ui/sidebar.tsx`; sections 4.6, 4.7 and 9.1)**

The single source-list sidebar's gaps were all closed by #1753, and that sidebar is itself
superseded by the three-island shell (owner decision, 2026-11-25). Shell v2 closes the audit's
"15 record types push everything below the fold" (Show all) and the duplicated Workspace unavailable
banner (the connection line, section 9.1). What remains:

- [ ] Show all and the headings a reader opened or closed are kept only while Studio is open; the
  submenu island's own collapse is kept per browser.
- [ ] Home and Inbox list only their own page in the submenu island. A "Recent" list of the records,
  runs and views the reader opened (local view state, kept per browser, with Clear) is TARGET.
- [ ] Applets are declared per app in its model (`Section.applet` in `main.tsx`); `@norn/ui` has no
  applet primitive for generated apps yet (TARGET).
- [ ] The rail shows each applet's label only as its name and tooltip; there is no
  label-under-icon variant for readers who want words on the rail.
- [ ] The connection line learns that the page shows the full Workspace unavailable state by
  watching the page for `[data-workspace-unavailable]`; TARGET: one owner of the workspace's state
  (`lib/workspace.tsx`) tells both.
- [ ] The glass is measured over the backdrop and its glows only. The page scrolls as a document
  inside the main island, so no page content passes under a translucent island today; a shell that
  scrolls content under an island must measure that too.
- [ ] The dock shell (opt-in) still lists destinations, not applets.

**Dock shell, opt-in (`shell.tsx`; section 9.2)**

These gaps apply only when an app or a reader opts into the dock, and the dock's rules still bind
whenever it is on.

- [x] `FloatingDock` is icon-only at every width, with no labels below 640px. Its buttons are
  `size-10` (36px at the 90% root), under the 44px target.
  Closed in restyle 2 (Part of #1705): 40px buttons, and below 640px labelled tiles of at least 44 × 44px.
- [x] The dock's attention dot uses physical `right-[7px]` and is colour-only, with no accessible
  text.
  Closed in #1753: `end-[7px]`, and the destination's name carries its count.
- [x] The dock-mode top bar (`data-slot="dock-topbar"`) is borderless, but it is `h-[4.4rem]` on
  `bg-background` and has no guarantee tick bar, language pill or scheme toggle.
  Closed in restyle 2 (Part of #1705): `h-[4.5rem]` on `--canvas`, with the three extras before the principal's 36px disc.
- [x] `index.css` `--dock-muted` is #a5a5ad (4.15:1 on the glass-light dock composite #3f414a);
  the spec value is #b4b4bc (4.93:1) (section 4.5).
  Closed in #1753.

**RTL**

- [x] Physical spacing in first-party code: `actions/action-form.tsx` (3),
  `learning/episodes-page.tsx`, `learning/episode-page.tsx`; vendored `components/ui/*`
  (`dropdown-menu`, `button`, `toggle`, `input-group`, `sidebar`, `sheet`) when touched.
  Narrowed by #1753: the episode pages, the records grid, linked records and the touched primitives are logical; `actions/action-form.tsx` remains.
  Closed in restyle 2 (Part of #1705): every file in `src/`, vendored primitives included, is logical, and the type-lock lint keeps it so (centring with `left-1/2 -translate-x-1/2` is the one allowed physical pair).
- [ ] No Arabic font; no `<bdi>` discipline for ids and money.
  Narrowed in restyle 2: `SourceTag` and `ListRow`'s code are `<bdi>`. Cairo and a `<bdi>` rule for money remain.

**Refusals and call outcomes (`src/lib/actions.ts`, `src/lib/screen/call.ts`)**

- [ ] `refusalOf` drops the wire's `category` and `retryable` and makes `explanation` optional; the
  `refused` arm of `CallResult` and the `refused` call phase carry neither, so no screen can show the
  category icon or a server-directed Retry (section 12.3).
- [ ] `listActions`' `refused` arm keeps only `status`, `code` and `message`.
- [ ] `callAction` and `listActions` map every 404 and 503 to `unavailable` without using the body,
  so a typed `Refusal` on those statuses is never shown (section 12.4).
- [ ] An untyped 503 renders the unsupported-route copy with no Retry; it should read as a temporary
  outage with Retry, and only an untyped 404 keeps "doesn't take actions" (section 12.4).

**Agent operability**

- [ ] No UI SDK `createAgent` wiring; the Sidekick (`sidekick/client.ts`) does not drive named
  controls. List rows have no `data-slot`.
  Narrowed in restyle 2: every `ListRow` is `data-slot="list-row"` in a `data-slot="row-list"`; `createAgent` remains.

**Gates**

- [x] Contrast gate does not yet cover the new tokens (subtle, avatars, action-foreground on
  action in dark, muted on `--well-hover`), and nothing checks that every dot is paired with a
  text label.
  Closed in restyle 2 (Part of #1705): subtle and avatar pairs gate in the validator and the tests; a `--dot-*` fill may live only in `StatusDot`/`Dot` (whose word is required) and the dot-first `Badge`, so a dot cannot appear without its word (`test/type-lock.test.ts`).
  Narrowed by #1753: the section 4.6 sidebar pairs (ground, 50% wash, pill, ring and dot, phone sheet) gate every skin in the test and in the StudioTheme validator; subtle and avatar pairs remain.
- [x] No 44px mobile minimum on rows, pills or buttons (`h-9` is 32.4px at the 90% root).
  Narrowed by #1753: buttons (default, sm, lg), the sidebar sheet, the sheet close and the record picker are 44px below 640px; rows, pills and inputs elsewhere remain.
  Closed in restyle 2 (Part of #1705): `ListRow`, `Input`, `Toggle`/`ToggleGroup` segments, the top-bar pills, the dock and the builder screens' rows, segments and icon buttons are 44px below 640px.
- [x] No lint for arbitrary type sizes, banned weights or physical spacing.
  Closed in restyle 2 (Part of #1705): `test/type-lock.test.ts` (gate 6).

---

## 19. The AI surface (CURRENT since `ui/ai-surface`)

Owner decision, 2026-11-25 (third, quoted at the top). Everything here is CURRENT unless marked.

### 19.1 Luma is shadcn's Luma, in Polar's monochrome

- **Selected, not imitated.** `components.json` names `base-luma` (Base UI, since first-party code has
  no Radix; `rtl: true`, so the CLI writes logical classes). `components/ui/*` are derived from the
  shadcn CLI 4.23.0 output for `base-luma` (`npx shadcn@4.23.0 add <name> --dry-run --view …`), with
  Norn's deltas recorded in `components/ui/provenance.json`: `cn` from `@/lib/utils` (it knows the type
  steps), the type steps and weights of section 8, the focus outline of section 16.2, 44px targets
  below 640px, and the Base UI / UI SDK backing. New this round: `alert`, `scroll-area`, `spinner`,
  `message`, `bubble`, `trace`.
- **Geometry is Luma's** (section 5): pill buttons, toggles and tabs, `rounded-3xl` popovers and
  menus, `rounded-4xl` cards and dialogs, through `--radius-step-*`. Two places follow the measured
  references instead (section 19.6): inputs are white 1px-hairline fields at 12–14px, not Luma's filled
  pills, and the islands are opaque.
- **Colour is Polar's and Open's monochrome** (section 19.6): pure R = G = B greys. Luma's own
  `neutral` theme is already grey; the measured references set the exact steps.
- **The HR non-negotiables hold.** Double-shell cards (Luma has single cards: the rim takes Luma's 4xl
  radius, the well sits 4px inside, as in Open's KPI cards), weights 400/500 (Luma sets `font-medium`
  on buttons, badges and menu rows: Norn keeps 400 except where section 8.2 allows 500), dot + word for
  status, the floating sheet with one primary action, and the five-step type scale (Luma's
  `text-sm`/`text-xs` become `text-body`/`text-meta`; the title step moved to 21.6px).
- **Focus** stays the 2px `--ring` outline (Luma's `ring-3 ring-ring/30` misses 3:1; gate 5).
- `polar`, `open` and `glass` keep their crisp geometry (8px controls, 11px rows, 5px chips, 18px
  islands) through their own `--radius-*` and `--radius-step-*`.

### 19.2 No sparkle

The sparkle "AI" glyph is banned everywhere in `src/`: no lucide `Sparkle`, `Sparkles`,
`WandSparkles` (any `Wand`) or `Stars` icon, named, aliased or deep-imported, no ✨, and no drawn "AI"
lettering (the old dock's `AiGlyph`). `test/type-lock.test.ts` fails on any of them, with no
allowlist. The assistant is the mascot (section 19.4) or its name in words ("Sidekick", "Suggested:",
"Proposed by the assistant"); a non-AI place that used a sparkle takes a plain icon (the Get started
checklist: `ListChecks`; theme presets: `Palette`).

### 19.3 The composer: shadcn's chat primitives over `useChat`

- **Premade and official.** Sidekick is drawn with shadcn's own chat primitives, installed with the
  shadcn CLI from the `base-luma` registry: `Message` (with `MessageAvatar` for the mascot), `Bubble`
  (the person's words, `muted`) and `Trace` (each tool call, collapsible, its state a dot and a word).
  The assistant's words render with Streamdown. The composer (`components/chat-composer.tsx`) is
  shadcn's `input-group` with a textarea, the shadcn docs' chat-composer pattern; the Ask Norn bar is
  the same composer on one line.
- **Why not AI Elements.** AI Elements (Vercel) was the first pick, but it is Apache-2.0, and the
  provenance gate admits only MIT for vendored software (`crates/norn-skeleton-guard/src/ci/provenance.rs`,
  "invalid MIT software provenance"). Widening that list is a gate change for the owner, not a lane;
  shadcn's own MIT primitives are the "official shadcn" choice the owner preferred anyway. The AI SDK
  packages are npm dependencies, not vendored files.
- **Chat state: the AI SDK's `useChat`** (decided by a spike, 2026-11-25). Both candidates need an ACP
  adapter, because Sidekick's bridge speaks ACP over a WebSocket, not HTTP streaming:
  - AI SDK (`ai` 7.0.169): a custom `ChatTransport` (`sidekick/transport.ts`) maps each ACP update to a
    UI message chunk (`sidekick/transcript.ts`). Approvals map one to one: the bridge's
    `session/request_permission` is pending inside the running turn, and the UI stream takes
    `tool-approval-request` and `tool-approval-response` chunks mid-turn with no new request
    (`ai/dist/index.js`), so the transport answers the pending question in place and records the
    person's answer in the same stream, in order with what the bridge sends next.
  - TanStack AI (`@tanstack/ai-react` 1.0.2, `@tanstack/ai-client` 1.1.0): approvals resume through
    interrupts, which call the connection again with the resolutions (`chat-client.js`,
    `resumeInterruptsUnsafeForGeneration`), a second request ACP has no place for.
- **No half-features.** Send (Enter; Shift+Enter is a new line; IME composition is the input
  method's), Stop (cancels the turn at the bridge and every open question), Retry (after an error,
  `regenerate` asks again), New chat, suggestions while empty, the scroll-to-latest button, and
  keyboard and screen-reader names on every control. No attachments: the server takes none.
- **The server decides.** Tool calls are still Norn MCP calls the cell authorizes as the person.

### 19.4 The mascot

`components/system/mascot.tsx`: an original character, a soft pebble with two eyes and a looped
thread standing on its head (the knot of the Norn mark). It is in the genre of friendly character
mascots the owner named, and copies none of them.

- **States:** `idle` (blinks now and then), `thinking` (the thread sways, the gaze lifts, a 0.8px
  bob), `answered` (happy arcs and a small smile), `unavailable` (eyes closed to lines: refused, no
  bridge, or an error), `asking` (round eyes and a raised hand: waiting on the person's approval).
  Motion is CSS in `index.css`; under `prefers-reduced-motion` every pose holds still.
- **Tones:** `brand` (body `--mascot-body`, its own violet #5b52f0 / #7c74ff, the one colour on the
  AI surface; eyes `--mascot-eye` #ffffff: 5.37:1 light, 3.62:1 dark, a graphic so 3:1) for avatars and
  empty states; `ink` (`currentColor`, eyes cut out by
  a mask) for monochrome chrome: the rail and dock button, the Ask bar's lead, badges.
- **Direction:** it mirrors in RTL (`rtl:-scale-x-100`), so the hand and the gaze point to the
  reading end.
- **Where:** Sidekick's avatar (its pose follows the turn), Sidekick's empty state, the Ask Norn bar,
  the rail and dock assistant button, the dashboard assistant, and the decision tree's agent pick.
- **Candidates:** `pebble` (the pick), `loom` (a yarn ball with a wound thread) and `drop` (a drop
  standing point-up, its tip curling into a thread), all on the contact sheet (`mascot/candidates`).
  Pebble wins because its silhouette and two eyes still read at 16–20px in the rail, and its thread
  loop carries the knot and the thinking motion. The owner can swap it with `variant`.

### 19.5 Approvals stay on the server

The bridge (`apps/sidekick-bridge/relay.ts`) pre-allows only the Norn tools the cell's own
`tools/list` marks `readOnlyHint: true`. For any other Norn tool the agent must ask; the bridge
forwards that `session/request_permission` to the page with only "allow once" and "reject", takes
back only an option it offered (anything else is the reject), and answers `cancelled` for every open
question when the person stops the turn. If `tools/list` fails, nothing is pre-allowed. The page
draws the question as the tool's Confirmation and relays the person's pick; it holds no rule of its
own. The cell still decides every call, so an allowed call can still be refused (section 12.3), and
an action that needs another person's sign-off still parks as a decision (section 12.4).

### 19.6 Monochrome chrome (owner, 2026-11-25)

In the owner's words: "again, i said monochrome, black/white/greys like polar … in the main islands,
etc. colours only for highlights, visualisations, etc." Values are measured from Polar's dashboard (its
Apache-2.0 source) and the getopen.so demo (`getComputedStyle` and pixel samples); the reference spec
and screenshots are internal.

- **Pure greys in the default skin.** Every chrome colour `luma` declares has R = G = B, shadows, dock,
  islands and kind icons included: no cool or warm cast.

  | Role | Light | Dark | Reference |
  | --- | --- | --- | --- |
  | Backdrop, nav ground | `#f6f6f6` | `#090909` | Open's well, Polar's canvas |
  | Main island / card, popover | `#ffffff` | `#111111` / `#181818` | Polar's island |
  | Island and card hairline | `#e5e5e5` | `#212121` | Polar's `#e5e7eb`, without its cast |
  | Well (inside a card) / hovered | `#f6f6f6` / `#f0f0f0` | `#1e1e1e` / `#2a2a2a` | Open's well |
  | Selected nav pill | `#ebebeb` | `#212121` | Polar's and Open's selected row |
  | Text / muted / decorative | `#171717` / `#6d6d6d` / `#8a8a8a` | `#ededed` / `#969696` / `#7a7a7a` | Open |
  | Primary (CTA) and its ink | `#181818` / `#fafafa` | `#f4f4f4` / `#181818` | Polar's black and white pills |
  | Link, focus ring | `#171717`, `#737373` | `#a1a1a1`, `#a1a1a1` | neutral; the ring holds 3:1 |
  | Kind icons (`--kind-*`) | `#5c5c5c` | `#a3a3a3` | neither reference colour-codes types |
  | Dock (opt-in) | `#2b2b2b`, active `rgba(255,255,255,.1)`, inactive icons 40% white | same | Open's dock |

  The spec named `#18181b` / `#f4f4f5` for the CTA (polar's zinc row); the default takes `#181818` /
  `#f4f4f4` so that the R = G = B rule holds without an exception (a 3/255 step in blue).
- **No glass, no tinted shadow in the default.** The islands are opaque with a 1px hairline and a
  shadow-xs (`0 1px 2px rgb(0 0 0/.05)`); the ambient pools are off; cards have no shadow; popovers and
  sheets take Polar's soft, neutral `shadow-md`; `--elevation-action` is neutral. This supersedes the
  liquid-glass islands of sections 4.7 and 9.1; the `glass` skin keeps a plain blur, without
  `saturate()`, as an opt-in.
- **Type:** page titles are 21.6px at 500 (the title step, between Open's 20px and Polar's 24px); no
  step was added.
- **Colour is allowed only in named tokens:** `--chart-1…5` and the canvas's own tints, edges and
  nodes (visualisations), `--dot-live`, `--dot-held`, `--dot-bad`, `--success`, `--warning`,
  `--destructive`, `--destructive-text` (status, beside a word), `--avatar-n-*`, `--mascot-body` (the
  mascot), and `--tone-*` inside the data they mark (a masked entity, a diff mark). `--knot` is
  neutral; the mascot has its own colour token.
- **Category marks are neutral.** A `Chip` or `StatusDot` in a kind tone (a source system, a version,
  a role) draws the neutral mark; its word tells it apart.
- **The gate:** `test/monochrome.test.ts` fails when a chrome colour in `luma` (light and dark) is not
  R = G = B, when a chrome token in `polar`, `open` or `glass` has OKLCH chroma of 0.02 or more (those
  skins keep a zinc cast, about C 0.005, but no hue), when the CTA, accent text or link is not near
  black on light and near white or grey on dark, or when a category dot takes a hue. No allowlist
  beyond the named data, status, avatar and mascot tokens.
- **Measured** (section 4.2's formula; light: island `#ffffff`, well `#f6f6f6`, hovered well `#f0f0f0`;
  dark: `#111111`, `#181818`, `#1e1e1e`, `#2a2a2a`; h marks the hovered well, the minimum):

  | Scheme | figure | muted | `--action` | `--link` | `--ring` | subtle | CTA ink on fill / hover | nav: muted on hover wash / pill ink / ring on pill |
  | --- | --- | --- | --- | --- | --- | --- | --- | --- |
  | luma L | 15.73 h | 4.54 h | 15.58 h | 15.73 h | 4.16 h | 3.03 h | 17.01 / 12.10 | 4.58 / 15.04 / 3.98 |
  | luma D | 12.26 h | 4.85 h | 13.05 h | 5.56 h | 5.56 h | 3.34 h | 16.14 / 12.58 | 6.17 / 13.75 / 6.23 |

  These supersede the luma rows of sections 4.2, 4.6 and 4.7; `open` and `glass` take polar's ink row
  for the CTA, ring and link. `test/contrast.test.ts` and the StudioTheme validator gate every skin and
  scheme.
