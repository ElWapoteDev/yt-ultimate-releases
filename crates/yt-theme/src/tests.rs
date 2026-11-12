use super::*;
use crate::{
    color::{Oklab, Oklch},
    palette::{
        AMBIENT_DISTINCT_DISTANCE, AMBIENT_LIGHTNESS_RANGE, AMBIENT_MAX_CHROMA,
        HOVER_LIGHTNESS_STEP,
    },
};

/// Edge, in pixels, of the thumbnails `yt-core` samples from artwork.
const SAMPLE_EDGE: usize = 64;
/// Numerical Recipes LCG constants, for reproducible pseudo-random artwork.
const LCG_MULTIPLIER: u32 = 1_664_525;
const LCG_INCREMENT: u32 = 1_013_904_223;
const LCG_SEED: u32 = 0x2545_f491;
/// Largest hue drift (degrees) tolerated between artwork and derived colors.
const HUE_TOLERANCE_DEGREES: f64 = 8.0;
/// Chroma below which a derived color is considered gray in assertions.
const GRAY_CHROMA: f64 = 0.02;

/// A `SAMPLE_EDGE`×`SAMPLE_EDGE` thumbnail drawn by `pixel(x, y)`.
fn thumbnail(pixel: impl Fn(usize, usize) -> [u8; 3]) -> Vec<Rgb> {
    (0..SAMPLE_EDGE * SAMPLE_EDGE)
        .map(|index| Rgb::from(pixel(index % SAMPLE_EDGE, index / SAMPLE_EDGE)))
        .collect()
}

fn solid(color: [u8; 3]) -> Vec<Rgb> {
    thumbnail(|_, _| color)
}

fn noise(count: usize) -> Vec<[u8; 3]> {
    let mut state = LCG_SEED;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(LCG_MULTIPLIER)
                .wrapping_add(LCG_INCREMENT);
            let [_, r, g, b] = state.to_le_bytes();
            [r, g, b]
        })
        .collect()
}

fn hue(color: Rgb) -> f64 {
    Oklab::from_rgb(color)
        .to_lch()
        .h
        .to_degrees()
        .rem_euclid(360.0)
}

fn hue_gap(first: Rgb, second: Rgb) -> f64 {
    let gap = (hue(first) - hue(second)).rem_euclid(360.0);
    gap.min(360.0 - gap)
}

fn lightness(color: Rgb) -> f64 {
    Oklab::from_rgb(color).l
}

fn chroma(color: Rgb) -> f64 {
    Oklab::from_rgb(color).to_lch().c
}

fn is_gray(color: Rgb) -> bool {
    let channels = [color.r, color.g, color.b];
    channels.iter().max().unwrap() - channels.iter().min().unwrap() <= 1
}

fn assert_guarantees(palette: &Palette, options: &PaletteOptions, context: &str) {
    let minimum = options.min_text_contrast;
    let background = options.background;
    let Palette {
        accent,
        accent_hover,
        on_accent,
        ambient,
        neutral,
    } = palette.clone();
    for (name, color) in [("accent", accent), ("accent_hover", accent_hover)] {
        assert!(
            color.contrast_ratio(background) >= minimum,
            "{context}: {name} {} has {:.3}:1 against {}",
            color.to_hex(),
            color.contrast_ratio(background),
            background.to_hex()
        );
        assert!(
            on_accent.contrast_ratio(color) >= minimum,
            "{context}: on_accent {} has {:.3}:1 on {name} {}",
            on_accent.to_hex(),
            on_accent.contrast_ratio(color),
            color.to_hex()
        );
    }
    assert!(
        [ON_ACCENT_LIGHT, ON_ACCENT_DARK].contains(&on_accent),
        "{context}"
    );
    assert!(
        (lightness(accent_hover) - lightness(accent)).abs() >= HOVER_LIGHTNESS_STEP / 2.0,
        "{context}: hover {} is not perceptibly different from {}",
        accent_hover.to_hex(),
        accent.to_hex()
    );
    for color in ambient {
        assert!(
            AMBIENT_TEXT.contrast_ratio(color) >= minimum,
            "{context}: white has {:.3}:1 on ambient {}",
            AMBIENT_TEXT.contrast_ratio(color),
            color.to_hex()
        );
        assert!(
            chroma(color) <= AMBIENT_MAX_CHROMA + GRAY_CHROMA,
            "{context}"
        );
    }
    if neutral {
        for color in [accent, accent_hover].into_iter().chain(ambient) {
            assert!(
                is_gray(color),
                "{context}: neutral color {} has a hue",
                color.to_hex()
            );
        }
    }
}

#[test]
fn hex_notation_is_lowercase_and_zero_padded() {
    assert_eq!(Rgb::new(0x0a, 0xbc, 0xff).to_hex(), "#0abcff");
    assert_eq!(Rgb::from([0, 0, 0]).to_hex(), "#000000");
    assert_eq!(Rgb::from([1, 2, 3]), Rgb::new(1, 2, 3));
}

#[test]
fn black_on_white_is_the_maximum_contrast() {
    let (black, white) = (Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
    assert!((black.contrast_ratio(white) - 21.0).abs() < 1e-9);
    assert!((white.contrast_ratio(black) - 21.0).abs() < 1e-9);
}

#[test]
fn solid_artwork_keeps_its_hue_family() {
    let options = PaletteOptions::default();
    for (name, color) in [
        ("red", [220, 30, 40]),
        ("orange", [240, 130, 20]),
        ("yellow", [235, 210, 30]),
        ("green", [40, 170, 70]),
        ("teal", [20, 150, 150]),
        ("blue", [30, 60, 220]),
        ("purple", [130, 50, 200]),
        ("pink", [230, 60, 150]),
    ] {
        let palette = from_pixels(&solid(color), &options).unwrap();
        assert!(!palette.neutral, "{name}");
        for derived in [palette.accent, palette.accent_hover, palette.ambient[0]] {
            assert!(
                hue_gap(derived, Rgb::from(color)) < HUE_TOLERANCE_DEGREES,
                "{name}: {} drifted {:.1}° from the artwork",
                derived.to_hex(),
                hue_gap(derived, Rgb::from(color))
            );
        }
        assert_guarantees(&palette, &options, name);
    }
}

#[test]
fn a_vivid_minority_outranks_a_dull_dominant_background() {
    let options = PaletteOptions::default();
    let (gray, red) = ([120, 120, 124], [230, 20, 40]);
    let pixels = thumbnail(|x, _| if x * 10 < SAMPLE_EDGE * 9 { gray } else { red });
    let palette = from_pixels(&pixels, &options).unwrap();
    assert!(!palette.neutral);
    assert!(hue_gap(palette.accent, Rgb::from(red)) < HUE_TOLERANCE_DEGREES);
    assert!(
        chroma(palette.ambient[0]) < GRAY_CHROMA,
        "the dominant gray comes first"
    );
    assert!(hue_gap(palette.ambient[1], Rgb::from(red)) < HUE_TOLERANCE_DEGREES);
    assert_guarantees(&palette, &options, "90/10 split");
}

#[test]
fn specks_never_become_the_accent() {
    let options = PaletteOptions::default();
    let (blue, green) = ([30, 60, 200], [20, 230, 60]);
    // One vivid pixel in a thumbnail is well below the accent share threshold.
    let pixels = thumbnail(|x, y| if (x, y) == (0, 0) { green } else { blue });
    let palette = from_pixels(&pixels, &options).unwrap();
    assert!(hue_gap(palette.accent, Rgb::from(blue)) < HUE_TOLERANCE_DEGREES);
}

#[test]
fn grayscale_artwork_is_neutral_and_hue_less() {
    let options = PaletteOptions::default();
    let pixels = thumbnail(|x, y| {
        let value = ((x + y) * 2) as u8;
        [value, value, value]
    });
    let palette = from_pixels(&pixels, &options).unwrap();
    assert!(palette.neutral);
    assert_guarantees(&palette, &options, "gradient");
    // A faint tint, as compression leaves on gray photos, is still neutral.
    let tinted = from_pixels(&solid([118, 120, 123]), &options).unwrap();
    assert!(tinted.neutral);
}

#[test]
fn empty_input_is_rejected() {
    assert_eq!(
        from_pixels(&[], &PaletteOptions::default()),
        Err(ThemeError::NoOpaquePixels)
    );
}

#[test]
fn invalid_options_are_rejected() {
    let pixels = solid([10, 200, 30]);
    for min_text_contrast in [f64::NAN, f64::INFINITY, 0.5, 21.5] {
        let options = PaletteOptions {
            min_text_contrast,
            ..PaletteOptions::default()
        };
        assert!(matches!(
            from_pixels(&pixels, &options),
            Err(ThemeError::InvalidOptions(_))
        ));
    }
}

#[test]
fn contrast_guarantees_hold_across_hue_lightness_and_chroma() {
    let backgrounds = [
        DEFAULT_BACKGROUND,
        Rgb::new(0x2c, 0x2c, 0x32),
        Rgb::new(0xf2, 0xf2, 0xf7),
        Rgb::new(0xff, 0xff, 0xff),
    ];
    for background in backgrounds {
        let options = PaletteOptions {
            background,
            ..PaletteOptions::default()
        };
        for hue in (0..360).step_by(15) {
            for lightness in [0.02, 0.1, 0.2, 0.35, 0.5, 0.65, 0.8, 0.9, 0.97] {
                for chroma in [0.0, 0.02, 0.05, 0.1, 0.18, 0.3] {
                    let color = Oklch {
                        l: lightness,
                        c: chroma,
                        h: f64::from(hue).to_radians(),
                    }
                    .to_rgb_in_gamut();
                    let context = format!(
                        "{} on {} (hue {hue}, L {lightness}, C {chroma})",
                        color.to_hex(),
                        background.to_hex()
                    );
                    let palette = from_pixels(&[color; 32], &options).unwrap();
                    assert_guarantees(&palette, &options, &context);
                    let complement = Oklch {
                        l: 1.0 - lightness,
                        c: AMBIENT_MAX_CHROMA,
                        h: f64::from(hue + 150).to_radians(),
                    }
                    .to_rgb_in_gamut();
                    let mut mixed = vec![color; 24];
                    mixed.extend([complement; 8]);
                    let palette = from_pixels(&mixed, &options).unwrap();
                    assert_guarantees(
                        &palette,
                        &options,
                        &format!("{context} + {}", complement.to_hex()),
                    );
                }
            }
        }
    }
}

#[test]
fn stricter_contrast_targets_are_honored() {
    let options = PaletteOptions {
        min_text_contrast: 7.0,
        ..PaletteOptions::default()
    };
    for color in [[30, 60, 220], [220, 30, 40], [40, 170, 70], [128, 128, 128]] {
        let palette = from_pixels(&solid(color), &options).unwrap();
        assert_guarantees(&palette, &options, &Rgb::from(color).to_hex());
    }
}

#[test]
fn ambient_colors_are_dark_distinct_and_dominant_first() {
    let options = PaletteOptions::default();
    let (blue, orange, green) = ([30, 60, 200], [240, 130, 20], [40, 170, 70]);
    let mut pixels = vec![Rgb::from(blue); 60];
    pixels.extend([Rgb::from(orange); 25]);
    pixels.extend([Rgb::from(green); 15]);
    let palette = from_pixels(&pixels, &options).unwrap();
    assert!(hue_gap(palette.ambient[0], Rgb::from(blue)) < HUE_TOLERANCE_DEGREES);
    assert!(hue_gap(palette.ambient[1], Rgb::from(orange)) < HUE_TOLERANCE_DEGREES);
    assert!(hue_gap(palette.ambient[2], Rgb::from(green)) < HUE_TOLERANCE_DEGREES);
    let derived = from_pixels(&solid(blue), &options).unwrap();
    for (index, first) in derived.ambient.iter().enumerate() {
        assert!(lightness(*first) <= *AMBIENT_LIGHTNESS_RANGE.end() + GRAY_CHROMA);
        for second in &derived.ambient[index + 1..] {
            let distance = Oklab::from_rgb(*first).distance(Oklab::from_rgb(*second));
            assert!(
                distance >= AMBIENT_DISTINCT_DISTANCE,
                "derived variants collapsed"
            );
        }
    }
}

#[test]
fn palettes_are_deterministic() {
    let options = PaletteOptions::default();
    let pixels: Vec<Rgb> = noise(SAMPLE_EDGE * SAMPLE_EDGE)
        .into_iter()
        .map(Rgb::from)
        .collect();
    let first = from_pixels(&pixels, &options).unwrap();
    assert_eq!(first, from_pixels(&pixels, &options).unwrap());
    assert_guarantees(&first, &options, "noise");
}

#[test]
#[ignore = "timing measurement: cargo test -p yt-theme --release -- --ignored --nocapture"]
fn palette_timing() {
    const RUNS: u32 = 200;
    const LARGE_EDGE: usize = 1200;
    let options = PaletteOptions::default();
    let texture = noise(SAMPLE_EDGE * SAMPLE_EDGE);
    let artwork = |edge: usize| -> Vec<Rgb> {
        (0..edge * edge)
            .map(|index| {
                let (x, y) = (index % edge, index / edge);
                let grain = texture[(y % SAMPLE_EDGE) * SAMPLE_EDGE + x % SAMPLE_EDGE];
                Rgb::new(
                    ((x * 200 / edge) as u8).saturating_add(grain[0] / 8),
                    ((y * 160 / edge) as u8).saturating_add(grain[1] / 8),
                    (((x + y) * 120 / edge) as u8).saturating_add(grain[2] / 8),
                )
            })
            .collect()
    };
    for (label, pixels, runs) in [
        ("64x64 thumbnail", artwork(SAMPLE_EDGE), RUNS),
        ("1200x1200 full image", artwork(LARGE_EDGE), RUNS / 20),
    ] {
        let started = std::time::Instant::now();
        for _ in 0..runs {
            from_pixels(&pixels, &options).unwrap();
        }
        let per_run = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        println!("{label}: {per_run:.3} ms per palette");
    }
}
