//! Accent selection and contrast fitting.

use crate::{
    color::{Oklab, Oklch, Rgb, SEARCH_STEPS},
    quantize::{self, Swatch},
    ThemeError,
};
use std::ops::RangeInclusive;

/// Default surface the accent is read against: the app's `--bg-base`.
pub const DEFAULT_BACKGROUND: Rgb = Rgb::new(0x18, 0x18, 0x1a);
/// Default minimum WCAG 2.x contrast ratio for text and icons (AA body text).
pub const DEFAULT_MIN_TEXT_CONTRAST: f64 = 4.5;
/// Text and icon color for accent fills, used when it contrasts more than
/// [`ON_ACCENT_DARK`].
pub const ON_ACCENT_LIGHT: Rgb = Rgb::new(0xff, 0xff, 0xff);
/// Near-black text and icon color for light accent fills; softer than black.
pub const ON_ACCENT_DARK: Rgb = Rgb::new(0x10, 0x10, 0x12);
/// Text color that every ambient color carries at the minimum text contrast.
pub const AMBIENT_TEXT: Rgb = Rgb::new(0xff, 0xff, 0xff);
/// Number of ambient colors in a [`Palette`].
pub const AMBIENT_COUNT: usize = 3;

const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);
const BLACK: Rgb = Rgb::new(0, 0, 0);
/// Every WCAG 2.x contrast ratio lies in this range.
const CONTRAST_RANGE: RangeInclusive<f64> = 1.0..=21.0;
/// Smallest share of sampled pixels (0–1) a swatch needs to compete for the
/// accent, so specks and noise never become the theme.
const MIN_ACCENT_SHARE: f64 = 0.02;
/// Exponent on a swatch's population share when scoring accents; below 1 it
/// favors characteristic colors over sheer area.
const POPULATION_EXPONENT: f64 = 0.5;
/// OKLCH chroma from which a swatch counts as fully vivid when scoring accents.
const VIVID_CHROMA: f64 = 0.12;
/// OKLCH chroma below which every competing swatch makes the artwork neutral.
const NEUTRAL_CHROMA: f64 = 0.03;
/// OKLab lightness window for accent scoring: suitability rises from 0 at
/// `[0]` to 1 at `[1]`, stays 1 until `[2]`, and falls back to 0 at `[3]`.
const ACCENT_LIGHTNESS_WINDOW: [f64; 4] = [0.12, 0.45, 0.82, 0.97];
/// Suitability outside the window, so very dark or pale artwork still yields
/// an accent of its own hue.
const LIGHTNESS_SUITABILITY_FLOOR: f64 = 0.1;
/// OKLab lightness of the hue-less accent of neutral artwork; a light gray
/// reads as deliberate on dark surfaces.
const NEUTRAL_ACCENT_LIGHTNESS: f64 = 0.86;
/// OKLab lightness distance between `accent` and `accent_hover`.
pub(crate) const HOVER_LIGHTNESS_STEP: f64 = 0.06;
/// OKLab lightness range of `accent_hover`; at the extremes sRGB has no
/// chroma left to keep the hue.
const HOVER_LIGHTNESS_RANGE: RangeInclusive<f64> = 0.04..=0.96;
/// OKLab lightness range of ambient colors: dark enough to sit behind white
/// text, never pure black.
pub(crate) const AMBIENT_LIGHTNESS_RANGE: RangeInclusive<f64> = 0.18..=0.42;
/// Largest OKLCH chroma of an ambient color, so large glows stay calm.
pub(crate) const AMBIENT_MAX_CHROMA: f64 = 0.12;
/// Smallest OKLab distance between two ambient colors.
pub(crate) const AMBIENT_DISTINCT_DISTANCE: f64 = 0.06;
/// OKLab lightness step between the dominant ambient color and the variants
/// derived from it when the artwork has fewer distinct colors.
const AMBIENT_DERIVED_STEP: f64 = 0.08;

/// Colors derived from one artwork, ready to theme an interface.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    /// The artwork's most characteristic color. Its hue is kept while OKLCH
    /// lightness moves (and chroma shrinks only to stay in sRGB) until text and
    /// icons in this color reach the minimum contrast against the background.
    pub accent: Rgb,
    /// A hover or pressed variant of the same hue: lighter on dark
    /// backgrounds, or darker when the accent is already at the lightness
    /// ceiling. It meets the same contrast targets as `accent`, including
    /// against `on_accent`.
    pub accent_hover: Rgb,
    /// Text and icon color for content drawn on an `accent` fill: whichever of
    /// [`ON_ACCENT_LIGHT`] and [`ON_ACCENT_DARK`] contrasts more, and always at
    /// least the minimum contrast against both `accent` and `accent_hover`.
    pub on_accent: Rgb,
    /// Dark atmosphere colors for background glows and gradients, dominant
    /// first, distinct from each other when the artwork allows. Each keeps
    /// [`AMBIENT_TEXT`] at the minimum contrast.
    pub ambient: [Rgb; AMBIENT_COUNT],
    /// The artwork has no meaningful chroma, so every color stays hue-less
    /// instead of inventing a hue.
    pub neutral: bool,
}

/// The surface and contrast target a palette is fitted to.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteOptions {
    /// Surface on which `accent` and `accent_hover` are read as text or icons.
    /// Default: [`DEFAULT_BACKGROUND`].
    pub background: Rgb,
    /// Minimum WCAG 2.x contrast ratio, `1.0..=21.0`, for every text pairing
    /// the palette promises. Default: [`DEFAULT_MIN_TEXT_CONTRAST`].
    pub min_text_contrast: f64,
}

impl Default for PaletteOptions {
    fn default() -> Self {
        Self {
            background: DEFAULT_BACKGROUND,
            min_text_contrast: DEFAULT_MIN_TEXT_CONTRAST,
        }
    }
}

impl PaletteOptions {
    fn validate(&self) -> Result<(), ThemeError> {
        if CONTRAST_RANGE.contains(&self.min_text_contrast) {
            Ok(())
        } else {
            Err(ThemeError::InvalidOptions(
                "min_text_contrast must be a WCAG ratio from 1 to 21",
            ))
        }
    }
}

/// Builds a palette from sampled, opaque artwork pixels in any order.
///
/// Callers decode and downsample the artwork first and drop pixels that are
/// mostly transparent; a few thousand samples (such as a 64×64 thumbnail)
/// are plenty. Equal input always produces an equal palette. See [`Palette`]
/// for the contrast guarantees of the result.
pub fn from_pixels(pixels: &[Rgb], options: &PaletteOptions) -> Result<Palette, ThemeError> {
    options.validate()?;
    let swatches = quantize::swatches(pixels);
    if swatches.is_empty() {
        return Err(ThemeError::NoOpaquePixels);
    }
    let (source, neutral) = select_accent(&swatches);
    let minimum = options.min_text_contrast;
    let background = options.background;
    let direction = Direction::away_from(background);
    let accent = fit(source, direction, |color| {
        color.contrast_ratio(background) >= minimum
            && on_accent_for(color).contrast_ratio(color) >= minimum
    });
    let on_accent = on_accent_for(accent);
    let accent_hover = hover(accent, direction, |color| {
        color != accent
            && color.contrast_ratio(background) >= minimum
            && on_accent.contrast_ratio(color) >= minimum
    });
    Ok(Palette {
        accent,
        accent_hover,
        on_accent,
        ambient: ambient(&swatches, neutral, minimum),
        neutral,
    })
}

/// The swatch whose color best characterizes the artwork, and whether the
/// artwork is neutral (then the source is a hue-less light gray).
fn select_accent(swatches: &[Swatch]) -> (Oklch, bool) {
    let total = swatches.iter().map(|swatch| swatch.population).sum::<u64>() as f64;
    let share = |swatch: &Swatch| swatch.population as f64 / total;
    let qualifying: Vec<&Swatch> = swatches
        .iter()
        .filter(|swatch| share(swatch) >= MIN_ACCENT_SHARE)
        .collect();
    // Extremely fragmented artwork has no swatch above the share threshold;
    // judging every swatch then is better than declaring it colorless.
    let competing = if qualifying.is_empty() {
        swatches.iter().collect()
    } else {
        qualifying
    };
    let neutral_source = Oklch {
        l: NEUTRAL_ACCENT_LIGHTNESS,
        c: 0.0,
        h: 0.0,
    };
    if competing
        .iter()
        .all(|swatch| swatch.color.to_lch().c < NEUTRAL_CHROMA)
    {
        return (neutral_source, true);
    }
    competing
        .into_iter()
        .map(|swatch| {
            let lch = swatch.color.to_lch();
            let vividness = (lch.c / VIVID_CHROMA).min(1.0);
            let score =
                share(swatch).powf(POPULATION_EXPONENT) * vividness * lightness_suitability(lch.l);
            (score, lch)
        })
        // Strictly greater keeps the more populous swatch on ties.
        .fold(None, |best: Option<(f64, Oklch)>, candidate| match best {
            Some(top) if top.0 >= candidate.0 => Some(top),
            _ => Some(candidate),
        })
        .map_or((neutral_source, true), |(_, lch)| (lch, false))
}

fn lightness_suitability(lightness: f64) -> f64 {
    let [dark, ideal_low, ideal_high, light] = ACCENT_LIGHTNESS_WINDOW;
    let suitability = if lightness <= dark || lightness >= light {
        0.0
    } else if lightness < ideal_low {
        (lightness - dark) / (ideal_low - dark)
    } else if lightness > ideal_high {
        (light - lightness) / (light - ideal_high)
    } else {
        1.0
    };
    suitability.max(LIGHTNESS_SUITABILITY_FLOOR)
}

/// The lightness direction in which contrast against a reference color grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Lighter,
    Darker,
}

impl Direction {
    fn away_from(reference: Rgb) -> Self {
        if WHITE.contrast_ratio(reference) >= BLACK.contrast_ratio(reference) {
            Self::Lighter
        } else {
            Self::Darker
        }
    }

    fn limit(self) -> f64 {
        match self {
            Self::Lighter => 1.0,
            Self::Darker => 0.0,
        }
    }

    fn shift(self, lightness: f64, amount: f64) -> f64 {
        match self {
            Self::Lighter => lightness + amount,
            Self::Darker => lightness - amount,
        }
    }

    fn reverse(self) -> Self {
        match self {
            Self::Lighter => Self::Darker,
            Self::Darker => Self::Lighter,
        }
    }
}

/// Moves `color` toward `direction`'s lightness limit by the smallest amount,
/// found by bisection, for which `accept` holds on the final 8-bit color. Hue
/// is kept and chroma only shrinks to stay in sRGB. Returns the limit color
/// when even it is not accepted (an unreachable target).
fn fit(color: Oklch, direction: Direction, accept: impl Fn(Rgb) -> bool) -> Rgb {
    let start = color.to_rgb_in_gamut();
    if accept(start) {
        return start;
    }
    let limit = color.with_lightness(direction.limit()).to_rgb_in_gamut();
    if !accept(limit) {
        return limit;
    }
    // `accept` is evaluated on rounded colors and gamut mapping bends the
    // curve, so the search only ever returns a candidate that passed.
    let (mut failing, mut passing, mut best) = (color.l, direction.limit(), limit);
    for _ in 0..SEARCH_STEPS {
        let middle = (failing + passing) / 2.0;
        let candidate = color.with_lightness(middle).to_rgb_in_gamut();
        if accept(candidate) {
            passing = middle;
            best = candidate;
        } else {
            failing = middle;
        }
    }
    best
}

fn hover(accent: Rgb, direction: Direction, accept: impl Fn(Rgb) -> bool) -> Rgb {
    let base = Oklab::from_rgb(accent).to_lch();
    [direction, direction.reverse()]
        .into_iter()
        .map(|toward| toward.shift(base.l, HOVER_LIGHTNESS_STEP))
        .filter(|lightness| HOVER_LIGHTNESS_RANGE.contains(lightness))
        .map(|lightness| base.with_lightness(lightness).to_rgb_in_gamut())
        .find(|candidate| accept(*candidate))
        .unwrap_or(accent)
}

fn on_accent_for(fill: Rgb) -> Rgb {
    if ON_ACCENT_LIGHT.contrast_ratio(fill) >= ON_ACCENT_DARK.contrast_ratio(fill) {
        ON_ACCENT_LIGHT
    } else {
        ON_ACCENT_DARK
    }
}

fn ambient(swatches: &[Swatch], neutral: bool, minimum: f64) -> [Rgb; AMBIENT_COUNT] {
    let direction = Direction::away_from(AMBIENT_TEXT);
    let readable = |color: Rgb| AMBIENT_TEXT.contrast_ratio(color) >= minimum;
    let (lowest, highest) = (
        *AMBIENT_LIGHTNESS_RANGE.start(),
        *AMBIENT_LIGHTNESS_RANGE.end(),
    );
    let tone = |color: Oklab| {
        let lch = color.to_lch();
        Oklch {
            l: lch.l.clamp(lowest, highest),
            c: if neutral {
                0.0
            } else {
                lch.c.min(AMBIENT_MAX_CHROMA)
            },
            h: lch.h,
        }
    };
    let mut colors = Vec::with_capacity(AMBIENT_COUNT);
    for swatch in swatches {
        if colors.len() == AMBIENT_COUNT {
            break;
        }
        push_distinct(&mut colors, fit(tone(swatch.color), direction, readable));
    }
    let dominant = swatches.first().map_or(
        Oklch {
            l: lowest,
            c: 0.0,
            h: 0.0,
        },
        |swatch| tone(swatch.color),
    );
    for step in 1..=AMBIENT_COUNT {
        for sign in [-1.0, 1.0] {
            let lightness = dominant.l + sign * step as f64 * AMBIENT_DERIVED_STEP;
            if colors.len() < AMBIENT_COUNT && AMBIENT_LIGHTNESS_RANGE.contains(&lightness) {
                push_distinct(
                    &mut colors,
                    fit(dominant.with_lightness(lightness), direction, readable),
                );
            }
        }
    }
    let fallback = fit(dominant.with_lightness(lowest), direction, readable);
    let mut ambient = [fallback; AMBIENT_COUNT];
    for (slot, color) in ambient.iter_mut().zip(colors) {
        *slot = color;
    }
    ambient
}

fn push_distinct(colors: &mut Vec<Rgb>, color: Rgb) {
    let lab = Oklab::from_rgb(color);
    if colors
        .iter()
        .all(|other| Oklab::from_rgb(*other).distance(lab) >= AMBIENT_DISTINCT_DISTANCE)
    {
        colors.push(color);
    }
}
