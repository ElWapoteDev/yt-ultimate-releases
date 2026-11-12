//! Artwork theme controller: the accent and ambient glow follow the cover of
//! the media that is playing. Colour math lives in `yt-theme`; this module
//! decodes off the UI thread, caches palettes and publishes CSS custom
//! properties, the only thing the WebView receives.
use crate::state::AppState;
use dioxus::prelude::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};
use yt_core::Track;
use yt_theme::{Palette, PaletteOptions, Rgb};

/// Side, in pixels, of the box-filtered cover sample analysed per track.
const SAMPLE_SIDE: u32 = 64;
/// Pixels of a resizable cover downloaded for sampling: twice the sample.
const SOURCE_EDGE: u32 = 2 * SAMPLE_SIDE;
/// Elements that paint with the ambient colors. The properties are registered
/// without inheritance, so each element that paints with them receives them.
const AMBIENT_SCOPES: &str = ".app-ambient,.np-aurora__blob";
/// Palettes remembered per cover.
const CACHE_LIMIT: usize = 128;
/// A remote cover can be replaced by its owner; recompute after this long.
const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// The cover a track shows. Local covers are keyed by file, so replacing a
/// cover yields a new palette.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Cover {
    File(PathBuf),
    Remote(String),
}

type Cache = RefCell<HashMap<Cover, (Instant, Palette)>>;

/// Palettes remembered per cover, shared by the playing theme and every
/// surface colored by its artwork.
#[derive(Clone, Default)]
pub(crate) struct Palettes(Rc<Cache>);

/// CSS custom properties for a palette. Greyscale covers keep the neutral
/// accent but no glow, so the canvas never turns a muddy grey.
fn css(palette: &Palette) -> String {
    let glow = |index: usize| {
        palette
            .ambient
            .get(index)
            .filter(|_| !palette.neutral)
            .map(|colour| colour.to_hex())
            .unwrap_or_else(|| "transparent".into())
    };
    let (first, second, third) = (glow(0), glow(1), glow(2));
    format!(
        ":root{{--accent:{};--accent-hover:{};--on-accent:{};--tint:{first};}}\
         {AMBIENT_SCOPES}{{--ambient:{first};--ambient-2:{second};--ambient-3:{third};}}",
        palette.accent.to_hex(),
        palette.accent_hover.to_hex(),
        palette.on_accent.to_hex(),
    )
}

/// The image a song's colors come from: the cover its thumbnails show
/// (`crate::album_art`), at sampling size. Reading subscribes the caller.
fn theme_src(state: &AppState, track: &Track) -> String {
    let src = crate::app::artwork_src(track);
    if track.media_type == "video" {
        return src;
    }
    state.album_art.image(&src, Some(SOURCE_EDGE))
}

async fn library_cover(state: &AppState, id: String) -> Option<Cover> {
    let core = state.services.core.clone();
    tokio::task::spawn_blocking(move || core.artwork_path(&id).ok().flatten())
        .await
        .ok()
        .flatten()
        .map(Cover::File)
}

/// The cover behind an image source made by `crate::app::artwork_src`: a
/// remote `https` image or the library route `/artwork/<track id>`, with the
/// revision `crate::album_art` adds to a replaced cover.
async fn cover_of_src(state: &AppState, src: &str) -> Option<Cover> {
    if src.starts_with("https://") {
        return Some(Cover::Remote(src.to_owned()));
    }
    let route = src.strip_prefix("/artwork/")?;
    let id = route.split_once('?').map_or(route, |(id, _)| id);
    library_cover(state, id.to_owned()).await
}

fn palette_from_bytes(bytes: &[u8]) -> Option<Palette> {
    let pixels = yt_core::artwork_samples(bytes, SAMPLE_SIDE)
        .ok()?
        .into_iter()
        .map(Rgb::from)
        .collect::<Vec<_>>();
    yt_theme::from_pixels(&pixels, &PaletteOptions::default()).ok()
}

async fn compute(state: &AppState, cover: &Cover) -> Option<Palette> {
    match cover {
        Cover::Remote(url) => {
            let bytes = state
                .services
                .youtube_client()
                .ok()?
                .fetch_thumbnail(url, yt_media::CancellationToken::new())
                .await
                .ok()?;
            tokio::task::spawn_blocking(move || palette_from_bytes(&bytes))
                .await
                .ok()?
        }
        Cover::File(path) => {
            let path = path.clone();
            tokio::task::spawn_blocking(move || palette_from_bytes(&std::fs::read(path).ok()?))
                .await
                .ok()?
        }
    }
}

fn remember(cache: &Cache, cover: Cover, palette: Palette, now: Instant) {
    let mut entries = cache.borrow_mut();
    if entries.len() >= CACHE_LIMIT && !entries.contains_key(&cover) {
        let oldest = entries
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            entries.remove(&oldest);
        }
    }
    entries.insert(cover, (now, palette));
}

async fn resolve(state: &AppState, cache: &Cache, cover: Cover) -> Option<Palette> {
    let fresh = cache
        .borrow()
        .get(&cover)
        .filter(|(at, _)| at.elapsed() < CACHE_TTL)
        .map(|(_, palette)| palette.clone());
    if fresh.is_some() {
        return fresh;
    }
    let palette = compute(state, &cover).await?;
    remember(cache, cover, palette.clone(), Instant::now());
    Some(palette)
}

impl AppState {
    /// Turn artwork-derived colours on or off; off restores the default accent.
    pub fn set_dynamic_color(&self, enabled: bool) {
        let mut settings = self.settings.peek().clone();
        settings.appearance.dynamic_color = enabled;
        self.save_settings(settings);
    }
}

/// Publishes the playing cover's palette as CSS custom properties, unless the
/// user turned dynamic colour off. The previous palette stays until the next
/// one is ready, so changing tracks never flashes the default accent; a newer
/// track drops the pending work of the previous one.
#[component]
pub fn ArtworkTheme() -> Element {
    let state = use_context::<AppState>();
    let settings = state.settings;
    let current = state.current;
    let dynamic = use_memo(move || settings.read().appearance.dynamic_color);
    let mut applied = use_signal(|| None::<Palette>);
    use_resource(move || {
        let state = state.clone();
        async move {
            let enabled = dynamic();
            let src = current
                .read()
                .as_ref()
                .filter(|_| enabled)
                .map(|track| theme_src(&state, track));
            let palette = match src {
                Some(src) => match cover_of_src(&state, &src).await {
                    Some(cover) => resolve(&state, &state.palettes.0, cover).await,
                    None => None,
                },
                None => None,
            };
            if *applied.peek() != palette {
                applied.set(palette);
            }
        }
    });
    let css = applied.read().as_ref().map(css);
    rsx! {
        if let Some(css) = css {
            style { "{css}" }
        }
    }
}

/// Inline custom properties that color a surface after the artwork `src`
/// (see [`cover_of_src`]): the cover glow (`--cover-1..3`) and the accent
/// tokens of its buttons. Empty until the palette is ready, when the artwork
/// is greyscale or unreadable, and when the user turned dynamic color off.
pub(crate) fn use_artwork_tokens(src: String) -> String {
    let state = use_context::<AppState>();
    // The surface shows the cover its thumbnails show.
    let src = state.album_art.image(&src, Some(SOURCE_EDGE));
    let settings = state.settings;
    let dynamic = use_memo(move || settings.read().appearance.dynamic_color);
    let palette = use_resource(use_reactive((&src,), move |(src,)| {
        let state = state.clone();
        async move {
            if src.is_empty() || !dynamic() {
                return None;
            }
            let cover = cover_of_src(&state, &src).await?;
            resolve(&state, &state.palettes.0, cover).await
        }
    }));
    let tokens = palette
        .read()
        .as_ref()
        .and_then(Option::as_ref)
        .filter(|palette| !palette.neutral)
        .map(surface_tokens)
        .unwrap_or_default();
    tokens
}

/// Custom properties of a colored surface: the accent drives the glow and the
/// buttons, the ambient colors deepen the glow.
fn surface_tokens(palette: &Palette) -> String {
    format!(
        "--cover-1:{};--cover-2:{};--cover-3:{};--accent:{};--accent-hover:{};--on-accent:{}",
        palette.accent.to_hex(),
        palette.ambient[0].to_hex(),
        palette.ambient[1].to_hex(),
        palette.accent.to_hex(),
        palette.accent_hover.to_hex(),
        palette.on_accent.to_hex(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(neutral: bool) -> Palette {
        let pixels = if neutral {
            vec![Rgb::new(30, 30, 30), Rgb::new(220, 220, 220)]
        } else {
            vec![Rgb::new(255, 106, 61), Rgb::new(91, 26, 110)]
        };
        yt_theme::from_pixels(&pixels, &PaletteOptions::default()).unwrap()
    }

    #[test]
    fn surfaces_take_the_accent_for_glow_and_buttons() {
        let vivid = palette(false);
        let tokens = surface_tokens(&vivid);
        let accent = vivid.accent.to_hex();
        assert!(tokens.starts_with(&format!("--cover-1:{accent};")));
        assert!(tokens.contains(&format!("--accent:{accent};")));
        assert!(tokens.contains(&format!("--on-accent:{}", vivid.on_accent.to_hex())));
    }

    #[test]
    fn css_publishes_every_token_and_hides_the_glow_of_greyscale_covers() {
        let vivid = palette(false);
        let text = css(&vivid);
        for token in [
            format!("--accent:{}", vivid.accent.to_hex()),
            format!("--accent-hover:{}", vivid.accent_hover.to_hex()),
            format!("--on-accent:{}", vivid.on_accent.to_hex()),
            format!("--ambient:{}", vivid.ambient[0].to_hex()),
            format!("--ambient-2:{}", vivid.ambient[1].to_hex()),
            format!("--ambient-3:{}", vivid.ambient[2].to_hex()),
        ] {
            assert!(text.contains(&token), "{token} missing from {text}");
        }
        // The full-screen aurora paints with the same colors as the page glow.
        assert!(text.contains(".app-ambient,.np-aurora__blob{"));
        let grey = css(&palette(true));
        assert!(grey.contains("--tint:transparent"));
        assert!(
            grey.contains("--ambient:transparent;--ambient-2:transparent;--ambient-3:transparent")
        );
    }

    #[test]
    fn songs_take_their_colors_from_the_cover_their_thumbnails_show() {
        let mut app = crate::test_support::TestApp::new();
        let song = crate::test_support::remote_track("abcdefghijk", "Song");
        let mut thumbnail = song.clone();
        thumbnail.artwork_url = "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg".into();
        let mut video = thumbnail.clone();
        video.media_type = "video".into();
        let cover = "https://yt3.googleusercontent.com/albumCover";
        app.run(|state| {
            assert_eq!(theme_src(state, &song), "/artwork/youtube:abcdefghijk");
            assert_eq!(theme_src(state, &thumbnail), thumbnail.artwork_url);
            state.album_art.remember(yt_core::AlbumArtwork {
                video_id: "abcdefghijk".into(),
                url: Some(cover.into()),
                checked_at: crate::discovery::unix_now(),
            });
            assert_eq!(theme_src(state, &song), format!("{cover}=s{SOURCE_EDGE}"));
            assert_eq!(
                theme_src(state, &thumbnail),
                format!("{cover}=s{SOURCE_EDGE}")
            );
            assert_eq!(theme_src(state, &video), video.artwork_url);
        });
    }

    #[test]
    fn the_cache_evicts_its_oldest_cover_when_full() {
        let cache = Cache::default();
        let start = Instant::now();
        for index in 0..CACHE_LIMIT {
            remember(
                &cache,
                Cover::Remote(format!("https://i.ytimg.com/{index}")),
                palette(false),
                start + Duration::from_millis(index as u64),
            );
        }
        remember(
            &cache,
            Cover::File("/cover.jpg".into()),
            palette(true),
            start + Duration::from_secs(1),
        );
        let entries = cache.borrow();
        assert_eq!(entries.len(), CACHE_LIMIT);
        assert!(!entries.contains_key(&Cover::Remote("https://i.ytimg.com/0".into())));
        assert!(entries.contains_key(&Cover::File("/cover.jpg".into())));
    }
}
