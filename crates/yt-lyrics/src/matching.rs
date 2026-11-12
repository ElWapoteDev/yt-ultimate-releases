//! Choosing the catalog record that belongs to the song that plays.

use yt_core::{artist_key, title_key, Lyrics, LyricsCandidate, LyricsQuery};

/// Thresholds and weights for accepting a catalog record. Word timing weighs
/// less than any score difference: among records with the best score, synced
/// lyrics with word timing win, and remaining ties keep catalog order. A
/// closer recording therefore keeps its line timing over word timing that
/// may be shifted against the recording that plays.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchConfig {
    /// Duration differences, in seconds, scored from best to worst. A record
    /// within the first bound scores most; one beyond the last is rejected,
    /// since its timing cannot belong to the same recording.
    pub duration_bounds: [f64; 3],
    /// Longest accepted duration difference when the record is plain text,
    /// whose lines carry no timing to misalign.
    pub plain_duration_limit: f64,
    /// Shortest comparison key that may match by containment rather than
    /// equality, so «Intro» never matches every song containing it.
    pub min_contained_key_chars: usize,
    /// Lowest total score accepted.
    pub min_score: i32,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            duration_bounds: [3.0, 10.0, 45.0],
            plain_duration_limit: 120.0,
            min_contained_key_chars: 4,
            min_score: 3,
        }
    }
}

/// What a match offers to show.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chosen<'a> {
    Synced(&'a Lyrics),
    Plain(&'a Lyrics),
    /// The catalog knows the recording has no vocals.
    Instrumental,
}

/// The best record for `query`, or `None` when no record plausibly is the
/// same song. The title must match (equal comparison keys, or one containing
/// the other); the artist and a close duration add confidence, a different
/// artist removes some, and synced lyrics win over plain ones of an equally
/// good record. Among records with the same score, word timing wins, then
/// catalog order.
pub fn choose<'a>(
    query: &LyricsQuery,
    candidates: &'a [LyricsCandidate],
    config: &MatchConfig,
) -> Option<Chosen<'a>> {
    let title = title_key(query.title());
    let artist = artist_key(query.artist());
    let mut best: Option<((i32, bool), Chosen<'a>)> = None;
    for candidate in candidates {
        let Some(title_score) = key_score(&title, &title_key(&candidate.title), config) else {
            continue;
        };
        let artist_score = if artist.is_empty() {
            0
        } else {
            key_score(&artist, &artist_key(&candidate.artist), config).map_or(-2, |score| score - 1)
        };
        let difference = (query.duration() > 0.0 && candidate.duration > 0.0)
            .then(|| (query.duration() - candidate.duration).abs());
        let Some((chosen, kind_score)) = offer(candidate, difference, config) else {
            continue;
        };
        let duration_score = difference.map_or(0, |difference| {
            config
                .duration_bounds
                .iter()
                .position(|bound| difference <= *bound)
                .map_or(-1, |index| 3 - index as i32)
        });
        let score = title_score + artist_score + duration_score + kind_score;
        let word_timed = matches!(chosen, Chosen::Synced(lyrics) if lyrics.has_word_timing());
        let rank = (score, word_timed);
        if score >= config.min_score && best.as_ref().is_none_or(|(top, _)| rank > *top) {
            best = Some((rank, chosen));
        }
    }
    best.map(|(_, chosen)| chosen)
}

/// 3 for equal keys, 1 when one contains the other and is long enough.
fn key_score(wanted: &str, offered: &str, config: &MatchConfig) -> Option<i32> {
    if wanted.is_empty() || offered.is_empty() {
        return None;
    }
    if wanted == offered {
        return Some(3);
    }
    let shorter = wanted.chars().count().min(offered.chars().count());
    (shorter >= config.min_contained_key_chars
        && (wanted.contains(offered) || offered.contains(wanted)))
    .then_some(1)
}

/// What `candidate` can show given the duration difference, and its bonus.
fn offer<'a>(
    candidate: &'a LyricsCandidate,
    difference: Option<f64>,
    config: &MatchConfig,
) -> Option<(Chosen<'a>, i32)> {
    let within = |limit: f64| difference.is_none_or(|difference| difference <= limit);
    let [.., synced_limit] = config.duration_bounds;
    if candidate.instrumental {
        return within(config.plain_duration_limit).then_some((Chosen::Instrumental, 0));
    }
    if let Some(lyrics) = candidate.synced.as_ref().filter(|_| within(synced_limit)) {
        return Some((Chosen::Synced(lyrics), 2));
    }
    candidate
        .plain
        .as_ref()
        .filter(|_| within(config.plain_duration_limit))
        .map(|lyrics| (Chosen::Plain(lyrics), 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(title: &str, artist: &str, duration: f64, synced: bool) -> LyricsCandidate {
        lyrics_candidate(
            title,
            artist,
            duration,
            synced.then_some("[00:01.00]Synced line"),
        )
    }

    fn lyrics_candidate(
        title: &str,
        artist: &str,
        duration: f64,
        synced: Option<&str>,
    ) -> LyricsCandidate {
        LyricsCandidate::from_record(
            title,
            artist,
            "",
            duration,
            false,
            synced.and_then(Lyrics::parse),
            Lyrics::parse("Plain line"),
        )
        .unwrap()
    }

    fn query(title: &str, artist: &str, duration: f64) -> LyricsQuery {
        LyricsQuery::new(title, artist, "", duration).unwrap()
    }

    fn synced_choice(chosen: Option<Chosen<'_>>) -> Option<bool> {
        chosen.map(|chosen| matches!(chosen, Chosen::Synced(_)))
    }

    #[test]
    fn the_closest_recording_with_synced_lyrics_wins() {
        let candidates = [
            candidate("Bailando", "Enrique Iglesias", 300.0, true),
            candidate(
                "Bailando (Spanish Version)",
                "Enrique Iglesias",
                243.0,
                true,
            ),
            candidate("Bailando", "Enrique Iglesias", 244.0, false),
            candidate("Bailando", "Enrique Iglesias", 243.0, true),
        ];
        let chosen = choose(
            &query("Bailando", "Enrique Iglesias", 243.0),
            &candidates,
            &MatchConfig::default(),
        );
        assert_eq!(synced_choice(chosen), Some(true));
        let Some(Chosen::Synced(lyrics)) = chosen else {
            unreachable!()
        };
        assert!(std::ptr::eq(lyrics, candidates[3].synced.as_ref().unwrap()));
    }

    #[test]
    fn other_songs_and_distant_recordings_are_rejected() {
        let config = MatchConfig::default();
        let other = [candidate("Intro", "Enrique Iglesias", 243.0, true)];
        assert_eq!(
            choose(
                &query("Bailando", "Enrique Iglesias", 243.0),
                &other,
                &config
            ),
            None
        );
        let far = [candidate("Bailando", "Enrique Iglesias", 400.0, true)];
        assert_eq!(
            choose(&query("Bailando", "Enrique Iglesias", 243.0), &far, &config),
            None
        );
        let close_plain = [candidate("Bailando", "Enrique Iglesias", 320.0, true)];
        assert_eq!(
            synced_choice(choose(
                &query("Bailando", "Enrique Iglesias", 243.0),
                &close_plain,
                &config
            )),
            Some(false)
        );
        assert_eq!(choose(&query("Bailando", "", 0.0), &[], &config), None);
    }

    #[test]
    fn channel_names_and_unknown_durations_still_match_the_title() {
        let candidates = [candidate("Tití Me Preguntó", "Bad Bunny", 243.0, true)];
        let config = MatchConfig::default();
        assert_eq!(
            synced_choice(choose(
                &query("Tití Me Preguntó", "Rimas Music", 0.0),
                &candidates,
                &config
            )),
            Some(true)
        );
        assert_eq!(
            synced_choice(choose(
                &query("TITÍ ME PREGUNTÓ", "", 243.5),
                &candidates,
                &config
            )),
            Some(true)
        );
        let instrumental =
            [
                LyricsCandidate::from_record("Interlude", "Someone", "", 60.0, true, None, None)
                    .unwrap(),
            ];
        assert_eq!(
            choose(&query("Interlude", "Someone", 61.0), &instrumental, &config),
            Some(Chosen::Instrumental)
        );
    }

    #[test]
    fn word_timing_decides_only_between_equally_good_records() {
        let word_synced = |duration| {
            lyrics_candidate(
                "Bailando",
                "Enrique Iglesias",
                duration,
                Some("[00:01.00]<00:01.00>Synced <00:01.50>line"),
            )
        };
        let word_timed = |chosen: Option<Chosen<'_>>| match chosen {
            Some(Chosen::Synced(lyrics)) => Some(lyrics.has_word_timing()),
            _ => None,
        };
        let config = MatchConfig::default();
        let wanted = query("Bailando", "Enrique Iglesias", 244.0);
        let equal = [
            candidate("Bailando", "Enrique Iglesias", 244.0, true),
            word_synced(243.0),
        ];
        assert_eq!(word_timed(choose(&wanted, &equal, &config)), Some(true));
        let closer = [
            word_synced(250.0),
            candidate("Bailando", "Enrique Iglesias", 244.0, true),
        ];
        assert_eq!(word_timed(choose(&wanted, &closer, &config)), Some(false));
    }
}
