//! Bounded native AAC, Vorbis and Opus encoding. Containers and resampling are
//! handled in Rust; the codec libraries are linked from their bundled sources.
use crate::{check, codec, report_progress, AudioFormat, Result};
use rodio::Source;
use rubato::Resampler;
use std::{
    collections::VecDeque,
    fs::File,
    io::{Seek, SeekFrom, Write},
    num::{NonZeroU32, NonZeroU8},
    ptr,
};
use yt_media::{CancellationToken, DecodedAudio};

pub(crate) fn encode(
    source: &mut DecodedAudio,
    file: &mut File,
    format: AudioFormat,
    bitrate: u16,
    cancel: &CancellationToken,
    progress: &impl Fn(f64),
) -> Result<u64> {
    if !(32..=320).contains(&bitrate) {
        return Err(codec("El bitrate debe estar entre 32 y 320 kbps"));
    }
    let channels = source.channels() as usize;
    let input_rate = source.sample_rate();
    let expected = source
        .total_duration()
        .map(|d| d.as_secs_f64() * input_rate as f64 * channels as f64);
    let rate = match format {
        AudioFormat::Opus(_) => 48_000,
        AudioFormat::M4a(_) | AudioFormat::Aac(_)
            if ![8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000]
                .contains(&input_rate) =>
        {
            48_000
        }
        _ => input_rate,
    };
    let mut pcm = Pcm::new(source, rate, cancel)?;
    let update = |read| report_progress(read, expected, progress);
    match format {
        AudioFormat::M4a(_) => encode_aac(&mut pcm, file, bitrate, false, &update)?,
        AudioFormat::Aac(_) => encode_aac(&mut pcm, file, bitrate, true, &update)?,
        AudioFormat::Ogg(_) => encode_vorbis(&mut pcm, file, bitrate, &update)?,
        AudioFormat::Opus(_) => encode_opus(&mut pcm, file, bitrate, &update)?,
        _ => unreachable!("only additional encoders are dispatched here"),
    }
    Ok(pcm.read)
}

/// FFT resampling compensates its delay and trims to the precise rounded frame
/// count. Only a few chunks are resident, including while draining the filter.
struct Pcm<'a> {
    source: &'a mut DecodedAudio,
    cancel: &'a CancellationToken,
    channels: usize,
    input_rate: u32,
    rate: u32,
    read: u64,
    emitted: u64,
    skip: usize,
    eof: bool,
    pending: VecDeque<f32>,
    resampler: Option<rubato::FftFixedInOut<f32>>,
}
impl<'a> Pcm<'a> {
    fn new(source: &'a mut DecodedAudio, rate: u32, cancel: &'a CancellationToken) -> Result<Self> {
        let channels = source.channels() as usize;
        let input_rate = source.sample_rate();
        let resampler = if rate == input_rate {
            None
        } else {
            Some(
                rubato::FftFixedInOut::new(input_rate as usize, rate as usize, 2048, channels)
                    .map_err(codec)?,
            )
        };
        let skip = resampler.as_ref().map(|r| r.output_delay()).unwrap_or(0);
        Ok(Self {
            source,
            cancel,
            channels,
            input_rate,
            rate,
            read: 0,
            emitted: 0,
            skip,
            eof: false,
            pending: VecDeque::new(),
            resampler,
        })
    }
    fn frames(&mut self, count: usize) -> Result<Vec<f32>> {
        check(self.cancel)?;
        if self.resampler.is_none() {
            let values = self
                .source
                .by_ref()
                .take(count * self.channels)
                .collect::<Vec<_>>();
            self.read += values.len() as u64;
            self.validate(&values)?;
            return Ok(values);
        }
        while self.pending.len() < count * self.channels {
            let target = ((self.read / self.channels as u64) * self.rate as u64
                + self.input_rate as u64 / 2)
                / self.input_rate as u64;
            if self.eof && self.emitted >= target {
                break;
            }
            check(self.cancel)?;
            let needed = self.resampler.as_ref().unwrap().input_frames_next();
            let values = if self.eof {
                vec![]
            } else {
                self.source
                    .by_ref()
                    .take(needed * self.channels)
                    .collect::<Vec<_>>()
            };
            self.validate(&values)?;
            self.read += values.len() as u64;
            if values.len() < needed * self.channels {
                self.eof = true;
            }
            let mut planar = vec![vec![0.0; needed]; self.channels];
            for (i, value) in values.into_iter().enumerate() {
                planar[i % self.channels][i / self.channels] = value;
            }
            let output = self
                .resampler
                .as_mut()
                .unwrap()
                .process(&planar, None)
                .map_err(codec)?;
            let start = self.skip.min(output[0].len());
            self.skip -= start;
            let target = ((self.read / self.channels as u64) * self.rate as u64
                + self.input_rate as u64 / 2)
                / self.input_rate as u64;
            for frame in start..output[0].len() {
                if self.eof && self.emitted >= target {
                    break;
                }
                for channel in &output {
                    self.pending.push_back(channel[frame]);
                }
                self.emitted += 1;
            }
        }
        let count = (count * self.channels).min(self.pending.len());
        Ok(self.pending.drain(..count).collect())
    }
    fn validate(&self, values: &[f32]) -> Result<()> {
        if !values.len().is_multiple_of(self.channels) || values.iter().any(|v| !v.is_finite()) {
            return Err(codec("La fuente contiene cuadros de audio inválidos"));
        }
        if let Some(error) = self.source.decoding_error() {
            return Err(codec(error));
        }
        Ok(())
    }
}

pub(crate) fn resample_wave(
    input: &std::path::Path,
    output: &std::path::Path,
    rate: u32,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut source = yt_media::decode_file(input).map_err(codec)?;
    let channels = source.channels();
    let mut pcm = Pcm::new(&mut source, rate, cancel)?;
    let mut writer = hound::WavWriter::create(
        output,
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .map_err(codec)?;
    loop {
        let samples = pcm.frames(4096)?;
        if samples.is_empty() {
            break;
        }
        for sample in samples {
            writer.write_sample(sample).map_err(codec)?;
        }
    }
    check(cancel)?;
    writer.finalize().map_err(codec)
}

fn encode_vorbis(
    pcm: &mut Pcm<'_>,
    file: &mut File,
    bitrate: u16,
    progress: &impl Fn(u64),
) -> Result<()> {
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};
    let mut builder = VorbisEncoderBuilder::new(
        NonZeroU32::new(pcm.rate).ok_or_else(|| codec("Frecuencia inválida"))?,
        NonZeroU8::new(pcm.channels as u8).unwrap(),
        file,
    )
    .map_err(codec)?;
    // Vorbis nominal bitrate mode rejects valid low-rate/mono sources at high
    // requested rates. Its quality mode supports every source rate/channel
    // combination; map the requested rate per channel to its quality scale.
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: (0.5 + 0.4 * (bitrate as f32 * 2.0 / pcm.channels as f32 / 160.0).log2())
            .clamp(-0.1, 1.0),
    });
    let mut encoder = builder.build().map_err(codec)?;
    loop {
        let samples = pcm.frames(4096)?;
        if samples.is_empty() {
            break;
        }
        let mut planar = vec![Vec::with_capacity(samples.len() / pcm.channels); pcm.channels];
        for (i, v) in samples.into_iter().enumerate() {
            planar[i % pcm.channels].push(v);
        }
        encoder.encode_audio_block(&planar).map_err(codec)?;
        progress(pcm.read);
    }
    check(pcm.cancel)?;
    encoder.finish().map_err(codec)?;
    Ok(())
}

struct OpusEncoder(ptr::NonNull<opusic_sys::OpusEncoder>);
impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: this pointer was allocated by opus_encoder_create and is owned.
        unsafe {
            opusic_sys::opus_encoder_destroy(self.0.as_ptr());
        }
    }
}
fn encode_opus(
    pcm: &mut Pcm<'_>,
    file: &mut File,
    bitrate: u16,
    progress: &impl Fn(u64),
) -> Result<()> {
    use ogg::writing::{PacketWriteEndInfo as End, PacketWriter};
    use opusic_sys as sys;
    let mut error = 0;
    // SAFETY: validated mono/stereo configuration; the error pointer is writable.
    let pointer = unsafe {
        sys::opus_encoder_create(
            48000,
            pcm.channels as i32,
            sys::OPUS_APPLICATION_AUDIO,
            &mut error,
        )
    };
    let encoder =
        OpusEncoder(ptr::NonNull::new(pointer).ok_or_else(|| codec(format!("Opus: {error}")))?);
    let mut lookahead = 0i32;
    // SAFETY: each ctl uses the exact integer/pointer argument required by Opus.
    unsafe {
        for status in [
            sys::opus_encoder_ctl(
                pointer,
                sys::OPUS_SET_BITRATE_REQUEST,
                bitrate as i32 * 1000,
            ),
            sys::opus_encoder_ctl(pointer, sys::OPUS_SET_COMPLEXITY_REQUEST, 10i32),
            sys::opus_encoder_ctl(
                pointer,
                sys::OPUS_GET_LOOKAHEAD_REQUEST,
                &mut lookahead as *mut i32,
            ),
        ] {
            if status < 0 {
                return Err(codec(format!("Opus: {status}")));
            }
        }
    }
    let preskip = u16::try_from(lookahead).map_err(codec)?;
    let mut writer = PacketWriter::new(file);
    let serial = 0x5954554f;
    let mut head = b"OpusHead".to_vec();
    head.extend([1, pcm.channels as u8]);
    head.extend(preskip.to_le_bytes());
    head.extend(pcm.input_rate.to_le_bytes());
    head.extend([0, 0, 0]);
    writer.write_packet(head, serial, End::EndPage, 0)?;
    let vendor = b"YT Ultimate Rust";
    let mut tags = b"OpusTags".to_vec();
    tags.extend((vendor.len() as u32).to_le_bytes());
    tags.extend(vendor);
    tags.extend(0u32.to_le_bytes());
    writer.write_packet(tags, serial, End::EndPage, 0)?;
    let mut total = 0u64;
    let mut coded = 0u64;
    let mut eof = false;
    loop {
        check(pcm.cancel)?;
        let mut samples = if eof { vec![] } else { pcm.frames(960)? };
        let frames = samples.len() / pcm.channels;
        total += frames as u64;
        if frames < 960 {
            eof = true;
        }
        if total == 0 && eof {
            return Err(codec("La fuente no contiene muestras de audio"));
        }
        samples.resize(960 * pcm.channels, 0.0);
        let mut packet = vec![0u8; 4000];
        // SAFETY: samples contains 960 interleaved frames; packet capacity is
        // exactly the supplied writable bound. The encoder remains alive here.
        let size = unsafe {
            sys::opus_encode_float(
                encoder.0.as_ptr(),
                samples.as_ptr(),
                960,
                packet.as_mut_ptr(),
                packet.len() as i32,
            )
        };
        if size < 0 {
            return Err(codec(format!("Opus: {size}")));
        }
        packet.truncate(size as usize);
        coded += 960;
        let last = eof && coded >= total + preskip as u64;
        let granule = if last { total + preskip as u64 } else { coded };
        writer.write_packet(
            packet,
            serial,
            if last { End::EndStream } else { End::EndPage },
            granule,
        )?;
        progress(pcm.read);
        if last {
            break;
        }
    }
    Ok(())
}

struct AacEncoder(fdk_aac_sys::HANDLE_AACENCODER);
impl Drop for AacEncoder {
    fn drop(&mut self) {
        // SAFETY: the handle is exclusively owned and closed exactly once.
        unsafe {
            fdk_aac_sys::aacEncClose(&mut self.0);
        }
    }
}
fn aac_status(status: fdk_aac_sys::AACENC_ERROR) -> Result<()> {
    if status == fdk_aac_sys::AACENC_ERROR_AACENC_OK {
        Ok(())
    } else {
        Err(codec(format!("AAC: código {status}")))
    }
}
impl AacEncoder {
    fn new(
        rate: u32,
        channels: usize,
        bitrate: u16,
        adts: bool,
    ) -> Result<(Self, fdk_aac_sys::AACENC_InfoStruct)> {
        use fdk_aac_sys as s;
        let mut raw = ptr::null_mut();
        // SAFETY: writable handle, parameters use documented AAC-LC constants;
        // initialization and info reads occur only after each successful call.
        unsafe {
            aac_status(s::aacEncOpen(&mut raw, 0, channels as u32))?;
            let encoder = Self(raw);
            for (key, value) in [
                (s::AACENC_PARAM_AACENC_AOT, 2),
                (s::AACENC_PARAM_AACENC_SAMPLERATE, rate),
                (s::AACENC_PARAM_AACENC_CHANNELMODE, channels as u32),
                (s::AACENC_PARAM_AACENC_CHANNELORDER, 1),
                (s::AACENC_PARAM_AACENC_BITRATE, bitrate as u32 * 1000),
                (s::AACENC_PARAM_AACENC_TRANSMUX, if adts { 2 } else { 0 }),
            ] {
                aac_status(s::aacEncoder_SetParam(raw, key, value))?;
            }
            aac_status(s::aacEncEncode(
                raw,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
            ))?;
            let mut info = std::mem::MaybeUninit::uninit();
            aac_status(s::aacEncInfo(raw, info.as_mut_ptr()))?;
            Ok((encoder, info.assume_init()))
        }
    }
    fn frame(&mut self, input: Option<&[i16]>, output: &mut [u8]) -> Result<(usize, usize, bool)> {
        use fdk_aac_sys as s;
        let mut input_pointer = input.map_or(ptr::null_mut(), |v| v.as_ptr().cast_mut().cast());
        let mut output_pointer = output.as_mut_ptr().cast();
        let mut input_id = s::AACENC_BufferIdentifier_IN_AUDIO_DATA as i32;
        let mut output_id = s::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as i32;
        let mut input_size = input.map_or(0, |v| std::mem::size_of_val(v) as i32);
        let mut output_size = output.len() as i32;
        let mut input_element = 2;
        let mut output_element = 1;
        let input_buffer = s::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut input_pointer,
            bufferIdentifiers: &mut input_id,
            bufSizes: &mut input_size,
            bufElSizes: &mut input_element,
        };
        let output_buffer = s::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut output_pointer,
            bufferIdentifiers: &mut output_id,
            bufSizes: &mut output_size,
            bufElSizes: &mut output_element,
        };
        let args = s::AACENC_InArgs {
            numInSamples: input.map_or(-1, |v| v.len() as i32),
            numAncBytes: 0,
        };
        // SAFETY: descriptors point into live buffers and all sizes are bytes;
        // -1 input samples requests documented end-of-input draining.
        unsafe {
            let mut result = std::mem::zeroed::<s::AACENC_OutArgs>();
            let status = s::aacEncEncode(self.0, &input_buffer, &output_buffer, &args, &mut result);
            if status == s::AACENC_ERROR_AACENC_ENCODE_EOF {
                return Ok((0, 0, true));
            }
            aac_status(status)?;
            Ok((
                result.numInSamples as usize,
                result.numOutBytes as usize,
                false,
            ))
        }
    }
}

fn encode_aac(
    pcm: &mut Pcm<'_>,
    file: &mut File,
    bitrate: u16,
    adts: bool,
    progress: &impl Fn(u64),
) -> Result<()> {
    use shiguredo_mp4::{
        boxes::{
            AudioSampleEntryFields, EdtsBox, ElstBox, ElstEntry, EsdsBox, Mp4aBox, SampleEntry,
        },
        descriptors::{
            DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor,
        },
        mux::{Mp4FileMuxer, Sample},
        Encode, FixedPointNumber, TrackKind, Uint,
    };
    let (mut encoder, info) = AacEncoder::new(pcm.rate, pcm.channels, bitrate, adts)?;
    let entry = SampleEntry::Mp4a(Mp4aBox {
        audio: AudioSampleEntryFields {
            data_reference_index: std::num::NonZeroU16::MIN,
            channelcount: pcm.channels as u16,
            samplesize: 16,
            samplerate: FixedPointNumber::new(pcm.rate as u16, 0),
        },
        esds_box: EsdsBox {
            es: EsDescriptor {
                es_id: 1,
                stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
                depends_on_es_id: None,
                url_string: None,
                ocr_es_id: None,
                dec_config_descr: DecoderConfigDescriptor {
                    object_type_indication: 0x40,
                    stream_type: DecoderConfigDescriptor::STREAM_TYPE_AUDIO,
                    up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                    buffer_size_db: Uint::new(info.maxOutBufBytes),
                    max_bitrate: bitrate as u32 * 1000,
                    avg_bitrate: bitrate as u32 * 1000,
                    dec_specific_info: Some(DecoderSpecificInfo {
                        payload: info.confBuf[..info.confSize as usize].to_vec(),
                    }),
                },
                sl_config_descr: SlConfigDescriptor,
            },
        },
        unknown_boxes: vec![],
    });
    let mut mux = Mp4FileMuxer::new().map_err(codec)?;
    if !adts {
        file.write_all(mux.initial_boxes_bytes())?;
    }
    let mut offset = mux.initial_boxes_bytes().len() as u64;
    let mut first = Some(entry);
    let mut output = vec![0u8; info.maxOutBufBytes as usize];
    let mut total = 0u64;
    let mut frames = 0u64;
    let mut eof = false;
    loop {
        check(pcm.cancel)?;
        let values = if eof {
            vec![]
        } else {
            pcm.frames(info.frameLength as usize)?
        };
        if values.is_empty() {
            eof = true;
        }
        total += (values.len() / pcm.channels) as u64;
        let input = values
            .into_iter()
            .map(|v| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .collect::<Vec<_>>();
        let mut used = 0;
        loop {
            check(pcm.cancel)?;
            let (consumed, count, done) =
                encoder.frame((!eof).then_some(&input[used..]), &mut output)?;
            if done {
                break;
            }
            if consumed > input.len().saturating_sub(used) {
                return Err(codec("Consumo de AAC inválido"));
            }
            used += consumed;
            if count > 0 {
                file.write_all(&output[..count])?;
                if !adts {
                    mux.append_sample(&Sample {
                        track_kind: TrackKind::Audio,
                        sample_entry: first.take(),
                        keyframe: true,
                        timescale: NonZeroU32::new(pcm.rate).unwrap(),
                        duration: info.frameLength,
                        composition_time_offset: None,
                        data_offset: offset,
                        data_size: count,
                    })
                    .map_err(codec)?;
                }
                offset += count as u64;
                frames += 1;
                if frames > 8_000_000 {
                    return Err(codec("El archivo supera el límite de cuadros AAC"));
                }
            }
            if eof {
                if count == 0 {
                    return Err(codec("El codificador AAC no pudo finalizar"));
                }
                continue;
            }
            if used == input.len() {
                break;
            }
            if consumed == 0 && count == 0 {
                return Err(codec("El codificador AAC no avanzó"));
            }
        }
        progress(pcm.read);
        if eof {
            break;
        }
    }
    if total == 0 {
        return Err(codec("La fuente no contiene muestras de audio"));
    }
    if adts {
        return Ok(());
    }
    let finalized = mux.finalize().map_err(codec)?;
    if finalized.is_faststart_enabled() {
        return Err(codec("Disposición AAC inesperada"));
    }
    for (position, bytes) in finalized.offset_and_bytes_pairs() {
        file.seek(SeekFrom::Start(position))?;
        file.write_all(bytes)?;
    }
    let mut moov = finalized.moov_box().clone();
    let duration =
        (total as u128 * moov.mvhd_box.timescale.get() as u128 / pcm.rate as u128) as u64;
    moov.mvhd_box.duration = duration;
    let track = &mut moov.trak_boxes[0];
    track.tkhd_box.duration = duration;
    track.edts_box = Some(EdtsBox {
        elst_box: Some(ElstBox {
            entries: vec![ElstEntry {
                edit_duration: duration,
                media_time: info.nDelay as i64,
                media_rate: FixedPointNumber::new(1, 0),
            }],
        }),
        unknown_boxes: vec![],
    });
    let bytes = moov.encode_to_vec().map_err(codec)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(&bytes)?;
    file.set_len(offset + bytes.len() as u64)?;
    Ok(())
}
