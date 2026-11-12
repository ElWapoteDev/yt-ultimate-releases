# Lyrics

Time-synced lyrics for singing along with what plays: library files, YouTube audio and YouTube videos. The line being
sung lights up and fills word by word (or across the line when only lines are timed), the view follows it, and a tap
on a line seeks there. Lyrics come from the files of the library first and, while online lyrics are on, from
[LRCLIB](https://lrclib.net), a free public lyrics API. No web page is scraped.

## Files

- `crates/yt-core/src/lyrics.rs`: `Lyrics` (validated synced or plain lines), the parsers (LRC with several stamps
  per line and `[offset:]`, enhanced `<mm:ss.xx>` word stamps, ID3v2 SYLT entries, plain text), `LyricsQuery`,
  `LyricsCandidate`, `LyricsSource`, `LyricsLookup`, `AppCore::local_lyrics`, `cached_lyrics` and `store_lyrics`.
- `crates/yt-core/src/lyrics/lyricsfile.rs`: `Lyrics::from_lyricsfile`, the parser of
  [Lyricsfile 1.0](https://github.com/tranxuanthang/lyricsfile/blob/main/SPECIFICATION.md) documents (YAML with line
  and word timing in milliseconds).
- `crates/yt-core/src/normalize.rs`: `song_title`, `clean_artist`, `title_key` and `artist_key`, shared with
  discovery.
- `crates/yt-core/src/settings.rs`: `LyricsPreferences::online`.
- `crates/yt-lyrics/`: queries, matching and karaoke timing (its `README.md` owns the numbers).
- `crates/yt-media/src/lrclib.rs`: `LrclibClient::{get, search}`; `crates/yt-media/examples/lyrics_probe.rs` is the
  live check.
- `crates/yt-media/examples/lyrics_align_probe.rs` (feature `metal-align`) and
  `crates/yt-media/examples/lyrics_set_probe.rs`: measurement of a proposed, not yet built, on-device job that times
  plain lyrics against the song's audio, and the builder of its LRCLIB-referenced test sets; the design and its
  numbers are in [`docs/LYRICS_ALIGNMENT.md`](../LYRICS_ALIGNMENT.md).
- `crates/yt-app/src/lyrics.rs`: the controller (lookup order, cache policy, offsets, the playback clock).
- `crates/yt-app/src/lyrics_ui.rs`, `crates/yt-app/src/lyrics_ui.css`: `LyricsButton` (player bar), `LyricsPanel`,
  `LyricsStage` (full-screen view) and the karaoke lines. The stylesheet is included once at the root.
- Call sites: `crates/yt-app/src/player.rs` (button), `crates/yt-app/src/app.rs` (panel, Esc),
  `crates/yt-app/src/now_playing.rs` (the «Letra» action and the stage), `crates/yt-app/src/settings_ui.rs` (`LyricsSection`),
  `crates/yt-app/src/listening.rs` (erasure).

## Where lyrics come from

`refresh_lyrics` runs only while a lyrics view is on screen (the panel, or the stage of the full-screen view) and when
the media changes under it. With no view open nothing is read or requested. The first answer wins:

1. A library track's sidecar `.lrc` (same name, same folder), then its embedded tags (`LYRICS`/USLT/`©lyr`, also
   `UNSYNCEDLYRICS` text, which is often LRC, or an MP3 SYLT frame in milliseconds), when they are synced.
2. The cached catalog lookup, while fresh: 30 days when lyrics were found, 3 days for a miss.
3. With «Buscar letras en línea» on and the catalog not asking to wait: for curated tags, the exact lookup
   (`/api/get` by title, artist and duration, which LRCLIB matches within ±2 s); then one search per derived query
   (`/api/search`, at most two), each answered by `yt_lyrics::choose`. Requests go one at a time.
4. Plain lyrics of the file, then a stale cached lookup, when the catalog had nothing or could not be reached.

Most catalog records time only lines. A record marked with word sync (`hasWordSync`; 5 of the 20 records of a popular
song on 2026-11-11) also carries its words' timing in `lyricsfile`, a Lyricsfile document, and offers that document
as its synced lyrics instead of its LRC, as long as some word timing survives validation. One record's LRC and
lyricsfile can disagree, so its synced lyrics come from one of them, never both. Among records that match equally
well, `choose` prefers word timing; a closer recording still wins with line timing.

A remote track, or a library file downloaded from YouTube, is named by its upload: «Performer - Song (Official Video)»
by «PerformerVEVO» is asked for as «Song» by «Performer», then by the channel.

## Karaoke timing

- `active_line` runs in a `use_memo` over the shared 250 ms playback poll's position plus the sync offset; the list
  re-renders only when the active line changes. Rows are components whose props compare by identity.
- The active line and the next one render `LyricFill`: one span per segment whose `animation-delay` and
  `animation-duration` are computed once, when the span mounts, from the lyrics time and the playback rate. The
  WebView animates the registered `--lyric-fill` (painted through the text with `background-clip: text`), so the fill
  runs at full frame rate with no Rust work per frame, and the next line's fill starts on time even though the poll
  switches the active line up to 250 ms later.
- Fills pause with `animation-play-state` while playback is paused, buffering or loading. `follow_lyrics_clock`
  feeds every poll sample to `SyncClock`; a seek, a rate change, an offset change or drift beyond 350 ms bumps
  `LyricsState::sync`, which remounts the fills from the current position.
- When a line becomes active, an anchor inside it mounts and scrolls the lyrics list, and only the list (never the
  full-screen view around it), so the line sits at 40 % of the list's height (`FOCUS_POINT`), a little above the
  middle so more of what comes next shows: smoothly, unless the WebView reports reduced motion through a fixed
  `matchMedia` query. Scrolling the lyrics with the wheel pauses following for 4 seconds.
- Lines step back by distance from the voice, with opacity and scale only: the sung line at full size, its sung
  words bright and tinted by the accent and its words still to sing at 55 %; the next line at 42 %; later lines at
  28 % and sung lines at 16 %, both scaled to 94 %. The list fades at its top and bottom edges, wrapped lines are
  balanced, and an instrumental break shows three dots that breathe while it lasts.
- Reduced motion removes the fill sweep, the scale, the dots' breathing and the line transitions; the active line
  still changes instantly.

## Sync offset

Music videos often open with an intro the catalog's recording does not have. The «−» / «+» buttons move the shown
lyrics by 0,5 s within ±30 s (positive shows them earlier); the label resets it. Offsets are remembered for up to 200
songs during the session, never written to disk, and cleared with the listening history. A tap on a line seeks to its
start minus the offset; the index comes from the rendered list and is checked against the lyrics shown for the media
that plays.

## Trust boundary

- Everything that reaches the views is a `Lyrics` value: at most 1 000 lines of at most 300 control-free characters,
  synced lines in time order, word stamps ascending and spelling their line, timestamps within the media bounds.
  Parsing never panics and cuts text to the bounds; deserializing a cached value validates it again.
- Catalog records are parsed one by one with every field optional; a record that does not validate is dropped.
  Responses are capped (512 KiB for the exact lookup, 2 MiB for a search), every request has a 15 s timeout and
  cancellation, and only `https://lrclib.net` is reachable, including redirects. No cookie or account data is sent.
- A `lyricsfile` is read by `serde-saphyr` into typed fields, within 256 KiB, as one version 1.0 YAML document:
  anchors and aliases (so no alias expansion), duplicate keys, nesting deeper than 8 levels and values of another
  type refuse it, and the record keeps its LRC. Out-of-range or disordered timing only loses the line or the word
  timing it affects. Its `offset_ms` is ignored while the draft leaves its meaning open.
- Sidecar files must resolve, through links, to a regular file in the media file's own folder, within 256 KiB;
  UTF-8, UTF-16 with a byte order mark and Latin-1 are read.
- Offsets and line seeks are bounded in Rust; the WebView only sends clicks.

## Privacy and network budget

- A lookup sends the song's title, artist and duration (and, for curated tags, nothing else), with the application
  name and version as LRCLIB asks clients to identify themselves. Nothing is sent while no lyrics view is open, and
  nothing at all with «Buscar letras en línea» off (Settings → Letras).
- Cached lookups live in the bounded provider cache: they are pruned with it and Settings → Privacidad erases them
  with the listening history, together with the session's offsets.
- A busy catalog (429 or 503) is honoured: its `Retry-After` (60 s when absent, at most an hour) plus up to 30 s of
  random jitter, so many clients do not return together; meanwhile cached and local lyrics still show. Misses are
  cached so a song without lyrics is not asked again for 3 days.
- Each lookup costs one to three sequential requests; a popular song's search answer is about 400 KB, the exact
  lookup about 34 KB. Answers are parsed on the blocking pool, never on the interface thread that polls the lookup:
  the five word-synced documents of a popular song's search (about 26 KB each) take about 5 ms of YAML parsing in a
  release build on an Apple M-series Mac.

## Future server

The contract is provider-neutral: `LyricsQuery` in, validated `LyricsCandidate`s out. A server can take over the
catalog requests behind the same contract, share one cache among all clients, register with the catalog (LRCLIB offers
higher limits to registered applications) or move to a licensed provider, and re-validate every query it receives.

## Tests

`yt-core` lyrics tests (LRC tags, offsets, repeated stamps, enhanced word stamps, plain stanzas, malformed input,
bounds, SYLT grouping and an MP3 with a SYLT frame, Lyricsfile word timing and line ends, unusable timing, refused
documents and bounds, validation of stored values, text decoding, sidecar files and links outside the folder, cache
round trip and erasure); `yt-lyrics` tests (including word timing deciding only between equally good records);
`yt-media` LRCLIB tests (host allowlist, a recorded-shape fixture with a word-synced record whose LRC disagrees and one
whose lyricsfile has another version, `Retry-After`, lookups that need no request); `crates/yt-app/src/lyrics.rs` (hidden views never
load, bounded offsets, line seeks, fallbacks) and `crates/yt-app/src/lyrics_ui.rs` (rendering of synced and plain
lyrics, every empty state, offset labels, line kinds, and a lookup that ends in the «Buscar de nuevo» message rendering
again while playback changes: the messages replace one another in place, so the retry button is a flag, never an
optional handler).
