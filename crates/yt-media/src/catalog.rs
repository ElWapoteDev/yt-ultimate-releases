//! Music catalog: typed search, artist and album pages, upcoming concerts and
//! the newest official uploads of an artist.
//!
//! Catalog pages come from YouTube Music's InnerTube client, which links every
//! item to a typed page (artist, album, playlist) or video (song or music
//! video). Labels are localized, so items are classified by those links and
//! never by label text; only release kinds and years exist solely as labels.
//! Concerts and new uploads come from the regular web client.

use crate::{
    connection::is_youtube_url,
    search::{badge_duration, parse_matching_results},
    youtube::{
        bounded_innertube_json, bounded_music_json, canonical_playlist_url, is_video_id,
        playlist_continuation_token, search_text, thumbnail_url, watch_url, RequestMessages,
        SEARCH_REQUEST,
    },
    CancellationToken, MediaError, Result, SearchResult, YoutubeClient, PLAYLIST_ITEM_LIMIT,
};
use innertube_rs::parser::nodes::misc::text::TextNode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use url::Url;
use yt_core::{contains_words, fold_words};

/// Most entries [`YoutubeClient::catalog_search`] returns per kind.
pub const CATALOG_KIND_LIMIT: usize = 20;
/// Longest accepted name, title or display line, in characters.
const MAX_TEXT_CHARS: usize = 500;
/// Longest accepted artist or album description, in characters.
const MAX_DESCRIPTION_CHARS: usize = 10_000;
/// Longest accepted image or ticket URL, in bytes.
const MAX_URL_BYTES: usize = 2048;
/// Most entries kept per artist-page list; YouTube Music shows about ten.
const ARTIST_LIST_LIMIT: usize = 50;
/// Most upcoming concerts kept per artist.
const EVENT_LIMIT: usize = 50;
/// Most tracks kept per album, and most result pages read to collect them.
const ALBUM_TRACK_LIMIT: usize = PLAYLIST_ITEM_LIMIT;
const ALBUM_PAGE_LIMIT: usize = 5;
/// Most uploads [`YoutubeClient::new_releases`] keeps from its one result page.
const NEW_RELEASE_LIMIT: usize = 20;
/// Real items sit about ten levels deep; deeper input is not a catalog page.
const MAX_DEPTH: usize = 32;

/// Channel ids are `UC` followed by 22 URL-safe characters.
const CHANNEL_ID_PREFIX: &str = "UC";
const CHANNEL_ID_LENGTH: usize = 24;
/// Album releases are playlists with this prefix; their pages use the other.
const ALBUM_PLAYLIST_PREFIX: &str = "OLAK5uy_";
const ALBUM_BROWSE_PREFIX: &str = "MPREb_";
/// Upper bound on release page ids; observed ones are 17 characters.
const MAX_RELEASE_ID_LENGTH: usize = 64;
/// Most credited artists kept per song; bylines name a handful.
const CREDITED_ARTIST_LIMIT: usize = 20;
/// Browse ids of playlist pages are the playlist id behind this prefix.
const PLAYLIST_BROWSE_PREFIX: &str = "VL";

const CARD: &str = "musicCardShelfRenderer";
/// The queue entry a YouTube Music watch-next response gives for each video.
const QUEUE_ENTRY: &str = "playlistPanelVideoRenderer";
const ROW: &str = "musicResponsiveListItemRenderer";
const TILE: &str = "musicTwoRowItemRenderer";
const DESCRIPTION_SHELF: &str = "musicDescriptionShelfRenderer";
const ALBUM_HEADER: &str = "musicResponsiveHeaderRenderer";
const ARTIST_PAGE: &str = "MUSIC_PAGE_TYPE_ARTIST";
const ALBUM_PAGE: &str = "MUSIC_PAGE_TYPE_ALBUM";
const PLAYLIST_PAGE: &str = "MUSIC_PAGE_TYPE_PLAYLIST";
/// Uploaders that are not artists, e.g. the channel behind a user video.
const UPLOADER_PAGE: &str = "MUSIC_PAGE_TYPE_USER_CHANNEL";
/// Art tracks: the audio of a released recording.
const SONG_VIDEO_TYPE: &str = "MUSIC_VIDEO_TYPE_ATV";
const EPISODE_VIDEO_TYPE: &str = "MUSIC_VIDEO_TYPE_PODCAST_EPISODE";
/// Rows of tracks that cannot be played are greyed out.
const GREYED_OUT: &str = "MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT";
const PAGE_TYPE: &str =
    "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType";
const VIDEO_TYPE: &str =
    "/watchEndpointMusicSupportedConfigs/watchEndpointMusicConfig/musicVideoType";
const PLAY_BUTTON: &str =
    "/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint";
const THUMBNAILS: &str = "/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails";
/// Where a release page names its playlist: the header's play button, or the
/// page's canonical link.
const RELEASE_PLAY_BUTTON: &str =
    "/musicPlayButtonRenderer/playNavigationEndpoint/watchPlaylistEndpoint/playlistId";
const RELEASE_CANONICAL: &str = "/microformat/microformatDataRenderer/urlCanonical";
const TILE_THUMBNAILS: &str = "/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails";
/// Separates the parts of a subtitle, e.g. «Álbum • Artist • 2023».
const PART_SEPARATOR: &str = "•";
/// YouTube Music search filter that returns only artists.
const ARTIST_SEARCH_FILTER: &str = "EgWKAQIgAWoKEAkQBRAKEAMQBA%3D%3D";
/// Release labels of the languages this application serves (Spanish,
/// English, Portuguese, French, Italian, German), compared folded.
const ALBUM_LABELS: [&str; 1] = ["album"];
const SINGLE_LABELS: [&str; 3] = ["single", "sencillo", "singolo"];
const EP_LABELS: [&str; 1] = ["ep"];

/// Web search filter: uploaded within the last year, videos only. The month
/// window rarely reaches an official upload past the fan copies (checked
/// 2026-11-11), and YouTube ignores the upload-date sort order.
const RECENT_UPLOADS_FILTER: &str = "EgQIBRAB";
/// Badge of official artist channels in classic cards, and its icon in
/// view-model cards.
const ARTIST_BADGE_STYLE: &str = "BADGE_STYLE_TYPE_VERIFIED_ARTIST";
const ARTIST_BADGE_ICON: &str = "AUDIO_BADGE";
const LOCKUP_CHANNEL: &str =
    "/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows/0/metadataParts/0/text";
const ICON_NAME: &str = "/element/type/imageType/image/sources/0/clientResource/imageName";
/// Collaborations list their channels in a dialog behind the owner line.
const COLLABORATORS: &str = "/runs/0/navigationEndpoint/showDialogCommand/panelLoadingStrategy/inlineContent/dialogViewModel/customContent/listViewModel/listItems";

const WATCH_RESULTS: &str = "/contents/twoColumnWatchNextResults/results/results/contents";
const TICKET_SHELF: &str = "ticketShelfRenderer";
const TICKET_EVENT: &str = "ticketEventRenderer";
/// Ticket buttons open YouTube's redirector; the destination is this parameter.
const REDIRECT_PATH: &str = "/redirect";
const REDIRECT_TARGET: &str = "q";
/// Ticket vendors YouTube's concert shelf links to. Every link observed on
/// 2026-11-11 (50 events of nine artists) pointed to Bandsintown.
const TICKET_HOSTS: [&str; 1] = ["bandsintown.com"];
/// The shelf title names the highlighted event's place after its label:
/// «Evento más cercano - San Antonio, TX» or «Nearest event · Egypt».
const SHELF_TITLE_SEPARATORS: [&str; 2] = [" - ", " · "];
/// Separates the place line of an event: «location · venue».
const PLACE_SEPARATOR: &str = " · ";
/// Joins the date fields when no subtitle carries them: «22 may, Sáb, 18:00».
const DATE_FIELD_SEPARATOR: &str = ", ";

const ARTIST_REQUEST: RequestMessages = RequestMessages {
    too_large: "La página del artista supera el límite admitido.",
    invalid: "La página del artista no entregó datos válidos.",
    timeout: "La página del artista tardó demasiado.",
};

const ALBUM_REQUEST: RequestMessages = RequestMessages {
    too_large: "La página del álbum supera el límite admitido.",
    invalid: "La página del álbum no entregó datos válidos.",
    timeout: "La página del álbum tardó demasiado.",
};

const SONG_REQUEST: RequestMessages = RequestMessages {
    too_large: "La página de la canción supera el límite admitido.",
    invalid: "La página de la canción no entregó datos válidos.",
    timeout: "La consulta de la canción tardó demasiado.",
};

const EVENTS_REQUEST: RequestMessages = RequestMessages {
    too_large: "La página de conciertos supera el límite admitido.",
    invalid: "La página de conciertos no entregó datos válidos.",
    timeout: "La consulta de conciertos tardó demasiado.",
};

/// The InnerTube client a catalog request goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// The regular web client of www.youtube.com.
    Web,
    /// YouTube Music's web client, whose items are typed by page and video kind.
    Music,
}

/// One artist as YouTube Music lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistSummary {
    /// Channel id (`UC…`) that [`YoutubeClient::artist`] opens.
    pub id: String,
    pub name: String,
    /// Artist image on a YouTube image host; empty when unknown.
    pub thumbnail: String,
    /// Audience line exactly as YouTube displays it, localized: subscribers on
    /// artist pages, usually the monthly audience in lists; empty when unknown.
    pub subscribers: String,
}

/// What a release is. Only YouTube's localized label tells the kinds apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseKind {
    Album,
    Single,
    Ep,
    /// Playlists, and releases labeled in a language this parser does not know.
    Unknown,
}

/// One album, single, EP or playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumSummary {
    /// Playlist id: `OLAK5uy_…` for releases, opened by
    /// [`YoutubeClient::album`]; any public playlist id for playlists, opened by
    /// [`YoutubeClient::playlist_with_limit`].
    pub id: String,
    pub title: String,
    /// Artists as displayed (YouTube joins several with its own conjunction),
    /// or the playlist owner; may be empty.
    pub artist: String,
    pub year: Option<u16>,
    pub kind: ReleaseKind,
    /// Cover on a YouTube image host; empty when unknown.
    pub thumbnail: String,
    /// Known only when the provider prints it; lists of releases do not.
    pub track_count: Option<u32>,
}

/// Typed results of one catalog search, each list in provider order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogSearch {
    /// Released recordings (art tracks).
    pub songs: Vec<SearchResult>,
    pub artists: Vec<ArtistSummary>,
    pub albums: Vec<AlbumSummary>,
    pub playlists: Vec<AlbumSummary>,
    /// Music videos, live performances and user uploads.
    pub videos: Vec<SearchResult>,
}

/// One upcoming concert of an artist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveEvent {
    /// YouTube's title for the event, or the artist's name when it gives none.
    pub title: String,
    /// Date and time as YouTube prints them, localized: «22 may, Sáb, 18:00».
    pub date_text: String,
    pub venue: String,
    /// City and region; empty when YouTube names only the venue.
    pub location: String,
    /// `https` ticket page on a host of `TICKET_HOSTS`.
    pub url: String,
}

/// An artist page: header, shelves and upcoming concerts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistProfile {
    /// The channel id the page was requested with.
    pub id: String,
    pub name: String,
    /// Biography, often a Wikipedia excerpt with its attribution; may be empty.
    pub description: String,
    /// Subscriber line as displayed, localized; may be empty.
    pub subscribers: String,
    /// Square artist image; YouTube Music derives it from the banner.
    pub avatar: String,
    /// Wide header image.
    pub banner: String,
    pub top_songs: Vec<SearchResult>,
    pub albums: Vec<AlbumSummary>,
    /// Singles and EPs.
    pub singles: Vec<AlbumSummary>,
    pub videos: Vec<SearchResult>,
    pub related: Vec<ArtistSummary>,
    /// Upcoming concerts with ticket links; empty when YouTube lists none.
    pub events: Vec<LiveEvent>,
}

/// Who a song is by and the release it belongs to, as YouTube Music credits it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SongContext {
    /// Credited artists in byline order, each with its artist page id; never
    /// the uploading channel («Artist - Topic», «ArtistVEVO») unless YouTube
    /// links that channel as the artist. Thumbnails and audience lines are empty.
    pub artists: Vec<ArtistSummary>,
    /// The album, single or EP of an art track, with its `OLAK5uy_…` id;
    /// `None` for music videos, which YouTube does not link to a release.
    pub album: Option<AlbumSummary>,
}

/// An album page with its playable tracks in album order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumDetails {
    /// The album's `OLAK5uy_…` playlist id.
    pub id: String,
    pub title: String,
    /// Artists as displayed; several are joined by YouTube's conjunction.
    pub artist: String,
    /// Channel id of the first credited artist, when linked.
    pub artist_id: Option<String>,
    pub year: Option<u16>,
    pub kind: ReleaseKind,
    pub thumbnail: String,
    pub description: String,
    /// Art tracks; greyed-out and duplicate entries are left out.
    pub tracks: Vec<SearchResult>,
}

fn invalid() -> MediaError {
    MediaError::InvalidInput("Los datos del catálogo no son válidos.".into())
}

fn check(valid: bool) -> Result<()> {
    if valid {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// A display line: bounded and without control characters; may be empty.
fn line_ok(text: &str) -> bool {
    text.chars().count() <= MAX_TEXT_CHARS && !text.chars().any(char::is_control)
}

fn named(text: &str) -> bool {
    !text.trim().is_empty() && line_ok(text)
}

/// Descriptions keep their line breaks and nothing else below U+0020.
fn description_ok(text: &str) -> bool {
    text.chars().count() <= MAX_DESCRIPTION_CHARS
        && !text.chars().any(|c| c.is_control() && c != '\n')
}

fn image_url(url: &str) -> bool {
    url.len() <= MAX_URL_BYTES && Url::parse(url).is_ok_and(|url| thumbnail_url(&url))
}

fn image_ok(url: &str) -> bool {
    url.is_empty() || image_url(url)
}

fn url_safe(id: &str) -> bool {
    id.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_channel_id(id: &str) -> bool {
    id.len() == CHANNEL_ID_LENGTH && id.starts_with(CHANNEL_ID_PREFIX) && url_safe(id)
}

fn playlist_id_ok(id: &str) -> bool {
    url_safe(id) && canonical_playlist_url(id).is_ok()
}

fn album_id_ok(id: &str) -> bool {
    id.starts_with(ALBUM_PLAYLIST_PREFIX) && playlist_id_ok(id)
}

/// Release page ids: `MPREb_` and URL-safe characters.
fn release_id_ok(id: &str) -> bool {
    id.starts_with(ALBUM_BROWSE_PREFIX)
        && (ALBUM_BROWSE_PREFIX.len() < id.len() && id.len() <= MAX_RELEASE_ID_LENGTH)
        && url_safe(id)
}

/// Four-digit release years.
fn year_ok(year: Option<u16>) -> bool {
    year.is_none_or(|year| (1000..=9999).contains(&year))
}

fn video_ok(video: &SearchResult) -> bool {
    is_video_id(&video.id)
        && video.url == watch_url(&video.id)
        && named(&video.title)
        && line_ok(&video.channel)
        && image_ok(&video.thumbnail)
}

fn videos_ok(videos: &[SearchResult], limit: usize) -> bool {
    videos.len() <= limit && videos.iter().all(video_ok)
}

/// Ticket pages: `https` on a vendor of `TICKET_HOSTS`, without credentials.
fn ticket_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.as_str().len() <= MAX_URL_BYTES
        && url.host_str().is_some_and(|host| {
            TICKET_HOSTS
                .iter()
                .any(|vendor| host == *vendor || host.ends_with(&format!(".{vendor}")))
        })
}

impl ArtistSummary {
    /// Check the id, text bounds and the image host, e.g. of a cached copy.
    pub fn validate(&self) -> Result<()> {
        check(
            is_channel_id(&self.id)
                && named(&self.name)
                && image_ok(&self.thumbnail)
                && line_ok(&self.subscribers),
        )
    }
}

impl AlbumSummary {
    /// Check the playlist id (`OLAK5uy_…` for album, single and EP kinds),
    /// text bounds, the year and the image host.
    pub fn validate(&self) -> Result<()> {
        let id_ok = match self.kind {
            ReleaseKind::Unknown => playlist_id_ok(&self.id),
            _ => album_id_ok(&self.id),
        };
        check(
            id_ok
                && named(&self.title)
                && line_ok(&self.artist)
                && year_ok(self.year)
                && image_ok(&self.thumbnail),
        )
    }
}

impl CatalogSearch {
    /// Check every entry and that no list exceeds [`CATALOG_KIND_LIMIT`].
    pub fn validate(&self) -> Result<()> {
        check(
            videos_ok(&self.songs, CATALOG_KIND_LIMIT)
                && videos_ok(&self.videos, CATALOG_KIND_LIMIT)
                && self.artists.len() <= CATALOG_KIND_LIMIT
                && self.albums.len() <= CATALOG_KIND_LIMIT
                && self.playlists.len() <= CATALOG_KIND_LIMIT,
        )?;
        self.artists.iter().try_for_each(ArtistSummary::validate)?;
        self.albums
            .iter()
            .chain(&self.playlists)
            .try_for_each(AlbumSummary::validate)
    }
}

impl LiveEvent {
    /// Check text bounds and that the ticket link is `https` on an allowed vendor.
    pub fn validate(&self) -> Result<()> {
        check(
            named(&self.title)
                && named(&self.date_text)
                && named(&self.venue)
                && line_ok(&self.location)
                && Url::parse(&self.url).is_ok_and(|url| ticket_url(&url)),
        )
    }
}

impl ArtistProfile {
    /// Check the id, texts, image hosts, every shelf entry and list bounds.
    pub fn validate(&self) -> Result<()> {
        check(
            is_channel_id(&self.id)
                && named(&self.name)
                && description_ok(&self.description)
                && line_ok(&self.subscribers)
                && image_ok(&self.avatar)
                && image_ok(&self.banner)
                && videos_ok(&self.top_songs, ARTIST_LIST_LIMIT)
                && videos_ok(&self.videos, ARTIST_LIST_LIMIT)
                && self.albums.len() <= ARTIST_LIST_LIMIT
                && self.singles.len() <= ARTIST_LIST_LIMIT
                && self.related.len() <= ARTIST_LIST_LIMIT
                && self.events.len() <= EVENT_LIMIT,
        )?;
        self.albums
            .iter()
            .chain(&self.singles)
            .try_for_each(AlbumSummary::validate)?;
        self.related.iter().try_for_each(ArtistSummary::validate)?;
        self.events.iter().try_for_each(LiveEvent::validate)
    }
}

impl SongContext {
    /// Check every artist, the artist count, and that the album is an
    /// `OLAK5uy_…` release.
    pub fn validate(&self) -> Result<()> {
        check(
            self.artists.len() <= CREDITED_ARTIST_LIMIT
                && self
                    .album
                    .as_ref()
                    .is_none_or(|album| album_id_ok(&album.id)),
        )?;
        self.artists.iter().try_for_each(ArtistSummary::validate)?;
        self.album.iter().try_for_each(AlbumSummary::validate)
    }
}

impl AlbumDetails {
    /// Check the `OLAK5uy_…` id, the artist id, texts, the year, the cover host
    /// and every track.
    pub fn validate(&self) -> Result<()> {
        check(
            album_id_ok(&self.id)
                && named(&self.title)
                && line_ok(&self.artist)
                && self.artist_id.as_deref().is_none_or(is_channel_id)
                && year_ok(self.year)
                && image_ok(&self.thumbnail)
                && description_ok(&self.description)
                && videos_ok(&self.tracks, ALBUM_TRACK_LIMIT),
        )
    }
}

impl YoutubeClient {
    /// Songs, artists, albums, playlists and videos for `query` from one
    /// YouTube Music search, at most `per_kind` (1 to [`CATALOG_KIND_LIMIT`])
    /// of each in provider order. YouTube's first result page holds a handful
    /// per kind. Podcasts and user profiles are left out, ids are validated,
    /// duplicates removed and thumbnails outside YouTube's image hosts cleared.
    /// List rows show compact localized play counts, so `view_count` is 0.
    pub async fn catalog_search(
        &self,
        query: &str,
        per_kind: usize,
        cancel: CancellationToken,
    ) -> Result<CatalogSearch> {
        let query = search_text(query)?;
        if !(1..=CATALOG_KIND_LIMIT).contains(&per_kind) {
            return Err(MediaError::InvalidInput(format!(
                "El número de resultados por tipo debe estar entre 1 y {CATALOG_KIND_LIMIT}."
            )));
        }
        let response = self
            .innertube(
                Service::Music,
                "/search",
                json!({ "query": query }),
                &SEARCH_REQUEST,
                &cancel,
            )
            .await?;
        Ok(parse_search(&response, per_kind))
    }

    /// The YouTube Music page of the artist channel `channel_id` (`UC…`): header,
    /// popular songs, albums, singles and EPs, videos and related artists, as
    /// many as the page shows. A second, best-effort watch-page request for one
    /// of the artist's own songs reads YouTube's concert shelf, so `events` is
    /// empty when YouTube lists no concerts for the artist or that request fails.
    pub async fn artist(
        &self,
        channel_id: &str,
        cancel: CancellationToken,
    ) -> Result<ArtistProfile> {
        if !is_channel_id(channel_id) {
            return Err(MediaError::InvalidInput(
                "El identificador del artista no es válido.".into(),
            ));
        }
        let page = self
            .innertube(
                Service::Music,
                "/browse",
                json!({ "browseId": channel_id }),
                &ARTIST_REQUEST,
                &cancel,
            )
            .await?;
        let mut profile = parse_artist(&page, channel_id).ok_or_else(|| {
            MediaError::Youtube("El artista no está disponible en YouTube Music.".into())
        })?;
        if let Some(video) = event_video(&profile) {
            let request = json!({ "videoId": video });
            profile.events = match self
                .innertube(Service::Web, "/next", request, &EVENTS_REQUEST, &cancel)
                .await
            {
                Ok(response) => parse_events(&response, &profile.name),
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(_) => Vec::new(),
            };
        }
        Ok(profile)
    }

    /// The YouTube Music artist best matching `name`, from one artist-only
    /// search: the first whose name equals `name` ignoring case, accents and
    /// punctuation; else the artist named first in `name` («A y B» gives A);
    /// else the top result when one name contains the other as whole words;
    /// else `None`.
    pub async fn find_artist(
        &self,
        name: &str,
        cancel: CancellationToken,
    ) -> Result<Option<ArtistSummary>> {
        let name = search_text(name)?;
        let payload = json!({ "query": name, "params": ARTIST_SEARCH_FILTER });
        let response = self
            .innertube(Service::Music, "/search", payload, &SEARCH_REQUEST, &cancel)
            .await?;
        Ok(best_artist(
            parse_search(&response, CATALOG_KIND_LIMIT).artists,
            name,
        ))
    }

    /// The album, single or EP `playlist_id` (its `OLAK5uy_…` playlist, bare or
    /// in a playlist link, or its `MPREb_…` release page id) with its playable
    /// art tracks in order, up to 500 across at most five pages, plus its header
    /// (artist, year, kind, cover, description) from the release page. Always
    /// reports the `OLAK5uy_…` id.
    pub async fn album(
        &self,
        playlist_id: &str,
        cancel: CancellationToken,
    ) -> Result<AlbumDetails> {
        let unavailable =
            || MediaError::Youtube("El álbum no está disponible en YouTube Music.".into());
        let input = playlist_id.trim();
        let release_page = if release_id_ok(input) {
            Some(self.release_page(input, &cancel).await?)
        } else {
            None
        };
        let id = match &release_page {
            Some(page) => release_playlist(page).ok_or_else(unavailable)?,
            None => album_playlist(input)?,
        };
        let mut tracks = self.album_tracks(&id, &cancel).await?;
        let header = match release_page {
            Some(page) => page,
            None => {
                let release = tracks.release.take().ok_or_else(unavailable)?;
                self.release_page(&release, &cancel).await?
            }
        };
        let mut album = parse_album_header(&header, &id).ok_or_else(unavailable)?;
        for track in &mut tracks.tracks {
            if track.channel.is_empty() {
                track.channel.clone_from(&album.artist);
            }
        }
        album.tracks = tracks.tracks;
        Ok(album)
    }

    /// The credited artists of `video_id` (an id or a YouTube link) and the
    /// release its recording belongs to, as YouTube Music shows them: one
    /// watch-next request reads the byline's artist and album links, and a
    /// second one reads the release page when there is an album. Music videos
    /// have artists but no album; uploads whose byline links no artist page
    /// (fan copies, non-music videos) return an empty context.
    pub async fn song_context(
        &self,
        video_id: &str,
        cancel: CancellationToken,
    ) -> Result<SongContext> {
        let id = crate::youtube::video_id(video_id)?;
        let response = self
            .innertube(
                Service::Music,
                "/next",
                json!({ "videoId": id }),
                &SONG_REQUEST,
                &cancel,
            )
            .await?;
        let Some(song) = parse_song(&response, &id) else {
            return Ok(SongContext::default());
        };
        let album = match song.release {
            Some(release) => {
                let page = self.release_page(release, &cancel).await?;
                release_summary(&page, song.year)
            }
            None => None,
        };
        Ok(SongContext {
            artists: song.artists,
            album,
        })
    }

    /// Videos uploaded within the last year by the official artist channel of
    /// `artist_name`, from one web search with YouTube's upload-date filter.
    /// Only cards whose channel, or one of whose collaborating channels, carries
    /// the official-artist badge and is named exactly like the artist (ignoring
    /// case and accents) are kept, which leaves out fan, lyric, reaction and
    /// look-alike channels. At most 20, in relevance order, which may include
    /// the artist's non-music uploads.
    pub async fn new_releases(
        &self,
        artist_name: &str,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>> {
        let artist = search_text(artist_name)?;
        let wanted = fold_words(artist);
        let payload = json!({ "query": artist, "params": RECENT_UPLOADS_FILTER });
        let response = self
            .innertube(Service::Web, "/search", payload, &SEARCH_REQUEST, &cancel)
            .await?;
        Ok(parse_matching_results(
            &response,
            NEW_RELEASE_LIMIT,
            |card| official_upload(card, &wanted),
        ))
    }

    /// One bounded InnerTube request through `service` that honours `cancel`;
    /// a cancelled token never starts the network session.
    pub(crate) async fn innertube(
        &self,
        service: Service,
        endpoint: &str,
        payload: Value,
        messages: &RequestMessages,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(MediaError::Cancelled),
            result = async {
                let client = self.client().await?;
                match service {
                    Service::Web => bounded_innertube_json(client, endpoint, payload, messages).await,
                    Service::Music => bounded_music_json(client, endpoint, payload, messages).await,
                }
            } => result,
        }
    }

    /// The YouTube Music page of the release `release` (`MPREb_…`).
    async fn release_page(&self, release: &str, cancel: &CancellationToken) -> Result<Value> {
        let request = json!({ "browseId": release });
        self.innertube(Service::Music, "/browse", request, &ALBUM_REQUEST, cancel)
            .await
    }

    /// The tracks of the album playlist `id`, following its continuations.
    async fn album_tracks(&self, id: &str, cancel: &CancellationToken) -> Result<AlbumTracks> {
        let request = json!({ "browseId": format!("{PLAYLIST_BROWSE_PREFIX}{id}") });
        let mut page = self
            .innertube(Service::Music, "/browse", request, &ALBUM_REQUEST, cancel)
            .await?;
        let mut tracks = AlbumTracks::default();
        let mut tokens = HashSet::new();
        for pages in 1.. {
            tracks.read(&page);
            let Some(token) = playlist_continuation_token(&page) else {
                break;
            };
            if pages >= ALBUM_PAGE_LIMIT
                || tracks.tracks.len() >= ALBUM_TRACK_LIMIT
                || !tokens.insert(token.clone())
            {
                break;
            }
            let request = json!({ "continuation": token });
            page = self
                .innertube(Service::Music, "/browse", request, &ALBUM_REQUEST, cancel)
                .await?;
        }
        Ok(tracks)
    }
}

/// The `OLAK5uy_…` id of a bare album id or a playlist link.
fn album_playlist(input: &str) -> Result<String> {
    let invalid = || MediaError::InvalidInput("Usa un álbum válido de YouTube Music.".into());
    let url = canonical_playlist_url(input).map_err(|_| invalid())?;
    let id = Url::parse(&url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "list")
                .map(|(_, id)| id.into_owned())
        })
        .filter(|id| id.starts_with(ALBUM_PLAYLIST_PREFIX))
        .ok_or_else(invalid)?;
    Ok(id)
}

/// Where an item, or one run of its text, leads.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Link<'a> {
    /// A typed page: artist (`UC…`), album (`MPREb_…`) or playlist (`VL…`).
    Page { id: &'a str, kind: &'a str },
    /// A video; its music type tells songs from videos and episodes.
    Video { id: &'a str, kind: Option<&'a str> },
}

fn link(endpoint: Option<&Value>) -> Option<Link<'_>> {
    let endpoint = endpoint?;
    if let Some(browse) = endpoint.get("browseEndpoint") {
        return Some(Link::Page {
            id: browse.get("browseId")?.as_str()?,
            kind: browse
                .pointer(PAGE_TYPE)
                .and_then(Value::as_str)
                .unwrap_or_default(),
        });
    }
    let watch = endpoint.get("watchEndpoint")?;
    Some(Link::Video {
        id: watch.get("videoId")?.as_str()?,
        kind: watch.pointer(VIDEO_TYPE).and_then(Value::as_str),
    })
}

/// One run of a title or subtitle.
pub(crate) struct Run<'a> {
    text: &'a str,
    link: Option<Link<'a>>,
}

pub(crate) fn runs(text: Option<&Value>) -> Vec<Run<'_>> {
    text.and_then(|text| text.get("runs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|run| {
            Some(Run {
                text: run.get("text")?.as_str()?,
                link: link(run.get("navigationEndpoint")),
            })
        })
        .collect()
}

pub(crate) fn joined(runs: &[Run<'_>]) -> String {
    runs.iter().map(|run| run.text).collect()
}

/// Split runs into «•»-separated parts, dropping empty ones.
fn split_parts(runs: Vec<Run<'_>>) -> Vec<Vec<Run<'_>>> {
    let mut parts = vec![Vec::new()];
    for run in runs {
        if run.text.trim() == PART_SEPARATOR {
            parts.push(Vec::new());
        } else if let Some(part) = parts.last_mut() {
            part.push(run);
        }
    }
    parts.retain(|part| !joined(part).trim().is_empty());
    parts
}

/// Trimmed display text, or `None` when empty, oversized or with control characters.
fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    named(text).then(|| text.to_owned())
}

/// Description text: line breaks kept, other control characters dropped,
/// bounded by `MAX_DESCRIPTION_CHARS`.
fn description(text: Option<&Value>) -> String {
    joined(&runs(text))
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_DESCRIPTION_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The widest image on a YouTube image host, or empty.
fn image(thumbnails: Option<&Value>) -> String {
    thumbnails
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|thumbnail| {
            let url = thumbnail.get("url")?.as_str()?;
            let width = thumbnail.get("width").and_then(Value::as_u64).unwrap_or(0);
            image_url(url).then_some((width, url))
        })
        .max_by_key(|(width, _)| *width)
        .map(|(_, url)| url.to_owned())
        .unwrap_or_default()
}

fn year(text: &str) -> Option<u16> {
    let text = text.trim();
    let year = (text.len() == 4 && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten();
    year.filter(|year| year_ok(Some(*year)))
}

fn release_label(text: &str) -> Option<ReleaseKind> {
    let label = fold_words(text);
    let label = label.as_str();
    if ALBUM_LABELS.contains(&label) {
        Some(ReleaseKind::Album)
    } else if SINGLE_LABELS.contains(&label) {
        Some(ReleaseKind::Single)
    } else if EP_LABELS.contains(&label) {
        Some(ReleaseKind::Ep)
    } else {
        None
    }
}

fn clock(text: &str) -> Option<u64> {
    badge_duration(&Value::from(text), 0)
}

/// The fields the catalog reads from a YouTube Music card, list row or tile.
struct Item<'a> {
    title: Vec<Run<'a>>,
    /// Secondary text: every column after the title, split into parts.
    parts: Vec<Vec<Run<'a>>>,
    link: Option<Link<'a>>,
    thumbnails: Option<&'a Value>,
    /// Playlist the play button starts: an album's `OLAK5uy_…` id.
    play_list: Option<&'a str>,
}

fn play_list(overlay: Option<&Value>) -> Option<&str> {
    overlay?
        .pointer(PLAY_BUTTON)?
        .pointer("/watchPlaylistEndpoint/playlistId")?
        .as_str()
}

impl<'a> Item<'a> {
    /// A list row: a title column, detail columns and fixed columns.
    fn row(row: &'a Value) -> Self {
        let column = |key: &'static str, renderer: &'static str| {
            row.get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |column| column.get(renderer)?.get("text"))
        };
        let mut columns = column("flexColumns", "musicResponsiveListItemFlexColumnRenderer");
        let title = runs(columns.next());
        let details = columns.chain(column(
            "fixedColumns",
            "musicResponsiveListItemFixedColumnRenderer",
        ));
        let parts = details
            .flat_map(|text| split_parts(runs(Some(text))))
            .collect();
        // Song and video rows name their video in the play data, the title run
        // or the play button; only the runs carry the music type.
        let button = link(
            row.get("overlay")
                .and_then(|overlay| overlay.pointer(PLAY_BUTTON)),
        );
        let videos: Vec<(&str, Option<&str>)> = title
            .iter()
            .map(|run| run.link)
            .chain([button])
            .flatten()
            .filter_map(|link| match link {
                Link::Video { id, kind } => Some((id, kind)),
                Link::Page { .. } => None,
            })
            .collect();
        let video_id = row
            .pointer("/playlistItemData/videoId")
            .and_then(Value::as_str)
            .or_else(|| videos.first().map(|(id, _)| *id));
        let kind = videos.iter().find_map(|(_, kind)| *kind);
        let link =
            link(row.get("navigationEndpoint")).or(video_id.map(|id| Link::Video { id, kind }));
        Self {
            title,
            parts,
            link,
            thumbnails: row.pointer(THUMBNAILS),
            play_list: play_list(row.get("overlay")),
        }
    }

    /// A carousel tile.
    fn tile(tile: &'a Value) -> Self {
        Self {
            title: runs(tile.get("title")),
            parts: split_parts(runs(tile.get("subtitle"))),
            link: link(tile.get("navigationEndpoint")),
            thumbnails: tile.pointer(TILE_THUMBNAILS),
            play_list: play_list(tile.get("thumbnailOverlay")),
        }
    }

    /// The top-result card of a search.
    fn card(card: &'a Value) -> Self {
        let title = runs(card.get("title"));
        let link = title
            .iter()
            .find_map(|run| run.link)
            .or_else(|| link(card.get("onTap")));
        Self {
            title,
            parts: split_parts(runs(card.get("subtitle"))),
            link,
            thumbnails: card.pointer(THUMBNAILS),
            play_list: play_list(card.get("thumbnailOverlay")),
        }
    }

    fn title(&self) -> Option<String> {
        clean(&joined(&self.title))
    }

    /// The part naming the credited artists or uploader, as displayed.
    fn credited(&self) -> Option<String> {
        self.parts
            .iter()
            .find(|part| part.iter().any(|run| credit_link(run.link).is_some()))
            .and_then(|part| clean(&joined(part)))
    }

    fn year(&self) -> Option<u16> {
        self.parts.iter().find_map(|part| year(&joined(part)))
    }

    fn label(&self) -> Option<ReleaseKind> {
        self.parts
            .iter()
            .find_map(|part| release_label(&joined(part)))
    }

    fn duration(&self) -> u64 {
        self.parts
            .iter()
            .flatten()
            .find_map(|run| clock(run.text))
            .unwrap_or(0)
    }

    /// The release page (`MPREb_…`) a track row links its album column to.
    fn release_page(&self) -> Option<&'a str> {
        self.parts.iter().flatten().find_map(|run| match run.link {
            Some(Link::Page { id, kind }) if kind == ALBUM_PAGE && release_id_ok(id) => Some(id),
            _ => None,
        })
    }
}

/// The channel of a run linking to an artist or uploader page.
fn credit_link(link: Option<Link<'_>>) -> Option<&str> {
    match link? {
        Link::Page { id, kind } if kind == ARTIST_PAGE || kind == UPLOADER_PAGE => Some(id),
        _ => None,
    }
}

fn video(item: &Item<'_>, id: &str, artist: Option<&str>) -> Option<SearchResult> {
    if !is_video_id(id) {
        return None;
    }
    Some(SearchResult {
        url: watch_url(id),
        id: id.to_owned(),
        title: item.title()?,
        thumbnail: image(item.thumbnails),
        duration_seconds: item.duration(),
        channel: item
            .credited()
            .or_else(|| artist.map(str::to_owned))
            .unwrap_or_default(),
        view_count: 0,
    })
}

/// `labeled` rows lead with a type label («Artista»), so their audience line
/// is the last part only when there are two.
fn artist(item: &Item<'_>, id: &str, labeled: bool) -> Option<ArtistSummary> {
    if !is_channel_id(id) {
        return None;
    }
    let subscribers = item
        .parts
        .last()
        .filter(|_| item.parts.len() > usize::from(labeled))
        .and_then(|part| clean(&joined(part)))
        .unwrap_or_default();
    Some(ArtistSummary {
        id: id.to_owned(),
        name: item.title()?,
        thumbnail: image(item.thumbnails),
        subscribers,
    })
}

fn release(item: &Item<'_>, artist: Option<&str>, kind: ReleaseKind) -> Option<AlbumSummary> {
    let id = item.play_list.filter(|id| album_id_ok(id))?;
    Some(AlbumSummary {
        id: id.to_owned(),
        title: item.title()?,
        artist: item
            .credited()
            .or_else(|| artist.map(str::to_owned))
            .unwrap_or_default(),
        year: item.year(),
        kind: item.label().unwrap_or(kind),
        thumbnail: image(item.thumbnails),
        track_count: None,
    })
}

/// Playlist subtitles read «Playlist • Owner • …»; the owner is linked only
/// for some users.
fn playlist(item: &Item<'_>, browse_id: &str) -> Option<AlbumSummary> {
    let id = browse_id
        .strip_prefix(PLAYLIST_BROWSE_PREFIX)
        .filter(|id| playlist_id_ok(id))?;
    let owner = item
        .credited()
        .or_else(|| item.parts.get(1).and_then(|part| clean(&joined(part))));
    Some(AlbumSummary {
        id: id.to_owned(),
        title: item.title()?,
        artist: owner.unwrap_or_default(),
        year: None,
        kind: ReleaseKind::Unknown,
        thumbnail: image(item.thumbnails),
        track_count: None,
    })
}

/// Call `visit` with every card, row and tile below `value` in document
/// order, without descending into them.
fn visit_items<'a>(value: Option<&'a Value>, visit: &mut impl FnMut(&'a str, &'a Value)) {
    fn walk<'a>(value: &'a Value, depth: usize, visit: &mut impl FnMut(&'a str, &'a Value)) {
        if depth > MAX_DEPTH {
            return;
        }
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    if [CARD, ROW, TILE].contains(&key.as_str()) {
                        visit(key, child);
                    } else {
                        walk(child, depth + 1, visit);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, depth + 1, visit);
                }
            }
            _ => {}
        }
    }
    if let Some(value) = value {
        walk(value, 0, visit);
    }
}

/// First value stored under `key` below `value`, depth-first.
pub(crate) fn find_key<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    find_key_where(value, key, |_| true)
}

/// First value stored under `key` below `value`, depth-first, that `keep`
/// accepts.
fn find_key_where<'a>(
    value: Option<&'a Value>,
    key: &str,
    keep: impl Fn(&Value) -> bool,
) -> Option<&'a Value> {
    fn walk<'a>(
        value: &'a Value,
        key: &str,
        keep: &dyn Fn(&Value) -> bool,
        depth: usize,
    ) -> Option<&'a Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        match value {
            Value::Object(object) => object.get(key).filter(|found| keep(found)).or_else(|| {
                object
                    .values()
                    .find_map(|child| walk(child, key, keep, depth + 1))
            }),
            Value::Array(items) => items
                .iter()
                .find_map(|item| walk(item, key, keep, depth + 1)),
            _ => None,
        }
    }
    walk(value?, key, &keep, 0)
}

/// One classified search entry.
enum Entry {
    Song(SearchResult),
    Video(SearchResult),
    Artist(ArtistSummary),
    Album(AlbumSummary),
    Playlist(AlbumSummary),
}

/// Search rows lead with a type label; `artist` fills rows of an artist's
/// top-result card, which do not repeat the artist.
fn entry(item: &Item<'_>, artist_name: Option<&str>) -> Option<Entry> {
    match item.link? {
        Link::Video { kind, .. } if kind == Some(EPISODE_VIDEO_TYPE) => None,
        Link::Video { id, kind } => {
            let found = video(item, id, artist_name)?;
            Some(if kind == Some(SONG_VIDEO_TYPE) {
                Entry::Song(found)
            } else {
                Entry::Video(found)
            })
        }
        Link::Page { id, kind } if kind == ARTIST_PAGE => artist(item, id, true).map(Entry::Artist),
        Link::Page { kind, .. } if kind == ALBUM_PAGE => {
            release(item, artist_name, ReleaseKind::Unknown).map(Entry::Album)
        }
        Link::Page { id, kind } if kind == PLAYLIST_PAGE => playlist(item, id).map(Entry::Playlist),
        Link::Page { .. } => None,
    }
}

fn parse_search(value: &Value, per_kind: usize) -> CatalogSearch {
    let mut search = CatalogSearch::default();
    let mut seen = HashSet::new();
    let mut add = |entry: Option<Entry>| {
        let Some(entry) = entry else {
            return;
        };
        let id = match &entry {
            Entry::Song(video) | Entry::Video(video) => &video.id,
            Entry::Artist(artist) => &artist.id,
            Entry::Album(album) | Entry::Playlist(album) => &album.id,
        };
        if !seen.insert(id.clone()) {
            return;
        }
        match entry {
            Entry::Song(video) => search.songs.push(video),
            Entry::Video(video) => search.videos.push(video),
            Entry::Artist(artist) => search.artists.push(artist),
            Entry::Album(album) => search.albums.push(album),
            Entry::Playlist(album) => search.playlists.push(album),
        }
    };
    visit_items(value.get("contents"), &mut |kind, node| match kind {
        CARD => {
            let card = Item::card(node);
            let artist_name =
                matches!(card.link, Some(Link::Page { kind, .. }) if kind == ARTIST_PAGE)
                    .then(|| card.title())
                    .flatten();
            add(entry(&card, None));
            for row in node
                .get("contents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|content| content.get(ROW))
            {
                add(entry(&Item::row(row), artist_name.as_deref()));
            }
        }
        ROW => add(entry(&Item::row(node), None)),
        _ => add(entry(&Item::tile(node), None)),
    });
    search.songs.truncate(per_kind);
    search.videos.truncate(per_kind);
    search.artists.truncate(per_kind);
    search.albums.truncate(per_kind);
    search.playlists.truncate(per_kind);
    search
}

/// Read an artist page. Rows are the popular songs; tiles are releases,
/// videos and related artists. Releases that credit another artist belong to
/// «featured on» shelves and are skipped. The albums shelf prints only the
/// year, while singles and EPs carry their localized kind.
fn parse_artist(value: &Value, id: &str) -> Option<ArtistProfile> {
    let header = value.get("header")?.as_object()?.values().next()?;
    let name = clean(&joined(&runs(header.get("title"))))?;
    let biography = find_key(value.get("contents"), DESCRIPTION_SHELF)
        .map(|shelf| description(shelf.get("description")))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| description(header.get("description")));
    let mut profile = ArtistProfile {
        id: id.to_owned(),
        description: biography,
        subscribers: clean(&joined(&runs(header.pointer(
            "/subscriptionButton/subscribeButtonRenderer/longSubscriberCountText",
        ))))
        .unwrap_or_default(),
        avatar: image(value.pointer("/microformat/microformatDataRenderer/thumbnail/thumbnails")),
        banner: image(header.pointer(THUMBNAILS)),
        name,
        top_songs: Vec::new(),
        albums: Vec::new(),
        singles: Vec::new(),
        videos: Vec::new(),
        related: Vec::new(),
        events: Vec::new(),
    };
    let mut seen = HashSet::from([id.to_owned()]);
    let artist_name = profile.name.clone();
    visit_items(value.get("contents"), &mut |kind, node| {
        let row = kind == ROW;
        let item = if row {
            Item::row(node)
        } else {
            Item::tile(node)
        };
        match item.link {
            Some(Link::Video { kind, .. }) if kind == Some(EPISODE_VIDEO_TYPE) => {}
            Some(Link::Video { id, .. }) => {
                if let Some(found) = video(&item, id, Some(&artist_name)) {
                    if seen.insert(found.id.clone()) {
                        if row {
                            profile.top_songs.push(found);
                        } else {
                            profile.videos.push(found);
                        }
                    }
                }
            }
            Some(Link::Page { id, kind }) if kind == ARTIST_PAGE && !row => {
                if let Some(found) = artist(&item, id, false) {
                    if seen.insert(found.id.clone()) {
                        profile.related.push(found);
                    }
                }
            }
            Some(Link::Page { kind, .. }) if kind == ALBUM_PAGE && item.credited().is_none() => {
                let year_only = item.parts.len() == 1 && item.year().is_some();
                let shelf_kind = if year_only {
                    ReleaseKind::Album
                } else {
                    ReleaseKind::Unknown
                };
                if let Some(found) = release(&item, Some(&artist_name), shelf_kind) {
                    if seen.insert(found.id.clone()) {
                        if found.kind == ReleaseKind::Album {
                            profile.albums.push(found);
                        } else {
                            profile.singles.push(found);
                        }
                    }
                }
            }
            _ => {}
        }
    });
    profile.top_songs.truncate(ARTIST_LIST_LIMIT);
    profile.albums.truncate(ARTIST_LIST_LIMIT);
    profile.singles.truncate(ARTIST_LIST_LIMIT);
    profile.videos.truncate(ARTIST_LIST_LIMIT);
    profile.related.truncate(ARTIST_LIST_LIMIT);
    Some(profile)
}

/// The video whose watch page lists the artist's concerts: preferably a
/// popular song by the artist alone, since a duet's page may list the guest's.
fn event_video(profile: &ArtistProfile) -> Option<&str> {
    let artist = fold_words(&profile.name);
    profile
        .top_songs
        .iter()
        .find(|song| fold_words(&song.channel) == artist)
        .or_else(|| profile.top_songs.first())
        .or_else(|| profile.videos.first())
        .map(|video| video.id.as_str())
}

/// Tracks of an album's playlist pages and the release page they link to.
#[derive(Default)]
struct AlbumTracks {
    tracks: Vec<SearchResult>,
    release: Option<String>,
    seen: HashSet<String>,
}

impl AlbumTracks {
    fn read(&mut self, page: &Value) {
        visit_items(Some(page), &mut |kind, node| {
            if kind != ROW
                || self.tracks.len() >= ALBUM_TRACK_LIMIT
                || node
                    .get("musicItemRendererDisplayPolicy")
                    .and_then(Value::as_str)
                    == Some(GREYED_OUT)
            {
                return;
            }
            let item = Item::row(node);
            if self.release.is_none() {
                self.release = item.release_page().map(str::to_owned);
            }
            if let Some(Link::Video { id, .. }) = item.link {
                if let Some(track) = video(&item, id, None) {
                    if self.seen.insert(track.id.clone()) {
                        self.tracks.push(track);
                    }
                }
            }
        });
    }
}

/// What a song's byline in a YouTube Music watch-next response links to.
struct SongByline<'a> {
    artists: Vec<ArtistSummary>,
    /// The release page (`MPREb_…`) of an art track's album.
    release: Option<&'a str>,
    year: Option<u16>,
}

/// The byline of `video_id`'s queue entry, or `None` when it links no artist
/// page: fan copies and other uploads only link their channel as a user, and
/// podcast episodes are not songs.
fn parse_song<'a>(response: &'a Value, video_id: &str) -> Option<SongByline<'a>> {
    let entry = find_key_where(response.get("contents"), QUEUE_ENTRY, |entry| {
        entry.get("videoId").and_then(Value::as_str) == Some(video_id)
    })?;
    let episode = entry
        .pointer("/navigationEndpoint/watchEndpoint")
        .and_then(|watch| watch.pointer(VIDEO_TYPE))
        .and_then(Value::as_str)
        == Some(EPISODE_VIDEO_TYPE);
    let byline = split_parts(runs(entry.get("longBylineText")));
    let mut seen = HashSet::new();
    let artists: Vec<ArtistSummary> = byline
        .iter()
        .flatten()
        .filter_map(|run| match run.link {
            Some(Link::Page { id, kind }) if kind == ARTIST_PAGE && is_channel_id(id) => {
                Some(ArtistSummary {
                    id: id.to_owned(),
                    name: clean(run.text)?,
                    thumbnail: String::new(),
                    subscribers: String::new(),
                })
            }
            _ => None,
        })
        .filter(|artist| seen.insert(artist.id.clone()))
        .take(CREDITED_ARTIST_LIMIT)
        .collect();
    if episode || artists.is_empty() {
        return None;
    }
    let release = byline.iter().flatten().find_map(|run| match run.link {
        Some(Link::Page { id, kind }) if kind == ALBUM_PAGE && release_id_ok(id) => Some(id),
        _ => None,
    });
    Some(SongByline {
        artists,
        release,
        year: byline.iter().find_map(|part| year(&joined(part))),
    })
}

/// The `OLAK5uy_…` playlist of a release page: its play button, or its
/// canonical link.
fn release_playlist(page: &Value) -> Option<String> {
    let buttons = find_key(page.get("contents"), ALBUM_HEADER)
        .and_then(|header| header.get("buttons"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|button| button.pointer(RELEASE_PLAY_BUTTON)?.as_str())
        .map(str::to_owned);
    let canonical = page
        .pointer(RELEASE_CANONICAL)
        .and_then(Value::as_str)
        .and_then(|link| album_playlist(link).ok());
    buttons.chain(canonical).find(|id| album_id_ok(id))
}

/// A release page as a summary; `year` from the song's byline fills a header
/// that prints none.
fn release_summary(page: &Value, year: Option<u16>) -> Option<AlbumSummary> {
    let id = release_playlist(page)?;
    let album = parse_album_header(page, &id)?;
    Some(AlbumSummary {
        id: album.id,
        title: album.title,
        artist: album.artist,
        year: album.year.or(year),
        kind: album.kind,
        thumbnail: album.thumbnail,
        track_count: None,
    })
}

/// The header of a release page (`MPREb_…`); tracks are added by the caller.
fn parse_album_header(value: &Value, id: &str) -> Option<AlbumDetails> {
    let header = find_key(value.get("contents"), ALBUM_HEADER)?;
    let strapline = runs(header.get("straplineTextOne"));
    let subtitle = Item {
        title: Vec::new(),
        parts: split_parts(runs(header.get("subtitle"))),
        link: None,
        thumbnails: None,
        play_list: None,
    };
    Some(AlbumDetails {
        id: id.to_owned(),
        title: clean(&joined(&runs(header.get("title"))))?,
        artist: clean(&joined(&strapline)).unwrap_or_default(),
        artist_id: strapline
            .iter()
            .find_map(|run| credit_link(run.link))
            .filter(|id| is_channel_id(id))
            .map(str::to_owned),
        year: subtitle.year(),
        kind: subtitle.label().unwrap_or(ReleaseKind::Unknown),
        thumbnail: image(header.pointer(THUMBNAILS)),
        description: description(
            header.pointer("/description/musicDescriptionShelfRenderer/description"),
        ),
        tracks: Vec::new(),
    })
}

/// The best artist for `name` among search candidates in provider order: an
/// exact folded match, else the artist named first in `name` («A y B» gives
/// A), else the top candidate when one name contains the other as words.
fn best_artist(candidates: Vec<ArtistSummary>, name: &str) -> Option<ArtistSummary> {
    let wanted = fold_words(name);
    if wanted.is_empty() {
        return None;
    }
    let names: Vec<String> = candidates
        .iter()
        .map(|artist| fold_words(&artist.name))
        .collect();
    let leads =
        |found: &str| !found.is_empty() && format!("{wanted} ").starts_with(&format!("{found} "));
    let chosen = names
        .iter()
        .position(|found| *found == wanted)
        .or_else(|| names.iter().position(|found| leads(found)))
        .or_else(|| {
            candidates
                .first()
                .filter(|top| contains_words(name, &top.name) || contains_words(&top.name, name))
                .map(|_| 0)
        })?;
    candidates.into_iter().nth(chosen)
}

/// Whether a web search card is an upload of the official channel named
/// exactly `artist` (folded), as its only channel or as one of its badged
/// collaborators.
fn official_upload(card: &Value, artist: &str) -> bool {
    let named = |name: &str| fold_words(name) == artist;
    if let Some(owner) = card.get("ownerText").or_else(|| card.get("longBylineText")) {
        let badged = card
            .get("ownerBadges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|badge| {
                badge
                    .pointer("/metadataBadgeRenderer/style")
                    .and_then(Value::as_str)
                    == Some(ARTIST_BADGE_STYLE)
            });
        if badged && TextNode::from_value(owner).is_some_and(|owner| named(&owner.text)) {
            return true;
        }
        return owner
            .pointer(COLLABORATORS)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.pointer("/listItemViewModel/title"))
            .any(|title| {
                artist_icon(title)
                    && title
                        .get("content")
                        .and_then(Value::as_str)
                        .is_some_and(named)
            });
    }
    card.pointer(LOCKUP_CHANNEL).is_some_and(|channel| {
        artist_icon(channel)
            && channel
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(named)
    })
}

/// Whether a view-model text carries the official-artist icon.
fn artist_icon(text: &Value) -> bool {
    text.get("attachmentRuns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|run| run.pointer(ICON_NAME).and_then(Value::as_str) == Some(ARTIST_BADGE_ICON))
}

/// Upcoming concerts of `artist` from a watch-next response's ticket shelf.
/// The first event is the highlighted one: the only one with a title, which
/// names the artist, and the one whose place the shelf title holds. A shelf
/// whose title names someone else (a guest of the video) yields nothing.
/// Vendors list some concerts twice; the first listing of a date and place wins.
fn parse_events(response: &Value, artist: &str) -> Vec<LiveEvent> {
    let Some(shelf) = response
        .pointer(WATCH_RESULTS)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|item| item.get(TICKET_SHELF))
    else {
        return Vec::new();
    };
    let events: Vec<&Value> = shelf
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|event| event.get(TICKET_EVENT))
        .collect();
    let about_artist = events
        .first()
        .and_then(|event| event.get("title"))
        .and_then(Value::as_str)
        .is_none_or(|title| contains_words(title, artist));
    if !about_artist {
        return Vec::new();
    }
    let shelf_place = shelf
        .get("title")
        .and_then(Value::as_str)
        .and_then(shelf_location);
    let mut seen = HashSet::new();
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            live_event(event, artist, (index == 0).then_some(shelf_place).flatten())
        })
        .filter(|event| {
            seen.insert((
                event.date_text.clone(),
                event.venue.clone(),
                event.location.clone(),
            ))
        })
        .take(EVENT_LIMIT)
        .collect()
}

fn shelf_location(title: &str) -> Option<&str> {
    SHELF_TITLE_SEPARATORS
        .iter()
        .filter_map(|separator| title.split_once(separator))
        .min_by_key(|(label, _)| label.len())
        .map(|(_, place)| place.trim())
        .filter(|place| !place.is_empty())
}

/// One event. One subtitle starts with the date; the other is the place:
/// «location · venue», or «weekday, time · venue» for the highlighted event.
fn live_event(event: &Value, artist: &str, shelf_place: Option<&str>) -> Option<LiveEvent> {
    let field = |key: &str| {
        event
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let date = field("timeDate")?;
    let subtitles: Vec<&str> = ["subtitle1", "subtitle2"]
        .into_iter()
        .filter_map(field)
        .collect();
    let dated = subtitles.iter().position(|text| text.starts_with(date));
    let date_text = match dated {
        Some(index) => subtitles[index].to_owned(),
        None => [Some(date), field("timeWeekday"), field("timeTime")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(DATE_FIELD_SEPARATOR),
    };
    let place = subtitles
        .iter()
        .enumerate()
        .find(|(index, _)| Some(*index) != dated)
        .map(|(_, text)| *text)?;
    let place: Vec<&str> = place
        .split(PLACE_SEPARATOR)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let time = field("timeTime");
    let location = place
        .first()
        .filter(|_| place.len() > 1)
        .filter(|first| !time.is_some_and(|time| first.contains(time)))
        .copied()
        .or(shelf_place)
        .unwrap_or_default();
    let event = LiveEvent {
        title: field("title").unwrap_or(artist).to_owned(),
        date_text,
        venue: (*place.last()?).to_owned(),
        location: location.to_owned(),
        url: ticket_link(event.get("buttonCommand")?)?,
    };
    event.validate().is_ok().then_some(event)
}

/// The vendor page behind a ticket button: its URL endpoint, or the first
/// offer of its menu when several vendors sell tickets. YouTube's redirector
/// is unwrapped so the link reaches the vendor directly.
fn ticket_link(command: &Value) -> Option<String> {
    let link = find_key(Some(command), "urlEndpoint")?
        .get("url")?
        .as_str()?;
    let mut url = Url::parse(link).ok()?;
    if is_youtube_url(&url) && url.path() == REDIRECT_PATH {
        let target = url
            .query_pairs()
            .find(|(key, _)| key == REDIRECT_TARGET)
            .map(|(_, target)| target.into_owned())?;
        url = Url::parse(&target).ok()?;
    }
    ticket_url(&url).then(|| url.to_string())
}

/// Replace every value stored under `key` below `value` with `with`: the
/// malformed variants of recorded responses that parser tests feed in.
#[cfg(test)]
pub(crate) fn poison(value: &mut Value, key: &str, with: &Value) {
    match value {
        Value::Object(object) => {
            for (name, child) in object.iter_mut() {
                if name == key {
                    *child = with.clone();
                } else {
                    poison(child, key, with);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| poison(item, key, with)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIST: &str = "UCfixtureChannel00000001";
    const ALBUM: &str = "OLAK5uy_fixtureAlbum000000000000000000001";

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn ids(videos: &[SearchResult]) -> Vec<&str> {
        videos.iter().map(|video| video.id.as_str()).collect()
    }

    fn titles(albums: &[AlbumSummary]) -> Vec<&str> {
        albums.iter().map(|album| album.title.as_str()).collect()
    }

    /// A carousel tile linking to `link` with `subtitle` runs.
    fn tile(link: Value, subtitle: Value, play: Option<&str>) -> Value {
        let mut tile = json!({
            "title": {"runs": [{"text": "Release"}]},
            "subtitle": {"runs": subtitle},
            "navigationEndpoint": link,
            "thumbnailRenderer": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [
                {"url": "https://yt3.googleusercontent.com/cover=w226-h226", "width": 226},
                {"url": "https://yt3.googleusercontent.com/cover=w544-h544", "width": 544},
                {"url": "https://images.example/huge.jpg", "width": 4096}
            ]}}}
        });
        if let Some(play) = play {
            tile["thumbnailOverlay"] = json!({"musicItemThumbnailOverlayRenderer": {"content": {
                "musicPlayButtonRenderer": {"playNavigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": play}}}
            }}});
        }
        json!({ TILE: tile })
    }

    fn page(id: &str, kind: &str) -> Value {
        json!({"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {
            "browseEndpointContextMusicConfig": {"pageType": kind}
        }}})
    }

    fn artist_page(tiles: Vec<Value>) -> Value {
        json!({
            "header": {"musicImmersiveHeaderRenderer": {"title": {"runs": [{"text": "Artist"}]}}},
            "contents": {"sectionListRenderer": {"contents": [
                {"musicCarouselShelfRenderer": {"contents": tiles}}
            ]}}
        })
    }

    #[test]
    fn real_search_shape_yields_typed_lists_without_podcasts_or_profiles() {
        let search = parse_search(&fixture("music_search.json"), CATALOG_KIND_LIMIT);
        assert!(search.validate().is_ok());
        // The artist card's songs do not repeat the artist; the card names it.
        assert_eq!(
            ids(&search.songs),
            ["fixture0001", "fixture0002", "fixture0003"]
        );
        let channels: Vec<_> = search.songs.iter().map(|s| s.channel.as_str()).collect();
        assert_eq!(channels, ["Artist 1", "Artist 1", "Artist 3"]);
        assert_eq!(search.songs[0].duration_seconds, 285);
        assert_eq!(search.songs[2].duration_seconds, 0);
        assert!(search.songs.iter().all(|song| song.view_count == 0));
        let artists: Vec<_> = search
            .artists
            .iter()
            .map(|a| (a.id.as_str(), a.name.as_str()))
            .collect();
        assert_eq!(
            artists,
            [
                (ARTIST, "Artist 1"),
                ("UCfixtureChannel00000002", "Artist 2")
            ]
        );
        assert_eq!(
            search.artists[0].subscribers,
            "Público mensual: 81.8\u{a0}M usuarios"
        );
        assert_eq!(
            search.artists[0].thumbnail,
            "https://lh3.googleusercontent.com/fixtureImage001=w120-h120-p-l90-rj"
        );
        let album = &search.albums[0];
        assert_eq!(search.albums.len(), 1);
        assert_eq!(
            (
                album.id.as_str(),
                album.title.as_str(),
                album.artist.as_str()
            ),
            (ALBUM, "Album 2", "Artist 1")
        );
        assert_eq!((album.year, album.kind), (Some(2023), ReleaseKind::Album));
        assert!(album
            .thumbnail
            .ends_with("fixtureImage005=w544-h544-l90-rj"));
        let playlists: Vec<_> = search
            .playlists
            .iter()
            .map(|p| (p.id.as_str(), p.artist.as_str(), p.kind))
            .collect();
        assert_eq!(
            playlists,
            [
                (
                    "RDCLAK5uy_fixtureMix000000000000000000001",
                    "Title 7",
                    ReleaseKind::Unknown
                ),
                (
                    "PLfixturePlaylist00000000000000002",
                    "Artist 4",
                    ReleaseKind::Unknown
                )
            ]
        );
        assert_eq!(ids(&search.videos), ["fixture0004"]);
        assert_eq!(search.videos[0].channel, "Artist 1 y Artist 2");
        let limited = parse_search(&fixture("music_search.json"), 1);
        assert_eq!(ids(&limited.songs), ["fixture0001"]);
        assert_eq!((limited.artists.len(), limited.playlists.len()), (1, 1));
    }

    #[test]
    fn real_artist_page_shape_fills_every_shelf_and_skips_featured_playlists() {
        let profile = parse_artist(&fixture("music_artist.json"), ARTIST).unwrap();
        assert!(profile.validate().is_ok());
        assert_eq!(profile.name, "Artist 1");
        assert_eq!(profile.subscribers, "9.51\u{a0}M de suscriptores");
        assert_eq!(
            profile.description,
            "Full biography of Artist 1.\nSecond paragraph."
        );
        assert!(profile
            .avatar
            .ends_with("fixtureImage001=w544-h544-p-l90-rj"));
        assert!(profile
            .banner
            .ends_with("fixtureImage001=w2880-h1200-p-l90-rj"));
        assert_eq!(
            ids(&profile.top_songs),
            ["fixture0001", "fixture0006", "fixture0007"]
        );
        assert_eq!(profile.top_songs[0].channel, "Artist 1 y Artist 6");
        assert_eq!(titles(&profile.albums), ["Album 6", "Album 5"]);
        assert!(profile
            .albums
            .iter()
            .all(|a| a.kind == ReleaseKind::Album && a.artist == "Artist 1"));
        assert_eq!(profile.albums[0].year, Some(2026));
        assert_eq!(titles(&profile.singles), ["Album 9", "Album 11"]);
        assert!(profile
            .singles
            .iter()
            .all(|s| s.kind == ReleaseKind::Single));
        assert_eq!(ids(&profile.videos), ["fixture0008", "fixture0009"]);
        let related: Vec<_> = profile.related.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(related, ["Artist 7", "Artist 8"]);
        assert_eq!(
            profile.related[0].subscribers,
            "Público mensual: 11.3\u{a0}M usuarios"
        );
        assert!(profile.events.is_empty());
        assert_eq!(event_video(&profile), Some("fixture0007"));
        assert!(parse_artist(&json!({"contents": {}}), ARTIST).is_none());
    }

    #[test]
    fn release_kinds_come_from_labels_or_the_year_only_albums_shelf() {
        let album_page = |id: &str| page(id, ALBUM_PAGE);
        let tiles = vec![
            tile(
                album_page("MPREb_albumYear"),
                json!([{"text": "2021"}]),
                Some("OLAK5uy_yearOnlyAlbum0000000000000000001"),
            ),
            tile(
                album_page("MPREb_single"),
                json!([{"text": "Sencillo"}, {"text": " • "}, {"text": "2026"}]),
                Some("OLAK5uy_singleRelease000000000000000001"),
            ),
            tile(
                album_page("MPREb_ep"),
                json!([{"text": "EP"}, {"text": " • "}, {"text": "2025"}]),
                Some("OLAK5uy_extendedPlay0000000000000000001"),
            ),
            tile(
                album_page("MPREb_unknown"),
                json!([{"text": "Singel"}, {"text": " • "}, {"text": "2024"}]),
                Some("OLAK5uy_unknownLabel0000000000000000001"),
            ),
            tile(
                album_page("MPREb_featured"),
                json!([{"text": "Álbum"}, {"text": " • "},
                {"text": "Other", "navigationEndpoint": page("UCotherArtist0000000000A", ARTIST_PAGE)}]),
                Some("OLAK5uy_featuredAlbum000000000000000001"),
            ),
            tile(
                album_page("MPREb_noPlaylist"),
                json!([{"text": "2020"}]),
                None,
            ),
            tile(
                album_page("MPREb_badPlaylist"),
                json!([{"text": "2020"}]),
                Some("PLnotAnAlbumPlaylist000000001"),
            ),
        ];
        let profile = parse_artist(&artist_page(tiles), ARTIST).unwrap();
        let kinds = |albums: &[AlbumSummary]| {
            albums
                .iter()
                .map(|a| (a.id.clone(), a.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&profile.albums),
            [(
                "OLAK5uy_yearOnlyAlbum0000000000000000001".into(),
                ReleaseKind::Album
            )]
        );
        assert_eq!(
            kinds(&profile.singles),
            [
                (
                    "OLAK5uy_singleRelease000000000000000001".into(),
                    ReleaseKind::Single
                ),
                (
                    "OLAK5uy_extendedPlay0000000000000000001".into(),
                    ReleaseKind::Ep
                ),
                (
                    "OLAK5uy_unknownLabel0000000000000000001".into(),
                    ReleaseKind::Unknown
                )
            ]
        );
        // The widest image on a trusted host wins; the larger foreign one is ignored.
        assert_eq!(
            profile.albums[0].thumbnail,
            "https://yt3.googleusercontent.com/cover=w544-h544"
        );
        assert!(profile.validate().is_ok());
        for (label, kind) in [
            ("Álbum", ReleaseKind::Album),
            ("ALBUM", ReleaseKind::Album),
            ("Single", ReleaseKind::Single),
            ("Singolo", ReleaseKind::Single),
            ("ep", ReleaseKind::Ep),
        ] {
            assert_eq!(release_label(label), Some(kind), "{label}");
        }
        assert_eq!(release_label("Playlist"), None);
        assert_eq!(
            (year("2023"), year("0999"), year("20233"), year("20a3")),
            (Some(2023), None, None, None)
        );
    }

    #[test]
    fn real_album_pages_give_tracks_release_link_continuation_and_header() {
        let page = fixture("music_album_tracks.json");
        let mut tracks = AlbumTracks::default();
        tracks.read(&page);
        assert_eq!(ids(&tracks.tracks), ["fixture0010", "fixture0011"]);
        assert_eq!(tracks.tracks[0].title, "Title 25");
        assert_eq!(tracks.tracks[0].channel, "Artist 9");
        assert_eq!(tracks.tracks[0].duration_seconds, 305);
        assert_eq!(tracks.release.as_deref(), Some("MPREb_fixt0000007"));
        assert_eq!(
            playlist_continuation_token(&page).as_deref(),
            Some("fixture-continuation-token")
        );
        let mut album = parse_album_header(&fixture("music_album_page.json"), ALBUM).unwrap();
        assert_eq!(
            (
                album.title.as_str(),
                album.artist.as_str(),
                album.artist_id.as_deref()
            ),
            ("Album 2", "Artist 1", Some(ARTIST))
        );
        assert_eq!((album.year, album.kind), (Some(2023), ReleaseKind::Album));
        assert!(album
            .thumbnail
            .ends_with("fixtureImage005=w544-h544-l90-rj"));
        assert_eq!(
            album.description,
            "Album 2 is the third studio album of Artist 1."
        );
        album.tracks = tracks.tracks;
        assert!(album.validate().is_ok());
        assert!(parse_album_header(&json!({"contents": []}), ALBUM).is_none());
    }

    #[test]
    fn greyed_out_duplicate_and_unidentified_album_rows_are_skipped() {
        let row = |id: Option<&str>, policy: Option<&str>| {
            let mut row = json!({
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Track"}]}}},
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Album",
                        "navigationEndpoint": page("MPREb_release01", ALBUM_PAGE)}]}}}
                ],
                "fixedColumns": [{"musicResponsiveListItemFixedColumnRenderer": {"text": {"runs": [{"text": "3:05"}]}}}]
            });
            if let Some(id) = id {
                row["playlistItemData"] = json!({"videoId": id});
            }
            if let Some(policy) = policy {
                row["musicItemRendererDisplayPolicy"] = json!(policy);
            }
            json!({ ROW: row })
        };
        let page = json!({"contents": [
            row(Some("trackOne111"), None),
            row(Some("greyedOut11"), Some(GREYED_OUT)),
            row(Some("trackOne111"), None),
            row(None, None),
            row(Some("short"), None),
            row(Some("trackTwo222"), None)
        ]});
        let mut tracks = AlbumTracks::default();
        tracks.read(&page);
        assert_eq!(ids(&tracks.tracks), ["trackOne111", "trackTwo222"]);
        assert!(tracks
            .tracks
            .iter()
            .all(|t| t.duration_seconds == 185 && t.channel.is_empty()));
        assert_eq!(tracks.release.as_deref(), Some("MPREb_release01"));
    }

    #[test]
    fn real_ticket_shelf_gives_deduplicated_events_with_direct_vendor_links() {
        let shelf = fixture("watch_ticket_shelf.json");
        let events = parse_events(&shelf, "Artist 1");
        let summary: Vec<_> = events
            .iter()
            .map(|e| {
                (
                    e.title.as_str(),
                    e.date_text.as_str(),
                    e.venue.as_str(),
                    e.location.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "Artist 1 - Entradas para eventos",
                    "22 may, Sáb, 18:00",
                    "Venue 1",
                    "City 1, Region"
                ),
                (
                    "Artist 1",
                    "19 nov, Jue, 20:00",
                    "Venue 2",
                    "City 2, Region"
                ),
                (
                    "Artist 1 - Entradas para eventos",
                    "28 nov, Sáb, 14:00",
                    "Venue 3",
                    ""
                )
            ]
        );
        let urls: Vec<_> = events.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://www.bandsintown.com/t/1000001?ticket_id=1",
                "https://www.bandsintown.com/t/1000002?ticket_id=1",
                "https://www.bandsintown.com/t/1000004?ticket_id=1"
            ]
        );
        assert!(events.iter().all(|event| event.validate().is_ok()));
        // A shelf about a guest of the video, or no shelf at all, gives nothing.
        assert!(parse_events(&shelf, "Somebody Else").is_empty());
        assert!(parse_events(&json!({"contents": {}}), "Artist 1").is_empty());
    }

    #[test]
    fn event_dates_and_places_follow_both_subtitle_layouts() {
        let event = json!({
            "timeDate": "3 mar", "timeWeekday": "Mar", "timeTime": "21:00",
            "subtitle1": "Venue Only",
            "buttonCommand": {"urlEndpoint": {"url": "https://www.bandsintown.com/t/9"}}
        });
        let parsed = live_event(&event, "Artist", Some("Shelf City")).unwrap();
        assert_eq!(parsed.date_text, "3 mar, Mar, 21:00");
        assert_eq!(
            (parsed.venue.as_str(), parsed.location.as_str()),
            ("Venue Only", "Shelf City")
        );
        assert_eq!(parsed.title, "Artist");
        assert_eq!(shelf_location("Nearest event · Egypt"), Some("Egypt"));
        assert_eq!(
            shelf_location("Evento más cercano - Abu Dhabi - Emiratos Árabes Unidos"),
            Some("Abu Dhabi - Emiratos Árabes Unidos")
        );
        assert_eq!(shelf_location("Upcoming events"), None);
        let mut without_date = event.clone();
        without_date.as_object_mut().unwrap().remove("timeDate");
        assert!(live_event(&without_date, "Artist", None).is_none());
        let mut without_link = event;
        without_link["buttonCommand"] = json!({});
        assert!(live_event(&without_link, "Artist", None).is_none());
    }

    #[test]
    fn ticket_links_are_unwrapped_and_limited_to_vendor_hosts() {
        let link = |url: &str| ticket_link(&json!({"urlEndpoint": {"url": url}}));
        let redirect = |target: &str| {
            format!(
                "https://www.youtube.com/redirect?event=ticket_shelf&q={}",
                url::form_urlencoded::byte_serialize(target.as_bytes()).collect::<String>()
            )
        };
        assert_eq!(
            link(&redirect("https://www.bandsintown.com/t/1?ticket_id=2")).as_deref(),
            Some("https://www.bandsintown.com/t/1?ticket_id=2")
        );
        assert_eq!(
            link("https://bandsintown.com/t/3").as_deref(),
            Some("https://bandsintown.com/t/3")
        );
        for bad in [
            redirect("http://www.bandsintown.com/t/1"),
            redirect("https://bandsintown.com.evil.test/t/1"),
            redirect("https://evilbandsintown.com/t/1"),
            redirect("https://user:pass@www.bandsintown.com/t/1"),
            redirect("https://www.bandsintown.com:8443/t/1"),
            redirect("javascript:alert(1)"),
            "https://www.youtube.com/redirect?event=ticket_shelf".into(),
            "https://youtube.com.evil.test/redirect?q=https://www.bandsintown.com/t/1".into(),
            "not a url".into(),
        ] {
            assert_eq!(link(&bad), None, "{bad}");
        }
    }

    #[test]
    fn real_recent_uploads_keep_only_the_official_channel_and_its_collaborations() {
        let search = fixture("recent_uploads_search.json");
        let wanted = fold_words("ARTIST 1");
        let uploads = parse_matching_results(&search, NEW_RELEASE_LIMIT, |card| {
            official_upload(card, &wanted)
        });
        // Fan, verified non-artist and look-alike («Artist 1 Fans») channels are dropped.
        assert_eq!(ids(&uploads), ["fixture0013", "fixture0017", "fixture0018"]);
        assert_eq!(uploads[1].channel, "Artist 1 y Guest 1");
        // The guest is an official artist too, so the collaboration is also theirs
        // unless its entry in the dialog lacks the badge.
        let guest = fold_words("Guest 1");
        let as_guest = |search: &Value| {
            let uploads = parse_matching_results(search, NEW_RELEASE_LIMIT, |card| {
                official_upload(card, &guest)
            });
            uploads
                .into_iter()
                .map(|upload| upload.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(as_guest(&search), ["fixture0017"]);
        let mut unbadged = search.clone();
        let collaborators = unbadged
            .pointer_mut("/contents/twoColumnSearchResultsRenderer/primaryContents/sectionListRenderer/contents/0/itemSectionRenderer/contents/4/videoRenderer/ownerText")
            .and_then(|owner| owner.pointer_mut(COLLABORATORS))
            .unwrap();
        collaborators[1]["listItemViewModel"]["title"]
            .as_object_mut()
            .unwrap()
            .remove("attachmentRuns");
        assert!(as_guest(&unbadged).is_empty());
    }

    #[test]
    fn best_artist_prefers_exact_folded_names_then_the_first_named_artist() {
        let artist = |name: &str| ArtistSummary {
            id: ARTIST.into(),
            name: name.into(),
            thumbnail: String::new(),
            subscribers: String::new(),
        };
        let names = |found: Option<ArtistSummary>| found.map(|artist| artist.name);
        let candidates = || {
            vec![
                artist("ROSALÍA"),
                artist("Rosalia Paranoia"),
                artist("Rosalia"),
            ]
        };
        assert_eq!(
            names(best_artist(candidates(), "rosalia")).as_deref(),
            Some("ROSALÍA")
        );
        let duet = || vec![artist("Guest"), artist("Main Artist")];
        assert_eq!(
            names(best_artist(duet(), "Main Artist y Guest")).as_deref(),
            Some("Main Artist")
        );
        assert_eq!(
            names(best_artist(vec![artist("Band")], "The Band - Topic")).as_deref(),
            Some("Band")
        );
        assert_eq!(
            names(best_artist(vec![artist("Bad Bunny")], "bad")).as_deref(),
            Some("Bad Bunny")
        );
        assert_eq!(best_artist(vec![artist("Someone")], "Nobody Here"), None);
        assert_eq!(best_artist(Vec::new(), "Anyone"), None);
        assert_eq!(best_artist(vec![artist("Anyone")], "¡¿?!"), None);
    }

    #[test]
    fn validation_rejects_bad_ids_hosts_texts_and_bounds() {
        let artist = ArtistSummary {
            id: ARTIST.into(),
            name: "Artist".into(),
            thumbnail: "https://yt3.ggpht.com/a=s88".into(),
            subscribers: "1 M".into(),
        };
        assert!(artist.validate().is_ok());
        for bad in [
            ArtistSummary {
                id: "UCshort".into(),
                ..artist.clone()
            },
            ArtistSummary {
                id: "UC/fixtureChannel0000001".into(),
                ..artist.clone()
            },
            ArtistSummary {
                name: " ".into(),
                ..artist.clone()
            },
            ArtistSummary {
                name: "bad\u{7}".into(),
                ..artist.clone()
            },
            ArtistSummary {
                thumbnail: "https://images.example/a.jpg".into(),
                ..artist.clone()
            },
            ArtistSummary {
                thumbnail: "http://yt3.ggpht.com/a".into(),
                ..artist.clone()
            },
            ArtistSummary {
                thumbnail: "https://lh3.googleusercontent.com.evil.test/a".into(),
                ..artist.clone()
            },
            ArtistSummary {
                subscribers: "x".repeat(MAX_TEXT_CHARS + 1),
                ..artist.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let album = AlbumSummary {
            id: ALBUM.into(),
            title: "Album".into(),
            artist: String::new(),
            year: Some(2020),
            kind: ReleaseKind::Album,
            thumbnail: String::new(),
            track_count: None,
        };
        assert!(album.validate().is_ok());
        let playlist = AlbumSummary {
            id: "PLfixturePlaylist00000000000000002".into(),
            kind: ReleaseKind::Unknown,
            ..album.clone()
        };
        assert!(playlist.validate().is_ok());
        for bad in [
            AlbumSummary {
                id: "PLfixturePlaylist00000000000000002".into(),
                ..album.clone()
            },
            AlbumSummary {
                id: "https://www.youtube.com/playlist?list=OLAK5uy_x".into(),
                ..album.clone()
            },
            AlbumSummary {
                year: Some(10_000),
                ..album.clone()
            },
            AlbumSummary {
                title: String::new(),
                ..album.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let event = LiveEvent {
            title: "Artist".into(),
            date_text: "1 dic".into(),
            venue: "Venue".into(),
            location: String::new(),
            url: "https://www.bandsintown.com/t/1".into(),
        };
        assert!(event.validate().is_ok());
        for url in [
            "http://www.bandsintown.com/t/1",
            "https://tickets.example/1",
            "https://www.youtube.com/redirect?q=x",
        ] {
            assert!(
                LiveEvent {
                    url: url.into(),
                    ..event.clone()
                }
                .validate()
                .is_err(),
                "{url}"
            );
        }
        assert!(LiveEvent {
            venue: String::new(),
            ..event.clone()
        }
        .validate()
        .is_err());
        let crowded = CatalogSearch {
            artists: vec![artist.clone(); CATALOG_KIND_LIMIT + 1],
            ..CatalogSearch::default()
        };
        assert!(crowded.validate().is_err());
        let video = SearchResult {
            id: "fixture0001".into(),
            url: watch_url("fixture0001"),
            title: "Song".into(),
            thumbnail: String::new(),
            duration_seconds: 0,
            channel: String::new(),
            view_count: 0,
        };
        let details = AlbumDetails {
            id: ALBUM.into(),
            title: "Album".into(),
            artist: String::new(),
            artist_id: None,
            year: None,
            kind: ReleaseKind::Unknown,
            thumbnail: String::new(),
            description: "Line one\nLine two".into(),
            tracks: vec![video.clone()],
        };
        assert!(details.validate().is_ok());
        for bad in [
            AlbumDetails {
                artist_id: Some("channel".into()),
                ..details.clone()
            },
            AlbumDetails {
                description: "tab\tinside".into(),
                ..details.clone()
            },
            AlbumDetails {
                tracks: vec![SearchResult {
                    url: "https://evil.example/watch?v=fixture0001".into(),
                    ..video.clone()
                }],
                ..details.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn album_inputs_accept_bare_ids_and_links_to_album_playlists_only() {
        assert_eq!(album_playlist(ALBUM).unwrap(), ALBUM);
        assert_eq!(
            album_playlist(&format!("https://music.youtube.com/playlist?list={ALBUM}")).unwrap(),
            ALBUM
        );
        for bad in [
            "PLfixturePlaylist00000000000000002",
            "OLAK5uy_",
            "https://evil.example/playlist?list=OLAK5uy_fixtureAlbum000000000000000000001",
            "album",
        ] {
            assert!(album_playlist(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn nesting_beyond_the_depth_bound_is_not_traversed() {
        let mut nested = tile(page(ARTIST, ARTIST_PAGE), json!([{"text": "1 M"}]), None);
        for _ in 0..=MAX_DEPTH {
            nested = json!([nested]);
        }
        let mut found = 0;
        visit_items(Some(&nested), &mut |_, _| found += 1);
        assert_eq!(found, 0);
        assert!(find_key(Some(&nested), TILE).is_none());
    }

    #[test]
    fn malformed_responses_never_panic() {
        let fixtures = [
            "music_search.json",
            "music_artist.json",
            "music_album_tracks.json",
            "music_album_page.json",
            "watch_ticket_shelf.json",
            "recent_uploads_search.json",
            "music_watch_topic_track.json",
            "music_watch_vevo_video.json",
            "music_watch_fan_upload.json",
        ];
        let keys = [
            "runs",
            "text",
            "browseId",
            "videoId",
            "playlistId",
            "thumbnails",
            "contents",
            "header",
            "title",
            "subtitle",
            "events",
            "url",
            "timeDate",
            "pageType",
            "navigationEndpoint",
            "ownerText",
            "attachmentRuns",
            "listItems",
            "longBylineText",
            "watchEndpoint",
            "buttons",
            "urlCanonical",
            "width",
        ];
        let garbage = [
            json!(null),
            json!(42),
            json!("•"),
            json!([[]]),
            json!({"runs": "x"}),
        ];
        let wanted = fold_words("Artist 1");
        for name in fixtures {
            for key in keys {
                for with in &garbage {
                    let mut value = fixture(name);
                    poison(&mut value, key, with);
                    let _ = parse_search(&value, CATALOG_KIND_LIMIT).validate();
                    if let Some(profile) = parse_artist(&value, ARTIST) {
                        assert!(profile.validate().is_ok(), "{name} {key} {with}");
                    }
                    let mut tracks = AlbumTracks::default();
                    tracks.read(&value);
                    let _ = parse_album_header(&value, ALBUM);
                    assert!(parse_events(&value, "Artist 1")
                        .iter()
                        .all(|e| e.validate().is_ok()));
                    let _ = parse_matching_results(&value, NEW_RELEASE_LIMIT, |card| {
                        official_upload(card, &wanted)
                    });
                    for video in ["fixture0019", "fixture0020", "fixture0021"] {
                        if let Some(song) = parse_song(&value, video) {
                            let context = SongContext {
                                artists: song.artists,
                                album: None,
                            };
                            assert!(context.validate().is_ok(), "{name} {key} {with}");
                        }
                    }
                    if let Some(album) = release_summary(&value, None) {
                        assert!(album.validate().is_ok(), "{name} {key} {with}");
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn client_methods_reject_bad_input_and_precancellation_without_network() {
        let client = YoutubeClient::new().unwrap();
        let none = CancellationToken::new;
        assert!(matches!(
            client.catalog_search("song", 0, none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client
                .catalog_search("song", CATALOG_KIND_LIMIT + 1, none())
                .await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.catalog_search(" \n ", 5, none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.artist("not-a-channel", none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client
                .album("PLfixturePlaylist00000000000000002", none())
                .await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.find_artist("a\u{0}b", none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.new_releases("", none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.credits("not a video", none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client
                .song_context("https://evil.example/watch?v=dQw4w9WgXcQ", none())
                .await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.album("MPREb_", none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        assert!(matches!(
            client.suggestions(&"x".repeat(4096), none()).await,
            Err(MediaError::InvalidInput(_))
        ));
        let cancelled = || {
            let token = CancellationToken::new();
            token.cancel();
            token
        };
        assert!(matches!(
            client.catalog_search("song", 5, cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.artist(ARTIST, cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.find_artist("artist", cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.album(ALBUM, cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.new_releases("artist", cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.credits("dQw4w9WgXcQ", cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.song_context("dQw4w9WgXcQ", cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.album("MPREb_fixt0000008", cancelled()).await,
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            client.suggestions("art", cancelled()).await,
            Err(MediaError::Cancelled)
        ));
    }

    fn credited<'a>(song: &'a SongByline<'_>) -> Vec<(&'a str, &'a str)> {
        song.artists
            .iter()
            .map(|artist| (artist.id.as_str(), artist.name.as_str()))
            .collect()
    }

    fn queue_entry(response: &mut Value) -> &mut Value {
        response
            .pointer_mut("/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs/0/tabRenderer/content/musicQueueRenderer/content/playlistPanelRenderer/contents/0/playlistPanelVideoRenderer")
            .unwrap()
    }

    #[test]
    fn topic_and_vevo_uploads_resolve_to_the_credited_artists() {
        // Recorded from an art track whose web channel is the localized Topic
        // channel («…: Tema») and from an official video of a former VEVO
        // channel: YouTube Music credits the artists, never the channels.
        let track = fixture("music_watch_topic_track.json");
        let song = parse_song(&track, "fixture0019").unwrap();
        assert_eq!(
            credited(&song),
            [
                ("UCfixtureChannel00000011", "Artist 11"),
                ("UCfixtureChannel00000012", "Artist 12")
            ]
        );
        assert_eq!(song.release, Some("MPREb_fixt0000008"));
        assert_eq!(song.year, Some(1994));
        let video = fixture("music_watch_vevo_video.json");
        let song = parse_song(&video, "fixture0020").unwrap();
        assert_eq!(
            credited(&song),
            [
                ("UCfixtureChannel00000013", "Artist 13"),
                ("UCfixtureChannel00000014", "Artist 14")
            ]
        );
        assert_eq!(song.release, None, "music videos link no release");
        let context = SongContext {
            artists: song.artists,
            album: None,
        };
        assert!(context.validate().is_ok());
    }

    #[test]
    fn fan_uploads_other_entries_and_episodes_have_no_song_context() {
        assert!(parse_song(&fixture("music_watch_fan_upload.json"), "fixture0021").is_none());
        let track = fixture("music_watch_topic_track.json");
        assert!(parse_song(&track, "otherVideo1").is_none());
        let mut episode = track.clone();
        queue_entry(&mut episode)["navigationEndpoint"]["watchEndpoint"]
            ["watchEndpointMusicSupportedConfigs"]["watchEndpointMusicConfig"]["musicVideoType"] =
            json!(EPISODE_VIDEO_TYPE);
        assert!(parse_song(&episode, "fixture0019").is_none());
        // A song whose byline links its artists but no album has no release.
        let mut single = track;
        let runs = queue_entry(&mut single)["longBylineText"]["runs"]
            .as_array_mut()
            .unwrap();
        runs.retain(|run| !run.to_string().contains(ALBUM_PAGE));
        let song = parse_song(&single, "fixture0019").unwrap();
        assert_eq!(song.artists.len(), 2);
        assert_eq!(song.release, None);
    }

    #[test]
    fn release_pages_give_their_album_playlist_and_summary() {
        let page = fixture("music_album_page.json");
        assert_eq!(release_playlist(&page).as_deref(), Some(ALBUM));
        let summary = release_summary(&page, Some(1999)).unwrap();
        assert!(summary.validate().is_ok());
        assert_eq!(
            (
                summary.id.as_str(),
                summary.title.as_str(),
                summary.artist.as_str()
            ),
            (ALBUM, "Album 2", "Artist 1")
        );
        assert_eq!(
            (summary.year, summary.kind, summary.track_count),
            (Some(2023), ReleaseKind::Album, None)
        );
        assert!(summary
            .thumbnail
            .ends_with("fixtureImage005=w544-h544-l90-rj"));
        let header = "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0/musicResponsiveHeaderRenderer";
        let mut canonical_only = page.clone();
        canonical_only.pointer_mut(header).unwrap()["buttons"] = json!([]);
        assert_eq!(release_playlist(&canonical_only).as_deref(), Some(ALBUM));
        let mut foreign = canonical_only.clone();
        foreign.pointer_mut(header).unwrap()["buttons"] = json!([{"musicPlayButtonRenderer":
            {"playNavigationEndpoint": {"watchPlaylistEndpoint": {"playlistId": "PLfixturePlaylist00000000000000002"}}}}]);
        foreign["microformat"]["microformatDataRenderer"]["urlCanonical"] =
            json!("https://evil.example/playlist?list=OLAK5uy_fixtureAlbum000000000000000000001");
        assert_eq!(release_playlist(&foreign), None);
        assert!(release_summary(&foreign, None).is_none());
        let mut yearless = page;
        yearless.pointer_mut(header).unwrap()["subtitle"] = json!({"runs": [{"text": "Álbum"}]});
        assert_eq!(
            release_summary(&yearless, Some(1999)).unwrap().year,
            Some(1999)
        );
    }

    #[test]
    fn release_ids_are_recognized_before_any_request() {
        assert!(release_id_ok("MPREb_fixt0000008"));
        let oversized = format!("{ALBUM_BROWSE_PREFIX}{}", "a".repeat(MAX_RELEASE_ID_LENGTH));
        for bad in [
            "MPREb_",
            "MPREb_bad id",
            "MPREb_a/b",
            ALBUM,
            oversized.as_str(),
        ] {
            assert!(!release_id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn song_context_validation_checks_artists_and_the_release() {
        let artist = ArtistSummary {
            id: ARTIST.into(),
            name: "Artist".into(),
            thumbnail: String::new(),
            subscribers: String::new(),
        };
        let album = AlbumSummary {
            id: ALBUM.into(),
            title: "Album".into(),
            artist: "Artist".into(),
            year: Some(2024),
            kind: ReleaseKind::Single,
            thumbnail: String::new(),
            track_count: None,
        };
        let context = SongContext {
            artists: vec![artist.clone()],
            album: Some(album.clone()),
        };
        assert!(context.validate().is_ok());
        assert!(SongContext::default().validate().is_ok());
        let playlist = AlbumSummary {
            id: "PLfixturePlaylist00000000000000002".into(),
            kind: ReleaseKind::Unknown,
            ..album
        };
        for bad in [
            SongContext {
                album: Some(playlist),
                ..context.clone()
            },
            SongContext {
                artists: vec![artist.clone(); CREDITED_ARTIST_LIMIT + 1],
                ..context.clone()
            },
            SongContext {
                artists: vec![ArtistSummary {
                    id: "UCshort".into(),
                    ..artist
                }],
                ..context
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
