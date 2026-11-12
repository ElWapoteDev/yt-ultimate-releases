# Decoder fixtures

`tone.opus` is a generated one-second 440 Hz stereo sine tone at 48 kHz,
encoded as Ogg Opus at 32 kbit/s. It contains no third-party recording.
The fixture tests the bundled native Opus decoder and seeking without an
external program, network connection, or audio output device.

Generated for development with FFmpeg's `sine` filter and `libopus` encoder;
FFmpeg is not a runtime or test dependency.

# Watch-next fixture

`watch_next.json` keeps the exact nesting of a real 2026-11-11 WEB `/next`
response with `playlistId=RD<id>`: three Mix `playlistPanelVideoRenderer`
entries (the first is the selected seed), two related video and one playlist
`lockupViewModel`, a Shorts `reelShelfRenderer` and a continuation item.
Identifiers, titles, channels, thumbnails and tokens were replaced with
placeholders and tracking fields were removed; no third-party metadata remains.

# LRCLIB fixture

`lrclib_search.json` keeps the shape of a real 2026-11-11 `/api/search`
response: `id`, `name`, `trackName`, `artistName`, `albumName`, a float or
integer `duration`, `instrumental`, nullable `plainLyrics` and
`syncedLyrics` (line stamps followed by one space, a final empty stamp),
`lyricsfile` and `hasWordSync`. It holds a synced record, a plain-only
record, an instrumental record, a record without a title, one with a
wrongly typed duration, a word-synced record and a record marked with word
sync whose lyricsfile has another version. The word-synced lyricsfile is
written as LRCLIB writes them: block style, `metadata` with `duration_ms` and
`language`, lines with `end_ms`, words without one and single-quoted when
they end in a space, and a literal `plain` block; its LRC stamps disagree with
its lines, as one live record's did. Every title, name and lyric line is a
placeholder; no third-party lyrics remain.

# Album cover fixture

`album_art_next.json` keeps the exact nesting of the parts of a real
2026-11-11 WEB `/next` response (client 2.20261111) that the album cover
parser reads: the title under the player, the comments panel, and the
`engagement-panel-structured-description` panel with its
`videoTitleHeaderViewModel`, a `videoDescriptionHeaderRenderer` without a
title, the description body, the one-song «Música» `horizontalCardListRenderer`
with a square `videoAttributeViewModel`, and the transcript section. Titles,
names and the cover URL are placeholders; tracking fields were removed.

# Music catalog fixtures

Each file keeps the exact nesting of the parts of a real 2026-11-11 response
(session locale es-419, MX) that the catalog parsers read; menus, tracking,
accessibility and styling fields were removed, at most two thumbnail sizes and
two items per carousel were kept, and identifiers, names, titles, descriptions,
images, venues, ticket links and tokens were replaced with placeholders.
Localized labels («Álbum», «Sencillo», «Canción»), separators, years, clock
durations and generic counts stay, because the parsers depend on their shape.

- `music_search.json`: YouTube Music `/search` (`WEB_REMIX`): an artist
  `musicCardShelfRenderer` with two of its songs, then one
  `musicResponsiveListItemRenderer` each for an artist, an album (play button
  with its `OLAK5uy_` playlist), a song with linked artists, a YouTube Music
  playlist, a music video, a podcast episode, a user playlist with a linked
  owner and a user profile.
- `music_artist.json`: an artist `/browse` page: `musicImmersiveHeaderRenderer`,
  the microformat image, the popular-songs `musicShelfRenderer` and the albums,
  singles and EPs, videos, featured-playlists and related-artists carousels of
  `musicTwoRowItemRenderer` tiles, and the description shelf.
- `music_album_tracks.json`: the `VL<OLAK5uy_…>` playlist page of a box set: two
  art-track rows linking the `MPREb_` release page, and the
  `continuationItemRenderer` of the next 100 rows.
- `music_album_page.json`: the `MPREb_` release page: its
  `musicResponsiveHeaderRenderer` with the play button that names the
  `OLAK5uy_` playlist, the canonical playlist link and one track row.
- `music_watch_topic_track.json`, `music_watch_vevo_video.json` and
  `music_watch_fan_upload.json`: the queue entry of a YouTube Music `/next`
  response (only `videoId` sent) for an art track uploaded by a Topic channel
  (two credited artists, the `MPREb_` album link and the year), an official
  video of a former VEVO channel (two credited artists and counts, no album)
  and a fan lyric video (only its `MUSIC_PAGE_TYPE_USER_CHANNEL` link).
- `music_track_credits.json`: the `MPTC<videoId>` track credits page of an art
  track (`/browse`): the `dismissableDialogRenderer` popup with its
  «Interpretada por», «Escrita por», «Producida por» and «Metadatos musicales
  proporcionados por» sections (one run per name, «\n» runs between names) and
  the track summary YouTube shows above them.
- `watch_ticket_shelf.json`: the `ticketShelfRenderer` of a WEB `/next`
  response: the highlighted event, an upcoming event listed twice, an event
  sold by several vendors (a menu of redirector links) and one whose link points
  to a host outside the vendor allowlist.
- `recent_uploads_search.json`: a WEB `/search` with the upload-date filter: the
  official artist channel alone, a fan channel, a verified non-artist channel, a
  badged look-alike channel, a collaboration whose channels are listed in an
  owner dialog, and a `lockupViewModel` card with the artist icon.
