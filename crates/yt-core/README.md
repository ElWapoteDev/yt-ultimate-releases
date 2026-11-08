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
- Copied files use collision-resistant names and never overwrite an existing
  file. Repeat imports of an unchanged source return its existing track.
- Favorite flags, metadata updates, record removal, ordered playlist CRUD and
  duplicate-free membership. Removing a record never deletes a media file.
- Typed validated settings, preservation of unknown legacy preferences,
  persisted playback state and bounded playback history.
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
  library tables. Legacy playback JSON without `remote_tracks` remains readable.

## Scope boundaries

This crate supplies data services, not the media player, download extractor,
audio DSP, operating system integration, or plugin execution. Those require
separate Rust integrations. It preserves existing Python plugin configuration
without executing those plugins. It does not convert a Python plugin into a
Rust plugin.

Lofty provides tags/properties for supported audio containers; unsupported video
metadata falls back to the filename and an unknown duration. Remote URLs inside
M3U files are reported explicitly and are not downloaded as an import side
effect. M3U imports create a playlist named after the file, preserving entry order
and merging repeated imports without duplicate tracks. Existing playlist names
use SQLite's case-insensitive matching. Reference files moved to a new location
require explicit reimport; original files are never relocated by a scan.

## Verification

`cargo test -p yt-core` checks temporary real SQLite profiles, a generated PCM
WAV, migration and backup, legacy playlist triggers, path confinement, import
deduplication and partial errors, playlist transaction rollback, settings,
restart recovery and playback history. It never opens the user's real profile.
