# yt-core

In-process Rust domain services used by the Windows and macOS application. This
crate does not launch Python, run an HTTP server, or depend on a desktop UI.

`AppCore::open(profile)` creates a profile with `yt-ultimate.sqlite3` and a
`Library` directory. `AppCore::open_with_library(profile, library)` is the entry
point for an existing installation whose managed library lives elsewhere. Clone
the returned handle to share its synchronized SQLite connection; open the
profile once per application process.

## Implemented

- Library listing and Unicode case-insensitive search; copy/reference imports;
  bounded recursive folder import; M3U/M3U8 local entry import; supported format
  recognition; metadata and duration extraction through Lofty where supported.
- MKV/WebM duration, title, artist, album and audio frequency from bounded
  Matroska metadata; MP4 movie duration from `mvhd`, including video-only files.
  Existing zero-duration video entries are repaired during import/rescan while
  preserving IDs, memberships, paths, nonempty user metadata and original bytes.
- Copied files use collision-resistant names and never overwrite an existing
  file. Repeat imports of an unchanged source return its existing track.
- Metadata updates, record removal, ordered playlist CRUD and duplicate-free
  membership. Removing a record never deletes a media file.
- Typed validated settings, preservation of unknown legacy preferences and
  persisted playback state.
- Persistent jobs with guarded state transitions. Jobs active at startup become
  `Interrupted`; completed output and job history are retained. The execution
  engine owns cancellation, scheduling and retry; the core stores those states.
- Compatibility with the legacy v4 database's track, playlist, settings and job
  tables. Migration uses a SQLite backup including WAL data before introducing
  Rust-specific tables. Track IDs and plugin data are preserved. Database schema
  versions other than legacy v4 and unknown populated databases are rejected
  before migration. This is not an upgrade tool for older legacy schemas.
- Path validation for managed legacy records, including prevention of parent
  traversal and symlink escape from the managed library.
- Removing a library entry preserves the file and records a persistent exclusion
  from automatic scans. An explicit file, directory or M3U import lifts that
  exclusion. Rust schema 2 adds these exclusions without changing legacy tables.
- Preference saves preserve independently updated runtime records, including
  the current playback queue and position, in the reserved `rust.` namespace.

- Schema migrations: the legacy v4 baseline (`src/schema.sql`) is idempotent; every
  Rust-owned change is a numbered migration in `src/migrations/` (`migrations.rs`),
  applied once in its own transaction and recorded in `rust_schema_migrations`.
  Before the first pending migration of an existing Rust profile a
  `pre-schema-<latest>-<id>.sqlite3` backup is written with the SQLite backup API
  (switched to rollback journaling so it is one self-contained file). Schema 3
  moves favorites and playback history into the tables below and drops
  `rust_track_preferences` and `rust_playback_history` after copying them.
- Remote media (`media.rs`): `RemoteMedia` is validated display metadata of an
  online item (`youtube:<11-char id>`, title, artist, `https` artwork, duration);
  the canonical watch URL is derived from the id, never stored. `MediaItem` is a
  library `Track` or `RemoteMedia`. Unreferenced remote records are pruned.
- Favorites (`favorites.rs`): one `rust_favorites` table keyed by media id for
  library tracks and remote media. `set_favorite` rejects unknown ids;
  `set_remote_favorite` remembers and flags in one transaction; re-adding keeps
  the original date. Removing a track removes its favorite and events.
- Listening events (`listening.rs`): `begin_listen` / `record_listen_progress` /
  `end_listen` store raw facts (seconds heard, duration, end reason, who chose
  it). At most one event is open; events left open by a crash end as
  interrupted on the next open. Bounded to `LISTENING_EVENT_LIMIT`.
  `listening_signals` feeds `yt-discovery`; `recently_played` lists distinct
  playable media. `clear_listening_history` erases events, the provider cache,
  remembered album cover lookups, the sound measurements of online media,
  remembered searches and remote media that is not a favorite
  (`ListeningErasure` reports each count). Library measurements, favorites and
  followed artists stay.
- Provider cache (`discovery_cache.rs`, schema 4): one bounded table of provider
  responses keyed by request. `cached_payload` / `store_payload` hold any valid
  JSON up to `MAX_PAYLOAD_BYTES` (1 MiB); `cached_discovery` / `store_discovery`
  are the typed recommendation lists (at most `DISCOVERY_LIST_LIMIT` items).
  At most `DISCOVERY_CACHE_LIMIT` entries, oldest evicted; freshness is the
  caller's policy and callers re-validate what they read back.
- Album cover lookups (`album_artwork.rs`, schema 5, see `docs/systems/Artwork.md`):
  `store_album_artwork(video_id, url)` remembers, as checked now, the `https`
  album cover of an online video or that it has none; `album_artwork(video_id)`
  and `album_artworks()` read them back, skipping rows that no longer validate.
  At most `ALBUM_ARTWORK_LIMIT` videos, the least recently checked evicted on
  write; freshness is the caller's policy.
- Sound measurements (`audio_features.rs`, schema 4): `AudioFeatures` on fixed
  scales whose bounds are associated constants (`ENERGY_RANGE_DBFS`,
  `FULL_DYNAMICS_DB`, `BRIGHTNESS_RANGE_HZ`, `FULL_ONSET_RATE`, `TEMPO_RANGE`,
  `MIN_ANALYZED_SECONDS`). `store_audio_features` validates, rejects unknown
  media and keeps the measurement that covers more audio; `audio_features`
  skips corrupt rows; `unanalyzed_library_tracks` lists library audio without
  one.
- Musical analysis for the DJ (`track_analysis.rs`): `TrackAnalysis` holds beat
  positions in frames at its `sample_rate`, downbeats as beat indices, `Meter`
  (beats per bar, `Subdivision::{Binary, Ternary}`, confidence), tempo and its
  stability, `KeyEstimate` (`PitchClass` 0–11, `KeyMode`, confidence), EBU R128
  integrated, short-term and momentary loudness, true peak, per-bar
  `BandEnergy`, the song's `Section`s (start bar, strong or calm, level; the
  first at bar 0, strictly increasing) and `vocal_db`, the level of its
  separated vocals every loudness step (empty until measured; sections, vocals
  and momentary loudness default to empty in older rows). `audible_start()` is
  where its sound starts past leading silence (`LEADING_SILENCE_LU`, 30 LU below
  the integrated loudness) and `audible_end()` where its music ends before
  trailing silence and most of a fade-out (`TAIL_DROP_LU`, 10 LU), from the
  momentary curve, or the short-term one less `SHORT_TERM_LAG_SECONDS`.
  Bounds are associated constants (`SAMPLE_RATE`, `MAX_DURATION_SECONDS`,
  `MAX_BEATS`, `TEMPO_BPM`, `BEATS_PER_BAR`, `STABLE_INTERVAL`,
  `LOUDNESS_STEP_SECONDS`, `SILENCE_LUFS`, `LUFS`, `LEVEL_DB`, `LOW_BAND_HZ`,
  `HIGH_BAND_HZ`, `MAX_SECTIONS`);
  `validate()` checks a value from the analysis worker or a cache row before
  use. `yt-mir` produces it.
- DJ analysis cache (`dj_analysis.rs`, schema 6): `store_track_analysis(media_id,
  content_key, analysis)` validates the analysis and both keys (1–
  `MAX_ANALYSIS_KEY_BYTES`, no control characters), replaces analyses of other
  versions for the same audio and prunes to `DJ_ANALYSIS_LIMIT` by last use in one
  transaction; `track_analysis(media_id, content_key, analyzer_version)` marks the
  row used, and deletes a row that no longer parses or validates. Erasing the
  listening history removes the analyses of online media
  (`ListeningErasure::dj_analyses`); library files keep theirs, like their sound
  measurements.
- DJ rights per source (`source_rights.rs`): `MediaOrigin::{Library, YouTube}`
  (`of_media_id`) and `SourceRights` (analyze, mix, separate, rearrange, generate,
  keep derived audio), denied by default (`NONE`). `SourceRights::of(origin)`:
  library files allow everything; YouTube allows analysis, mixing, separating,
  rearranging and fusing with a generative model while a transition plays,
  never keeping derived audio (option 1 of `docs/AI_DJ.md` §7.3, the owner's
  choices of 12 November 2026). `with` combines
  the two songs of a transition. Exporting derived audio is never allowed.
- Search history (`search_history.rs`, schema 4): `record_search` collapses
  whitespace, rejects control characters and text over `MAX_SEARCH_CHARS`, and
  counts repeats case-insensitively; `search_history(limit)` is newest first;
  `forget_search` and `clear_search_history` erase. At most
  `SEARCH_HISTORY_LIMIT` entries are kept.
- Followed artists (`followed_artists.rs`, schema 4): YouTube channels
  (`is_channel_id`: `UC` plus 22 characters) with validated name and `https`
  thumbnail; following again keeps the original date.
- Text comparison (`normalize.rs`): `fold` is the one comparison form of text
  for every crate (NFKD without combining marks, lowercase, apostrophe
  variants unified, special Latin letters spelled out, single spaces);
  `fold_into` reuses a buffer; `fold_words` drops punctuation between words
  and `contains_words` matches whole words. `artist_key`, `title_key`,
  `song_title` and `clean_artist` compare uploads of one artist or song.
- Library organization (`library_index.rs`): `library_artists`,
  `library_albums` and `library_album` group playable tracks by their tags,
  ignoring case and spacing.
- Credits (`credits.rs`): `Credits` (provider, label, release date, roles and
  names) with bounds `MAX_CREDIT_CHARS`, `MAX_CREDIT_ROLES`,
  `MAX_CREDIT_NAMES`; `track_credits(id)` reads composer, lyricist, writer,
  producer, arranger, conductor, engineers and remixer tags, label or
  copyright, and the release date of a library file. Values that do not
  validate are dropped; untagged files have empty credits.
- Typed preference groups stored as one row each: `playback`
  (`PlaybackPreferences`: autoplay, DJ mode, crossfade length within
  `CROSSFADE_SECONDS`, `CrossfadeCurve`, crossfade on skip, `DjStyle`
  (`Classic`, `Professional`, `Mashup`, with `plans_mixes()`; settings that
  stored the retired `hybrid` or `generative` styles read as `Mashup`),
  `MixLength` with its `bars()`, the DJ intensity within `DJ_INTENSITY`, and
  `dj_show_mix`, whether the progress bar marks the next mix, and `dj_always_fuse`,
  whether the mashup style fuses every pair), `discovery`
  (`DiscoveryPreferences::online`), `lyrics` (`LyricsPreferences::online`),
  `appearance` (`AppearancePreferences`: `dynamic_color`, `album_artwork`) and `region`
  (`RegionPreferences`: the city and country for nearby concerts, at most
  `MAX_CHARS` each, and `detect`: whether `yt-app` fills them from the
  device's location, see `docs/systems/Region.md`; only names are stored,
  never a position). `detect` defaults to on; a stored group without it reads
  as off when it holds a typed place, so older profiles keep their city. An
  invalid stored group falls back to its defaults; an invalid group is never
  saved.
- Lyrics (`lyrics.rs`, see `docs/systems/Lyrics.md`): `Lyrics` is validated
  synced or plain text, at most `MAX_LYRICS_LINES` lines of `MAX_LINE_CHARS`
  control-free characters; synced lines are in time order and optional word
  stamps ascend and spell their line. `Lyrics::parse` reads LRC (several stamps
  per line, `[offset:]`, metadata tags, enhanced `<mm:ss.xx>` word stamps) or
  plain text, cutting to the bounds and never panicking; `from_timed_entries`
  reads ID3v2 SYLT entries; `from_lyricsfile` (`lyrics/lyricsfile.rs`) reads
  Lyricsfile 1.0 documents (YAML with line and word timing, at most
  `MAX_LYRICS_BYTES`): anchors, aliases, duplicate keys, more than one document,
  nesting deeper than 8 levels and values of another type refuse the document,
  timing outside the media bounds or out of order only loses the line or the
  word timing it affects, and `offset_ms` is ignored. Deserializing validates
  again. `LyricsQuery` and `LyricsCandidate` are the provider-neutral catalog
  contract (bounded text, finite durations); `LyricsCandidate::from_record`
  takes the lyrics the caller parsed with the parser of each text's format and
  drops untimed synced lyrics. `local_lyrics(track_id)` reads a sidecar `.lrc` that must
  resolve to a regular file in the media file's folder (UTF-8, UTF-16 with a
  byte order mark or Latin-1, at most `MAX_LYRICS_BYTES`), then `LYRICS`/USLT/
  `©lyr`/`UNSYNCEDLYRICS` tags and an MP3 SYLT frame in milliseconds, synced
  first. `cached_lyrics`/`store_lyrics` keep catalog lookups (`LyricsLookup`:
  found, instrumental, missing) in the provider cache, so they are bounded with
  it and erased with the listening history.
- Song names (`normalize.rs`): `song_title` splits an upload title into
  performer and song without packaging («(Official Video)», featured artists)
  and `clean_artist` removes channel decorations («VEVO», « - Topic»), keeping
  the letters for catalog searches; `title_key` and `artist_key` are the
  comparison keys built from them (shared by discovery and lyrics).
- `artwork_samples(bytes, side) -> Result<Vec<[u8; 3]>>` decodes with the
  `validate_artwork` bounds and box-filters to at most side×side opaque RGB
  samples (used by the theme). `is_video_frame(bytes)` reads only the image
  header and tells a 16:9 or 4:3 video frame from square album art.
- Embedded front-cover extraction into a validated, hash-addressed Artwork cache;
  old entries acquire embedded artwork on rescan or metadata export. Images are
  decoded with size/dimension/allocation limits and SVG is rejected.
- `media_metadata(id)` exposes library overrides and a validated local artwork
  path for `yt-audio`; `cache_artwork(bytes)` and `set_track_artwork(id, bytes)`
  support downloaded covers without trusting a caller-supplied cache filename.
- `export_playlist_m3u8(id, destination)` writes UTF-8 extended M3U in stored
  order, using relative paths beneath the destination directory and absolute
  paths elsewhere. Missing/unrepresentable entries are reported, and an
  existing export is never overwritten.
- `clean_temporary_files()` removes only recognized, unreferenced partial
  Artwork/Updates files older than 24 hours. It refuses active jobs and preserves
  source media, final covers, installers, symlinks and unknown files.
- Playback state includes optional remote queue records without adding them to
  library tables, plus an independent ordered session-history stack used by
  Previous and Repeat All. Legacy playback JSON without `remote_tracks` or
  `session_history` remains readable. The application validates remote identities
  before restoring them; signed transport URLs never belong in these records.

## Scope boundaries

This crate supplies data services, not the media player, download extractor,
audio DSP, operating system integration, or plugin execution. Those require
separate Rust integrations. It preserves existing Python plugin configuration
without executing those plugins. It does not convert a Python plugin into a
Rust plugin.

`serde-saphyr` (deserializer only, without its serializer and comment
features) parses Lyricsfile YAML straight into typed fields. It is maintained,
denies `unsafe` code, is fuzz-tested not to panic, rejects duplicate keys and
enforces a budget (documents, depth, anchors, aliases), so a hostile catalog
record can neither expand aliases nor nest deep enough to exhaust the stack.
It was chosen over `serde_yaml` (deprecated) and its forks without releases
since 2024, and over tree parsers that would need hand-written limits and
field walking.

Lofty provides tags/properties for supported audio containers. Matroska parsing
uses a metadata-only view capped at 16 MiB, excluding media blocks, attachments,
Cues and arbitrary SeekHead offsets. A malformed/oversized container can still
be imported with its filename and unknown duration; probing never changes it.
Remote URLs inside
M3U files are reported explicitly and are not downloaded as an import side
effect. M3U imports create a playlist named after the file, preserving entry order
and merging repeated imports without duplicate tracks. Existing playlist names
use SQLite's case-insensitive matching. Reference files moved to a new location
require explicit reimport; original files are never relocated by a scan.

## Verification

`cargo test -p yt-core` checks temporary real SQLite profiles, a generated PCM
WAV, migration and backup, legacy playlist triggers, path confinement, import
deduplication and partial errors, playlist transaction rollback, settings,
restart recovery, migrations from Rust schemas 1 and 2 with their backups,
favorites, listening events and erasure, the discovery cache, album cover
lookups and video-frame shapes, preference groups,
lyrics parsing, bounds, SYLT, Lyricsfile documents (word timing, unusable
timing, refused shapes), sidecar confinement and the lyrics cache, song-name
keys,
MKV/WebM metadata, video-only MP4 movie
timing, malformed/oversized metadata and non-destructive repair. It never opens
the user's real profile.
