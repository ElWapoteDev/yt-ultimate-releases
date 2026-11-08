use crate::{
    youtube::{parse_duration, parse_views},
    SearchResult,
};
use innertube_rs::parser::nodes::video::VideoNode;
use serde_json::Value;
use std::collections::HashMap;

/// Parse only complete renderer cards. The upstream generic tree walker also
/// recognizes nested watch endpoints as videos, producing duplicate empty rows.
pub(crate) fn parse_results(value: &Value, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut indices = HashMap::new();
    collect(value, &mut results, &mut indices, 0);
    results.truncate(limit);
    results
}

fn collect(
    value: &Value,
    results: &mut Vec<SearchResult>,
    indices: &mut HashMap<String, usize>,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    if let Some(object) = value.as_object() {
        for key in [
            "videoRenderer",
            "compactVideoRenderer",
            "gridVideoRenderer",
            "lockupViewModel",
        ] {
            if let Some(card) = object.get(key) {
                if let Some(video) = parse_card(card) {
                    if let Some(&index) = indices.get(&video.id) {
                        let previous = &mut results[index];
                        if previous.duration_seconds == 0 {
                            previous.duration_seconds = video.duration_seconds;
                        }
                        if previous.channel.is_empty() {
                            previous.channel = video.channel;
                        }
                        if previous.thumbnail.is_empty() {
                            previous.thumbnail = video.thumbnail;
                        }
                        previous.view_count = previous.view_count.max(video.view_count);
                    } else {
                        indices.insert(video.id.clone(), results.len());
                        results.push(video);
                    }
                }
                return;
            }
        }
        for child in object.values() {
            collect(child, results, indices, depth + 1);
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            collect(child, results, indices, depth + 1);
        }
    }
}

fn parse_card(card: &Value) -> Option<SearchResult> {
    if card
        .get("contentType")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "LOCKUP_CONTENT_TYPE_VIDEO")
    {
        return None;
    }
    let video = VideoNode::from_value(card)?;
    if video.id.len() != 11
        || !video
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    let duration_seconds = video
        .duration_ms
        .map(|ms| ms / 1000)
        .or_else(|| video.duration.as_deref().map(parse_duration))
        .filter(|v| *v > 0)
        .or_else(|| badge_duration(card.get("contentImage").unwrap_or(&Value::Null), 0))
        .unwrap_or(0);
    Some(SearchResult {
        url: format!("https://www.youtube.com/watch?v={}", video.id),
        id: video.id,
        title: video.title,
        thumbnail: video
            .thumbnails
            .thumbnails
            .last()
            .map(|t| t.url.clone())
            .unwrap_or_default(),
        channel: video.author.map(|a| a.name).unwrap_or_default(),
        duration_seconds,
        view_count: video.view_count.as_deref().map(parse_views).unwrap_or(0),
    })
}

/// Badge order and wrapper names vary. Search the thumbnail subtree, never
/// title/accessibility prose, and accept only a clock-shaped duration.
fn badge_duration(value: &Value, depth: usize) -> Option<u64> {
    if depth > 16 {
        return None;
    }
    match value {
        Value::String(text) => {
            let text = text.trim();
            let parts: Vec<_> = text.split(':').collect();
            if !(2..=3).contains(&parts.len())
                || parts
                    .iter()
                    .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            parts
                .iter()
                .skip(1)
                .all(|p| p.parse::<u64>().is_ok_and(|v| v < 60))
                .then(|| parse_duration(text))
                .filter(|v| *v > 0)
        }
        Value::Array(values) => values.iter().find_map(|v| badge_duration(v, depth + 1)),
        Value::Object(values) => values.values().find_map(|v| badge_duration(v, depth + 1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn modern_cards_ignore_nested_endpoints_and_read_later_duration_badges() {
        let value = json!({"contents":[
          {"watchEndpoint":{"videoId":"abcdefghijk"}},
          {"lockupViewModel":{"contentId":"abcdefghijk","contentType":"LOCKUP_CONTENT_TYPE_VIDEO",
            "metadata":{"lockupMetadataViewModel":{"title":{"content":"Song"}}},
            "contentImage":{"thumbnailViewModel":{"overlays":[{"badge":{"text":"HD"}},{"badge":{"text":"3:08"}}]}},
            "rendererContext":{"commandContext":{"onTap":{"innertubeCommand":{"watchEndpoint":{"videoId":"abcdefghijk"}}}}}}},
          {"videoRenderer":{"videoId":"abcdefghijk","title":{"simpleText":"Song"},"lengthText":{"simpleText":"3:08"}}},
          {"lockupViewModel":{"contentId":"PLabcdefghijk","contentType":"LOCKUP_CONTENT_TYPE_PLAYLIST"}}
        ]});
        let rows = parse_results(&value, 25);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Song");
        assert_eq!(rows[0].duration_seconds, 188);
    }
    #[test]
    fn duplicate_cards_enrich_first_position_without_inventing_live_durations() {
        let value = json!({"contents":[
          {"videoRenderer":{"videoId":"abcdefghijk","title":{"simpleText":"First"}}},
          {"videoRenderer":{"videoId":"lmnopqrstuv","title":{"simpleText":"Live"},"lengthText":{"simpleText":"LIVE"}}},
          {"videoRenderer":{"videoId":"abcdefghijk","title":{"simpleText":"First"},"lengthText":{"simpleText":"1:02:03"},"ownerText":{"runs":[{"text":"Artist"}]}}}
        ]});
        let rows = parse_results(&value, 25);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].duration_seconds, 3723);
        assert_eq!(rows[0].channel, "Artist");
        assert_eq!(rows[1].duration_seconds, 0);
        assert_eq!(parse_results(&value, 1).len(), 1);
        assert_eq!(badge_duration(&json!({"text":"3:99"}), 0), None);
    }
}
