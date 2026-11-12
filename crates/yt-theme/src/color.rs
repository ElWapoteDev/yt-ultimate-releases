//! sRGB, OKLab/OKLCH and WCAG 2.x contrast math.
//!
//! The OKLab matrices are the published constants of Björn Ottosson's
//! "A perceptual color space for image processing" (2020).

/// An opaque color in 8-bit sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Bisection steps used by every color search (lightness and chroma). 24
/// halvings resolve OKLab coordinates far below one 8-bit sRGB step.
pub(crate) const SEARCH_STEPS: u32 = 24;
/// Largest 8-bit channel value: the divisor that maps channels to `0.0..=1.0`.
pub(crate) const CHANNEL_MAX: f64 = 255.0;

/// Encoded-value breakpoint of the sRGB transfer function (IEC 61966-2-1).
const SRGB_ENCODED_BREAKPOINT: f64 = 0.04045;
/// Linear-light breakpoint of the inverse sRGB transfer function.
const SRGB_LINEAR_BREAKPOINT: f64 = 0.003_130_8;
/// Slope of the linear segment of the sRGB transfer function.
const SRGB_LINEAR_SLOPE: f64 = 12.92;
/// Exponent of the power segment of the sRGB transfer function.
const SRGB_EXPONENT: f64 = 2.4;
/// Offset of the power segment of the sRGB transfer function.
const SRGB_OFFSET: f64 = 0.055;
/// WCAG 2.x relative-luminance weights of linear R, G and B (Rec. 709 primaries).
const LUMINANCE_WEIGHTS: [f64; 3] = [0.2126, 0.7152, 0.0722];
/// Flare term that WCAG 2.x adds to both luminances of a contrast ratio.
const CONTRAST_FLARE: f64 = 0.05;
/// Tolerance for floating-point error when testing sRGB gamut membership.
const GAMUT_TOLERANCE: f64 = 1e-9;

const LINEAR_SRGB_TO_LMS: [[f64; 3]; 3] = [
    [0.412_221_470_8, 0.536_332_536_3, 0.051_445_992_9],
    [0.211_903_498_2, 0.680_699_545_1, 0.107_396_956_6],
    [0.088_302_461_9, 0.281_718_837_6, 0.629_978_700_5],
];
const LMS_ROOT_TO_OKLAB: [[f64; 3]; 3] = [
    [0.210_454_255_3, 0.793_617_785_0, -0.004_072_046_8],
    [1.977_998_495_1, -2.428_592_205_0, 0.450_593_709_9],
    [0.025_904_037_1, 0.782_771_766_2, -0.808_675_766_0],
];
const OKLAB_TO_LMS_ROOT: [[f64; 3]; 3] = [
    [1.0, 0.396_337_777_4, 0.215_803_757_3],
    [1.0, -0.105_561_345_8, -0.063_854_172_8],
    [1.0, -0.089_484_177_5, -1.291_485_548_0],
];
const LMS_TO_LINEAR_SRGB: [[f64; 3]; 3] = [
    [4.076_741_662_1, -3.307_711_591_3, 0.230_969_929_2],
    [-1.268_438_004_6, 2.609_757_401_1, -0.341_319_396_5],
    [-0.004_196_086_3, -0.703_418_614_7, 1.707_614_701_0],
];

impl Rgb {
    /// Creates a color from 8-bit sRGB channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Lowercase CSS hexadecimal notation, `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// WCAG 2.x relative luminance in `0.0..=1.0` (black is 0, white is 1).
    pub fn relative_luminance(self) -> f64 {
        self.linear()
            .iter()
            .zip(LUMINANCE_WEIGHTS)
            .map(|(channel, weight)| channel * weight)
            .sum()
    }

    /// WCAG 2.x contrast ratio in `1.0..=21.0`; symmetric in its two colors.
    pub fn contrast_ratio(self, other: Rgb) -> f64 {
        let (first, second) = (self.relative_luminance(), other.relative_luminance());
        let (lighter, darker) = if first >= second {
            (first, second)
        } else {
            (second, first)
        };
        (lighter + CONTRAST_FLARE) / (darker + CONTRAST_FLARE)
    }

    pub(crate) fn linear(self) -> [f64; 3] {
        [self.r, self.g, self.b].map(|channel| decode_srgb(f64::from(channel) / CHANNEL_MAX))
    }

    /// Rounds linear-light channels to 8-bit sRGB, clamping float noise at the
    /// gamut edges.
    pub(crate) fn from_linear(linear: [f64; 3]) -> Self {
        let [r, g, b] = linear
            .map(|channel| (encode_srgb(channel.clamp(0.0, 1.0)) * CHANNEL_MAX).round() as u8);
        Self { r, g, b }
    }
}

impl From<[u8; 3]> for Rgb {
    /// Reads `[r, g, b]` 8-bit sRGB channels, the sample layout `yt-core`
    /// produces when decoding artwork.
    fn from([r, g, b]: [u8; 3]) -> Self {
        Self::new(r, g, b)
    }
}

/// A color in OKLab: `l` is perceived lightness (black 0, white 1); `a` and
/// `b` are the green–red and blue–yellow opponent axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Oklab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

impl Oklab {
    pub fn from_rgb(color: Rgb) -> Self {
        Self::from_linear(color.linear())
    }

    /// From non-linear sRGB channels in `0.0..=1.0`, such as averaged 8-bit
    /// values that no longer fall on whole steps.
    pub fn from_encoded(encoded: [f64; 3]) -> Self {
        Self::from_linear(encoded.map(decode_srgb))
    }

    fn from_linear(linear: [f64; 3]) -> Self {
        let roots = multiply(&LINEAR_SRGB_TO_LMS, linear).map(f64::cbrt);
        let [l, a, b] = multiply(&LMS_ROOT_TO_OKLAB, roots);
        Self { l, a, b }
    }

    fn to_linear(self) -> [f64; 3] {
        let cone = multiply(&OKLAB_TO_LMS_ROOT, [self.l, self.a, self.b]).map(|root| root.powi(3));
        multiply(&LMS_TO_LINEAR_SRGB, cone)
    }

    /// Euclidean OKLab distance (ΔEOK); about 0.02 is one just-noticeable
    /// difference.
    pub fn distance(self, other: Self) -> f64 {
        ((self.l - other.l).powi(2) + (self.a - other.a).powi(2) + (self.b - other.b).powi(2))
            .sqrt()
    }

    pub fn to_lch(self) -> Oklch {
        Oklch {
            l: self.l,
            c: self.a.hypot(self.b),
            h: self.b.atan2(self.a),
        }
    }
}

/// OKLab in polar form: lightness, chroma, and hue angle in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

impl Oklch {
    pub fn to_lab(self) -> Oklab {
        Oklab {
            l: self.l,
            a: self.c * self.h.cos(),
            b: self.c * self.h.sin(),
        }
    }

    pub fn with_lightness(self, l: f64) -> Self {
        Self { l, ..self }
    }

    /// The 8-bit color with this hue and lightness (clamped to `0.0..=1.0`)
    /// and the largest chroma, not above `self.c`, inside the sRGB gamut.
    /// Only chroma is reduced, so hue and lightness survive gamut mapping.
    pub fn to_rgb_in_gamut(self) -> Rgb {
        let lightness = self.l.clamp(0.0, 1.0);
        let linear_at = |chroma: f64| {
            Oklch {
                l: lightness,
                c: chroma,
                h: self.h,
            }
            .to_lab()
            .to_linear()
        };
        let requested = self.c.max(0.0);
        let full = linear_at(requested);
        if in_gamut(full) {
            return Rgb::from_linear(full);
        }
        let (mut inside, mut outside) = (0.0, requested);
        for _ in 0..SEARCH_STEPS {
            let middle = (inside + outside) / 2.0;
            if in_gamut(linear_at(middle)) {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        Rgb::from_linear(linear_at(inside))
    }
}

fn decode_srgb(encoded: f64) -> f64 {
    if encoded <= SRGB_ENCODED_BREAKPOINT {
        encoded / SRGB_LINEAR_SLOPE
    } else {
        ((encoded + SRGB_OFFSET) / (1.0 + SRGB_OFFSET)).powf(SRGB_EXPONENT)
    }
}

fn encode_srgb(linear: f64) -> f64 {
    if linear <= SRGB_LINEAR_BREAKPOINT {
        linear * SRGB_LINEAR_SLOPE
    } else {
        (1.0 + SRGB_OFFSET) * linear.powf(SRGB_EXPONENT.recip()) - SRGB_OFFSET
    }
}

fn in_gamut(linear: [f64; 3]) -> bool {
    linear
        .iter()
        .all(|channel| (-GAMUT_TOLERANCE..=1.0 + GAMUT_TOLERANCE).contains(channel))
}

fn multiply(matrix: &[[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_srgb_color_survives_an_oklch_round_trip() {
        for r in (0..=255u8).step_by(15) {
            for g in (0..=255u8).step_by(15) {
                for b in (0..=255u8).step_by(15) {
                    let color = Rgb::new(r, g, b);
                    let round_trip = Oklab::from_rgb(color).to_lch().to_rgb_in_gamut();
                    for (original, restored) in
                        [(r, round_trip.r), (g, round_trip.g), (b, round_trip.b)]
                    {
                        assert!(
                            original.abs_diff(restored) <= 1,
                            "{color:?} became {round_trip:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn oklab_matches_the_reference_white_black_and_primaries() {
        let white = Oklab::from_rgb(Rgb::new(255, 255, 255));
        assert!((white.l - 1.0).abs() < 1e-6 && white.a.abs() < 1e-6 && white.b.abs() < 1e-6);
        assert!(Oklab::from_rgb(Rgb::new(0, 0, 0)).l.abs() < 1e-9);
        // Reference OKLab values of the sRGB primaries from the OKLab definition.
        for (color, expected) in [
            (Rgb::new(255, 0, 0), [0.627_955, 0.224_863, 0.125_846]),
            (Rgb::new(0, 255, 0), [0.866_440, -0.233_888, 0.179_498]),
            (Rgb::new(0, 0, 255), [0.452_014, -0.032_457, -0.311_528]),
        ] {
            let lab = Oklab::from_rgb(color);
            for (actual, expected) in [lab.l, lab.a, lab.b].into_iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-4, "{color:?}: {lab:?}");
            }
        }
    }

    #[test]
    fn out_of_gamut_requests_keep_hue_and_lightness_while_losing_chroma() {
        let requested = Oklch {
            l: 0.7,
            c: 0.5,
            h: 2.0,
        };
        let mapped = Oklab::from_rgb(requested.to_rgb_in_gamut()).to_lch();
        assert!((mapped.l - requested.l).abs() < 0.01);
        assert!((mapped.h - requested.h).abs() < 0.05);
        assert!(mapped.c < requested.c);
        let gray = Oklch {
            l: 0.5,
            c: 0.0,
            h: 0.0,
        }
        .to_rgb_in_gamut();
        assert!(gray.r == gray.g && gray.g == gray.b);
    }

    #[test]
    fn wcag_luminance_and_contrast_match_reference_values() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert_eq!(black.relative_luminance(), 0.0);
        assert!((white.relative_luminance() - 1.0).abs() < 1e-12);
        assert!((Rgb::new(128, 128, 128).relative_luminance() - 0.215_861).abs() < 1e-5);
        assert!((black.contrast_ratio(white) - 21.0).abs() < 1e-9);
        assert_eq!(black.contrast_ratio(white), white.contrast_ratio(black));
        assert_eq!(white.contrast_ratio(white), 1.0);
    }
}
