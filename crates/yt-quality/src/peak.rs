use crate::{check, wav::Reader, CancellationToken, Result};
use std::{f64::consts::PI, path::Path};

#[derive(Clone, Debug, serde::Serialize)]
pub struct PeakReport {
    pub method: &'static str,
    pub peak_linear: f64,
    pub peak_dbtp: Option<f64>,
}

/// Engineering true-peak estimator equivalent to scipy resample_poly(x,8,1),
/// its default 161-tap Kaiser(beta=5) low-pass, and zero padding at file edges.
/// This is not an IEC certification claim.
pub fn estimate_true_peak(
    source: impl AsRef<Path>,
    cancel: &CancellationToken,
) -> Result<PeakReport> {
    check(cancel)?;
    let mut audio = Reader::open(source)?;
    let filter = interpolation_filter();
    let mut peak = 0.0_f64;
    for position in (0..audio.spec.frames).step_by(65536) {
        check(cancel)?;
        let start = position.saturating_sub(64);
        let end = (position + 65536 + 64).min(audio.spec.frames);
        audio.seek(start)?;
        let samples = audio.read((end - start) as usize)?;
        let first = (position - start) as usize;
        let stop = ((position + 65536).min(audio.spec.frames) - start) as usize;
        peak = peak.max(block_peak(
            &samples,
            audio.spec.channels,
            first,
            stop,
            &filter,
        ));
    }
    Ok(PeakReport {
        method: "fir_polyphase_8x_kaiser5",
        peak_linear: peak,
        peak_dbtp: (peak > 0.0).then(|| 20.0 * peak.log10()),
    })
}

fn i0(x: f64) -> f64 {
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..50 {
        term *= (x * x / 4.0) / (k * k) as f64;
        sum += term;
        if term.abs() < sum.abs() * 1e-17 {
            break;
        }
    }
    sum
}

fn interpolation_filter() -> Vec<f64> {
    let denominator = i0(5.0);
    let mut taps: Vec<f64> = (0..161)
        .map(|i| {
            let t = i as f64 - 80.0;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (PI * t / 8.0).sin() / (PI * t / 8.0)
            };
            sinc / 8.0 * i0(5.0 * (1.0 - (t / 80.0).powi(2)).max(0.0).sqrt()) / denominator
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for tap in &mut taps {
        *tap *= 8.0 / sum;
    }
    taps
}

fn block_peak(samples: &[f64], channels: usize, first: usize, stop: usize, taps: &[f64]) -> f64 {
    let mut peak = 0.0_f64;
    let frames = samples.len() / channels;
    for frame in first..stop {
        for phase in 0..8 {
            for channel in 0..channels {
                let mut value = 0.0;
                for offset in -10_isize..=10 {
                    let input = frame as isize + offset;
                    let tap = 80 + phase as isize - offset * 8;
                    if input >= 0 && (input as usize) < frames && (0..161).contains(&tap) {
                        value += samples[input as usize * channels + channel] * taps[tap as usize];
                    }
                }
                peak = peak.max(value.abs());
            }
        }
    }
    peak
}
