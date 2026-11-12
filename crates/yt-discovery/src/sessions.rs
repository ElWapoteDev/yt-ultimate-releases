//! Listening sessions: runs of listens without a long pause.

use crate::{listen::heard_seconds, ListenPolicy};
use yt_core::ListenSignal;

/// A pause longer than this many seconds starts a new session: 30 minutes,
/// the inactivity timeout commonly used to split listening logs.
pub const SESSION_GAP_SECONDS: f64 = 30.0 * 60.0;

/// Listens heard one after another with no pause longer than the gap.
#[derive(Debug, Clone, PartialEq)]
pub struct ListeningSession {
    /// Media in listening order; a song played twice appears twice.
    pub media_ids: Vec<String>,
    /// Unix seconds when the first listen started.
    pub started_at: f64,
    /// Unix seconds when the last listen ended (its start plus the seconds
    /// heard).
    pub ended_at: f64,
}

/// Split `signals` (any order) into chronological sessions.
///
/// Skips, as judged by `policy`, are left out: the user rejected them, so
/// they say nothing about what goes together. A listen joins the current
/// session when it starts at most `gap_seconds` after the session's latest
/// end; a negative or NaN gap keeps only overlapping listens together.
/// Listens with a non-finite start are ignored. Output is independent of the
/// input order.
pub fn sessions(
    signals: &[ListenSignal],
    gap_seconds: f64,
    policy: &ListenPolicy,
) -> Vec<ListeningSession> {
    let gap = gap_seconds.max(0.0);
    let mut listens: Vec<&ListenSignal> = signals
        .iter()
        .filter(|signal| signal.started_at.is_finite() && policy.is_play(signal))
        .collect();
    listens.sort_by(|left, right| {
        left.started_at
            .total_cmp(&right.started_at)
            .then_with(|| left.media_id.cmp(&right.media_id))
    });
    let mut sessions: Vec<ListeningSession> = Vec::new();
    for listen in listens {
        let ended_at = listen.started_at + heard_seconds(listen);
        match sessions.last_mut() {
            Some(session) if listen.started_at - session.ended_at <= gap => {
                session.media_ids.push(listen.media_id.clone());
                session.ended_at = session.ended_at.max(ended_at);
            }
            _ => sessions.push(ListeningSession {
                media_ids: vec![listen.media_id.clone()],
                started_at: listen.started_at,
                ended_at,
            }),
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{completed, skipped, NOW};

    #[test]
    fn pauses_split_sessions_and_skips_are_left_out() {
        let mut long = completed("long", "A", NOW + 5_000.0);
        long.listened_seconds = 3_000.0;
        let signals = vec![
            completed("after-long", "A", NOW + 9_000.0),
            completed("b", "A", NOW + 300.0),
            skipped("skip", "A", NOW + 600.0),
            completed("a", "A", NOW),
            long,
        ];
        let split = sessions(&signals, SESSION_GAP_SECONDS, &ListenPolicy::default());
        let ids: Vec<Vec<&str>> = split
            .iter()
            .map(|session| session.media_ids.iter().map(String::as_str).collect())
            .collect();
        // «b» ends at +500 s and «long» starts 75 minutes later; «long» plays
        // 3 000 s, so «after-long» starts within the gap.
        assert_eq!(ids, [vec!["a", "b"], vec!["long", "after-long"]]);
        assert_eq!(split[0].started_at, NOW);
        assert_eq!(split[0].ended_at, NOW + 500.0);
        assert_eq!(split[1].ended_at, NOW + 9_200.0);
        let mut reversed = signals.clone();
        reversed.reverse();
        assert_eq!(
            sessions(&reversed, SESSION_GAP_SECONDS, &ListenPolicy::default()),
            split
        );
        assert_eq!(
            sessions(&signals, f64::NAN, &ListenPolicy::default()).len(),
            4
        );
        assert!(sessions(&[], SESSION_GAP_SECONDS, &ListenPolicy::default()).is_empty());
    }
}
