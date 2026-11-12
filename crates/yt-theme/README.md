# yt-theme

Pure color math that turns artwork pixels into an accessible interface
palette, so the application can theme itself with the current song's main
color. The crate performs no file, network, image-format or UI work and has no
dependency beyond `thiserror`. Equal input always produces an equal palette.

Decoding happens before this crate: `yt_core::artwork_samples(bytes, side)`
decodes cover art within its own size and dimension limits, downsamples it and
drops mostly transparent pixels. The resulting `[r, g, b]` samples convert with
`Rgb::from` and go to `from_pixels`.

## API

- `from_pixels(&[Rgb], &PaletteOptions) -> Result<Palette, ThemeError>`: the
  only entry point. Pixels may come in any order; a few thousand samples (a
  64×64 thumbnail) are plenty.
- `PaletteOptions { background, min_text_contrast }`: the surface the accent is
  read against (default `DEFAULT_BACKGROUND`, the app's `--bg-base` #18181a)
  and the WCAG 2.x contrast target, `1.0..=21.0` (default
  `DEFAULT_MIN_TEXT_CONTRAST`, 4.5, AA body text).
- `Palette { accent, accent_hover, on_accent, ambient: [Rgb; AMBIENT_COUNT],
  neutral }`.
- `Rgb { r, g, b }` with `new`, `From<[u8; 3]>`, `to_hex` (`#rrggbb`,
  lowercase), `relative_luminance` and `contrast_ratio` (WCAG 2.x).
- `ThemeError::NoOpaquePixels` for empty input and
  `ThemeError::InvalidOptions` for a contrast target outside `1.0..=21.0` or
  not finite.
- Text colors used by the guarantees: `ON_ACCENT_LIGHT` (#ffffff),
  `ON_ACCENT_DARK` (#101012) and `AMBIENT_TEXT` (#ffffff).

## Algorithm

1. Pixels are bucketed in a 4-bit-per-channel sRGB histogram (4,096 buckets).
   Bucket means are converted to OKLab and merged greedily, most populated
   first, while they are within 0.08 ΔEOK (about four just-noticeable
   differences). Gradients and compression noise collapse into the colors a
   person would name.
2. Swatches with at least 2 % of the pixels compete for the accent (all
   swatches compete when none reaches it). If every competitor has OKLCH chroma
   below 0.03 the artwork is neutral. Otherwise the score is
   `share^0.5 × vividness × lightness suitability`: vividness saturates at
   chroma 0.12 and suitability is a lightness window that penalizes
   near-black and near-white colors without ever reaching zero. A vivid
   minority therefore beats a dull dominant background, while specks never win.
3. Contrast fitting keeps the OKLCH hue, reduces chroma only to stay in sRGB,
   and bisects lightness toward the direction where contrast with the
   background grows (lighter on dark backgrounds, darker on light ones). The
   predicate is evaluated on the final 8-bit color, so every returned color
   really meets it.
4. `accent_hover` moves 0.06 OKLab lightness further from the background, or
   the other way when the accent is already at the lightness ceiling, keeping
   all contrast pairings. `on_accent` is whichever text color contrasts more.
5. Ambient colors take the most populated swatches in order, clamp them to
   OKLab lightness 0.18–0.42 and chroma ≤ 0.12, darken them if white text would
   fall below the target, and skip colors closer than 0.06 ΔEOK to one already
   chosen. Missing slots are derived from the dominant color by lightness
   steps of 0.08.

Every threshold is a named constant documented in `src/palette.rs` or
`src/quantize.rs`.

## Guarantees

With a reachable target (any target with the default background):

- `accent` and `accent_hover` reach `min_text_contrast` against `background`.
- `on_accent` reaches it against both `accent` and `accent_hover`.
- `AMBIENT_TEXT` reaches it against every ambient color.
- `accent_hover` differs from `accent` by a visible lightness step.
- Neutral artwork yields hue-less grays; chromatic artwork keeps its hue.

## Limits

- An unreachable target (for example 21:1 against a mid gray) returns the
  extreme color in the fitting direction instead of failing.
- `from_pixels` trusts that its samples are opaque; transparency handling and
  decoding bounds belong to the caller (`yt-core`).
- The accent stays faithful to the artwork: muted art gives a muted accent and
  pale art a pale one. Chroma is never boosted.

## Verification

`cargo test -p yt-theme` checks OKLab reference values and round trips, WCAG
reference luminances and the 21:1 maximum, hue preservation for eight solid
hues, a 90/10 gray/vivid split, specks, grayscale and faintly tinted neutral
art, empty input and invalid options, ambient ordering and distinctness,
determinism, and a sweep over 24 hues × 9 lightnesses × 6 chromas (solid and
mixed with a complement) against four dark and light backgrounds plus a 7:1
target. A release timing test is ignored by default:

```sh
cargo test -p yt-theme --release -- --ignored --nocapture
```

On an Apple Silicon Mac it measured 0.09 ms per palette for a 64×64 sample and
1.7 ms for 1200×1200 pixels.
