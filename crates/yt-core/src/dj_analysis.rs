//! Cache of the DJ's [`TrackAnalysis`] values, one per media item and audio
//! content, so a track is analyzed once. Analyses of online media reveal what
//! was played and are erased with the listening history; those of library
//! files describe the files and stay, like their audio features.

use crate::{AppCore, CoreError, Result, TrackAnalysis, now};
use rusqlite::{OptionalExtension, params};

/// Analyses kept; storing a new one removes the least recently used beyond it.
pub const DJ_ANALYSIS_LIMIT: usize = 1_000;
/// Longest media id or content key accepted, in bytes.
pub const MAX_ANALYSIS_KEY_BYTES: usize = 256;
/// Largest serialized analysis accepted, in bytes (20,000 beats fit easily).
const MAX_ANALYSIS_BYTES: usize = 1 << 20;

impl AppCore {
    /// Store `analysis` for `media_id` (a library track id or a `youtube:` id)
    /// and the audio identified by `content_key` (whatever tells two encodings
    /// of the same media apart, such as a format and its size). Validates the
    /// analysis and both keys, replaces analyses of other versions for the
    /// same audio, and prunes the table to [`DJ_ANALYSIS_LIMIT`], all in one
    /// transaction.
    pub fn store_track_analysis(
        &self,
        media_id: &str,
        content_key: &str,
        analysis: &TrackAnalysis,
    ) -> Result<()> {
        validate_key("media id", media_id)?;
        validate_key("content key", content_key)?;
        analysis.validate()?;
        let json = serde_json::to_string(analysis)?;
        if json.len() > MAX_ANALYSIS_BYTES {
            return Err(CoreError::Invalid("track analysis is too large".into()));
        }
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        transaction.execute(
            "DELETE FROM rust_dj_analysis WHERE media_id=?1 AND content_key=?2",
            params![media_id, content_key],
        )?;
        transaction.execute(
            "INSERT INTO rust_dj_analysis(media_id,content_key,analyzer_version,analysis,used_at) VALUES(?1,?2,?3,?4,?5)",
            params![media_id, content_key, analysis.analyzer_version, json, now()],
        )?;
        transaction.execute(
            "DELETE FROM rust_dj_analysis WHERE rowid NOT IN (SELECT rowid FROM rust_dj_analysis ORDER BY used_at DESC LIMIT ?1)",
            [DJ_ANALYSIS_LIMIT as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// The stored analysis of `media_id` and `content_key` made by
    /// `analyzer_version`, marked as just used. A row that no longer parses or
    /// validates is deleted and reads as `None`.
    pub fn track_analysis(
        &self,
        media_id: &str,
        content_key: &str,
        analyzer_version: u16,
    ) -> Result<Option<TrackAnalysis>> {
        let db = self.connection()?;
        let stored: Option<String> = db
            .query_row(
                "SELECT analysis FROM rust_dj_analysis WHERE media_id=?1 AND content_key=?2 AND analyzer_version=?3",
                params![media_id, content_key, analyzer_version],
                |row| row.get(0),
            )
            .optional()?;
        let Some(json) = stored else {
            return Ok(None);
        };
        let parsed = serde_json::from_str::<TrackAnalysis>(&json)
            .ok()
            .filter(|analysis| {
                analysis.analyzer_version == analyzer_version && analysis.validate().is_ok()
            });
        if parsed.is_some() {
            db.execute(
                "UPDATE rust_dj_analysis SET used_at=?4 WHERE media_id=?1 AND content_key=?2 AND analyzer_version=?3",
                params![media_id, content_key, analyzer_version, now()],
            )?;
        } else {
            db.execute(
                "DELETE FROM rust_dj_analysis WHERE media_id=?1 AND content_key=?2 AND analyzer_version=?3",
                params![media_id, content_key, analyzer_version],
            )?;
        }
        Ok(parsed)
    }
}

fn validate_key(what: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ANALYSIS_KEY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(CoreError::Invalid(format!(
            "{what} must be 1–{MAX_ANALYSIS_KEY_BYTES} bytes without control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BandEnergy, KeyEstimate, KeyMode, Meter, PitchClass, Subdivision};

    fn core() -> (tempfile::TempDir, AppCore) {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        (profile, core)
    }

    fn analysis(version: u16) -> TrackAnalysis {
        TrackAnalysis {
            analyzer_version: version,
            sample_rate: 44_100,
            duration_frames: 44_100 * 8,
            beats: (0..16).map(|beat| beat * 22_050).collect(),
            downbeats: vec![0, 4, 8, 12],
            meter: Some(Meter {
                beats_per_bar: 4,
                subdivision: Subdivision::Binary,
                confidence: 1.0,
            }),
            tempo_bpm: Some(120.0),
            tempo_stability: 1.0,
            key: Some(KeyEstimate {
                tonic: PitchClass::new(2).unwrap(),
                mode: KeyMode::Major,
                confidence: 0.7,
            }),
            integrated_lufs: Some(-10.0),
            true_peak_dbtp: -1.0,
            short_term_lufs: vec![-11.0; 16],
            bar_energy: vec![
                BandEnergy {
                    low_db: -20.0,
                    mid_db: -18.0,
                    high_db: -26.0,
                };
                3
            ],
            sections: Vec::new(),
            vocal_db: Vec::new(),
            momentary_lufs: Vec::new(),
        }
    }

    #[test]
    fn analyses_round_trip_per_version_and_content() {
        let (_profile, core) = core();
        core.store_track_analysis("youtube:abcdefghijk", "itag-140:3456789", &analysis(1))
            .unwrap();
        let found = core
            .track_analysis("youtube:abcdefghijk", "itag-140:3456789", 1)
            .unwrap();
        assert_eq!(found, Some(analysis(1)));
        assert_eq!(
            core.track_analysis("youtube:abcdefghijk", "itag-251:1", 1)
                .unwrap(),
            None
        );
        assert_eq!(
            core.track_analysis("youtube:abcdefghijk", "itag-140:3456789", 2)
                .unwrap(),
            None
        );
        core.store_track_analysis("youtube:abcdefghijk", "itag-140:3456789", &analysis(2))
            .unwrap();
        assert_eq!(
            core.track_analysis("youtube:abcdefghijk", "itag-140:3456789", 1)
                .unwrap(),
            None,
            "a newer version replaces the older one"
        );
        assert!(
            core.track_analysis("youtube:abcdefghijk", "itag-140:3456789", 2)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn invalid_input_is_refused_and_corrupt_rows_are_dropped() {
        let (_profile, core) = core();
        assert!(core.store_track_analysis("", "key", &analysis(1)).is_err());
        assert!(
            core.store_track_analysis("id", "bad\nkey", &analysis(1))
                .is_err()
        );
        assert!(
            core.store_track_analysis(&"x".repeat(MAX_ANALYSIS_KEY_BYTES + 1), "key", &analysis(1))
                .is_err()
        );
        let mut broken = analysis(1);
        broken.tempo_bpm = Some(1_000.0);
        assert!(core.store_track_analysis("id", "key", &broken).is_err());
        core.connection()
            .unwrap()
            .execute(
                "INSERT INTO rust_dj_analysis VALUES('id','key',1,'{\"not\":\"an analysis\"}',1.0)",
                [],
            )
            .unwrap();
        assert_eq!(core.track_analysis("id", "key", 1).unwrap(), None);
        let left: i64 = core
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM rust_dj_analysis", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(left, 0, "the corrupt row is deleted");
    }

    #[test]
    fn the_least_recently_used_analyses_are_pruned() {
        let (_profile, core) = core();
        for index in 0..=DJ_ANALYSIS_LIMIT {
            core.store_track_analysis(&format!("track-{index}"), "key", &analysis(1))
                .unwrap();
        }
        let count: i64 = core
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM rust_dj_analysis", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, DJ_ANALYSIS_LIMIT as i64);
        assert!(
            core.track_analysis(&format!("track-{DJ_ANALYSIS_LIMIT}"), "key", 1)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn erasing_the_listening_history_keeps_only_library_analyses() {
        let (_profile, core) = core();
        core.store_track_analysis("youtube:abcdefghijk", "key", &analysis(1))
            .unwrap();
        core.store_track_analysis("library-track", "key", &analysis(1))
            .unwrap();
        let erased = core.clear_listening_history().unwrap();
        assert_eq!(erased.dj_analyses, 1);
        assert!(
            core.track_analysis("youtube:abcdefghijk", "key", 1)
                .unwrap()
                .is_none()
        );
        assert!(
            core.track_analysis("library-track", "key", 1)
                .unwrap()
                .is_some()
        );
    }
}
