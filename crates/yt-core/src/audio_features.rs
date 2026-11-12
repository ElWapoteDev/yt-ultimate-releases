//! How songs sound, measured locally from their decoded audio while they play
//! or from library files. The measurements feed moods and recommendations.

use crate::{AppCore, CoreError, Result, Track, media::media_exists, now};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, ops::RangeInclusive};

/// Measured character of one song. Every field has a fixed scale so values
/// from different sources and versions compare directly.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioFeatures {
    /// Seconds of audio the measurement covers.
    pub analyzed_seconds: f64,
    /// Mean loudness: the mean per-frame RMS level in dBFS mapped linearly
    /// from [`Self::ENERGY_RANGE_DBFS`] to [0, 1].
    pub energy: f64,
    /// Loudness variation: the standard deviation of per-frame dBFS divided
    /// by [`Self::FULL_DYNAMICS_DB`], clamped to [0, 1].
    pub dynamics: f64,
    /// Timbre brightness: the mean spectral centroid mapped logarithmically
    /// from [`Self::BRIGHTNESS_RANGE_HZ`] to [0, 1].
    pub brightness: f64,
    /// Rhythmic density: detected onsets per second divided by
    /// [`Self::FULL_ONSET_RATE`], clamped to [0, 1].
    pub onset_rate: f64,
    /// Estimated tempo in beats per minute within `TEMPO_RANGE`, or 0 when no
    /// stable beat was found.
    pub tempo_bpm: f64,
    /// Tonality: correlation with the best major key profile minus the best
    /// minor one (Krumhansl–Kessler), clamped to [-1, 1]; positive leans major.
    pub mode: f64,
}

impl AudioFeatures {
    /// Frame levels mapped linearly onto energy 0 and 1, in dBFS (0 dBFS is
    /// the RMS of a full-scale square wave; a full-scale sine reads -3 dBFS).
    pub const ENERGY_RANGE_DBFS: RangeInclusive<f64> = -60.0..=0.0;
    /// Standard deviation of the frame level that maps to dynamics 1, in dB.
    pub const FULL_DYNAMICS_DB: f64 = 20.0;
    /// Spectral centroids mapped logarithmically onto brightness 0 and 1, in Hz.
    pub const BRIGHTNESS_RANGE_HZ: RangeInclusive<f64> = 100.0..=8_000.0;
    /// Onsets per second that map to onset rate 1.
    pub const FULL_ONSET_RATE: f64 = 8.0;
    /// Shortest measurement worth storing, in seconds.
    pub const MIN_ANALYZED_SECONDS: f64 = 20.0;
    /// Tempos the analysis reports; anything else is 0 (unknown).
    pub const TEMPO_RANGE: RangeInclusive<f64> = 40.0..=220.0;
    /// Longest measurement accepted, in seconds (one week).
    const MAX_ANALYZED_SECONDS: f64 = 7.0 * 86_400.0;

    /// Check every scale documented on the fields.
    pub fn validate(&self) -> Result<()> {
        let unit = |value: f64| value.is_finite() && (0.0..=1.0).contains(&value);
        let valid = self.analyzed_seconds.is_finite()
            && (Self::MIN_ANALYZED_SECONDS..=Self::MAX_ANALYZED_SECONDS)
                .contains(&self.analyzed_seconds)
            && unit(self.energy)
            && unit(self.dynamics)
            && unit(self.brightness)
            && unit(self.onset_rate)
            && (self.tempo_bpm == 0.0 || Self::TEMPO_RANGE.contains(&self.tempo_bpm))
            && self.mode.is_finite()
            && (-1.0..=1.0).contains(&self.mode);
        if valid {
            Ok(())
        } else {
            Err(CoreError::Invalid("Invalid audio features".into()))
        }
    }
}

fn row_to_features(row: &rusqlite::Row<'_>) -> rusqlite::Result<AudioFeatures> {
    Ok(AudioFeatures {
        analyzed_seconds: row.get("analyzed_seconds")?,
        energy: row.get("energy")?,
        dynamics: row.get("dynamics")?,
        brightness: row.get("brightness")?,
        onset_rate: row.get("onset_rate")?,
        tempo_bpm: row.get("tempo_bpm")?,
        mode: row.get("mode")?,
    })
}

impl AppCore {
    /// Store the measurement of a known media item. A measurement covering
    /// fewer seconds than the stored one is ignored, so a short sample never
    /// replaces a fuller analysis.
    pub fn store_audio_features(&self, media_id: &str, features: &AudioFeatures) -> Result<()> {
        features.validate()?;
        let mut db = self.connection()?;
        let transaction = db.transaction()?;
        if !media_exists(&transaction, media_id)? {
            return Err(CoreError::NotFound(media_id.into()));
        }
        let stored: Option<f64> = transaction
            .query_row(
                "SELECT analyzed_seconds FROM rust_audio_features WHERE media_id=?1",
                [media_id],
                |row| row.get(0),
            )
            .optional()?;
        if stored.is_some_and(|seconds| seconds > features.analyzed_seconds) {
            return Ok(());
        }
        transaction.execute(
            "INSERT INTO rust_audio_features(media_id,analyzed_seconds,energy,dynamics,brightness,onset_rate,tempo_bpm,mode,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(media_id) DO UPDATE SET analyzed_seconds=excluded.analyzed_seconds,energy=excluded.energy,
               dynamics=excluded.dynamics,brightness=excluded.brightness,onset_rate=excluded.onset_rate,
               tempo_bpm=excluded.tempo_bpm,mode=excluded.mode,updated_at=excluded.updated_at",
            params![
                media_id,
                features.analyzed_seconds,
                features.energy,
                features.dynamics,
                features.brightness,
                features.onset_rate,
                features.tempo_bpm,
                features.mode,
                now()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every stored measurement by media id; corrupt rows are skipped.
    pub fn audio_features(&self) -> Result<HashMap<String, AudioFeatures>> {
        let db = self.connection()?;
        let mut query = db.prepare("SELECT * FROM rust_audio_features")?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>("media_id")?, row_to_features(row)?))
        })?;
        let mut features = HashMap::new();
        for row in rows {
            let (id, value) = row?;
            if value.validate().is_ok() {
                features.insert(id, value);
            }
        }
        Ok(features)
    }

    /// Library audio tracks that still need an analysis, newest first.
    pub fn unanalyzed_library_tracks(&self, limit: usize) -> Result<Vec<Track>> {
        let ids: Vec<String> = {
            let db = self.connection()?;
            let mut query = db.prepare(
                "SELECT t.id FROM tracks t LEFT JOIN rust_audio_features a ON a.media_id=t.id
                 WHERE a.media_id IS NULL AND t.media_type='audio' AND t.missing=0
                 ORDER BY t.created_at DESC LIMIT ?1",
            )?;
            query
                .query_map([limit as i64], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        Ok(ids
            .iter()
            .filter_map(|id| self.track(id).ok())
            .filter(|track| !track.missing)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteMedia;

    pub(crate) fn sample(seconds: f64) -> AudioFeatures {
        AudioFeatures {
            analyzed_seconds: seconds,
            energy: 0.6,
            dynamics: 0.2,
            brightness: 0.5,
            onset_rate: 0.4,
            tempo_bpm: 120.0,
            mode: 0.3,
        }
    }

    #[test]
    fn fuller_measurements_win_and_unknown_media_is_rejected() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let track = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
        assert!(matches!(
            core.store_audio_features("missing", &sample(30.0)),
            Err(CoreError::NotFound(_))
        ));
        core.store_audio_features(&track.id, &sample(60.0)).unwrap();
        let mut shorter = sample(30.0);
        shorter.energy = 0.1;
        core.store_audio_features(&track.id, &shorter).unwrap();
        assert_eq!(core.audio_features().unwrap()[&track.id].energy, 0.6);
        let online = RemoteMedia::from_parts("aqz-KE-bpKQ", "Online", "", "", 200.0).unwrap();
        core.remember_remote(&online).unwrap();
        core.store_audio_features(&online.id, &sample(25.0))
            .unwrap();
        assert_eq!(core.audio_features().unwrap().len(), 2);
    }

    #[test]
    fn invalid_scales_are_rejected() {
        let valid = sample(30.0);
        for invalid in [
            AudioFeatures {
                analyzed_seconds: 5.0,
                ..valid
            },
            AudioFeatures {
                energy: 1.5,
                ..valid
            },
            AudioFeatures {
                brightness: f64::NAN,
                ..valid
            },
            AudioFeatures {
                tempo_bpm: 20.0,
                ..valid
            },
            AudioFeatures {
                mode: -2.0,
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err(), "accepted {invalid:?}");
        }
        assert!(
            AudioFeatures {
                tempo_bpm: 0.0,
                ..valid
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn unanalyzed_tracks_exclude_measured_and_video_media() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let first = core.import_bytes("first.wav", &[1, 2, 3, 4]).unwrap();
        let second = core.import_bytes("second.wav", &[5, 6, 7, 8]).unwrap();
        core.import_bytes("clip.mp4", &[9, 9, 9]).unwrap();
        core.store_audio_features(&first.id, &sample(30.0)).unwrap();
        let pending = core.unanalyzed_library_tracks(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, second.id);
    }
}
