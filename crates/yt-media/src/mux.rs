//! Lossless, bounded-I/O multiplexing of separate MP4 video and audio files.

use crate::{CancellationToken, DownloadReceipt, MediaError, Result};
use shiguredo_mp4::{
    boxes::{HdlrBox, MoovBox, TrakBox},
    demux::{DemuxError, Fmp4FileDemuxer, Input, Mp4FileDemuxer, RequiredInput, TrackInfo},
    mux::{Mp4FileMuxer, Sample},
    BoxHeader, Decode, Encode, TrackKind,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const METADATA_LIMIT: usize = 64 * 1024 * 1024;
const SAMPLE_LIMIT: u64 = 2_000_000;
const COPY_BUFFER: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MuxProgress {
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

impl MuxProgress {
    pub fn fraction(self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.copied_bytes as f64 / self.total_bytes as f64).min(1.0)
        }
    }
}

fn invalid(message: impl std::fmt::Display) -> MediaError {
    MediaError::Processing(format!("No se pudo unir el MP4: {message}"))
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(MediaError::Cancelled)
    } else {
        Ok(())
    }
}

struct Source {
    file: File,
    length: u64,
    demux: Demux,
    moov: MoovBox,
    count: u64,
    next_dts: u64,
}

enum Demux {
    Regular(Mp4FileDemuxer),
    Fragmented(Fmp4FileDemuxer),
}

impl Demux {
    fn open(file: &mut File, length: u64, cancel: &CancellationToken) -> Result<(Self, MoovBox)> {
        let (regular, moov) = read_metadata(file, length, cancel)?;
        let mut demux = if moov.mvex_box.is_some() {
            Self::Fragmented(Fmp4FileDemuxer::new())
        } else {
            Self::Regular(regular)
        };
        demux.fill(file, length, cancel)?;
        Ok((demux, moov))
    }

    fn required(&self) -> Option<RequiredInput> {
        match self {
            Self::Regular(demux) => demux.required_input(),
            Self::Fragmented(demux) => demux.required_input(),
        }
    }

    fn fill(&mut self, file: &mut File, length: u64, cancel: &CancellationToken) -> Result<()> {
        let mut requests = 0;
        while let Some(required) = self.required() {
            check_cancel(cancel)?;
            requests += 1;
            if requests > 4096 {
                return Err(invalid("demasiadas cajas MP4"));
            }
            let remaining = length
                .checked_sub(required.position)
                .ok_or(MediaError::IncompleteDownload)?;
            // The fragmented demuxer learns EOF from an empty header request.
            let size = required
                .size
                .map(|n| n as u64)
                .unwrap_or(remaining)
                .min(remaining);
            if size > METADATA_LIMIT as u64 {
                return Err(invalid("un fragmento MP4 excede 64 MiB"));
            }
            let mut data = vec![0; size as usize];
            file.seek(SeekFrom::Start(required.position))?;
            file.read_exact(&mut data)?;
            let input = Input {
                position: required.position,
                data: &data,
            };
            match self {
                Self::Regular(demux) => demux.handle_input(input),
                Self::Fragmented(demux) => demux.handle_input(input),
            }
        }
        Ok(())
    }

    fn tracks(&mut self) -> std::result::Result<&[TrackInfo], DemuxError> {
        match self {
            Self::Regular(demux) => demux.tracks(),
            Self::Fragmented(demux) => demux.tracks(),
        }
    }

    fn next(
        &mut self,
    ) -> std::result::Result<Option<shiguredo_mp4::demux::Sample<'_>>, DemuxError> {
        match self {
            Self::Regular(demux) => demux.next_sample(),
            Self::Fragmented(demux) => demux.next_sample(),
        }
    }
}

struct Pending {
    sample: Sample,
    dts: u64,
}

impl Source {
    fn open(path: &Path, kind: TrackKind, cancel: &CancellationToken) -> Result<Self> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let (mut demux, moov) = Demux::open(&mut file, length, cancel)?;
        let tracks = demux.tracks().map_err(invalid)?;
        if tracks.len() != 1 || tracks[0].kind != kind || moov.trak_boxes.len() != 1 {
            return Err(invalid("se necesita una única pista de cada tipo"));
        }
        Ok(Self {
            file,
            length,
            demux,
            moov,
            count: 0,
            next_dts: 0,
        })
    }

    fn next(&mut self, cancel: &CancellationToken) -> Result<Option<Pending>> {
        check_cancel(cancel)?;
        self.demux.fill(&mut self.file, self.length, cancel)?;
        let Some(sample) = self.demux.next().map_err(invalid)? else {
            if self.count == 0 {
                return Err(invalid("la pista está vacía"));
            }
            return Ok(None);
        };
        self.count += 1;
        if self.count > SAMPLE_LIMIT || sample.timestamp != self.next_dts || sample.duration == 0 {
            return Err(invalid("la tabla de tiempos no es compatible"));
        }
        self.next_dts = sample
            .timestamp
            .checked_add(u64::from(sample.duration))
            .ok_or_else(|| invalid("desbordamiento de tiempo"))?;
        if sample
            .data_offset
            .checked_add(sample.data_size as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(MediaError::IncompleteDownload);
        }
        Ok(Some(Pending {
            dts: sample.timestamp,
            sample: Sample {
                track_kind: sample.track.kind,
                sample_entry: sample.sample_entry.cloned(),
                keyframe: sample.keyframe,
                timescale: sample.track.timescale,
                duration: sample.duration,
                composition_time_offset: sample.composition_time_offset,
                data_offset: sample.data_offset,
                data_size: sample.data_size,
            },
        }))
    }
}

fn read_metadata(
    file: &mut File,
    length: u64,
    cancel: &CancellationToken,
) -> Result<(Mp4FileDemuxer, MoovBox)> {
    let mut demux = Mp4FileDemuxer::new();
    let mut moov = None;
    let mut requests = 0;
    while let Some(required) = demux.required_input() {
        check_cancel(cancel)?;
        requests += 1;
        if requests > 4096 {
            return Err(invalid("demasiadas cajas MP4"));
        }
        let remaining = length
            .checked_sub(required.position)
            .ok_or(MediaError::IncompleteDownload)?;
        let size = required.size.map(|size| size as u64).unwrap_or(remaining);
        if size > remaining {
            return Err(MediaError::IncompleteDownload);
        }
        if size == 0 || size > METADATA_LIMIT as u64 {
            return Err(invalid("los metadatos exceden el límite de 64 MiB"));
        }
        let mut data = vec![0; size as usize];
        file.seek(SeekFrom::Start(required.position))?;
        file.read_exact(&mut data)?;
        if let Ok((header, _)) = BoxHeader::decode(&data) {
            if header.box_type == MoovBox::TYPE
                && (header.box_size.get() == 0 || header.box_size.get() <= size)
            {
                moov = Some(MoovBox::decode(&data).map_err(invalid)?.0);
            }
        }
        demux.handle_input(required.to_input(&data));
    }
    demux.tracks().map_err(invalid)?;
    Ok((
        demux,
        moov.ok_or_else(|| invalid("faltan los metadatos MP4"))?,
    ))
}

fn rescale(value: u64, from: u32, to: u32) -> Result<u64> {
    let scaled = (u128::from(value) * u128::from(to) + u128::from(from / 2)) / u128::from(from);
    u64::try_from(scaled).map_err(|_| invalid("desbordamiento de duración"))
}

fn preserve_track_metadata(
    output: &mut TrakBox,
    source: &Source,
    movie_timescale: u32,
) -> Result<()> {
    let original = &source.moov.trak_boxes[0];
    output.edts_box = original.edts_box.clone();
    if let Some(edits) = output
        .edts_box
        .as_mut()
        .and_then(|edts| edts.elst_box.as_mut())
    {
        for edit in &mut edits.entries {
            edit.edit_duration = rescale(
                edit.edit_duration,
                source.moov.mvhd_box.timescale.get(),
                movie_timescale,
            )?;
        }
    }
    if original.tkhd_box.duration != 0 {
        output.tkhd_box.duration = rescale(
            original.tkhd_box.duration,
            source.moov.mvhd_box.timescale.get(),
            movie_timescale,
        )?;
    }
    output.tkhd_box.matrix = original.tkhd_box.matrix;
    output.tkhd_box.width = original.tkhd_box.width;
    output.tkhd_box.height = original.tkhd_box.height;
    output.mdia_box.mdhd_box.language = original.mdia_box.mdhd_box.language;
    output.mdia_box.hdlr_box.name = original.mdia_box.hdlr_box.name.clone();
    Ok(())
}

/// Copy encoded video/audio samples into one MP4 without re-encoding.
///
/// Run on a blocking worker. Payload copies use a 64 KiB buffer; metadata
/// boxes and fMP4 fragments are limited to 64 MiB, each track to two million samples. Temporary
/// files are removed on error/cancellation, and an existing destination is
/// never replaced. The moov box stays at EOF so edit lists can be preserved.
pub fn mux_mp4(
    video: &Path,
    audio: &Path,
    destination: &Path,
    cancel: CancellationToken,
    on_progress: impl Fn(MuxProgress),
) -> Result<DownloadReceipt> {
    check_cancel(&cancel)?;
    if destination.try_exists()? {
        return Err(MediaError::DestinationExists);
    }
    let mut video = Source::open(video, TrackKind::Video, &cancel)?;
    let mut audio = Source::open(audio, TrackKind::Audio, &cancel)?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".ytu-mux-")
        .suffix(".part")
        .tempfile_in(parent)?;
    let mut mux = Mp4FileMuxer::new().map_err(invalid)?;
    temporary.write_all(mux.initial_boxes_bytes())?;
    let mut position = mux.initial_boxes_bytes().len() as u64;
    let mut next_video = video.next(&cancel)?;
    let mut next_audio = audio.next(&cancel)?;
    let mut buffer = vec![0; COPY_BUFFER];
    let total = video
        .length
        .checked_add(audio.length)
        .ok_or_else(|| invalid("tamaño excesivo"))?;
    let mut copied = 0;
    on_progress(MuxProgress {
        copied_bytes: 0,
        total_bytes: total,
    });
    while next_video.is_some() || next_audio.is_some() {
        let take_video = match (&next_video, &next_audio) {
            (Some(v), Some(a)) => {
                u128::from(v.dts) * u128::from(a.sample.timescale.get())
                    <= u128::from(a.dts) * u128::from(v.sample.timescale.get())
            }
            (Some(_), None) => true,
            _ => false,
        };
        let (source, pending) = if take_video {
            (&mut video, &mut next_video)
        } else {
            (&mut audio, &mut next_audio)
        };
        let mut current = pending.take().expect("selected a pending sample");
        source
            .file
            .seek(SeekFrom::Start(current.sample.data_offset))?;
        let mut remaining = current.sample.data_size;
        while remaining != 0 {
            check_cancel(&cancel)?;
            let length = remaining.min(buffer.len());
            source.file.read_exact(&mut buffer[..length])?;
            temporary.write_all(&buffer[..length])?;
            remaining -= length;
            copied += length as u64;
        }
        current.sample.data_offset = position;
        position = position
            .checked_add(current.sample.data_size as u64)
            .ok_or_else(|| invalid("tamaño excesivo"))?;
        mux.append_sample(&current.sample).map_err(invalid)?;
        *pending = source.next(&cancel)?;
        if copied % (1024 * 1024) < current.sample.data_size as u64 {
            on_progress(MuxProgress {
                copied_bytes: copied,
                total_bytes: total,
            });
        }
    }
    check_cancel(&cancel)?;
    let finalized = mux.finalize().map_err(invalid)?;
    if finalized.is_faststart_enabled() {
        return Err(invalid("disposición MP4 inesperada"));
    }
    for (offset, bytes) in finalized.offset_and_bytes_pairs() {
        temporary.seek(SeekFrom::Start(offset))?;
        temporary.write_all(bytes)?;
    }
    let mut moov = finalized.moov_box().clone();
    let timescale = moov.mvhd_box.timescale.get();
    for track in &mut moov.trak_boxes {
        let source = if track.mdia_box.hdlr_box.handler_type == HdlrBox::HANDLER_TYPE_VIDE {
            &video
        } else {
            &audio
        };
        preserve_track_metadata(track, source, timescale)?;
    }
    moov.mvhd_box.duration = moov
        .trak_boxes
        .iter()
        .map(|track| track.tkhd_box.duration)
        .max()
        .unwrap_or(0);
    let bytes = moov.encode_to_vec().map_err(invalid)?;
    temporary.seek(SeekFrom::Start(position))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().set_len(position + bytes.len() as u64)?;
    temporary.flush()?;
    let length = temporary.as_file().metadata()?.len();
    let (mut verified, _) = read_metadata(temporary.as_file_mut(), length, &cancel)?;
    let tracks = verified.tracks().map_err(invalid)?;
    if tracks.len() != 2
        || !tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video && track.duration == video.next_dts)
        || !tracks
            .iter()
            .any(|track| track.kind == TrackKind::Audio && track.duration == audio.next_dts)
    {
        return Err(invalid("la validación de las pistas falló"));
    }
    temporary.as_file().sync_all()?;
    check_cancel(&cancel)?;
    temporary.persist_noclobber(destination).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            MediaError::DestinationExists
        } else {
            MediaError::Io(error.error)
        }
    })?;
    on_progress(MuxProgress {
        copied_bytes: total,
        total_bytes: total,
    });
    Ok(DownloadReceipt {
        path: destination.to_owned(),
        bytes: length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source as _;
    use sha2::{Digest, Sha256};
    use shiguredo_mp4::{
        boxes::{
            AudioSampleEntryFields, Avc1Box, AvccBox, DopsBox, EdtsBox, ElstBox, ElstEntry,
            OpusBox, SampleEntry, VisualSampleEntryFields,
        },
        FixedPointNumber, Uint,
    };
    use std::{cell::Cell, num::NonZeroU32};

    fn entry(kind: TrackKind) -> SampleEntry {
        if kind == TrackKind::Video {
            SampleEntry::Avc1(Avc1Box {
                visual: VisualSampleEntryFields {
                    data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
                    width: 320,
                    height: 240,
                    horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
                    vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
                    frame_count: 1,
                    compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
                    depth: VisualSampleEntryFields::DEFAULT_DEPTH,
                },
                avcc_box: AvccBox {
                    avc_profile_indication: 66,
                    profile_compatibility: 0,
                    avc_level_indication: 30,
                    length_size_minus_one: Uint::new(3),
                    sps_list: vec![vec![0x67, 0x42, 0, 30]],
                    pps_list: vec![vec![0x68, 0]],
                    chroma_format: None,
                    bit_depth_luma_minus8: None,
                    bit_depth_chroma_minus8: None,
                    sps_ext_list: vec![],
                },
                unknown_boxes: vec![],
            })
        } else {
            SampleEntry::Opus(OpusBox {
                audio: AudioSampleEntryFields {
                    data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
                    channelcount: 2,
                    samplesize: 16,
                    samplerate: FixedPointNumber::new(48000, 0),
                },
                dops_box: DopsBox {
                    output_channel_count: 2,
                    pre_skip: 312,
                    input_sample_rate: 48000,
                    output_gain: 0,
                },
                unknown_boxes: vec![],
            })
        }
    }

    fn fixture(path: &Path, kind: TrackKind) {
        let mut output = File::create(path).unwrap();
        let mut mux = Mp4FileMuxer::new().unwrap();
        output.write_all(mux.initial_boxes_bytes()).unwrap();
        let mut offset = mux.initial_boxes_bytes().len() as u64;
        for index in 0..3 {
            let data = vec![index as u8 + 1; 70_000 + index];
            output.write_all(&data).unwrap();
            mux.append_sample(&Sample {
                track_kind: kind,
                sample_entry: (index == 0).then(|| entry(kind)),
                keyframe: index != 1,
                timescale: NonZeroU32::new(if kind == TrackKind::Video { 30 } else { 48000 })
                    .unwrap(),
                duration: if kind == TrackKind::Video { 1 } else { 1600 },
                composition_time_offset: (kind == TrackKind::Video).then_some([2, -1, 0][index]),
                data_offset: offset,
                data_size: data.len(),
            })
            .unwrap();
            offset += data.len() as u64;
        }
        let finalized = mux.finalize().unwrap();
        for (offset, bytes) in finalized.offset_and_bytes_pairs() {
            output.seek(SeekFrom::Start(offset)).unwrap();
            output.write_all(bytes).unwrap();
        }
        let mut moov = finalized.moov_box().clone();
        moov.trak_boxes[0].edts_box = Some(EdtsBox {
            elst_box: Some(ElstBox {
                entries: vec![ElstEntry {
                    edit_duration: moov.mvhd_box.duration,
                    media_time: if kind == TrackKind::Audio { 312 } else { 0 },
                    media_rate: FixedPointNumber::new(1, 0),
                }],
            }),
            unknown_boxes: vec![],
        });
        output.seek(SeekFrom::Start(offset)).unwrap();
        let bytes = moov.encode_to_vec().unwrap();
        output.write_all(&bytes).unwrap();
        output.set_len(offset + bytes.len() as u64).unwrap();
    }

    // Each tuple describes encoded bytes and the complete decode/presentation
    // timeline, independently of interleaving or file offsets in the output.
    type SampleEvidence = (u64, u32, Option<i64>, bool, Vec<u8>, Vec<u8>);

    fn samples(path: &Path, kind: TrackKind) -> Vec<SampleEvidence> {
        let mut file = File::open(path).unwrap();
        let length = file.metadata().unwrap().len();
        let (mut demux, _) = Demux::open(&mut file, length, &CancellationToken::new()).unwrap();
        let mut result = vec![];
        let mut configuration = vec![];
        loop {
            demux
                .fill(&mut file, length, &CancellationToken::new())
                .unwrap();
            let Some(sample) = demux.next().unwrap() else {
                break;
            };
            if sample.track.kind != kind {
                continue;
            }
            if let Some(entry) = sample.sample_entry {
                configuration = entry.encode_to_vec().unwrap();
            }
            let mut data = vec![0; sample.data_size];
            file.seek(SeekFrom::Start(sample.data_offset)).unwrap();
            file.read_exact(&mut data).unwrap();
            result.push((
                sample.timestamp,
                sample.duration,
                sample.composition_time_offset,
                sample.keyframe,
                configuration.clone(),
                Sha256::digest(&data).to_vec(),
            ));
        }
        result
    }

    #[test]
    fn preserves_encoded_samples_timing_configs_and_edit_lists() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("video.mp4");
        let audio = dir.path().join("audio.mp4");
        let target = dir.path().join("mux.mp4");
        fixture(&video, TrackKind::Video);
        fixture(&audio, TrackKind::Audio);
        let progress = Cell::new(0.0);
        let receipt = mux_mp4(&video, &audio, &target, CancellationToken::new(), |p| {
            assert!(p.fraction() >= progress.get());
            progress.set(p.fraction());
        })
        .unwrap();
        assert_eq!(receipt.bytes, target.metadata().unwrap().len());
        assert_eq!(progress.get(), 1.0);
        assert_eq!(
            samples(&video, TrackKind::Video),
            samples(&target, TrackKind::Video)
        );
        assert_eq!(
            samples(&audio, TrackKind::Audio),
            samples(&target, TrackKind::Audio)
        );
        let (_, moov) = read_metadata(
            &mut File::open(&target).unwrap(),
            receipt.bytes,
            &CancellationToken::new(),
        )
        .unwrap();
        for track in moov.trak_boxes {
            let audio = track.mdia_box.hdlr_box.handler_type == HdlrBox::HANDLER_TYPE_SOUN;
            let edit = &track.edts_box.unwrap().elst_box.unwrap().entries[0];
            assert_eq!(edit.media_time, if audio { 312 } else { 0 });
            assert_eq!(
                edit.edit_duration * 10,
                u64::from(moov.mvhd_box.timescale.get())
            );
        }
    }

    #[test]
    fn fragmented_dash_tracks_roundtrip_and_truncation_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("video.mp4");
        let audio = dir.path().join("audio.m4a");
        let target = dir.path().join("mux.mp4");
        for (path, kind) in [(&video, TrackKind::Video), (&audio, TrackKind::Audio)] {
            let mut file = File::create(path).unwrap();
            let mut mux = shiguredo_mp4::mux::Fmp4SegmentMuxer::new().unwrap();
            for index in 0..3 {
                let data = vec![index as u8; 70_000];
                let metadata = mux
                    .create_media_segment_metadata(&[Sample {
                        track_kind: kind,
                        sample_entry: (index == 0).then(|| entry(kind)),
                        keyframe: true,
                        timescale: NonZeroU32::new(48000).unwrap(),
                        duration: 1600,
                        composition_time_offset: None,
                        data_offset: 0,
                        data_size: data.len(),
                    }])
                    .unwrap();
                if index == 0 {
                    file.write_all(&mux.init_segment_bytes().unwrap()).unwrap();
                }
                file.write_all(&metadata).unwrap();
                file.write_all(&data).unwrap();
            }
        }
        mux_mp4(&video, &audio, &target, CancellationToken::new(), |_| {}).unwrap();
        assert_eq!(
            samples(&video, TrackKind::Video),
            samples(&target, TrackKind::Video)
        );
        assert_eq!(
            samples(&audio, TrackKind::Audio),
            samples(&target, TrackKind::Audio)
        );
        let broken_target = dir.path().join("broken.mp4");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&audio)
            .unwrap();
        file.set_len(file.metadata().unwrap().len() - 50).unwrap();
        assert!(mux_mp4(
            &video,
            &audio,
            &broken_target,
            CancellationToken::new(),
            |_| {}
        )
        .is_err());
        assert!(!broken_target.exists());
    }

    #[test]
    fn cancellation_and_destination_races_remove_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("video.mp4");
        let audio = dir.path().join("audio.mp4");
        let target = dir.path().join("mux.mp4");
        fixture(&video, TrackKind::Video);
        fixture(&audio, TrackKind::Audio);
        let cancel = CancellationToken::new();
        assert!(matches!(
            mux_mp4(&video, &audio, &target, cancel.clone(), |_| cancel.cancel()),
            Err(MediaError::Cancelled)
        ));
        assert!(!target.exists());
        assert!(matches!(
            mux_mp4(&video, &audio, &target, CancellationToken::new(), |_| {
                if !target.exists() {
                    std::fs::write(&target, b"keep").unwrap();
                }
            }),
            Err(MediaError::DestinationExists)
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
    }

    #[test]
    fn rejects_truncated_metadata_and_wrong_track_type() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio.mp4");
        let truncated = dir.path().join("broken.mp4");
        let target = dir.path().join("mux.mp4");
        fixture(&audio, TrackKind::Audio);
        std::fs::write(&truncated, [0, 0, 0, 32, b'f', b't', b'y', b'p']).unwrap();
        assert!(mux_mp4(
            &truncated,
            &audio,
            &target,
            CancellationToken::new(),
            |_| {}
        )
        .is_err());
        assert!(mux_mp4(&audio, &audio, &target, CancellationToken::new(), |_| {}).is_err());
        assert!(!target.exists());
    }

    #[test]
    #[ignore = "requires separately downloaded DASH video.mp4 and audio.m4a in YTU_DASH_FIXTURE_DIR"]
    fn real_dash_payload_and_decoding_roundtrip() {
        let root = std::path::PathBuf::from(
            std::env::var_os("YTU_DASH_FIXTURE_DIR").expect("fixture directory"),
        );
        let video = root.join("video.mp4");
        let audio = root.join("audio.m4a");
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mux.mp4");
        let receipt = mux_mp4(&video, &audio, &target, CancellationToken::new(), |_| {}).unwrap();
        assert_eq!(
            samples(&video, TrackKind::Video),
            samples(&target, TrackKind::Video)
        );
        assert_eq!(
            samples(&audio, TrackKind::Audio),
            samples(&target, TrackKind::Audio)
        );
        let original = crate::decode_file(&audio).unwrap();
        let merged = crate::decode_file(&target).unwrap();
        assert_eq!(original.sample_rate(), merged.sample_rate());
        assert!(original.eq(merged));
        eprintln!("DASH mux verified: {} bytes; encoded audio/video samples, timestamps and decoded audio match", receipt.bytes);
    }
}
