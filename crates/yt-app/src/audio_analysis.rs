//! Audio analysis controller: keeps the sound measurement of each song as it
//! plays and measures library files in the background, so moods and
//! recommendations know how the music sounds. The signal processing lives in
//! `yt_media::analyze_file` and the playback engine; this module only decides
//! when to measure and what to keep. See `docs/systems/Discovery.md`.

use crate::state::AppState;
use rand::seq::SliceRandom;
use std::time::Duration;
use yt_core::{AudioFeatures, Track};
use yt_media::CancellationToken;

/// Pause after startup before background analysis begins, so launching and
/// restoring playback never compete with it.
const BACKGROUND_DELAY: Duration = Duration::from_secs(30);
/// Unmeasured library files considered per launch; a random sample of them
/// is measured, so files that cannot be measured never block the rest.
const BACKGROUND_CANDIDATES: usize = 2_000;
/// Library files measured per launch.
const BACKGROUND_BATCH: usize = 150;
/// Audio measured per library file.
const SAMPLE_LENGTH: Duration = Duration::from_secs(30);
/// Where the measurement starts, as a share of the file's duration: past the
/// intro, where most songs settle into their character.
const SAMPLE_START_SHARE: f64 = 0.3;
/// Pause between two files, so the analysis stays a light background task.
const BACKGROUND_PAUSE: Duration = Duration::from_millis(750);

/// Where to start measuring a file of `duration` seconds (0 when unknown):
/// [`SAMPLE_START_SHARE`] into it, early enough to leave a full sample.
fn sample_start(duration: f64) -> Duration {
    if !duration.is_finite() || duration <= 0.0 {
        return Duration::ZERO;
    }
    let latest = (duration - SAMPLE_LENGTH.as_secs_f64()).max(0.0);
    Duration::from_secs_f64((duration * SAMPLE_START_SHARE).min(latest))
}

/// Whether a library file can ever yield a measurement worth keeping.
fn measurable(track: &Track) -> bool {
    track.duration <= 0.0 || track.duration >= AudioFeatures::MIN_ANALYZED_SECONDS
}

impl AppState {
    /// Keep the measurement taken while `media_id` played, when it covers
    /// enough audio. A shorter one than already stored is ignored by
    /// `yt-core`. Bookkeeping: failures are logged, never shown.
    pub(crate) fn keep_sound(&self, media_id: &str, features: &AudioFeatures) {
        if features.analyzed_seconds < AudioFeatures::MIN_ANALYZED_SECONDS {
            return;
        }
        if let Err(error) = self.services.core.store_audio_features(media_id, features) {
            crate::native::record_log(
                &self.services.profile,
                &format!("Sound measurement was not stored: {error}"),
            );
        }
    }

    /// Measure library songs that have no measurement yet, one at a time off
    /// the UI thread, after a delay and with pauses in between.
    pub(crate) fn start_library_analysis(&self) {
        let state = self.clone();
        dioxus::core::spawn_forever(async move {
            tokio::time::sleep(BACKGROUND_DELAY).await;
            let core = state.services.core.clone();
            let mut tracks = tokio::task::spawn_blocking(move || {
                core.unanalyzed_library_tracks(BACKGROUND_CANDIDATES)
            })
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            tracks.retain(measurable);
            tracks.shuffle(&mut rand::rng());
            tracks.truncate(BACKGROUND_BATCH);
            let cancel = CancellationToken::new();
            for track in tracks {
                let path = track.path.clone();
                let start = sample_start(track.duration);
                let token = cancel.clone();
                let measured = tokio::task::spawn_blocking(move || {
                    yt_media::analyze_file(&path, start, SAMPLE_LENGTH, &token)
                })
                .await;
                if let Ok(Ok(features)) = measured {
                    state.keep_sound(&track.id, &features);
                }
                tokio::time::sleep(BACKGROUND_PAUSE).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_start_past_the_intro_and_still_fit_in_the_file() {
        assert_eq!(sample_start(0.0), Duration::ZERO);
        assert_eq!(sample_start(f64::NAN), Duration::ZERO);
        assert_eq!(sample_start(200.0), Duration::from_secs(60));
        assert_eq!(sample_start(40.0), Duration::from_secs(10));
        assert_eq!(sample_start(25.0), Duration::ZERO);
    }

    #[tokio::test]
    async fn measurements_of_played_songs_are_kept_only_when_long_enough() {
        let mut app = crate::test_support::TestApp::new();
        let track = app.video_track("measured");
        let core = app.services.core.clone();
        let features = |seconds: f64| AudioFeatures {
            analyzed_seconds: seconds,
            energy: 0.7,
            dynamics: 0.3,
            brightness: 0.6,
            onset_rate: 0.5,
            tempo_bpm: 124.0,
            mode: 0.4,
        };
        app.run(|state| {
            state.keep_sound(&track.id, &features(5.0));
            assert!(core.audio_features().unwrap().is_empty());
            state.keep_sound(&track.id, &features(45.0));
        });
        assert_eq!(core.audio_features().unwrap()[&track.id].tempo_bpm, 124.0);
    }
}
