use crate::{
    check, invalid,
    wav::{Reader, Writer},
    CancellationToken, Result,
};
use rustfft::{num_complex::Complex, FftPlanner};
use sha2::{Digest, Sha256};
use std::{f64::consts::TAU, path::Path};
const FFT: usize = 2048;
const HOP: usize = 512;

#[derive(Clone, Debug, serde::Serialize)]
pub struct EqReport {
    pub seed_sha256: String,
    pub max_gain_db: f64,
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: usize,
    pub band_centers_hz: Vec<f64>,
    pub modulation_rates_hz: Vec<f64>,
    pub phases_radians: Vec<f64>,
}

/// Deterministic seven-band spectral EQ, applied only to the supplied stem.
/// Preserves exact frame count and shared stereo gain; uses bounded memory.
pub fn dynamic_instrumental_eq(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    seed: &str,
    max_gain_db: f64,
    cancel: &CancellationToken,
) -> Result<EqReport> {
    check(cancel)?;
    if !max_gain_db.is_finite() || !(0.0..=6.0).contains(&max_gain_db) {
        return Err(invalid("La ganancia EQ debe estar entre 0 y 6 dB."));
    }
    let digest = Sha256::digest(seed.as_bytes());
    let mut audio = Reader::open(source)?;
    let spec = audio.spec;
    let centers: Vec<f64> = [100., 220., 500., 1100., 2500., 5500., 11000.]
        .into_iter()
        .filter(|v| *v < spec.rate as f64 / 2.0)
        .collect();
    let phases: Vec<f64> = (0..centers.len())
        .map(|i| TAU * u16::from_le_bytes([digest[i * 2], digest[i * 2 + 1]]) as f64 / 65535.0)
        .collect();
    let rates: Vec<f64> = (0..centers.len())
        .map(|i| {
            0.14 + 0.20 * u16::from_le_bytes([digest[14 + i * 2], digest[15 + i * 2]]) as f64
                / 65535.0
        })
        .collect();
    let band_weights: Vec<Vec<f64>> = (0..=FFT / 2)
        .map(|bin| {
            let freq = (bin as f64 * spec.rate as f64 / FFT as f64)
                .max(20.0)
                .log2();
            centers
                .iter()
                .map(|center| (-0.5 * ((freq - center.log2()) / 0.55).powi(2)).exp())
                .collect()
        })
        .collect();
    let window: Vec<f64> = (0..FFT)
        .map(|i| (0.5 - 0.5 * (TAU * i as f64 / FFT as f64).cos()).sqrt())
        .collect();
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(FFT);
    let inverse = planner.plan_fft_inverse(FFT);
    let mut input = vec![0.0; FFT * spec.channels];
    let mut overlap = input.clone();
    let mut weight = vec![0.0; FFT];
    let mut spectrum = vec![Complex::default(); FFT];
    let mut output = Writer::new(destination, spec)?;
    let mut position = -(FFT as i64) + HOP as i64;
    let mut written = 0;
    while position < spec.frames as i64 {
        check(cancel)?;
        let block = audio.read(HOP)?;
        input.copy_within(HOP * spec.channels.., 0);
        input[(FFT - HOP) * spec.channels..].fill(0.0);
        input[(FFT - HOP) * spec.channels..(FFT - HOP) * spec.channels + block.len()]
            .copy_from_slice(&block);
        let time = (position as f64 + FFT as f64 / 2.0) / spec.rate as f64;
        let band_gains: Vec<f64> = rates
            .iter()
            .zip(&phases)
            .map(|(rate, phase)| max_gain_db * (TAU * rate * time + phase).sin())
            .collect();
        let gains: Vec<f64> = band_weights
            .iter()
            .map(|weights| {
                10_f64.powf(
                    weights
                        .iter()
                        .zip(&band_gains)
                        .map(|(w, g)| w * g)
                        .sum::<f64>()
                        .clamp(-max_gain_db, max_gain_db)
                        / 20.0,
                )
            })
            .collect();
        for channel in 0..spec.channels {
            for i in 0..FFT {
                spectrum[i] = Complex::new(input[i * spec.channels + channel] * window[i], 0.0);
            }
            forward.process(&mut spectrum);
            for (i, value) in spectrum.iter_mut().enumerate() {
                *value *= gains[i.min(FFT - i)];
            }
            inverse.process(&mut spectrum);
            for i in 0..FFT {
                overlap[i * spec.channels + channel] += spectrum[i].re / FFT as f64 * window[i];
            }
        }
        for (total, value) in weight.iter_mut().zip(&window) {
            *total += value * value;
        }
        if position >= 0 {
            let count = HOP.min((spec.frames - position as u64) as usize);
            let mut block = overlap[..count * spec.channels].to_vec();
            for (frame, samples) in block.chunks_exact_mut(spec.channels).enumerate() {
                for sample in samples {
                    *sample /= weight[frame];
                }
            }
            output.write(&block)?;
            written += count as u64;
        }
        overlap.copy_within(HOP * spec.channels.., 0);
        overlap[(FFT - HOP) * spec.channels..].fill(0.0);
        weight.copy_within(HOP.., 0);
        weight[FFT - HOP..].fill(0.0);
        position += HOP as i64;
    }
    if written != spec.frames {
        return Err(invalid("El EQ no conservó la duración."));
    }
    check(cancel)?;
    output.finish()?;
    Ok(EqReport {
        seed_sha256: format!("{digest:x}"),
        max_gain_db,
        frames: written,
        sample_rate: spec.rate,
        channels: spec.channels,
        band_centers_hz: centers,
        modulation_rates_hz: rates,
        phases_radians: phases,
    })
}
