//! culori's conversions from each CSS colour space Studio registers to sRGB, and the sRGB
//! transfer function both ways (`apps/studio/src/lib/color.ts` reads colours through these).

use std::f64::consts::PI;

fn normalize_hue(hue: f64) -> f64 {
    let hue = hue % 360.0;
    if hue < 0.0 { hue + 360.0 } else { hue }
}

/// The sextant `Math.floor(h / 60)` of a normalized hue, or `None` outside 0–5 (culori's
/// `default` branch).
fn sextant(hue: f64) -> Option<u8> {
    let sector = (hue / 60.0).floor();
    (0_u8..6).find(|candidate| (f64::from(*candidate) - sector).abs() < 0.5)
}

pub(super) fn hsl_to_rgb(h: Option<f64>, s: Option<f64>, l: Option<f64>) -> [f64; 3] {
    let h = normalize_hue(h.unwrap_or(0.0));
    let s = s.unwrap_or(0.0);
    let l = l.unwrap_or(0.0);
    let m1 = l + s * (if l < 0.5 { l } else { 1.0 - l });
    let m2 = m1 - (m1 - l) * 2.0 * (((h / 60.0) % 2.0) - 1.0).abs();
    let low = 2.0 * l - m1;
    match sextant(h) {
        Some(0) => [m1, m2, low],
        Some(1) => [m2, m1, low],
        Some(2) => [low, m1, m2],
        Some(3) => [low, m2, m1],
        Some(4) => [m2, low, m1],
        Some(5) => [m1, low, m2],
        _ => [low, low, low],
    }
}

fn hsv_to_rgb(h: Option<f64>, s: f64, v: f64) -> [f64; 3] {
    let h = normalize_hue(h.unwrap_or(0.0));
    let f = (((h / 60.0) % 2.0) - 1.0).abs();
    match sextant(h) {
        Some(0) => [v, v * (1.0 - s * f), v * (1.0 - s)],
        Some(1) => [v * (1.0 - s * f), v, v * (1.0 - s)],
        Some(2) => [v * (1.0 - s), v, v * (1.0 - s * f)],
        Some(3) => [v * (1.0 - s), v * (1.0 - s * f), v],
        Some(4) => [v * (1.0 - s * f), v * (1.0 - s), v],
        Some(5) => [v, v * (1.0 - s), v * (1.0 - s * f)],
        _ => [v * (1.0 - s), v * (1.0 - s), v * (1.0 - s)],
    }
}

#[allow(clippy::float_cmp, reason = "culori tests `b === 1` exactly")]
pub(super) fn hwb_to_rgb(h: Option<f64>, w: Option<f64>, b: Option<f64>) -> [f64; 3] {
    let mut w = w.unwrap_or(0.0);
    let mut b = b.unwrap_or(0.0);
    if w + b > 1.0 {
        let sum = w + b;
        w /= sum;
        b /= sum;
    }
    let s = if b == 1.0 { 1.0 } else { 1.0 - w / (1.0 - b) };
    hsv_to_rgb(h, s, 1.0 - b)
}

pub(super) fn linear_to_gamma(channel: f64) -> f64 {
    let magnitude = channel.abs();
    if magnitude > 0.003_130_8 {
        channel.signum() * (1.055 * magnitude.powf(1.0 / 2.4) - 0.055)
    } else {
        channel * 12.92
    }
}

pub(super) fn gamma_to_linear(channel: f64) -> f64 {
    let magnitude = channel.abs();
    if magnitude <= 0.040_45 {
        channel / 12.92
    } else {
        channel.signum() * ((magnitude + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn lab_to_rgb(l: Option<f64>, a: Option<f64>, b: Option<f64>) -> [f64; 3] {
    let kappa = 29.0_f64.powf(3.0) / 3.0_f64.powf(3.0);
    let epsilon = 6.0_f64.powf(3.0) / 29.0_f64.powf(3.0);
    let finv = |value: f64| {
        if value.powf(3.0) > epsilon {
            value.powf(3.0)
        } else {
            (116.0 * value - 16.0) / kappa
        }
    };
    let fy = (l.unwrap_or(0.0) + 16.0) / 116.0;
    let fx = a.unwrap_or(0.0) / 500.0 + fy;
    let fz = fy - b.unwrap_or(0.0) / 200.0;
    let xx = finv(fx) * (0.3457 / 0.3585);
    let yy = finv(fy);
    let zz = finv(fz) * ((1.0 - 0.3457 - 0.3585) / 0.3585);
    [
        xx * 3.134_135_956_995_870_7 - yy * 1.617_386_332_161_253_8 - 0.490_661_946_008_353_2 * zz,
        xx * -0.978_795_502_912_089 + yy * 1.916_254_567_259_524 + 0.033_442_731_161_319_49 * zz,
        xx * 0.071_955_379_884_116_77 - yy * 0.228_976_826_415_832_2 + 1.405_386_058_324_125 * zz,
    ]
    .map(linear_to_gamma)
}

/// lch to lab (and oklch to oklab): `[l, a, b]`, with `l` passed through.
#[allow(
    clippy::float_cmp,
    reason = "culori tests the chroma's truthiness: 0 and NaN are falsy"
)]
pub(super) fn polar_to_axes(
    lightness: Option<f64>,
    chroma: Option<f64>,
    hue: Option<f64>,
) -> [Option<f64>; 3] {
    let hue = hue.unwrap_or(0.0);
    let (a_axis, b_axis) = match chroma {
        Some(chroma) if chroma != 0.0 && !chroma.is_nan() => (
            chroma * ((hue / 180.0) * PI).cos(),
            chroma * ((hue / 180.0) * PI).sin(),
        ),
        _ => (0.0, 0.0),
    };
    [lightness, Some(a_axis), Some(b_axis)]
}

pub(super) fn oklab_to_rgb(l: Option<f64>, a: Option<f64>, b: Option<f64>) -> [f64; 3] {
    let (lightness, a_axis, b_axis) = (l.unwrap_or(0.0), a.unwrap_or(0.0), b.unwrap_or(0.0));
    let long =
        (lightness + 0.396_337_777_376_174_9 * a_axis + 0.215_803_757_309_913_6 * b_axis).powf(3.0);
    let medium =
        (lightness - 0.105_561_345_815_658_6 * a_axis - 0.063_854_172_825_813_3 * b_axis).powf(3.0);
    let short =
        (lightness - 0.089_484_177_529_811_9 * a_axis - 1.291_485_548_019_409_2 * b_axis).powf(3.0);
    [
        4.076_741_636_075_957 * long - 3.307_711_539_258_061_6 * medium
            + 0.230_969_903_182_104_4 * short,
        -1.268_437_973_285_031_7 * long + 2.609_757_349_287_688_7 * medium
            - 0.341_319_376_002_657_3 * short,
        -0.004_196_076_138_675_6 * long - 0.703_418_617_935_936_2 * medium
            + 1.707_614_694_074_611_7 * short,
    ]
    .map(linear_to_gamma)
}
