# Theme

The interface takes its accent and ambient glow from the cover of the media that is playing, so the whole
application follows the song while staying legible. With nothing playing, without a cover, or with dynamic color
turned off, the default accent (#fc3c44) applies.

## Files

- `crates/yt-theme/`: pure color math (OKLab, swatches, WCAG fitting). Its `README.md` owns the algorithm, the
  guarantees and the tests.
- `crates/yt-core/src/artwork.rs`: `artwork_samples(bytes, side)`, the bounded decoder shared with `validate_artwork`.
  It downsamples a cover to at most `side`×`side` opaque RGB samples.
- `crates/yt-app/src/theme.rs`: the controller. `ArtworkTheme` resolves the playing track's cover, computes or
  reuses its palette and publishes CSS custom properties. `Palettes` (held by `AppState::palettes`) is the palette
  cache shared with `use_artwork_tokens`, which colors one surface (a detail hero) after its own artwork.
- `crates/yt-app/assets/app.css`: the design tokens in `:root`, their registered ambient properties and the
  `.app-ambient` layer.
- `crates/yt-app/src/app.rs`: `remote_artwork`, the rule that says whether a track shows a remote cover (also used by
  `artwork_src`), and the `.app-ambient` element inside `main.main-wrap`.
- `crates/yt-app/src/album_art.rs`: `AlbumArt::image`, which picks the image a song's thumbnails show (its album cover
  when known, see `docs/systems/Artwork.md`); the theme samples that same image.
- `crates/yt-app/src/now_playing_ui.css`: the full-screen aurora, whose blobs paint with the ambient colors.
- `crates/yt-app/src/settings_ui.rs`: `AppearanceSection`, the «Color dinámico» switch. It calls
  `AppState::set_dynamic_color` (in `theme.rs`), which saves `settings.appearance.dynamic_color` (owned by `yt-core`).

## Tokens

`theme.rs` writes one `<style>` element:

| Token | Scope | Meaning |
|---|---|---|
| `--accent` | `:root` | Characteristic cover color, legible on `--bg-canvas` |
| `--accent-hover` | `:root` | Hover and pressed variant of the accent |
| `--on-accent` | `:root` | Text and icons drawn on an accent fill |
| `--tint` | `:root` | Dominant ambient color for chrome tints (player bar); `transparent` for greyscale covers |
| `--ambient`, `--ambient-2`, `--ambient-3` | `.app-ambient`, `.np-aurora__blob` | The glow colors behind the pages and the full-screen aurora |

`use_artwork_tokens(src)` returns inline declarations for one surface instead: `--cover-1` and the accent tokens
from the artwork's accent, `--cover-2` and `--cover-3` from its first two ambient colors. They override the playing
theme inside that element only, and stay empty for greyscale or unreadable artwork and with dynamic color off.

`--accent-subtle` and the default `--accent-hover` derive from `--accent` with `color-mix` in `app.css`, so features
never write a literal accent. `--bg-canvas` must equal `yt_theme::DEFAULT_BACKGROUND` (#18181a): accents are fitted to
it.

## Lifecycle

1. `ArtworkTheme` (mounted once at the root) runs a `use_resource` that reads `state.current` and the memoized
   `settings.appearance.dynamic_color`. A newer track drops the pending work of the previous one.
2. The cover is the image the song's thumbnails show (`theme_src`): its album cover at 128 px when `AlbumArt` knows
   one, else `remote_artwork(track)` (an allowlisted `https` thumbnail) or `AppCore::artwork_path` (a confined local
   file), resolved off the UI thread. Videos keep their thumbnail. A new lookup or a replaced library cover re-runs the
   resource.
3. A palette cached for that cover in the last 6 hours is reused. Otherwise the bytes are fetched
   (`YoutubeClient::fetch_thumbnail`) or read, then decoded with `artwork_samples(…, 64)` and passed to
   `yt_theme::from_pixels` inside `spawn_blocking`.
4. The previous palette stays applied until the new one is ready, so the default accent never flashes between tracks.
   Greyscale covers keep their neutral accent but publish a transparent glow.

## Invariants

- Rust decides every color; the WebView receives only `#rrggbb` hex values or `transparent`.
- The cache holds at most 128 covers and evicts the oldest entry when full; entries expire after 6 hours. Failures
  are not cached, so a cover that failed to load is retried the next time the track plays.
- `--ambient`, `--ambient-2` and `--ambient-3` are registered with `inherits: false`, so their 900 ms transition
  restyles only the elements that paint with them (`AMBIENT_SCOPES` in `theme.rs`) and never the page tree.

## Trust boundary

Remote covers come only through `fetch_thumbnail`: YouTube image hosts, including every redirect, a 16 MiB cap and
cancellation. Local covers come from `artwork_path`, which canonicalizes and confines the file. Decoding is bounded by
`yt-core` (format allowlist, 8192×8192 pixels, 128 MiB allocation).

## Performance

- Fetching, file reads, decoding and palette math never run on the UI thread. A 64×64 sample costs about 0.1 ms of
  palette math (see the `yt-theme` README); decoding dominates and depends on the cover size.
- The resource re-runs only when the playing track, its cover or the dynamic-color preference changes; a cached cover
  costs one hash-map lookup. Album covers are sampled from a 128 px copy, so a palette never downloads the original.
  `use_artwork_tokens` re-runs only when its image source or the preference changes, and shares the same bounded
  cache, so opening an album whose cover is playing costs no new decode.
- The glow animates registered custom properties on the elements of `AMBIENT_SCOPES` only (the page glow and, while
  the full-screen view is open, its four aurora blobs); reduced motion disables the transition of the page glow.

## Tests

- `theme::tests`: the CSS carries every token for every ambient scope, greyscale covers hide the glow, surfaces take the
  accent for glow and buttons, songs take their colors from the cover their thumbnails show (videos from their frame),
  and the cache evicts its oldest cover.
- `yt-core` `artwork::tests`: sampling bounds, color preservation, transparent pixels and invalid bytes.
- `yt-theme`: color science, contrast guarantees and determinism (see its README).
