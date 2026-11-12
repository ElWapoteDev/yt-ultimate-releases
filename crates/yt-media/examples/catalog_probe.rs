//! Live check of the music catalog for one artist or query: suggestions,
//! typed search, artist page with concerts, album, credits, song context and
//! new uploads.
//! Prints counts and a few fields; image and ticket links are reduced to their
//! host. Reads public metadata only; no stream URL, cookie or token is used.
//!
//! `cargo run -p yt-media --example catalog_probe -- "peso pluma" [video]`
//!
//! Credits are printed in full for `video` (an id or a link), else for the
//! artist's most popular song, and for the artist's first music video.
use yt_media::{
    AlbumSummary, ArtistSummary, CancellationToken, SearchResult, SongContext, YoutubeClient,
};

const DEFAULT_QUERY: &str = "peso pluma";
const SAMPLE_ROWS: usize = 3;
/// Characters of the query sent as the suggestion prefix.
const PREFIX_CHARS: usize = 6;
const PER_KIND: usize = 5;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_QUERY.into());
    let youtube = YoutubeClient::new()?;
    let cancel = CancellationToken::new;

    let prefix: String = query.chars().take(PREFIX_CHARS).collect();
    let suggestions = youtube.suggestions(&prefix, cancel()).await?;
    println!(
        "Suggestions for {prefix:?}: {} {suggestions:?}",
        suggestions.len()
    );

    let search = youtube.catalog_search(&query, PER_KIND, cancel()).await?;
    println!(
        "Catalog search: valid={} songs={} artists={} albums={} playlists={} videos={}",
        search.validate().is_ok(),
        search.songs.len(),
        search.artists.len(),
        search.albums.len(),
        search.playlists.len(),
        search.videos.len()
    );
    print_videos("  song", &search.songs);
    print_artists("  artist", &search.artists);
    print_albums("  album", &search.albums);
    print_albums("  playlist", &search.playlists);
    print_videos("  video", &search.videos);

    let Some(found) = youtube.find_artist(&query, cancel()).await? else {
        println!("find_artist: no match");
        return Ok(());
    };
    println!(
        "find_artist: {} {} [{}]",
        found.id, found.name, found.subscribers
    );

    let artist = youtube.artist(&found.id, cancel()).await?;
    println!(
        "Artist: valid={} {} [{}] description={} chars avatar={} banner={}",
        artist.validate().is_ok(),
        artist.name,
        artist.subscribers,
        artist.description.chars().count(),
        host(&artist.avatar),
        host(&artist.banner)
    );
    println!(
        "  top_songs={} albums={} singles={} videos={} related={} events={}",
        artist.top_songs.len(),
        artist.albums.len(),
        artist.singles.len(),
        artist.videos.len(),
        artist.related.len(),
        artist.events.len()
    );
    print_videos("  top song", &artist.top_songs);
    print_albums("  album", &artist.albums);
    print_albums("  single", &artist.singles);
    print_videos("  video", &artist.videos);
    print_artists("  related", &artist.related);
    for event in artist.events.iter().take(SAMPLE_ROWS) {
        println!(
            "  event {} | {} | {} | {} | {}",
            event.title,
            event.date_text,
            event.venue,
            event.location,
            host(&event.url)
        );
    }

    if let Some(first) = artist.albums.first().or(artist.singles.first()) {
        let album = youtube.album(&first.id, cancel()).await?;
        println!(
            "Album: valid={} {} | {} ({:?}) | {:?} {:?} | cover={} | description={} chars | tracks={}",
            album.validate().is_ok(),
            album.title,
            album.artist,
            album.artist_id,
            album.year,
            album.kind,
            host(&album.thumbnail),
            album.description.chars().count(),
            album.tracks.len()
        );
        print_videos("  track", &album.tracks);
    }

    let credited = std::env::args()
        .nth(2)
        .or_else(|| artist.top_songs.first().map(|song| song.id.clone()));
    let music_video = artist.videos.first().map(|video| video.id.clone());
    for video in credited.into_iter().chain(music_video) {
        let credits = youtube.credits(&video, cancel()).await?;
        println!(
            "Credits of {video}: valid={} provider={:?} label={:?} released={:?} roles={}",
            credits.validate().is_ok(),
            credits.provider,
            credits.label,
            credits.released,
            credits.roles.len()
        );
        for role in &credits.roles {
            println!("  {}: {}", role.role, role.names.join(", "));
        }
    }

    // A popular song (an art track) and a music video of the artist.
    for video in [artist.top_songs.first(), artist.videos.first()]
        .into_iter()
        .flatten()
    {
        let context = youtube.song_context(&video.id, cancel()).await?;
        print_context(&video.id, &context);
        if let Some(album) = &context.album {
            let opened = youtube.album(&album.id, cancel()).await?;
            println!("  album opens: {} tracks", opened.tracks.len());
        }
    }

    let releases = youtube.new_releases(&artist.name, cancel()).await?;
    println!("New releases of {}: {}", artist.name, releases.len());
    print_videos("  upload", &releases);
    Ok(())
}

fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "-".into())
}

fn print_context(video: &str, context: &SongContext) {
    let artists: Vec<String> = context
        .artists
        .iter()
        .map(|artist| format!("{} ({})", artist.name, artist.id))
        .collect();
    println!(
        "Song context of {video}: valid={} artists={artists:?}",
        context.validate().is_ok()
    );
    print_albums("  album", context.album.as_slice());
}

fn print_videos(label: &str, videos: &[SearchResult]) {
    for video in videos.iter().take(SAMPLE_ROWS) {
        println!(
            "{label} {} | {} | {} | {}:{:02} | {}",
            video.id,
            video.title,
            video.channel,
            video.duration_seconds / 60,
            video.duration_seconds % 60,
            host(&video.thumbnail)
        );
    }
}

fn print_artists(label: &str, artists: &[ArtistSummary]) {
    for artist in artists.iter().take(SAMPLE_ROWS) {
        println!(
            "{label} {} | {} | {} | {}",
            artist.id,
            artist.name,
            artist.subscribers,
            host(&artist.thumbnail)
        );
    }
}

fn print_albums(label: &str, albums: &[AlbumSummary]) {
    for album in albums.iter().take(SAMPLE_ROWS) {
        println!(
            "{label} {} | {} | {} | {:?} {:?} | {}",
            album.id,
            album.title,
            album.artist,
            album.year,
            album.kind,
            host(&album.thumbnail)
        );
    }
}
