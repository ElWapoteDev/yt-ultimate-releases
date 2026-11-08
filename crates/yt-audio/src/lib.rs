//! Local audio conversion with Rust orchestration and linked codec libraries.
//! Files are published only after successful decoding, encoding and flushing.
mod codecs;
mod fingerprint;
mod metadata;
mod streaming_flac;
mod variants;
pub use fingerprint::{
    compare_audio, compare_fingerprint_frames, fingerprint, AudioComparison, AudioFingerprint,
    FrameComparison,
};
pub use metadata::copy_metadata;
use rodio::Source;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
pub use variants::{create_variant, VariantOptions, VariantReport};
pub use yt_core::MediaMetadata;
use yt_media::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{0}")]
    Codec(String),
    #[error("La operación fue cancelada")]
    Cancelled,
    #[error("El archivo de destino ya existe")]
    DestinationExists,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, AudioError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3(u16),
    M4a(u16),
    Aac(u16),
    Ogg(u16),
    Opus(u16),
    Wav,
    Flac,
}

impl AudioFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3(_) => "mp3",
            Self::M4a(_) => "m4a",
            Self::Aac(_) => "aac",
            Self::Ogg(_) => "ogg",
            Self::Opus(_) => "opus",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
}

fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(AudioError::Cancelled)
    } else {
        Ok(())
    }
}

/// Convert an entire local audio track in bounded chunks. Cancellation is
/// checked between decode/encode blocks and before atomic publication.
pub fn convert(
    input: &Path,
    output: &Path,
    format: AudioFormat,
    cancel: &CancellationToken,
    progress: impl Fn(f64),
) -> Result<PathBuf> {
    convert_with_metadata(
        input,
        output,
        format,
        &MediaMetadata::default(),
        cancel,
        progress,
    )
}

/// Convert and preserve source tags/pictures, then apply explicit library or
/// download metadata before publishing the completed output.
pub fn convert_with_metadata(
    input: &Path,
    output: &Path,
    format: AudioFormat,
    metadata: &MediaMetadata,
    cancel: &CancellationToken,
    progress: impl Fn(f64),
) -> Result<PathBuf> {
    check(cancel)?;
    if output.exists() {
        return Err(AudioError::DestinationExists);
    }
    let mut source = yt_media::decode_file(input).map_err(|e| AudioError::Codec(e.to_string()))?;
    let rate = source.sample_rate();
    let channels = source.channels();
    if channels == 0 || channels > 2 {
        return Err(AudioError::Codec(
            "La conversión admite audio mono o estéreo".into(),
        ));
    }
    let expected = source
        .total_duration()
        .map(|d| d.as_secs_f64() * rate as f64 * channels as f64);
    let parent = output
        .parent()
        .ok_or_else(|| AudioError::Codec("Destino inválido".into()))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    let mut samples_read = 0u64;
    match format {
        AudioFormat::M4a(bitrate)
        | AudioFormat::Aac(bitrate)
        | AudioFormat::Ogg(bitrate)
        | AudioFormat::Opus(bitrate) => {
            samples_read = codecs::encode(
                &mut source,
                file.as_file_mut(),
                format,
                bitrate,
                cancel,
                &progress,
            )?;
        }
        AudioFormat::Wav => {
            let mut wav = hound::WavWriter::new(
                file.as_file_mut(),
                hound::WavSpec {
                    channels,
                    sample_rate: rate,
                    bits_per_sample: 24,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .map_err(codec)?;
            for sample in source.by_ref() {
                if samples_read.is_multiple_of(16384) {
                    check(cancel)?;
                    report_progress(samples_read, expected, &progress);
                }
                if !sample.is_finite() {
                    return Err(AudioError::Codec(
                        "La fuente contiene audio inválido".into(),
                    ));
                }
                wav.write_sample(pcm24(sample)).map_err(codec)?;
                samples_read += 1;
            }
            wav.finalize().map_err(codec)?;
        }
        AudioFormat::Mp3(bitrate) => {
            use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, MonoPcm, Quality};
            let bitrate = match bitrate {
                128 => Bitrate::Kbps128,
                192 => Bitrate::Kbps192,
                256 => Bitrate::Kbps256,
                320 => Bitrate::Kbps320,
                _ => return Err(AudioError::Codec("Bitrate no compatible".into())),
            };
            let mut builder = Builder::new()
                .ok_or_else(|| AudioError::Codec("No se pudo iniciar el codificador MP3".into()))?;
            builder.set_num_channels(channels as u8).map_err(codec)?;
            builder.set_sample_rate(rate).map_err(codec)?;
            builder.set_brate(bitrate).map_err(codec)?;
            builder.set_quality(Quality::Best).map_err(codec)?;
            let mut encoder = builder.build().map_err(codec)?;
            let mut encoded =
                Vec::<u8>::with_capacity(mp3lame_encoder::max_required_buffer_size(16384));
            loop {
                check(cancel)?;
                let samples = source
                    .by_ref()
                    .take(16384 * channels as usize)
                    .collect::<Vec<_>>();
                if samples.is_empty() {
                    break;
                }
                if samples.iter().any(|v| !v.is_finite()) {
                    return Err(AudioError::Codec(
                        "La fuente contiene audio inválido".into(),
                    ));
                }
                encoded.clear();
                if channels == 2 {
                    encoder
                        .encode_to_vec(InterleavedPcm(&samples), &mut encoded)
                        .map_err(codec)?;
                } else {
                    encoder
                        .encode_to_vec(MonoPcm(&samples), &mut encoded)
                        .map_err(codec)?;
                }
                file.write_all(&encoded)?;
                samples_read += samples.len() as u64;
                report_progress(samples_read, expected, &progress);
            }
            encoded.clear();
            encoder
                .flush_to_vec::<FlushNoGap>(&mut encoded)
                .map_err(codec)?;
            file.write_all(&encoded)?;
        }
        AudioFormat::Flac => {
            samples_read =
                streaming_flac::encode(&mut source, file.as_file_mut(), cancel, &progress)?;
        }
    }
    if samples_read == 0 {
        return Err(AudioError::Codec(
            "La fuente no contiene muestras de audio".into(),
        ));
    }
    if let Some(error) = source.decoding_error() {
        return Err(AudioError::Codec(error));
    }
    check(cancel)?;
    metadata::write_metadata(input, file.as_file_mut(), format, metadata, cancel)?;
    check(cancel)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(output).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            AudioError::DestinationExists
        } else {
            AudioError::Io(e.error)
        }
    })?;
    progress(1.0);
    Ok(output.to_owned())
}

fn pcm24(sample: f32) -> i32 {
    (sample.clamp(-1.0, 1.0) as f64 * 8_388_608.0)
        .round()
        .clamp(-8_388_608.0, 8_388_607.0) as i32
}
fn codec(error: impl std::fmt::Display) -> AudioError {
    AudioError::Codec(error.to_string())
}
fn report_progress(samples: u64, expected: Option<f64>, progress: &impl Fn(f64)) {
    if let Some(total) = expected.filter(|n| *n > 0.0) {
        progress((samples as f64 / total).min(0.99));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_during_flac_encoding_stops_without_publishing_or_leaving_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.wav");
        let output = dir.path().join("result.flac");
        wave(&input, 2);
        let original = std::fs::read(&input).unwrap();
        let cancel = CancellationToken::new();
        let encoding_started = std::cell::Cell::new(false);
        let result = convert(&input, &output, AudioFormat::Flac, &cancel, |fraction| {
            // Cancel after several encoded blocks, before final publication.
            if fraction > 0.45 && fraction < 0.9 {
                encoding_started.set(true);
                cancel.cancel();
            }
        });
        assert!(encoding_started.get());
        assert!(matches!(result, Err(AudioError::Cancelled)));
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }

    #[test]
    fn pcm24_stays_within_one_lsb_including_full_scale() {
        for sample in [-1.0, -0.99999, -0.5, 0.0, 0.5, 0.99999, 1.0] {
            let quantized = pcm24(sample) as f64 / 8_388_608.0;
            assert!((quantized - sample as f64).abs() <= 1.0 / 8_388_608.0);
        }
    }
    fn wave(path: &Path, channels: u16) {
        let mut w = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels,
                sample_rate: 44100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..44100 {
            for _ in 0..channels {
                w.write_sample((1000.0 * (frame as f64 * 0.03).sin()) as i16)
                    .unwrap();
            }
        }
        w.finalize().unwrap();
    }
    #[test]
    fn real_formats_roundtrip_and_preserve_source() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.wav");
        wave(&input, 2);
        let original = std::fs::read(&input).unwrap();
        for format in [
            AudioFormat::Mp3(192),
            AudioFormat::Wav,
            AudioFormat::Flac,
            AudioFormat::M4a(192),
            AudioFormat::Aac(192),
            AudioFormat::Ogg(192),
            AudioFormat::Opus(192),
        ] {
            let output = dir.path().join(format!("result.{}", format.extension()));
            convert(&input, &output, format, &CancellationToken::new(), |_| {}).unwrap();
            let decoded = yt_media::decode_file(&output)
                .unwrap_or_else(|error| panic!("{format:?}: {error}"));
            assert_eq!(decoded.channels(), 2);
            assert!(decoded.count() >= 44100 * 2 - 3000);
            assert_eq!(std::fs::read(&input).unwrap(), original);
            assert!(matches!(
                convert(&input, &output, format, &CancellationToken::new(), |_| {}),
                Err(AudioError::DestinationExists)
            ));
        }
    }
    #[test]
    fn cancellation_never_publishes_and_mono_encodes() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("mono.wav");
        wave(&input, 1);
        let output = dir.path().join("mono.mp3");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            convert(&input, &output, AudioFormat::Mp3(128), &cancel, |_| {}),
            Err(AudioError::Cancelled)
        ));
        assert!(!output.exists());
        convert(
            &input,
            &output,
            AudioFormat::Mp3(128),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        assert_eq!(yt_media::decode_file(&output).unwrap().channels(), 1);
    }

    #[test]
    fn extra_encoders_cancel_midstream_without_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.wav");
        wave(&input, 2);
        for format in [
            AudioFormat::M4a(192),
            AudioFormat::Aac(192),
            AudioFormat::Ogg(192),
            AudioFormat::Opus(192),
        ] {
            let output = dir.path().join(format!("result.{}", format.extension()));
            let token = CancellationToken::new();
            let result = convert(&input, &output, format, &token, |p| {
                if p > 0.1 {
                    token.cancel()
                }
            });
            assert!(
                matches!(result, Err(AudioError::Cancelled)),
                "{format:?}: {result:?}"
            );
            assert!(!output.exists());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn extra_encoders_preserve_pitch_channels_and_non_aligned_duration() {
        let dir = tempfile::tempdir().unwrap();
        for channels in [1u16, 2] {
            let input = dir.path().join(format!("source-{channels}.wav"));
            let rate = 44117;
            let frames = 44319;
            let mut writer = hound::WavWriter::create(
                &input,
                hound::WavSpec {
                    channels,
                    sample_rate: rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for frame in 0..frames {
                for channel in 0..channels {
                    let frequency = if channel == 0 { 440.0 } else { 660.0 };
                    writer
                        .write_sample(
                            (8000.0
                                * (frame as f64 * frequency * std::f64::consts::TAU / rate as f64)
                                    .sin()) as i16,
                        )
                        .unwrap();
                }
            }
            writer.finalize().unwrap();
            for format in [
                AudioFormat::M4a(320),
                AudioFormat::Aac(320),
                AudioFormat::Ogg(320),
                AudioFormat::Opus(320),
            ] {
                let output = dir
                    .path()
                    .join(format!("output-{channels}.{}", format.extension()));
                convert(&input, &output, format, &CancellationToken::new(), |_| {})
                    .unwrap_or_else(|e| panic!("{format:?}: {e}"));
                let decoded = yt_media::decode_file(&output).unwrap();
                assert_eq!(decoded.channels(), channels);
                let output_rate = decoded.sample_rate();
                let samples = decoded.collect::<Vec<_>>();
                let duration = samples.len() as f64 / channels as f64 / output_rate as f64;
                assert!(
                    (duration - frames as f64 / rate as f64).abs() < 0.065,
                    "{format:?} duration {duration}"
                );
                for channel in 0..channels as usize {
                    let signal = samples
                        .iter()
                        .skip(channel)
                        .step_by(channels as usize)
                        .copied()
                        .skip(output_rate as usize / 5)
                        .take(output_rate as usize / 2)
                        .collect::<Vec<_>>();
                    let crossings = signal
                        .windows(2)
                        .filter(|v| v[0] <= 0.0 && v[1] > 0.0)
                        .count();
                    let frequency = crossings as f64 / (signal.len() as f64 / output_rate as f64);
                    let expected = if channel == 0 { 440.0 } else { 660.0 };
                    assert!(
                        (frequency - expected).abs() < 5.0,
                        "{format:?} frequency {frequency}"
                    );
                    let rms = (signal.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
                        / signal.len() as f64)
                        .sqrt();
                    assert!((0.13..0.21).contains(&rms), "{format:?} rms {rms}");
                }
            }
        }
    }
}
