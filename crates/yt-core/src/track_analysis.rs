//! Musical analysis of one track for the DJ: where its beats and bars fall,
//! its meter, tempo, key and loudness. `yt-mir` measures it; the DJ caches it
//! and plans transitions from it. Every bound lives on [`TrackAnalysis`], and
//! [`TrackAnalysis::validate`] checks a value produced elsewhere (the analysis
//! worker, a cache row) before anything uses it.

use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

/// Pitch class of a key's tonic, C = 0 through B = 11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct PitchClass(u8);

impl PitchClass {
    /// Pitch classes per octave.
    pub const COUNT: u8 = 12;

    /// A pitch class; fails for values of [`Self::COUNT`] or more.
    pub fn new(value: u8) -> Result<Self> {
        if value < Self::COUNT {
            Ok(Self(value))
        } else {
            Err(CoreError::Invalid(format!(
                "pitch class {value} is not below 12"
            )))
        }
    }

    /// The pitch class as 0 (C) through 11 (B).
    pub fn value(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for PitchClass {
    type Error = CoreError;

    fn try_from(value: u8) -> Result<Self> {
        Self::new(value)
    }
}

impl From<PitchClass> for u8 {
    fn from(value: PitchClass) -> Self {
        value.0
    }
}

/// Major or minor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    Major,
    Minor,
}

/// An estimated key and how well the music fits it: the Pearson correlation
/// of its pitch-class profile with the key's Krumhansl–Kessler profile,
/// clamped to [0, 1].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KeyEstimate {
    pub tonic: PitchClass,
    pub mode: KeyMode,
    pub confidence: f32,
}

/// How each beat divides: in two (most pop, reggaeton, cumbia) or in three
/// (a 6/8 or 12/8 feel, common in corridos tumbados).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subdivision {
    Binary,
    Ternary,
}

/// Beats per bar, how each beat divides, and the share of bars with that many
/// beats, in [0, 1].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    pub beats_per_bar: u8,
    pub subdivision: Subdivision,
    pub confidence: f32,
}

/// Mean level of three frequency bands over one bar, in dBFS: below
/// [`TrackAnalysis::LOW_BAND_HZ`], up to [`TrackAnalysis::HIGH_BAND_HZ`], and above.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BandEnergy {
    pub low_db: f32,
    pub mid_db: f32,
    pub high_db: f32,
}

/// A stretch of bars with one character; the song's structure changes where
/// the next one starts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// Index of its first bar (a downbeat).
    pub start_bar: u32,
    /// Louder than the song's average: a chorus or a drop. Intros,
    /// breakdowns and outros are not.
    pub high_energy: bool,
    /// Mean level of its bars, in dB.
    pub level_db: f32,
}

/// Musical analysis of one track. Positions are frames at `sample_rate`, the
/// rate of the audio that was analyzed, so the playback engine can place a
/// transition on an exact sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackAnalysis {
    /// Version of the analysis that produced the value; a cache keeps only the
    /// current one.
    pub analyzer_version: u16,
    pub sample_rate: u32,
    pub duration_frames: u64,
    /// Beat positions, strictly increasing and before `duration_frames`.
    pub beats: Vec<u64>,
    /// Indices into `beats` where a bar starts, strictly increasing.
    pub downbeats: Vec<u32>,
    /// `None` when no bar was found.
    pub meter: Option<Meter>,
    /// `None` when fewer than three beats were found.
    pub tempo_bpm: Option<f32>,
    /// Share of beat intervals within [`Self::STABLE_INTERVAL`] of the median
    /// interval, in [0, 1]; low for rubato, live tempo changes or no beat.
    pub tempo_stability: f32,
    /// `None` when the music has no pitch-class profile (silence, drums only).
    pub key: Option<KeyEstimate>,
    /// EBU R128 integrated loudness, in LUFS; `None` for silence.
    pub integrated_lufs: Option<f32>,
    /// Highest true peak of any channel, in dBTP.
    pub true_peak_dbtp: f32,
    /// EBU R128 short-term loudness every [`Self::LOUDNESS_STEP_SECONDS`], in
    /// LUFS; silence reads [`Self::SILENCE_LUFS`].
    pub short_term_lufs: Vec<f32>,
    /// Band levels of each bar, the span from one downbeat to the next.
    pub bar_energy: Vec<BandEnergy>,
    /// The song's sections in order, the first at bar 0; empty when it has
    /// too few bars to tell.
    #[serde(default)]
    pub sections: Vec<Section>,
    /// RMS level of the song's separated vocals every
    /// [`Self::LOUDNESS_STEP_SECONDS`], in dBFS; empty until they are measured
    /// (a second, slower job than the rest of the analysis).
    #[serde(default)]
    pub vocal_db: Vec<f32>,
    /// EBU R128 momentary loudness (a 400 ms window) every
    /// [`Self::LOUDNESS_STEP_SECONDS`], in LUFS; silence reads
    /// [`Self::SILENCE_LUFS`]. Where the track's sound starts and its music
    /// ends ([`Self::audible_start`], [`Self::audible_end`]); empty in older
    /// analyses.
    #[serde(default)]
    pub momentary_lufs: Vec<f32>,
}

impl TrackAnalysis {
    /// Accepted sample rates, in Hz.
    pub const SAMPLE_RATE: RangeInclusive<u32> = 8_000..=192_000;
    /// Longest track analyzed, in seconds.
    pub const MAX_DURATION_SECONDS: u32 = 7_200;
    /// Most beats stored: 250 BPM for [`Self::MAX_DURATION_SECONDS`] would be
    /// 30,000, so anything above this is noise, not a beat.
    pub const MAX_BEATS: usize = 20_000;
    /// Tempos reported, in BPM.
    pub const TEMPO_BPM: RangeInclusive<f32> = 40.0..=250.0;
    /// Beats per bar reported.
    pub const BEATS_PER_BAR: RangeInclusive<u8> = 2..=12;
    /// Relative distance from the median beat interval that still counts as
    /// the same tempo.
    pub const STABLE_INTERVAL: f32 = 0.04;
    /// Spacing of the short-term loudness curve, in seconds.
    pub const LOUDNESS_STEP_SECONDS: f32 = 0.5;
    /// Loudness reported for silence, in LUFS (the EBU R128 absolute gate).
    pub const SILENCE_LUFS: f32 = -70.0;
    /// Loudness values accepted, in LUFS.
    pub const LUFS: RangeInclusive<f32> = -70.0..=10.0;
    /// True peaks and band levels accepted, in dB; digital silence is clamped
    /// to the low end.
    pub const LEVEL_DB: RangeInclusive<f32> = -200.0..=30.0;
    /// Upper edge of the low band, in Hz (the bass a DJ swaps).
    pub const LOW_BAND_HZ: f32 = 150.0;
    /// Upper edge of the mid band, in Hz.
    pub const HIGH_BAND_HZ: f32 = 4_000.0;
    /// Most sections stored.
    pub const MAX_SECTIONS: usize = 64;
    /// A track's sound starts where its momentary loudness first comes within
    /// this many LU of its integrated loudness: anything quieter before it is
    /// silence or noise, while a soft intro still counts.
    pub const LEADING_SILENCE_LU: f32 = 30.0;
    /// A track's music ends where its momentary loudness last comes within
    /// this many LU of its integrated loudness: its trailing silence and most
    /// of a fade-out lie past it.
    pub const TAIL_DROP_LU: f32 = 10.0;
    /// How long the 3 s short-term loudness stays within [`Self::TAIL_DROP_LU`]
    /// after the music stops, in seconds: what older analyses, without the
    /// momentary curve, take off their end.
    pub const SHORT_TERM_LAG_SECONDS: f32 = 2.7;

    /// Seconds from the start of the track to `frame`.
    pub fn seconds(&self, frame: u64) -> f64 {
        frame as f64 / f64::from(self.sample_rate)
    }

    /// Frame where the track's sound starts, past any leading silence
    /// ([`Self::LEADING_SILENCE_LU`]); 0 without loudness.
    pub fn audible_start(&self) -> u64 {
        let (curve, _) = self.loudness_curve();
        let Some(floor) = self
            .integrated_lufs
            .map(|lufs| lufs - Self::LEADING_SILENCE_LU)
        else {
            return 0;
        };
        curve
            .iter()
            .position(|lufs| *lufs >= floor)
            .map_or(0, |step| self.step_frame(step).min(self.duration_frames))
    }

    /// Frame where the track's music ends, before any trailing silence and
    /// most of a fade-out ([`Self::TAIL_DROP_LU`]); its end without loudness.
    pub fn audible_end(&self) -> u64 {
        let (curve, lag) = self.loudness_curve();
        let Some(floor) = self.integrated_lufs.map(|lufs| lufs - Self::TAIL_DROP_LU) else {
            return self.duration_frames;
        };
        let lag = (f64::from(lag) * f64::from(self.sample_rate)) as u64;
        curve
            .iter()
            .rposition(|lufs| *lufs >= floor)
            .map_or(self.duration_frames, |step| {
                self.step_frame(step + 1).saturating_sub(lag).clamp(
                    self.audible_start().min(self.duration_frames),
                    self.duration_frames,
                )
            })
    }

    /// The loudness curve that tells where the track is heard, and how many
    /// seconds it lags behind the audio: the momentary one, or the short-term
    /// one of analyses made before it.
    fn loudness_curve(&self) -> (&[f32], f32) {
        if self.momentary_lufs.is_empty() {
            (&self.short_term_lufs, Self::SHORT_TERM_LAG_SECONDS)
        } else {
            (&self.momentary_lufs, 0.0)
        }
    }

    /// Frame where loudness step `step` starts.
    fn step_frame(&self, step: usize) -> u64 {
        (step as f64 * f64::from(Self::LOUDNESS_STEP_SECONDS) * f64::from(self.sample_rate)) as u64
    }

    /// Check every bound documented on the fields.
    pub fn validate(&self) -> Result<()> {
        let fail = |what: &str| Err(CoreError::Invalid(format!("track analysis: {what}")));
        let unit = |value: f32| value.is_finite() && (0.0..=1.0).contains(&value);
        if !Self::SAMPLE_RATE.contains(&self.sample_rate) {
            return fail("sample rate outside its range");
        }
        let max_frames = u64::from(Self::MAX_DURATION_SECONDS) * u64::from(self.sample_rate);
        if self.duration_frames == 0 || self.duration_frames > max_frames {
            return fail("duration outside its range");
        }
        if self.beats.len() > Self::MAX_BEATS
            || !self.beats.windows(2).all(|pair| pair[0] < pair[1])
            || self
                .beats
                .last()
                .is_some_and(|last| *last >= self.duration_frames)
        {
            return fail("beats are not increasing positions inside the track");
        }
        if !self.downbeats.windows(2).all(|pair| pair[0] < pair[1])
            || self
                .downbeats
                .last()
                .is_some_and(|last| *last as usize >= self.beats.len())
        {
            return fail("downbeats are not increasing beat indices");
        }
        if self.meter.is_some_and(|meter| {
            !Self::BEATS_PER_BAR.contains(&meter.beats_per_bar) || !unit(meter.confidence)
        }) {
            return fail("meter outside its range");
        }
        if self
            .tempo_bpm
            .is_some_and(|tempo| !tempo.is_finite() || !Self::TEMPO_BPM.contains(&tempo))
        {
            return fail("tempo outside its range");
        }
        if !unit(self.tempo_stability) {
            return fail("tempo stability outside [0, 1]");
        }
        if self.key.is_some_and(|key| !unit(key.confidence)) {
            return fail("key confidence outside [0, 1]");
        }
        let lufs = |value: f32| value.is_finite() && Self::LUFS.contains(&value);
        let level = |value: f32| value.is_finite() && Self::LEVEL_DB.contains(&value);
        if self.integrated_lufs.is_some_and(|value| !lufs(value)) || !level(self.true_peak_dbtp) {
            return fail("loudness outside its range");
        }
        let duration_seconds = self.duration_frames as f64 / f64::from(self.sample_rate);
        let max_steps =
            (duration_seconds / f64::from(Self::LOUDNESS_STEP_SECONDS)).ceil() as usize + 1;
        if self.short_term_lufs.len() > max_steps
            || !self.short_term_lufs.iter().copied().all(lufs)
            || self.momentary_lufs.len() > max_steps
            || !self.momentary_lufs.iter().copied().all(lufs)
        {
            return fail("short-term loudness curve outside its range");
        }
        if self.bar_energy.len() > self.downbeats.len()
            || !self
                .bar_energy
                .iter()
                .all(|bar| level(bar.low_db) && level(bar.mid_db) && level(bar.high_db))
        {
            return fail("bar energy outside its range");
        }
        let sections_valid = self.sections.len() <= Self::MAX_SECTIONS
            && self
                .sections
                .first()
                .is_none_or(|first| first.start_bar == 0)
            && self
                .sections
                .windows(2)
                .all(|pair| pair[0].start_bar < pair[1].start_bar)
            && self
                .sections
                .last()
                .is_none_or(|last| (last.start_bar as usize) < self.downbeats.len())
            && self.sections.iter().all(|section| level(section.level_db));
        if !sections_valid {
            return fail("sections are not increasing bars of the track");
        }
        if self.vocal_db.len() > max_steps || !self.vocal_db.iter().copied().all(level) {
            return fail("vocal levels outside their range");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis() -> TrackAnalysis {
        TrackAnalysis {
            analyzer_version: 1,
            sample_rate: 44_100,
            duration_frames: 44_100 * 10,
            beats: (0..20).map(|beat| beat * 22_050).collect(),
            downbeats: vec![0, 4, 8, 12, 16],
            meter: Some(Meter {
                beats_per_bar: 4,
                subdivision: Subdivision::Binary,
                confidence: 1.0,
            }),
            tempo_bpm: Some(120.0),
            tempo_stability: 1.0,
            key: Some(KeyEstimate {
                tonic: PitchClass::new(9).unwrap(),
                mode: KeyMode::Minor,
                confidence: 0.8,
            }),
            integrated_lufs: Some(-9.5),
            true_peak_dbtp: -0.8,
            short_term_lufs: vec![-10.0; 20],
            bar_energy: vec![
                BandEnergy {
                    low_db: -20.0,
                    mid_db: -18.0,
                    high_db: -30.0
                };
                4
            ],
            sections: Vec::new(),
            vocal_db: Vec::new(),
            momentary_lufs: Vec::new(),
        }
    }

    #[test]
    fn a_consistent_analysis_validates_and_round_trips() {
        let value = analysis();
        value.validate().unwrap();
        let json = serde_json::to_string(&value).unwrap();
        let back: TrackAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(back, value);
        assert!((value.seconds(22_050) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn every_bound_is_enforced() {
        let broken: Vec<fn(&mut TrackAnalysis)> = vec![
            |a| a.sample_rate = 4_000,
            |a| a.duration_frames = 0,
            |a| a.beats.swap(1, 2),
            |a| a.beats.push(a.duration_frames),
            |a| a.downbeats = vec![0, 0],
            |a| a.downbeats.push(25),
            |a| {
                a.meter = Some(Meter {
                    beats_per_bar: 1,
                    subdivision: Subdivision::Binary,
                    confidence: 1.0,
                })
            },
            |a| a.tempo_bpm = Some(400.0),
            |a| a.tempo_bpm = Some(f32::NAN),
            |a| a.tempo_stability = 1.5,
            |a| a.integrated_lufs = Some(20.0),
            |a| a.true_peak_dbtp = f32::INFINITY,
            |a| a.short_term_lufs = vec![-10.0; 100],
            |a| a.short_term_lufs[0] = -90.0,
            |a| a.vocal_db = vec![-20.0; 100],
            |a| a.vocal_db = vec![f32::NAN],
            |a| a.momentary_lufs = vec![-10.0; 100],
            |a| a.momentary_lufs = vec![-90.0],
            |a| {
                a.bar_energy = vec![
                    BandEnergy {
                        low_db: 0.0,
                        mid_db: 0.0,
                        high_db: 0.0
                    };
                    6
                ]
            },
        ];
        for (index, breaks) in broken.into_iter().enumerate() {
            let mut value = analysis();
            breaks(&mut value);
            assert!(value.validate().is_err(), "case {index} should fail");
        }
    }

    #[test]
    fn the_audible_span_leaves_out_leading_and_trailing_silence() {
        // 30 s: 3 s of silence, music at -9 LUFS, then 7 s of silence.
        let steps = 60;
        let curve: Vec<f32> = (0..steps)
            .map(|step| if (6..46).contains(&step) { -9.0 } else { -70.0 })
            .collect();
        let track = TrackAnalysis {
            duration_frames: 44_100 * 30,
            integrated_lufs: Some(-9.0),
            short_term_lufs: curve.clone(),
            momentary_lufs: curve,
            ..analysis()
        };
        assert!(track.validate().is_ok());
        assert_eq!(track.seconds(track.audible_start()), 3.0);
        assert_eq!(track.seconds(track.audible_end()), 23.0);
        let older = TrackAnalysis {
            momentary_lufs: Vec::new(),
            ..track.clone()
        };
        let lagged = older.seconds(older.audible_end());
        assert!(
            (lagged - (23.0 - f64::from(TrackAnalysis::SHORT_TERM_LAG_SECONDS))).abs() < 0.01,
            "{lagged}"
        );
        let unmeasured = TrackAnalysis {
            integrated_lufs: None,
            ..track
        };
        assert_eq!(unmeasured.audible_start(), 0);
        assert_eq!(unmeasured.audible_end(), unmeasured.duration_frames);
    }

    #[test]
    fn pitch_classes_reject_values_outside_an_octave() {
        assert!(PitchClass::new(11).is_ok());
        assert!(PitchClass::new(12).is_err());
        assert!(serde_json::from_str::<PitchClass>("12").is_err());
        assert_eq!(serde_json::from_str::<PitchClass>("7").unwrap().value(), 7);
    }
}
