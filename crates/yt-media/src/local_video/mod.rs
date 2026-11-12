//! Lossless local-container preparation. It never modifies or imports the source.
mod scan;

use crate::{CancellationToken, MediaError, MuxProgress, Result};
use matroska_demuxer::{MatroskaFile, TrackEntry, TrackType};
use scan::Packet;
use shiguredo_mp4::{
    bitstream::{aac, h264, h265},
    boxes::{
        Avc1Box, AvccBox, EdtsBox, ElstBox, ElstEntry, HdlrBox, Hvc1Box, HvccBox, SampleEntry,
        TrakBox, UnknownBox, VisualSampleEntryFields,
    },
    mux::{Mp4FileMuxer, Sample},
    Decode, Encode, FixedPointNumber, TrackKind,
};
use std::{
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
};

const NS: i128 = 1_000_000_000;
const VIDEO_SCALE: u32 = 1_000_000;

#[derive(Debug)]
pub struct PreparedLocalVideo {
    _directory: tempfile::TempDir,
    path: PathBuf,
    duration: f64,
    width: u32,
    height: u32,
    warnings: Vec<String>,
}
impl PreparedLocalVideo {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn duration_seconds(&self) -> f64 {
        self.duration
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

fn invalid(reason: impl std::fmt::Display) -> MediaError {
    MediaError::Processing(format!("No se pudo preparar el vídeo local: {reason}"))
}
fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(MediaError::Cancelled)
    } else {
        Ok(())
    }
}

struct Video {
    id: u64,
    entry: SampleEntry,
    length_size: usize,
    width: u32,
    height: u32,
    times: Vec<(u32, i64)>,
    start_ns: i128,
    end_ns: i128,
    media_start: i64,
    media_end: u64,
}
struct Audio {
    id: u64,
    entry: SampleEntry,
    rate: u32,
    frames: u32,
    start_ns: i128,
    end_ns: i128,
    media_start: i64,
}

/// Prepare local Matroska AVC/HEVC + AAC as an owned MP4, or VP8/VP9 with
/// Opus/Vorbis as WebM. This blocks; use a worker. Coded pictures, presentation
/// times and AAC trim are preserved; duplicate in-band parameter sets move to
/// the MP4 sample entry. Decoding support still belongs to the system WebView.
pub fn prepare_local_video(
    input: &Path,
    cancel: CancellationToken,
    on_progress: impl Fn(MuxProgress),
) -> Result<PreparedLocalVideo> {
    check_cancel(&cancel)?;
    let mut source = File::open(input)?;
    let original = source.metadata()?;
    if !original.is_file() {
        return Err(invalid("el origen no es un archivo"));
    }
    let size = original.len();
    on_progress(MuxProgress {
        copied_bytes: 0,
        total_bytes: size,
    });
    let index = scan::index(&mut source, &cancel)?;
    let metadata = MatroskaFile::open(Cursor::new(&index.metadata)).map_err(invalid)?;
    let scale = metadata.info().timestamp_scale().get();
    if scale > 1_000_000_000 {
        return Err(invalid("la escala temporal no es compatible"));
    }
    let track = selected_track(metadata.tracks(), TrackType::Video)
        .ok_or_else(|| invalid("no hay pista de vídeo"))?;
    if matches!(track.codec_id(), "V_VP8" | "V_VP9") {
        return prepare_webm(source, &original, &index, &metadata, &cancel, on_progress);
    }
    let video = prepare_video_track(track, &index.packets, scale)?;
    let audio = metadata
        .tracks()
        .iter()
        .rev()
        .filter(|track| {
            track.track_type() == TrackType::Audio
                && track.flag_enabled()
                && (track.codec_id() == "A_AAC" || track.codec_id().starts_with("A_AAC/"))
                && plain_track(track).is_ok()
        })
        .max_by_key(|track| (track.flag_default(), !track.flag_commentary()))
        .map(|track| prepare_audio_track(track, &index.packets, scale))
        .transpose()?;
    let warnings = missing_audio_warning(metadata.tracks(), audio.is_some());
    let directory = tempfile::Builder::new()
        .prefix("ytu-local-video-")
        .tempdir()?;
    let path = directory.path().join("ready.mp4");
    let mut output = File::create(&path)?;
    let mut mux = Mp4FileMuxer::new().map_err(invalid)?;
    output.write_all(mux.initial_boxes_bytes())?;
    let mut position = mux.initial_boxes_bytes().len() as u64;
    let mut vi = 0;
    let mut ai = 0;
    let mut buffer = [0; 64 * 1024];
    for packet in &index.packets {
        check_cancel(&cancel)?;
        let mut ranges = vec![(packet.offset, packet.size)];
        let sample = if packet.track == video.id {
            let unit = validate_access_unit(&mut source, packet, &video)?;
            let keyframe = unit.keyframe;
            ranges = unit.ranges;
            if vi == 0 && !keyframe {
                return Err(invalid(
                    "el primer cuadro no es un punto de acceso independiente",
                ));
            }
            let (duration, offset) = video.times[vi];
            let sample = Sample {
                track_kind: TrackKind::Video,
                sample_entry: (vi == 0).then(|| video.entry.clone()),
                keyframe: keyframe && packet.keyframe,
                timescale: NonZeroU32::new(VIDEO_SCALE).unwrap(),
                duration,
                composition_time_offset: Some(offset),
                data_offset: position,
                data_size: ranges.iter().map(|(_, size)| size).sum(),
            };
            vi += 1;
            sample
        } else if let Some(audio) = audio.as_ref().filter(|a| a.id == packet.track) {
            let sample = Sample {
                track_kind: TrackKind::Audio,
                sample_entry: (ai == 0).then(|| audio.entry.clone()),
                keyframe: true,
                timescale: NonZeroU32::new(audio.rate).unwrap(),
                duration: audio.frames,
                composition_time_offset: None,
                data_offset: position,
                data_size: packet.size,
            };
            ai += 1;
            sample
        } else {
            continue;
        };
        for (offset, size) in ranges {
            source.seek(SeekFrom::Start(offset))?;
            let mut remaining = size;
            while remaining > 0 {
                check_cancel(&cancel)?;
                let count = remaining.min(buffer.len());
                source.read_exact(&mut buffer[..count])?;
                output.write_all(&buffer[..count])?;
                remaining -= count;
            }
        }
        mux.append_sample(&sample).map_err(invalid)?;
        position = position
            .checked_add(sample.data_size as u64)
            .ok_or_else(|| invalid("archivo demasiado grande"))?;
        on_progress(MuxProgress {
            copied_bytes: (packet.offset + packet.size as u64).min(size),
            total_bytes: size,
        });
    }
    let finalized = mux.finalize().map_err(invalid)?;
    for (offset, bytes) in finalized.offset_and_bytes_pairs() {
        output.seek(SeekFrom::Start(offset))?;
        output.write_all(bytes)?;
    }
    let mut moov = finalized.moov_box().clone();
    let movie_scale = moov.mvhd_box.timescale.get();
    for track in &mut moov.trak_boxes {
        if track.mdia_box.hdlr_box.handler_type == HdlrBox::HANDLER_TYPE_VIDE {
            set_edits(
                track,
                video.start_ns,
                video.end_ns,
                video.media_start,
                movie_scale,
            )?;
            track.mdia_box.mdhd_box.duration =
                video.media_end.max(track.mdia_box.mdhd_box.duration);
        } else if let Some(audio) = &audio {
            set_edits(
                track,
                audio.start_ns,
                audio.end_ns,
                audio.media_start,
                movie_scale,
            )?;
        }
    }
    moov.mvhd_box.duration = moov
        .trak_boxes
        .iter()
        .map(|track| track.tkhd_box.duration)
        .max()
        .unwrap_or(0);
    let duration = moov.mvhd_box.duration as f64 / f64::from(movie_scale);
    let bytes = moov.encode_to_vec().map_err(invalid)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(invalid("la tabla MP4 supera 64 MiB"));
    }
    output.seek(SeekFrom::Start(position))?;
    output.write_all(&bytes)?;
    output.set_len(position + bytes.len() as u64)?;
    output.sync_all()?;
    check_cancel(&cancel)?;
    if source.metadata()?.len() != size
        || source.metadata()?.modified().ok() != original.modified().ok()
    {
        return Err(invalid("el archivo original cambió durante la preparación"));
    }
    on_progress(MuxProgress {
        copied_bytes: size,
        total_bytes: size,
    });
    Ok(PreparedLocalVideo {
        _directory: directory,
        path,
        duration,
        width: video.width,
        height: video.height,
        warnings,
    })
}

fn selected_track(tracks: &[TrackEntry], kind: TrackType) -> Option<&TrackEntry> {
    tracks
        .iter()
        .rev()
        .filter(|t| t.track_type() == kind && t.flag_enabled())
        .max_by_key(|t| (t.flag_default(), !t.flag_commentary()))
}
fn missing_audio_warning(tracks: &[TrackEntry], has_audio: bool) -> Vec<String> {
    if !has_audio
        && tracks
            .iter()
            .any(|track| track.track_type() == TrackType::Audio && track.flag_enabled())
    {
        vec![
            "Este vídeo se reproducirá sin audio porque su formato de sonido no es compatible."
                .into(),
        ]
    } else {
        Vec::new()
    }
}
fn plain_track(track: &TrackEntry) -> Result<()> {
    if track.content_encodings().is_some_and(|v| !v.is_empty()) {
        Err(invalid(
            "la pista utiliza compresión o cifrado de contenedor",
        ))
    } else {
        Ok(())
    }
}
fn pts(packet: &Packet, scale: u64, delay: u64) -> i128 {
    i128::from(packet.pts_ticks) * i128::from(scale) - i128::from(delay)
}
fn ticks(ns: i128, rate: u32) -> Result<i64> {
    i64::try_from((ns * i128::from(rate) + NS / 2).div_euclid(NS))
        .map_err(|_| invalid("tiempo fuera de rango"))
}

fn prepare_webm(
    mut source: File,
    original: &std::fs::Metadata,
    index: &scan::Index,
    metadata: &MatroskaFile<Cursor<&Vec<u8>>>,
    cancel: &CancellationToken,
    on_progress: impl Fn(MuxProgress),
) -> Result<PreparedLocalVideo> {
    let keep: Vec<u64> = metadata
        .tracks()
        .iter()
        .filter(|track| {
            plain_track(track).is_ok()
                && match track.track_type() {
                    TrackType::Video => matches!(track.codec_id(), "V_VP8" | "V_VP9"),
                    TrackType::Audio => matches!(track.codec_id(), "A_OPUS" | "A_VORBIS"),
                    _ => false,
                }
        })
        .map(|track| track.track_number().get())
        .collect();
    let primary = selected_track(metadata.tracks(), TrackType::Video)
        .ok_or_else(|| invalid("falta pista WebM"))?;
    plain_track(primary)?;
    let warnings = missing_audio_warning(
        metadata.tracks(),
        metadata.tracks().iter().any(|track| {
            track.track_type() == TrackType::Audio
                && track.flag_enabled()
                && keep.contains(&track.track_number().get())
        }),
    );
    let redactions = if keep.len() == metadata.tracks().len() {
        Vec::new()
    } else {
        scan::webm_redactions(&mut source, index, &keep, cancel)?
    };
    let video = selected_track(metadata.tracks(), TrackType::Video)
        .and_then(TrackEntry::video)
        .ok_or_else(|| invalid("faltan dimensiones WebM"))?;
    let duration = metadata
        .info()
        .duration()
        .map(|ticks| ticks * metadata.info().timestamp_scale().get() as f64 / 1e9)
        .filter(|d| d.is_finite() && *d > 0.0)
        .ok_or_else(|| invalid("falta duración de WebM"))?;
    let width = u32::try_from(video.pixel_width().get()).map_err(invalid)?;
    let height = u32::try_from(video.pixel_height().get()).map_err(invalid)?;
    let directory = tempfile::Builder::new()
        .prefix("ytu-local-video-")
        .tempdir()?;
    let path = directory.path().join("ready.webm");
    let mut output = File::create(&path)?;
    // Equal-length Void elements remove unsupported tracks and their blocks/
    // cues. Every retained byte keeps its Segment-relative offset, including
    // CueRelativePosition and SeekHead destinations. Unchanged files are copied
    // byte-for-byte after replacing only their EBML and Segment headers.
    output.write_all(&[
        0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 1, 0x42, 0xf7, 0x81, 1, 0x42, 0xf2, 0x81,
        4, 0x42, 0xf3, 0x81, 8, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm', 0x42, 0x87, 0x81, 4,
        0x42, 0x85, 0x81, 2, 0x18, 0x53, 0x80, 0x67, 0xff,
    ])?;
    scan::copy_webm_segment(
        &mut source,
        &mut output,
        index,
        &redactions,
        cancel,
        on_progress,
    )?;
    output.sync_all()?;
    check_cancel(cancel)?;
    if source.metadata()?.len() != original.len()
        || source.metadata()?.modified().ok() != original.modified().ok()
    {
        return Err(invalid("el archivo original cambió durante la preparación"));
    }
    Ok(PreparedLocalVideo {
        _directory: directory,
        path,
        duration,
        width,
        height,
        warnings,
    })
}

fn prepare_video_track(track: &TrackEntry, packets: &[Packet], scale: u64) -> Result<Video> {
    plain_track(track)?;
    let private = track
        .codec_private()
        .ok_or_else(|| invalid("falta configuración de vídeo"))?;
    let mut boxed = Vec::with_capacity(private.len() + 8);
    boxed.extend_from_slice(
        &u32::try_from(private.len() + 8)
            .map_err(invalid)?
            .to_be_bytes(),
    );
    let (mut entry, length_size, width, height) = match track.codec_id() {
        "V_MPEG4/ISO/AVC" => {
            boxed.extend_from_slice(b"avcC");
            boxed.extend_from_slice(private);
            let (mut avcc, used) = AvccBox::decode(&boxed).map_err(invalid)?;
            if used != boxed.len() || avcc.sps_list.is_empty() || avcc.pps_list.is_empty() {
                return Err(invalid("configuración AVC incompleta"));
            }
            let sps = h264::parse_sps(&avcc.sps_list[0]).map_err(invalid)?;
            complete_avcc(&mut avcc)?;
            let length = usize::from(avcc.length_size_minus_one.get()) + 1;
            (
                SampleEntry::Avc1(Avc1Box {
                    visual: visual_fields(sps.width, sps.height),
                    avcc_box: avcc,
                    unknown_boxes: vec![],
                }),
                length,
                sps.width,
                sps.height,
            )
        }
        "V_MPEGH/ISO/HEVC" => {
            boxed.extend_from_slice(b"hvcC");
            boxed.extend_from_slice(private);
            let (mut hvcc, used) = HvccBox::decode(&boxed).map_err(invalid)?;
            if used != boxed.len() {
                return Err(invalid("configuración HEVC incompleta"));
            }
            for kind in [32, 33, 34] {
                if !hvcc
                    .nalu_arrays
                    .iter()
                    .any(|array| array.nal_unit_type.get() == kind && !array.nalus.is_empty())
                {
                    return Err(invalid("faltan VPS/SPS/PPS HEVC"));
                }
            }
            let sps = h265::parse_sps(
                &hvcc
                    .nalu_arrays
                    .iter()
                    .find(|array| array.nal_unit_type.get() == 33)
                    .unwrap()
                    .nalus[0],
            )
            .map_err(invalid)?;
            for array in &mut hvcc.nalu_arrays {
                if matches!(array.nal_unit_type.get(), 32..=34) {
                    array.array_completeness = shiguredo_mp4::Uint::new(1);
                }
            }
            let length = usize::from(hvcc.length_size_minus_one.get()) + 1;
            (
                SampleEntry::Hvc1(Hvc1Box {
                    visual: visual_fields(sps.width, sps.height),
                    hvcc_box: hvcc,
                    unknown_boxes: vec![],
                }),
                length,
                sps.width,
                sps.height,
            )
        }
        other => {
            return Err(invalid(format!(
                "códec de vídeo todavía no compatible: {other}"
            )))
        }
    };
    if ![1, 2, 4].contains(&length_size) {
        return Err(invalid("longitud NAL no compatible"));
    }
    let boxes = display_boxes(track, width, height)?;
    match &mut entry {
        SampleEntry::Avc1(avc) => avc.unknown_boxes = boxes,
        SampleEntry::Hvc1(hevc) => hevc.unknown_boxes = boxes,
        _ => unreachable!(),
    }
    let id = track.track_number().get();
    let packets: Vec<_> = packets.iter().filter(|p| p.track == id).collect();
    if packets.is_empty()
        || packets
            .iter()
            .any(|p| p.lace_count != 1 || p.invisible || p.discard_padding != 0)
    {
        return Err(invalid(
            "vídeo vacío, entrelazado por bloques o con cuadros invisibles",
        ));
    }
    let times_ns: Vec<_> = packets
        .iter()
        .map(|p| pts(p, scale, track.codec_delay().unwrap_or(0)))
        .collect();
    let base = *times_ns.iter().min().unwrap();
    let default = track.default_duration().map(|v| i128::from(v.get()));
    let mut sorted = times_ns.clone();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid("timestamps de vídeo duplicados"));
    }
    for pair in sorted.windows(2) {
        if ticks(pair[1] - base, VIDEO_SCALE)? <= ticks(pair[0] - base, VIDEO_SCALE)? {
            return Err(invalid("timestamps de vídeo demasiado próximos para MP4"));
        }
    }
    // Matroska stores presentation timestamps and packets in decode order, but
    // no DTS. Assign each picture its own presentation interval as decode
    // duration, then construct a strictly increasing decode clock in packet
    // order. CTS retains every original PTS; an edit removes decode lead-in.
    // In particular, the last PRESENTED picture must keep its explicit/default
    // duration even when B pictures follow it in decode order.
    let mut durations = Vec::with_capacity(packets.len());
    for (packet, time) in packets.iter().zip(&times_ns) {
        let position = sorted.binary_search(time).unwrap();
        let duration = packet
            .duration_ticks
            .map(|value| i128::from(value) * i128::from(scale))
            .or_else(|| sorted.get(position + 1).map(|next| next - time))
            .or(default)
            .ok_or_else(|| invalid("falta duración del último cuadro presentado"))?;
        if duration <= 0 || duration > NS * 60 {
            return Err(invalid("duración de cuadro fuera de rango"));
        }
        if let Some(next) = sorted.get(position + 1) {
            if (duration - (next - time)).abs() > i128::from(scale) + 1 {
                return Err(invalid(
                    "BlockDuration contiene un hueco o solapamiento de vídeo",
                ));
            }
        }
        durations.push(duration);
    }
    let mut decode_times = Vec::with_capacity(packets.len() + 1);
    decode_times.push(0);
    for duration in &durations {
        decode_times.push(decode_times.last().unwrap() + duration);
    }
    let shift = times_ns
        .iter()
        .enumerate()
        .map(|(i, time)| decode_times[i] - (*time - base))
        .max()
        .unwrap_or(0)
        .max(0);
    let mut times = Vec::with_capacity(packets.len());
    let mut media_end = 0;
    for (i, time) in times_ns.iter().enumerate() {
        let dts = ticks(decode_times[i], VIDEO_SCALE)?;
        let next = ticks(decode_times[i + 1], VIDEO_SCALE)?;
        let presentation = ticks(*time - base + shift, VIDEO_SCALE)?;
        let duration =
            u32::try_from(next - dts).map_err(|_| invalid("duración MP4 fuera de rango"))?;
        if duration == 0 {
            return Err(invalid("cuadros sin duración"));
        }
        let offset = presentation - dts;
        if offset < 0 || offset > i64::from(u32::MAX) {
            return Err(invalid("desfase de composición MP4 fuera de rango"));
        }
        times.push((duration, offset));
        media_end = media_end.max(
            u64::try_from(presentation + i64::from(duration))
                .map_err(|_| invalid("tiempo negativo"))?,
        );
    }
    let last = times_ns
        .iter()
        .position(|time| Some(time) == sorted.last())
        .unwrap();
    let end = times_ns[last] + durations[last];
    let start = base.max(0);
    let media_start = ticks(shift + (-base).max(0), VIDEO_SCALE)?;
    if end <= start {
        return Err(invalid("el vídeo no contiene cuadros visibles"));
    }
    Ok(Video {
        id,
        entry,
        length_size,
        width: u32::from(width),
        height: u32::from(height),
        times,
        start_ns: start,
        end_ns: end,
        media_start,
        media_end,
    })
}

fn complete_avcc(avcc: &mut AvccBox) -> Result<()> {
    let sps = h264::parse_sps(avcc.sps_list.first().ok_or_else(|| invalid("falta SPS"))?)
        .map_err(invalid)?;
    if !matches!(avcc.avc_profile_indication, 66 | 77 | 88) {
        avcc.chroma_format
            .get_or_insert(shiguredo_mp4::Uint::new(sps.chroma_format_idc));
        avcc.bit_depth_luma_minus8
            .get_or_insert(shiguredo_mp4::Uint::new(sps.bit_depth_luma_minus8));
        avcc.bit_depth_chroma_minus8
            .get_or_insert(shiguredo_mp4::Uint::new(sps.bit_depth_chroma_minus8));
    }
    Ok(())
}

fn prepare_audio_track(track: &TrackEntry, packets: &[Packet], scale: u64) -> Result<Audio> {
    plain_track(track)?;
    if track.codec_id() != "A_AAC" && !track.codec_id().starts_with("A_AAC/") {
        return Err(invalid(format!(
            "códec de audio todavía no compatible: {}",
            track.codec_id()
        )));
    }
    let private = track
        .codec_private()
        .ok_or_else(|| invalid("falta AudioSpecificConfig AAC"))?;
    let asc = parse_aac_config(private)?;
    let rate = asc.sampling_frequency.hz();
    let frames = 1024;
    let mut mp4a = aac::build_mp4a_box(
        &asc,
        &aac::Mp4aSampleEntryConfig {
            es_id: 2,
            buffer_size_db: 0,
            max_bitrate: 0,
            avg_bitrate: 0,
        },
    )
    .map_err(invalid)?;
    mp4a.esds_box
        .es
        .dec_config_descr
        .dec_specific_info
        .as_mut()
        .unwrap()
        .payload = private.to_vec();
    let entry = SampleEntry::Mp4a(mp4a);
    let id = track.track_number().get();
    let packets: Vec<_> = packets.iter().filter(|p| p.track == id).collect();
    let first = packets
        .first()
        .ok_or_else(|| invalid("la pista AAC está vacía"))?;
    let base = pts(first, scale, track.codec_delay().unwrap_or(0));
    let tolerance = i128::from(scale) + NS / i128::from(rate);
    for (i, packet) in packets.iter().enumerate() {
        if packet.invisible {
            return Err(invalid("audio marcado invisible"));
        }
        let expected = base + i as i128 * i128::from(frames) * NS / i128::from(rate);
        let actual = pts(packet, scale, track.codec_delay().unwrap_or(0))
            + i128::from(packet.lace_index) * i128::from(frames) * NS / i128::from(rate);
        if (expected - actual).abs() > tolerance {
            return Err(invalid(
                "la pista AAC contiene huecos o timestamps incompatibles",
            ));
        }
        if packet.discard_padding != 0
            && !((i == 0 && packet.discard_padding < 0)
                || (i + 1 == packets.len() && packet.discard_padding > 0))
        {
            return Err(invalid("recorte AAC en medio de la pista"));
        }
    }
    let first_trim = i128::from(first.discard_padding.min(0)).abs();
    let start = (base + first_trim).max(0);
    let end = base + packets.len() as i128 * i128::from(frames) * NS / i128::from(rate)
        - i128::from(packets.last().unwrap().discard_padding.max(0));
    if end <= start {
        return Err(invalid("el audio está recortado por completo"));
    }
    let media_start = ticks(start - base, rate)?;
    Ok(Audio {
        id,
        entry,
        rate,
        frames,
        start_ns: start,
        end_ns: end,
        media_start,
    })
}

fn parse_aac_config(private: &[u8]) -> Result<aac::AudioSpecificConfig> {
    if private.len() < 2 || private.len() > 64 {
        return Err(invalid("AudioSpecificConfig AAC no válido"));
    }
    let index = ((private[0] & 7) << 1) | (private[1] >> 7);
    let core = if index == 15 { 5 } else { 2 };
    if private.len() < core {
        return Err(invalid("AudioSpecificConfig AAC truncado"));
    }
    let parsed = aac::parse_audio_specific_config(&private[..core]).map_err(invalid)?;
    let suffix = &private[core..];
    if suffix.iter().any(|byte| *byte != 0) {
        // Encoders often append syncExtensionType=0x2b7, AOT=5, with SBR
        // explicitly disabled. Preserve those bytes without changing AAC-LC's
        // sample count. Enabled SBR/PS needs a different timing contract.
        if suffix.len() < 3
            || suffix[0] != 0x56
            || suffix[1] != 0xe5
            || suffix[2..].iter().any(|byte| *byte != 0)
        {
            return Err(invalid(
                "la extensión AAC SBR/PS requiere otro decodificador",
            ));
        }
    }
    Ok(parsed)
}

fn set_edits(
    track: &mut TrakBox,
    start: i128,
    end: i128,
    media_start: i64,
    movie_scale: u32,
) -> Result<()> {
    let start =
        u64::try_from(ticks(start, movie_scale)?).map_err(|_| invalid("inicio negativo"))?;
    let end = u64::try_from(ticks(end, movie_scale)?).map_err(|_| invalid("final negativo"))?;
    if end <= start {
        return Err(invalid("duración de presentación vacía"));
    }
    let mut entries = Vec::new();
    if start > 0 {
        entries.push(ElstEntry {
            edit_duration: start,
            media_time: -1,
            media_rate: FixedPointNumber::new(1, 0),
        });
    }
    entries.push(ElstEntry {
        edit_duration: end - start,
        media_time: media_start,
        media_rate: FixedPointNumber::new(1, 0),
    });
    track.edts_box = Some(EdtsBox {
        elst_box: Some(ElstBox { entries }),
        unknown_boxes: vec![],
    });
    track.tkhd_box.duration = end;
    Ok(())
}

fn visual_fields(width: u16, height: u16) -> VisualSampleEntryFields {
    VisualSampleEntryFields {
        data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
        width,
        height,
        horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
        vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
        frame_count: 1,
        compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
        depth: VisualSampleEntryFields::DEFAULT_DEPTH,
    }
}

struct AccessUnit {
    keyframe: bool,
    ranges: Vec<(u64, usize)>,
}

fn display_boxes(track: &TrackEntry, width: u16, height: u16) -> Result<Vec<UnknownBox>> {
    let Some(video) = track.video() else {
        return Ok(Vec::new());
    };
    if video.display_unit() == Some(matroska_demuxer::DisplayUnit::Unknown)
        || (video
            .display_unit()
            .is_some_and(|unit| unit != matroska_demuxer::DisplayUnit::Pixels)
            && (video.display_width().is_none() || video.display_height().is_none()))
    {
        return Err(invalid("dimensiones de presentación incompletas"));
    }
    let left = video.pixel_crop_left().unwrap_or(0);
    let right = video.pixel_crop_right().unwrap_or(0);
    let top = video.pixel_crop_top().unwrap_or(0);
    let bottom = video.pixel_crop_bottom().unwrap_or(0);
    let cropped_width = left
        .checked_add(right)
        .and_then(|crop| u64::from(width).checked_sub(crop))
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("recorte horizontal inválido"))?;
    let cropped_height = top
        .checked_add(bottom)
        .and_then(|crop| u64::from(height).checked_sub(crop))
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("recorte vertical inválido"))?;
    let mut boxes = Vec::new();
    if left + right + top + bottom > 0 {
        let values = [
            cropped_width as u32,
            1,
            cropped_height as u32,
            1,
            (left as i64 - right as i64) as i32 as u32,
            2,
            (top as i64 - bottom as i64) as i32 as u32,
            2,
        ];
        boxes.push(extra_box(
            *b"clap",
            &values
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect::<Vec<_>>(),
        )?);
    }
    let display_width = video
        .display_width()
        .map_or(cropped_width, |value| value.get());
    let display_height = video
        .display_height()
        .map_or(cropped_height, |value| value.get());
    let horizontal = display_width
        .checked_mul(cropped_height)
        .ok_or_else(|| invalid("relación de aspecto demasiado grande"))?;
    let vertical = display_height
        .checked_mul(cropped_width)
        .ok_or_else(|| invalid("relación de aspecto demasiado grande"))?;
    if horizontal != vertical {
        let (mut a, mut b) = (horizontal, vertical);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let horizontal = u32::try_from(horizontal / a).map_err(invalid)?;
        let vertical = u32::try_from(vertical / a).map_err(invalid)?;
        boxes.push(extra_box(
            *b"pasp",
            &[horizontal.to_be_bytes(), vertical.to_be_bytes()].concat(),
        )?);
    }
    Ok(boxes)
}

fn extra_box(kind: [u8; 4], payload: &[u8]) -> Result<UnknownBox> {
    let bytes = [
        u32::try_from(payload.len() + 8)
            .map_err(invalid)?
            .to_be_bytes()
            .as_slice(),
        kind.as_slice(),
        payload,
    ]
    .concat();
    Ok(UnknownBox::decode(&bytes).map_err(invalid)?.0)
}
fn validate_access_unit(file: &mut File, packet: &Packet, video: &Video) -> Result<AccessUnit> {
    let end = packet.offset + packet.size as u64;
    let mut offset = packet.offset;
    let mut keyframe = false;
    let mut slices = false;
    let mut count = 0;
    let mut ranges: Vec<(u64, usize)> = Vec::new();
    let hevc = matches!(video.entry, SampleEntry::Hvc1(_));
    while offset < end {
        count += 1;
        if count > 4096 || end - offset < video.length_size as u64 {
            return Err(invalid("NAL truncado"));
        }
        let start = offset;
        file.seek(SeekFrom::Start(offset))?;
        let mut length = [0; 4];
        file.read_exact(&mut length[4 - video.length_size..])?;
        let size = u64::from(u32::from_be_bytes(length));
        offset += video.length_size as u64;
        if size < if hevc { 2 } else { 1 } || size > end - offset {
            return Err(invalid("NAL fuera del frame"));
        }
        let mut header = [0; 2];
        file.read_exact(&mut header[..if hevc { 2 } else { 1 }])?;
        if header[0] & 0x80 != 0 {
            return Err(invalid("NAL inválido"));
        }
        let kind = if hevc {
            (header[0] >> 1) & 63
        } else {
            header[0] & 31
        };
        if hevc {
            if header[0] & 1 != 0 || header[1] >> 3 != 0 || header[1] & 7 == 0 {
                return Err(invalid("HEVC multicapa o cabecera no compatible"));
            }
            if kind <= 31 {
                slices = true;
            }
            if matches!(kind, 16..=21) {
                keyframe = true;
            }
        } else {
            if matches!(kind, 1 | 5) {
                slices = true;
            }
            if kind == 5 {
                keyframe = true;
            }
        }
        let parameter = if hevc {
            matches!(kind, 32..=34)
        } else {
            matches!(kind, 7 | 8)
        };
        if parameter {
            if size > 65535 {
                return Err(invalid("configuración NAL demasiado grande"));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut data = vec![0; size as usize];
            file.read_exact(&mut data)?;
            let identical =
                match &video.entry {
                    SampleEntry::Avc1(avc) => {
                        if kind == 7 {
                            avc.avcc_box.sps_list.contains(&data)
                        } else {
                            avc.avcc_box.pps_list.contains(&data)
                        }
                    }
                    SampleEntry::Hvc1(hevc) => hevc.hvcc_box.nalu_arrays.iter().any(|array| {
                        array.nal_unit_type.get() == kind && array.nalus.contains(&data)
                    }),
                    _ => false,
                };
            if !identical {
                return Err(invalid("la configuración del vídeo cambia entre cuadros"));
            }
            // avc1/hvc1 carry parameter sets out of band. Remove only exact
            // duplicates; coded pictures and SEI payloads remain byte-identical.
        } else {
            let length = video.length_size + size as usize;
            if let Some((previous, previous_size)) = ranges
                .last_mut()
                .filter(|(previous, previous_size)| *previous + *previous_size as u64 == start)
            {
                let _ = previous;
                *previous_size += length;
            } else {
                ranges.push((start, length));
            }
        }
        offset += size;
    }
    if !slices {
        return Err(invalid("un frame no contiene imagen"));
    }
    Ok(AccessUnit { keyframe, ranges })
}

#[cfg(test)]
mod tests;
