# yt-lyrics

Pure lyrics catalog matching and karaoke timing for YT Ultimate. It turns what the application knows about the song
that plays into catalog queries, chooses the catalog record that belongs to the recording, and times the karaoke fill
of the line being sung. It performs no file, network, or database access, never reads the clock (positions and elapsed
time are parameters), and returns equal output for equal input. Lyrics parsing, validation and storage live in
`yt-core`; the LRCLIB client lives in `yt-media`; `crates/yt-app/src/lyrics.rs` orchestrates them
(`docs/systems/Lyrics.md`).

## API

| Item | Contract |
|---|---|
| `SongMetadata` | Title, artist, album, duration (0 when unknown) and whether the names come from an online upload (`upload`: a YouTube title and channel, including library files downloaded from YouTube) rather than curated tags. |
| `queries(&SongMetadata) -> Vec<LyricsQuery>` | At most `MAX_QUERIES` (2) validated queries, best first, never two with the same title and artist keys. Packaging such as «(Official Video)» and featured artists are removed (`yt_core::song_title`). An upload «Performer - Song» asks for «Song» by the performer, then by the channel (« - Topic», «VEVO» removed); uploads never send the album. Curated tags keep their title and album, and a « - Live» / « - Remastered 2011» suffix is asked for again without it. Empty when nothing names a song. |
| `choose(&LyricsQuery, &[LyricsCandidate], &MatchConfig) -> Option<Chosen>` | The record that plausibly is the same recording, or `None`. The title must match (`title_key` equal: 3, or one containing the other when the shorter key has at least 4 characters: 1). The artist adds 2 when equal, 0 when contained and −2 when different (a channel can be a label). A duration within 3 / 10 / 45 s adds 3 / 2 / 1; beyond 45 s synced lyrics are refused (their timing cannot belong to the recording) and only plain lyrics within 120 s or an instrumental mark remain. Synced lyrics add 2. The minimum score is 3. Word timing weighs less than any score difference (`MatchConfig` documents it): among records with the best score, synced lyrics with word timing win and remaining ties keep catalog order, so a closer recording keeps its line timing over word timing that may be shifted against the recording. Returns `Chosen::{Synced, Plain, Instrumental}` borrowing the candidate. |
| `active_line(&Lyrics, position_ms) -> Option<usize>` | The first line of the latest start at or before `position_ms` (lines sharing a start, such as a translation under its original, are sung together). `None` before the first line, for plain lyrics and for a non-finite position. O(log n). |
| `line_segments(&Lyrics, index, &KaraokeConfig) -> Vec<Segment>` | How a line fills: one segment per timed word (ending at the next word, the marked line end, or a paced estimate), otherwise the whole line from its start to the next line's start, capped at `max_ms_per_char` (180 ms) per character with a `min_fill_ms` (400 ms) floor, so an instrumental break never stretches the fill. Empty for plain lyrics, empty lines and indexes out of range. |
| `Segment::schedule(now_ms, rate) -> (delay_ms, length_ms)` | The CSS animation of a segment seen at lyrics time `now_ms` at playback `rate` (clamped to 0.25–4×): a negative delay once the segment started. |
| `SyncClock` | Follows a fill scheduled from one position sample. `observe(position_ms, rate, wall_ms, running)` returns `true` and re-anchors when the sample disagrees with the expected position by more than `TOLERANCE_MS` (350 ms), the rate changed or the position is not finite: a seek, a rate change or drift between the media and animation clocks. |

## Trust boundary

Inputs are typed values that `yt-core` already validated: `LyricsQuery` (bounded, control-free text, finite duration),
`LyricsCandidate` (parsed from catalog records) and `Lyrics` (bounded, ordered lines). Matching only selects among
candidates and timing only reads lines; neither creates identities or text.

## Performance

Matching is linear in the at most 20 candidates of a search (one key computation each); it runs once per lookup, off
the UI thread's hot path. `active_line` is a binary search run once per 250 ms playback poll sample; segments are
computed once per line when its fill mounts. No allocation happens per frame: the WebView animates the fill.

## Verification

`cargo test -p yt-lyrics` covers query derivation for VEVO, label and Topic uploads, tag suffixes and empty titles;
choosing the closest synced recording, rejecting other songs and distant recordings, channel artists, unknown
durations and instrumental records; word timing deciding only between equally good records; the active line with shared starts; paced fills that hold through breaks; timed
words with a marked end; schedules at other rates; and the clock's seek, pause, drift and rate handling.
