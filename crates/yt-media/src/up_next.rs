//! Watch-next discovery: YouTube's automatic Mix and the related videos of one seed.
use crate::{
    search::{self, badge_duration, parse_card},
    youtube::{is_video_id, thumbnail_url, watch_url},
    SearchResult,
};
use innertube_rs::parser::nodes::misc::text::TextNode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

/// Largest number of entries `YoutubeClient::up_next` returns per list.
pub const UP_NEXT_ITEM_LIMIT: usize = 50;

// End screens, engagement panels and the autoplay endpoint repeat cards or add
// unrelated ones, so only these two subtrees of a watch-next response are read.
const MIX_ROOT: &str = "/contents/twoColumnWatchNextResults/playlist/playlist/contents";
const RELATED_ROOT: &str =
    "/contents/twoColumnWatchNextResults/secondaryResults/secondaryResults/results";
const MIX_CARD: &str = "playlistPanelVideoRenderer";
/// Shorts shelves hold vertical clips, not songs.
const SHORTS_SHELF: &str = "reelShelfRenderer";
/// Separates the channel from view counts or album details in a byline.
const BYLINE_SEPARATOR: &str = " • ";
/// Mix entries carry their duration as clock text, or only in a time overlay.
const DURATION_FIELDS: [&str; 2] = ["lengthText", "thumbnailOverlays"];
/// Real cards sit two to four levels below each root; deeper input is not a card list.
const MAX_DEPTH: usize = 32;

/// Mix and related videos for one seed video, each list in provider order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpNext {
    /// YouTube's automatic Mix (radio) for the seed in playlist order; seed excluded.
    pub mix: Vec<SearchResult>,
    /// Watch-page related videos; the seed and every entry of the provider's Mix
    /// are excluded, including Mix entries beyond the requested limit.
    pub related: Vec<SearchResult>,
}

/// Read both lists from one watch-next response, keeping at most `limit` entries each.
pub(crate) fn parse_up_next(value: &Value, seed_id: &str, limit: usize) -> UpNext {
    let mut seen = HashSet::from([seed_id.to_owned()]);
    let mut mix = distinct(
        cards(value.pointer(MIX_ROOT), &[MIX_CARD]).filter_map(parse_mix_card),
        &mut seen,
    );
    let mut related = distinct(
        cards(value.pointer(RELATED_ROOT), &search::CARD_KEYS).filter_map(parse_card),
        &mut seen,
    );
    mix.truncate(limit);
    related.truncate(limit);
    UpNext { mix, related }
}

/// Inner objects of the `keys` renderers below `root`, in document order.
fn cards<'a>(root: Option<&'a Value>, keys: &[&str]) -> impl Iterator<Item = &'a Value> {
    let mut found = Vec::new();
    if let Some(root) = root {
        collect_cards(root, keys, 0, &mut found);
    }
    found.into_iter()
}

fn collect_cards<'a>(value: &'a Value, keys: &[&str], depth: usize, found: &mut Vec<&'a Value>) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::Object(object) => {
            if let Some(card) = keys.iter().find_map(|key| object.get(*key)) {
                found.push(card);
                return;
            }
            for (key, child) in object {
                if key != SHORTS_SHELF {
                    collect_cards(child, keys, depth + 1, found);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_cards(item, keys, depth + 1, found);
            }
        }
        _ => {}
    }
}

/// First occurrence of each id not already `seen`, with only trusted thumbnails.
fn distinct(
    videos: impl Iterator<Item = SearchResult>,
    seen: &mut HashSet<String>,
) -> Vec<SearchResult> {
    videos
        .filter(|video| seen.insert(video.id.clone()))
        .map(trusted_thumbnail)
        .collect()
}

/// Deleted or blocked Mix entries stay listed with an `unplayableText`
/// explanation instead of becoming playable, so they are skipped.
fn parse_mix_card(card: &Value) -> Option<SearchResult> {
    let id = card.get("videoId")?.as_str()?;
    if !is_video_id(id) || card.get("unplayableText").is_some() {
        return None;
    }
    let title = text(card.get("title"))?;
    let channel = card
        .get("longBylineText")
        .or_else(|| card.get("shortBylineText"))
        .and_then(|byline| text(Some(byline)))
        .and_then(|byline| {
            byline
                .split(BYLINE_SEPARATOR)
                .next()
                .map(|channel| channel.trim().to_owned())
        })
        .unwrap_or_default();
    let duration_seconds = DURATION_FIELDS
        .iter()
        .find_map(|field| badge_duration(card.get(*field)?, 0))
        .unwrap_or(0);
    // YouTube lists card thumbnails from smallest to largest.
    let thumbnail = card
        .pointer("/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|thumbnails| {
            thumbnails
                .iter()
                .rev()
                .find_map(|thumbnail| thumbnail.get("url")?.as_str())
        })
        .unwrap_or_default()
        .to_owned();
    Some(SearchResult {
        url: watch_url(id),
        id: id.to_owned(),
        title,
        thumbnail,
        duration_seconds,
        channel,
        view_count: 0,
    })
}

fn text(value: Option<&Value>) -> Option<String> {
    TextNode::from_value(value?)
        .map(|node| node.text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// Card thumbnails come from the response body; keep only YouTube image hosts.
fn trusted_thumbnail(mut video: SearchResult) -> SearchResult {
    if !Url::parse(&video.thumbnail).is_ok_and(|url| thumbnail_url(&url)) {
        video.thumbnail.clear();
    }
    video
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SEED: &str = "seedVideo00";

    fn watch_next(mix: Vec<Value>, related: Vec<Value>) -> Value {
        json!({"contents":{"twoColumnWatchNextResults":{
            "playlist":{"playlist":{"title":"Mix","contents":mix}},
            "secondaryResults":{"secondaryResults":{"results":related}}
        }}})
    }

    fn panel(id: &str, title: &str) -> Value {
        json!({"playlistPanelVideoRenderer":{
            "videoId":id,
            "title":{"accessibility":{"accessibilityData":{"label":format!("{title} 3 minutes")}},"simpleText":title},
            "longBylineText":{"runs":[{"text":"Channel"}]},
            "lengthText":{"accessibility":{"accessibilityData":{"label":"3 minutes, 5 seconds"}},"simpleText":"3:05"},
            "thumbnail":{"thumbnails":[
                {"url":format!("https://i.ytimg.com/vi/{id}/default.jpg"),"width":120,"height":90},
                {"url":format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),"width":480,"height":360}
            ]}
        }})
    }

    fn compact(id: &str, title: &str) -> Value {
        json!({"compactVideoRenderer":{
            "videoId":id,
            "title":{"simpleText":title},
            "longBylineText":{"runs":[{"text":"Related channel"}]},
            "lengthText":{"simpleText":"2:30"}
        }})
    }

    fn lockup(id: &str, kind: &str) -> Value {
        json!({"lockupViewModel":{
            "contentId":id,
            "contentType":kind,
            "metadata":{"lockupMetadataViewModel":{
                "title":{"content":"Lockup song"},
                "metadata":{"contentMetadataViewModel":{"metadataRows":[
                    {"metadataParts":[{"text":{"content":"Lockup channel"}}]}
                ]}}
            }},
            "contentImage":{"thumbnailViewModel":{"overlays":[
                {"thumbnailBottomOverlayViewModel":{"badges":[{"thumbnailBadgeViewModel":{"text":"3:31"}}]}}
            ]}}
        }})
    }

    fn ids(videos: &[SearchResult]) -> Vec<&str> {
        videos.iter().map(|video| video.id.as_str()).collect()
    }

    #[test]
    fn real_watch_next_shape_yields_mix_and_related_without_shorts_or_playlists() {
        let value: Value =
            serde_json::from_str(include_str!("../tests/fixtures/watch_next.json")).unwrap();
        let up_next = parse_up_next(&value, "fixture0000", UP_NEXT_ITEM_LIMIT);
        assert_eq!(ids(&up_next.mix), ["fixture0001", "fixture0002"]);
        assert_eq!(up_next.mix[0].title, "Title 1");
        assert_eq!(up_next.mix[0].channel, "Channel 1");
        assert_eq!(up_next.mix[0].duration_seconds, 287);
        assert_eq!(up_next.mix[1].duration_seconds, 360);
        assert_eq!(
            up_next.mix[0].url,
            "https://www.youtube.com/watch?v=fixture0001"
        );
        assert_eq!(
            up_next.mix[0].thumbnail,
            "https://i.ytimg.com/vi/fixture0001/hqdefault.jpg"
        );
        assert_eq!(ids(&up_next.related), ["fixture0003", "fixture0005"]);
        assert_eq!(up_next.related[0].title, "Title 3");
        assert_eq!(up_next.related[0].channel, "Meta 0");
        assert_eq!(up_next.related[0].duration_seconds, 211);
    }

    #[test]
    fn seed_and_duplicates_are_excluded_within_and_across_lists() {
        let value = watch_next(
            vec![
                panel(SEED, "Seed"),
                panel("mixSongAAAA", "A"),
                panel("mixSongAAAA", "A again"),
                panel("mixSongBBBB", "B"),
            ],
            vec![
                compact(SEED, "Seed"),
                compact("mixSongAAAA", "A related"),
                lockup("relatedCCCC", "LOCKUP_CONTENT_TYPE_VIDEO"),
                compact("mixSongBBBB", "B related"),
                compact("relatedCCCC", "C again"),
            ],
        );
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        assert_eq!(ids(&up_next.mix), ["mixSongAAAA", "mixSongBBBB"]);
        assert_eq!(up_next.mix[0].title, "A");
        assert_eq!(ids(&up_next.related), ["relatedCCCC"]);
        assert_eq!(up_next.related[0].channel, "Lockup channel");
        assert_eq!(up_next.related[0].duration_seconds, 211);
    }

    #[test]
    fn invalid_unplayable_and_non_video_entries_are_dropped() {
        let mut unplayable = panel("unplayable1", "Removed");
        unplayable["playlistPanelVideoRenderer"]["unplayableText"] =
            json!({"simpleText":"Video unavailable"});
        let mut untitled = panel("untitled111", "");
        untitled["playlistPanelVideoRenderer"]["title"] = json!({"simpleText":"  "});
        let value = watch_next(
            vec![
                panel("short", "Too short"),
                panel("bad id!!!!!", "Bad characters"),
                unplayable,
                untitled,
                panel("playable111", "Playable"),
            ],
            vec![
                compact("bad/id/1234", "Bad"),
                lockup("PLplaylist01", "LOCKUP_CONTENT_TYPE_PLAYLIST"),
                lockup("playlist111", "LOCKUP_CONTENT_TYPE_PLAYLIST"),
                compact("related1111", "Related"),
            ],
        );
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        assert_eq!(ids(&up_next.mix), ["playable111"]);
        assert_eq!(ids(&up_next.related), ["related1111"]);
    }

    #[test]
    fn bylines_keep_only_the_channel() {
        let mut runs = panel("runsByline1", "Runs");
        runs["playlistPanelVideoRenderer"]["longBylineText"] =
            json!({"runs":[{"text":"Artist"},{"text":" • "},{"text":"Album"}]});
        let mut simple = panel("simpleBylin", "Simple");
        simple["playlistPanelVideoRenderer"]
            .as_object_mut()
            .unwrap()
            .remove("longBylineText");
        simple["playlistPanelVideoRenderer"]["shortBylineText"] =
            json!({"simpleText":" Channel • 1.2M views "});
        let mut missing = panel("noByline111", "Missing");
        missing["playlistPanelVideoRenderer"]
            .as_object_mut()
            .unwrap()
            .remove("longBylineText");
        let up_next = parse_up_next(
            &watch_next(vec![runs, simple, missing], vec![]),
            SEED,
            UP_NEXT_ITEM_LIMIT,
        );
        let channels: Vec<_> = up_next.mix.iter().map(|v| v.channel.as_str()).collect();
        assert_eq!(channels, ["Artist", "Channel", ""]);
    }

    #[test]
    fn durations_read_clock_text_or_time_overlays_and_never_prose() {
        let with_length = |id: &str, length: Value| {
            let mut card = panel(id, "Song");
            card["playlistPanelVideoRenderer"]["lengthText"] = length;
            card
        };
        let mut overlay = panel("overlayTime", "Overlay");
        let entry = overlay["playlistPanelVideoRenderer"]
            .as_object_mut()
            .unwrap();
        entry.remove("lengthText");
        entry.insert(
            "thumbnailOverlays".into(),
            json!([{"thumbnailOverlayTimeStatusRenderer":{"text":{"simpleText":"2:10"}}}]),
        );
        let value = watch_next(
            vec![
                with_length("longVideo11", json!({"simpleText":"1:02:03"})),
                with_length("runsLength1", json!({"runs":[{"text":"3:05"}]})),
                overlay,
                with_length("liveStream1", json!({"simpleText":"LIVE"})),
                with_length(
                    "proseOnly11",
                    json!({"accessibility":{"accessibilityData":{"label":"4 minutes, 42 seconds"}}}),
                ),
                with_length("badClock111", json!({"simpleText":"4:99"})),
            ],
            vec![],
        );
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        let durations: Vec<_> = up_next.mix.iter().map(|v| v.duration_seconds).collect();
        assert_eq!(durations, [3723, 185, 130, 0, 0, 0]);
    }

    #[test]
    fn limit_truncates_each_list_while_related_still_excludes_the_whole_mix() {
        let value = watch_next(
            vec![
                panel("mixSongAAAA", "A"),
                panel("mixSongBBBB", "B"),
                panel("mixSongCCCC", "C"),
            ],
            vec![
                compact("mixSongCCCC", "C related"),
                compact("relatedDDDD", "D"),
                compact("relatedEEEE", "E"),
            ],
        );
        let up_next = parse_up_next(&value, SEED, 1);
        assert_eq!(ids(&up_next.mix), ["mixSongAAAA"]);
        assert_eq!(ids(&up_next.related), ["relatedDDDD"]);
        assert_eq!(parse_up_next(&value, SEED, 0), UpNext::default());
    }

    #[test]
    fn missing_mix_panel_still_returns_related_videos() {
        let mut value = watch_next(vec![], vec![compact("related1111", "Related")]);
        value["contents"]["twoColumnWatchNextResults"]
            .as_object_mut()
            .unwrap()
            .remove("playlist");
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        assert!(up_next.mix.is_empty());
        assert_eq!(ids(&up_next.related), ["related1111"]);
        assert_eq!(
            parse_up_next(&json!({"contents":{}}), SEED, UP_NEXT_ITEM_LIMIT),
            UpNext::default()
        );
    }

    #[test]
    fn cards_outside_both_lists_and_inside_shorts_shelves_are_ignored() {
        let mut value = watch_next(
            vec![panel("mixSongAAAA", "A")],
            vec![
                json!({"reelShelfRenderer":{"items":[lockup("shortClip11", "LOCKUP_CONTENT_TYPE_VIDEO")]}}),
                json!({"itemSectionRenderer":{"contents":[compact("sectioned11", "Sectioned")]}}),
            ],
        );
        value["playerOverlays"] = json!({"endScreen":[compact("endScreen11", "End")]});
        value["engagementPanels"] = json!([panel("engagement1", "Panel")]);
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        assert_eq!(ids(&up_next.mix), ["mixSongAAAA"]);
        assert_eq!(ids(&up_next.related), ["sectioned11"]);
    }

    #[test]
    fn nesting_beyond_the_depth_bound_is_not_traversed() {
        let mut nested = compact("tooDeep1111", "Deep");
        for _ in 0..MAX_DEPTH {
            nested = json!([nested]);
        }
        let value = watch_next(vec![], vec![nested, compact("shallow1111", "Shallow")]);
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        assert_eq!(ids(&up_next.related), ["shallow1111"]);
    }

    #[test]
    fn thumbnails_use_the_largest_entry_and_only_youtube_image_hosts() {
        let with_thumbnail = |id: &str, url: &str| {
            let mut card = panel(id, "Song");
            card["playlistPanelVideoRenderer"]["thumbnail"] = json!({"thumbnails":[{"url":url}]});
            card
        };
        let value = watch_next(
            vec![
                panel("largestThmb", "Largest"),
                with_thumbnail("foreignHost", "https://images.example/cover.jpg"),
                with_thumbnail("plainHttp11", "http://i.ytimg.com/vi/plainHttp11/hq.jpg"),
                with_thumbnail("credentials", "https://user@i.ytimg.com/vi/x/hq.jpg"),
            ],
            vec![],
        );
        let up_next = parse_up_next(&value, SEED, UP_NEXT_ITEM_LIMIT);
        let thumbnails: Vec<_> = up_next.mix.iter().map(|v| v.thumbnail.as_str()).collect();
        assert_eq!(
            thumbnails,
            [
                "https://i.ytimg.com/vi/largestThmb/hqdefault.jpg",
                "",
                "",
                ""
            ]
        );
    }
}
