//! Search controller: the library index, personal ranking, completion, the
//! top result, the online search and remembered searches. Relevance is
//! decided by `yt-search`; this module feeds it typed values and maps its
//! answers back. See `docs/systems/Search.md`.

use crate::{discovery::unix_now, state::AppState};
use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};
use yt_core::{library_artists, Track};
use yt_media::{AlbumSummary, ArtistSummary, CancellationToken, CatalogSearch, SearchResult};
use yt_search::{
    Bm25Config, CompletionConfig, Document, Field, HistoryQuery, Index, SuggestionSource, TopKind,
};

/// Remembered searches kept in memory for completion and the recent list.
pub(crate) const HISTORY_IN_MEMORY: usize = 50;
/// Songs listed beside the top result.
pub(crate) const TOP_SONGS: usize = 4;
/// Results per kind asked of the catalog search; YouTube Music's first page
/// holds about this many.
const ONLINE_PER_KIND: usize = 12;
/// Web results asked for when the catalog search is unavailable.
const FALLBACK_RESULTS: usize = 25;
/// Library hits that may become the top result; they come before online
/// songs, so the user's own copy wins over an equal online match.
const TOP_CANDIDATES: usize = 10;
/// Largest lift the listening profile gives a library hit, in BM25 score
/// units: enough to reorder close hits, never to bury a clearly better one.
const PERSONAL_WEIGHT: f64 = 1.0;
/// Share of an artist's affinity credited to their songs the profile does
/// not know yet.
const ARTIST_AFFINITY_SHARE: f64 = 0.5;

/// The single most likely answer to a search.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TopResult {
    /// A song in the library.
    Library(Track),
    /// An artist the library knows by name.
    LibraryArtist(String),
    /// An online artist with their channel.
    Artist(ArtistSummary),
    /// An online song.
    Song(SearchResult),
    /// An online album, single or EP.
    Album(AlbumSummary),
}

/// One suggestion under the search field.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Suggestion {
    pub text: String,
    pub icon: &'static str,
}

/// The library prepared for searching; rebuilt when the library changes.
pub(crate) struct LibrarySearch {
    index: Index,
    tracks: HashMap<String, Track>,
    /// Distinct artist names in library order, for the top result.
    artists: Vec<String>,
    /// Distinct artist, title and album names, for completion.
    names: Vec<String>,
}

impl LibrarySearch {
    /// Index every playable track by title, artist, album and file name.
    pub(crate) fn build(library: &[Track]) -> Self {
        let playable: Vec<Track> = library
            .iter()
            .filter(|track| !track.missing)
            .cloned()
            .collect();
        let documents: Vec<Document> = playable
            .iter()
            .map(|track| Document {
                id: track.id.clone(),
                fields: vec![
                    (Field::Title, track.title.clone()),
                    (Field::Artist, track.artist.clone()),
                    (Field::Album, track.album.clone()),
                    (Field::Other, file_stem(&track.filename).to_owned()),
                ],
            })
            .collect();
        let mut seen = HashSet::new();
        let names = playable
            .iter()
            .flat_map(|track| [&track.artist, &track.title, &track.album])
            .filter(|name| !name.trim().is_empty() && seen.insert(yt_core::fold(name)))
            .cloned()
            .collect();
        Self {
            index: Index::build(&documents, Bm25Config::default()),
            artists: library_artists(&playable)
                .into_iter()
                .map(|artist| artist.name)
                .collect(),
            tracks: playable
                .into_iter()
                .map(|track| (track.id.clone(), track))
                .collect(),
            names,
        }
    }

    /// Tracks matching `query`, best first, with their scores, lifted by the
    /// user's `affinity` for each track.
    pub(crate) fn search(
        &self,
        query: &str,
        limit: usize,
        affinity: impl Fn(&Track) -> f64,
    ) -> Vec<(Track, f64)> {
        let scored = self
            .index
            .search(query, limit)
            .into_iter()
            .map(|hit| (hit.id, hit.score))
            .collect();
        yt_search::personalize(
            scored,
            |id| self.tracks.get(id).map_or(0.0, &affinity),
            PERSONAL_WEIGHT,
        )
        .into_iter()
        .filter_map(|(id, score)| self.tracks.get(&id).cloned().map(|track| (track, score)))
        .collect()
    }
}

/// A file name without its extension, as people remember it.
fn file_stem(filename: &str) -> &str {
    std::path::Path::new(filename)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(filename)
}

/// The top result: an artist named by the query (online artists first,
/// they have a page), else a song whose title matches it (library hits
/// first, so the user's own copy wins), else an album, else the best library
/// hit.
pub(crate) fn top_result(
    query: &str,
    library: &LibrarySearch,
    hits: &[(Track, f64)],
    online: Option<&CatalogSearch>,
) -> Option<TopResult> {
    let empty = CatalogSearch::default();
    let online = online.unwrap_or(&empty);
    let local: Vec<&Track> = hits
        .iter()
        .take(TOP_CANDIDATES)
        .map(|(track, _)| track)
        .collect();
    let artists: Vec<&str> = online
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .chain(library.artists.iter().map(String::as_str))
        .collect();
    let songs: Vec<&str> = local
        .iter()
        .map(|track| track.title.as_str())
        .chain(online.songs.iter().map(|song| song.title.as_str()))
        .collect();
    let albums: Vec<&str> = online
        .albums
        .iter()
        .map(|album| album.title.as_str())
        .collect();
    let best = hits.first().map(|(_, score)| *score);
    let (kind, index) = yt_search::top_result(query, &artists, &songs, &albums, best)?;
    match kind {
        TopKind::Artist => match online.artists.get(index) {
            Some(artist) => Some(TopResult::Artist(artist.clone())),
            None => index
                .checked_sub(online.artists.len())
                .and_then(|known| library.artists.get(known))
                .cloned()
                .map(TopResult::LibraryArtist),
        },
        TopKind::Song => match local.get(index) {
            Some(track) => Some(TopResult::Library((*track).clone())),
            None => index
                .checked_sub(local.len())
                .and_then(|song| online.songs.get(song))
                .cloned()
                .map(TopResult::Song),
        },
        TopKind::Album => online.albums.get(index).cloned().map(TopResult::Album),
        TopKind::Library => local
            .first()
            .map(|track| TopResult::Library((*track).clone())),
    }
}

impl AppState {
    /// How much the user likes `track`: its own affinity in the last built
    /// profile, else part of its artist's.
    pub(crate) fn track_affinity(&self, track: &Track) -> f64 {
        let taste = self.discovery.taste.borrow();
        let Some(profile) = taste.as_ref().map(|taste| &taste.profile) else {
            return 0.0;
        };
        profile.media_affinity(&track.id).map_or_else(
            || ARTIST_AFFINITY_SHARE * profile.artist_score(&track.artist),
            |media| media.score,
        )
    }

    /// Completions for `prefix`: remembered searches (frequent and recent
    /// first), then library names, then the provider's suggestions.
    pub(crate) fn search_suggestions(
        &self,
        prefix: &str,
        library: &LibrarySearch,
        provider: &[String],
        limit: usize,
    ) -> Vec<Suggestion> {
        let history: Vec<HistoryQuery> = self
            .search_history
            .peek()
            .iter()
            .map(|entry| HistoryQuery {
                text: entry.query.clone(),
                uses: entry.uses,
                last_at: entry.last_at,
            })
            .collect();
        let names: Vec<&str> = library.names.iter().map(String::as_str).collect();
        yt_search::complete(
            prefix,
            &history,
            &names,
            provider,
            unix_now(),
            &CompletionConfig::default(),
            limit,
        )
        .into_iter()
        .map(|suggestion| Suggestion {
            text: suggestion.text,
            icon: match suggestion.source {
                SuggestionSource::History => "history",
                SuggestionSource::Library => "music",
                SuggestionSource::Provider => "search",
            },
        })
        .collect()
    }

    /// Search YouTube Music for `query` by kind, falling back to a plain web
    /// search when the catalog is unavailable; a link opens its video. Gives
    /// up as soon as `cancel` fires.
    pub(crate) async fn search_online(
        &self,
        query: &str,
        cancel: CancellationToken,
    ) -> Result<CatalogSearch, String> {
        let youtube = self
            .services
            .youtube_client()
            .map_err(|error| error.to_string())?;
        let songs = |songs| CatalogSearch {
            songs,
            ..CatalogSearch::default()
        };
        let request = async {
            if query.starts_with("https://") || query.starts_with("http://") {
                return youtube.metadata(query).await.map(|item| songs(vec![item]));
            }
            match youtube
                .catalog_search(query, ONLINE_PER_KIND, cancel.clone())
                .await
            {
                Err(yt_media::MediaError::Cancelled) => Err(yt_media::MediaError::Cancelled),
                Err(_) => youtube.search(query, FALLBACK_RESULTS).await.map(songs),
                found => found,
            }
        };
        let mut found = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(yt_media::MediaError::Cancelled.to_string()),
            result = request => result.map_err(|error| error.to_string())?,
        };
        crate::catalog::unique_ids(&mut found.songs, |song| &song.id);
        crate::catalog::unique_ids(&mut found.artists, |artist| &artist.id);
        crate::catalog::unique_ids(&mut found.albums, |album| &album.id);
        crate::catalog::unique_ids(&mut found.playlists, |list| &list.id);
        crate::catalog::unique_ids(&mut found.videos, |video| &video.id);
        Ok(found)
    }

    /// The provider's completions for `prefix`; empty on any failure, since
    /// they only add to the local ones.
    pub(crate) async fn provider_suggestions(
        &self,
        prefix: &str,
        cancel: CancellationToken,
    ) -> Vec<String> {
        let Ok(youtube) = self.services.youtube_client() else {
            return Vec::new();
        };
        youtube
            .suggestions(prefix, cancel)
            .await
            .unwrap_or_default()
    }

    /// Remember a search the user ran; invalid text is simply not kept.
    pub(crate) fn remember_search(&self, query: &str) {
        if self.services.core.record_search(query).is_ok() {
            self.reload_search_history();
        }
    }

    pub(crate) fn forget_search(&self, query: &str) {
        match self.services.core.forget_search(query) {
            Ok(()) => self.reload_search_history(),
            Err(error) => self.notify_error(format!("No se pudo borrar la búsqueda: {error}")),
        }
    }

    pub(crate) fn clear_search_history(&self) {
        match self.services.core.clear_search_history() {
            Ok(_) => self.reload_search_history(),
            Err(error) => self.notify_error(format!("No se pudo borrar el historial: {error}")),
        }
    }

    /// Reload remembered searches from the database.
    pub(crate) fn reload_search_history(&self) {
        let mut history = self.search_history;
        history.set(
            self.services
                .core
                .search_history(HISTORY_IN_MEMORY)
                .unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{remote_track, TestApp};

    fn track(video_id: &str, title: &str, artist: &str) -> Track {
        let mut track = remote_track(video_id, title);
        track.artist = artist.into();
        track
    }

    #[tokio::test]
    async fn searches_are_remembered_newest_first_and_can_be_forgotten() {
        let mut app = TestApp::new();
        app.run(|state| {
            state.remember_search("Peso Pluma");
            state.remember_search("Karol G");
            state.remember_search("  ");
            let queries: Vec<String> = state
                .search_history
                .peek()
                .iter()
                .map(|entry| entry.query.clone())
                .collect();
            assert_eq!(queries, ["Karol G", "Peso Pluma"]);
            state.forget_search("karol g");
            assert_eq!(state.search_history.peek().len(), 1);
            state.clear_search_history();
            assert!(state.search_history.peek().is_empty());
        });
    }

    #[test]
    fn library_search_tolerates_typos_and_puts_the_artist_or_own_copy_on_top() {
        let library = vec![
            track("aaaaaaaaaaa", "Bailando", "Enrique Iglesias"),
            track("bbbbbbbbbbb", "Hero", "Enrique Iglesias"),
            track("ccccccccccc", "Ella Baila Sola", "Peso Pluma"),
        ];
        let search = LibrarySearch::build(&library);
        let hits = search.search("bailnado", 5, |_| 0.0);
        assert_eq!(
            hits.first().map(|(track, _)| track.title.as_str()),
            Some("Bailando")
        );
        let artist = search.search("enrique iglesias", 5, |_| 0.0);
        assert_eq!(
            top_result("enrique iglesias", &search, &artist, None),
            Some(TopResult::LibraryArtist("Enrique Iglesias".into()))
        );
        let song = search.search("ella baila sola", 5, |_| 0.0);
        let online = CatalogSearch {
            songs: vec![SearchResult {
                id: "ddddddddddd".into(),
                url: String::new(),
                title: "Ella Baila Sola".into(),
                thumbnail: String::new(),
                duration_seconds: 160,
                channel: "Peso Pluma".into(),
                view_count: 1,
            }],
            artists: vec![ArtistSummary {
                id: "UCabcdefghijklmnopqrstuv".into(),
                name: "Peso Pluma".into(),
                thumbnail: String::new(),
                subscribers: String::new(),
            }],
            ..CatalogSearch::default()
        };
        let top = top_result("ella baila sola", &search, &song, Some(&online));
        assert!(matches!(top, Some(TopResult::Library(track)) if track.title == "Ella Baila Sola"));
        let only_online = top_result("ella baila sola", &search, &[], Some(&online));
        assert!(matches!(only_online, Some(TopResult::Song(song)) if song.id == "ddddddddddd"));
        let artist = top_result("peso pluma", &search, &[], Some(&online));
        assert!(matches!(artist, Some(TopResult::Artist(artist)) if artist.name == "Peso Pluma"));
    }

    #[test]
    fn liked_tracks_rise_among_close_matches() {
        let library = vec![
            track("aaaaaaaaaaa", "Amor Eterno", "Rocío Dúrcal"),
            track("bbbbbbbbbbb", "Amor Eterno", "Juan Gabriel"),
        ];
        let search = LibrarySearch::build(&library);
        let neutral = search.search("amor eterno", 5, |_| 0.0);
        assert_eq!(neutral[0].0.artist, "Rocío Dúrcal");
        let liked = search.search("amor eterno", 5, |track| {
            if track.artist == "Juan Gabriel" {
                3.0
            } else {
                0.0
            }
        });
        assert_eq!(liked[0].0.artist, "Juan Gabriel");
    }
}
