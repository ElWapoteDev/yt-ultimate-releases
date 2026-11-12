//! Lyrics controller: finds the lyrics of the media that plays (files of the
//! library first, then the LRCLIB catalog while online lyrics are on), keeps
//! the session's sync offsets and follows the playback clock for the karaoke
//! fill. See `docs/systems/Lyrics.md`.

use crate::state::{use_app_signal, AppState};
use dioxus::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};
use yt_core::{CachedLyrics, Lyrics, LyricsLookup, LyricsQuery, LyricsSource, Track};
use yt_lyrics::{choose, queries, Chosen, MatchConfig, SongMetadata, SyncClock};
use yt_media::{CancellationToken, LrclibClient, MediaError};

/// A catalog lookup that found lyrics is reused for this long.
const FOUND_TTL: Duration = Duration::from_secs(30 * 86_400);
/// A catalog miss is asked again after this long; the catalog keeps growing.
const MISSING_TTL: Duration = Duration::from_secs(3 * 86_400);
/// Step of the sync offset buttons, in milliseconds.
pub(crate) const OFFSET_STEP_MS: i32 = 500;
/// Largest sync offset either way, in milliseconds: music videos can open
/// with a long intro the catalog's recording does not have.
pub(crate) const OFFSET_LIMIT_MS: i32 = 30_000;
/// Songs whose offset is remembered during a session.
const OFFSET_MEMORY: usize = 200;
/// Longest random extra wait after the catalog asked to slow down, so many
/// clients told to wait the same time do not all come back together.
const BUSY_JITTER: Duration = Duration::from_secs(30);

/// What the lyrics view shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LyricsView {
    /// Nothing plays.
    Idle,
    Loading,
    Found(Rc<ShownLyrics>),
    /// The catalog knows the recording has no vocals.
    Instrumental,
    /// No source has lyrics; `asked_online` tells whether the catalog was asked.
    Missing {
        asked_online: bool,
    },
    /// The catalog could not be reached (offline is a normal state), or
    /// asked to slow down (`busy`).
    Unavailable {
        busy: bool,
    },
}

/// Lyrics shown for one media item.
#[derive(Debug, PartialEq)]
pub(crate) struct ShownLyrics {
    pub media_id: String,
    pub lyrics: Lyrics,
    pub source: LyricsSource,
}

/// Lyrics state shared with the views; cloned with `AppState`.
#[derive(Clone)]
pub(crate) struct LyricsState {
    /// The side panel is open.
    pub panel_open: Signal<bool>,
    /// The full-screen view shows lyrics in place of the artwork.
    pub stage_open: Signal<bool>,
    pub view: Signal<LyricsView>,
    /// Sync offset of the shown song, in milliseconds; positive shows the
    /// lyrics earlier.
    pub offset_ms: Signal<i32>,
    /// Bumped whenever running fills must be rescheduled.
    pub sync: Signal<u64>,
    offsets: Rc<RefCell<HashMap<String, i32>>>,
    clock: Rc<RefCell<Option<(SyncClock, Instant)>>>,
    generation: Rc<Cell<u64>>,
    /// Media id the view belongs to, including while it loads.
    loaded: Rc<RefCell<Option<String>>>,
    cancel: Rc<RefCell<Option<CancellationToken>>>,
    busy_until: Rc<Cell<Option<Instant>>>,
}

pub(crate) fn use_lyrics() -> LyricsState {
    LyricsState {
        panel_open: use_app_signal(|| false),
        stage_open: use_app_signal(|| false),
        view: use_app_signal(|| LyricsView::Idle),
        offset_ms: use_app_signal(|| 0),
        sync: use_app_signal(|| 0),
        offsets: use_hook(|| Rc::new(RefCell::new(HashMap::new()))),
        clock: use_hook(|| Rc::new(RefCell::new(None))),
        generation: use_hook(|| Rc::new(Cell::new(0))),
        loaded: use_hook(|| Rc::new(RefCell::new(None))),
        cancel: use_hook(|| Rc::new(RefCell::new(None))),
        busy_until: use_hook(|| Rc::new(Cell::new(None))),
    }
}

/// How the application names itself to the catalog, as LRCLIB asks.
fn catalog_user_agent() -> String {
    format!(
        "YT-Ultimate/{} (https://github.com/{})",
        env!("CARGO_PKG_VERSION"),
        yt_updater::RELEASE_REPOSITORY
    )
}

/// Whether `track` is named by an online upload (a YouTube title and channel)
/// rather than curated tags, including library files downloaded from YouTube.
fn named_by_upload(track: &Track) -> bool {
    crate::remote::is_remote(track) || crate::remote::video_id_from_url(&track.source_url).is_some()
}

fn shown(media_id: &str, lyrics: Lyrics, source: LyricsSource) -> LyricsView {
    LyricsView::Found(Rc::new(ShownLyrics {
        media_id: media_id.to_owned(),
        lyrics,
        source,
    }))
}

/// The view for a catalog lookup; plain lyrics of the file stand in when the
/// catalog has nothing.
fn view_for(
    media_id: &str,
    lookup: LyricsLookup,
    local: Option<(Lyrics, LyricsSource)>,
) -> LyricsView {
    match (lookup, local) {
        (LyricsLookup::Found { lyrics, source }, _) => shown(media_id, lyrics, source),
        (LyricsLookup::Instrumental, _) => LyricsView::Instrumental,
        (LyricsLookup::Missing, Some((lyrics, source))) => shown(media_id, lyrics, source),
        (LyricsLookup::Missing, None) => LyricsView::Missing { asked_online: true },
    }
}

/// What to show without asking the catalog: a cached lookup, fresh or stale,
/// else plain lyrics of the file, else `otherwise`.
fn known_view(
    media_id: &str,
    cached: Option<CachedLyrics>,
    local: Option<(Lyrics, LyricsSource)>,
    otherwise: LyricsView,
) -> LyricsView {
    match (cached, local) {
        (Some(cached), local) => view_for(media_id, cached.lookup, local),
        (None, Some((lyrics, source))) => shown(media_id, lyrics, source),
        (None, None) => otherwise,
    }
}

/// Lyrics time, in milliseconds, at playback `position_seconds` with the
/// sync `offset_ms` (positive shows lyrics earlier).
pub(crate) fn lyrics_clock_ms(position_seconds: f64, offset_ms: i32) -> f64 {
    position_seconds * 1_000.0 + f64::from(offset_ms)
}

fn lookup_of(chosen: Chosen<'_>) -> LyricsLookup {
    match chosen {
        Chosen::Synced(lyrics) | Chosen::Plain(lyrics) => LyricsLookup::Found {
            lyrics: lyrics.clone(),
            source: LyricsSource::Lrclib,
        },
        Chosen::Instrumental => LyricsLookup::Instrumental,
    }
}

impl AppState {
    /// Lyrics are on screen: the panel, or the full-screen lyrics view.
    pub(crate) fn lyrics_visible(&self) -> bool {
        *self.lyrics.panel_open.peek()
            || (*self.lyrics.stage_open.peek() && *self.now_playing_open.peek())
    }

    /// Open or close the lyrics panel; it replaces the queue and activity
    /// panels, which share its place.
    pub(crate) fn toggle_lyrics_panel(&self) {
        let open = !*self.lyrics.panel_open.peek();
        let mut panel = self.lyrics.panel_open;
        panel.set(open);
        if open {
            let mut queue = self.queue_open;
            queue.set(false);
            let mut activity = self.activity.open;
            activity.set(false);
            self.refresh_lyrics(false);
        }
    }

    /// Show lyrics, or the artwork, in the full-screen view.
    pub(crate) fn set_lyrics_stage(&self, open: bool) {
        let mut stage = self.lyrics.stage_open;
        stage.set(open);
        if open {
            self.refresh_lyrics(false);
        }
    }

    /// Load the lyrics of what plays while they are on screen. Nothing is
    /// read or requested while no lyrics view is open. `force` asks the
    /// catalog again even when a cached answer is fresh.
    pub(crate) fn refresh_lyrics(&self, force: bool) {
        if !self.lyrics_visible() {
            return;
        }
        let current = self.current.peek().clone();
        let Some(track) = current else {
            self.reset_lyrics_view(LyricsView::Idle);
            return;
        };
        if !force && self.lyrics.loaded.borrow().as_deref() == Some(track.id.as_str()) {
            return;
        }
        let generation = self.reset_lyrics_view(LyricsView::Loading);
        *self.lyrics.loaded.borrow_mut() = Some(track.id.clone());
        let offset = self
            .lyrics
            .offsets
            .borrow()
            .get(&track.id)
            .copied()
            .unwrap_or(0);
        let mut offset_signal = self.lyrics.offset_ms;
        offset_signal.set(offset);
        let cancel = CancellationToken::new();
        *self.lyrics.cancel.borrow_mut() = Some(cancel.clone());
        let online = self.settings.peek().lyrics.online;
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let view = state.find_lyrics(&track, online, force, cancel).await;
            if state.lyrics.generation.get() == generation {
                let mut signal = state.lyrics.view;
                signal.set(view);
            }
        });
    }

    /// Forget what the view shows and cancel a pending lookup; returns the
    /// generation that a new lookup must carry.
    fn reset_lyrics_view(&self, view: LyricsView) -> u64 {
        if let Some(cancel) = self.lyrics.cancel.borrow_mut().take() {
            cancel.cancel();
        }
        *self.lyrics.loaded.borrow_mut() = None;
        let generation = self.lyrics.generation.get().wrapping_add(1);
        self.lyrics.generation.set(generation);
        let mut signal = self.lyrics.view;
        signal.set(view);
        generation
    }

    /// Forget everything lyrics learned this session, after the listening
    /// history was erased; a visible view reloads.
    pub(crate) fn reset_lyrics(&self) {
        self.lyrics.offsets.borrow_mut().clear();
        self.reload_lyrics();
    }

    /// Load the shown lyrics again, after their sources changed (online
    /// lyrics were switched on or off).
    pub(crate) fn reload_lyrics(&self) {
        self.reset_lyrics_view(LyricsView::Idle);
        self.refresh_lyrics(false);
    }

    async fn find_lyrics(
        &self,
        track: &Track,
        online: bool,
        force: bool,
        cancel: CancellationToken,
    ) -> LyricsView {
        let core = self.services.core.clone();
        let id = track.id.clone();
        let library = !crate::remote::is_remote(track) && !track.path.as_os_str().is_empty();
        let (local, cached) = tokio::task::spawn_blocking(move || {
            let local = library
                .then(|| core.local_lyrics(&id).ok().flatten())
                .flatten();
            (local, core.cached_lyrics(&id).ok().flatten())
        })
        .await
        .unwrap_or_default();
        if let Some((lyrics, source)) = local.clone().filter(|(lyrics, _)| lyrics.is_synced()) {
            return shown(&track.id, lyrics, source);
        }
        let age = cached
            .as_ref()
            .map(|cached| crate::discovery::unix_now() - cached.fetched_at);
        let ttl = match cached.as_ref().map(|cached| &cached.lookup) {
            Some(LyricsLookup::Missing) => MISSING_TTL,
            _ => FOUND_TTL,
        };
        let fresh = age.is_some_and(|age| age < ttl.as_secs_f64());
        let busy = self
            .lyrics
            .busy_until
            .get()
            .is_some_and(|until| Instant::now() < until);
        if !online || busy || (fresh && !force) {
            let otherwise = if online {
                LyricsView::Unavailable { busy: true }
            } else {
                LyricsView::Missing {
                    asked_online: false,
                }
            };
            return known_view(&track.id, cached, local, otherwise);
        }
        match self.lookup_catalog(track, cancel).await {
            Ok(lookup) => {
                let core = self.services.core.clone();
                let id = track.id.clone();
                let stored = lookup.clone();
                if let Ok(Err(error)) =
                    tokio::task::spawn_blocking(move || core.store_lyrics(&id, &stored)).await
                {
                    crate::native::record_log(
                        &self.services.profile,
                        &format!("Lyrics lookup was not cached: {error}"),
                    );
                }
                view_for(&track.id, lookup, local)
            }
            Err(error) => {
                let busy = matches!(error, MediaError::Busy { .. });
                if let MediaError::Busy { retry_after } = error {
                    let jitter = BUSY_JITTER.mul_f64(rand::random::<f64>());
                    self.lyrics
                        .busy_until
                        .set(Some(Instant::now() + retry_after + jitter));
                }
                known_view(&track.id, cached, local, LyricsView::Unavailable { busy })
            }
        }
    }

    /// Ask the catalog, one request at a time: the exact lookup for curated
    /// tags, then searches for each derived query until a record matches.
    async fn lookup_catalog(
        &self,
        track: &Track,
        cancel: CancellationToken,
    ) -> Result<LyricsLookup, MediaError> {
        let options = crate::state::Services::connection_options(&self.settings.peek());
        let client = LrclibClient::new(&options, &catalog_user_agent())?;
        let duration = if track.duration > 0.0 {
            track.duration
        } else {
            *self.duration.peek()
        };
        let metadata = SongMetadata {
            title: &track.title,
            artist: &track.artist,
            album: &track.album,
            duration,
            upload: named_by_upload(track),
        };
        let queries: Vec<LyricsQuery> = queries(&metadata);
        let config = MatchConfig::default();
        if let Some(first) = queries.first().filter(|_| !metadata.upload) {
            if let Some(candidate) = client.get(first, cancel.clone()).await? {
                if let Some(chosen) = choose(first, std::slice::from_ref(&candidate), &config) {
                    return Ok(lookup_of(chosen));
                }
            }
        }
        for query in &queries {
            let candidates = client.search(query, cancel.clone()).await?;
            if let Some(chosen) = choose(query, &candidates, &config) {
                return Ok(lookup_of(chosen));
            }
        }
        Ok(LyricsLookup::Missing)
    }

    /// Move the shown lyrics by `delta_ms` (positive shows them earlier),
    /// within `OFFSET_LIMIT_MS`, for the rest of the session.
    pub(crate) fn nudge_lyrics(&self, delta_ms: i32) {
        let offset = self
            .lyrics
            .offset_ms
            .peek()
            .saturating_add(delta_ms)
            .clamp(-OFFSET_LIMIT_MS, OFFSET_LIMIT_MS);
        self.set_lyrics_offset(offset);
    }

    pub(crate) fn reset_lyrics_offset(&self) {
        self.set_lyrics_offset(0);
    }

    fn set_lyrics_offset(&self, offset_ms: i32) {
        let Some(media_id) = self.lyrics.loaded.borrow().clone() else {
            return;
        };
        {
            let mut offsets = self.lyrics.offsets.borrow_mut();
            if offsets.len() >= OFFSET_MEMORY && !offsets.contains_key(&media_id) {
                offsets.clear();
            }
            if offset_ms == 0 {
                offsets.remove(&media_id);
            } else {
                offsets.insert(media_id, offset_ms);
            }
        }
        let mut signal = self.lyrics.offset_ms;
        signal.set(offset_ms);
        self.reschedule_lyrics();
    }

    /// Seek to where line `index` of the shown lyrics starts. The index comes
    /// from the rendered list and is checked against the lyrics shown for the
    /// media that plays now.
    pub(crate) fn seek_to_lyric(&self, index: usize) {
        let start = {
            let view = self.lyrics.view.peek();
            let current = self.current.peek();
            match (&*view, current.as_ref()) {
                (LyricsView::Found(shown), Some(track))
                    if shown.media_id == track.id && shown.lyrics.is_synced() =>
                {
                    shown.lyrics.lines().get(index).map(|line| line.start_ms)
                }
                _ => None,
            }
        };
        if let Some(start) = start {
            let offset = f64::from(*self.lyrics.offset_ms.peek());
            self.seek(((f64::from(start) - offset) / 1_000.0).max(0.0));
        }
    }

    /// Lyrics time now, in milliseconds: playback position plus the offset.
    pub(crate) fn lyrics_time_ms(&self) -> f64 {
        lyrics_clock_ms(*self.position.peek(), *self.lyrics.offset_ms.peek())
    }

    /// Media advances and fills animate. Reads subscribe the caller.
    pub(crate) fn lyrics_running(&self) -> bool {
        *self.playing.read() && !*self.buffering.read() && !*self.stream_loading.read()
    }

    /// Compare the latest playback sample with the running fills, and
    /// reschedule them after a seek, a rate change or drift.
    pub(crate) fn follow_lyrics_clock(&self) {
        let position = self.lyrics_time_ms();
        let rate = f64::from(*self.rate.peek());
        let running = self.lyrics_running();
        let now = Instant::now();
        let reschedule = {
            let mut slot = self.lyrics.clock.borrow_mut();
            match slot.as_mut() {
                Some((clock, sampled)) => {
                    let wall_ms = now.duration_since(*sampled).as_secs_f64() * 1_000.0;
                    *sampled = now;
                    clock.observe(position, rate, wall_ms, running)
                }
                None => {
                    *slot = Some((SyncClock::new(position, rate), now));
                    false
                }
            }
        };
        if reschedule {
            let mut sync = self.lyrics.sync;
            sync += 1;
        }
    }

    /// Restart every running fill from the current position.
    fn reschedule_lyrics(&self) {
        self.lyrics.clock.borrow_mut().take();
        let mut sync = self.lyrics.sync;
        sync += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    #[test]
    fn the_catalog_is_named_with_a_valid_user_agent() {
        let agent = catalog_user_agent();
        assert!(agent.starts_with("YT-Ultimate/"));
        assert!(LrclibClient::new(&yt_media::ConnectionOptions::default(), &agent).is_ok());
    }

    #[test]
    fn uploads_are_told_apart_from_curated_tags() {
        let remote = remote_track("aqz-KE-bpKQ", "Artist - Song (Official Video)");
        assert!(named_by_upload(&remote));
        let app = TestApp::new();
        let mut downloaded = app.video_track("downloaded");
        assert!(!named_by_upload(&downloaded));
        downloaded.source_url = "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into();
        assert!(named_by_upload(&downloaded));
    }

    #[test]
    fn hidden_lyrics_never_load_and_offsets_stay_bounded() {
        let mut app = TestApp::new();
        let track = app.video_track("song");
        app.run(|state| {
            state.current.set(Some(track.clone()));
            state.refresh_lyrics(false);
            assert_eq!(*state.lyrics.view.peek(), LyricsView::Idle);
            assert!(state.lyrics.loaded.borrow().is_none());
            *state.lyrics.loaded.borrow_mut() = Some(track.id.clone());
            for _ in 0..200 {
                state.nudge_lyrics(OFFSET_STEP_MS);
            }
            assert_eq!(*state.lyrics.offset_ms.peek(), OFFSET_LIMIT_MS);
            state.nudge_lyrics(i32::MIN);
            assert_eq!(*state.lyrics.offset_ms.peek(), -OFFSET_LIMIT_MS);
            state.reset_lyrics_offset();
            assert_eq!(*state.lyrics.offset_ms.peek(), 0);
            assert!(state.lyrics.offsets.borrow().is_empty());
        });
    }

    #[test]
    fn line_seeks_use_the_shown_lyrics_of_the_current_song() {
        let mut app = TestApp::new();
        let track = app.video_track("song");
        let lyrics = Lyrics::parse("[00:05.00]One\n[00:12.00]Two").unwrap();
        app.run(|state| {
            state.current.set(Some(track.clone()));
            state.duration.set(60.0);
            state
                .lyrics
                .view
                .set(shown("another-song", lyrics.clone(), LyricsSource::Lrclib));
            state.seek_to_lyric(1);
            assert_eq!(*state.position.peek(), 0.0);
            state
                .lyrics
                .view
                .set(shown(&track.id, lyrics.clone(), LyricsSource::Lrclib));
            state.lyrics.offset_ms.set(1_000);
            state.seek_to_lyric(1);
            assert_eq!(*state.position.peek(), 11.0);
            state.seek_to_lyric(99);
            assert_eq!(*state.position.peek(), 11.0);
        });
    }

    #[test]
    fn catalog_misses_fall_back_to_plain_lyrics_of_the_file() {
        let plain = Lyrics::parse("Plain words").unwrap();
        assert_eq!(
            view_for(
                "id",
                LyricsLookup::Missing,
                Some((plain.clone(), LyricsSource::Embedded))
            ),
            shown("id", plain, LyricsSource::Embedded)
        );
        assert_eq!(
            view_for("id", LyricsLookup::Missing, None),
            LyricsView::Missing { asked_online: true }
        );
        assert_eq!(
            view_for("id", LyricsLookup::Instrumental, None),
            LyricsView::Instrumental
        );
    }
}
