//! Catalog queries from the metadata of the media that plays.

use yt_core::{artist_key, clean_artist, song_title, LyricsQuery};

/// Most queries derived for one song; each one is a catalog request.
pub const MAX_QUERIES: usize = 2;

/// What the application knows about the media that plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongMetadata<'a> {
    pub title: &'a str,
    /// Tag artist, or the channel of an upload.
    pub artist: &'a str,
    pub album: &'a str,
    /// Seconds; 0 when unknown.
    pub duration: f64,
    /// The title and artist come from an online upload (a YouTube title and
    /// channel), not from curated tags: a «Performer - Song» title names the
    /// performer, and the channel may be a label.
    pub upload: bool,
}

/// Catalog queries for `song`, best first, at most `MAX_QUERIES` and never
/// two for the same title and artist. Packaging such as «(Official Video)»
/// and featured artists are removed. An upload titled «Performer - Song» is
/// asked for as «Song» by that performer, then by the channel. Curated tags
/// keep their title, and a title with a « - » version suffix (« - Live»,
/// « - Remastered 2011») is asked for again without it. Empty when nothing
/// names a song.
pub fn queries(song: &SongMetadata<'_>) -> Vec<LyricsQuery> {
    let title = song_title(song.title);
    let artist = clean_artist(song.artist);
    let album = if song.upload { "" } else { song.album };
    let attempts: Vec<(String, String)> = match (song.upload, title.performer) {
        (true, Some(performer)) => vec![(title.song.clone(), performer), (title.song, artist)],
        (false, Some(head)) => vec![
            (format!("{head} - {}", title.song), artist.clone()),
            (head, artist),
        ],
        (_, None) => vec![(title.song, artist)],
    };
    let mut seen = Vec::new();
    attempts
        .into_iter()
        .filter(|(title, artist)| {
            let key = (yt_core::title_key(title), artist_key(artist));
            let fresh = !key.0.is_empty() && !seen.contains(&key);
            seen.push(key);
            fresh
        })
        .filter_map(|(title, artist)| LyricsQuery::new(&title, &artist, album, song.duration).ok())
        .take(MAX_QUERIES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song<'a>(title: &'a str, artist: &'a str, upload: bool) -> SongMetadata<'a> {
        SongMetadata {
            title,
            artist,
            album: "Album",
            duration: 215.0,
            upload,
        }
    }

    fn pairs(queries: &[LyricsQuery]) -> Vec<(&str, &str, &str)> {
        queries
            .iter()
            .map(|query| (query.title(), query.artist(), query.album()))
            .collect()
    }

    #[test]
    fn uploads_ask_for_the_performer_then_the_channel() {
        let vevo = queries(&song(
            "Enrique Iglesias - Bailando ft. Descemer Bueno (Official Video)",
            "EnriqueIglesiasVEVO",
            true,
        ));
        assert_eq!(pairs(&vevo), [("Bailando", "Enrique Iglesias", "")]);
        let label = queries(&song("Bad Bunny - Tití Me Preguntó", "Rimas Music", true));
        assert_eq!(
            pairs(&label),
            [
                ("Tití Me Preguntó", "Bad Bunny", ""),
                ("Tití Me Preguntó", "Rimas Music", "")
            ]
        );
        let topic = queries(&song("Tití Me Preguntó", "Bad Bunny - Topic", true));
        assert_eq!(pairs(&topic), [("Tití Me Preguntó", "Bad Bunny", "")]);
        assert_eq!(vevo[0].duration(), 215.0);
    }

    #[test]
    fn tags_keep_their_title_and_retry_without_a_version_suffix() {
        let tagged = queries(&song("Bohemian Rhapsody (Remastered 2011)", "Queen", false));
        assert_eq!(pairs(&tagged), [("Bohemian Rhapsody", "Queen", "Album")]);
        let suffixed = queries(&song("Yesterday - Remastered 2009", "The Beatles", false));
        assert_eq!(
            pairs(&suffixed),
            [
                ("Yesterday - Remastered 2009", "The Beatles", "Album"),
                ("Yesterday", "The Beatles", "Album")
            ]
        );
        assert!(queries(&song("(Official Video)", "Someone", true)).is_empty());
    }
}
