# Discovery

Home recommends what to listen to next from what the user actually listens to: their library and, when online
recommendations are on (the default), YouTube. Every playback becomes a measured listening event; the profile built
from those events and from favorites seeds the shelves and autoplay.

## Files

- `crates/yt-core/src/listening.rs`: listening events (raw facts), `recently_played`, `clear_listening_history`.
- `crates/yt-core/src/media.rs`: `RemoteMedia`, `MediaItem`, remembered online metadata.
- `crates/yt-core/src/discovery_cache.rs`: bounded cache of provider lists.
- `crates/yt-core/src/settings.rs`: `DiscoveryPreferences::online`.
- `crates/yt-discovery/`: meter, outcome policy, affinity profile and ranking (its `README.md` owns the numbers).
- `crates/yt-media/src/up_next.rs`, `crates/yt-media/src/youtube.rs`: `YoutubeClient::up_next` (Mix + related) and
  `search`.
- `crates/yt-app/src/listening.rs`: the listening controller and the privacy erasure.
- `crates/yt-core/src/audio_features.rs`: `AudioFeatures` (fixed scales as associated constants), storage that keeps
  the fuller measurement, `unanalyzed_library_tracks`.
- `crates/yt-media/src/analysis.rs`: `Analyzer` and `analyze_file`, the signal processing (its README row owns the
  constants and costs).
- `crates/yt-app/src/audio_analysis.rs`: what to measure and keep (`keep_sound`, `start_library_analysis`).
- `crates/yt-app/src/stations.rs`: stations and their refills; `crates/yt-app/src/radio_ui.rs`: the Radio page.
- `crates/yt-app/src/provider_cache.rs`: the freshness policy shared by every provider lookup (`fresh_or_fetch`) and
  typed JSON entries (`cached_json`).
- `crates/yt-app/src/discovery.rs`: profile loading, cached lookups, Home shelves.
- `crates/yt-app/src/home_ui.rs`, `crates/yt-app/src/home_ui.css`: the Home page.
- `crates/yt-app/src/search_ui.rs`: `genre_queries`, shared by the Search chips and the Home starter shelf.
- `crates/yt-app/src/settings_ui.rs`: `DiscoverySection` («Recomendaciones en línea») and `PrivacySection`.

## Listening lifecycle

1. `AppState::playback_started` (local audio, local video, online audio and video) calls `begin_listening`, which
   closes any open event as interrupted and opens a new one attributed to the user, or to autoplay when
   `Listening::attribute_next` marked the pick. A recovery of the same online stream does not open a new event.
2. The shared 250 ms poll calls `listening_tick`: the meter counts heard seconds and every 15 s the progress is
   written, so a crash loses at most that much.
3. `next(automatic)` ends the event as finished when automatic (natural end or DJ blend) and interrupted otherwise.
   Events left open by a previous run end as interrupted when the profile opens.
4. History writes are bookkeeping: a failure is logged, never shown as an interruption.

## Sound measurements

Moods and recommendations need to know how songs sound, measured locally and never asked of a service:

- While audio plays, the engine's analysis tap measures the decoded samples (before the tempo stage, so speed
  changes do not alter the result) and publishes the measurement every 2 s of audio. The shared poll hands it to
  `observe_sound`, which keeps the latest one for the open listening event; `end_listening` stores it through
  `keep_sound` when it covers at least `AudioFeatures::MIN_ANALYZED_SECONDS` (20 s). Online media is measured the same
  way, from the stream that plays.
- Thirty seconds after startup, `start_library_analysis` measures library audio files without a measurement: a random
  sample of at most 150 of them per launch (so files that cannot be measured never block the others), 30 s each
  starting 30 % into the file, one at a time in `spawn_blocking` with a pause in between. Files shorter than the
  minimum are skipped.
- `yt-core` keeps the measurement covering more audio, so a short sample never replaces a fuller one.

## The listening taste

`listening_taste` builds a `Taste` off the UI thread: the affinity `Profile` from the last 2 000 events and all
favorites (long-term score and a 3-day short-term score), listening `sessions` (30-minute gaps, skips left out),
`Embeddings` that place songs played together close (shifted PPMI factorized by randomized SVD, Levy & Goldberg 2014;
Barkan & Koenigstein 2016), `ArtistBeliefs` (a Beta posterior per artist from completions and skips, sampled by
Thompson sampling, Chapelle & Li 2011), and the stored sound measurements. Home keeps the last one in
`Discovery::taste`; search ranking and stations reuse it (`current_taste` builds one when Home has not yet). The
algorithms, parameters and references are owned by the `yt-discovery` README.

## Home shelves

`refresh_home(force)` runs when Home is shown (at most every 15 minutes) or from its refresh button:

1. The taste is built.
2. Local shelves appear at once: «Seguir escuchando», «Tus favoritos», «Redescubre tu biblioteca», «Añadido
   recientemente», and the made-for-you collections that need no network (`made_for_you`), each shown only with at
   least five playable songs:
   - «En loop»: songs played at least three times in the last 14 days, not skipped.
   - «Replay <año>» and «Lo más escuchado de <mes>»: most heard songs since the local start of the year and month.
   - «Mezcla de <artista>»: one per taste cluster (affinity-weighted k-means++ over the embeddings, Arthur &
     Vassilvitskii 2007), named after its leading artist.
   - Moods («Con energía», «De fiesta», «En calma», «Concentración», «Melancolía», «Intensidad»): songs whose measured
     sound clearly belongs to the mood (membership at least 0.3 of 1 on Russell's valence–arousal plane), liked ones
     first.
   - The part of the day («Para tu mañana», «Para tu tarde», «Para tu noche», «Para la madrugada»): liked songs whose
     listening concentrates at this local hour (von Mises kernel).
3. Stations follow the profile: «Tu estación», one per top artist, one per taste mix and mood, and one per favorite
   song.
4. With online recommendations on, shelves are added as they arrive: one «Porque escuchaste…» per seed (up to three
   liked songs, one per artist), «Descubrimiento semanal», «Tu mezcla» (the seeds' lists interleaved and ranked),
   «Más de <artista>» for the top artist, and «Para empezar» for a profile without seeds. A library seed without a
   YouTube source is looked up by artist and title.
5. «Descubrimiento semanal» takes songs the user never heard from the seeds' lists (`weekly_discovery`: provider order
   plus artist affinity, a Thompson-sampled exploration bonus, a novelty bonus for unheard artists, one song per
   artist and MMR diversity, Carbonell & Goldstein 1998). Draws derive from the ISO week and the list is stored under
   `weekly:<year>-<week>`, so it stays the same until Monday.
6. Shelves never repeat items among the per-seed and artist shelves; shelf order is `ShelfKind` order. Home groups
   the collections into «Hechos para ti», «Estados de ánimo» and «Replay».

Provider lists are cached in `rust_provider_cache` (watch-next 6 h, search 24 h, the weekly list 7 days). A failed
lookup falls back to the stale cached list and Home shows an offline banner; the library shelves always remain.

Stations refill through `yt_discovery::station_picks`: continuity with the seeds' artists and with their embedding
centroid, at most a quarter of exploration slots for little-known artists chosen by Thompson sampling, MMR diversity,
and never a recent or queued song.

## Trust boundary and privacy

- Provider results are converted to `RemoteMedia::from_parts`, which validates identity, text lengths, control
  characters, `https` artwork and duration; anything else is dropped. Thumbnails outside YouTube's image hosts are
  cleared by `yt-media`.
- With «Recomendaciones en línea» off, discovery and autoplay make no network requests.
- Requests carry one video id or a search text (an artist name, or artist and title of a library seed), nothing else
  about the user.
- Settings → Privacidad → «Borrar historial de escucha» (`erase_listening_history`) deletes every event, the cached
  provider lists, remembered album cover lookups, the sound measurements of online media, remembered searches,
  remembered online media that is not a favorite, the in-memory history and the playback back stack, then rebuilds
  Home. Favorites and followed artists stay; so do the sound measurements of library files, which describe the files
  rather than the listening.

## Performance

Building the taste (profile, sessions, embeddings, beliefs) and database reads run in `spawn_blocking`; for 5 000
events over 1 500 songs, embeddings and clusters take about 15 ms in release. Network lookups are sequential per
refresh, cached, and superseded by a generation counter so a slow response never overwrites a newer Home. Song
shelves hold at most 12 items; collections hold at most 30. The background library analysis measures at most 150
files per launch, one at a time, at about 0.4 s of one core per long song.

## Tests

`crates/yt-discovery` unit tests (every algorithm, see its README); `yt-core` listening, cache, measurement and
erasure tests; `crates/yt-media` analysis tests; `crates/yt-app/src/listening.rs` (events per playback, erasure),
`crates/yt-app/src/audio_analysis.rs` (sample placement, measurements kept only when long enough),
`crates/yt-app/src/provider_cache.rs` (freshness policy, invalid cached entries refetched),
`crates/yt-app/src/discovery.rs` (interleaving, mapping, collection texts and calendar), and
`crates/yt-app/src/home_ui.rs` (shelf order, badges, offline and local-only states).
