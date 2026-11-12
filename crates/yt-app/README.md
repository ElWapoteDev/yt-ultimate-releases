# yt-app

The desktop application: Dioxus components rendered in WKWebView (macOS) or WebView2 (Windows), the controllers that
own state transitions, and the platform glue. It is the composition root of the workspace and the only crate that
knows Dioxus. Rules, validation, ranking, color math, and persistence live in the crates below it (see `AGENTS.md`).

## Trust boundary

The WebView is the client and the client is public: every event payload, JavaScript bridge reply, and dropped path
is parsed and validated in Rust before it reaches a controller. Controllers call `yt-core`, `yt-media`,
`yt-discovery`, `yt-search`, `yt-theme` and `yt-geo`; components read signals, render, and call controller methods.
Answers of the operating system (the device's position) are validated into typed values the same way.

## Modules

| Module | Role | System document |
|---|---|---|
| `src/main.rs`, `src/lib.rs` | process entry, window configuration, single instance | — |
| `src/state.rs` | composition root (`Services`), shared signals (`AppState`), playback flow (`play`, `next`, `previous`, seek, the 250 ms poll) | `docs/systems/Playback.md` |
| `src/app.rs` | root layout, sidebar, shared media components (`Thumb`, `MediaCard`, `TrackRow`) | `docs/systems/Browse.md`, `docs/systems/Artwork.md` (`Thumb`) |
| `src/navigation.rs`, `src/navigation.css` | page navigation, the detail stack (artist, album, collection), browse menu entries | `docs/systems/Browse.md` |
| `src/cards.rs`, `src/cards.css` | shelves, cover cards, generated animated covers, detail hero | `docs/systems/Browse.md` |
| `src/detail_ui.rs` | detail view host | `docs/systems/Browse.md` |
| `src/library_pages.rs` | Recientes, Artistas and Álbumes pages | `docs/systems/Browse.md` |
| `src/artist_ui.rs`, `src/artist_ui.css` | artist page: banner, follow, library songs and albums; the stylesheet (concerts, biography, online track lists) is included once at the root | `docs/systems/Browse.md` |
| `src/artist_online.rs` | the online part of an artist page: popular songs, releases, videos, related artists, biography | `docs/systems/Browse.md` |
| `src/concerts_ui.rs` | upcoming concerts, the user's region first, and ticket links; showing them starts the session's zone detection | `docs/systems/Browse.md`, `docs/systems/Region.md` |
| `src/region.rs` | region controller: detects the zone for nearby concerts from the device's location and saves it | `docs/systems/Region.md` |
| `src/native_location.rs` | the device's position from Core Location (macOS) or `Windows.Devices.Geolocation`, with the system's permission prompt; opens the location privacy settings | `docs/systems/Region.md` |
| `src/catalog.rs` | catalog controller: cached artist pages, albums, credits, new releases, and the real artist and album of a song | `docs/systems/Browse.md` |
| `src/album_ui.rs` | album page: library and online releases, more from the artist | `docs/systems/Browse.md` |
| `src/new_releases_ui.rs` | Novedades page: new uploads of followed and most listened artists | `docs/systems/Browse.md` |
| `src/following.rs` | followed artists controller (optimistic toggles) | `docs/systems/Browse.md` |
| `src/credits_ui.rs`, `src/credits_ui.css` | song credits: loading, Spanish role names, menu label and dialog | `docs/systems/Browse.md` |
| `src/pages.rs` | page entry points | — |
| `src/player.rs` | player bar, transport controls, seek bar, video panel | `docs/systems/Playback.md` |
| `src/queue_ui.rs`, `src/queue_ui.css` | queue panel, including autoplay suggestions | `docs/systems/Playback.md` |
| `src/autoplay.rs` | autoplay controller: suggestions after the queue runs out | `docs/systems/Playback.md` |
| `src/dj.rs` | DJ controller: planned mixes (a new seed per transition) and their trigger, crossfades as the fallback, stream preparation, the song after the current one, the next mix the player shows (`NextMix`, `MixName`), the scene of the blend being heard (`BlendScene`: the leaving song and the music's beat), labels of the DJ settings | `docs/systems/Playback.md`, `docs/systems/Dj.md` |
| `src/dj_analysis.rs` | DJ analysis controller: measures the playing and next songs in `ytu-worker` and keeps them in the analysis cache | `docs/systems/Dj.md` |
| `src/dj_analysis_ui.rs`, `src/dj_analysis_ui.css` | developer view of the DJ's analysis in Settings | `docs/systems/Dj.md` |
| `src/dj_audio.rs` | the audio a planned mix needs besides the songs, made in `ytu-worker`, read back into memory, its files removed at once: vocal stems (a mashup or a vocal ride; the outgoing ones moved to the incoming key when the plan says so) and an AI fusion's rendering | `docs/systems/Dj.md` |
| `src/dj_ui.rs`, `src/dj_ui.css` | what the player shows of the DJ: the chip naming the next mix and when it starts, its optional zone on the progress bar, the badge naming the technique with its meter and crossfader during a blend, the «IA» tag, the light on the progress bar, and the blend's show, on the beat of beat-matched songs: the player bar's beam, halo and leaving cover, and the full-screen view's rays, rings, leaving cover and banner with a crossfader from one song to the other | `docs/systems/Dj.md` |
| `src/bundled.rs` | finds files shipped beside the executable: `ytu-worker`, ONNX Runtime, models, and the fusion program and model folder | `docs/systems/Dj.md` |
| `src/now_playing.rs`, `src/now_playing_ui.css` | full-screen now-playing view and its song-colored aurora; during a blend it places the DJ's show around the cover and above the title | `docs/systems/Playback.md` |
| `src/listening.rs` | listening controller: one measured event per playback | `docs/systems/Discovery.md` |
| `src/audio_analysis.rs` | sound measurements: keeps what the engine measured while a song played, measures library files in the background | `docs/systems/Discovery.md` |
| `src/discovery.rs` | Home recommendations, cached online lookups, listening profile | `docs/systems/Discovery.md` |
| `src/home_ui.rs`, `src/home_ui.css` | Home page shelves | `docs/systems/Discovery.md` |
| `src/provider_cache.rs` | freshness policy for provider lookups and typed JSON cache entries | `docs/systems/Discovery.md` |
| `src/stations.rs` | stations controller: personal, song, artist, mix, mood and genre stations | `docs/systems/Discovery.md` |
| `src/radio_ui.rs`, `src/radio_ui.css` | Radio page and station cards | `docs/systems/Discovery.md` |
| `src/favorites.rs` | favorites controller (optimistic toggles) | `docs/systems/Favorites.md` |
| `src/favorites_ui.rs`, `src/favorites_ui.css` | Favorites page and favorite button (stylesheet included once at the root) | `docs/systems/Favorites.md` |
| `src/theme.rs` | song-derived theme controller and CSS tokens | `docs/systems/Theme.md` |
| `src/album_art.rs` | album artwork controller: album covers for songs, image sizes per thumbnail, library cover upgrades | `docs/systems/Artwork.md` |
| `src/activity.rs`, `src/activity_ui.rs`, `src/activity_ui.css` | notifications, toasts, running-task registry | `docs/systems/Activity.md` |
| `src/remote.rs` | online playback, remote identities and conversions | `docs/systems/Playback.md` |
| `src/video.rs` | WebView video element bridge | — |
| `src/search.rs` | search controller: history, suggestions, ranking, online search | `docs/systems/Search.md` |
| `src/search_ui.rs`, `src/search_ui.css` | Search page | `docs/systems/Search.md` |
| `src/search_results.rs` | top result and online result sections | `docs/systems/Search.md` |
| `src/library_ui.rs`, `src/library_ui.css` | Songs page and track information | — |
| `src/playlists_ui.rs`, `src/playlists_ui.css` | Playlists page | — |
| `src/downloads.rs`, `src/downloads_ui.rs` | download engine orchestration and page (song downloads embed their album cover) | `docs/systems/Artwork.md` (covers) |
| `src/processing.rs`, `src/processing_ui.rs`, `src/processing_ui.css` | stems, variants, chipmunk jobs | — |
| `src/settings_ui.rs`, `src/settings_ui.css` | Settings page; playback (with the DJ style, mix length and intensity), discovery, region, appearance and privacy sections | `docs/systems/Playback.md`, `docs/systems/Dj.md`, `docs/systems/Discovery.md`, `docs/systems/Region.md`, `docs/systems/Theme.md`, `docs/systems/Artwork.md` |
| `src/updates.rs` | update checks and prompts | — |
| `src/plugins.rs` | plugin panel and host events | — |
| `src/context_menu.rs` | context menus and in-page menus | — |
| `src/local_media.rs` | application protocols (`/media/`, `/artwork/`, `/subtitle/`, `/preview/`) | — |
| `src/lyrics.rs` | lyrics controller: lookup order and cache policy, session sync offsets, the playback clock of the karaoke fill | `docs/systems/Lyrics.md` |
| `src/lyrics_ui.rs`, `src/lyrics_ui.css` | lyrics button, side panel, full-screen lyrics stage and karaoke lines (stylesheet included once at the root) | `docs/systems/Lyrics.md` |
| `src/icons.rs` | static Lucide icon set | — |
| `src/test_support.rs` | test-only harness: `AppState` in a `VirtualDom` on a temporary profile (`TestApp`, `TestApp::showing` a component and `step` to change state and render again), `render` to HTML | — |
| `src/native.rs`, `src/native_*.rs` | menus, window frame, media keys, logging, single instance (location: see `src/native_location.rs`) | — |

Modules marked «—» predate the system documents; give one a document when a change touches its behavior.

## Verification

`cargo test -p yt-app` renders components in a `VirtualDom` and drives controllers against temporary profiles; it
never opens the owner's profile, an audio device, or the network.
