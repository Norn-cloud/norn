//! CSS colours exactly as Studio reads them.
//!
//! Studio parses, canonicalizes, composites and measures theme colours with culori 4.0.2
//! (`apps/studio/src/lib/color.ts`: CSS Color 4 hex, named, `rgb()`, `hsl()`, `hwb()`, `lab()`,
//! `lch()`, `oklab()`, `oklch()` and `color(srgb …)`, clipped to sRGB). The server must give the
//! same answer, so this module reproduces that reading step by step: culori's tokenizer and
//! modern-syntax parsers, its legacy comma forms, its conversions to sRGB, `clampRgb`, the
//! canonical `#rrggbb` / `rgba(r, g, b, a)` form, source-over compositing rounded to 8 bits per
//! channel, WCAG 2.x contrast and OKLCH chroma. `tests/theme.rs` pins the cases that differ
//! between CSS parsers (bare hex, `none`, hue units, unclosed functions, out-of-gamut values).

use std::f64::consts::PI;
use std::sync::LazyLock;

use super::convert::{
    gamma_to_linear, hsl_to_rgb, hwb_to_rgb, lab_to_rgb, linear_to_gamma, oklab_to_rgb,
    polar_to_axes,
};

/// An sRGB colour, channels 0–1 (NaN only where culori's own arithmetic yields it), alpha 0–1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Color {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
    pub(crate) alpha: f64,
}

/// JavaScript's `Math.min`: NaN in, NaN out.
fn js_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.min(right)
    }
}

/// JavaScript's `Math.max`: NaN in, NaN out.
fn js_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else {
        left.max(right)
    }
}

fn clamp01(value: f64) -> f64 {
    js_min(1.0, js_max(0.0, value))
}

/// JavaScript's `String.prototype.trim` (Unicode `WhiteSpace` and `LineTerminator` code points).
pub(crate) fn js_trim(value: &str) -> &str {
    let js_space = |c: char| {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    };
    value.trim_matches(js_space)
}

/// The colour spaces Studio registers with culori.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Rgb,
    Lrgb,
    Hsl,
    Hwb,
    Lab,
    Lch,
    Oklab,
    Oklch,
}

/// A parsed colour in its own space; `None` is a missing (`none`) channel.
#[derive(Debug, Clone, Copy)]
struct Parsed {
    mode: Mode,
    channels: [Option<f64>; 3],
    alpha: Option<f64>,
}

impl Parsed {
    const fn rgb(r: f64, g: f64, b: f64, alpha: Option<f64>) -> Self {
        Self {
            mode: Mode::Rgb,
            channels: [Some(r), Some(g), Some(b)],
            alpha,
        }
    }
}

/// culori threw while reading the input; Studio's `parseColor` reads that as "not a colour".
struct Thrown;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Function(String),
    Ident(String),
    Number(f64),
    Percentage(f64),
    ParenClose,
    None,
    Hue(f64),
    Alpha(Box<Tok>),
    /// A number with a unit culori does not know: its tokenizer yields `undefined` here.
    Invalid,
}

fn is_digit(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_digit())
}

fn is_ident_start(c: Option<char>) -> bool {
    c.is_some_and(|c| !c.is_ascii() || c.is_ascii_alphabetic() || c == '_')
}

fn is_ident_char(c: Option<char>) -> bool {
    c.is_some_and(|c| !c.is_ascii() || c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn js_number(text: &str) -> f64 {
    text.parse::<f64>().unwrap_or(f64::NAN)
}

struct Cursor {
    chars: Vec<char>,
    at: usize,
}

impl Cursor {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at.saturating_add(ahead)).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let current = self.peek(0);
        self.at = self.at.saturating_add(1);
        current
    }

    fn back(&mut self) {
        self.at = self.at.saturating_sub(1);
    }

    fn done(&self) -> bool {
        self.at >= self.chars.len()
    }

    fn is_num(&self) -> bool {
        match self.peek(0) {
            Some('-' | '+') => {
                is_digit(self.peek(1)) || (self.peek(1) == Some('.') && is_digit(self.peek(2)))
            }
            Some('.') => is_digit(self.peek(1)),
            other => is_digit(other),
        }
    }

    fn is_ident(&self) -> bool {
        if self.done() {
            return false;
        }
        let current = self.peek(0);
        if is_ident_start(current) {
            return true;
        }
        if current == Some('-') {
            if self.chars.len().saturating_sub(self.at) < 2 {
                return false;
            }
            let next = self.peek(1);
            return next == Some('-') || is_ident_start(next);
        }
        false
    }

    fn digits(&mut self, out: &mut String) {
        while is_digit(self.peek(0)) {
            out.extend(self.bump());
        }
    }

    fn ident(&mut self) -> String {
        let mut out = String::new();
        while !self.done() && is_ident_char(self.peek(0)) {
            out.extend(self.bump());
        }
        out
    }

    fn num(&mut self) -> Tok {
        let mut text = String::new();
        if matches!(self.peek(0), Some('-' | '+')) {
            text.extend(self.bump());
        }
        self.digits(&mut text);
        if self.peek(0) == Some('.') && is_digit(self.peek(1)) {
            text.extend(self.bump());
            self.digits(&mut text);
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            if matches!(self.peek(1), Some('-' | '+')) && is_digit(self.peek(2)) {
                text.extend(self.bump());
                text.extend(self.bump());
                self.digits(&mut text);
            } else if is_digit(self.peek(1)) {
                text.extend(self.bump());
                self.digits(&mut text);
            }
        }
        let value = js_number(&text);
        if self.is_ident() {
            return match self.ident().as_str() {
                "deg" => Tok::Hue(value),
                "rad" => Tok::Hue(value * (180.0 / PI)),
                "grad" => Tok::Hue(value * (9.0 / 10.0)),
                "turn" => Tok::Hue(value * 360.0),
                _ => Tok::Invalid,
            };
        }
        if self.peek(0) == Some('%') {
            self.bump();
            return Tok::Percentage(value);
        }
        Tok::Number(value)
    }

    fn identlike(&mut self) -> Tok {
        let name = self.ident();
        if self.peek(0) == Some('(') {
            self.bump();
            return Tok::Function(name);
        }
        if name == "none" {
            return Tok::None;
        }
        Tok::Ident(name)
    }

    fn skip_space(&mut self) {
        while !self.done() && matches!(self.peek(0), Some('\n' | '\t' | ' ')) {
            self.bump();
        }
    }
}

/// culori's `tokenize`: `Ok(None)` where it returns `undefined`.
fn tokenize(value: &str) -> Result<Option<Vec<Tok>>, Thrown> {
    let mut cursor = Cursor {
        chars: js_trim(value).chars().collect(),
        at: 0,
    };
    let mut tokens = Vec::new();
    while !cursor.done() {
        let Some(current) = cursor.bump() else {
            break;
        };
        match current {
            '\n' | '\t' | ' ' => cursor.skip_space(),
            ',' => return Ok(None),
            ')' => tokens.push(Tok::ParenClose),
            '+' | '.' => {
                cursor.back();
                if !cursor.is_num() {
                    return Ok(None);
                }
                tokens.push(cursor.num());
            }
            '-' => {
                cursor.back();
                if cursor.is_num() {
                    tokens.push(cursor.num());
                } else if cursor.is_ident() {
                    tokens.push(Tok::Ident(cursor.ident()));
                } else {
                    return Ok(None);
                }
            }
            '/' => {
                cursor.skip_space();
                if cursor.is_num() {
                    let alpha = cursor.num();
                    // `alpha.type` of culori's `undefined` token throws.
                    if alpha == Tok::Invalid {
                        return Err(Thrown);
                    }
                    // A hue is no alpha: culori falls through to the `none` check.
                    if !matches!(alpha, Tok::Hue(_)) {
                        tokens.push(Tok::Alpha(Box::new(alpha)));
                        continue;
                    }
                }
                if cursor.is_ident() && cursor.ident() == "none" {
                    tokens.push(Tok::Alpha(Box::new(Tok::None)));
                    continue;
                }
                return Ok(None);
            }
            c if c.is_ascii_digit() => {
                cursor.back();
                tokens.push(cursor.num());
            }
            c if is_ident_start(Some(c)) => {
                cursor.back();
                tokens.push(cursor.identlike());
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(tokens))
}

/// culori's `consumeCoords`: three channels and an alpha (`Tok::None` when absent).
fn consume_coords(
    tokens: &[Tok],
    start: usize,
    include_hue: bool,
) -> Result<Option<[Tok; 4]>, Thrown> {
    let mut coords: Vec<Tok> = Vec::new();
    let mut index = start;
    while let Some(token) = tokens.get(index) {
        index = index.saturating_add(1);
        match token {
            Tok::Invalid => return Err(Thrown),
            Tok::None | Tok::Number(_) | Tok::Alpha(_) | Tok::Percentage(_) => {
                coords.push(token.clone());
            }
            Tok::Hue(_) if include_hue => coords.push(token.clone()),
            Tok::ParenClose => {
                if index < tokens.len() {
                    return Ok(None);
                }
            }
            Tok::Hue(_) | Tok::Function(_) | Tok::Ident(_) => return Ok(None),
        }
    }
    match coords.len() {
        3 => coords.push(Tok::None),
        4 => match coords.pop() {
            Some(Tok::Alpha(alpha)) => coords.push(*alpha),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    }
    if coords.iter().any(|coord| matches!(coord, Tok::Alpha(_))) {
        return Ok(None);
    }
    Ok(<[Tok; 4]>::try_from(coords).ok())
}

/// culori's `parseModernSyntax`: the function name and its four coordinates.
fn parse_modern(tokens: &[Tok]) -> Result<Option<(String, [Tok; 4])>, Thrown> {
    let Some(Tok::Function(name)) = tokens.first() else {
        return Ok(None);
    };
    Ok(consume_coords(tokens, 1, true)?.map(|coords| (name.clone(), coords)))
}

/// A coordinate's value; `None` for a `none` coordinate.
const fn coord(token: &Tok) -> Option<f64> {
    match token {
        Tok::Number(value) | Tok::Percentage(value) | Tok::Hue(value) => Some(*value),
        Tok::None
        | Tok::Function(_)
        | Tok::Ident(_)
        | Tok::ParenClose
        | Tok::Alpha(_)
        | Tok::Invalid => None,
    }
}

const fn is_hue(token: &Tok) -> bool {
    matches!(token, Tok::Hue(_))
}

const fn is_percentage(token: &Tok) -> bool {
    matches!(token, Tok::Percentage(_))
}

/// The alpha coordinate, clamped to 0–1 (a number as is, a percentage over 100).
fn modern_alpha(token: &Tok) -> Option<f64> {
    coord(token).map(|value| {
        let alpha = if matches!(token, Tok::Number(_)) {
            value
        } else {
            value / 100.0
        };
        js_min(1.0, js_max(0.0, alpha))
    })
}

fn parse_modern_rgb(name: &str, [r, g, b, alpha]: &[Tok; 4]) -> Option<Parsed> {
    if !matches!(name, "rgb" | "rgba") || [r, g, b].into_iter().any(is_hue) {
        return None;
    }
    let channel = |token: &Tok| {
        coord(token).map(|value| {
            if matches!(token, Tok::Number(_)) {
                value / 255.0
            } else {
                value / 100.0
            }
        })
    };
    Some(Parsed {
        mode: Mode::Rgb,
        channels: [channel(r), channel(g), channel(b)],
        alpha: modern_alpha(alpha),
    })
}

/// `hsl()` and `hwb()`: a hue, then two percentages (numbers read as percentages too).
fn parse_modern_hue_first(mode: Mode, [h, x, y, alpha]: &[Tok; 4]) -> Option<Parsed> {
    if is_percentage(h) || is_hue(x) || is_hue(y) {
        return None;
    }
    Some(Parsed {
        mode,
        channels: [
            coord(h),
            coord(x).map(|value| value / 100.0),
            coord(y).map(|value| value / 100.0),
        ],
        alpha: modern_alpha(alpha),
    })
}

fn parse_modern_lab(mode: Mode, [l, a, b, alpha]: &[Tok; 4]) -> Option<Parsed> {
    if [l, a, b].into_iter().any(is_hue) {
        return None;
    }
    let number = |token: &Tok| matches!(token, Tok::Number(_));
    // A percentage of the reference range: 125 for lab's axes, 0.4 for oklab's.
    let (lightness, reference) = if mode == Mode::Lab {
        (
            coord(l).map(|value| js_min(js_max(0.0, value), 100.0)),
            125.0,
        )
    } else {
        (
            coord(l).map(|value| {
                js_min(
                    js_max(0.0, if number(l) { value } else { value / 100.0 }),
                    1.0,
                )
            }),
            0.4,
        )
    };
    let axis = |token: &Tok| {
        coord(token).map(|value| {
            if number(token) {
                value
            } else {
                (value * reference) / 100.0
            }
        })
    };
    Some(Parsed {
        mode,
        channels: [lightness, axis(a), axis(b)],
        alpha: modern_alpha(alpha),
    })
}

fn parse_modern_lch(mode: Mode, [l, c, h, alpha]: &[Tok; 4]) -> Option<Parsed> {
    if is_hue(l) || is_percentage(h) {
        return None;
    }
    let number = |token: &Tok| matches!(token, Tok::Number(_));
    let (lightness, chroma) = if mode == Mode::Lch {
        (
            coord(l).map(|value| js_min(js_max(0.0, value), 100.0)),
            coord(c).map(|value| {
                js_max(
                    0.0,
                    if number(c) {
                        value
                    } else {
                        (value * 150.0) / 100.0
                    },
                )
            }),
        )
    } else {
        (
            coord(l).map(|value| {
                js_min(
                    js_max(0.0, if number(l) { value } else { value / 100.0 }),
                    1.0,
                )
            }),
            coord(c).map(|value| {
                js_max(
                    0.0,
                    if number(c) {
                        value
                    } else {
                        (value * 0.4) / 100.0
                    },
                )
            }),
        )
    };
    Some(Parsed {
        mode,
        channels: [lightness, chroma, coord(h)],
        alpha: modern_alpha(alpha),
    })
}

fn parse_modern_mode(name: &str, coords: &[Tok; 4]) -> Option<Parsed> {
    match name {
        "hsl" | "hsla" => parse_modern_hue_first(Mode::Hsl, coords),
        "hwb" => parse_modern_hue_first(Mode::Hwb, coords),
        "lab" => parse_modern_lab(Mode::Lab, coords),
        "oklab" => parse_modern_lab(Mode::Oklab, coords),
        "lch" => parse_modern_lch(Mode::Lch, coords),
        "oklch" => parse_modern_lch(Mode::Oklch, coords),
        _ => None,
    }
}

/// culori's `parseNumber`: a hex integer of 3, 4, 6 or 8 digits.
fn parse_hex_number(value: u32, digits: usize) -> Option<Parsed> {
    let byte = |shift: u32| f64::from((value >> shift) & 0xff) / 255.0;
    let nibble = |shift: u32| {
        let low = (value >> shift) & 0xf;
        f64::from(low | (low << 4)) / 255.0
    };
    match digits {
        3 => Some(Parsed::rgb(nibble(8), nibble(4), nibble(0), None)),
        4 => Some(Parsed::rgb(
            nibble(12),
            nibble(8),
            nibble(4),
            Some(nibble(0)),
        )),
        6 => Some(Parsed::rgb(byte(16), byte(8), byte(0), None)),
        8 => Some(Parsed::rgb(byte(24), byte(16), byte(8), Some(byte(0)))),
        _ => None,
    }
}

/// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`; like culori, the `#` is optional.
fn parse_hex(value: &str) -> Option<Parsed> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if !matches!(digits.len(), 3 | 4 | 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    parse_hex_number(u32::from_str_radix(digits, 16).ok()?, digits.len())
}

fn parse_named(value: &str) -> Option<Parsed> {
    let name = value.to_lowercase();
    let index = super::named::NAMED
        .binary_search_by(|(candidate, _)| (*candidate).cmp(name.as_str()))
        .ok()?;
    super::named::NAMED
        .get(index)
        .and_then(|(_, hex)| parse_hex_number(*hex, 6))
}

const NUM: &str = r"([+-]?\d*\.?\d+(?:[eE][+-]?\d+)?)";

fn legacy_regex(pattern: &str) -> Option<regex_lite::Regex> {
    regex_lite::Regex::new(&pattern.replace("NUM", NUM)).ok()
}

fn group(captures: &regex_lite::Captures<'_>, index: usize) -> Option<f64> {
    captures.get(index).map(|m| js_number(m.as_str()))
}

fn legacy_alpha(captures: &regex_lite::Captures<'_>, percent: usize, number: usize) -> Option<f64> {
    group(captures, percent)
        .map(|value| js_max(0.0, js_min(1.0, value / 100.0)))
        .or_else(|| group(captures, number).map(|value| js_max(0.0, js_min(1.0, value))))
}

/// culori's legacy `rgb()` regexes: all numbers, then all percentages.
static RGB_LEGACY: LazyLock<[Option<regex_lite::Regex>; 2]> = LazyLock::new(|| {
    ["NUM", "NUM%"].map(|unit| {
        legacy_regex(&format!(
            r"^rgba?\(\s*{unit}\s*,\s*{unit}\s*,\s*{unit}\s*(?:,\s*(?:NUM%|NUM)\s*)?\)$"
        ))
    })
});

/// culori's legacy `hsl()` regex.
static HSL_LEGACY: LazyLock<Option<regex_lite::Regex>> = LazyLock::new(|| {
    legacy_regex(
        r"^hsla?\(\s*(?:NUM(deg|grad|rad|turn)|NUM)\s*,\s*NUM%\s*,\s*NUM%\s*(?:,\s*(?:NUM%|NUM)\s*)?\)$",
    )
});

/// Studio's `EXPLICIT_NONE_ALPHA`: a functional colour whose alpha is written `/ none`.
static EXPLICIT_NONE_ALPHA: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(r"/\s*none\s*\)$").ok());

/// `rgb(r, g, b[, a])`, all numbers or all percentages.
fn parse_rgb_legacy(value: &str) -> Option<Parsed> {
    let [numbers, percentages] = &*RGB_LEGACY;
    for (re, scale) in [(numbers, 255.0), (percentages, 100.0)] {
        let Some(captures) = re.as_ref().and_then(|re| re.captures(value)) else {
            continue;
        };
        let channel = |index| group(&captures, index).map(|v| v / scale);
        return Some(Parsed {
            mode: Mode::Rgb,
            channels: [channel(1), channel(2), channel(3)],
            alpha: legacy_alpha(&captures, 4, 5),
        });
    }
    None
}

fn hue_to_deg(value: f64, unit: &str) -> Option<f64> {
    match unit {
        "deg" => Some(value),
        "rad" => Some((value / PI) * 180.0),
        "grad" => Some((value / 10.0) * 9.0),
        "turn" => Some(value * 360.0),
        _ => None,
    }
}

/// `hsl(h, s%, l%[, a])`.
fn parse_hsl_legacy(value: &str) -> Option<Parsed> {
    let captures = HSL_LEGACY.as_ref()?.captures(value)?;
    let hue = group(&captures, 3).or_else(|| {
        let value = group(&captures, 1)?;
        hue_to_deg(value, captures.get(2)?.as_str())
    });
    let percent = |index| group(&captures, index).map(|v| js_min(js_max(0.0, v / 100.0), 1.0));
    Some(Parsed {
        mode: Mode::Hsl,
        channels: [hue, percent(4), percent(5)],
        alpha: legacy_alpha(&captures, 6, 7),
    })
}

/// culori's `parseColorSyntax`: `color(srgb …)` and `color(srgb-linear …)`.
fn parse_color_function(tokens: &[Tok]) -> Result<Option<Parsed>, Thrown> {
    if !matches!(tokens.first(), Some(Tok::Function(name)) if name == "color") {
        return Ok(None);
    }
    let mode = match tokens.get(1) {
        Some(Tok::Ident(profile)) => match profile.as_str() {
            "srgb" => Mode::Rgb,
            "srgb-linear" => Mode::Lrgb,
            // Any other profile is unregistered (or an inherited object member that throws).
            _ => return Ok(None),
        },
        // `token.type` of a missing token throws.
        None | Some(Tok::Invalid) => return Err(Thrown),
        Some(_) => return Ok(None),
    };
    let Some([r, g, b, alpha]) = consume_coords(tokens, 2, false)? else {
        return Ok(None);
    };
    let channel = |token: &Tok| {
        coord(token).map(|value| {
            if matches!(token, Tok::Number(_)) {
                value
            } else {
                value / 100.0
            }
        })
    };
    Ok(Some(Parsed {
        mode,
        channels: [channel(&r), channel(&g), channel(&b)],
        alpha: channel(&alpha).map(|value| js_max(0.0, js_min(1.0, value))),
    }))
}

/// culori's `parse` over the modes Studio registers, in their registration order.
fn parse(value: &str) -> Result<Option<Parsed>, Thrown> {
    let tokens = tokenize(value)?;
    let modern = match &tokens {
        Some(tokens) => parse_modern(tokens)?,
        None => None,
    };
    let modern = modern.as_ref();
    let found = modern
        .and_then(|(name, coords)| parse_modern_rgb(name, coords))
        .or_else(|| parse_hex(value))
        .or_else(|| parse_rgb_legacy(value))
        .or_else(|| parse_named(value))
        .or_else(|| (value == "transparent").then_some(Parsed::rgb(0.0, 0.0, 0.0, Some(0.0))))
        .or_else(|| {
            modern
                .filter(|(name, _)| matches!(name.as_str(), "hsl" | "hsla"))
                .and_then(|(name, coords)| parse_modern_mode(name, coords))
        })
        .or_else(|| parse_hsl_legacy(value))
        .or_else(|| modern.and_then(|(name, coords)| parse_modern_mode(name, coords)));
    if found.is_some() {
        return Ok(found);
    }
    match &tokens {
        Some(tokens) => parse_color_function(tokens),
        None => Ok(None),
    }
}

/// The colour's sRGB channels, each `None` only when it is a missing `rgb()` channel.
fn to_rgb(parsed: &Parsed) -> [Option<f64>; 3] {
    let [first, second, third] = parsed.channels;
    let some = |channels: [f64; 3]| channels.map(Some);
    match parsed.mode {
        Mode::Rgb => parsed.channels,
        Mode::Lrgb => {
            some([first, second, third].map(|channel| linear_to_gamma(channel.unwrap_or(0.0))))
        }
        Mode::Hsl => some(hsl_to_rgb(first, second, third)),
        Mode::Hwb => some(hwb_to_rgb(first, second, third)),
        Mode::Lab => some(lab_to_rgb(first, second, third)),
        Mode::Lch => {
            let [lightness, a_axis, b_axis] = polar_to_axes(first, second, third);
            some(lab_to_rgb(lightness, a_axis, b_axis))
        }
        Mode::Oklab => some(oklab_to_rgb(first, second, third)),
        Mode::Oklch => {
            let [lightness, a_axis, b_axis] = polar_to_axes(first, second, third);
            some(oklab_to_rgb(lightness, a_axis, b_axis))
        }
    }
}

/// Studio's `parseColor`: a CSS colour clipped to sRGB, or `None` when it isn't one.
pub(crate) fn parse_color(raw: &str) -> Option<Color> {
    let value = js_trim(raw);
    if value.is_empty() || value.encode_utf16().count() > 64 {
        return None;
    }
    let parsed = parse(value).ok()??;
    let mut channels = to_rgb(&parsed);
    // culori's `clampRgb`: a colour outside sRGB has every channel clipped (missing ones to 0).
    let displayable = channels
        .iter()
        .flatten()
        .all(|channel| (0.0..=1.0).contains(channel));
    if !displayable {
        channels = channels.map(|channel| Some(clamp01(channel.unwrap_or(0.0))));
    }
    let [r, g, b] = channels.map(|channel| channel.unwrap_or(0.0));
    let explicit_none_alpha = || {
        EXPLICIT_NONE_ALPHA
            .as_ref()
            .is_some_and(|re| re.is_match(value))
    };
    let alpha = parsed
        .alpha
        .unwrap_or_else(|| if explicit_none_alpha() { 0.0 } else { 1.0 });
    Some(Color { r, g, b, alpha })
}

/// culori's `Math.round(clamp(v) * 255)` for one serialized channel.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped to 0..=255 first"
)]
fn channel_byte(channel: f64) -> u8 {
    let clamped = if channel.is_nan() {
        0.0
    } else {
        clamp01(channel)
    };
    (clamped * 255.0).round() as u8
}

/// Studio's `cssColor`: `#rrggbb` when opaque, else `rgba(r, g, b, a)` with alpha to 2 places.
pub(crate) fn css_color(color: Color) -> String {
    let [r, g, b] = [color.r, color.g, color.b].map(channel_byte);
    if color.alpha >= 1.0 {
        return format!("#{r:02x}{g:02x}{b:02x}");
    }
    let alpha = if color.alpha.is_nan() {
        0.0
    } else {
        clamp01(color.alpha)
    };
    let alpha = (alpha * 100.0).round() / 100.0;
    format!("rgba({r}, {g}, {b}, {alpha})")
}

/// Studio's `canonicalColor` (full CSS Color 4 via culori; used for palette parity tests).
#[cfg(test)]
pub(crate) fn canonical_color(raw: &str) -> Option<String> {
    parse_color(raw).map(css_color)
}

/// Parse one theme wire colour (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`,
/// `rgb(r, g, b)` or `rgba(r, g, b, a)`, integer channels `0..=255` and decimal
/// alpha `0..=1`), matching `tools/norn-ts/src/builder/theme.ts`'s `parseThemeColor`
/// and Studio's `parseServedTheme`.
pub(crate) fn parse_wire_color(raw: &str) -> Option<Color> {
    let value = js_trim(raw);
    if let Some(hex) = value.strip_prefix('#') {
        if !matches!(hex.len(), 3 | 4 | 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let wide: String = if hex.len() > 4 {
            hex.to_owned()
        } else {
            hex.chars().flat_map(|c| [c, c]).collect()
        };
        let byte = |at: usize| -> Option<f64> {
            let chunk = wide.get(at..at.saturating_add(2))?;
            u8::from_str_radix(chunk, 16)
                .ok()
                .map(|value| f64::from(value) / 255.0)
        };
        let alpha = if wide.len() == 8 { byte(6)? } else { 1.0 };
        return Some(Color {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            alpha,
        });
    }
    let lower = value.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(js_trim).collect();
    if !matches!(parts.len(), 3 | 4) {
        return None;
    }
    let channel = |text: &str| -> Option<f64> {
        if !(1..=3).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let parsed = text.parse::<u16>().ok()?;
        (parsed <= 255).then(|| f64::from(parsed) / 255.0)
    };
    let r = channel(parts.first()?)?;
    let g = channel(parts.get(1)?)?;
    let b = channel(parts.get(2)?)?;
    let alpha = match parts.get(3) {
        None => 1.0,
        Some(text) => {
            let valid_alpha = *text == "0"
                || *text == "1"
                || text
                    .strip_prefix("0.")
                    .or_else(|| text.strip_prefix('.'))
                    .is_some_and(|frac| {
                        !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit())
                    })
                || text
                    .strip_prefix("1.")
                    .is_some_and(|frac| !frac.is_empty() && frac.bytes().all(|b| b == b'0'));
            if !valid_alpha {
                return None;
            }
            text.parse::<f64>().ok()?
        }
    };
    Some(Color { r, g, b, alpha })
}

/// Canonical CSS for a wire theme colour (`#rrggbb` or `rgba(r, g, b, a)`),
/// rejecting non-canonical CSS forms such as named colours or `oklch()`.
pub(crate) fn canonical_wire_color(raw: &str) -> Option<String> {
    parse_wire_color(raw).map(css_color)
}

/// A channel rounded to the nearest of the 256 values a browser paints.
fn to8(channel: f64) -> f64 {
    (channel * 255.0).round() / 255.0
}

/// Studio's `over`: `top` painted over `bottom` (source-over), each channel rounded to 8 bits.
#[allow(clippy::float_cmp, reason = "culori tests `alpha === 0` exactly")]
pub(crate) fn over(top: Color, bottom: Color) -> Color {
    let alpha = top.alpha + bottom.alpha * (1.0 - top.alpha);
    let mix = |source: f64, backdrop: f64| {
        if alpha == 0.0 {
            return 0.0;
        }
        let value = top.alpha * (1.0 - bottom.alpha) * source
            + top.alpha * bottom.alpha * source
            + (1.0 - top.alpha) * bottom.alpha * backdrop;
        js_max(0.0, js_min(1.0, value / alpha))
    };
    Color {
        r: to8(mix(top.r, bottom.r)),
        g: to8(mix(top.g, bottom.g)),
        b: to8(mix(top.b, bottom.b)),
        alpha,
    }
}

/// Studio's `fade`: the alpha scaled, as Tailwind's `/p` modifier paints it.
pub(crate) fn fade(color: Color, ratio: f64) -> Color {
    Color {
        alpha: color.alpha * ratio,
        ..color
    }
}

fn luminance(color: Color) -> f64 {
    0.2126 * gamma_to_linear(color.r)
        + 0.7152 * gamma_to_linear(color.g)
        + 0.0722 * gamma_to_linear(color.b)
}

/// Studio's `contrastOn`: WCAG 2.x contrast of `ink` painted over the opaque `surface`.
pub(crate) fn contrast_on(ink: Color, surface: Color) -> f64 {
    let first = luminance(over(ink, surface));
    let second = luminance(surface);
    (js_max(first, second) + 0.05) / (js_min(first, second) + 0.05)
}

/// OKLCH chroma of an sRGB colour (alpha ignored), as culori's `oklch` converter reports it.
#[allow(
    clippy::float_cmp,
    reason = "culori zeroes a and b when r === g === b exactly"
)]
pub(crate) fn oklch_chroma(color: Color) -> f64 {
    if color.r == color.g && color.g == color.b {
        return 0.0;
    }
    let [red, green, blue] = [color.r, color.g, color.b].map(gamma_to_linear);
    let long = (0.412_221_469_470_763 * red
        + 0.536_332_537_261_734_8 * green
        + 0.051_445_993_267_502_2 * blue)
        .cbrt();
    let medium = (0.211_903_495_817_825_2 * red
        + 0.680_699_550_645_234_4 * green
        + 0.107_396_953_536_940_6 * blue)
        .cbrt();
    let short = (0.088_302_459_190_056_4 * red
        + 0.281_718_839_136_121_5 * green
        + 0.629_978_701_673_822_2 * blue)
        .cbrt();
    let a_axis = 1.977_998_532_431_168_4 * long - 2.428_592_242_048_58 * medium
        + 0.450_593_709_617_411 * short;
    let b_axis = 0.025_904_042_465_547_8 * long + 0.782_771_712_457_529_6 * medium
        - 0.808_675_754_923_077_4 * short;
    (a_axis * a_axis + b_axis * b_axis).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Studio's answers (`canonicalColor` and culori's OKLCH chroma of `parseColor`, culori 4.0.2
    /// with Studio's registered modes), recorded from `apps/studio/src/lib/color.ts`.
    const STUDIO: &[(&str, Option<&str>, Option<f64>)] = &[
        ("#fff", Some("#ffffff"), Some(0.0)),
        ("#FFF", Some("#ffffff"), Some(0.0)),
        ("fff", Some("#ffffff"), Some(0.0)),
        ("abc", Some("#aabbcc"), Some(0.030_669)),
        ("bad", Some("#bbaadd"), Some(0.073_962)),
        ("0abcde", Some("#0abcde"), Some(0.130_05)),
        ("#12345678", Some("rgba(18, 52, 86, 0.47)"), Some(0.072_455)),
        ("#1234", Some("rgba(17, 34, 51, 0.27)"), Some(0.039_848)),
        ("#12345", None, None),
        (
            "deadbeef",
            Some("rgba(222, 173, 190, 0.94)"),
            Some(0.061_141),
        ),
        ("red", Some("#ff0000"), Some(0.257_683)),
        ("RED", Some("#ff0000"), Some(0.257_683)),
        (" Blue ", Some("#0000ff"), Some(0.313_214)),
        ("transparent", Some("rgba(0, 0, 0, 0)"), Some(0.0)),
        ("Transparent", None, None),
        ("rebeccapurple", Some("#663399"), Some(0.160_296)),
        ("rgb(255 0 0)", Some("#ff0000"), Some(0.257_683)),
        ("rgb(255, 0, 0)", Some("#ff0000"), Some(0.257_683)),
        (
            "rgba(255,0,0,0.5)",
            Some("rgba(255, 0, 0, 0.5)"),
            Some(0.257_683),
        ),
        (
            "rgba(255, 0, 0, 50%)",
            Some("rgba(255, 0, 0, 0.5)"),
            Some(0.257_683),
        ),
        (
            "rgb(100% 0% 0% / 25%)",
            Some("rgba(255, 0, 0, 0.25)"),
            Some(0.257_683),
        ),
        ("rgb(1e2 2e1 .5)", Some("#641401"), Some(0.116_276)),
        ("rgb(+10 -10 300)", Some("#0a00ff"), Some(0.312_138)),
        (
            "rgb(10 20 30 / none)",
            Some("rgba(10, 20, 30, 0)"),
            Some(0.025_336),
        ),
        ("rgb(none 20 30)", Some("#00141e"), Some(0.035_711)),
        ("rgb(10 20 30", Some("#0a141e"), Some(0.025_336)),
        ("rgb(10 20 30))", None, None),
        ("RGB(1 2 3)", None, None),
        ("rgb(10px 0 0)", None, None),
        ("rgb(1 2 3 / 5px)", None, None),
        (
            "rgb(1 2 3 / 0.333)",
            Some("rgba(1, 2, 3, 0.33)"),
            Some(0.008_035),
        ),
        ("hsl(120 50% 50%)", Some("#40bf40"), Some(0.200_836)),
        ("hsl(120deg 50% 50%)", Some("#40bf40"), Some(0.200_836)),
        ("hsl(0.5turn 100% 25%)", Some("#008080"), Some(0.092_449)),
        ("hsl(3.14159rad 100% 50%)", Some("#00ffff"), Some(0.154_55)),
        ("hsl(200grad 100% 50%)", Some("#00ffff"), Some(0.154_55)),
        (
            "hsla(120, 50%, 50%, 0.3)",
            Some("rgba(64, 191, 64, 0.3)"),
            Some(0.200_836),
        ),
        ("hsl(120, 50%, 50%)", Some("#40bf40"), Some(0.200_836)),
        ("hsl(-120 50% 50%)", Some("#4040bf"), Some(0.193_608)),
        ("hsl(480 50% 50%)", Some("#40bf40"), Some(0.200_836)),
        ("hsl(120 50 50)", Some("#40bf40"), Some(0.200_836)),
        ("hsl(120% 50% 50%)", None, None),
        ("hsl(none 0% 40%)", Some("#666666"), Some(0.0)),
        ("hwb(120 20% 30%)", Some("#33b333"), Some(0.198_689)),
        ("hwb(0 60% 60%)", Some("#808080"), Some(0.0)),
        ("lab(50 40 -20)", Some("#ab5a9a"), Some(0.132_365)),
        ("lab(50% 40% -20%)", Some("#b750a3"), Some(0.166_155)),
        ("lch(60 50 120)", Some("#769c3e"), Some(0.129_683)),
        ("lch(60 50% 120deg)", Some("#61a100"), Some(0.177_521)),
        ("oklab(0.6 0.1 -0.1)", Some("#9f63ba"), Some(0.141_421)),
        ("oklab(60% 25% -25%)", Some("#9f63ba"), Some(0.141_421)),
        ("oklch(0.7 0.15 250)", Some("#4ba3f7"), Some(0.15)),
        (
            "oklch(70% 0.15 250 / 0.5)",
            Some("rgba(75, 163, 247, 0.5)"),
            Some(0.15),
        ),
        ("oklch(0.9 0.4 30)", Some("#ff0000"), Some(0.257_683)),
        ("oklch(0.5 0 0)", Some("#636363"), Some(0.0)),
        ("oklch(1 0 0)", Some("#ffffff"), Some(0.0)),
        ("color(srgb 1 0 0)", Some("#ff0000"), Some(0.257_683)),
        (
            "color(srgb 100% 50% 0% / 50%)",
            Some("rgba(255, 128, 0, 0.5)"),
            Some(0.186_114),
        ),
        ("color(srgb-linear 0.5 0.5 0.5)", Some("#bcbcbc"), Some(0.0)),
        ("color(display-p3 1 0 0)", None, None),
        ("color(", None, None),
        ("color(srgb 1 0)", None, None),
        ("", None, None),
        ("   ", None, None),
        ("#ggg", None, None),
        ("rgb()", None, None),
        ("rgb(1,2)", None, None),
        ("rgb(1 2 3 4)", None, None),
        ("rgb(1, 2 3)", None, None),
        ("hsl(1,2,3)", None, None),
        ("-red", None, None),
        ("--x", None, None),
        ("#1e5", Some("#11ee55"), Some(0.247_583)),
        ("1e5", Some("#11ee55"), Some(0.247_583)),
        ("rgb(.5 .5 .5)", Some("#010101"), Some(0.0)),
        ("rgb(0.5e1 1 1)", Some("#050101"), Some(0.018_465)),
        ("rgba(0 0 0 / 0.07)", Some("rgba(0, 0, 0, 0.07)"), Some(0.0)),
        ("rgba(0,0,0,0.065)", Some("rgba(0, 0, 0, 0.07)"), Some(0.0)),
        ("rgb(0 0 0 / 0.995)", Some("rgba(0, 0, 0, 1)"), Some(0.0)),
        ("rgb(0 0 0 / -1)", Some("rgba(0, 0, 0, 0)"), Some(0.0)),
        ("rgb(0 0 0 / 2)", Some("#000000"), Some(0.0)),
        (
            "oklch(0.6 0.2 30 / none)",
            Some("rgba(222, 62, 45, 0)"),
            Some(0.2),
        ),
        (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
            None,
        ),
        (
            "#aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            None,
            None,
        ),
    ];

    #[test]
    fn colours_read_exactly_as_studio_reads_them() {
        for (input, canonical, chroma) in STUDIO {
            assert_eq!(canonical_color(input).as_deref(), *canonical, "{input:?}");
            let measured = parse_color(input).map(oklch_chroma);
            match (measured, chroma) {
                (Some(measured), Some(chroma)) => {
                    assert!(
                        (measured - chroma).abs() < 1e-5,
                        "{input:?}: chroma {measured} != {chroma}"
                    );
                }
                (None, None) => {}
                other => panic!("{input:?}: chroma {other:?}"),
            }
        }
    }

    #[test]
    fn contrast_matches_studio() {
        let ratio = |ink: &str, surface: &str| {
            contrast_on(
                parse_color(ink).expect("ink"),
                parse_color(surface).expect("surface"),
            )
        };
        assert!((ratio("#6d6d6d", "#eceef2") - 4.454_464_954_101_925).abs() < 1e-12);
        assert!((ratio("rgba(0,0,0,0.5)", "#ffffff") - 3.949_439_648_049_115_6).abs() < 1e-12);
    }
}
