# Native media engine

Application logic is Rust. No Python, yt-dlp, Node, FFmpeg subprocess, or local
HTTP service is needed for playback, YouTube resolution or downloads.

| Capability | Implementation and verification |
| --- | --- |
| Audio playback | Rodio/CPAL output; a streaming Symphonia decoder selects a supported audio track within the container. |
| Audio formats | MP3, AAC/M4A, ALAC, WAV/PCM, FLAC, Vorbis and Opus. Opus is provided by the bundled native libopus library. MP4, WebM and Matroska containers are supported when their audio codec is supported. |
| Controls | Play, pause, seek, volume, sleep timer and SoundTouch tempo from 0.25× to 4× while preserving pitch. Position is reported in source time. At 1× the samples pass straight through (SoundTouch engages on the first speed change and again only after a seek), so a song at normal speed is exact and costs nothing; each deck's `TempoControl::frame` publishes the source frame of every output frame, the clock a mix aligns to. A stretch keeps beats within 5 ms of where the tempo puts them (tested at 0.94–1.06×). |
| Crossfades | The engine thread owns decks: one active source plus at most two fading out, each with its own sink, tempo, network cache and remote session. `play_with(path, Transition)` and `play_prepared(stream, Transition)` accept `Transition::Cut` or `Transition::Crossfade(Crossfade)` (1 ms–30 s, `FadeCurve::{EqualPower, Linear, Smooth}`; `starting_at(start)` starts the new source past its leading silence, and a mix's fallback crossfade starts it at the plan's entry). A blend only happens when the current deck is audible; otherwise it starts like a cut. Gains are sample-accurate envelopes (`crossfade.rs`) read by the audio thread once per frame from one packed `AtomicU64`, with no locks or allocations per sample; equal-power pairs keep unit power and linear/smooth pairs unit amplitude. A seek or stop ends any blend; a faded deck releases its stream and session when its ramp ends. `prepare_stream` opens and buffers a remote stream without making it audible (dropping the `PreparedStream` cancels it), so a blend into online media starts on time. `StreamingHandle::complete_file` (and `PlaybackSession::complete_file`) returns the stream's temporary file once every byte is downloaded, so the DJ analyzes it without downloading it again; the file lives as long as the stream's playback or preparation. `PlaybackSnapshot::crossfading` reports an active blend. `Transition::Mix(Mix)` plays a beat-matched mix (`Mix::new` validates a `yt_mix::MixPlan`): the incoming deck is opened at its entry (moved by any lateness so the beats still meet), at the plan's speed, and its `MixStage` (`mix.rs`) stays silent without reading its source until the outgoing deck's frame clock reaches the exit frame; then both stages run their lanes through `yt_mix::DeckProcessor`, updated every 64 frames. The outgoing deck is released after the mix ends; a gate still closed 500 ms after its planned exit opens by force; a matched incoming song glides back to the user's speed after the overlap over 4 s per percent of speed it was changed by, at least 8 s and at most 40 s (`speed_return`: a quarter of a percent a second goes unnoticed, so a mashup at its 10 % limit takes 40 s; a fusion's larger changes return in those 40 s too rather than keep the song off its tempo for minutes). A mix needs an audible deck at the plan's sample rate and 1×, and no more than half its overlap late; otherwise it falls back to an equal-power crossfade of the overlap's length. A seek ends a mix; pausing moves its deadlines (and those of crossfades) with the time paused. A plan that plays the vocals apart (`MixPlan::uses_stems`) needs `Mix::with_stems(outgoing, incoming)`: two `Stem`s (`deck_stems.rs`; vocals of a window of each song, interleaved at its decoded format, at most `MAX_STEM_SECONDS`, 120 s, validated) that cover the plan's `stem_frames` at its rates; without them, or in another channel layout than the decks', the mix falls back to the crossfade. Every deck runs a `StemStage` on its decoded frames, before the tempo stage, so a stem lines up sample for sample: idle it passes samples through; armed, each sample inside the stem becomes `vocals × v + instrumental × (x − v)` with both gains read from the plan's lanes every 64 frames on the mix's clock (seconds since the exit, from the deck's source frame and speed). Nothing in the stage allocates or locks per sample; a stem's buffer is shared (`Arc`) with the engine thread. A mashup between clashing keys plays the outgoing vocals in the incoming key (`MixPlan::vocal_shift`): `Stem::moved(semitones, from)` transposes the stem ahead of time from one second before source frame `from` to its end (`transpose`, SoundTouch's pitch shift over the whole buffer, so it is called off the interface and audio threads), and from that frame the stage plays the moved vocals instead of the song's own, blended in over 20 ms so the change does not click, while the rest of the song is still told apart by its own vocals; `Mix::with_stems` refuses stems whose outgoing vocals are not moved exactly as the plan says (`Stem::move_of`). `transpose(samples, channels, rate, semitones)` keeps the length and puts every sound within 10 ms of where it was (tested with a tone two semitones up and one down). A plan with a fusion (`MixPlan::fusion`) needs `Mix::with_rendering(samples, channels, rate)`: the model's rendering, mono or stereo, finite, as long as `Fusion::clip_frames` within `RENDERING_TOLERANCE_SECONDS`; the engine plays it on its own sink with the fusion's gain lane, gated on the outgoing clock a seam before the exit (a fusion cannot be placed late), pausing, seeking and stopping with the decks and released after it. The incoming deck's gate then opens where the fusion ends, its lanes timed from the exit (`DeckMix::incoming` takes the gate frame and the lane time there). `stretch(samples, channels, rate, speed)` plays samples through the decks' own time stretch, so audio stretched outside the engine lines up with a deck that starts on the same frame. Nothing loops or repeats. |
| Audio analysis | `analysis.rs`: `Analyzer::new(rate, channels)`, `push(sample)`, `analyzed_seconds()`, `features()` measure a song on the fixed scales of `yt_core::AudioFeatures`. Mono downmix; Hann frames of the power of two nearest 46 ms (2,048 samples at 44.1/48 kHz) with 50 % overlap; per frame the RMS level, spectral centroid, positive flux of log-compressed magnitudes normalized to the recent peak level (so onsets do not depend on playback level) with adaptive-threshold peak picking, and a pitch-class profile from interpolated spectral peaks between 100 Hz and 5 kHz (A4 = 440 Hz). Frames below −70 dBFS are left out of energy, dynamics, brightness and mode. Tempo is the peak of a whole-song autocorrelation of the smoothed onset envelope, weighted by a log-normal prior at 120 BPM (σ = 1 octave) that settles octave ambiguity, refined between frame lags, and 0 when the peak is weak (normalized correlation < 0.2, flat envelope) or outside `TEMPO_RANGE`; mode is the best Krumhansl–Kessler major correlation minus the best minor one over all 24 keys, and `key()` returns the best of those 24 keys (`yt_core::KeyEstimate`, its correlation clamped to [0, 1]) for the DJ's analysis in `yt-mir`, `None` before 5 s or without a pitch profile. Memory is fixed at creation (roughly 100 KiB), `push` never allocates or locks, and `features()` is `None` before `Analyzer::MIN_SECONDS` (5 s). Every deck taps its decoded source before the tempo stage (source time, independent of speed) and publishes features every 2 s of audio and at the end with `try_lock` only, so the audio thread never waits; a seek restarts the frame and onset history and keeps the statistics. `PlaybackSnapshot::analysis` reports the active deck. `analyze_file(path, start, length, cancel)` measures a library file on a worker, starts from the beginning when `start` cannot be reached, and checks cancellation every 65,536 samples. Release cost on this Mac (2026-11-11, `examples/analysis_probe.rs`): about 0.73 ms per second of 44.1 kHz stereo (0.07 % of one core), `features()` 2 µs; a 289-second MP3 analyzes in 0.4 s including decoding. Limits: tuning is assumed to be A4 = 440 Hz, songs below about 85 BPM with steady double-time subdivisions can read at double tempo (a 200 BPM pulse reads 100), a tempo change within a song yields its dominant period, and instrument overtones pull some major material toward minor. Unit tests use synthetic tones, click trains, noise, silence and generated WAV files. |
| Search and metadata | `innertube-rs` with an embedded QuickJS interpreter. Complete legacy/modern search cards are parsed once, merging duplicate identities and reading duration badges without inventing rows from nested watch commands. No external JavaScript runtime is launched. |
| Public playlists | Real browse/continuation requests preserve order and repeated videos. Up to 500 playable entries, 20 pages and 120 seconds, with cancellation. Partial results are explicitly marked; unavailable entries are counted separately. Current `continuationItemViewModel` nodes are supported. |
| Up next | `YoutubeClient::up_next(video, limit, cancel)` reads one watch-next request (`/next` with the seed's `RD<id>` Mix) and returns `UpNext { mix, related }`, each capped at 1–50 entries in provider order. Only the playlist panel and the sidebar results are read; end screens, engagement panels and Shorts shelves are ignored. The seed, invalid ids, unplayable Mix entries and non-video lockups are dropped, duplicates are removed (Mix wins, and `related` excludes the whole provider Mix), and thumbnails outside YouTube's image hosts are cleared. The body is capped at 16 MiB with the search timeout and honours cancellation; a video without a Mix returns an empty `mix`. |
| Music catalog | `catalog.rs`, `credits.rs`, `suggest.rs`. Catalog pages use YouTube Music's InnerTube client (`WEB_REMIX` through `innertube-rs`, `music.youtube.com` origin), whose items are classified by their page and video types, never by localized labels. `catalog_search(query, per_kind, cancel)` returns `CatalogSearch { songs, artists, albums, playlists, videos }`, 1–`CATALOG_KIND_LIMIT` (20) per kind from one search page (YouTube shows a handful per kind); podcasts and profiles are dropped. `artist(channel, cancel)` returns `ArtistProfile`: name, subscriber line, biography, square avatar, banner, popular songs, albums, singles and EPs, videos and related artists as shown on the page, plus `events` from one best-effort web watch-next request for a popular song by the artist alone: YouTube's concert shelf (`ticketShelfRenderer`) gives date, venue, location and a Bandsintown link unwrapped from YouTube's redirector; other vendor hosts, repeated listings and shelves about a guest artist are dropped, and a failed request leaves `events` empty. `find_artist(name, cancel)` reads one artist-only search: exact name ignoring case and accents, else the first artist named in `name`, else the top result when the names contain each other. `album(id, cancel)` takes an `OLAK5uy_` playlist (bare or in a link) or an `MPREb_` release page id and returns `AlbumDetails`, always with the `OLAK5uy_` id: the album playlist's art tracks (up to 500 over five pages; greyed-out and repeated rows dropped) and the release page's header. `song_context(video, cancel)` returns `SongContext { artists, album }`, how YouTube Music credits a song, from one YouTube Music watch-next request and, only when the byline links an album, one release page request: the byline's artist links (real artists, never the uploading «Artist - Topic» or «ArtistVEVO» channel), and for art tracks the album, single or EP as an `AlbumSummary` with its `OLAK5uy_` id. Music videos link no release, so their `album` is `None`; uploads whose byline links only a user channel (fan copies, the artist's own non-music uploads, other videos) return an empty context. `credits(video, cancel)` returns `yt_core::Credits` for art tracks, recognized by the «Provided to YouTube by …» description that one web watch-next request reads (provider, ℗ line, `Released on`, roles split such as «Composer Lyricist»), and only for them one YouTube Music request reads the track credits page (`MPTC<video>`): its performers, writers and producers come first as `Performer`, `Writer` and `Producer`, then the description's roles, with names deduplicated per role and everything within `MAX_CREDIT_ROLES`/`MAX_CREDIT_NAMES`/`MAX_CREDIT_CHARS`; a page name the description already credits in a detailed role (`DETAILED_ROLES`: composer, lyricist, songwriter or author for writers; associated, main or featured performer or vocals for performers) is not repeated, so the page fills only what the description omits. Those sections are told apart only by their titles, which follow the session's language, through a folded table of the Spanish, English, Portuguese, French, Italian and German titles (regional variants included); other titles, such as the metadata source, are skipped, and a failed page request leaves the description's credits. Other videos give empty credits without a second request. `suggestions(prefix, cancel)` returns at most 10 completions from `suggestqueries-clients6.youtube.com` only (redirects included, 64 KiB, 10 s), sending the prefix and fixed client parameters but no cookies or session data. `new_releases(artist, cancel)` returns at most 20 uploads of the last year from one web search with the upload-date filter, kept only when the channel or a badged collaborator is the official artist channel named exactly like the artist. Release kinds come from Spanish, English, Portuguese, French, Italian and German labels, otherwise `ReleaseKind::Unknown`; catalog rows carry no play counts (`view_count` 0) because YouTube prints them compact and localized. Every type has `validate()` (ids, text bounds, control characters, image and ticket hosts) so cached copies can be checked again. Requests share the 16 MiB cap and 45 s timeout of the other InnerTube calls and honour cancellation. |
| HTTP playback | A seekable disk range cache opens the decoder before download completion, prioritizes seeks, reports buffering and cancels promptly. One expired YouTube URL can be renewed using the original connection preferences. |
| YouTube audio | AAC/M4A selected for interoperability. The currently validated player client is VISIONOS; earlier IOS/ANDROID_VR clients could resolve URLs but serve only their first minute. |
| Video muxing | Pure Rust MP4/fMP4 demux and mux preserve encoded samples, codec configuration, DTS/CTS, keyframes, edit lists, rotation and language. No external FFmpeg runs. Files with discontinuous timelines, segments/metadata above 64 MiB, or more than two million samples per track are explicitly rejected. |
| Video selection | Best or a maximum height from 144p to 4320p. Returns the actual resolution and separate original MP4 video/AAC tracks where DASH is needed. H.264 is preferred at equal height and frame rate for compatibility. |
| Temporary video playback | `prepare_video` selects H.264/AAC, downloads and muxes original tracks into an owned temporary MP4 outside the library. It reports bytes and actual resolution, supports cancellation, and removes its files on drop. This fallback waits for preparation when no progressive video is available; it does not depend on HLS support in the Windows WebView. |
| Local Matroska video | `prepare_local_video` remuxes AVC/HEVC with AAC-LC into MP4, including B pictures, variable cadence, crop/aspect, codec delay and final audio padding. VP8/VP9 with Vorbis/Opus is rewrapped as WebM while preserving retained payloads and seek offsets; inputs without incompatible tracks retain the entire Segment. Originals remain untouched; temporary output belongs to its returned handle. |
| Downloads | Bounded ranges, known complete length verification, cancellation, progress and atomic no-clobber completion. Failed/aborted transfers remove temporary files. |
| Resumable downloads | Durable ID/itag/language/DRC/size/SHA256 checkpoints, OS file locks and validated range retries. Restarting with a fresh URL resumes committed bytes; corrupted or mismatched partials are rejected. Cancellation pauses; explicit discard deletes only the matching inactive job. |
| Subtitles | Lists the actual caption tracks, downloads the exact selected language, and exports timed SRT/WebVTT without a subprocess. |
| Lyrics catalog | `LrclibClient::get` (exact title, artist and duration; LRCLIB matches within ±2 s and the album is never sent because it must match exactly) and `search` (title and artist, at most `LRCLIB_SEARCH_LIMIT` records) return validated `yt_core::LyricsCandidate`s, parsing each record with every field optional. A record marked with word sync (`hasWordSync`) offers its `lyricsfile` (`yt_core::Lyrics::from_lyricsfile`) as its synced lyrics instead of its LRC when the document yields word timing; the two can disagree, so one record's sources are never mixed. Bodies are parsed on Tokio's blocking pool, never on the executor that polls the lookup (the interface thread in the application). Only `https://lrclib.net` is reachable, including at most 3 redirects; bodies are capped at 512 KiB and 2 MiB, each request has a 15 s timeout and cancellation, and the connection's proxy and timeout are honoured but never its cookies. 429 and 503 become `MediaError::Busy` with the `Retry-After` wait (60 s when absent, at most an hour). The caller names the application in the `User-Agent`, as LRCLIB asks. |
| Connection preferences | Configurable socket timeout and HTTP(S) proxy; explicit Netscape cookie files filter expired, foreign-origin and path-restricted cookies. A user-initiated browser connection reads only YouTube cookies and keeps the session in memory. Native Keychain/DPAPI permission checks are honored. Credentials and signed URLs are excluded from diagnostic output and resume files. Browser stores are never opened automatically. |
| Cover art | Bounded 16 MiB HTTPS image downloads from YouTube image hosts (`*.ytimg.com`, `yt3.ggpht.com`, `yt3.googleusercontent.com` and YouTube Music's `lh3.googleusercontent.com`), with cancellation, redirect validation, and no account cookies. |
| Album covers | `YoutubeClient::album_artwork(video, cancel)` reads one watch-next request (`/next` with only the video id, same size cap and timeout as up next, cancellable) and returns the base URL of the square album cover of the one song YouTube identifies in the video's structured description, or `None`: no song (non-music videos), several (mixes, medleys), a song the title does not mention as whole words ignoring case, accents and bracketed details (background music), or an image outside `yt3.googleusercontent.com`/`yt3.ggpht.com` without credentials, port, query or fragment. Both the current description layout (`videoTitleHeaderViewModel`) and the earlier one are read. `sized_artwork(url, edge)` asks those hosts for `=s<edge>` or the original (`None`), which they never upscale, and leaves other URLs unchanged; `thumbnail_video_id` names the video of an `i.ytimg.com/vi/<id>/…` thumbnail. See `docs/systems/Artwork.md`. |
| Existing files | No download overwrites an existing destination, including concurrent completion races. |
| Optional vocal separation | `native-stems`: real BS-RoFormer inference in Rust/LibTorch, overlapping 8-second windows and float WAV vocals/instrumental. Detects available CUDA/MPS and retries on CPU if the model cannot execute there. Reports the actual device and fallback reason. The current model falls back on this Mac because MPS cannot load its float64 constants; Windows Full bundles CPU LibTorch. |

The downloader treats ordinary HTTP 206 `Content-Range` responses separately
from Google video CDN query ranges, whose response length describes one chunk.
`download_sized` should receive the resolver's complete `content_length`.
Short or metadata-only network probes do not prove a complete video can download.

## Verification

`cargo test -p yt-media` covers decoding, Opus seeking, URL validation,
filename portability, cancellation, interrupted responses and byte-range joins.
These tests do not contact YouTube and do not need an audio device.

`cargo run -p yt-media --example audio_probe` plays generated digital silence
and exercises the actual audio output device. Verified on this Mac.

`cargo run -p yt-media --example online_probe -- aqz-KE-bpKQ --download`
searches/resolves the open Blender short *Big Buck Bunny*, downloads its complete
audio into an automatically removed temporary directory and decodes it natively.
On 2026-11-08 the complete 10,271,496-byte, 635-second AAC file passed this check.
Network access is required, and YouTube can change its client behavior.

`cargo run -p yt-media --example playlist_probe` reads a public MIT linear
algebra playlist. On 2026-11-08 it returned all 35 entries with `truncated=false`;
limiting it to three entries correctly returned `truncated=true`.
`cargo run -p yt-media --example playlist_probe -- UUEBb1b_L6zDS3xTUrIALZOw 105`
also passed: 105 entries from the 8,161-item MIT OpenCourseWare uploads list,
proving a real continuation beyond the first 100-item page. These probes only
read metadata and do not download videos or publish playlists.

`cargo run -p yt-media --example up_next_probe -- <video>` prints the Mix and
related counts with sample rows; it reads metadata only. On 2026-11-11 the
default official music video returned 24 Mix and 7 related videos (the sidebar
largely repeats the Mix), a `music.youtube.com` link returned 24 and 19, and the
non-music *Big Buck Bunny* returned no Mix and 19 related videos. Unit tests use
`tests/fixtures/watch_next.json`, an anonymized copy of that response's shape.

`cargo run -p yt-media --example album_art_probe -- <video>…` prints the album
cover each video gets and the URLs the application loads at 120 px, 544 px and
the original size; it reads metadata only. On 2026-11-11 an official video, a
lyric video and an audio upload of the same song returned the same cover (its
original is 1400×1400), a lyric video of a remix returned the original album's
cover, a one-hour mix naming ten songs and *Big Buck Bunny* returned none. Unit
tests use `tests/fixtures/album_art_next.json`, an anonymized copy of the
description panel of that response with the current web client.

`cargo run -p yt-media --example catalog_probe -- "<artist or query>" [video]`
calls every catalog method, validates each result and prints counts and sample
rows, and every credit role of `video` (else of the artist's most popular song)
and of the artist's first music video; image and ticket links are printed as
hosts and only public metadata is read. On 2026-11-11 (session locale es-419,
MX) «peso pluma» gave 10 suggestions; a search with 5 per kind gave 5 songs, 4
artists, 3 albums, 5 playlists and 3 videos; the artist page had 5 popular
songs, 8 albums, 10 singles, 20 videos, 10 related artists and no concerts; its
newest album had 19 tracks; the credits of «HOLLYWOOD» named The Orchard
Enterprises, «℗ 2024 Double P Records», 2024-06-20 and 6 roles: from the credits
page the performers Peso Pluma and Estevan Plazola and the writer Estevan
Plazola, whom the description does not name, then the description's three
producers, mixing and mastering engineer and publisher; its music video had no
credits; Morat's «Cuando Nadie Ve», whose description names its six composers
and lyricists, gained only its performer from the page; and 15 official uploads
of the last year were found. The credits pages
of 28 art tracks found by searching ten artists (classical, rock, pop, K-pop,
chanson, reggaeton, opera) all had «Interpretada por», «Escrita por» and
«Metadatos musicales proporcionados por» sections, 23 also «Producida por», and
none another title; the page of a music video had no sections and that of an
ordinary upload was not found (404). «morat»
returned 21 concerts and «metallica» 19 (two repeated listings removed), all with
Bandsintown links, and Metallica's «ReLoad (Remastered Deluxe Box Set)» 221 tracks
over three pages. No other ticket vendor host appeared among 50 events of nine
artists. `song_context` credited Peso Pluma and Estevan Plazola for both
«HOLLYWOOD» uploads, with the album «ÉXODO» (2024, 24 tracks when opened) only
for the art track; Despacito's official video credited Luis Fonsi and Daddy
Yankee; an art track of a Topic channel («…: Tema» on the web) credited the
orchestra and Beethoven with its 1994 album; a single resolved with kind
`Single`; a fan lyric video, the artist's own concert film and *Big Buck Bunny*
gave empty contexts. Unit tests use `tests/fixtures/music_search.json`,
`music_artist.json`, `music_album_tracks.json`, `music_album_page.json`,
`music_watch_topic_track.json`, `music_watch_vevo_video.json`,
`music_watch_fan_upload.json`, `music_track_credits.json`, `watch_ticket_shelf.json`
and `recent_uploads_search.json`, anonymized copies of those responses' shapes.

`cargo run -p yt-media --example stream_probe` verifies decoding and seeking
without an audio device. The same complete 10.27 MiB YouTube source opened in
167 ms after only 256 KiB had arrived; seeking to 120 seconds also passed while
the cache was incomplete. A deliberately slow local range-server regression test
asserts both behaviors without Internet access.

`cargo run -p yt-media --example resume_probe` pauses that public audio at
1,048,576 bytes, creates a new resolver and downloader, resumes it, and decodes all
634.625 seconds. Offline tests also simulate an uncommitted crash tail, corrupted
committed bytes, and mismatched identities. The temporary files are removed.

`cargo run -p yt-media --example dash_probe` selects the Blender short's best
available resolution (2160p60 in the verified response), then downloads its 144p
MP4 video and AAC audio into `ytu-dash-probe` under the operating system temporary
directory for mux verification. An optional second argument selects the directory.

`cargo run -p yt-media --example subtitles_probe` found 36 caption tracks for
3Blue1Brown's linear algebra preview and retrieved the English track's 80 cues
(8,044-byte WebVTT). It never downloads a substitute language silently.

`cargo run -p yt-media --example lyrics_probe -- "<title>" "<artist>" [seconds]`
asks LRCLIB for one song with a generic client name. On 2026-11-11 the default
(«Bailando», Enrique Iglesias, 244 s) found the exact record in 581 ms with 80
synced lines, all with word timing from its lyricsfile (its LRC has 81 lines),
and 97 plain lines; a search returned 20 records, 5 with word timing, in 253 ms.
Unit tests use `tests/fixtures/lrclib_search.json`, a placeholder copy of that
response's shape.

`cargo run --release -p yt-media --features metal-align --example lyrics_align_probe --
<whisper-dir> <jamendolyrics-dir> [languages] [songs] [f32|f16] [variant]` (macOS)
measures a proposed lyrics alignment job: Whisper forced alignment of known lyrics
on Metal, with cross-attention DTW over the published alignment heads, against the
word onsets of JamendoLyrics MultiLang. The feature only adds the optional
`candle-nn` (Metal) and `tokenizers` dependencies for this example; nothing in the
library uses it. On 2026-11-11 Whisper small in FP16, with timing corrections
calibrated on the German and English songs, placed 90 % of the 79 songs' words
and 86 % of their line starts within 0.3 s (89 % and 83 % on the held-out Spanish
and French songs) with 3.4 s of GPU time per 4-minute song, and every aligned song
but one parsed as valid enhanced LRC through `yt_core::Lyrics::parse`. The method, all measurements and the design are in
[`docs/LYRICS_ALIGNMENT.md`](../../docs/LYRICS_ALIGNMENT.md); models and data stay
outside the repository.

`cargo run --release -p yt-media --example lyrics_set_probe -- <songs.txt> <out-dir>`
builds such a test set from `title | artist` lines: the LRCLIB record with both
synced and plain lyrics, and the YouTube audio whose title names the song and whose
duration matches the record within 2 s, one request at a time. The alignment probe
reads the `manifest.tsv` it writes, aligns the plain text and compares the line
starts with the record's synced ones. On 2026-11-11, for 30 songs of the owner's
genres, half the words agreed with LRCLIB's user timing within 0.3 s, and three in
four once each song's constant offset was removed.

`cargo run -p yt-media --example preview_probe -- 'peso pluma' --prepare`
verified 23 unique search results with actual durations, then prepared Hollywood
as a 17,798,586-byte H.264/AAC 360p MP4. Preparation and decoding all 289.065 seconds
of audio completed in 9.44 seconds on this Mac. The file is removed on completion.

## Resuming a job

Persist the canonical video ID and destination in the app job record, then resolve
a fresh URL when retrying. `ResumeKey::for_media(&media)` contains only the video
identity, format, audio-track identifier, DRC flag and expected total. YouTube can
reuse the same itag for several dubbed or compressed variants; `resolve_resume`
renews the exact saved variant instead of switching its language. Pass it to `Downloader::download_resumable`.
`resume_status` exposes committed progress. Cancelling its token retains the
checkpoint for a retry; after the future finishes, `discard_resume` explicitly
removes that matching job's partial files. Neither operation overwrites a final
file. Checkpoint digests are validated before truncating an uncommitted tail or
requesting any additional bytes.

## Local video preparation

Run the blocking `prepare_local_video(&path, cancel, progress)` on a worker. The
returned `PreparedLocalVideo` exposes its path, duration and coded dimensions;
keeping the handle alive keeps the temporary file alive. The app's private
preview registry also retains that handle while range requests are in flight.
No source is imported, modified, renamed, or deleted by preparation.

The EBML index bounds metadata at 8 MiB, a block at 64 MiB and indexed frames at
two million. Media payloads are copied in 64 KiB chunks. It skips untrusted seek
offsets and preserves all three audio lacing modes. AVC/HEVC preparation selects
the enabled/default video and audio tracks; it does not copy embedded subtitle,
attachment or additional audio tracks into the temporary MP4. WebM preparation
keeps supported WebM audio/video tracks. Unsupported tracks, their blocks and
their cue entries are replaced with equal-length EBML Void elements, retaining
every surviving Segment/Cluster-relative seek offset and encoded payload. CRC-32
elements in changed ancestors are removed; unchanged inputs retain their exact
Segment bytes. This also handles unknown-length Clusters and mixed-track Cues.
AVC/HEVC can play without sound when no compatible AAC track is present;
`PreparedLocalVideo::warnings()` explains that limitation to the app. An available
compatible AAC track is preferred over an unsupported audio track. This does not
decode AC3 or render embedded subtitles.

Matroska supplies presentation timestamps and decode packet order, but no DTS.
The mux builds a monotonic decode clock without sorting encoded packets. Each
picture retains its presentation interval; CTS and edit lists preserve the
original timeline, including reordered variable-rate pictures. The final
presented picture needs an explicit `BlockDuration` or `DefaultDuration`.
AAC-LC uses its 1,024-sample clock and preserves `CodecDelay`/`DiscardPadding`.
Identical in-band AVC/HEVC parameter sets move into the MP4 sample description;
coded pictures and SEI bytes remain unchanged. Changes of codec configuration,
encryption/container compression of the selected video, malformed supported audio
or timestamps, and non-unit deprecated track timestamp scales return explicit
errors. HEVC and VP9 playback still requires the system decoder for that profile.

`cargo run -p yt-media --example local_video_probe -- source.mkv [new-output.mp4]`
prepares a file and optionally copies the result to a destination that must not
already exist. Unit tests cover reordered and variable-rate timelines, AAC
padding/lacing, duplicate parameter sets, display crop/aspect, cancellation,
truncation and owned temporary-file cleanup.

On 2026-11-08, integration probes used the public W3C
[Sintel MP4](https://media.w3.org/2010/05/sintel/trailer.mp4) and
[WebM](https://media.w3.org/2010/05/sintel/trailer.webm). Rust generated Matroska
fixtures from the MP4 without changing its encoded media. All 1,253 AVC pictures,
2,435 AAC packets and 4,986,880 decoded audio samples were identical after remux.
The WebM Segment retained all 3,521 packets. A five-second HEVC fixture generated
with the macOS encoder retained 120 pictures, 237 AAC packets and its exact
five-second presentation interval. A second Sintel fixture varies picture
cadence while retaining real B pictures; all presentation times survive within
the original millisecond timestamp precision. The ignored tests use
`YTU_LOCAL_VIDEO_FIXTURE`, `YTU_LOCAL_WEBM_FIXTURE` and `YTU_LOCAL_HEVC_FIXTURE`;
ordinary tests require neither downloads nor system video decoding.

Additional macOS AVFoundation decodes produced 1,253 SHA-256-identical AVC
images, 120 identical HEVC images, and 1,253 identical images from the variable
cadence fixture. Maximum presentation timestamp differences were 0.334 ms for
AVC/HEVC and 0.501 ms for the variable-cadence timeline. This checks actual image
decoding in addition to MP4 tables;
it does not establish support for every codec profile on every Windows machine.

## Build dependencies and limitations

The default desktop build needs a C/C++ compiler and CMake for bundled native
dependencies, including QuickJS, libopus and SoundTouch; users do not need those tools to
run the packaged app. No credentials or browser cookies are read automatically.
Authenticated access depends on YouTube accepting the explicitly supplied cookies;
private-account access has not been verified against a real account. The explicit
browser reader supports Firefox SQLite, Safari binarycookies on macOS, and
Chromium-family profiles through macOS Keychain or the current Windows user's
DPAPI key. Chromium Windows App-Bound `v20` cookies are rejected with an actionable
error; the app never elevates, injects into a browser, or bypasses that protection.
Tests use synthetic profiles and keys only, including domain/partition/expiry
filtering and parser corruption checks. No real browser account was read for
validation. The adapted Safari parser's MIT license is in `LICENSE-UNJAR`.
Interactive sign-in/PO-token challenges and live recording are not implemented.
Format conversion lives in `yt-audio`. Playback of video codecs such
as AV1/VP9 depends on the operating system's video decoder; the downloaded source
codec is reported instead of silently pretending every system supports it.

AI processing is a separate optional build. See [model and native runtime
instructions](models/README.md). A real Rust inference probe passed on this Mac;
this does not establish Windows execution. The additional CALIDAD DSP chain and
its numerical reference tests live in [`yt-quality`](../yt-quality/README.md).
