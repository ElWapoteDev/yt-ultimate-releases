//! Colors for generated covers of made-for-you collections and stations,
//! which have no artwork of their own. Derived only from a seed, so a
//! collection keeps its look across sessions.

use crate::color::{Oklch, Rgb};
use std::f64::consts::TAU;

/// Lightness, chroma and hue offset (radians) of the three gradient stops.
/// Lightness stays low enough for white titles to keep large-text contrast.
const STOPS: [(f64, f64, f64); 3] = [(0.6, 0.19, 0.0), (0.68, 0.16, 0.6), (0.46, 0.15, -0.5)];

/// SplitMix64 finalizer: spreads nearby seeds over the whole hue circle.
fn mix(seed: u64) -> u64 {
    let mut value = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Three harmonious gradient colors (main, light, deep) for a generated
/// cover, chosen from `seed` alone.
pub fn generated_palette(seed: u64) -> [Rgb; 3] {
    let hue = (mix(seed) >> 11) as f64 / (1u64 << 53) as f64 * TAU;
    STOPS.map(|(l, c, offset)| {
        Oklch {
            l,
            c,
            h: hue + offset,
        }
        .to_rgb_in_gamut()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG minimum for large text.
    const LARGE_TEXT_CONTRAST: f64 = 3.0;

    #[test]
    fn generated_palettes_are_stable_varied_and_keep_white_titles_legible() {
        assert_eq!(generated_palette(7), generated_palette(7));
        let distinct = (0..64u64)
            .map(|seed| generated_palette(seed)[0])
            .collect::<std::collections::HashSet<_>>();
        assert!(
            distinct.len() > 48,
            "only {} distinct main colors",
            distinct.len()
        );
        let white = Rgb::new(255, 255, 255);
        for seed in 0..512u64 {
            for stop in [generated_palette(seed)[0], generated_palette(seed)[2]] {
                assert!(
                    white.contrast_ratio(stop) >= LARGE_TEXT_CONTRAST,
                    "seed {seed}: {}",
                    stop.to_hex()
                );
            }
        }
    }
}
