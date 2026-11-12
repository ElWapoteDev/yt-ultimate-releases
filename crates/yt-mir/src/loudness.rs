//! EBU R128 loudness of the track and the band levels of each bar.

use crate::{audio::Pcm, MirError, Result};
use ebur128::{EbuR128, Mode};
use realfft::RealFftPlanner;
use yt_core::{BandEnergy, TrackAnalysis};

/// FFT length of the band analysis, in samples.
const BAND_FFT: usize = 2_048;
/// Hop of the band analysis, in samples.
const BAND_HOP: usize = 512;
/// Smallest power taken to a logarithm, so silent bars stay finite.
const POWER_FLOOR: f64 = 1e-20;

/// Whole-track loudness.
pub(crate) struct Loudness {
    pub(crate) integrated_lufs: Option<f32>,
    pub(crate) true_peak_dbtp: f32,
    pub(crate) short_term_lufs: Vec<f32>,
    pub(crate) momentary_lufs: Vec<f32>,
}

/// Integrated and short-term loudness and the true peak of `pcm`.
pub(crate) fn measure(pcm: &Pcm<'_>) -> Result<Loudness> {
    let failure = |error: ebur128::Error| MirError::InvalidAudio(format!("loudness: {error}"));
    let channels = u32::from(pcm.channels());
    let mut meter = EbuR128::new(
        channels,
        pcm.sample_rate(),
        Mode::I | Mode::S | Mode::M | Mode::TRUE_PEAK,
    )
    .map_err(failure)?;
    let step_frames = (TrackAnalysis::LOUDNESS_STEP_SECONDS * pcm.sample_rate() as f32) as usize;
    let mut short_term_lufs = Vec::new();
    let mut momentary_lufs = Vec::new();
    for block in pcm
        .samples()
        .chunks(step_frames.max(1) * usize::from(pcm.channels()))
    {
        meter.add_frames_f32(block).map_err(failure)?;
        let value = meter.loudness_shortterm().map_err(failure)?;
        short_term_lufs.push(lufs(value).unwrap_or(TrackAnalysis::SILENCE_LUFS));
        let momentary = meter.loudness_momentary().map_err(failure)?;
        momentary_lufs.push(lufs(momentary).unwrap_or(TrackAnalysis::SILENCE_LUFS));
    }
    let integrated_lufs = lufs(meter.loudness_global().map_err(failure)?);
    let mut peak = 0.0_f64;
    for channel in 0..channels {
        peak = peak.max(meter.true_peak(channel).map_err(failure)?);
    }
    Ok(Loudness {
        integrated_lufs,
        true_peak_dbtp: decibels(20.0 * peak.log10()),
        short_term_lufs,
        momentary_lufs,
    })
}

/// Mean level of the low, mid and high bands in each `[start, end)` bar
/// (seconds) of mono `audio` at `rate`, in dBFS (a full-scale sine reads -3).
pub(crate) fn bar_energy(audio: &[f32], rate: u32, bars: &[(f64, f64)]) -> Vec<BandEnergy> {
    if bars.is_empty() || audio.len() < BAND_FFT {
        return Vec::new();
    }
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(BAND_FFT);
    let window: Vec<f32> = (0..BAND_FFT)
        .map(|n| (0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / BAND_FFT as f64).cos()) as f32)
        .collect();
    // Parseval: one-sided bin powers times 2 / (N · Σw²) add up to the mean square.
    let window_energy: f64 = window.iter().map(|w| f64::from(*w).powi(2)).sum();
    let scale = 2.0 / (BAND_FFT as f64 * window_energy);
    let bin_hz = f64::from(rate) / BAND_FFT as f64;
    let low_end = (f64::from(TrackAnalysis::LOW_BAND_HZ) / bin_hz).ceil() as usize;
    let mid_end = (f64::from(TrackAnalysis::HIGH_BAND_HZ) / bin_hz).ceil() as usize;
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut frames: Vec<(f64, [f64; 3])> = Vec::with_capacity(audio.len() / BAND_HOP + 1);
    for start in (0..=audio.len() - BAND_FFT).step_by(BAND_HOP) {
        for ((slot, sample), weight) in input.iter_mut().zip(&audio[start..]).zip(&window) {
            *slot = sample * weight;
        }
        // Lengths come from the plan itself, so processing cannot fail.
        if fft.process(&mut input, &mut spectrum).is_err() {
            continue;
        }
        let mut bands = [0.0_f64; 3];
        for (bin, value) in spectrum.iter().enumerate().skip(1) {
            let band = if bin < low_end {
                0
            } else if bin < mid_end {
                1
            } else {
                2
            };
            bands[band] += f64::from(value.norm_sqr()) * scale;
        }
        let centre = (start + BAND_FFT / 2) as f64 / f64::from(rate);
        frames.push((centre, bands));
    }
    bars.iter()
        .map(|&(start, end)| {
            let from = frames.partition_point(|(time, _)| *time < start);
            let to = frames.partition_point(|(time, _)| *time < end);
            let mut sums = [0.0_f64; 3];
            for (_, bands) in &frames[from..to.max(from)] {
                for (sum, band) in sums.iter_mut().zip(bands) {
                    *sum += band;
                }
            }
            let count = (to.saturating_sub(from)).max(1) as f64;
            let level = |sum: f64| decibels(10.0 * (sum / count).max(POWER_FLOOR).log10());
            BandEnergy {
                low_db: level(sums[0]),
                mid_db: level(sums[1]),
                high_db: level(sums[2]),
            }
        })
        .collect()
}

/// A loudness reading in LUFS, or `None` below the EBU R128 absolute gate.
fn lufs(value: f64) -> Option<f32> {
    (value.is_finite() && value >= f64::from(TrackAnalysis::SILENCE_LUFS))
        .then(|| (value as f32).min(*TrackAnalysis::LUFS.end()))
}

/// A level in dB clamped to [`TrackAnalysis::LEVEL_DB`] (digital silence is
/// the low end).
fn decibels(value: f64) -> f32 {
    let (low, high) = (
        *TrackAnalysis::LEVEL_DB.start(),
        *TrackAnalysis::LEVEL_DB.end(),
    );
    if value.is_finite() {
        (value as f32).clamp(low, high)
    } else {
        low
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn tone(hz: f32, amplitude: f32, seconds: f32) -> Vec<f32> {
        (0..(44_100.0 * seconds) as usize)
            .flat_map(|n| {
                let value = amplitude * (TAU * hz * n as f32 / 44_100.0).sin();
                [value, value]
            })
            .collect()
    }

    #[test]
    fn louder_audio_measures_louder_and_silence_reads_the_gate() {
        let loud = tone(1_000.0, 0.5, 10.0);
        let quiet = tone(1_000.0, 0.05, 10.0);
        let loud = measure(&Pcm::new(&loud, 2, 44_100).unwrap()).unwrap();
        let quiet = measure(&Pcm::new(&quiet, 2, 44_100).unwrap()).unwrap();
        let difference = loud.integrated_lufs.unwrap() - quiet.integrated_lufs.unwrap();
        assert!((difference - 20.0).abs() < 0.2, "{difference}");
        assert!(
            (loud.true_peak_dbtp - 20.0 * 0.5_f32.log10()).abs() < 0.3,
            "{}",
            loud.true_peak_dbtp
        );
        assert_eq!(loud.short_term_lufs.len(), 20);
        assert_eq!(loud.momentary_lufs.len(), 20);
        let silence = vec![0.0; 44_100 * 2 * 6];
        let silence = measure(&Pcm::new(&silence, 2, 44_100).unwrap()).unwrap();
        assert_eq!(silence.integrated_lufs, None);
        assert!(silence
            .short_term_lufs
            .iter()
            .all(|v| *v == TrackAnalysis::SILENCE_LUFS));
        assert_eq!(silence.true_peak_dbtp, *TrackAnalysis::LEVEL_DB.start());
    }

    #[test]
    fn bar_energy_puts_each_tone_in_its_band() {
        let rate = 22_050;
        let bass: Vec<f32> = (0..rate * 4)
            .map(|n| (TAU * 60.0 * n as f32 / rate as f32).sin())
            .collect();
        let bars = bar_energy(&bass, rate as u32, &[(0.5, 3.5)]);
        assert_eq!(bars.len(), 1);
        let bar = bars[0];
        assert!((bar.low_db + 3.0).abs() < 0.5, "{bar:?}");
        assert!(
            bar.mid_db < bar.low_db - 30.0 && bar.high_db < bar.low_db - 30.0,
            "{bar:?}"
        );
        assert!(bar_energy(&bass, rate as u32, &[]).is_empty());
    }
}
