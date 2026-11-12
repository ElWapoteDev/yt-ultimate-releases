# Browse

How the user moves around the music: the sidebar, library pages (Recientes, Artistas, Álbumes, Canciones,
Favoritos), the playlists listed in the sidebar, Radio and Novedades, and the detail pages for artists, albums and
made-for-you collections, with a back stack.

## Files

- `crates/yt-app/src/state.rs`: `Page` (every page, `Page::ALL` in sidebar order), `AppState::detail` (detail stack)
  and `AppState::selected_playlist`.
- `crates/yt-app/src/navigation.rs`, `crates/yt-app/src/navigation.css`: `navigate`, `open_detail`, `go_back`,
  `open_playlist`, `open_artist`; `ArtistRef`, `AlbumRef`, `Detail`; the shared track-menu entries «Crear
  estación», «Ir al artista», «Ir al álbum» and «Ver créditos» (`browse_entries`).
- `crates/yt-app/src/app.rs`: the sidebar (`Sidebar`, `NavButton`, `SidebarPlaylists`) and the page hosts; a detail
  view renders above the hidden page hosts.
- `crates/yt-app/src/detail_ui.rs`: dispatches a `Detail` to its page.
- `crates/yt-app/src/cards.rs`, `crates/yt-app/src/cards.css`: shared building blocks — `Shelf`, `CoverCard`,
  `GeneratedCover` (animated generated artwork), `DetailHero`, `cover_style`/`cover_seed`. The stylesheet is included
  once at the root.
- `crates/yt-app/src/library_pages.rs`: Recientes, Artistas and Álbumes pages and `TrackCard`.
- `crates/yt-app/src/artist_ui.rs`, `crates/yt-app/src/artist_online.rs`, `crates/yt-app/src/artist_ui.css`: the
  artist page and its online sections; the stylesheet (concerts, biography, online track lists) is included once at
  the root.
- `crates/yt-app/src/album_ui.rs`: the album page (library and online releases).
- `crates/yt-app/src/concerts_ui.rs`: upcoming concerts on the artist page; the zone they are matched against is the
  Region system ([Region.md](Region.md)).
- `crates/yt-app/src/credits_ui.rs`, `crates/yt-app/src/credits_ui.css`: song credits and their dialog.
- `crates/yt-app/src/catalog.rs`: the catalog controller (artist pages, albums, credits, new releases) over
  `crates/yt-app/src/provider_cache.rs`.
- `crates/yt-app/src/following.rs`: following artists, optimistic with rollback.
- `crates/yt-media/src/catalog.rs`, `crates/yt-media/src/credits.rs`: YouTube Music pages and credits (the `yt-media`
  README owns what each request returns and what YouTube does not provide).
- `crates/yt-core/src/settings.rs`: `RegionPreferences` (the zone for nearby concerts, detected or typed) and
  `matches`.
- `crates/yt-app/src/radio_ui.rs`, `crates/yt-app/src/radio_ui.css`: Radio page and `StationCard`.
- `crates/yt-app/src/new_releases_ui.rs`: Novedades.
- `crates/yt-core/src/library_index.rs`: `library_artists`, `library_albums` and `library_album` (grouping by tags,
  case- and spacing-insensitive).
- `crates/yt-app/src/theme.rs`: `use_artwork_tokens`, the artwork colors of a detail hero (see Theme.md).
- `crates/yt-core/src/followed_artists.rs`: followed artists by YouTube channel id.
- `crates/yt-theme/src/generated.rs`: `generated_palette(seed)`, the colors of generated covers.

## Navigation

- Sidebar order follows a music library: Inicio, Novedades, Radio, Buscar; «Biblioteca»: Recientes, Artistas,
  Álbumes, Canciones, Favoritos (Stems with advanced tools); «Playlists»: every playlist by name, and a heading
  button that opens all playlists; Descargas and Ajustes at the bottom.
- Choosing a page closes every detail view. Detail views stack up to 24 deep; the hero's back button pops one.
  Opening the view already on top does nothing.
- Page hosts stay mounted while hidden so each page keeps its state (scroll position, filters).

## Generated covers

Collections and stations have no artwork of their own. `GeneratedCover` draws three blurred color fields that drift
slowly (transform-only animation, disabled with reduced motion) over a gradient. Colors come from
`yt_theme::generated_palette(cover_seed(key))`: a stable hue per key, OKLCH lightness and chroma chosen so white
titles keep large-text contrast (≥ 3:1). The same key always looks the same.

## Artist and album pages

- **Artist.** `artist_profile` loads the YouTube Music page of the artist's channel; an artist known only by name is
  matched to a channel (`find_artist`, remembered for a week) while online recommendations are on. The hero shows the
  artist's wide banner (or their photo), audience, library counts, «Reproducir» (popular songs, else the library),
  «Aleatorio», «Estación» and «Seguir». Below: «Populares», concerts, «Álbumes», «Sencillos y EP», «Vídeos»,
  «Artistas similares», «Acerca de» (the biography, shortened with «Más»), and the artist's music in the library.
- **Album.** A release with a playlist id loads `album_details`: kind and year, numbered tracks with their durations,
  «Estación», «Ver artista», the album's description and «Más de <artista>» from the artist's cached page. A library
  album lists its tracks from the tags.
- **Concerts.** YouTube lists concerts only for some artists and regions (on the watch page of a popular song, with
  Bandsintown ticket links). Events whose venue or place names the user's city (or country when no city was given,
  `RegionPreferences::matches`, whole words ignoring accents and case) come first with «Cerca de ti». Without events
  the page offers a web search for concerts near the user and, when no place is known, a shortcut to Ajustes →
  «Conciertos cerca de ti» (and to the system's location settings when location access was refused). Showing the
  section starts the session's zone detection, which finds the city from the device's location with the system's
  permission unless the user turned it off to type their city ([Region.md](Region.md)); while it runs the section
  says «Buscando tu zona…».
- **The real artist and album of a song.** Online artist names are shown without channel decorations («X - Topic»,
  «XVEVO») everywhere (`track_from_result`, `track_from_media`). «Ir al artista» (song menus, the player bar and the
  full-screen view) and «Ir al álbum» ask YouTube Music who it credits for the song and which release it belongs to
  (`song_context`, cached for 30 days), so they open the real artist page and the album even for uploads of a «Topic»
  channel. A music video is linked to no release, so its album is the release of its audio version: the catalog is
  searched for its credited artist and song name and only a song with the same title (`title_key`) is accepted.
  Library songs open their tagged album; a song that has none says so.
- **Credits.** «Ver créditos» in every song menu and «Créditos» in the full-screen view open `CreditsDialog`: roles and
  names, label, release date and distributor. Roles read in Spanish through `ROLE_LABELS`, which covers library tags,
  the credits page and every role of 88 sampled art-track descriptions (instruments and engineering included); roles
  match ignoring case, accents, spaces and punctuation, so the descriptions' run-together roles («Recordingarranger»)
  read too, roles that share a Spanish name are merged into one row (`credit_rows`), and an unknown role keeps its
  English text. Library files read their tags (`AppCore::track_credits`). Online songs read YouTube's auto-generated
  description and, for art tracks only, their YouTube Music credits page (`YoutubeClient::credits`): its performers
  («Interpretación»), writers («Autoría») and producers come first, since descriptions often omit the first two, then
  the description's roles; a page name the description already credits as composer, lyricist or performer is not
  repeated. Credits are cached for 30 days under `catalog:credits-v2:<video>`, a key renamed so copies cached before
  the credits page are fetched again. Music videos and untagged files have none, and the dialog says so.
- **Novedades.** Recent official uploads of the followed artists and then the most listened ones (up to eight, each
  cached for 6 hours): «Lo nuevo de tus artistas», alternating artists, one shelf per artist, and the followed artists.

Catalog responses are cached in the provider cache (artist pages 24 h, albums 7 days, credits 30 days, new releases
6 h) and every cached copy is validated again when read back (`validate()` of each `yt-media` type); a page that cannot
be refreshed shows what was cached. Every list rendered as keyed rows holds each id once:
`catalog::unique_ids` keeps the first item of each id in search results, artist pages, albums, Novedades (where two
artists can list one collaboration) and Home shelves, because a repeated key aborts the whole interface (Dioxus
asserts unique keyed siblings).

## Artwork colors and motion

A detail hero with real artwork takes the artwork's own colors: `use_artwork_tokens` resolves the palette of the
hero's image (a library cover or an `https` thumbnail) off the UI thread through the theme's shared palette cache and
sets `--cover-1..3` (glow) and the accent tokens (action buttons) on the hero only. The rest of the page keeps the
playing song's theme. Greyscale artwork, unreadable images, and «Color dinámico» turned off leave the tokens unset,
and the hero falls back to the playing accent. A hero without artwork uses its generated cover colors.

Real artwork moves like motion artwork, with transforms only: the hero cover slowly breathes inside its frame, the
glow drifts as its own unfiltered layer (the blurred backdrop stays still, because animating a large blur costs every
frame on modest GPUs), and cover cards zoom slightly on hover. Reduced motion turns all of it off.

## Trust boundary

Detail references carry names typed by the provider or tags; channel ids are kept only when `is_channel_id`
validates them. Pages render text through RSX (escaped) and images through `Thumb`, which accepts only application
protocols and `https`. Ticket and search links open only through `native::open_url` (`http`/`https` without
credentials); ticket links come from `yt-media`, which unwraps YouTube's redirect and keeps only the allowed ticket
host, and concert searches are built with encoded query parameters. Online artist names are shown without channel
decorations («X - Topic», «XVEVO») through `yt_core::clean_artist`.

## Tests

`crates/yt-app/src/navigation.rs` (stack behavior), `crates/yt-app/src/app.rs` (sidebar active state for every page),
`crates/yt-app/src/cards.rs` (stable cover styles), `crates/yt-app/src/theme.rs` (surface tokens),
`crates/yt-app/src/concerts_ui.rs` (nearby first, encoded searches), `crates/yt-app/src/artist_online.rs` (release
labels, biography previews), `crates/yt-app/src/artist_ui.rs`, `crates/yt-app/src/credits_ui.rs` (Spanish roles,
spelling variants, merged rows), `crates/yt-app/src/new_releases_ui.rs` (artist order, alternating mix),
`crates/yt-core/src/settings.rs` (region
matching), the `yt-media` catalog and credits parsers on recorded fixtures (credits-page titles of every served
language), `crates/yt-core/src/library_index.rs` (grouping and album lookup),
`crates/yt-core/src/followed_artists.rs`, `crates/yt-theme/src/generated.rs` (contrast for 512 seeds).
