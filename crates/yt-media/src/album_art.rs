//! Album covers of music videos. YouTube identifies the song in most music
//! uploads (official videos, lyric videos, audio uploads, visualizers) and
//! describes it in the watch page's structured description, together with the
//! square cover of its album on Google's resizable image hosts.

use crate::youtube::{is_video_id, thumbnail_url};
use innertube_rs::parser::nodes::misc::text::TextNode;
use serde_json::Value;
use url::Url;

/// Engagement panel that holds the structured description of the video.
const DESCRIPTION_PANEL: &str = "engagement-panel-structured-description";
/// The video's title inside the description, in the current layout and the
/// earlier one; web client versions differ.
const DESCRIPTION_TITLES: [&str; 2] = [
    "/videoTitleHeaderViewModel/videoTitle",
    "/videoDescriptionHeaderRenderer/title",
];
/// Cards under the player, where every layout also repeats the title.
const PRIMARY_CARDS: &str = "/contents/twoColumnWatchNextResults/results/results/contents";
const PRIMARY_TITLE: &str = "/videoPrimaryInfoRenderer/title";
/// Image style of the cards that describe an identified song.
const SONG_CARD_STYLE: &str = "VIDEO_ATTRIBUTE_IMAGE_STYLE_SQUARE";
/// Google image hosts that serve any size of an image from its base URL.
const RESIZABLE_HOSTS: [&str; 2] = ["yt3.googleusercontent.com", "yt3.ggpht.com"];
/// Size suffix that asks a resizable host for the original image.
const ORIGINAL_EDGE: u32 = 0;
/// Path folders of YouTube video thumbnails; the video id follows them.
const THUMBNAIL_FOLDERS: [&str; 2] = ["vi", "vi_webp"];

/// Video id of a YouTube video thumbnail URL (`https://i.ytimg.com/vi/<id>/…`),
/// so the thumbnail can be swapped for the video's album cover.
pub fn thumbnail_video_id(url: &str) -> Option<&str> {
    let (host, path) = url.strip_prefix("https://")?.split_once('/')?;
    let youtube_host = (host == "ytimg.com" || host.ends_with(".ytimg.com"))
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    let mut segments = path.split('/');
    let folder = segments.next()?;
    let id = segments.next()?;
    (youtube_host
        && THUMBNAIL_FOLDERS.contains(&folder)
        && is_video_id(id)
        && segments.next().is_some())
    .then_some(id)
}

/// `url` resized so its longest side is at most `edge` pixels, or the original
/// image for `None`. Only images on Google's resizable hosts change; those
/// hosts never upscale, so an `edge` beyond the original returns the original.
/// Any other URL (video thumbnails, local covers) is returned unchanged.
pub fn sized_artwork(url: &str, edge: Option<u32>) -> String {
    match resizable_base(url) {
        Some(base) => format!("{base}=s{}", edge.unwrap_or(ORIGINAL_EDGE)),
        None => url.to_owned(),
    }
}

/// The URL without its size suffix, when it is a plain `https` image on a
/// resizable host.
fn resizable_base(url: &str) -> Option<&str> {
    let parsed = Url::parse(url).ok()?;
    let resizable = thumbnail_url(&parsed)
        && parsed
            .host_str()
            .is_some_and(|host| RESIZABLE_HOSTS.contains(&host))
        && parsed.query().is_none()
        && parsed.fragment().is_none();
    resizable.then(|| url.split_once('=').map_or(url, |(base, _)| base))
}

/// Base URL of the album cover of the one song a watch-next response
/// identifies in its video's description. `None` when the description names
/// no song (non-music videos), several songs (mixes, medleys), or a song the
/// video's title does not mention (background music in other videos), and
/// when the cover is not on a resizable YouTube image host.
pub(crate) fn parse_album_artwork(response: &Value) -> Option<String> {
    let panel = response
        .get("engagementPanels")?
        .as_array()?
        .iter()
        .filter_map(|panel| panel.get("engagementPanelSectionListRenderer"))
        .find(|panel| {
            panel.get("panelIdentifier").and_then(Value::as_str) == Some(DESCRIPTION_PANEL)
        })?;
    let items = panel
        .pointer("/content/structuredDescriptionContentRenderer/items")?
        .as_array()?;
    let title = items
        .iter()
        .find_map(|item| {
            DESCRIPTION_TITLES
                .iter()
                .find_map(|pointer| text(item.pointer(pointer)))
        })
        .or_else(|| {
            response
                .pointer(PRIMARY_CARDS)?
                .as_array()?
                .iter()
                .find_map(|card| text(card.pointer(PRIMARY_TITLE)))
        })?;
    let mut songs = items
        .iter()
        .filter_map(|item| {
            item.pointer("/horizontalCardListRenderer/cards")?
                .as_array()
        })
        .flatten()
        .filter_map(|card| card.get("videoAttributeViewModel"))
        .filter(|card| card.get("imageStyle").and_then(Value::as_str) == Some(SONG_CARD_STYLE));
    let song = songs.next()?;
    if songs.next().is_some() || !names_song(&title, song.get("title")?.as_str()?) {
        return None;
    }
    let url = song
        .pointer("/image/sources")?
        .as_array()?
        .iter()
        .max_by_key(|source| source.get("width").and_then(Value::as_u64).unwrap_or(0))?
        .get("url")?
        .as_str()?;
    resizable_base(url).map(str::to_owned)
}

/// Whether a video titled `video` is an upload of `song`: the song's name,
/// without bracketed details such as «(Remix)», appears in the title as whole
/// words, ignoring case and accents.
fn names_song(video: &str, song: &str) -> bool {
    yt_core::contains_words(video, &without_brackets(song))
}

fn without_brackets(text: &str) -> String {
    let mut depth = 0usize;
    text.chars()
        .filter(|character| match character {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                false
            }
            _ => depth == 0,
        })
        .collect()
}

fn text(value: Option<&Value>) -> Option<String> {
    TextNode::from_value(value?)
        .map(|node| node.text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const COVER: &str = "https://yt3.googleusercontent.com/albumCoverPlaceholder";

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/album_art_next.json")).unwrap()
    }

    const ITEMS: &str = "/engagementPanels/1/engagementPanelSectionListRenderer/content/structuredDescriptionContentRenderer/items";

    /// `fixture()` with its description cards replaced by `cards` and its
    /// video title by `title`.
    fn description(title: &str, cards: Vec<Value>) -> Value {
        let mut response = fixture();
        let items = response.pointer_mut(ITEMS).unwrap();
        items[0]["videoTitleHeaderViewModel"]["videoTitle"] = json!({"content": title});
        items[3]["horizontalCardListRenderer"]["cards"] = Value::Array(cards);
        response
    }

    fn song(title: &str, style: &str, url: &str) -> Value {
        json!({"videoAttributeViewModel": {
            "image": {"sources": [{"url": url}]},
            "imageStyle": style,
            "title": title,
            "subtitle": "Artist",
            "secondarySubtitle": {"content": "Album"}
        }})
    }

    #[test]
    fn the_identified_song_gives_its_album_cover_without_a_size_suffix() {
        assert_eq!(parse_album_artwork(&fixture()).as_deref(), Some(COVER));
        let sized = description(
            "Artist - Song Title (Official Video)",
            vec![song(
                "Song Title",
                SONG_CARD_STYLE,
                &format!("{COVER}=w544-h544-l90-rj"),
            )],
        );
        assert_eq!(parse_album_artwork(&sized).as_deref(), Some(COVER));
    }

    #[test]
    fn earlier_layouts_name_the_title_elsewhere() {
        let mut earlier = fixture();
        let items = earlier.pointer_mut(ITEMS).unwrap().as_array_mut().unwrap();
        items.remove(0);
        items[0]["videoDescriptionHeaderRenderer"]["title"] =
            json!({"runs": [{"text": "Artist - Song Title"}]});
        assert_eq!(parse_album_artwork(&earlier).as_deref(), Some(COVER));
        // Without a description title, the title under the player is used.
        items_without_titles(&mut earlier);
        assert_eq!(parse_album_artwork(&earlier).as_deref(), Some(COVER));
        earlier["contents"] = json!({});
        assert_eq!(parse_album_artwork(&earlier), None);
    }

    fn items_without_titles(response: &mut Value) {
        for item in response.pointer_mut(ITEMS).unwrap().as_array_mut().unwrap() {
            if let Some(header) = item.get_mut("videoDescriptionHeaderRenderer") {
                header.as_object_mut().unwrap().remove("title");
            }
        }
    }

    #[test]
    fn titles_match_whole_words_ignoring_case_accents_and_bracketed_details() {
        assert!(names_song(
            "Artista - Tití Me Preguntó (Video Oficial) | Álbum",
            "Titi Me Preguntó"
        ));
        assert!(names_song(
            "Singer - Song (Lyrics) ft. Guest",
            "Song (Remix)"
        ));
        assert!(names_song(
            "グループ「夜に駆ける」 Official Music Video",
            "夜に駆ける"
        ));
        assert!(!names_song("Songbird live session", "Song"));
        assert!(!names_song("Travel vlog in Paris", "Background Track"));
        assert!(!names_song("Anything", "(Only Brackets)"));
    }

    #[test]
    fn videos_without_one_named_song_or_a_trusted_cover_have_no_album_art() {
        let card = |title: &str| song(title, SONG_CARD_STYLE, COVER);
        for response in [
            json!({"engagementPanels": []}),
            description("Artist - Song Title", vec![]),
            description(
                "Mix 2026 - one hour",
                vec![card("Song Title"), card("Other Song")],
            ),
            description("Travel vlog", vec![card("Song Title")]),
            description(
                "Artist - Song Title",
                vec![song(
                    "Song Title",
                    "VIDEO_ATTRIBUTE_IMAGE_STYLE_CIRCLE",
                    COVER,
                )],
            ),
            description(
                "Artist - Song Title",
                vec![song(
                    "Song Title",
                    SONG_CARD_STYLE,
                    "https://images.example/cover.jpg",
                )],
            ),
            description(
                "Artist - Song Title",
                vec![song(
                    "Song Title",
                    SONG_CARD_STYLE,
                    "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg",
                )],
            ),
            description(
                "Artist - Song Title",
                vec![song(
                    "Song Title",
                    SONG_CARD_STYLE,
                    "http://yt3.googleusercontent.com/insecure",
                )],
            ),
        ] {
            assert_eq!(parse_album_artwork(&response), None, "{response}");
        }
    }

    #[test]
    fn video_thumbnails_name_their_video() {
        for (url, id) in [
            (
                "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg",
                Some("abcdefghijk"),
            ),
            (
                "https://i9.ytimg.com/vi_webp/abc-efg_ijk/maxresdefault.webp?v=1",
                Some("abc-efg_ijk"),
            ),
            ("https://i.ytimg.com/vi/short/hqdefault.jpg", None),
            ("https://i.ytimg.com/vi/abcdefghijk", None),
            ("https://i.ytimg.com/an/abcdefghijk/x.jpg", None),
            ("https://user@i.ytimg.com/vi/abcdefghijk/x.jpg", None),
            ("https://ytimg.com.evil.test/vi/abcdefghijk/x.jpg", None),
            ("http://i.ytimg.com/vi/abcdefghijk/x.jpg", None),
            (COVER, None),
        ] {
            assert_eq!(thumbnail_video_id(url), id, "{url}");
        }
    }

    #[test]
    fn resizable_covers_are_sized_and_other_images_stay_as_they_are() {
        assert_eq!(sized_artwork(COVER, Some(120)), format!("{COVER}=s120"));
        assert_eq!(
            sized_artwork(&format!("{COVER}=w544-h544-l90-rj"), None),
            format!("{COVER}=s0")
        );
        assert_eq!(
            sized_artwork("https://yt3.ggpht.com/avatar=s88-c-k-no-rj", Some(544)),
            "https://yt3.ggpht.com/avatar=s544"
        );
        for unchanged in [
            "https://i.ytimg.com/vi/abcdefghijk/hq720.jpg?sqp=abc",
            "/artwork/track-id",
            "https://yt3.googleusercontent.com/cover?size=1",
            "https://user:secret@yt3.googleusercontent.com/cover",
            "not a url",
        ] {
            assert_eq!(sized_artwork(unchanged, Some(120)), unchanged);
        }
    }
}
