use crate::{
    check, codec, compare_audio, convert, AudioComparison, AudioError, AudioFormat, Result,
};
use rodio::Source;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};
use yt_media::CancellationToken;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct VariantOptions {
    pub pitch_semitones: f64,
    pub speed: f64,
    pub brightness_db: f64,
    pub low_gain_db: f64,
    pub high_gain_db: f64,
}
impl VariantOptions {
    pub fn chipmunk(pitch_semitones: f64, speed: f64, brightness_db: f64) -> Self {
        Self {
            pitch_semitones,
            speed,
            brightness_db,
            low_gain_db: 0.0,
            high_gain_db: 0.0,
        }
    }
    pub fn variation(seed: &str, strong: bool) -> Self {
        let hash = Sha256::digest(seed.as_bytes());
        let seed = u32::from_be_bytes(hash[..4].try_into().expect("SHA256 has 32 bytes"));
        Self {
            pitch_semitones: if strong { 1.75 } else { 1.0 } + (seed % 21) as f64 / 100.0,
            speed: 1.025 + ((seed / 21) % 11) as f64 / 1000.0,
            brightness_db: 0.0,
            low_gain_db: -0.6 - ((seed / 701) % 5) as f64 / 10.0,
            high_gain_db: 0.6 + ((seed / 1301) % 5) as f64 / 10.0,
        }
    }
    fn validate(self) -> Result<()> {
        if !self.pitch_semitones.is_finite()
            || !(0.0..=12.0).contains(&self.pitch_semitones)
            || !self.speed.is_finite()
            || !(0.5..=2.0).contains(&self.speed)
            || !self.brightness_db.is_finite()
            || !(0.0..=12.0).contains(&self.brightness_db)
            || !self.low_gain_db.is_finite()
            || self.low_gain_db.abs() > 12.0
            || !self.high_gain_db.is_finite()
            || self.high_gain_db.abs() > 12.0
        {
            return Err(AudioError::Codec(
                "Parámetros de variación inválidos".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct VariantReport {
    pub parameters: VariantOptions,
    pub normalization_gain: f64,
    pub comparison: AudioComparison,
}

/// Pitch/formant change with independent tempo, linked SoundTouch, Rust EQ and
/// one constant gain to keep sample peaks below .97. Originals are untouched.
/// SoundTouch replaces FFmpeg's atempo; results are not bit-identical to v4.
pub fn create_variant(
    input: &Path,
    output: &Path,
    options: VariantOptions,
    cancel: &CancellationToken,
    progress: impl Fn(f64),
) -> Result<VariantReport> {
    check(cancel)?;
    options.validate()?;
    if output.exists() {
        return Err(AudioError::DestinationExists);
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let work = tempfile::tempdir_in(parent)?;
    let processed = work.path().join("processed.wav");
    let normalized = work.path().join("normalized.wav");
    let encoded = work.path().join("variant.flac");
    let mut source = yt_media::decode_file(input).map_err(codec)?;
    let rate = source.sample_rate();
    let channels = source.channels();
    if !(1..=2).contains(&channels) || !(8000..=192000).contains(&rate) {
        return Err(AudioError::Codec(
            "La variación requiere audio mono o estéreo entre 8 y 192 kHz".into(),
        ));
    }
    let total = source
        .total_duration()
        .map(|d| d.as_secs_f64() * rate as f64 * channels as f64);
    let mut processor = soundtouch::SoundTouch::new();
    processor
        .set_channels(channels as u32)
        .set_sample_rate(rate)
        .set_pitch(2.0f64.powf(options.pitch_semitones / 12.0))
        .set_tempo(options.speed);
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wave = hound::WavWriter::create(&processed, spec).map_err(codec)?;
    let mut filters = (0..channels)
        .map(|_| {
            [
                Biquad::peaking(rate, 180.0, 0.8, options.low_gain_db),
                Biquad::peaking(rate, 4200.0, 1.1, options.high_gain_db),
                Biquad::shelf(rate, 3000.0, options.brightness_db),
            ]
        })
        .collect::<Vec<_>>();
    let mut peak = 0.0f64;
    let mut count = 0usize;
    let mut buffer = vec![0.0; 4096 * channels as usize];
    let mut drain = |processor: &mut soundtouch::SoundTouch| -> Result<()> {
        loop {
            check(cancel)?;
            let frames = processor.receive_samples(&mut buffer, 4096);
            if frames == 0 {
                break;
            }
            for (index, sample) in buffer[..frames * channels as usize].iter().enumerate() {
                let mut value = *sample as f64;
                for filter in &mut filters[index % channels as usize] {
                    value = filter.process(value);
                }
                if !value.is_finite() {
                    return Err(AudioError::Codec(
                        "El procesamiento generó audio inválido".into(),
                    ));
                }
                peak = peak.max(value.abs());
                wave.write_sample(value as f32).map_err(codec)?;
            }
        }
        Ok(())
    };
    loop {
        check(cancel)?;
        let samples = source
            .by_ref()
            .take(4096 * channels as usize)
            .collect::<Vec<_>>();
        if samples.is_empty() {
            break;
        }
        if !samples.len().is_multiple_of(channels as usize)
            || samples.iter().any(|v| !v.is_finite())
        {
            return Err(AudioError::Codec("Audio de entrada inválido".into()));
        }
        processor.put_samples(&samples, samples.len() / channels as usize);
        drain(&mut processor)?;
        count += samples.len();
        if let Some(total) = total.filter(|n| *n > 0.0) {
            progress((count as f64 / total).min(1.0) * 0.65);
        }
    }
    if let Some(error) = source.decoding_error() {
        return Err(AudioError::Codec(error));
    }
    if count == 0 {
        return Err(AudioError::Codec("La fuente no contiene audio".into()));
    }
    processor.flush();
    drain(&mut processor)?;
    wave.finalize().map_err(codec)?;
    let gain = if peak > 0.97 { 0.97 / peak } else { 1.0 };
    let mut reader = hound::WavReader::open(&processed).map_err(codec)?;
    let mut wave = hound::WavWriter::create(&normalized, spec).map_err(codec)?;
    for (index, sample) in reader.samples::<f32>().enumerate() {
        if index.is_multiple_of(8192) {
            check(cancel)?;
        }
        wave.write_sample((sample.map_err(codec)? as f64 * gain) as f32)
            .map_err(codec)?;
    }
    wave.finalize().map_err(codec)?;
    convert(&normalized, &encoded, AudioFormat::Flac, cancel, |p| {
        progress(0.65 + p * 0.25)
    })?;
    let comparison = compare_audio(input, &encoded, cancel)?;
    if !comparison.pcm_changed {
        return Err(AudioError::Codec(
            "El procesamiento no cambió las muestras de audio".into(),
        ));
    }
    check(cancel)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut std::fs::File::open(encoded)?, &mut file)?;
    file.flush()?;
    file.as_file().sync_all()?;
    check(cancel)?;
    file.persist_noclobber(output).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            AudioError::DestinationExists
        } else {
            AudioError::Io(e.error)
        }
    })?;
    progress(1.0);
    Ok(VariantReport {
        parameters: options,
        normalization_gain: gain,
        comparison,
    })
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}
impl Biquad {
    fn coefficients(b: [f64; 3], a: [f64; 3]) -> Self {
        Self {
            b: b.map(|v| v / a[0]),
            a: [a[1] / a[0], a[2] / a[0]],
            z: [0.0; 2],
        }
    }
    fn peaking(rate: u32, frequency: f64, q: f64, db: f64) -> Self {
        let w = std::f64::consts::TAU * frequency.min(rate as f64 * 0.45) / rate as f64;
        let a = 10.0f64.powf(db / 40.0);
        let alpha = w.sin() / (2.0 * q);
        let c = w.cos();
        Self::coefficients(
            [1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a],
        )
    }
    fn shelf(rate: u32, frequency: f64, db: f64) -> Self {
        let w = std::f64::consts::TAU * frequency.min(rate as f64 * 0.45) / rate as f64;
        let a = 10.0f64.powf(db / 40.0);
        let c = w.cos();
        let beta = w.sin() * ((a + 1.0 / a) * (1.0 / 0.7 - 1.0) + 2.0).sqrt() * a.sqrt();
        Self::coefficients(
            [
                a * ((a + 1.0) + (a - 1.0) * c + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - beta),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - beta,
            ],
        )
    }
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chipmunk_changes_pitch_without_changing_requested_duration() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.wav");
        let output = dir.path().join("out.flac");
        let mut wave = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 1,
                sample_rate: 44100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..44100 * 3 {
            wave.write_sample(
                (5000.0 * (std::f64::consts::TAU * 440.0 * i as f64 / 44100.0).sin()) as i16,
            )
            .unwrap();
        }
        wave.finalize().unwrap();
        let result = create_variant(
            &input,
            &output,
            VariantOptions::chipmunk(12.0, 1.0, 0.0),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        assert!(result.comparison.pcm_changed);
        assert!(!result.comparison.recognition_tested);
        let samples = yt_media::decode_file(&output).unwrap().collect::<Vec<_>>();
        assert!((samples.len() as i64 - 44100 * 3).abs() < 2048);
        let middle = &samples[44100..88200];
        let crossings = middle
            .windows(2)
            .filter(|v| v[0] <= 0.0 && v[1] > 0.0)
            .count();
        assert!(
            (crossings as i32 - 880).abs() < 12,
            "measured {crossings} Hz"
        );
        assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 0.970001));
    }
}
