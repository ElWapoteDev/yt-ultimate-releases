//! Discovery controller: the listening profile, cached online lookups, and
//! the Home shelves. See `docs/systems/Discovery.md`.

use crate::state::{use_app_signal, AppState};
use dioxus::prelude::*;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    future::Future,
    rc::Rc,
    time::{Duration, Instant},
};
use yt_core::{AudioFeatures, ListenSignal, MediaItem, RemoteMedia, Track, DISCOVERY_LIST_LIMIT};
use yt_discovery::{
    day_part, day_part_mix, iso_week, mood_mix, on_repeat, rank, replay, sessions, taste_clusters,
    week_seed, weekly_discovery, AffinityConfig, ArtistBeliefs, Candidate, ClusterConfig, DayPart,
    EmbeddingConfig, Embeddings, ExploreConfig, FavoriteSignal, ListenPolicy, Mood, Profile,
    RankContext, RankingConfig, WeeklyConfig, SESSION_GAP_SECONDS,
};
use yt_media::{CancellationToken, UP_NEXT_ITEM_LIMIT};

/// Listening events read to build the profile.
const PROFILE_EVENTS: usize = 2_000;
/// Liked songs that each get a «Porque escuchaste…» shelf.
const SEED_SHELVES: usize = 3;
/// Items shown per shelf.
pub(crate) const SHELF_ITEMS: usize = 12;
/// Items in the generated mix.
const MIX_ITEMS: usize = 30;
/// Items requested from the provider per search.
const SEARCH_ITEMS: usize = 30;
/// A cached watch-next list is fresh for this long.
const UP_NEXT_TTL: Duration = Duration::from_secs(6 * 3_600);
/// A cached search is fresh for this long.
const SEARCH_TTL: Duration = Duration::from_secs(24 * 3_600);
/// Seeds of «Tu estación».
const PERSONAL_STATION_SEEDS: usize = 8;
/// Artists that get their own station.
const ARTIST_STATIONS: usize = 6;
/// Seeds per artist station.
const ARTIST_STATION_SEEDS: usize = 5;
/// Liked songs that get their own station.
const SONG_STATIONS: usize = 4;
/// Home refreshes on its own at most this often.
const HOME_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Window, in days, and plays within it that put a song «En loop».
const ON_REPEAT_DAYS: f64 = 14.0;
const ON_REPEAT_MIN_PLAYS: u32 = 3;
/// Songs in a Replay, mood or time-of-day collection.
const COLLECTION_ITEMS: usize = 30;
/// Fewest songs a made-for-you collection needs before Home shows it.
const MIN_COLLECTION_SONGS: usize = 5;
/// Least mood membership a song needs to join that mood's collection; six
/// moods share 1, so 0.3 means the mood clearly dominates.
const MIN_MOOD_MEMBERSHIP: f64 = 0.3;
/// Seed of the embedding factorization and the taste clustering, fixed so
/// mixes stay put between refreshes.
const TASTE_SEED: u64 = 0x7a57_e5ee_d000_0001;
/// Salt of the weekly seed: with the ISO week it fixes «Descubrimiento
/// semanal» for seven days.
const WEEKLY_SALT: u64 = 0x5eed_0000_7eec_1a11;
/// A weekly list is stored under its ISO week and reused all week.
const WEEKLY_TTL: Duration = Duration::from_secs(7 * 24 * 3_600);
/// Seeds of each station made from a taste mix or a mood.
const COLLECTION_STATION_SEEDS: usize = 8;

/// Kinds of Home shelves, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ShelfKind {
    ContinueListening,
    Mix,
    WeeklyDiscovery,
    OnRepeat,
    TasteMix,
    DayPart,
    Mood,
    Replay,
    BecauseYouListened,
    MoreFromArtist,
    Starter,
    Favorites,
    Rediscover,
    RecentlyAdded,
}

impl ShelfKind {
    /// Collections open as their own page («Hechos para ti»); the other
    /// shelves list songs directly on Home.
    pub(crate) fn is_collection(self) -> bool {
        matches!(
            self,
            Self::Mix
                | Self::WeeklyDiscovery
                | Self::OnRepeat
                | Self::TasteMix
                | Self::DayPart
                | Self::Mood
                | Self::Replay
                | Self::Rediscover
        )
    }

    /// Small label above a featured card.
    pub(crate) fn eyebrow(self) -> &'static str {
        match self {
            Self::WeeklyDiscovery => "Novedad semanal",
            Self::Replay => "Replay",
            Self::Mood => "Estado de ánimo",
            _ => "Hecho para ti",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::WeeklyDiscovery => "sparkles",
            Self::OnRepeat => "repeat",
            Self::Replay => "history",
            Self::Mood => "activity",
            Self::DayPart => "clock",
            Self::Rediscover => "retry",
            _ => "disc",
        }
    }
}

/// One Home shelf, ready to render.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shelf {
    pub kind: ShelfKind,
    /// Stable key among the shelves shown together.
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub tracks: Vec<Track>,
}

/// Home feed state shared with the Home page.
#[derive(Clone)]
pub(crate) struct Discovery {
    pub shelves: Signal<Vec<Shelf>>,
    /// Stations built from the profile, strongest first.
    pub stations: Signal<Vec<crate::stations::Station>>,
    pub loading: Signal<bool>,
    /// The last online refresh could not reach the provider.
    pub offline: Signal<bool>,
    /// What the last refresh learned, for features that personalize without
    /// rebuilding it (search ranking, stations).
    pub taste: Rc<std::cell::RefCell<Option<Rc<Taste>>>>,
    refreshed_at: Rc<Cell<Option<Instant>>>,
    generation: Rc<Cell<u64>>,
    /// Varies equal-score order between sessions, stable within one.
    salt: u64,
}

pub(crate) fn use_discovery() -> Discovery {
    Discovery {
        shelves: use_app_signal(Vec::new),
        stations: use_app_signal(Vec::new),
        loading: use_app_signal(|| false),
        offline: use_app_signal(|| false),
        taste: use_hook(Rc::default),
        refreshed_at: use_hook(|| Rc::new(Cell::new(None))),
        generation: use_hook(|| Rc::new(Cell::new(0))),
        salt: use_hook(rand::random::<u64>),
    }
}

/// Unix seconds now; pure engines receive time as a parameter.
pub(crate) fn unix_now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1_000.0
}

/// Offset of local time from UTC now, in seconds, for engines that reason
/// about the local day and week.
pub(crate) fn utc_offset_seconds() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

/// Unix seconds of the local start of this year and of this month.
fn year_and_month_start() -> (f64, f64) {
    use chrono::{Datelike, TimeZone};
    let now = chrono::Local::now();
    let start = |month: u32| {
        chrono::Local
            .with_ymd_and_hms(now.year(), month, 1, 0, 0, 0)
            .earliest()
            .map_or(0.0, |moment| moment.timestamp() as f64)
    };
    (start(1), start(now.month()))
}

/// Spanish name of a month (1 to 12).
fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ];
    month
        .checked_sub(1)
        .and_then(|index| NAMES.get(index as usize))
        .copied()
        .unwrap_or_default()
}

/// Title and subtitle of a mood collection.
pub(crate) fn mood_text(mood: Mood) -> (&'static str, &'static str) {
    match mood {
        Mood::Energetic => ("Con energía", "Tus canciones más vivas y luminosas"),
        Mood::Party => ("De fiesta", "Ritmo para bailar, de lo que ya te gusta"),
        Mood::Calm => ("En calma", "Suave y tranquilo para bajar el ritmo"),
        Mood::Focus => ("Concentración", "Constante y sin sobresaltos"),
        Mood::Melancholic => ("Melancolía", "Para los días grises"),
        Mood::Intense => ("Intensidad", "Fuerte, oscuro y con carácter"),
    }
}

/// Title and subtitle of the collection for a part of the day.
fn day_part_text(part: DayPart) -> (&'static str, &'static str) {
    match part {
        DayPart::Morning => ("Para tu mañana", "Lo que sueles escuchar al empezar el día"),
        DayPart::Afternoon => ("Para tu tarde", "Lo que sueles escuchar por la tarde"),
        DayPart::Evening => ("Para tu noche", "Lo que sueles escuchar al caer la noche"),
        DayPart::Night => ("Para la madrugada", "Lo que sueles escuchar a deshoras"),
    }
}

/// Everything Home learns from the listening history, built together off
/// the UI thread.
#[derive(Default)]
pub(crate) struct Taste {
    pub profile: Profile,
    /// The listening events the profile was built from.
    pub signals: Vec<ListenSignal>,
    /// Songs placed by what the user plays together.
    pub embeddings: Embeddings,
    /// How much each artist's songs are kept or skipped.
    pub beliefs: ArtistBeliefs,
    /// Sound measurements by media id.
    pub features: HashMap<String, AudioFeatures>,
}

/// Ranking candidates in provider order.
pub(crate) fn candidates(media: &[RemoteMedia]) -> Vec<Candidate> {
    media
        .iter()
        .enumerate()
        .map(|(provider_rank, item)| Candidate {
            media_id: item.id.clone(),
            title: item.title.clone(),
            artist: item.artist.clone(),
            duration: item.duration,
            provider_rank,
        })
        .collect()
}

/// Candidates for library tracks; the library has no provider order.
pub(crate) fn library_candidates(tracks: &[Track]) -> Vec<Candidate> {
    tracks
        .iter()
        .filter(|track| track.media_type == "audio" && !track.missing)
        .map(|track| Candidate {
            media_id: track.id.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            duration: track.duration,
            provider_rank: 0,
        })
        .collect()
}

/// Ranked candidates back to playable tracks.
pub(crate) fn remote_tracks(ranked: &[Candidate], media: &[RemoteMedia]) -> Vec<Track> {
    let by_id: HashMap<&str, &RemoteMedia> =
        media.iter().map(|item| (item.id.as_str(), item)).collect();
    ranked
        .iter()
        .filter_map(|candidate| by_id.get(candidate.media_id.as_str()))
        .map(|item| crate::remote::track_from_media(item))
        .collect()
}

pub(crate) fn library_tracks(ranked: &[Candidate], library: &[Track]) -> Vec<Track> {
    let by_id: HashMap<&str, &Track> = library
        .iter()
        .map(|track| (track.id.as_str(), track))
        .collect();
    ranked
        .iter()
        .filter_map(|candidate| {
            by_id
                .get(candidate.media_id.as_str())
                .map(|track| (*track).clone())
        })
        .collect()
}

/// Provider results as remembered media; invalid entries are dropped.
pub(crate) fn to_remote(items: Vec<yt_media::SearchResult>) -> Vec<RemoteMedia> {
    items
        .into_iter()
        .filter_map(|item| {
            RemoteMedia::from_parts(
                &item.id,
                &item.title,
                &item.channel,
                &item.thumbnail,
                item.duration_seconds as f64,
            )
            .ok()
        })
        .take(DISCOVERY_LIST_LIMIT)
        .collect()
}

impl AppState {
    /// What Home learned at its last refresh, or a fresh build that Home
    /// and later callers then reuse.
    pub(crate) async fn current_taste(&self) -> Rc<Taste> {
        let known = self.discovery.taste.borrow().clone();
        if let Some(taste) = known {
            return taste;
        }
        let taste = Rc::new(self.listening_taste().await);
        *self.discovery.taste.borrow_mut() = Some(taste.clone());
        taste
    }

    /// Build the profile, embeddings, artist beliefs and sound measurements
    /// from history, favorites and the audio analysis, off the UI thread.
    pub(crate) async fn listening_taste(&self) -> Taste {
        let core = self.services.core.clone();
        let now = unix_now();
        tokio::task::spawn_blocking(move || {
            let signals = core.listening_signals(PROFILE_EVENTS).unwrap_or_default();
            let favorites: Vec<FavoriteSignal> = core
                .favorites()
                .unwrap_or_default()
                .into_iter()
                .map(|entry| FavoriteSignal {
                    media_id: entry.item.id().to_owned(),
                    title: entry.item.title().to_owned(),
                    artist: entry.item.artist().to_owned(),
                    remote: matches!(entry.item, MediaItem::Remote(_)),
                    added_at: entry.added_at,
                })
                .collect();
            let profile = Profile::build(&signals, &favorites, now, &AffinityConfig::default());
            let sessions = sessions(&signals, SESSION_GAP_SECONDS, &ListenPolicy::default());
            let embeddings = Embeddings::build(&sessions, &EmbeddingConfig::default(), TASTE_SEED);
            let beliefs = ArtistBeliefs::build(&signals, now, &ExploreConfig::default());
            let features = core.audio_features().unwrap_or_default();
            Taste {
                profile,
                signals,
                embeddings,
                beliefs,
                features,
            }
        })
        .await
        .unwrap_or_default()
    }

    /// Mix and related media for one video, from the cache while fresh.
    pub(crate) async fn up_next_media(&self, video_id: &str) -> Result<Vec<RemoteMedia>, String> {
        let state = self.clone();
        let video_id = video_id.to_owned();
        self.cached_lookup(format!("up-next:{video_id}"), UP_NEXT_TTL, async move {
            let youtube = state
                .services
                .youtube_client()
                .map_err(|error| error.to_string())?;
            let up_next = youtube
                .up_next(&video_id, UP_NEXT_ITEM_LIMIT, CancellationToken::new())
                .await
                .map_err(|error| error.to_string())?;
            Ok(to_remote(
                up_next.mix.into_iter().chain(up_next.related).collect(),
            ))
        })
        .await
    }

    /// Search results for `query`, from the cache while fresh.
    pub(crate) async fn search_media(&self, query: &str) -> Result<Vec<RemoteMedia>, String> {
        let state = self.clone();
        let text = query.to_owned();
        self.cached_lookup(
            format!("search:{}", query.trim().to_lowercase()),
            SEARCH_TTL,
            async move {
                let youtube = state
                    .services
                    .youtube_client()
                    .map_err(|error| error.to_string())?;
                let results = youtube
                    .search(&text, SEARCH_ITEMS)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(to_remote(results))
            },
        )
        .await
    }

    /// Video id that represents `track` online; a library track without a
    /// source is looked up by artist and title.
    pub(crate) async fn seed_video_id(&self, track: &Track) -> Option<String> {
        if let Some(id) = crate::remote::video_id_of(track) {
            return Some(id);
        }
        let query = format!("{} {}", track.artist, track.title);
        let found = self.search_media(query.trim()).await.ok()?;
        found.first().map(|media| media.video_id().to_owned())
    }

    /// A media list cached under `key` with the shared freshness policy
    /// (`provider_cache::fresh_or_fetch`); `yt-core` validates every item.
    pub(crate) async fn cached_lookup(
        &self,
        key: String,
        ttl: Duration,
        fetch: impl Future<Output = Result<Vec<RemoteMedia>, String>>,
    ) -> Result<Vec<RemoteMedia>, String> {
        let cached = self
            .services
            .core
            .cached_discovery(&key)
            .ok()
            .flatten()
            .map(|cached| (cached.items, cached.fetched_at));
        let core = self.services.core.clone();
        crate::provider_cache::fresh_or_fetch(cached, ttl, unix_now(), fetch, |items| {
            let _ = core.store_discovery(&key, items);
        })
        .await
    }

    /// Rebuild Home unless it was built recently; `force` always rebuilds.
    pub(crate) fn refresh_home(&self, force: bool) {
        let discovery = self.discovery.clone();
        if !force
            && discovery
                .refreshed_at
                .get()
                .is_some_and(|at| at.elapsed() < HOME_REFRESH_INTERVAL)
        {
            return;
        }
        discovery.refreshed_at.set(Some(Instant::now()));
        let generation = discovery.generation.get().wrapping_add(1);
        discovery.generation.set(generation);
        let mut loading = discovery.loading;
        loading.set(true);
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let current = || state.discovery.generation.get() == generation;
            let taste = Rc::new(state.listening_taste().await);
            if !current() {
                return;
            }
            *state.discovery.taste.borrow_mut() = Some(taste.clone());
            let mut shown = HashSet::new();
            let collections = state.made_for_you(&taste).await;
            if !current() {
                return;
            }
            let mut stations = state.discovery.stations;
            stations.set(state.stations_from_taste(&taste.profile, &collections));
            let mut shelves = state.discovery.shelves;
            let mut list = state.local_shelves(&taste.profile, &mut shown).await;
            list.extend(collections);
            list.sort_by_key(|shelf| shelf.kind);
            shelves.set(list);
            let mut offline = state.discovery.offline;
            if state.settings.peek().discovery.online {
                let reached = state.online_shelves(&taste, &mut shown, &current).await;
                if current() {
                    offline.set(!reached);
                }
            } else {
                offline.set(false);
            }
            if current() {
                loading.set(false);
            }
        });
    }

    /// Forget every shelf, e.g. after the listening history is erased.
    pub(crate) fn reset_home(&self) {
        let discovery = &self.discovery;
        discovery.taste.borrow_mut().take();
        discovery
            .generation
            .set(discovery.generation.get().wrapping_add(1));
        discovery.refreshed_at.set(None);
        let mut shelves = discovery.shelves;
        shelves.set(Vec::new());
        let mut loading = discovery.loading;
        loading.set(false);
    }

    async fn local_shelves(&self, profile: &Profile, shown: &mut HashSet<String>) -> Vec<Shelf> {
        let core = self.services.core.clone();
        let (recent, favorites) = tokio::task::spawn_blocking(move || {
            (
                core.recently_played(SHELF_ITEMS).unwrap_or_default(),
                core.favorites().unwrap_or_default(),
            )
        })
        .await
        .unwrap_or_default();
        let library = self.library.peek().clone();
        let mut shelves = Vec::new();
        let recent: Vec<Track> = recent.iter().map(crate::remote::track_from_item).collect();
        shown.extend(recent.iter().map(|track| track.id.clone()));
        push_shelf(
            &mut shelves,
            ShelfKind::ContinueListening,
            "continue",
            "Seguir escuchando",
            "Lo último que pusiste",
            recent,
        );
        let favorites: Vec<Track> = favorites
            .iter()
            .map(|entry| crate::remote::track_from_item(&entry.item))
            .filter(|track| crate::remote::is_remote(track) || !track.missing)
            .take(SHELF_ITEMS)
            .collect();
        push_shelf(
            &mut shelves,
            ShelfKind::Favorites,
            "favorites",
            "Tus favoritos",
            "Lo que marcaste con un corazón",
            favorites,
        );
        let context = RankContext {
            profile,
            exclude: shown,
            seed_artist: None,
            now: unix_now(),
            salt: self.discovery.salt,
        };
        let ranked = rank(
            library_candidates(&library),
            &context,
            &RankingConfig::rediscover(),
            SHELF_ITEMS,
        );
        let rediscover = library_tracks(&ranked, &library);
        shown.extend(rediscover.iter().map(|track| track.id.clone()));
        push_shelf(
            &mut shelves,
            ShelfKind::Rediscover,
            "rediscover",
            "Redescubre tu biblioteca",
            "Canciones que hace tiempo no escuchas",
            rediscover,
        );
        let added: Vec<Track> = library.iter().take(SHELF_ITEMS).cloned().collect();
        push_shelf(
            &mut shelves,
            ShelfKind::RecentlyAdded,
            "added",
            "Añadido recientemente",
            "Lo nuevo en tu biblioteca",
            added,
        );
        shelves
    }

    /// Add online shelves one by one as they arrive. Returns whether the
    /// provider was reached at least once.
    async fn online_shelves(
        &self,
        taste: &Taste,
        shown: &mut HashSet<String>,
        current: &impl Fn() -> bool,
    ) -> bool {
        let profile = &taste.profile;
        let salt = self.discovery.salt;
        let mut reached = false;
        let seeds: Vec<_> = profile.seeds(SEED_SHELVES).into_iter().cloned().collect();
        let mut mix_pool: Vec<RemoteMedia> = Vec::new();
        for seed in &seeds {
            let track = self.seed_track(&seed.media_id, &seed.title, &seed.artist);
            let Some(video_id) = self.seed_video_id(&track).await else {
                continue;
            };
            let Ok(media) = self.up_next_media(&video_id).await else {
                continue;
            };
            reached = true;
            if !current() {
                return reached;
            }
            let mut exclude = shown.clone();
            exclude.insert(seed.media_id.clone());
            exclude.insert(format!("{}{video_id}", yt_core::REMOTE_ID_PREFIX));
            let context = RankContext {
                profile,
                exclude: &exclude,
                seed_artist: Some(&seed.artist),
                now: unix_now(),
                salt,
            };
            let ranked = rank(
                candidates(&media),
                &context,
                &RankingConfig::online(),
                SHELF_ITEMS,
            );
            let tracks = remote_tracks(&ranked, &media);
            shown.extend(tracks.iter().map(|track| track.id.clone()));
            mix_pool.extend(media);
            self.add_shelf(Shelf {
                kind: ShelfKind::BecauseYouListened,
                key: format!("because:{}", seed.media_id),
                title: format!("Porque escuchaste «{}»", seed.title),
                subtitle: yt_core::clean_artist(&seed.artist),
                tracks,
            });
        }
        if !mix_pool.is_empty() && current() {
            self.weekly_shelf(taste, &mix_pool, shown);
            let exclude: HashSet<String> = seeds.iter().map(|seed| seed.media_id.clone()).collect();
            let context = RankContext {
                profile,
                exclude: &exclude,
                seed_artist: None,
                now: unix_now(),
                salt,
            };
            let ranked = rank(
                candidates(&interleave(&mix_pool, seeds.len().max(1))),
                &context,
                &RankingConfig::online(),
                MIX_ITEMS,
            );
            self.add_shelf(Shelf {
                kind: ShelfKind::Mix,
                key: "mix".into(),
                title: "Tu mezcla".into(),
                subtitle: "Canciones elegidas por lo que escuchas".into(),
                tracks: remote_tracks(&ranked, &mix_pool),
            });
        }
        if let Some(artist) = profile.top_artists(1).first() {
            let name = yt_core::clean_artist(&artist.name);
            if let Ok(media) = self.search_media(&name).await {
                reached = true;
                if !current() {
                    return reached;
                }
                let context = RankContext {
                    profile,
                    exclude: shown,
                    seed_artist: Some(&artist.name),
                    now: unix_now(),
                    salt,
                };
                let ranked = rank(
                    candidates(&media),
                    &context,
                    &RankingConfig::online(),
                    SHELF_ITEMS,
                );
                let tracks = remote_tracks(&ranked, &media);
                shown.extend(tracks.iter().map(|track| track.id.clone()));
                self.add_shelf(Shelf {
                    kind: ShelfKind::MoreFromArtist,
                    key: format!("artist:{}", artist.key),
                    title: format!("Más de {name}"),
                    subtitle: "Uno de tus artistas más escuchados".into(),
                    tracks,
                });
            }
        }
        if seeds.is_empty() {
            let (label, query) = crate::search_ui::genre_queries()[0].clone();
            if let Ok(media) = self.search_media(&query).await {
                reached = true;
                if !current() {
                    return reached;
                }
                let context = RankContext {
                    profile,
                    exclude: shown,
                    seed_artist: None,
                    now: unix_now(),
                    salt,
                };
                let ranked = rank(
                    candidates(&media),
                    &context,
                    &RankingConfig::online(),
                    SHELF_ITEMS,
                );
                self.add_shelf(Shelf {
                    kind: ShelfKind::Starter,
                    key: "starter".into(),
                    title: "Para empezar".into(),
                    subtitle: format!("{label} · escucha algo y tu inicio aprenderá de ti"),
                    tracks: remote_tracks(&ranked, &media),
                });
            }
        }
        reached
    }

    /// A seed as a track: the library track when it is local, else its
    /// remembered online identity.
    fn seed_track(&self, media_id: &str, title: &str, artist: &str) -> Track {
        if let Some(track) = self
            .library
            .peek()
            .iter()
            .find(|track| track.id == media_id)
        {
            return track.clone();
        }
        if let Ok(Some(media)) = self.services.core.remote_media(media_id) {
            return crate::remote::track_from_media(&media);
        }
        let media = RemoteMedia {
            id: media_id.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            artwork_url: String::new(),
            duration: 0.0,
        };
        crate::remote::track_from_media(&media)
    }

    /// «Descubrimiento semanal» from the online pool: computed once per ISO
    /// week and reused until the next Monday.
    fn weekly_shelf(&self, taste: &Taste, pool: &[RemoteMedia], shown: &mut HashSet<String>) {
        let now = unix_now();
        let offset = utc_offset_seconds();
        let (year, week) = iso_week(now, offset);
        let key = format!("weekly:{year}-{week:02}");
        let core = &self.services.core;
        let cached = core
            .cached_discovery(&key)
            .ok()
            .flatten()
            .filter(|cached| now - cached.fetched_at < WEEKLY_TTL.as_secs_f64());
        let media = match cached {
            Some(cached) => cached.items,
            None => {
                let context = RankContext {
                    profile: &taste.profile,
                    exclude: shown,
                    seed_artist: None,
                    now,
                    salt: self.discovery.salt,
                };
                let picks = weekly_discovery(
                    candidates(pool),
                    &taste.profile,
                    &taste.beliefs,
                    &taste.embeddings,
                    &context,
                    week_seed(now, offset, WEEKLY_SALT),
                    &WeeklyConfig::default(),
                );
                let tracks = remote_tracks(&picks, pool);
                let media: Vec<RemoteMedia> = tracks
                    .iter()
                    .filter_map(crate::remote::remote_media)
                    .collect();
                if media.len() >= MIN_COLLECTION_SONGS {
                    let _ = core.store_discovery(&key, &media);
                }
                media
            }
        };
        if media.len() < MIN_COLLECTION_SONGS {
            return;
        }
        let tracks: Vec<Track> = media.iter().map(crate::remote::track_from_media).collect();
        shown.extend(tracks.iter().map(|track| track.id.clone()));
        self.add_shelf(Shelf {
            kind: ShelfKind::WeeklyDiscovery,
            key: "weekly".into(),
            title: "Descubrimiento semanal".into(),
            subtitle: "Música nueva para ti; cambia cada lunes".into(),
            tracks,
        });
    }

    /// Made-for-you collections that need no network: «En loop», Replay of
    /// the year and the month, taste mixes, moods and the part of the day.
    async fn made_for_you(&self, taste: &Taste) -> Vec<Shelf> {
        let now = unix_now();
        let offset = utc_offset_seconds();
        let (year_start, month_start) = year_and_month_start();
        let profile = &taste.profile;
        let mut lists: Vec<(ShelfKind, String, String, String, Vec<String>)> = Vec::new();
        lists.push((
            ShelfKind::OnRepeat,
            "onrepeat".into(),
            "En loop".into(),
            "Lo que no dejas de escuchar estas semanas".into(),
            on_repeat(
                &taste.signals,
                now,
                ON_REPEAT_DAYS,
                ON_REPEAT_MIN_PLAYS,
                COLLECTION_ITEMS,
            ),
        ));
        let today = chrono::Local::now();
        let ids = |entries: Vec<yt_discovery::ReplayEntry>| {
            entries.into_iter().map(|entry| entry.media_id).collect()
        };
        lists.push((
            ShelfKind::Replay,
            "replay:year".into(),
            format!("Replay {}", chrono::Datelike::year(&today)),
            "Lo que más escuchaste este año".into(),
            ids(replay(&taste.signals, year_start, now, COLLECTION_ITEMS)),
        ));
        lists.push((
            ShelfKind::Replay,
            "replay:month".into(),
            format!(
                "Lo más escuchado de {}",
                month_name(chrono::Datelike::month(&today))
            ),
            "Tu mes hasta hoy".into(),
            ids(replay(&taste.signals, month_start, now, COLLECTION_ITEMS)),
        ));
        for cluster in taste_clusters(
            profile,
            &taste.embeddings,
            &ClusterConfig::default(),
            TASTE_SEED,
        ) {
            let names: Vec<String> = cluster
                .artists
                .iter()
                .map(|artist| yt_core::clean_artist(artist))
                .collect();
            let Some(lead) = names.first() else {
                continue;
            };
            let others = names[1..].join(", ");
            lists.push((
                ShelfKind::TasteMix,
                format!("tastemix:{}", yt_discovery::artist_key(lead)),
                format!("Mezcla de {lead}"),
                if others.is_empty() {
                    "Tu lado de este artista".into()
                } else {
                    format!("Con {others} y más")
                },
                cluster.media_ids,
            ));
        }
        for mood in Mood::ALL {
            let (title, subtitle) = mood_text(mood);
            lists.push((
                ShelfKind::Mood,
                format!("mood:{}", mood.key()),
                title.into(),
                subtitle.into(),
                mood_mix(
                    mood,
                    profile,
                    &taste.features,
                    COLLECTION_ITEMS,
                    MIN_MOOD_MEMBERSHIP,
                ),
            ));
        }
        let part = day_part(now, offset);
        let (title, subtitle) = day_part_text(part);
        lists.push((
            ShelfKind::DayPart,
            format!("daypart:{}", part.key()),
            title.into(),
            subtitle.into(),
            day_part_mix(&taste.signals, profile, part, offset, COLLECTION_ITEMS),
        ));
        let known = self
            .known_tracks(
                profile,
                lists
                    .iter()
                    .flat_map(|list| list.4.iter().cloned())
                    .collect(),
            )
            .await;
        let mut keys = HashSet::new();
        lists
            .into_iter()
            .filter(|list| keys.insert(list.1.clone()))
            .filter_map(|(kind, key, title, subtitle, ids)| {
                let mut tracks: Vec<Track> =
                    ids.iter().filter_map(|id| known.get(id).cloned()).collect();
                crate::catalog::unique_ids(&mut tracks, |track| &track.id);
                (tracks.len() >= MIN_COLLECTION_SONGS).then_some(Shelf {
                    kind,
                    key,
                    title,
                    subtitle,
                    tracks,
                })
            })
            .collect()
    }

    /// Playable tracks for media ids the profile knows: library tracks that
    /// still exist and remembered online media. Unknown ids are dropped.
    async fn known_tracks(
        &self,
        profile: &Profile,
        ids: HashSet<String>,
    ) -> HashMap<String, Track> {
        let library: HashMap<String, Track> = self
            .library
            .peek()
            .iter()
            .filter(|track| !track.missing && ids.contains(&track.id))
            .map(|track| (track.id.clone(), track.clone()))
            .collect();
        let remote: Vec<String> = ids
            .into_iter()
            .filter(|id| {
                !library.contains_key(id)
                    && profile.media_affinity(id).is_some_and(|media| media.remote)
            })
            .collect();
        let core = self.services.core.clone();
        let online = tokio::task::spawn_blocking(move || {
            remote
                .iter()
                .filter_map(|id| core.remote_media(id).ok().flatten())
                .map(|media| (media.id.clone(), crate::remote::track_from_media(&media)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        library.into_iter().chain(online).collect()
    }

    /// «Tu estación», one station per top artist, per taste mix and per mood,
    /// and one per favorite song, all seeded from what the user likes.
    fn stations_from_taste(
        &self,
        profile: &Profile,
        collections: &[Shelf],
    ) -> Vec<crate::stations::Station> {
        use crate::stations::{Station, StationKind};
        let seed_tracks = |media: Vec<&yt_discovery::MediaAffinity>| {
            media
                .into_iter()
                .map(|item| self.seed_track(&item.media_id, &item.title, &item.artist))
                .collect::<Vec<_>>()
        };
        let mut stations = Vec::new();
        let personal = seed_tracks(profile.seeds(PERSONAL_STATION_SEEDS));
        if !personal.is_empty() {
            stations.push(Station {
                key: "station:personal".into(),
                kind: StationKind::Personal,
                title: "Tu estación".into(),
                subtitle: "Lo que te gusta y música nueva que encaja".into(),
                seeds: personal,
            });
        }
        for artist in profile.top_artists(ARTIST_STATIONS) {
            let seeds = seed_tracks(
                profile
                    .media()
                    .iter()
                    .filter(|item| {
                        item.score > 0.0 && yt_discovery::artist_key(&item.artist) == artist.key
                    })
                    .take(ARTIST_STATION_SEEDS)
                    .collect(),
            );
            if !seeds.is_empty() {
                stations.push(crate::artist_ui::artist_station(
                    &yt_core::clean_artist(&artist.name),
                    seeds,
                ));
            }
        }
        for collection in collections {
            let kind = match collection.kind {
                ShelfKind::TasteMix => StationKind::Mix,
                ShelfKind::Mood => StationKind::Mood,
                _ => continue,
            };
            stations.push(Station {
                key: format!("station:{}", collection.key),
                kind,
                title: collection.title.clone(),
                subtitle: "Estación: tus canciones y otras que encajan".into(),
                seeds: collection
                    .tracks
                    .iter()
                    .take(COLLECTION_STATION_SEEDS)
                    .cloned()
                    .collect(),
            });
        }
        for seed in seed_tracks(profile.seeds(SONG_STATIONS)) {
            stations.push(Station::from_song(&seed));
        }
        crate::catalog::unique_ids(&mut stations, |station| &station.key);
        stations
    }

    fn add_shelf(&self, mut shelf: Shelf) {
        crate::catalog::unique_ids(&mut shelf.tracks, |track| &track.id);
        if shelf.tracks.is_empty() {
            return;
        }
        let mut shelves = self.discovery.shelves;
        let mut list = shelves.write();
        list.retain(|existing| existing.key != shelf.key);
        let index = list
            .iter()
            .position(|existing| existing.kind > shelf.kind)
            .unwrap_or(list.len());
        list.insert(index, shelf);
    }
}

fn push_shelf(
    shelves: &mut Vec<Shelf>,
    kind: ShelfKind,
    key: &str,
    title: &str,
    subtitle: &str,
    mut tracks: Vec<Track>,
) {
    crate::catalog::unique_ids(&mut tracks, |track| &track.id);
    if !tracks.is_empty() {
        shelves.push(Shelf {
            kind,
            key: key.into(),
            title: title.into(),
            subtitle: subtitle.into(),
            tracks,
        });
    }
}

/// Round-robin over `lists` consecutive chunks so the mix alternates seeds.
fn interleave(pool: &[RemoteMedia], lists: usize) -> Vec<RemoteMedia> {
    let chunk = pool.len().div_ceil(lists).max(1);
    let chunks: Vec<&[RemoteMedia]> = pool.chunks(chunk).collect();
    (0..chunk)
        .flat_map(|index| chunks.iter().filter_map(move |list| list.get(index)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(video: &str, title: &str, artist: &str) -> RemoteMedia {
        RemoteMedia::from_parts(video, title, artist, "", 200.0).unwrap()
    }

    #[test]
    fn collection_texts_name_every_mood_part_of_day_and_month() {
        for mood in Mood::ALL {
            let (title, subtitle) = mood_text(mood);
            assert!(!title.is_empty() && !subtitle.is_empty());
        }
        for part in DayPart::ALL {
            assert!(day_part_text(part).0.starts_with("Para "));
        }
        assert_eq!(month_name(1), "enero");
        assert_eq!(month_name(11), "noviembre");
        assert_eq!(month_name(0), "");
        assert_eq!(month_name(13), "");
        let (year, month) = year_and_month_start();
        assert!(year <= month && month <= unix_now());
    }

    #[test]
    fn interleave_alternates_lists_and_keeps_every_item() {
        let pool = [
            media("aaaaaaaaaa1", "A1", "A"),
            media("aaaaaaaaaa2", "A2", "A"),
            media("bbbbbbbbbb1", "B1", "B"),
            media("bbbbbbbbbb2", "B2", "B"),
        ];
        let mixed = interleave(&pool, 2);
        let titles: Vec<_> = mixed.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["A1", "B1", "A2", "B2"]);
        assert_eq!(interleave(&pool, 1).len(), 4);
        assert!(interleave(&[], 3).is_empty());
    }

    #[test]
    fn ranked_candidates_map_back_to_playable_tracks_in_rank_order() {
        let pool = [
            media("aaaaaaaaaa1", "First", "A"),
            media("bbbbbbbbbb1", "Second", "B"),
        ];
        let mut ranked = candidates(&pool);
        ranked.reverse();
        let tracks = remote_tracks(&ranked, &pool);
        assert_eq!(tracks[0].title, "Second");
        assert!(tracks.iter().all(crate::remote::is_remote));
    }
}
