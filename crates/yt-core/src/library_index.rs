//! Library organization by artist and album, computed from track metadata.
//! Grouping is case- and spacing-insensitive, like a music library expects.

use crate::Track;
use std::collections::BTreeMap;

/// Display name used for tracks without an artist or album tag.
const UNKNOWN: &str = "";

/// An artist present in the library.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryArtist {
    /// Name as first seen in the library.
    pub name: String,
    /// Track ids, newest first.
    pub track_ids: Vec<String>,
    pub album_count: usize,
    /// A track whose artwork represents the artist, if any track has some.
    pub artwork_track: Option<String>,
}

/// An album present in the library.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryAlbum {
    pub title: String,
    pub artist: String,
    /// Track ids in title order.
    pub track_ids: Vec<String>,
    pub artwork_track: Option<String>,
    /// Total duration in seconds.
    pub duration: f64,
}

/// Grouping identity: lowercase with collapsed whitespace.
fn key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn has_artwork(track: &Track) -> bool {
    !track.artwork_url.is_empty()
}

/// Artists of playable audio and video tracks, sorted by name. Tracks without
/// an artist tag are left out.
pub fn library_artists(tracks: &[Track]) -> Vec<LibraryArtist> {
    let mut groups: BTreeMap<String, LibraryArtist> = BTreeMap::new();
    let mut albums: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    for track in tracks.iter().filter(|track| !track.missing) {
        let artist_key = key(&track.artist);
        if artist_key == UNKNOWN {
            continue;
        }
        let group = groups
            .entry(artist_key.clone())
            .or_insert_with(|| LibraryArtist {
                name: track.artist.trim().to_owned(),
                track_ids: Vec::new(),
                album_count: 0,
                artwork_track: None,
            });
        group.track_ids.push(track.id.clone());
        if group.artwork_track.is_none() && has_artwork(track) {
            group.artwork_track = Some(track.id.clone());
        }
        let album_key = key(&track.album);
        if album_key != UNKNOWN {
            albums.entry(artist_key).or_default().insert(album_key);
        }
    }
    groups
        .into_iter()
        .map(|(artist_key, mut group)| {
            group.album_count = albums.get(&artist_key).map_or(0, |set| set.len());
            group
        })
        .collect()
}

/// Album and artist comparison keys of one album.
type AlbumKey = (String, String);
/// Title comparison key and track id; sorting these orders an album.
type TitledTrack = (String, String);

/// Albums of playable tracks, grouped by album and artist and sorted by
/// title. Tracks without an album tag are left out.
pub fn library_albums(tracks: &[Track]) -> Vec<LibraryAlbum> {
    let mut groups: BTreeMap<AlbumKey, (LibraryAlbum, Vec<TitledTrack>)> = BTreeMap::new();
    for track in tracks.iter().filter(|track| !track.missing) {
        let album_key = key(&track.album);
        if album_key == UNKNOWN {
            continue;
        }
        let entry = groups
            .entry((album_key, key(&track.artist)))
            .or_insert_with(|| {
                (
                    LibraryAlbum {
                        title: track.album.trim().to_owned(),
                        artist: track.artist.trim().to_owned(),
                        track_ids: Vec::new(),
                        artwork_track: None,
                        duration: 0.0,
                    },
                    Vec::new(),
                )
            });
        entry.1.push((key(&track.title), track.id.clone()));
        entry.0.duration += track.duration.max(0.0);
        if entry.0.artwork_track.is_none() && has_artwork(track) {
            entry.0.artwork_track = Some(track.id.clone());
        }
    }
    groups
        .into_values()
        .map(|(mut album, mut titled)| {
            titled.sort();
            album.track_ids = titled.into_iter().map(|(_, id)| id).collect();
            album
        })
        .collect()
}

/// The album titled `title` by `artist`, matched the way albums group: case
/// and spacing are ignored.
pub fn library_album(tracks: &[Track], title: &str, artist: &str) -> Option<LibraryAlbum> {
    let (title, artist) = (key(title), key(artist));
    if title == UNKNOWN {
        return None;
    }
    library_albums(tracks)
        .into_iter()
        .find(|album| key(&album.title) == title && key(&album.artist) == artist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(id: &str, title: &str, artist: &str, album: &str, artwork: bool) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            filename: format!("{id}.mp3"),
            path: PathBuf::from(format!("/music/{id}.mp3")),
            media_type: "audio".into(),
            extension: "mp3".into(),
            duration: 100.0,
            size_bytes: 1,
            artwork_url: if artwork {
                format!("/artwork/{id}")
            } else {
                String::new()
            },
            favorite: false,
            missing: false,
            created_at: 0.0,
            source_url: String::new(),
        }
    }

    #[test]
    fn artists_and_albums_group_case_insensitively_and_skip_untagged_tracks() {
        let tracks = vec![
            track("1", "B song", "Peso Pluma", "Génesis", false),
            track("2", "A song", "peso  pluma", "génesis", true),
            track("3", "Single", "Peso Pluma", "", false),
            track("4", "Other", "Karol G", "Mañana", true),
            track("5", "Untagged", "", "", true),
        ];
        let artists = library_artists(&tracks);
        assert_eq!(artists.len(), 2);
        let peso = artists
            .iter()
            .find(|artist| artist.name == "Peso Pluma")
            .unwrap();
        assert_eq!(peso.track_ids, ["1", "2", "3"]);
        assert_eq!(peso.album_count, 1);
        assert_eq!(peso.artwork_track.as_deref(), Some("2"));
        let albums = library_albums(&tracks);
        assert_eq!(albums.len(), 2);
        let genesis = albums
            .iter()
            .find(|album| album.title == "Génesis")
            .unwrap();
        assert_eq!(genesis.track_ids, ["2", "1"]);
        assert_eq!(genesis.duration, 200.0);
        let found = library_album(&tracks, " GÉNESIS ", "peso pluma").unwrap();
        assert_eq!(found.track_ids, ["2", "1"]);
        assert!(library_album(&tracks, "", "Peso Pluma").is_none());
        assert!(library_album(&tracks, "Génesis", "Karol G").is_none());
    }
}
