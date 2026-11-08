//! Local audio conversion with Rust orchestration and linked codec libraries.
//! Files are published only after successful decoding, encoding and flushing.
mod fingerprint;
mod variants;
pub use fingerprint::{
    compare_audio, compare_fingerprint_frames, fingerprint, AudioComparison, AudioFingerprint,
    FrameComparison,
};
use rodio::Source;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
pub use variants::{create_variant, VariantOptions, VariantReport};
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
    Wav,
    Flac,
}

impl AudioFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3(_) => "mp3",
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

/// Convert an entire local audio track. MP3 and WAV encode in bounded chunks.
/// FLAC caps PCM input storage at 512 MiB; the encoded stream needs additional
/// memory. The encoder borrows this PCM buffer and checks cancellation per block.
pub fn convert(
    input: &Path,
    output: &Path,
    format: AudioFormat,
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
            use flacenc::{component::BitRepr, error::Verify};
            let mut samples = Vec::<i32>::new();
            for sample in source.by_ref() {
                if samples.len().is_multiple_of(16384) {
                    check(cancel)?;
                    report_progress(samples.len() as u64, expected, &|fraction| {
                        progress(fraction * 0.45)
                    });
                }
                if samples.len() >= 128 * 1024 * 1024 {
                    return Err(AudioError::Codec(
                        "Este archivo supera el límite de memoria para exportar FLAC".into(),
                    ));
                }
                if !sample.is_finite() {
                    return Err(AudioError::Codec(
                        "La fuente contiene audio inválido".into(),
                    ));
                }
                samples.push(pcm24(sample));
            }
            samples_read = samples.len() as u64;
            if samples.is_empty() || !samples.len().is_multiple_of(channels as usize) {
                return Err(AudioError::Codec(
                    "La fuente no contiene cuadros de audio completos".into(),
                ));
            }
            if let Some(error) = source.decoding_error() {
                return Err(AudioError::Codec(error));
            }
            check(cancel)?;
            let config = flacenc::config::Encoder::default()
                .into_verified()
                .map_err(|_| AudioError::Codec("Configuración FLAC inválida".into()))?;
            progress(0.45);
            let signal = CancelableFlacSource {
                samples: &samples,
                channels: channels as usize,
                rate: rate as usize,
                position: 0,
                cancel,
                progress: &progress,
            };
            let mut stream =
                flacenc::encode_with_fixed_block_size(&config, signal, config.block_size)
                    .map_err(codec)?;
            // Cancellation ends source reads gracefully so flacenc's parallel
            // workers can drain and join; the partial stream is never written.
            check(cancel)?;
            drop(samples);
            // RFC 9639 §8.2: the fixed stream's minimum excludes the short final
            // frame. flacenc 0.5.1 includes it when updating frame statistics.
            stream
                .stream_info_mut()
                .set_block_sizes(config.block_size, config.block_size)
                .map_err(codec)?;
            let mut sink = flacenc::bitsink::ByteSink::new();
            stream.write(&mut sink).map_err(codec)?;
            check(cancel)?;
            drop(stream);
            for chunk in sink.as_slice().chunks(1024 * 1024) {
                check(cancel)?;
                file.write_all(chunk)?;
            }
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

/// Returning end-of-stream on cancellation is deliberate: flacenc 0.5's
/// parallel feeder does not join its workers when Source returns an error.
/// The caller always checks the token before publishing this truncated stream.
struct CancelableFlacSource<'a> {
    samples: &'a [i32],
    channels: usize,
    rate: usize,
    position: usize,
    cancel: &'a CancellationToken,
    progress: &'a dyn Fn(f64),
}

impl flacenc::source::Source for CancelableFlacSource<'_> {
    fn channels(&self) -> usize {
        self.channels
    }

    fn bits_per_sample(&self) -> usize {
        24
    }

    fn sample_rate(&self) -> usize {
        self.rate
    }

    fn len_hint(&self) -> Option<usize> {
        Some(self.samples.len() / self.channels)
    }

    fn read_samples<F: flacenc::source::Fill>(
        &mut self,
        block_size: usize,
        destination: &mut F,
    ) -> std::result::Result<usize, flacenc::error::SourceError> {
        if self.cancel.is_cancelled() {
            return Ok(0);
        }
        let end = self
            .position
            .saturating_add(block_size.saturating_mul(self.channels))
            .min(self.samples.len());
        let samples = &self.samples[self.position..end];
        destination.fill_interleaved(samples)?;
        self.position = end;
        if !samples.is_empty() {
            (self.progress)(0.45 + 0.45 * end as f64 / self.samples.len() as f64);
        }
        Ok(samples.len() / self.channels)
    }
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
            // Decoding reports < .45; this is the first actual encoder block.
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
        for format in [AudioFormat::Mp3(192), AudioFormat::Wav, AudioFormat::Flac] {
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
}
