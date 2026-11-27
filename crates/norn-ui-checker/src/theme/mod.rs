//! Theme admission: a `theme` artifact is admitted only if Studio would accept and render it.
//!
//! The rules and their answers are Studio's own (`apps/studio/src/lib/studio-theme.ts`):
//! `parseStudioTheme` decides the shape, the token allowlist, the colours and the base skin;
//! `validateStudioThemeContrast` decides WCAG contrast for every ink/surface pair Studio paints, in
//! each scheme the theme applies to; and the monochrome rule (DESIGN.md 19.6,
//! `apps/studio/test/monochrome.test.ts`) keeps chrome neutral. The allowlisted tokens, the skins
//! and the palettes they recolour are read from `theme-palettes.json`, which Studio generates
//! from the CSS it ships, so the server and Studio measure the same colours.
//!
//! Each rule has its own code; the golden fixtures in `fixtures/themes/` pin the answers that
//! Studio's validators must also give.

mod color;
mod convert;
mod named;
mod pairs;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use norn_ir::{Diagnostic, DiagnosticRemediation, DiagnosticSeverity, JsonValue};

use color::{
    Color, canonical_wire_color, contrast_on, css_color, fade, oklch_chroma, over, parse_color,
};

/// The payload is not a `StudioTheme` (version, name, appearance, or the shape of a token map).
pub const THEME_SCHEMA: &str = "theme.validation.schema";
/// The base skin is not one of Studio's built-in skins.
pub const THEME_UNKNOWN_SKIN: &str = "theme.validation.unknown_skin";
/// A token map names a token outside Studio's allowlist.
pub const THEME_UNKNOWN_TOKEN: &str = "theme.validation.unknown_token";
/// A token, `accent` or `canvas` value is not a CSS colour.
pub const THEME_INVALID_COLOR: &str = "theme.validation.invalid_color";
/// An ink/surface pair Studio paints falls below its WCAG floor.
pub const THEME_CONTRAST: &str = "theme.validation.contrast";
/// A chrome token carries a hue (OKLCH chroma 0.02 or more).
pub const THEME_MONOCHROME: &str = "theme.validation.monochrome";
/// More than one theme in one workspace.
pub const THEME_MULTIPLE: &str = "theme.validation.multiple";

/// OKLCH chroma at or above which a chrome token counts as coloured (DESIGN.md 19.6).
const MAX_CHROME_CHROMA: f64 = 0.02;

/// The palette Studio paints for each built-in skin: skin, then `[light, dark]`, then token.
type Palettes = BTreeMap<String, [BTreeMap<String, String>; 2]>;

/// Studio's generated skin palettes (`scripts/theme-palettes.ts` writes them from `index.css`).
const PALETTES_JSON: &str = include_str!("../../../../apps/studio/src/lib/theme-palettes.json");

static PALETTES: LazyLock<Option<Palettes>> = LazyLock::new(|| {
    let serde_json::Value::Object(skins) =
        serde_json::from_str::<serde_json::Value>(PALETTES_JSON).ok()?
    else {
        return None;
    };
    let scheme = |value: Option<&serde_json::Value>| -> Option<BTreeMap<String, String>> {
        let serde_json::Value::Object(tokens) = value? else {
            return None;
        };
        tokens
            .iter()
            .map(|(token, colour)| Some((token.clone(), colour.as_str()?.to_owned())))
            .collect()
    };
    skins
        .iter()
        .map(|(skin, schemes)| {
            Some((
                skin.clone(),
                [scheme(schemes.get("light"))?, scheme(schemes.get("dark"))?],
            ))
        })
        .collect()
});

/// The semantic tokens a theme may set: the tokens of Studio's painted palettes.
fn allowed_tokens(palettes: &Palettes) -> BTreeSet<&str> {
    palettes
        .values()
        .flat_map(|schemes| schemes.iter().flat_map(|tokens| tokens.keys()))
        .map(String::as_str)
        .collect()
}

/// A light/dark colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Light,
    Dark,
}

impl Scheme {
    const fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Light => 0,
            Self::Dark => 1,
        }
    }
}

/// A token's resolved value and where in the payload the theme set it.
struct Resolved {
    value: String,
    path: String,
}

/// A theme that passed the shape rules, its colours in canonical CSS.
struct CanonicalTheme {
    skin: String,
    appearance: Option<Scheme>,
    accent: Option<String>,
    canvas: Option<String>,
    colors: BTreeMap<String, String>,
    variants: [BTreeMap<String, String>; 2],
}

impl CanonicalTheme {
    fn schemes(&self) -> Vec<Scheme> {
        self.appearance
            .map_or_else(|| vec![Scheme::Light, Scheme::Dark], |scheme| vec![scheme])
    }
}

/// A JSON string's text.
fn text(value: &JsonValue) -> Option<&str> {
    match value {
        JsonValue::String(text) => Some(text.as_str()),
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::Array(_)
        | JsonValue::Object(_) => None,
    }
}

fn finding(code: &str, message: String, path: &str, artifact_id: Option<&str>) -> Diagnostic {
    Diagnostic {
        code: code.to_owned(),
        severity: DiagnosticSeverity::Error,
        message,
        artifact_id: artifact_id.map(str::to_owned),
        path: Some(path.to_owned()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// Collects findings for one theme payload.
struct Findings<'a> {
    artifact_id: Option<&'a str>,
    found: Vec<Diagnostic>,
}

impl Findings<'_> {
    fn push(&mut self, code: &str, message: String, path: &str) {
        self.found.push(finding(
            code,
            message,
            &format!("payload.{path}"),
            self.artifact_id,
        ));
    }
}

/// Studio's `validateTokenMap`: a map of allowlisted tokens (an optional `--` prefix dropped) to
/// CSS colours, returned in canonical CSS.
fn token_map(
    value: Option<&JsonValue>,
    path: &str,
    allowed: &BTreeSet<&str>,
    findings: &mut Findings<'_>,
) -> BTreeMap<String, String> {
    let mut clean = BTreeMap::new();
    let Some(value) = value else {
        return clean;
    };
    let JsonValue::Object(entries) = value else {
        findings.push(
            THEME_SCHEMA,
            "Expected an object mapping semantic token names to CSS colors.".to_owned(),
            path,
        );
        return clean;
    };
    for (raw_key, raw_value) in entries {
        let key = raw_key.strip_prefix("--").unwrap_or(raw_key);
        if !allowed.contains(key) {
            findings.push(
                THEME_UNKNOWN_TOKEN,
                format!("Unknown or disallowed theme token \"{raw_key}\"."),
                &format!("{path}.{raw_key}"),
            );
            continue;
        }
        match text(raw_value).and_then(canonical_wire_color) {
            Some(colour) => {
                clean.insert(key.to_owned(), colour);
            }
            None => findings.push(
                THEME_INVALID_COLOR,
                format!(
                    "Invalid color value for \"{key}\". Use a canonical CSS color such as #rrggbb or rgba(r, g, b, a)."
                ),
                &format!("{path}.{key}"),
            ),
        }
    }
    clean
}

/// A shorthand colour (`accent`, `canvas`) in canonical CSS.
fn shorthand(
    payload: &BTreeMap<String, JsonValue>,
    field: &str,
    findings: &mut Findings<'_>,
) -> Option<String> {
    let value = payload.get(field)?;
    let colour = text(value).and_then(canonical_wire_color);
    if colour.is_none() {
        findings.push(
            THEME_INVALID_COLOR,
            format!("{field} must be a canonical CSS color such as #rrggbb or rgba(r, g, b, a)."),
            field,
        );
    }
    colour
}

/// The base skin: one of Studio's built-in skins, `luma` when absent.
fn base_skin(
    value: Option<&JsonValue>,
    palettes: &Palettes,
    findings: &mut Findings<'_>,
) -> String {
    match value {
        None => "luma".to_owned(),
        Some(value) => {
            if let Some(skin) = text(value).filter(|skin| palettes.contains_key(*skin)) {
                skin.to_owned()
            } else {
                let known: Vec<String> =
                    palettes.keys().map(|skin| format!("\"{skin}\"")).collect();
                findings.push(
                    THEME_UNKNOWN_SKIN,
                    format!("baseSkin must be one of {}.", known.join(", ")),
                    "baseSkin",
                );
                String::new()
            }
        }
    }
}

/// The scheme a theme is limited to, if any.
fn appearance(value: Option<&JsonValue>, findings: &mut Findings<'_>) -> Option<Scheme> {
    match value {
        None => None,
        Some(value) => match text(value) {
            Some("light") => Some(Scheme::Light),
            Some("dark") => Some(Scheme::Dark),
            _ => {
                findings.push(
                    THEME_SCHEMA,
                    "appearance must be \"light\" or \"dark\".".to_owned(),
                    "appearance",
                );
                None
            }
        },
    }
}

/// The per-scheme token maps (`[light, dark]`); members other than `light` and `dark` are refused.
fn variants(
    value: Option<&JsonValue>,
    allowed: &BTreeSet<&str>,
    findings: &mut Findings<'_>,
) -> [BTreeMap<String, String>; 2] {
    let mut variants = [BTreeMap::new(), BTreeMap::new()];
    match value {
        None => {}
        Some(JsonValue::Object(members)) => {
            for member in members
                .keys()
                .filter(|key| !matches!(key.as_str(), "light" | "dark"))
            {
                findings.push(
                    THEME_SCHEMA,
                    format!("variants may only hold \"light\" and \"dark\", not \"{member}\"."),
                    &format!("variants.{member}"),
                );
            }
            for scheme in [Scheme::Light, Scheme::Dark] {
                if let Some(slot) = variants.get_mut(scheme.index()) {
                    *slot = token_map(
                        members.get(scheme.name()),
                        &format!("variants.{}", scheme.name()),
                        allowed,
                        findings,
                    );
                }
            }
        }
        Some(_) => findings.push(
            THEME_SCHEMA,
            "variants must be an object with optional light and dark maps.".to_owned(),
            "variants",
        ),
    }
    variants
}

/// Studio's `parseStudioTheme` shape rules, with the server's closed shape: unknown top-level
/// members are refused by the payload root (`theme.validation.schema`), and so are unknown
/// `variants` members here.
fn canonical_theme(
    payload: &JsonValue,
    palettes: &Palettes,
    findings: &mut Findings<'_>,
) -> Option<CanonicalTheme> {
    let JsonValue::Object(payload) = payload else {
        findings.found.push(finding(
            THEME_SCHEMA,
            "Theme must be a JSON object.".to_owned(),
            "payload",
            findings.artifact_id,
        ));
        return None;
    };
    let before = findings.found.len();
    if !matches!(payload.get("version"), Some(JsonValue::Number(version)) if (*version - 1.0).abs() < f64::EPSILON)
    {
        findings.push(
            THEME_SCHEMA,
            "version is required and only theme version 1 is supported.".to_owned(),
            "version",
        );
    }
    let name = payload
        .get("name")
        .and_then(text)
        .map_or_default(color::js_trim);
    if name.is_empty() || name.encode_utf16().count() > 64 {
        findings.push(
            THEME_SCHEMA,
            "Theme name must be a non-empty string (max 64 characters).".to_owned(),
            "name",
        );
    }
    let skin = base_skin(payload.get("baseSkin"), palettes, findings);
    let appearance = appearance(payload.get("appearance"), findings);
    let accent = shorthand(payload, "accent", findings);
    let canvas = shorthand(payload, "canvas", findings);
    let allowed = allowed_tokens(palettes);
    let colors = token_map(payload.get("colors"), "colors", &allowed, findings);
    let variants = variants(payload.get("variants"), &allowed, findings);
    (findings.found.len() == before).then_some(CanonicalTheme {
        skin,
        appearance,
        accent,
        canvas,
        colors,
        variants,
    })
}

fn known(value: &str) -> Color {
    parse_color(value).unwrap_or(Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        alpha: 1.0,
    })
}

/// Studio's `resolveCustomThemeTokens`: the overrides a theme writes for one scheme, each with
/// the payload member that set it.
fn resolve_tokens(theme: &CanonicalTheme, scheme: Scheme) -> BTreeMap<String, Resolved> {
    let mut overrides: BTreeMap<String, Resolved> = BTreeMap::new();
    let mut set = |token: &str, value: &str, path: &str| {
        overrides.insert(
            token.to_owned(),
            Resolved {
                value: value.to_owned(),
                path: path.to_owned(),
            },
        );
    };
    if let Some(canvas) = &theme.canvas {
        set("canvas", canvas, "canvas");
        set("shell", canvas, "canvas");
    }
    if let Some(accent) = &theme.accent {
        let ink = parse_color(accent).map_or("#ffffff", |colour| {
            if contrast_on(known("#fafafa"), colour) >= contrast_on(known("#141416"), colour) {
                "#fafafa"
            } else {
                "#141416"
            }
        });
        for token in ["primary", "action", "action-hover", "ring", "link"] {
            set(token, accent, "accent");
        }
        set("primary-foreground", ink, "accent");
        set("action-foreground", ink, "accent");
    }
    for (token, value) in &theme.colors {
        set(token, value, &format!("colors.{token}"));
    }
    if let Some(variant) = theme.variants.get(scheme.index()) {
        for (token, value) in variant {
            set(token, value, &format!("variants.{}.{token}", scheme.name()));
        }
    }
    // A theme that paints its own ground but not the islands gets opaque islands in that ground,
    // on a backdrop one step darker (DESIGN.md 4.7).
    let ground = overrides
        .get("canvas")
        .map(|canvas| (canvas.value.clone(), canvas.path.clone()));
    if let Some((canvas, path)) = ground
        && let Some(ground) = parse_color(&canvas)
    {
        let derived = |value: String| Resolved {
            value,
            path: path.clone(),
        };
        let shell = overrides
            .get("shell")
            .map_or_else(|| canvas.clone(), |shell| shell.value.clone());
        let depth = if scheme == Scheme::Dark { 0.4 } else { 0.05 };
        let backdrop = css_color(over(
            fade(known("#000000"), depth),
            Color {
                alpha: 1.0,
                ..ground
            },
        ));
        overrides
            .entry("island".to_owned())
            .or_insert_with(|| derived(canvas.clone()));
        overrides
            .entry("sidebar".to_owned())
            .or_insert_with(|| derived(shell));
        overrides
            .entry("backdrop".to_owned())
            .or_insert_with(|| derived(backdrop));
    }
    overrides
}

/// Studio's `validateStudioThemeContrast` for one scheme.
fn check_contrast(
    theme: &CanonicalTheme,
    scheme: Scheme,
    palettes: &Palettes,
    findings: &mut Findings<'_>,
) {
    let overrides = resolve_tokens(theme, scheme);
    let base = palettes
        .get(&theme.skin)
        .and_then(|schemes| schemes.get(scheme.index()));
    let colour = |token: &str| {
        overrides
            .get(token)
            .map(|resolved| resolved.value.as_str())
            .or_else(|| {
                base.and_then(|tokens| tokens.get(token))
                    .map(String::as_str)
            })
            .and_then(parse_color)
    };
    let page = known(if scheme == Scheme::Dark {
        "#000000"
    } else {
        "#ffffff"
    });
    for pair in pairs::pairs() {
        let surface = pair.surface.iter().try_fold(page, |below, (token, alpha)| {
            colour(token).map(|layer| over(fade(layer, *alpha), below))
        });
        let ratio = colour(pair.ink)
            .zip(surface)
            .map_or(0.0, |(ink, surface)| contrast_on(ink, surface));
        if ratio < pair.floor {
            let path = overrides
                .get(pair.ink)
                .map_or_else(String::new, |resolved| resolved.path.clone());
            let scheme = scheme.name();
            findings.found.push(finding(
                THEME_CONTRAST,
                format!(
                    "{scheme} {} (on {}) contrast is {ratio:.2}:1 (WCAG AA requires >= {:.1}:1).",
                    pair.ink, pair.label, pair.floor
                ),
                &if path.is_empty() {
                    "payload".to_owned()
                } else {
                    format!("payload.{path}")
                },
                findings.artifact_id,
            ));
        }
    }
}

/// Colour is only for chart and canvas visualisation, status beside a word, avatars, the
/// mascot, and data highlights (`monochrome.test.ts`'s `DATA` tokens); every other token is
/// chrome.
fn is_data_token(token: &str) -> bool {
    fn lower(rest: &str) -> bool {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase())
    }
    fn digit(rest: &str) -> bool {
        rest.len() == 1 && rest.bytes().all(|b| b.is_ascii_digit())
    }
    fn avatar(rest: &str) -> bool {
        rest.split_once('-')
            .is_some_and(|(index, side)| digit(index) && matches!(side, "bg" | "fg"))
    }
    let suffix_is =
        |prefix: &str, test: fn(&str) -> bool| token.strip_prefix(prefix).is_some_and(test);
    matches!(
        token,
        "dot-live"
            | "dot-held"
            | "dot-bad"
            | "success"
            | "warning"
            | "destructive"
            | "destructive-foreground"
            | "destructive-text"
            | "mascot-body"
            | "edge"
            | "edge-active"
    ) || suffix_is("chart-", digit)
        || suffix_is("avatar-", avatar)
        || suffix_is("tone-", lower)
        || suffix_is("tint-", lower)
        || suffix_is("node-", lower)
        || suffix_is("canvas-", lower)
}

/// DESIGN.md 19.6: every chrome token the theme writes is neutral (OKLCH chroma below 0.02),
/// measured on the canonical sRGB value Studio applies.
fn check_monochrome(theme: &CanonicalTheme, scheme: Scheme, findings: &mut Findings<'_>) {
    for (token, resolved) in resolve_tokens(theme, scheme) {
        if is_data_token(&token) {
            continue;
        }
        let chroma = parse_color(&resolved.value).map_or(0.0, oklch_chroma);
        if chroma >= MAX_CHROME_CHROMA {
            findings.found.push(finding(
                THEME_MONOCHROME,
                format!(
                    "{} {token} is {} (OKLCH chroma {chroma:.3}); chrome is black, white and greys (chroma below 0.02). Colour belongs only to chart, status, avatar and data-highlight tokens.",
                    scheme.name(),
                    resolved.value
                ),
                &format!("payload.{}", resolved.path),
                findings.artifact_id,
            ));
        }
    }
}

/// Admit one `theme` payload (the bare payload object, `extensions` removed): Studio's shape,
/// token, colour and skin rules, then contrast and monochrome in each scheme it applies to.
#[must_use]
pub fn validate_theme_payload(payload: &JsonValue, artifact_id: Option<&str>) -> Vec<Diagnostic> {
    let mut findings = Findings {
        artifact_id,
        found: Vec::new(),
    };
    let Some(palettes) = PALETTES.as_ref() else {
        findings.push(
            THEME_SCHEMA,
            "Studio's skin palettes could not be read, so no theme can be checked.".to_owned(),
            "baseSkin",
        );
        return findings.found;
    };
    let Some(theme) = canonical_theme(payload, palettes, &mut findings) else {
        return findings.found;
    };
    for scheme in theme.schemes() {
        check_contrast(&theme, scheme, palettes, &mut findings);
    }
    for scheme in theme.schemes() {
        check_monochrome(&theme, scheme, &mut findings);
    }
    findings.found
}

/// A workspace has at most one active theme: a second active theme under another id is refused, so
/// the theme Studio applies is never a matter of order. A theme is replaced by applying it under
/// the same id, or cleared by deprecating it (`lifecycle: "deprecated"`).
#[must_use]
pub fn check_theme_set<'a>(
    artifacts: impl IntoIterator<Item = &'a norn_ir::ArtifactEnvelope>,
) -> Vec<Diagnostic> {
    let ids: BTreeSet<&str> = artifacts
        .into_iter()
        .filter(|artifact| {
            matches!(artifact.payload, norn_ir::ArtifactPayload::Theme(_))
                && !matches!(
                    artifact.lifecycle,
                    norn_ir::ArtifactLifecycle::Deprecated
                        | norn_ir::ArtifactLifecycle::Superseded
                        | norn_ir::ArtifactLifecycle::Rejected
                )
        })
        .map(|artifact| artifact.artifact_id.as_str())
        .collect();
    let mut ids = ids.into_iter();
    let Some(first) = ids.next() else {
        return Vec::new();
    };
    ids.map(|other| {
        finding(
            THEME_MULTIPLE,
            format!(
                "a workspace has one theme, but '{first}' and '{other}' are both themes; apply the new look under '{first}' to replace it"
            ),
            "payload",
            Some(other),
        )
    })
    .collect()
}
