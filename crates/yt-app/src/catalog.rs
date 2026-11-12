//! Catalog controller: artist pages, albums, song credits and new releases
//! from YouTube Music, through the provider cache so pages open instantly the
//! second time and keep working offline. Every cached copy is validated again
//! when read back. See `docs/systems/Browse.md`.

use crate::{
    discovery::to_remote,
    navigation::{AlbumRef, ArtistRef, Detail},
    state::AppState,
};
use dioxus::prelude::*;
use std::{collections::HashSet, time::Duration};
use yt_core::{clean_artist, song_title, title_key, Credits, RemoteMedia, Track};
use yt_media::{
    AlbumDetails, AlbumSummary, ArtistProfile, ArtistSummary, CancellationToken, SongContext,
};

/// How long an artist page stays fresh.
const ARTIST_TTL: Duration = Duration::from_secs(24 * 3_600);
/// How long the channel found for an artist name is trusted.
const ARTIST_MATCH_TTL: Duration = Duration::from_secs(7 * 24 * 3_600);
/// How long an album page stays fresh; releases rarely change.
const ALBUM_TTL: Duration = Duration::from_secs(7 * 24 * 3_600);
/// How long a song's credits stay fresh.
const CREDITS_TTL: Duration = Duration::from_secs(30 * 24 * 3_600);
/// How long an artist's newest uploads stay fresh.
const RELEASES_TTL: Duration = Duration::from_secs(6 * 3_600);
/// How long a song's credited artists and album stay trusted.
const SONG_TTL: Duration = Duration::from_secs(30 * 24 * 3_600);
/// Catalog songs compared when a music video's album is looked up through
/// its audio release.
const RELEASE_SEARCH_SONGS: usize = 5;

/// Keep the first item of every id. Providers sometimes list one item twice,
/// and keyed rows with a repeated id abort the whole interface, so every list
/// rendered as keyed rows passes through here.
pub(crate) fn unique_ids<T>(items: &mut Vec<T>, id: impl Fn(&T) -> &str) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(id(item).to_owned()));
}

/// An artist page whose lists hold each id once.
fn unique_profile(mut profile: ArtistProfile) -> ArtistProfile {
    unique_ids(&mut profile.top_songs, |song| &song.id);
    unique_ids(&mut profile.albums, |release| &release.id);
    unique_ids(&mut profile.singles, |release| &release.id);
    unique_ids(&mut profile.videos, |video| &video.id);
    unique_ids(&mut profile.related, |artist| &artist.id);
    profile
}

impl AppState {
    fn youtube(&self) -> Result<yt_media::YoutubeClient, String> {
        self.services
            .youtube_client()
            .map_err(|error| error.to_string())
    }

    /// The online page of `artist`: its channel when known; otherwise the
    /// channel that best matches its name, looked up only while online
    /// recommendations are on. `Ok(None)` when there is no online page.
    pub(crate) async fn artist_profile(
        &self,
        artist: &ArtistRef,
    ) -> Result<Option<ArtistProfile>, String> {
        let channel = match &artist.channel_id {
            Some(id) => Some(id.clone()),
            None if self.settings.peek().discovery.online => {
                self.artist_channel(&artist.name).await?
            }
            None => None,
        };
        let Some(channel) = channel else {
            return Ok(None);
        };
        let youtube = self.youtube()?;
        let id = channel.clone();
        self.cached_json(
            format!("catalog:artist:{channel}"),
            ARTIST_TTL,
            |profile: &ArtistProfile| profile.validate().is_ok(),
            async move {
                youtube
                    .artist(&id, CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .map(|profile| Some(unique_profile(profile)))
    }

    /// The channel of the artist named `name`; a miss is remembered too.
    async fn artist_channel(&self, name: &str) -> Result<Option<String>, String> {
        let youtube = self.youtube()?;
        let query = name.to_owned();
        self.cached_json(
            format!("catalog:artist-match:{}", yt_discovery::artist_key(name)),
            ARTIST_MATCH_TTL,
            |found: &Option<ArtistSummary>| {
                found
                    .as_ref()
                    .is_none_or(|artist| artist.validate().is_ok())
            },
            async move {
                youtube
                    .find_artist(&query, CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .map(|found| found.map(|artist| artist.id))
    }

    /// The release `playlist_id` with its tracks.
    pub(crate) async fn album_details(&self, playlist_id: &str) -> Result<AlbumDetails, String> {
        let youtube = self.youtube()?;
        let id = playlist_id.to_owned();
        self.cached_json(
            format!("catalog:album:{playlist_id}"),
            ALBUM_TTL,
            |album: &AlbumDetails| album.validate().is_ok(),
            async move {
                youtube
                    .album(&id, CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .map(|mut album| {
            unique_ids(&mut album.tracks, |track| &track.id);
            album
        })
    }

    /// Credits of the online recording `video_id`.
    pub(crate) async fn online_credits(&self, video_id: &str) -> Result<Credits, String> {
        let youtube = self.youtube()?;
        let id = video_id.to_owned();
        self.cached_json(
            // Version 2 adds the track credits page; older copies lack its roles.
            format!("catalog:credits-v2:{video_id}"),
            CREDITS_TTL,
            |credits: &Credits| credits.validate().is_ok(),
            async move {
                youtube
                    .credits(&id, CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())
            },
        )
        .await
    }

    /// The artists and release YouTube Music credits to the song `video_id`.
    pub(crate) async fn song_context(&self, video_id: &str) -> Result<SongContext, String> {
        let youtube = self.youtube()?;
        let id = video_id.to_owned();
        self.cached_json(
            format!("catalog:song:{video_id}"),
            SONG_TTL,
            |context: &SongContext| context.validate().is_ok(),
            async move {
                youtube
                    .song_context(&id, CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())
            },
        )
        .await
    }

    /// The release of `track`'s recording. A music video is linked to no
    /// release, so its audio release is found by searching the catalog for
    /// its artist and song, keeping only a song with the same title.
    async fn song_release(
        &self,
        track: &Track,
        video_id: &str,
    ) -> Result<Option<AlbumSummary>, String> {
        let context = self.song_context(video_id).await?;
        if context.album.is_some() {
            return Ok(context.album);
        }
        let artist = context
            .artists
            .first()
            .map_or_else(|| clean_artist(&track.artist), |artist| artist.name.clone());
        let song = song_title(&track.title).song;
        let wanted = title_key(&song);
        let found = self
            .youtube()?
            .catalog_search(
                &format!("{artist} {song}"),
                RELEASE_SEARCH_SONGS,
                CancellationToken::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let Some(audio) = found
            .songs
            .iter()
            .find(|candidate| candidate.id != video_id && title_key(&candidate.title) == wanted)
        else {
            return Ok(None);
        };
        Ok(self.song_context(&audio.id).await?.album)
    }

    /// Open the artist of `track`: the artist YouTube Music credits for a
    /// song with a YouTube identity (never a «- Topic» or VEVO channel), else
    /// the artist named by its tags.
    pub(crate) fn open_track_artist(&self, track: &Track) {
        let name = clean_artist(&track.artist);
        let Some(video) = crate::remote::video_id_of(track) else {
            self.open_artist(&name, None, "");
            return;
        };
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            let credited = state
                .song_context(&video)
                .await
                .ok()
                .and_then(|context| context.artists.into_iter().next());
            match credited {
                Some(artist) => state.open_artist(&artist.name, Some(artist.id), &artist.thumbnail),
                None => state.open_artist(&name, None, ""),
            }
        });
    }

    /// Open the album of `track`: its library album when its tags name one,
    /// else the release YouTube Music links to the song.
    pub(crate) fn open_track_album(&self, track: &Track) {
        if !crate::remote::is_remote(track) && !track.album.trim().is_empty() {
            self.open_detail(Detail::Album(AlbumRef {
                title: track.album.clone(),
                artist: track.artist.clone(),
                playlist_id: None,
                thumbnail: String::new(),
            }));
            return;
        }
        let Some(video) = crate::remote::video_id_of(track) else {
            return;
        };
        let state = self.clone();
        let track = track.clone();
        dioxus::core::spawn_forever(async move {
            match state.song_release(&track, &video).await {
                Ok(Some(release)) => crate::artist_online::open_release(&state, &release),
                Ok(None) => state.notify_warning("Esta canción no tiene un álbum en YouTube Music"),
                Err(error) => state.notify_error(format!("No se pudo buscar el álbum: {error}")),
            }
        });
    }

    /// This month's official uploads of the artist named `name`.
    pub(crate) async fn artist_releases(&self, name: &str) -> Result<Vec<RemoteMedia>, String> {
        let youtube = self.youtube()?;
        let query = name.to_owned();
        self.cached_lookup(
            format!("releases:{}", yt_discovery::artist_key(name)),
            RELEASES_TTL,
            async move {
                youtube
                    .new_releases(&query, CancellationToken::new())
                    .await
                    .map(to_remote)
                    .map_err(|error| error.to_string())
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_ids_keep_their_first_item_in_order() {
        let mut items = vec![("a", 1), ("b", 2), ("a", 3), ("c", 4), ("b", 5)];
        unique_ids(&mut items, |item| item.0);
        assert_eq!(items, [("a", 1), ("b", 2), ("c", 4)]);
    }
}
