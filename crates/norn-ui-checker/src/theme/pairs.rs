//! The ink/surface pairs Studio renders, each with its WCAG floor: Studio's `PAIRS`
//! (`apps/studio/src/lib/studio-theme.ts`), in its order. A surface lists its layers from the
//! bottom up, each a token at an alpha (Tailwind's `/p` modifier), over the scheme's canvas.

/// One painted layer: a token at an alpha.
pub(super) type Layer = (&'static str, f64);

/// One measured pair: the ink token, where it sits, the surface under it, and the floor.
pub(super) struct Pair {
    pub(super) ink: &'static str,
    pub(super) label: String,
    pub(super) surface: Vec<Layer>,
    pub(super) floor: f64,
}

/// A decorative hairline only has to stay visible against its surface.
const HAIRLINE: f64 = 1.1;
/// A status dot beside its word only has to stay a visible mark, not carry the state alone.
const MARK: f64 = 1.5;

fn layers(tokens: &[&'static str]) -> Vec<Layer> {
    tokens.iter().map(|token| (*token, 1.0)).collect()
}

fn on(base: &[Layer], top: &[Layer]) -> Vec<Layer> {
    base.iter().chain(top).copied().collect()
}

fn pair(ink: &'static str, label: impl Into<String>, surface: Vec<Layer>, floor: f64) -> Pair {
    Pair {
        ink,
        label: label.into(),
        surface,
        floor,
    }
}

/// DESIGN.md 4.6 and 4.7: what sits on the islands over one backdrop.
fn island_pairs(place: &str, backdrop: &[Layer]) -> Vec<Pair> {
    let island = on(backdrop, &[("island", 1.0)]);
    let card = on(&island, &[("card", 1.0)]);
    let well = on(&card, &[("well", 1.0)]);
    let nav = on(backdrop, &[("sidebar", 1.0)]);
    let hover = on(&nav, &[("sidebar-accent", 0.5)]);
    let pill = on(&nav, &[("sidebar-accent", 1.0)]);
    let main = format!("main island over {place}");
    let on_card = format!("card on the main island over {place}");
    let on_well = format!("well on the main island over {place}");
    let on_nav = format!("nav island over {place}");
    let on_hover = format!("nav island hover wash over {place}");
    let on_pill = format!("nav island pill over {place}");
    vec![
        pair("foreground", &*main, island.clone(), 4.5),
        pair("muted-foreground", &*main, island.clone(), 4.5),
        pair("action", &*main, island.clone(), 4.5),
        pair("link", &*main, island.clone(), 4.5),
        pair("ring", &*main, island.clone(), 3.0),
        pair("foreground", &*on_card, card.clone(), 4.5),
        pair("muted-foreground", &*on_card, card.clone(), 4.5),
        pair("action", &*on_card, card.clone(), 4.5),
        pair("ring", &*on_card, card, 3.0),
        pair("muted-foreground", on_well, well, 4.5),
        pair("dot-bad", &*main, island.clone(), MARK),
        pair("dot-held", main, island, MARK),
        pair("muted-foreground", &*on_nav, nav.clone(), 4.5),
        pair("muted-foreground", &*on_hover, hover.clone(), 4.5),
        pair("sidebar-foreground", &*on_nav, nav.clone(), 4.5),
        pair("sidebar-foreground", &*on_hover, hover.clone(), 4.5),
        pair("sidebar-accent-foreground", &*on_pill, pill.clone(), 4.5),
        pair("ring", on_nav, nav, 3.0),
        pair("ring", on_hover, hover, 3.0),
        pair("ring", on_pill, pill, 3.0),
    ]
}

/// The surfaces the page pairs are measured on.
struct Surfaces {
    canvas: Vec<Layer>,
    bg: Vec<Layer>,
    card: Vec<Layer>,
    popover: Vec<Layer>,
    sheet: Vec<Layer>,
    sheet_hover: Vec<Layer>,
    sheet_pill: Vec<Layer>,
    dock: Vec<Layer>,
    well: Vec<Layer>,
}

impl Surfaces {
    fn new() -> Self {
        let canvas = layers(&["canvas"]);
        let shell = layers(&["canvas", "shell"]);
        let bg = on(&shell, &[("background", 1.0)]);
        let card = on(&bg, &[("card", 1.0)]);
        let sheet = on(&canvas, &[("shell", 1.0)]);
        Self {
            popover: on(&bg, &[("popover", 1.0)]),
            sheet_hover: on(&sheet, &[("sidebar-accent", 0.5)]),
            sheet_pill: on(&sheet, &[("sidebar-accent", 1.0)]),
            dock: on(&bg, &[("dock-surface", 1.0)]),
            well: on(&card, &[("well", 1.0)]),
            canvas,
            bg,
            card,
            sheet,
        }
    }
}

fn with(base: &[Layer], token: &'static str) -> Vec<Layer> {
    on(base, &[(token, 1.0)])
}

fn tint(base: &[Layer], token: &'static str, alpha: f64) -> Vec<Layer> {
    on(base, &[(token, alpha)])
}

/// Primary text on the canvas, the page, cards, wells and menus.
fn text_pairs(s: &Surfaces) -> Vec<Pair> {
    vec![
        pair("foreground", "canvas", s.canvas.clone(), 4.5),
        pair("muted-foreground", "canvas", s.canvas.clone(), 4.5),
        pair("action", "canvas", s.canvas.clone(), 4.5),
        pair("link", "canvas", s.canvas.clone(), 4.5),
        pair("ring", "canvas", s.canvas.clone(), 3.0),
        pair("foreground", "background", s.bg.clone(), 4.5),
        pair("foreground", "card", s.card.clone(), 4.5),
        pair("foreground", "well", s.well.clone(), 4.5),
        pair("foreground", "well-hover", with(&s.card, "well-hover"), 4.5),
        pair(
            "foreground",
            "well-hover over background",
            with(&s.bg, "well-hover"),
            4.5,
        ),
        pair(
            "foreground",
            "well-hover over well",
            with(&s.well, "well-hover"),
            4.5,
        ),
        pair("foreground", "muted", with(&s.card, "muted"), 4.5),
        pair("foreground", "popover", s.popover.clone(), 4.5),
        pair("foreground", "secondary", with(&s.bg, "secondary"), 4.5),
        pair("foreground", "accent", with(&s.bg, "accent"), 4.5),
        pair(
            "foreground",
            "surface-subtle",
            with(&s.bg, "surface-subtle"),
            4.5,
        ),
        pair(
            "foreground",
            "surface-subtle over card",
            with(&s.card, "surface-subtle"),
            4.5,
        ),
        pair("card-foreground", "card", s.card.clone(), 4.5),
        pair("popover-foreground", "popover", s.popover.clone(), 4.5),
    ]
}

/// Secondary text on the page, cards, wells, menus and the phone sheet.
fn muted_pairs(s: &Surfaces) -> Vec<Pair> {
    vec![
        pair("muted-foreground", "background", s.bg.clone(), 4.5),
        pair("muted-foreground", "card", s.card.clone(), 4.5),
        pair("muted-foreground", "well", s.well.clone(), 4.5),
        pair(
            "muted-foreground",
            "well-hover",
            with(&s.card, "well-hover"),
            4.5,
        ),
        pair(
            "muted-foreground",
            "well-hover over background",
            with(&s.bg, "well-hover"),
            4.5,
        ),
        pair(
            "muted-foreground",
            "well-hover over well",
            with(&s.well, "well-hover"),
            4.5,
        ),
        pair("muted-foreground", "muted", with(&s.card, "muted"), 4.5),
        pair("muted-foreground", "popover", s.popover.clone(), 4.5),
        pair("muted-foreground", "accent", with(&s.bg, "accent"), 4.5),
        pair(
            "muted-foreground",
            "surface-subtle",
            with(&s.bg, "surface-subtle"),
            4.5,
        ),
        pair("muted-foreground", "phone sheet", s.sheet.clone(), 4.5),
        pair(
            "muted-foreground",
            "phone sheet hover wash",
            s.sheet_hover.clone(),
            4.5,
        ),
        pair(
            "secondary-foreground",
            "secondary",
            with(&s.bg, "secondary"),
            4.5,
        ),
        pair(
            "secondary-foreground",
            "surface-subtle",
            with(&s.bg, "surface-subtle"),
            4.5,
        ),
        pair("accent-foreground", "accent", with(&s.bg, "accent"), 4.5),
        pair("sidebar-foreground", "phone sheet", s.sheet.clone(), 4.5),
        pair(
            "sidebar-foreground",
            "phone sheet hover wash",
            s.sheet_hover.clone(),
            4.5,
        ),
        pair(
            "sidebar-accent-foreground",
            "phone sheet pill",
            s.sheet_pill.clone(),
            4.5,
        ),
    ]
}

/// Rings, status, the dock, glyphs, hairlines and marks.
fn control_pairs(s: &Surfaces) -> Vec<Pair> {
    vec![
        pair("ring", "phone sheet", s.sheet.clone(), 3.0),
        pair("ring", "phone sheet hover wash", s.sheet_hover.clone(), 3.0),
        pair("ring", "phone sheet pill", s.sheet_pill.clone(), 3.0),
        pair("primary-foreground", "primary", with(&s.bg, "primary"), 4.5),
        pair(
            "primary-foreground",
            "action-hover",
            with(&s.bg, "action-hover"),
            4.5,
        ),
        pair("action", "background", s.bg.clone(), 4.5),
        pair("action", "card", s.card.clone(), 4.5),
        pair("action", "popover", s.popover.clone(), 4.5),
        pair("link", "background", s.bg.clone(), 4.5),
        pair("link", "card", s.card.clone(), 4.5),
        pair("link", "popover", s.popover.clone(), 4.5),
        pair(
            "background",
            "success status icon",
            with(&s.bg, "success"),
            3.0,
        ),
        pair(
            "background",
            "destructive status icon",
            with(&s.bg, "destructive"),
            3.0,
        ),
        pair(
            "destructive-foreground",
            "destructive",
            with(&s.bg, "destructive"),
            4.5,
        ),
        pair(
            "destructive",
            "destructive/10 over popover",
            tint(&s.popover, "destructive", 0.1),
            4.5,
        ),
        pair("destructive-text", "background", s.bg.clone(), 4.5),
        pair("destructive-text", "card", s.card.clone(), 4.5),
        pair("destructive-text", "popover", s.popover.clone(), 4.5),
        pair(
            "destructive-text",
            "destructive/10 over background",
            tint(&s.bg, "destructive", 0.1),
            4.5,
        ),
        pair(
            "destructive-text",
            "destructive/10 over card",
            tint(&s.card, "destructive", 0.1),
            4.5,
        ),
        pair(
            "destructive-text",
            "destructive/10 over popover",
            tint(&s.popover, "destructive", 0.1),
            4.5,
        ),
        pair("dock-foreground", "dock-surface", s.dock.clone(), 4.5),
        pair(
            "dock-foreground",
            "dock-well",
            with(&s.dock, "dock-well"),
            4.5,
        ),
        pair(
            "dock-foreground",
            "dock-active",
            with(&s.dock, "dock-active"),
            4.5,
        ),
        pair("dock-muted", "dock-surface", s.dock.clone(), 4.5),
        pair("dock-muted", "dock-well", with(&s.dock, "dock-well"), 4.5),
        pair("ring", "background", s.bg.clone(), 3.0),
        pair("dock-ring", "dock-surface", s.dock.clone(), 3.0),
        pair("subtle-foreground", "canvas", s.canvas.clone(), 3.0),
        pair("subtle-foreground", "card", s.card.clone(), 3.0),
        pair("subtle-foreground", "well", s.well.clone(), 3.0),
        pair("line", "card", s.card.clone(), HAIRLINE),
        pair("line", "well", s.well.clone(), HAIRLINE),
        pair(
            "foreground",
            "action-soft over card",
            with(&s.card, "action-soft"),
            4.5,
        ),
        pair(
            "foreground",
            "action-soft over well",
            with(&s.well, "action-soft"),
            4.5,
        ),
    ]
}

/// Semantic status text rendered directly on page/card/popover grounds (e.g. `text-warning`,
/// `text-success`, `text-destructive`).
fn semantic_text_pairs(s: &Surfaces) -> Vec<Pair> {
    ["warning", "success", "destructive"]
        .into_iter()
        .flat_map(|token| {
            [
                pair(token, "background", s.bg.clone(), 4.5),
                pair(token, "card", s.card.clone(), 4.5),
                pair(token, "popover", s.popover.clone(), 4.5),
            ]
        })
        .collect()
}

/// Every pair Studio gates, in Studio's order.
pub(super) fn pairs() -> Vec<Pair> {
    let s = Surfaces::new();
    let mut all = text_pairs(&s);
    all.extend(muted_pairs(&s));
    all.extend(control_pairs(&s));
    all.extend(semantic_text_pairs(&s));
    for dot in [
        "dot-live",
        "dot-held",
        "dot-bad",
        "dot-quiet",
        "dot-accent",
        "action-dot",
    ] {
        all.push(pair(dot, "card", s.card.clone(), MARK));
        all.push(pair(dot, "well", s.well.clone(), MARK));
    }
    for (fg, bg_token) in [
        ("avatar-1-fg", "avatar-1-bg"),
        ("avatar-2-fg", "avatar-2-bg"),
        ("avatar-3-fg", "avatar-3-bg"),
        ("avatar-4-fg", "avatar-4-bg"),
        ("avatar-5-fg", "avatar-5-bg"),
        ("avatar-6-fg", "avatar-6-bg"),
    ] {
        all.push(pair(fg, bg_token, with(&s.card, bg_token), 4.5));
    }
    let backdrops: [(&str, Vec<Layer>); 4] = [
        ("backdrop", layers(&["backdrop"])),
        (
            "backdrop under glow 1",
            layers(&["backdrop", "backdrop-glow-1"]),
        ),
        (
            "backdrop under glow 2",
            layers(&["backdrop", "backdrop-glow-2"]),
        ),
        (
            "backdrop under both glows",
            layers(&["backdrop", "backdrop-glow-1", "backdrop-glow-2"]),
        ),
    ];
    for (place, backdrop) in &backdrops {
        all.extend(island_pairs(place, backdrop));
    }
    all
}
