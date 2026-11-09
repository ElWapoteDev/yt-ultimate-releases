use super::*;
use shiguredo_mp4::{boxes::MoovBox, demux::Mp4FileDemuxer, BoxHeader};

#[derive(Clone)]
struct Encoded {
    id: u32,
    kind: TrackKind,
    rate: u32,
    dts: u64,
    duration: u32,
    cts: i64,
    key: bool,
    data: Vec<u8>,
}
struct Fixture {
    entries: Vec<(u32, SampleEntry)>,
    packets: Vec<Encoded>,
    moov: MoovBox,
}
fn mp4(path: &Path) -> Fixture {
    let bytes = std::fs::read(path).unwrap();
    let mut demux = Mp4FileDemuxer::new();
    let mut moov = None;
    while let Some(input) = demux.required_input() {
        let start = input.position as usize;
        let end = input.size.map_or(bytes.len(), |n| start + n);
        let data = &bytes[start..end];
        if let Ok((header, _)) = BoxHeader::decode(data) {
            if header.box_type == MoovBox::TYPE && header.box_size.get() <= data.len() as u64 {
                moov = Some(MoovBox::decode(data).unwrap().0);
            }
        }
        demux.handle_input(input.to_input(data));
    }
    let mut entries = Vec::new();
    let mut packets = Vec::new();
    while let Some(sample) = demux.next_sample().unwrap() {
        if let Some(entry) = sample.sample_entry {
            entries.push((sample.track.track_id, entry.clone()));
        }
        packets.push(Encoded {
            id: sample.track.track_id,
            kind: sample.track.kind,
            rate: sample.track.timescale.get(),
            dts: sample.timestamp,
            duration: sample.duration,
            cts: sample.composition_time_offset.unwrap_or(0),
            key: sample.keyframe,
            data: bytes
                [sample.data_offset as usize..sample.data_offset as usize + sample.data_size]
                .to_vec(),
        });
    }
    Fixture {
        entries,
        packets,
        moov: moov.unwrap(),
    }
}
fn size(value: u64) -> Vec<u8> {
    for width in 1..=8 {
        if value < (1u64 << (width * 7)) - 1 {
            let encoded = value | (1u64 << (width * 7));
            return encoded.to_be_bytes()[8 - width..].to_vec();
        }
    }
    panic!("fixture too large")
}
fn element(id: u32, data: impl AsRef<[u8]>) -> Vec<u8> {
    let id = id.to_be_bytes();
    let start = id.iter().position(|b| *b != 0).unwrap();
    let data = data.as_ref();
    [&id[start..], size(data.len() as u64).as_slice(), data].concat()
}
fn uint(id: u32, value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    element(id, &bytes[start..])
}
fn header(doc: &str) -> Vec<u8> {
    element(
        0x1a45dfa3,
        [
            uint(0x4286, 1),
            uint(0x42f7, 1),
            uint(0x42f2, 4),
            uint(0x42f3, 8),
            element(0x4282, doc),
            uint(0x4287, 4),
            uint(0x4285, 2),
        ]
        .concat(),
    )
}
fn mkv_from_mp4(fixture: &Fixture, path: &Path) {
    let scale = 1_000_000u64;
    let mut tracks = Vec::new();
    for (id, entry) in &fixture.entries {
        let packet = fixture.packets.iter().find(|p| p.id == *id).unwrap();
        let mut fields = vec![
            uint(0xd7, u64::from(*id)),
            uint(0x73c5, u64::from(*id)),
            uint(
                0x83,
                if packet.kind == TrackKind::Video {
                    1
                } else {
                    2
                },
            ),
        ];
        let delay = fixture
            .moov
            .trak_boxes
            .iter()
            .find(|t| t.tkhd_box.track_id == *id)
            .and_then(|t| {
                t.edts_box
                    .as_ref()?
                    .elst_box
                    .as_ref()?
                    .entries
                    .iter()
                    .find(|e| e.media_time >= 0)
            })
            .map_or(0, |e| {
                e.media_time as u64 * 1_000_000_000 / u64::from(packet.rate)
            });
        if delay > 0 {
            fields.push(uint(0x56aa, delay));
        }
        match entry {
            SampleEntry::Avc1(avc) => {
                fields.push(element(0x86, "V_MPEG4/ISO/AVC"));
                let mut avcc = avc.avcc_box.clone();
                complete_avcc(&mut avcc).unwrap();
                fields.push(element(0x63a2, &avcc.encode_to_vec().unwrap()[8..]));
                fields.push(uint(
                    0x23e383,
                    u64::from(packet.duration) * 1_000_000_000 / u64::from(packet.rate),
                ));
                fields.push(element(
                    0xe0,
                    [
                        uint(0xb0, u64::from(avc.visual.width)),
                        uint(0xba, u64::from(avc.visual.height)),
                    ]
                    .concat(),
                ));
            }
            SampleEntry::Mp4a(aac) => {
                fields.push(element(0x86, "A_AAC"));
                fields.push(element(
                    0x63a2,
                    &aac.esds_box
                        .es
                        .dec_config_descr
                        .dec_specific_info
                        .as_ref()
                        .unwrap()
                        .payload,
                ));
                fields.push(element(
                    0xe1,
                    [
                        element(0xb5, (f64::from(packet.rate)).to_be_bytes()),
                        uint(0x9f, u64::from(aac.audio.channelcount)),
                    ]
                    .concat(),
                ));
            }
            SampleEntry::Hvc1(hevc) => {
                fields.push(element(0x86, "V_MPEGH/ISO/HEVC"));
                fields.push(element(
                    0x63a2,
                    &hevc.hvcc_box.encode_to_vec().unwrap()[8..],
                ));
                fields.push(uint(
                    0x23e383,
                    u64::from(packet.duration) * 1_000_000_000 / u64::from(packet.rate),
                ));
                fields.push(element(
                    0xe0,
                    [
                        uint(0xb0, u64::from(hevc.visual.width)),
                        uint(0xba, u64::from(hevc.visual.height)),
                    ]
                    .concat(),
                ));
            }
            other => panic!("unsupported fixture entry {other:?}"),
        }
        tracks.push(element(0xae, fields.concat()));
    }
    let info = element(
        0x1549a966,
        [
            uint(0x2ad7b1, scale),
            element(
                0x4489,
                (fixture.moov.mvhd_box.duration as f64
                    / f64::from(fixture.moov.mvhd_box.timescale.get())
                    * 1e3)
                    .to_be_bytes(),
            ),
            element(0x4d80, "YT Ultimate fixture"),
            element(0x5741, "YT Ultimate fixture"),
        ]
        .concat(),
    );
    let mut bytes = [
        header("matroska"),
        vec![0x18, 0x53, 0x80, 0x67, 0xff],
        info,
        element(0x1654ae6b, tracks.concat()),
    ]
    .concat();
    // A cluster per packet is intentionally simple; samples remain in decode
    // order. Millisecond timestamp quantization matches common Matroska files.
    for packet in &fixture.packets {
        let pts = (i128::from(packet.dts) + i128::from(packet.cts)) * NS / i128::from(packet.rate);
        assert!(pts >= 0);
        let ts = (pts + i128::from(scale) / 2) / i128::from(scale);
        let block = [
            size(u64::from(packet.id)),
            vec![0, 0, if packet.key { 0x80 } else { 0 }],
            packet.data.clone(),
        ]
        .concat();
        let padding = if packet.kind == TrackKind::Audio
            && std::ptr::eq(
                packet,
                fixture.packets.iter().rfind(|p| p.id == packet.id).unwrap(),
            ) {
            let track = fixture
                .moov
                .trak_boxes
                .iter()
                .find(|t| t.tkhd_box.track_id == packet.id)
                .unwrap();
            let end = track
                .edts_box
                .as_ref()
                .and_then(|edts| edts.elst_box.as_ref())
                .and_then(|edits| edits.entries.iter().find(|e| e.media_time >= 0))
                .map_or(
                    (i128::from(packet.dts) + i128::from(packet.duration)) * NS
                        / i128::from(packet.rate),
                    |edit| {
                        i128::from(edit.media_time) * NS / i128::from(packet.rate)
                            + i128::from(edit.edit_duration) * NS
                                / i128::from(fixture.moov.mvhd_box.timescale.get())
                    },
                );
            ((i128::from(packet.dts) + 1024) * NS / i128::from(packet.rate) - end).max(0)
        } else {
            0
        };
        let block = if padding > 0 {
            element(
                0xa0,
                [
                    element(0xa1, block),
                    element(0x75a2, (padding as i64).to_be_bytes()),
                ]
                .concat(),
            )
        } else {
            element(0xa3, block)
        };
        bytes.extend(element(0x1f43b675, [uint(0xe7, ts as u64), block].concat()));
    }
    std::fs::write(path, bytes).unwrap();
}

#[test]
#[ignore = "Public W3C Sintel fixture; set YTU_LOCAL_VIDEO_FIXTURE to its downloaded MP4"]
fn real_b_frames_aac_timing_and_encoded_payloads() {
    let input = PathBuf::from(std::env::var("YTU_LOCAL_VIDEO_FIXTURE").unwrap());
    let source = mp4(&input);
    let mkv = input.with_file_name("sintel-avc-aac.mkv");
    mkv_from_mp4(&source, &mkv);
    let prepared = prepare_local_video(&mkv, CancellationToken::new(), |_| {}).unwrap();
    let output = mp4(prepared.path());
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let original: Vec<_> = source.packets.iter().filter(|p| p.kind == kind).collect();
        let actual: Vec<_> = output.packets.iter().filter(|p| p.kind == kind).collect();
        assert_eq!(original.len(), actual.len());
        for (a, b) in original.iter().zip(&actual) {
            assert_eq!(a.data, b.data);
            assert_eq!(a.key, b.key);
            assert!((presentation(&source, a) - presentation(&output, b)).abs() < 0.0011);
        }
        println!(
            "{kind:?}: {} samples, source rate {}, output rate {}, composition offsets {}",
            original.len(),
            original[0].rate,
            actual[0].rate,
            original.iter().filter(|p| p.cts != 0).count(),
        );
    }
    let original_audio = crate::decode_file(&input).unwrap();
    let prepared_audio = crate::decode_file(prepared.path()).unwrap();
    let mut original_audio = original_audio;
    let mut prepared_audio = prepared_audio;
    let mut count = 0;
    loop {
        match (original_audio.next(), prepared_audio.next()) {
            (Some(a), Some(b)) => {
                assert_eq!(a.to_bits(), b.to_bits());
                count += 1;
            }
            (None, None) => break,
            _ => panic!("decoded PCM duration changed"),
        }
    }
    assert!(original_audio.decoding_error().is_none() && prepared_audio.decoding_error().is_none());
    println!("Decoded PCM: {count} samples identical");
    let destination = input.with_file_name("sintel-prepared.mp4");
    std::fs::copy(prepared.path(), &destination).unwrap();
    println!(
        "Prepared {}x{} {:.6}s: {}",
        prepared.width(),
        prepared.height(),
        prepared.duration_seconds(),
        destination.display()
    );
    let mut variable = mp4(&input);
    let first_pts = variable
        .packets
        .iter()
        .filter(|p| p.kind == TrackKind::Video)
        .map(|p| p.dts as i64 + p.cts)
        .min()
        .unwrap();
    for packet in variable
        .packets
        .iter_mut()
        .filter(|p| p.kind == TrackKind::Video)
    {
        let frame = packet.dts as i64 + packet.cts - first_pts;
        packet.rate *= 2;
        packet.dts *= 2;
        packet.duration *= 2;
        packet.cts = packet.cts * 2 + (frame & 1);
    }
    for track in &mut variable.moov.trak_boxes {
        if track.mdia_box.hdlr_box.handler_type == HdlrBox::HANDLER_TYPE_VIDE {
            if let Some(edits) = track
                .edts_box
                .as_mut()
                .and_then(|edts| edts.elst_box.as_mut())
            {
                for edit in &mut edits.entries {
                    if edit.media_time >= 0 {
                        edit.media_time *= 2;
                    }
                }
            }
        }
    }
    let path = input.with_file_name("sintel-variable-b-frames.mkv");
    mkv_from_mp4(&variable, &path);
    let prepared = prepare_local_video(&path, CancellationToken::new(), |_| {}).unwrap();
    let output = mp4(prepared.path());
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let original: Vec<_> = variable.packets.iter().filter(|p| p.kind == kind).collect();
        let actual: Vec<_> = output.packets.iter().filter(|p| p.kind == kind).collect();
        assert_eq!(original.len(), actual.len());
        for (a, b) in original.iter().zip(actual) {
            assert_eq!(a.data, b.data);
            assert!((presentation(&variable, a) - presentation(&output, b)).abs() < 0.0011);
        }
    }
    assert!((prepared.duration_seconds() - 52.208333).abs() < 0.002);
    std::fs::copy(
        prepared.path(),
        input.with_file_name("sintel-variable-prepared.mp4"),
    )
    .unwrap();
    println!(
        "Variable cadence + real B frames: all presentation timestamps preserved; {:.6}s",
        prepared.duration_seconds()
    );
}

fn presentation(fixture: &Fixture, packet: &Encoded) -> f64 {
    let track = fixture
        .moov
        .trak_boxes
        .iter()
        .find(|t| t.tkhd_box.track_id == packet.id)
        .unwrap();
    let mut empty = 0.0;
    let mut media_start = 0.0;
    if let Some(edits) = track
        .edts_box
        .as_ref()
        .and_then(|edts| edts.elst_box.as_ref())
    {
        for edit in &edits.entries {
            if edit.media_time < 0 {
                empty +=
                    edit.edit_duration as f64 / f64::from(fixture.moov.mvhd_box.timescale.get());
            } else {
                media_start = edit.media_time as f64 / f64::from(packet.rate);
                break;
            }
        }
    }
    (packet.dts as f64 + packet.cts as f64) / f64::from(packet.rate) - media_start + empty
}

fn synthetic_source(path: &Path) -> Fixture {
    // Real AVC parameter sets, synthetic access units. This fixture tests
    // container indexing and timing; the ignored Sintel test validates decoding.
    let sps = vec![
        103, 100, 0, 30, 172, 52, 230, 3, 96, 247, 155, 132, 0, 0, 3, 0, 4, 0, 0, 3, 0, 192, 60,
        88, 182, 104,
    ];
    let pps = vec![104, 233, 120, 76, 178, 44];
    let avc = h264::build_avc1_box(
        &[sps],
        &[pps],
        &h264::H264SampleEntryConfig {
            length_size: h264::LengthSize::FourBytes,
        },
    )
    .unwrap();
    let mut mux = Mp4FileMuxer::new().unwrap();
    let mut bytes = mux.initial_boxes_bytes().to_vec();
    for (i, pts) in [0, 120_000, 40_000, 80_000].into_iter().enumerate() {
        let data = [0, 0, 0, 2, if i == 0 { 0x65 } else { 0x41 }, 0x88];
        let sample = Sample {
            track_kind: TrackKind::Video,
            sample_entry: (i == 0).then(|| SampleEntry::Avc1(avc.clone())),
            keyframe: i == 0,
            timescale: NonZeroU32::new(VIDEO_SCALE).unwrap(),
            duration: 40_000,
            composition_time_offset: Some(pts - i as i64 * 40_000),
            data_offset: bytes.len() as u64,
            data_size: data.len(),
        };
        bytes.extend_from_slice(&data);
        mux.append_sample(&sample).unwrap();
    }
    let finalized = mux.finalize().unwrap();
    for (offset, data) in finalized.offset_and_bytes_pairs() {
        let offset = offset as usize;
        bytes.resize(bytes.len().max(offset + data.len()), 0);
        bytes[offset..offset + data.len()].copy_from_slice(data);
    }
    std::fs::write(path, bytes).unwrap();
    mp4(path)
}

#[test]
fn b_frame_order_presentation_cancel_and_owned_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mkv");
    let fixture = synthetic_source(&directory.path().join("source.mp4"));
    mkv_from_mp4(&fixture, &input);
    let original = std::fs::read(&input).unwrap();
    let prepared = prepare_local_video(&input, CancellationToken::new(), |_| {}).unwrap();
    let output = mp4(prepared.path());
    assert_eq!(output.packets.len(), 4);
    for (a, b) in fixture.packets.iter().zip(&output.packets) {
        assert_eq!(a.data, b.data);
        assert!((presentation(&fixture, a) - presentation(&output, b)).abs() < 0.000001);
    }
    assert!((prepared.duration_seconds() - 0.16).abs() < 0.000001);
    let cached = prepared.path().to_owned();
    drop(prepared);
    assert!(!cached.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        prepare_local_video(&input, cancel, |_| {}),
        Err(MediaError::Cancelled)
    ));
    let mut bad = original;
    bad.pop();
    std::fs::write(&input, bad).unwrap();
    assert!(prepare_local_video(&input, CancellationToken::new(), |_| {}).is_err());
}

#[derive(Clone, Copy)]
struct TestElement {
    id: u64,
    start: usize,
    data: usize,
    end: usize,
}
fn test_elements(bytes: &[u8], start: usize, end: usize) -> Vec<TestElement> {
    fn integer(bytes: &[u8], position: &mut usize, id: bool) -> (u64, bool) {
        let first = bytes[*position];
        let width = first.leading_zeros() as usize + 1;
        let mut value = u64::from(if id {
            first
        } else {
            first & !(1 << (8 - width))
        });
        *position += 1;
        for _ in 1..width {
            value = (value << 8) | u64::from(bytes[*position]);
            *position += 1;
        }
        (value, !id && value == (1 << (7 * width)) - 1)
    }
    let mut out = Vec::new();
    let mut position = start;
    while position < end {
        let start = position;
        let (id, _) = integer(bytes, &mut position, true);
        let (size, unknown) = integer(bytes, &mut position, false);
        let item = TestElement {
            id,
            start,
            data: position,
            end: if unknown {
                end
            } else {
                position + size as usize
            },
        };
        assert!(item.end <= end);
        out.push(item);
        position = item.end;
    }
    out
}
fn audio_track(number: u64, codec: &str) -> Vec<u8> {
    element(
        0xae,
        [
            uint(0xd7, number),
            uint(0x73c5, number),
            uint(0x83, 2),
            element(0x86, codec),
            element(
                0xe1,
                [element(0xb5, 48_000f64.to_be_bytes()), uint(0x9f, 2)].concat(),
            ),
        ]
        .concat(),
    )
}

#[test]
fn avc_and_hevc_keep_video_when_only_audio_codec_is_unsupported() {
    let directory = tempfile::tempdir().unwrap();
    for hevc in [false, true] {
        let input = directory.path().join("video-ac3.mkv");
        let mut fixture = synthetic_source(&directory.path().join("source.mp4"));
        if hevc {
            // Configuration from the native HEVC fixture; access units remain
            // synthetic because this regression checks remuxing, not decoding.
            let hex = "010160000000b000000000005af000fcfdf8f800000b03a00001001840010c01ffff016000000300b0000003000003005a15c090a100010023420101016000000300b0000003000003005aa006c201e1cde2057b9165535010101008a2000100074401c02cbd14d9";
            let config: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let boxed = [
                &((config.len() + 8) as u32).to_be_bytes()[..],
                b"hvcC",
                &config,
            ]
            .concat();
            fixture.entries[0].1 = SampleEntry::Hvc1(Hvc1Box {
                visual: visual_fields(854, 480),
                hvcc_box: HvccBox::decode(&boxed).unwrap().0,
                unknown_boxes: vec![],
            });
            for (i, packet) in fixture.packets.iter_mut().enumerate() {
                packet.data = vec![0, 0, 0, 3, if i == 0 { 0x26 } else { 2 }, 1, 0x88];
            }
        }
        mkv_from_mp4(&fixture, &input);
        let original = std::fs::read(&input).unwrap();
        let segment = test_elements(&original, 0, original.len())[1];
        let tracks = test_elements(&original, segment.data, segment.end)
            .into_iter()
            .find(|item| item.id == 0x1654ae6b)
            .unwrap();
        let extended_tracks = element(
            0x1654ae6b,
            [
                &original[tracks.data..tracks.end],
                &audio_track(99, "A_AC3"),
            ]
            .concat(),
        );
        let bytes = [
            &original[..tracks.start],
            &extended_tracks,
            &original[tracks.end..],
            &element(
                0x1f43b675,
                [uint(0xe7, 0), element(0xa3, [0xe3, 0, 0, 0x80, 11, 12])].concat(),
            ),
        ]
        .concat();
        std::fs::write(&input, &bytes).unwrap();
        let prepared = prepare_local_video(&input, CancellationToken::new(), |_| {}).unwrap();
        assert_eq!(prepared.warnings().len(), 1);
        assert!(prepared.warnings()[0].contains("sin audio"));
        let output = mp4(prepared.path());
        assert_eq!(output.packets.len(), fixture.packets.len());
        assert!(output
            .packets
            .iter()
            .all(|packet| packet.kind == TrackKind::Video));
        for (expected, actual) in fixture.packets.iter().zip(&output.packets) {
            assert_eq!(actual.data, expected.data);
            assert!(
                (presentation(&fixture, expected) - presentation(&output, actual)).abs() < 1e-6
            );
        }
        assert!((prepared.duration_seconds() - 0.16).abs() < 1e-6);
        assert_eq!(std::fs::read(&input).unwrap(), bytes);
    }
}

#[test]
fn webm_omits_unsupported_tracks_and_cues_without_shifting_retained_offsets() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("extra-tracks.mkv");
    let crc = element(0xbf, [0, 0, 0, 0]);
    let info = element(
        0x1549a966,
        [
            uint(0x2ad7b1, 1_000_000),
            element(0x4489, 2000f64.to_be_bytes()),
            element(0x4d80, "test"),
            element(0x5741, "test"),
        ]
        .concat(),
    );
    let video = element(
        0xae,
        [
            uint(0xd7, 1),
            uint(0x73c5, 1),
            uint(0x83, 1),
            element(0x86, "V_VP9"),
            element(0xe0, [uint(0xb0, 16), uint(0xba, 16)].concat()),
        ]
        .concat(),
    );
    let subtitle = element(
        0xae,
        [
            uint(0xd7, 3),
            uint(0x73c5, 3),
            uint(0x83, 17),
            element(0x86, "S_TEXT/UTF8"),
        ]
        .concat(),
    );
    let tracks = element(
        0x1654ae6b,
        [
            crc.clone(),
            video,
            audio_track(2, "A_OPUS"),
            subtitle,
            audio_track(4, "A_AC3"),
        ]
        .concat(),
    );
    let first = element(
        0x1f43b675,
        [
            crc.clone(),
            uint(0xe7, 0),
            element(0xa3, [0x81, 0, 0, 0x80, 0xaa, 0xbb]),
            element(
                0xa0,
                [
                    element(0xa1, [0x82, 0, 10, 4, 1, 10, 11, 12, 13]),
                    uint(0x9b, 20),
                    element(0x75a2, 123i64.to_be_bytes()),
                ]
                .concat(),
            ),
            element(0xa3, [0x83, 0, 0, 0x80, 31, 32]),
            element(
                0xa0,
                [element(0xa1, [0x84, 0, 0, 0, 41, 42]), uint(0x9b, 20)].concat(),
            ),
        ]
        .concat(),
    );
    let second = [
        vec![0x1f, 0x43, 0xb6, 0x75, 0xff],
        crc.clone(),
        uint(0xe7, 1000),
        element(0xa3, [0x83, 0, 0, 0x80, 51, 52]),
        element(0xa3, [0x81, 0, 0, 0x80, 0xcc, 0xdd]),
    ]
    .concat();
    let seek = |tracks: u64, cues: u64| {
        element(
            0x114d9b74,
            [
                element(
                    0x4dbb,
                    [
                        element(0x53ab, 0x1654ae6bu32.to_be_bytes()),
                        element(0x53ac, tracks.to_be_bytes()),
                    ]
                    .concat(),
                ),
                element(
                    0x4dbb,
                    [
                        element(0x53ab, 0x1c53bb6bu32.to_be_bytes()),
                        element(0x53ac, cues.to_be_bytes()),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        )
    };
    let tracks_offset = (crc.len() + seek(0, 0).len() + info.len()) as u64;
    let first_offset = tracks_offset + tracks.len() as u64;
    let second_offset = first_offset + first.len() as u64;
    let cues_offset = second_offset + second.len() as u64;
    let cue_position = |track, cluster| {
        let data = if cluster == first_offset {
            &first
        } else {
            &second
        };
        let container = test_elements(data, 0, data.len())[0];
        let block = test_elements(data, container.data, container.end)
            .into_iter()
            .find(|item| {
                let payload = match item.id {
                    0xa3 => Some(*item),
                    0xa0 => test_elements(data, item.data, item.end)
                        .into_iter()
                        .find(|child| child.id == 0xa1),
                    _ => None,
                };
                payload.is_some_and(|payload| u64::from(data[payload.data] & 0x7f) == track)
            })
            .unwrap();
        element(
            0xb7,
            [
                uint(0xf7, track),
                element(0xf1, u64::to_be_bytes(cluster)),
                uint(0xf0, (block.start - container.data) as u64),
            ]
            .concat(),
        )
    };
    let good_position = cue_position(1, first_offset);
    let good_point = element(
        0xbb,
        [uint(0xb3, 1000), cue_position(1, second_offset)].concat(),
    );
    let cues = element(
        0x1c53bb6b,
        [
            crc.clone(),
            element(
                0xbb,
                [
                    crc.clone(),
                    uint(0xb3, 0),
                    good_position.clone(),
                    cue_position(3, first_offset),
                ]
                .concat(),
            ),
            element(
                0xbb,
                [uint(0xb3, 20), cue_position(4, first_offset)].concat(),
            ),
            good_point.clone(),
        ]
        .concat(),
    );
    let bytes = [
        header("matroska"),
        vec![0x18, 0x53, 0x80, 0x67, 0xff],
        crc,
        seek(tracks_offset, cues_offset),
        info,
        tracks,
        first,
        second,
        cues,
    ]
    .concat();
    std::fs::write(&input, &bytes).unwrap();
    let original =
        scan::index(&mut File::open(&input).unwrap(), &CancellationToken::new()).unwrap();
    let prepared = prepare_local_video(&input, CancellationToken::new(), |_| {}).unwrap();
    assert!(prepared.warnings().is_empty());
    let output = std::fs::read(prepared.path()).unwrap();
    let actual = scan::index(
        &mut File::open(prepared.path()).unwrap(),
        &CancellationToken::new(),
    )
    .unwrap();
    let metadata = MatroskaFile::open(Cursor::new(&actual.metadata)).unwrap();
    assert_eq!(
        metadata
            .tracks()
            .iter()
            .map(|t| t.track_number().get())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        output.len() as u64 - actual.segment_start,
        bytes.len() as u64 - original.segment_start
    );
    let wanted: Vec<_> = original
        .packets
        .iter()
        .filter(|p| p.track == 1 || p.track == 2)
        .collect();
    assert_eq!(actual.packets.len(), wanted.len());
    for (a, b) in wanted.iter().zip(&actual.packets) {
        assert_eq!(
            (
                a.track,
                a.pts_ticks,
                a.keyframe,
                a.lace_index,
                a.lace_count,
                a.duration_ticks,
                a.discard_padding
            ),
            (
                b.track,
                b.pts_ticks,
                b.keyframe,
                b.lace_index,
                b.lace_count,
                b.duration_ticks,
                b.discard_padding
            )
        );
        assert_eq!(
            a.offset - original.segment_start,
            b.offset - actual.segment_start
        );
        assert_eq!(
            &bytes[a.offset as usize..a.offset as usize + a.size],
            &output[b.offset as usize..b.offset as usize + b.size]
        );
    }
    let segment = actual.segment_start as usize;
    assert_eq!(
        &output[segment + tracks_offset as usize..][..4],
        &0x1654ae6bu32.to_be_bytes()
    );
    assert_eq!(
        &output[segment + cues_offset as usize..][..4],
        &0x1c53bb6bu32.to_be_bytes()
    );
    assert_eq!(
        &output[segment + first_offset as usize..][..4],
        &0x1f43b675u32.to_be_bytes()
    );
    assert_eq!(
        &output[segment + second_offset as usize..][..4],
        &0x1f43b675u32.to_be_bytes()
    );
    let out_cues = test_elements(&output, segment + cues_offset as usize, output.len())[0];
    let points = test_elements(&output, out_cues.data, out_cues.end);
    assert!(!points.iter().any(|p| p.id == 0xbf));
    let points: Vec<_> = points.into_iter().filter(|p| p.id == 0xbb).collect();
    assert_eq!(points.len(), 2);
    let positions: Vec<_> = test_elements(&output, points[0].data, points[0].end)
        .into_iter()
        .filter(|p| p.id == 0xb7)
        .collect();
    assert_eq!(positions.len(), 1);
    assert_eq!(&output[positions[0].start..positions[0].end], good_position);
    assert_eq!(&output[points[1].start..points[1].end], good_point);
    for point in points {
        for position in test_elements(&output, point.data, point.end)
            .into_iter()
            .filter(|item| item.id == 0xb7)
        {
            let fields = test_elements(&output, position.data, position.end);
            let value = |id| {
                let field = fields.iter().find(|field| field.id == id).unwrap();
                output[field.data..field.end]
                    .iter()
                    .fold(0usize, |value, byte| (value << 8) | usize::from(*byte))
            };
            let cluster = test_elements(&output, segment + value(0xf1), output.len())[0];
            let block = test_elements(&output, cluster.data + value(0xf0), output.len())[0];
            assert_eq!(block.id, 0xa3);
            assert_eq!(output[block.data], 0x81);
        }
    }
    let cancel = CancellationToken::new();
    assert!(matches!(
        prepare_local_video(&input, cancel.clone(), |_| cancel.cancel()),
        Err(MediaError::Cancelled)
    ));
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}

#[test]
fn variable_cadence_b_frames_keep_last_presented_duration() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("variable.mkv");
    let mut fixture = synthetic_source(&directory.path().join("source.mp4"));
    // Decode order I, P, B, B: last picture presented is the SECOND packet.
    for (packet, pts) in fixture
        .packets
        .iter_mut()
        .zip([0, 180_000, 40_000, 100_000])
    {
        packet.cts = pts - packet.dts as i64;
    }
    mkv_from_mp4(&fixture, &input);
    let prepared = prepare_local_video(&input, CancellationToken::new(), |_| {}).unwrap();
    let output = mp4(prepared.path());
    for (a, b) in fixture.packets.iter().zip(&output.packets) {
        assert_eq!(a.data, b.data);
        assert!((presentation(&fixture, a) - presentation(&output, b)).abs() < 0.000001);
    }
    assert_eq!(output.packets[1].duration, 40_000);
    assert!((prepared.duration_seconds() - 0.22).abs() < 0.000001);
    assert!(output.packets.iter().all(|p| p.cts >= 0));
}

#[test]
fn repeated_parameter_sets_move_to_config_but_changes_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("parameters.mkv");
    let mut fixture = synthetic_source(&directory.path().join("source.mp4"));
    let SampleEntry::Avc1(entry) = &fixture.entries[0].1 else {
        panic!("expected AVC")
    };
    let sps = &entry.avcc_box.sps_list[0];
    let picture = fixture.packets[0].data.clone();
    fixture.packets[0].data = [
        (sps.len() as u32).to_be_bytes().as_slice(),
        sps.as_slice(),
        picture.as_slice(),
    ]
    .concat();
    mkv_from_mp4(&fixture, &path);
    let prepared = prepare_local_video(&path, CancellationToken::new(), |_| {}).unwrap();
    assert_eq!(mp4(prepared.path()).packets[0].data, picture);
    fixture.packets[0].data[5] ^= 1;
    mkv_from_mp4(&fixture, &path);
    assert!(prepare_local_video(&path, CancellationToken::new(), |_| {})
        .unwrap_err()
        .to_string()
        .contains("configuración"));
}

#[test]
fn display_crop_and_aspect_are_preserved_without_modifying_coded_dimensions() {
    let track = element(
        0xae,
        [
            uint(0xd7, 1),
            uint(0x73c5, 1),
            uint(0x83, 1),
            element(0x86, "V_MPEG4/ISO/AVC"),
            element(
                0xe0,
                [
                    uint(0xb0, 720),
                    uint(0xba, 480),
                    uint(0x54cc, 20),
                    uint(0x54dd, 10),
                    uint(0x54bb, 8),
                    uint(0x54aa, 2),
                    uint(0x54b0, 640),
                    uint(0x54ba, 360),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let bytes = [
        header("matroska"),
        vec![0x18, 0x53, 0x80, 0x67, 0xff],
        element(
            0x1549a966,
            [
                uint(0x2ad7b1, 1_000_000),
                element(0x4d80, "test"),
                element(0x5741, "test"),
            ]
            .concat(),
        ),
        element(0x1654ae6b, track),
        element(0x1f43b675, []),
    ]
    .concat();
    let metadata = MatroskaFile::open(Cursor::new(bytes)).unwrap();
    let boxes = display_boxes(&metadata.tracks()[0], 720, 480).unwrap();
    let crop = boxes
        .iter()
        .find(|b| b.box_type == shiguredo_mp4::BoxType::Normal(*b"clap"))
        .unwrap();
    let values: Vec<_> = crop
        .payload
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_be_bytes(*bytes))
        .collect();
    assert_eq!(values, [690, 1, 470, 1, 10, 2, 6, 2]);
    let aspect = boxes
        .iter()
        .find(|b| b.box_type == shiguredo_mp4::BoxType::Normal(*b"pasp"))
        .unwrap();
    let horizontal = u32::from_be_bytes(aspect.payload[..4].try_into().unwrap());
    let vertical = u32::from_be_bytes(aspect.payload[4..].try_into().unwrap());
    assert_eq!(
        u64::from(horizontal) * 360 * 690,
        u64::from(vertical) * 640 * 470
    );
}

fn raw_blocks(blocks: Vec<Vec<u8>>) -> Vec<u8> {
    [
        header("matroska"),
        vec![0x18, 0x53, 0x80, 0x67, 0xff],
        element(
            0x1549a966,
            [
                uint(0x2ad7b1, 1_000_000),
                element(0x4d80, "test"),
                element(0x5741, "test"),
            ]
            .concat(),
        ),
        element(0x1654ae6b, []),
        element(0x1f43b675, [uint(0xe7, 100), blocks.concat()].concat()),
    ]
    .concat()
}

#[test]
fn index_retains_all_lacing_sizes_and_block_padding() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("laces.mkv");
    let blocks = vec![
        element(
            0xa3,
            [vec![0x81, 0, 0, 0x82, 2, 2, 3], b"aabbbcccc".to_vec()].concat(),
        ),
        element(
            0xa3,
            [vec![0x81, 0, 0, 0x84, 2], b"aabbcc".to_vec()].concat(),
        ),
        element(
            0xa3,
            [vec![0x81, 0, 0, 0x86, 2, 0x82, 0xc0], b"aabbbcccc".to_vec()].concat(),
        ),
        element(
            0xa0,
            [
                element(0xa1, [0x81, 0, 0, 0, 0x11]),
                uint(0x9b, 20),
                element(0x75a2, 100i64.to_be_bytes()),
            ]
            .concat(),
        ),
    ];
    std::fs::write(&path, raw_blocks(blocks)).unwrap();
    let index = scan::index(&mut File::open(&path).unwrap(), &CancellationToken::new()).unwrap();
    assert_eq!(
        index.packets.iter().map(|p| p.size).collect::<Vec<_>>(),
        vec![2, 3, 4, 2, 2, 2, 2, 3, 4, 1]
    );
    assert_eq!(index.packets[8].lace_index, 2);
    assert_eq!(index.packets[9].discard_padding, 100);
    assert_eq!(index.packets[9].duration_ticks, Some(20));
    std::fs::write(
        &path,
        raw_blocks(vec![element(0xa3, [0x81, 0, 0, 0x86, 2, 0xff])]),
    )
    .unwrap();
    assert!(scan::index(&mut File::open(&path).unwrap(), &CancellationToken::new()).is_err());
}

#[test]
fn aac_lacing_codec_delay_and_final_padding_have_sample_exact_edits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("aac.mkv");
    let track = element(
        0xae,
        [
            uint(0xd7, 1),
            uint(0x73c5, 1),
            uint(0x83, 2),
            element(0x86, "A_AAC"),
            element(0x63a2, [0x11, 0x90, 0x56, 0xe5, 0]),
            uint(0x56aa, 20_000_000),
            element(
                0xe1,
                [element(0xb5, 48_000f64.to_be_bytes()), uint(0x9f, 2)].concat(),
            ),
        ]
        .concat(),
    );
    let bytes = [
        header("matroska"),
        vec![0x18, 0x53, 0x80, 0x67, 0xff],
        element(
            0x1549a966,
            [
                uint(0x2ad7b1, 1_000_000),
                element(0x4d80, "test"),
                element(0x5741, "test"),
            ]
            .concat(),
        ),
        element(0x1654ae6b, track),
        element(
            0x1f43b675,
            [
                uint(0xe7, 0),
                element(
                    0xa0,
                    [
                        element(0xa1, [0x81, 0, 0, 4, 2, 1, 2, 3]),
                        uint(0x9b, 64),
                        element(0x75a2, 10_000_000i64.to_be_bytes()),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        ),
    ]
    .concat();
    std::fs::write(&path, bytes).unwrap();
    let index = scan::index(&mut File::open(&path).unwrap(), &CancellationToken::new()).unwrap();
    let metadata = MatroskaFile::open(Cursor::new(index.metadata)).unwrap();
    let audio = prepare_audio_track(&metadata.tracks()[0], &index.packets, 1_000_000).unwrap();
    assert_eq!(audio.start_ns, 0);
    assert_eq!(audio.end_ns, 34_000_000);
    assert_eq!(audio.media_start, 960);
    assert_eq!(audio.frames, 1024);
    let SampleEntry::Mp4a(entry) = audio.entry else {
        panic!("expected AAC")
    };
    assert_eq!(
        entry
            .esds_box
            .es
            .dec_config_descr
            .dec_specific_info
            .unwrap()
            .payload,
        [0x11, 0x90, 0x56, 0xe5, 0]
    );
    assert!(parse_aac_config(&[0x11, 0x90, 0x56, 0xe5, 0x80]).is_err());
}

#[test]
#[ignore = "Public W3C WebM fixture; set YTU_LOCAL_WEBM_FIXTURE"]
fn real_webm_rewrap_preserves_entire_segment() {
    let input = PathBuf::from(std::env::var("YTU_LOCAL_WEBM_FIXTURE").unwrap());
    let mut file = File::open(&input).unwrap();
    let original = scan::index(&mut file, &CancellationToken::new()).unwrap();
    let mkv = input.with_file_name("sintel-vp8-vorbis.mkv");
    let mut source = header("matroska");
    source.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0xff]);
    file.seek(SeekFrom::Start(original.segment_start)).unwrap();
    file.read_to_end(&mut source).unwrap();
    std::fs::write(&mkv, &source).unwrap();
    let prepared = prepare_local_video(&mkv, CancellationToken::new(), |_| {}).unwrap();
    let mut output = File::open(prepared.path()).unwrap();
    let index = scan::index(&mut output, &CancellationToken::new()).unwrap();
    output.seek(SeekFrom::Start(index.segment_start)).unwrap();
    let mut actual = Vec::new();
    output.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, &source[header("matroska").len() + 5..]);
    assert_eq!(index.packets.len(), original.packets.len());
    println!(
        "WebM {}x{}, {:.3}s, {} packets unchanged",
        prepared.width(),
        prepared.height(),
        prepared.duration_seconds(),
        index.packets.len()
    );
}

#[test]
#[ignore = "Native HEVC fixture; set YTU_LOCAL_HEVC_FIXTURE"]
fn real_hevc_aac_preserves_pictures_timing_and_trim() {
    let input = PathBuf::from(std::env::var("YTU_LOCAL_HEVC_FIXTURE").unwrap());
    let source = mp4(&input);
    let mkv = input.with_file_name("sintel-hevc-aac.mkv");
    mkv_from_mp4(&source, &mkv);
    let prepared = prepare_local_video(&mkv, CancellationToken::new(), |_| {}).unwrap();
    let output = mp4(prepared.path());
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let original: Vec<_> = source.packets.iter().filter(|p| p.kind == kind).collect();
        let actual: Vec<_> = output.packets.iter().filter(|p| p.kind == kind).collect();
        assert_eq!(original.len(), actual.len());
        for (a, b) in original.iter().zip(actual) {
            assert_eq!(a.data, b.data);
            assert!((presentation(&source, a) - presentation(&output, b)).abs() < 0.0011);
        }
        println!(
            "HEVC fixture {kind:?}: {} identical payloads",
            original.len()
        );
    }
    let expected =
        source.moov.mvhd_box.duration as f64 / f64::from(source.moov.mvhd_box.timescale.get());
    assert!((prepared.duration_seconds() - expected).abs() < 0.002);
    std::fs::copy(
        prepared.path(),
        input.with_file_name("sintel-hevc-prepared.mp4"),
    )
    .unwrap();
    println!(
        "HEVC prepared {}x{} {:.6}s",
        prepared.width(),
        prepared.height(),
        prepared.duration_seconds()
    );
}
