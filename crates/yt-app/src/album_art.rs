//! Album artwork controller: finds the album cover of the songs that play and
//! shows it wherever the song appears instead of its video thumbnail, sized to
//! where it is drawn. Reading the provider and sizing images live in
//! `yt-media`, the remembered lookups in `yt-core`. See
//! `docs/systems/Artwork.md`.

use crate::{discovery::unix_now, state::AppState};
use dioxus::prelude::*;
use std::{collections::HashMap, time::Duration};
use yt_core::{AlbumArtwork, AppCore, Settings, ALBUM_ARTWORK_LIMIT, REMOTE_ID_PREFIX};
use yt_media::{sized_artwork, thumbnail_video_id, CancellationToken, YoutubeClient};

/// A found cover is looked up again after this long, in case it changed.
const FOUND_TTL: Duration = Duration::from_secs(30 * 86_400);
/// A video without a cover is asked about again after this long.
const MISSING_TTL: Duration = Duration::from_secs(7 * 86_400);
/// Pixels requested for covers in rows and the player bar (at most 60 px wide).
const SMALL_EDGE: u32 = 120;
/// Pixels requested for covers in cards and headers (at most 272 px wide).
const CARD_EDGE: u32 = 544;
/// Library covers are served under this application protocol prefix.
const LOCAL_ARTWORK: &str = "/artwork/";
/// Characters of a replaced cover's file name that make its URL new.
const REVISION_CHARS: usize = 12;

/// Remembered album cover lookups, shared by every thumbnail.
#[derive(Clone, Copy)]
pub(crate) struct AlbumArt {
    /// Lookups by video id, as stored in `yt-core`; empty while album covers
    /// are turned off.
    lookups: Signal<HashMap<String, AlbumArtwork>>,
    /// Library tracks whose stored cover was replaced this session, with a
    /// revision that makes the WebView load the new file.
    replaced: Signal<HashMap<String, String>>,
}

/// Restore the remembered lookups from SQLite at startup.
pub(crate) fn use_album_art(core: &AppCore, settings: Signal<Settings>) -> AlbumArt {
    let enabled = settings.peek().appearance.album_artwork;
    AlbumArt {
        lookups: crate::state::use_app_signal(|| {
            if enabled {
                remembered(core)
            } else {
                HashMap::new()
            }
        }),
        replaced: crate::state::use_app_signal(HashMap::new),
    }
}

fn remembered(core: &AppCore) -> HashMap<String, AlbumArtwork> {
    core.album_artworks()
        .unwrap_or_default()
        .into_iter()
        .map(|lookup| (lookup.video_id.clone(), lookup))
        .collect()
}

/// Pixels to request for a `Thumb` of `size`: about twice its largest CSS
/// width, so covers stay sharp on 2× displays, or the original image for the
/// full-screen view.
pub(crate) fn edge_for(size: &str) -> Option<u32> {
    match size {
        "full" => None,
        "xl" | "square" => Some(CARD_EDGE),
        _ => Some(SMALL_EDGE),
    }
}

/// Whether a remembered lookup can be used without asking again.
fn fresh(lookup: &AlbumArtwork, now: f64) -> bool {
    let ttl = if lookup.url.is_some() {
        FOUND_TTL
    } else {
        MISSING_TTL
    };
    now - lookup.checked_at < ttl.as_secs_f64()
}

/// Ask YouTube for the album cover of `video` and remember the answer. `None`
/// when YouTube could not be asked (offline, refused) or the answer does not
/// validate; the caller keeps what it knew.
async fn look_up(
    core: &AppCore,
    youtube: &YoutubeClient,
    video: &str,
    cancel: &CancellationToken,
) -> Option<AlbumArtwork> {
    let url = youtube.album_artwork(video, cancel.clone()).await.ok()?;
    let lookup = AlbumArtwork {
        video_id: video.to_owned(),
        url,
        checked_at: unix_now(),
    };
    let core = core.clone();
    let stored = lookup.clone();
    tokio::task::spawn_blocking(move || {
        core.store_album_artwork(&stored.video_id, stored.url.as_deref())
    })
    .await
    .ok()?
    .ok()?;
    Some(lookup)
}

/// The cover a download of `video` embeds: its album cover at the original
/// size when YouTube names one (remembered, or looked up unless album covers
/// are turned off); `None` keeps the video thumbnail.
pub(crate) async fn download_cover(
    core: &AppCore,
    youtube: &YoutubeClient,
    video: &str,
    cancel: &CancellationToken,
) -> Option<String> {
    if !core.settings().ok()?.appearance.album_artwork {
        return None;
    }
    let known = core.album_artwork(video).ok().flatten();
    let lookup = match known {
        Some(known) if fresh(&known, unix_now()) => known,
        known => look_up(core, youtube, video, cancel).await.or(known)?,
    };
    lookup.url.map(|url| sized_artwork(&url, None))
}

impl AlbumArt {
    /// What a thumbnail drawing `src` loads at `edge` pixels (`None`: the
    /// original). A YouTube video thumbnail becomes its video's album cover
    /// when one is known, covers on resizable hosts are sized, and a library
    /// cover replaced this session gets a revision so the WebView reloads it.
    /// Reading subscribes the caller, so thumbnails follow new lookups.
    pub(crate) fn image(&self, src: &str, edge: Option<u32>) -> String {
        if let Some(id) = src.strip_prefix(LOCAL_ARTWORK) {
            if let Some(cover) = id
                .strip_prefix(REMOTE_ID_PREFIX)
                .and_then(|video| self.cover(video))
            {
                return sized_artwork(&cover, edge);
            }
            return match self.replaced.read().get(id) {
                Some(revision) => format!("{src}?v={revision}"),
                None => src.to_owned(),
            };
        }
        let cover = thumbnail_video_id(src).and_then(|video| self.cover(video));
        sized_artwork(cover.as_deref().unwrap_or(src), edge)
    }

    fn cover(&self, video: &str) -> Option<String> {
        self.lookups
            .read()
            .get(video)
            .and_then(|lookup| lookup.url.clone())
    }

    /// Keep `lookup` in memory, evicting the least recently checked video
    /// beyond `ALBUM_ARTWORK_LIMIT`, as `yt-core` does on disk.
    pub(crate) fn remember(&self, lookup: AlbumArtwork) {
        let mut lookups = self.lookups;
        let mut lookups = lookups.write();
        if lookups.len() >= ALBUM_ARTWORK_LIMIT && !lookups.contains_key(&lookup.video_id) {
            let oldest = lookups
                .values()
                .min_by(|a, b| a.checked_at.total_cmp(&b.checked_at))
                .map(|oldest| oldest.video_id.clone());
            if let Some(oldest) = oldest {
                lookups.remove(&oldest);
            }
        }
        lookups.insert(lookup.video_id.clone(), lookup);
    }

    /// Forget the lookups in memory, e.g. after the listening history (where
    /// they are stored) is erased.
    pub(crate) fn forget(&self) {
        let mut lookups = self.lookups;
        lookups.set(HashMap::new());
    }
}

impl AppState {
    /// Turn album covers on or off. Off stops every lookup and shows video
    /// thumbnails again; covers already saved in the library stay.
    pub fn set_album_artwork(&self, enabled: bool) {
        let mut settings = self.settings.peek().clone();
        settings.appearance.album_artwork = enabled;
        self.save_settings(settings);
        let album_art = self.album_art;
        if !enabled {
            album_art.forget();
            return;
        }
        let core = self.services.core.clone();
        spawn(async move {
            let restored = tokio::task::spawn_blocking(move || remembered(&core))
                .await
                .unwrap_or_default();
            let mut lookups = album_art.lookups;
            lookups.set(restored);
        });
    }

    /// The album cover of `video`, remembered while fresh, else asked to
    /// YouTube; offline keeps what was known.
    async fn find_album_artwork(&self, video: &str) -> Option<String> {
        let known = self.album_art.lookups.peek().get(video).cloned();
        if let Some(known) = known.as_ref().filter(|known| fresh(known, unix_now())) {
            return known.url.clone();
        }
        let youtube = self.services.youtube_client().ok()?;
        match look_up(
            &self.services.core,
            &youtube,
            video,
            &CancellationToken::new(),
        )
        .await
        {
            Some(lookup) => {
                let url = lookup.url.clone();
                self.album_art.remember(lookup);
                url
            }
            None => known.and_then(|known| known.url),
        }
    }

    /// Save `cover`, at its original size, as the stored cover of library
    /// track `track_id` when the current one is missing or a video frame. A
    /// square cover (album art already, or the user's own) is kept.
    async fn replace_library_cover(&self, track_id: &str, cover: &str) {
        if self.album_art.replaced.peek().contains_key(track_id) {
            return;
        }
        let core = self.services.core.clone();
        let id = track_id.to_owned();
        let replaceable = tokio::task::spawn_blocking(move || match core.artwork_path(&id) {
            Ok(Some(path)) => std::fs::read(path)
                .ok()
                .and_then(|bytes| yt_core::is_video_frame(&bytes).ok())
                .unwrap_or(false),
            Ok(None) => true,
            Err(_) => false,
        })
        .await
        .unwrap_or(false);
        if !replaceable {
            return;
        }
        let Ok(youtube) = self.services.youtube_client() else {
            return;
        };
        let Ok(bytes) = youtube
            .fetch_thumbnail(&sized_artwork(cover, None), CancellationToken::new())
            .await
        else {
            return;
        };
        let core = self.services.core.clone();
        let id = track_id.to_owned();
        let Ok(Ok(path)) =
            tokio::task::spawn_blocking(move || core.set_track_artwork(&id, &bytes)).await
        else {
            return;
        };
        let revision = path
            .file_stem()
            .map(|stem| {
                stem.to_string_lossy()
                    .chars()
                    .take(REVISION_CHARS)
                    .collect()
            })
            .unwrap_or_default();
        let mut replaced = self.album_art.replaced;
        replaced.write().insert(track_id.to_owned(), revision);
    }
}

/// Finds the album cover of the song that plays, once per song. Mounted once
/// at the root; a newer song drops the pending lookup of the previous one.
/// Videos keep their own frames.
#[component]
pub fn AlbumCovers() -> Element {
    let state = use_context::<AppState>();
    let settings = state.settings;
    let current = state.current;
    let enabled = use_memo(move || settings.read().appearance.album_artwork);
    // Keyed by identity only, so refreshing the playing track's details (its
    // duration, say) never restarts a lookup in flight.
    let song = use_memo(move || {
        current
            .read()
            .as_ref()
            .filter(|track| track.media_type != "video")
            .and_then(|track| {
                let video = crate::remote::video_id_of(track)?;
                Some((track.id.clone(), video, crate::remote::is_remote(track)))
            })
    });
    use_resource(move || {
        let state = state.clone();
        async move {
            let Some((track_id, video, remote)) = song().filter(|_| enabled()) else {
                return;
            };
            let Some(cover) = state.find_album_artwork(&video).await else {
                return;
            };
            if !remote {
                state.replace_library_cover(&track_id, &cover).await;
            }
        }
    });
    rsx! {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Thumb, test_support::render};

    const VIDEO: &str = "abcdefghijk";
    const COVER: &str = "https://yt3.googleusercontent.com/albumCover";

    fn lookup(url: Option<&str>, checked_at: f64) -> AlbumArtwork {
        AlbumArtwork {
            video_id: VIDEO.into(),
            url: url.map(str::to_owned),
            checked_at,
        }
    }

    fn thumbs() -> Element {
        rsx! {
            Thumb { src: "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg", size: "sm" }
            Thumb { src: "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg", size: "full" }
            Thumb { src: "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg", size: "square", video: true }
            Thumb { src: "https://i.ytimg.com/vi/lmnopqrstuv/hqdefault.jpg", size: "sm" }
            Thumb { src: "/artwork/youtube:abcdefghijk", size: "square" }
            Thumb { src: "/artwork/library-track", size: "sm" }
        }
    }

    #[test]
    fn songs_show_their_album_cover_at_the_size_they_are_drawn() {
        let html = render(thumbs, |state| {
            state.album_art.remember(lookup(Some(COVER), unix_now()));
            let mut replaced = state.album_art.replaced;
            replaced
                .write()
                .insert("library-track".into(), "0123456789ab".into());
        });
        assert!(html.contains(&format!("src=\"{COVER}=s120\"")), "{html}");
        assert!(html.contains(&format!("src=\"{COVER}=s0\"")));
        assert!(html.contains(&format!("src=\"{COVER}=s544\"")));
        // A video keeps its frame; an unknown video keeps its thumbnail.
        assert!(html.contains("src=\"https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg\""));
        assert!(html.contains("src=\"https://i.ytimg.com/vi/lmnopqrstuv/hqdefault.jpg\""));
        assert!(html.contains("src=\"/artwork/library-track?v=0123456789ab\""));
    }

    #[test]
    fn without_lookups_thumbnails_show_what_they_were_given() {
        let html = render(thumbs, |_| {});
        assert!(!html.contains("googleusercontent"));
        assert!(html.contains("src=\"/artwork/library-track\""));
    }

    #[test]
    fn found_covers_stay_fresh_longer_than_missing_ones() {
        let now = 10_000_000.0;
        let day = 86_400.0;
        assert!(fresh(&lookup(Some(COVER), now - 20.0 * day), now));
        assert!(!fresh(&lookup(Some(COVER), now - 31.0 * day), now));
        assert!(fresh(&lookup(None, now - 6.0 * day), now));
        assert!(!fresh(&lookup(None, now - 8.0 * day), now));
    }

    #[test]
    fn thumbnail_sizes_request_twice_their_drawn_width() {
        assert_eq!(edge_for("xs"), Some(SMALL_EDGE));
        assert_eq!(edge_for("md"), Some(SMALL_EDGE));
        assert_eq!(edge_for("square"), Some(CARD_EDGE));
        assert_eq!(edge_for("full"), None);
    }

    #[test]
    fn memory_keeps_the_most_recently_checked_videos() {
        let mut app = crate::test_support::TestApp::new();
        app.run(|state| {
            for index in 0..ALBUM_ARTWORK_LIMIT {
                state.album_art.remember(AlbumArtwork {
                    video_id: format!("{index:0>11}"),
                    url: None,
                    checked_at: index as f64,
                });
            }
            state.album_art.remember(lookup(Some(COVER), 1e12));
            let lookups = state.album_art.lookups.peek();
            assert_eq!(lookups.len(), ALBUM_ARTWORK_LIMIT);
            assert!(!lookups.contains_key("00000000000"));
            assert!(lookups.contains_key(VIDEO));
        });
    }
}
