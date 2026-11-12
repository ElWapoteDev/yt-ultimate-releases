# Search

The Search page answers what the user types while they type: their own library ranked by relevance and by what they
like, completions from their remembered searches and their library, one top result, and YouTube's results. Every
decision is made in Rust; the WebView renders the field, the lists and the cards.

## Files

- `crates/yt-search/`: the pure engine — folding re-exported from `yt-core`, BM25F with as-you-type completion and
  typo tolerance, query auto-completion, reciprocal rank fusion, personal re-ranking and the top-result rule. Its
  `README.md` owns the algorithms, parameters, references and timings.
- `crates/yt-core/src/normalize.rs`: `fold`, the one comparison form of text in the workspace.
- `crates/yt-core/src/search_history.rs`: remembered searches (`record_search`, `search_history`, `forget_search`,
  `clear_search_history`).
- `crates/yt-app/src/search.rs`: the controller — `LibrarySearch` (index, tracks by id, artist and completion names),
  `top_result`, `AppState::track_affinity`, `search_suggestions`, `search_online`, and the history methods.
- `crates/yt-app/src/search_ui.rs`, `crates/yt-app/src/search_ui.css`: the page — field, suggestions, recent
  searches, categories, results, rows and their menus.
- `crates/yt-app/src/search_results.rs`: the top result card (song, library song or library artist) and the online
  sections; `views_label`, the compact play count.

## Lifecycle

1. While the Search page is shown, a resource builds `LibrarySearch` from the library in `spawn_blocking`. It rebuilds
   when the library changes or the page is shown again; the previous index serves until the new one is ready, and
   nothing is built while the page is hidden.
2. Every keystroke ranks the library: `Index::search` (BM25F over title, artist, album and file name; the last word
   completes, typos are corrected within a budget, all words must match), then `yt_search::personalize` lifts hits by
   `track_affinity` — the track's affinity in the last profile Home built, or half its artist's — by at most
   `PERSONAL_WEIGHT` (1.0 BM25 units), so liked songs pass close competitors but never bury a clearly better match.
3. While the field has focus, `search_suggestions` completes the text with `yt_search::complete`: remembered searches
   by time-decayed use (14-day half-life), then library names (artists, titles, albums), then provider suggestions.
4. Online, 400 ms after the last keystroke with at least two characters (or at once on Enter, a suggestion, a recent
   search or a category), `search_online` asks YouTube. Each search cancels the request of the previous one, and a
   generation check drops any answer that arrives after a newer search. A YouTube playlist link opens the playlist
   instead. Only searches the user commits (Enter, a suggestion, a recent search) are remembered; typing is not.
5. `top_result` asks `yt_search::top_result` with the library artists, then the titles of the best ten library hits
   followed by the online songs (so the user's own copy wins over an equal online match), then the best library hit.

## Interface

- With an empty field: «Búsquedas recientes» (each removable, «Borrar» clears all) and «Explorar categorías», tiles
  with generated animated covers that run a genre search.
- While typing: suggestions under the field (history, library and provider icons), the top result card with quick
  facts (kind, duration, play count) and play, station and favorite actions, the next songs beside it, «En tu
  biblioteca», and «Más canciones y vídeos».

## Trust boundary

- The query is user input: it is sent only to YouTube's search through `yt-media` (which validates and bounds it) and
  remembered only when committed, through `yt-core`, which rejects control characters and text over
  `MAX_SEARCH_CHARS`.
- Online results are typed `SearchResult` values parsed by `yt-media`; links open only through validated handlers.
- Remembered searches are personal data: they stay on the device, the page can forget one or all, and Settings →
  Privacidad erases them with the listening history.

## Performance

- Index build is off the UI thread (about 15–20 ms for 10 000 tracks in release). A query costs about 0.1 ms with two
  typos and about 20 µs without; completion is linear in library names (about 1.7 ms for 20 000 names).
- Online requests are debounced, cancelled when superseded, and never made per render.

## Tests

- `crates/yt-app/src/search.rs`: history newest first and forgetting; typo tolerance; library artist and the user's
  own copy as the top result, online song when the library has none; affinity lifting a liked song among close
  matches.
- `crates/yt-app/src/search_results.rs`: compact play counts.
- `crates/yt-search`: every algorithm (see its README); `crates/yt-core/src/normalize.rs`: folding.
