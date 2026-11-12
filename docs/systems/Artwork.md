# Artwork

Songs show the cover of their album instead of the frame YouTube picked for the video, at the resolution of the place
where they are drawn: small sizes in rows, larger in cards, the original image in the full-screen view. Videos keep
their own frames. The theme takes its colors from the same image the thumbnails show.

## Files

- `crates/yt-media/src/album_art.rs`: `parse_album_artwork`, which reads the one song YouTube identifies in a watch-next
  response, `sized_artwork(url, edge)`, which sizes an image on Google's resizable hosts, and `thumbnail_video_id`,
  which names the video of a `i.ytimg.com/vi/<id>/…` thumbnail. `YoutubeClient::album_artwork` (in
  `crates/yt-media/src/youtube.rs`) makes the request. The `yt-media` README owns their guarantees.
- `crates/yt-core/src/album_artwork.rs`: the remembered lookups (`rust_album_artwork`, migration
  `crates/yt-core/src/migrations/0005_album_artwork.sql`), bounded by `ALBUM_ARTWORK_LIMIT`.
- `crates/yt-core/src/artwork.rs`: `is_video_frame(bytes)`, which tells a 16:9 or 4:3 video frame from square art by
  reading only the image header.
- `crates/yt-core/src/settings.rs`: `AppearancePreferences::album_artwork`, the «Carátulas de álbum» switch.
- `crates/yt-app/src/album_art.rs`: the controller. `AlbumArt` (held by `AppState::album_art`) keeps the lookups in
  memory and answers `image(src, edge)`; `AlbumCovers` (mounted once at the root) finds the cover of what plays;
  `download_cover` gives downloads their cover; `set_album_artwork` saves the switch.
- `crates/yt-app/src/app.rs`: `Thumb`, the one component that draws covers. It asks `AlbumArt::image` for what to load
  and falls back to its own `src` when that image fails.
- `crates/yt-app/src/theme.rs`: `theme_src` and `use_artwork_tokens` sample the image `AlbumArt` chose.
- `crates/yt-app/src/downloads.rs`: a song download fetches `download_cover` instead of the video thumbnail.
- `crates/yt-app/src/settings_ui.rs`: `AppearanceSection`, with the «Carátulas de álbum» switch.
- `crates/yt-app/src/listening.rs`: erasing the listening history also forgets the lookups in memory.

## Where a cover comes from

YouTube identifies the song in most music uploads (official videos, lyric videos, audio uploads, visualizers) and
describes it in the video's structured description: a «Música» card with the song, the artist, the album and the
album's square cover on `yt3.googleusercontent.com`. A video has an album cover only when that description names
exactly one song and the video's title mentions it as whole words, ignoring case, accents and bracketed details such
as «(Remix)» (`yt_core::contains_words`, the comparison every crate shares). Mixes (several songs), non-music videos (no song) and videos with background music (a title that does
not name the song) keep their thumbnail. Live checks on 2026-11-11: official, lyric, audio and visualizer uploads of the
same song all named it with the same cover; a one-hour mix named ten songs; a short film named none.

## Sizes

Google's image hosts serve any size from the base URL (`=s<edge>`, `=s0` for the original) and never upscale. `Thumb`
requests about twice its drawn width so covers stay sharp on 2× displays:

| `Thumb` size | Drawn width | Requested |
|---|---|---|
| `xs`, `sm`, `md`, `lg` | up to 60 px | 120 px |
| `xl`, `square` | up to 272 px | 544 px |
| `full` (full-screen art and backdrop) | up to 560 px | original (1400 px for typical releases) |

The theme samples a 128 px copy. A library cover saved from an album cover keeps the original.

## Lifecycle

1. At startup `use_album_art` restores the remembered lookups from SQLite (at most 2,000 rows), unless the switch is
   off.
2. When a song starts (not a video), `AlbumCovers` takes its video id: the stream's own for online media, the source
   URL's for a library track. A lookup checked within 30 days (a found cover) or 7 days (none found) is reused;
   otherwise one watch-next request asks YouTube, and the answer is stored before it is shown. A failed request keeps
   what was known and asks again on the next play. A newer song drops the pending lookup.
3. Every `Thumb` that draws a thumbnail of that video, or the `/artwork/youtube:<id>` route of online media without a
   thumbnail, now loads the album cover at its size; the theme recomputes the palette from it.
4. For a library track, a cover that is missing or a video frame (`is_video_frame`) is replaced by the original album
   cover through `AppCore::set_track_artwork`. Square covers (album art already, or chosen by the user) are kept. The
   track's `/artwork/<id>` route then carries `?v=<revision>` for the rest of the session, so the WebView loads the new
   file.
5. A song download asks `download_cover` before fetching its artwork, so the file embeds and stores the album cover.

## Invariants

- One source of truth: `AlbumArt::image` decides what every thumbnail and the theme load.
- Lookups are bounded: 2,000 videos on disk and in memory, the least recently checked evicted first.
- Turning the switch off sends no more lookups and shows video thumbnails again; covers already saved in the library
  stay. Erasing the listening history (Settings → Privacidad) deletes every lookup, since they reveal what was played.
- The WebView never decides: it receives an `https` URL or an application route chosen in Rust.

## Trust boundary

The watch-next response is untrusted: only an `https` URL on `yt3.googleusercontent.com` or `yt3.ggpht.com` without
credentials, a port, a query or a fragment becomes a cover, and `yt-core` validates it again (length, characters) before
storing it. Image bytes come only through `fetch_thumbnail` (YouTube image hosts on every redirect, 16 MiB cap) and are
decoded under the `yt-core` artwork bounds before they are saved. A lookup sends only the video id, to YouTube, the
same provider the song streams from.

## Performance

- One request per song, about 475 KB for the watch page, then reused for 7 to 30 days; playing a remembered song costs
  one hash-map lookup. Lookups and downloads run off the UI thread; header checks and file writes run in
  `spawn_blocking`.
- Thumbnails re-render only when a lookup changes (about once per new song). Small sizes keep decoded images small: a
  120 px cover is 56 KB of pixels against 7.8 MB for a 1400 px original.

## Tests

- `yt-media` `album_art::tests`: the recorded-shape fixture `tests/fixtures/album_art_next.json`, title matching,
  mixes, background music and untrusted hosts, sizing, and thumbnail video ids.
- `yt-core` `album_artwork::tests` and `artwork::tests`: round trips, validation, the bound, erasure with the history,
  and video-frame shapes.
- `yt-app` `album_art::tests`: thumbnails load the cover at their size, videos and unknown videos keep their images,
  replaced library covers get a revision, freshness and the memory bound. `theme::tests` and `now_playing::tests`
  check that the palette and the full-screen view use the same cover.
- Live check: `cargo run -p yt-media --example album_art_probe -- <video>` prints the cover a video gets.
