//! Streaming container decoder with a bundled Opus codec and no subprocess.
use crate::{MediaError, Result};
use rodio::{source::SeekError, Source};
use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CodecRegistry, Decoder, DecoderOptions},
    errors::Error,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
    probe::Hint,
    units::TimeBase,
};

static CODECS: OnceLock<CodecRegistry> = OnceLock::new();

fn codecs() -> &'static CodecRegistry {
    CODECS.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_all::<symphonia_adapter_libopus::OpusDecoder>();
        registry
    })
}

/// Open MP3, M4A/AAC/ALAC, FLAC, WAV, Ogg/Vorbis/Opus or an audio track
/// inside Matroska/WebM/MP4. Memory usage is bounded by one decoded packet.
pub fn decode_file(path: impl AsRef<Path>) -> Result<DecodedAudio> {
    let path = path.as_ref();
    DecodedAudio::open(
        Box::new(File::open(path)?),
        path.extension().and_then(|e| e.to_str()),
    )
}

pub struct DecodedAudio {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    channels: u16,
    sample_rate: u32,
    duration: Option<Duration>,
    buffer: Vec<f32>,
    cursor: usize,
    ended: bool,
    last_error: Arc<Mutex<Option<String>>>,
}

impl DecodedAudio {
    fn open(source: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Self> {
        let mut hint = Hint::new();
        if let Some(extension) = extension {
            hint.with_extension(extension);
        }
        let stream = MediaSourceStream::new(source, Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(decode_error)?;
        // Some containers put the video track first. Pick a track for which
        // we actually have an audio decoder, rather than assuming track zero.
        let (track, decoder) = probed
            .format
            .tracks()
            .iter()
            .find_map(|track| {
                codecs()
                    .make(&track.codec_params, &DecoderOptions::default())
                    .ok()
                    .map(|decoder| (track.clone(), decoder))
            })
            .ok_or_else(|| {
                MediaError::Playback("El archivo no contiene una pista de audio compatible.".into())
            })?;
        let params = &track.codec_params;
        let duration = params.time_base.zip(params.n_frames).map(|(base, frames)| {
            let time = base.calc_time(frames);
            Duration::from_secs(time.seconds) + Duration::from_secs_f64(time.frac)
        });
        let mut source = Self {
            format: probed.format,
            decoder,
            track_id: track.id,
            time_base: params.time_base,
            channels: params.channels.map(|c| c.count() as u16).unwrap_or(2),
            sample_rate: params.sample_rate.unwrap_or(48_000),
            duration,
            buffer: Vec::new(),
            cursor: 0,
            ended: false,
            last_error: Arc::new(Mutex::new(None)),
        };
        if !source.read_packet().map_err(decode_error)? {
            return Err(MediaError::Playback(
                "El archivo no contiene muestras de audio.".into(),
            ));
        }
        Ok(source)
    }

    pub(crate) fn error_handle(&self) -> Arc<Mutex<Option<String>>> {
        self.last_error.clone()
    }

    /// A terminal decoding failure, if iteration ended before normal EOF.
    pub fn decoding_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn read_packet(&mut self) -> std::result::Result<bool, Error> {
        let mut corrupt_packets = 0;
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false)
                }
                Err(error) => return Err(error),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(Error::DecodeError(_)) if corrupt_packets < 16 => {
                    corrupt_packets += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if decoded.frames() == 0 {
                continue;
            }
            let spec = *decoded.spec();
            self.channels = spec.channels.count() as u16;
            self.sample_rate = spec.rate;
            let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            samples.copy_interleaved_ref(decoded);
            self.buffer.clear();
            self.buffer.extend_from_slice(samples.samples());
            self.cursor = 0;
            return Ok(true);
        }
    }
}

fn decode_error(error: Error) -> MediaError {
    MediaError::Playback(error.to_string())
}

impl Iterator for DecodedAudio {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.ended {
            return None;
        }
        if self.cursor >= self.buffer.len() {
            match self.read_packet() {
                Ok(true) => {}
                Ok(false) => {
                    self.ended = true;
                    return None;
                }
                Err(error) => {
                    *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.to_string());
                    self.ended = true;
                    return None;
                }
            }
        }
        let sample = self.buffer.get(self.cursor).copied();
        self.cursor += 1;
        sample
    }
}

impl Source for DecodedAudio {
    fn current_span_len(&self) -> Option<usize> {
        // A drained packet is not EOF: next() loads the following packet.
        // Returning zero here makes Rodio's uniform converter stop at the
        // first packet boundary. Match Rodio's Symphonia source contract.
        Some(self.buffer.len())
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        let position = self
            .duration
            .map(|duration| position.min(duration))
            .unwrap_or(position);
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: position.into(),
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| SeekError::Other(Box::new(e)))?;
        self.decoder.reset();
        self.buffer.clear();
        self.cursor = 0;
        self.ended = false;
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(time_base) = self.time_base {
            let gap = time_base.calc_time(seeked.required_ts.saturating_sub(seeked.actual_ts));
            let frames = ((gap.seconds as f64 + gap.frac) * self.sample_rate as f64).round() as u64;
            for _ in 0..frames.saturating_mul(self.channels as u64) {
                if self.next().is_none() {
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opus_decodes_and_seeks_without_external_programs() {
        let source = std::io::Cursor::new(include_bytes!("../tests/fixtures/tone.opus").to_vec());
        let mut decoder = DecodedAudio::open(Box::new(source), Some("opus")).unwrap();
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.sample_rate(), 48_000);
        assert!(decoder.by_ref().take(4800).any(|v| v.abs() > 0.01));
        decoder.try_seek(Duration::from_millis(400)).unwrap();
        assert!(decoder.by_ref().take(4800).any(|v| v.abs() > 0.01));
        decoder.try_seek(Duration::ZERO).unwrap();
        assert!(decoder.next().is_some());
    }

    #[test]
    fn uniform_playback_continues_across_packet_boundaries() {
        let open = || {
            let source =
                std::io::Cursor::new(include_bytes!("../tests/fixtures/tone.opus").to_vec());
            DecodedAudio::open(Box::new(source), Some("opus")).unwrap()
        };
        let expected = open().count();
        let actual = rodio::source::UniformSourceIterator::new(open(), 2, 48_000).count();
        assert!(actual >= 90_000);
        assert_eq!(actual, expected);
    }
}
