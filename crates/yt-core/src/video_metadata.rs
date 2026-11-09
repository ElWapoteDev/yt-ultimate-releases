//! Bounded container metadata probing; never decode or rewrite the media payload.
use matroska_demuxer::MatroskaFile;
use shiguredo_mp4::{Decode, boxes::MvhdBox};
use std::{
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::Path,
};

const MAX_METADATA: u64 = 16 * 1024 * 1024;
const MAX_ELEMENTS: usize = 100_000;

#[derive(Default, Debug)]
pub(crate) struct Properties {
    pub duration: f64,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
}

pub(crate) fn probe(path: &Path) -> Option<Properties> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let mut properties = match extension.as_str() {
        "mkv" | "webm" => matroska(&mut file, length).ok()?,
        "mp4" => Properties {
            duration: mp4_duration(&mut file, length).ok()?,
            ..Default::default()
        },
        _ => return None,
    };
    if !properties.duration.is_finite() || properties.duration < 0.0 {
        properties.duration = 0.0;
    }
    if properties.duration > 0.0 {
        properties.bitrate = (length as f64 * 8.0 / properties.duration)
            .round()
            .clamp(0.0, u32::MAX as f64) as u32;
    }
    Some(properties)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid or oversized media metadata",
    )
}

fn vint(file: &mut File, id: bool) -> io::Result<(u64, bool)> {
    let mut first = [0];
    file.read_exact(&mut first)?;
    let length = first[0].leading_zeros() as usize + 1;
    if length > if id { 4 } else { 8 } {
        return Err(invalid());
    }
    let mask = 1u8 << (8 - length);
    let mut value = if id {
        first[0] as u64
    } else {
        (first[0] & (mask - 1)) as u64
    };
    for _ in 1..length {
        let mut next = [0];
        file.read_exact(&mut next)?;
        value = (value << 8) | next[0] as u64;
    }
    Ok((value, !id && value == ((1u64 << (length * 7)) - 1)))
}

struct Element {
    id: u64,
    start: u64,
    data: u64,
    end: u64,
    unknown: bool,
}
fn element(file: &mut File, end: u64) -> io::Result<Element> {
    let start = file.stream_position()?;
    let (id, _) = vint(file, true)?;
    let (size, unknown) = vint(file, false)?;
    let data = file.stream_position()?;
    let finish = if unknown {
        end
    } else {
        data.checked_add(size).ok_or_else(invalid)?
    };
    if data > end || finish > end {
        return Err(invalid());
    }
    Ok(Element {
        id,
        start,
        data,
        end: finish,
        unknown,
    })
}

/// Only copy bounded metadata masters into a synthetic metadata-only view.
/// The third-party parser never sees original SeekHead offsets, Cues, media
/// blocks or attachments, so malicious declared sizes cannot allocate them.
fn matroska(file: &mut File, length: u64) -> io::Result<Properties> {
    let header = element(file, length)?;
    if header.id != 0x1A45DFA3 || header.unknown || header.end > 65536 {
        return Err(invalid());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut view = vec![0; header.end as usize];
    file.read_exact(&mut view)?;
    let segment = element(file, length)?;
    if segment.id != 0x18538067 {
        return Err(invalid());
    }
    view.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0xff]);
    let mut position = segment.data;
    let mut count = 0;
    while position < segment.end {
        count += 1;
        if count > MAX_ELEMENTS {
            return Err(invalid());
        }
        file.seek(SeekFrom::Start(position))?;
        let item = element(file, segment.end)?;
        if matches!(item.id, 0x1549A966 | 0x1654AE6B | 0x1254C367) {
            let size = item.end - item.start;
            if item.unknown || size > MAX_METADATA || view.len() as u64 + size > MAX_METADATA {
                return Err(invalid());
            }
            file.seek(SeekFrom::Start(item.start))?;
            let offset = view.len();
            view.resize(offset + size as usize, 0);
            file.read_exact(&mut view[offset..])?;
        }
        // Unknown-size clusters are walked by child headers; block payloads
        // still have explicit lengths and are skipped without reading them.
        position = if item.unknown && item.id == 0x1F43B675 {
            item.data
        } else if item.unknown {
            return Err(invalid());
        } else {
            item.end
        };
    }
    view.extend_from_slice(&[0x1f, 0x43, 0xb6, 0x75, 0x83, 0xe7, 0x81, 0x00]);
    let parsed = MatroskaFile::open(Cursor::new(view)).map_err(|_| invalid())?;
    let mut result = Properties {
        duration: parsed.info().duration().unwrap_or(0.0)
            * parsed.info().timestamp_scale().get() as f64
            / 1e9,
        title: parsed.info().title().unwrap_or_default().to_owned(),
        sample_rate: parsed
            .tracks()
            .iter()
            .filter_map(|track| track.audio())
            .next()
            .map(|audio| {
                audio
                    .output_sampling_frequency()
                    .unwrap_or(audio.sampling_frequency())
            })
            .filter(|rate| rate.is_finite() && *rate > 0.0 && *rate <= u32::MAX as f64)
            .unwrap_or(0.0)
            .round() as u32,
        ..Default::default()
    };
    for tag in parsed.tags().unwrap_or_default() {
        for item in tag.simple_tags() {
            let Some(value) = item
                .string()
                .filter(|s| !s.trim().is_empty() && s.len() <= 2000)
            else {
                continue;
            };
            match item.name().to_ascii_uppercase().as_str() {
                "TITLE" if result.title.is_empty() => result.title = value.into(),
                "ARTIST" | "LEAD_PERFORMER" if result.artist.is_empty() => {
                    result.artist = value.into()
                }
                "ALBUM" if result.album.is_empty() => result.album = value.into(),
                "DURATION" if result.duration == 0.0 => {
                    result.duration = parse_duration(value).unwrap_or(0.0);
                }
                _ => {}
            }
        }
    }
    Ok(result)
}

fn parse_duration(value: &str) -> Option<f64> {
    let fields = value.split(':').collect::<Vec<_>>();
    if fields.len() != 3 {
        return None;
    }
    let seconds = fields[0].parse::<f64>().ok()? * 3600.0
        + fields[1].parse::<f64>().ok()? * 60.0
        + fields[2].parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

fn mp4_box(file: &mut File, end: u64) -> io::Result<([u8; 4], u64, u64, u64)> {
    let start = file.stream_position()?;
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    let size = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let (size, header) = match size {
        0 => (end - start, 8),
        1 => {
            file.read_exact(&mut bytes[..8])?;
            (u64::from_be_bytes(bytes), 16)
        }
        size => (size as u64, 8),
    };
    let finish = start.checked_add(size).ok_or_else(invalid)?;
    if size < header || finish > end {
        return Err(invalid());
    }
    // The large-size field replaced `bytes`, so read the box type separately.
    file.seek(SeekFrom::Start(start + 4))?;
    let mut kind = [0; 4];
    file.read_exact(&mut kind)?;
    file.seek(SeekFrom::Start(start + header))?;
    Ok((kind, start, start + header, finish))
}

fn mp4_duration(file: &mut File, length: u64) -> io::Result<f64> {
    let mut position = 0;
    let mut count = 0;
    while position < length {
        count += 1;
        if count > MAX_ELEMENTS {
            return Err(invalid());
        }
        file.seek(SeekFrom::Start(position))?;
        let (kind, _, data, end) = mp4_box(file, length)?;
        if &kind == b"moov" {
            let mut child = data;
            while child < end {
                count += 1;
                if count > MAX_ELEMENTS {
                    return Err(invalid());
                }
                file.seek(SeekFrom::Start(child))?;
                let (kind, start, _, finish) = mp4_box(file, end)?;
                if &kind == b"mvhd" {
                    if finish - start > 4096 {
                        return Err(invalid());
                    }
                    let mut bytes = vec![0; (finish - start) as usize];
                    file.seek(SeekFrom::Start(start))?;
                    file.read_exact(&mut bytes)?;
                    let (header, _) = MvhdBox::decode(&bytes).map_err(|_| invalid())?;
                    if header.duration == u64::MAX || header.duration == u32::MAX as u64 {
                        return Ok(0.0);
                    }
                    return Ok(header.duration as f64 / header.timescale.get() as f64);
                }
                child = finish;
            }
        }
        position = end;
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppCore, ImportMode};
    use std::io::Write;

    fn ebml(id: u32, payload: &[u8]) -> Vec<u8> {
        let bytes = id.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap();
        let mut result = bytes[first..].to_vec();
        let length = (1..=8)
            .find(|length| (payload.len() as u64) < ((1u64 << (length * 7)) - 1))
            .unwrap();
        let size = (payload.len() as u64) | (1u64 << (length * 7));
        result.extend_from_slice(&size.to_be_bytes()[8 - length..]);
        result.extend_from_slice(payload);
        result
    }
    fn joined(items: Vec<Vec<u8>>) -> Vec<u8> {
        items.into_iter().flatten().collect()
    }
    fn mkv(doctype: &str, info_duration: bool, unknown_cluster: bool) -> Vec<u8> {
        let header = ebml(
            0x1A45DFA3,
            &joined(vec![
                ebml(0x4282, doctype.as_bytes()),
                ebml(0x4287, &[4]),
                ebml(0x4285, &[2]),
            ]),
        );
        let mut info = joined(vec![
            ebml(0x2AD7B1, &1_000_000u64.to_be_bytes()),
            ebml(0x4D80, b"fixture"),
            ebml(0x5741, b"fixture"),
            ebml(0x7BA9, "Vídeo de prueba".as_bytes()),
        ]);
        if info_duration {
            info.extend(ebml(0x4489, &12_345.5f64.to_be_bytes()));
        }
        let tracks = ebml(
            0x1654AE6B,
            &joined(vec![
                ebml(
                    0xAE,
                    &joined(vec![
                        ebml(0xD7, &[1]),
                        ebml(0x73C5, &[1]),
                        ebml(0x83, &[1]),
                        ebml(0x86, b"V_VP9"),
                        ebml(0xE0, &joined(vec![ebml(0xB0, &[64]), ebml(0xBA, &[36])])),
                    ]),
                ),
                ebml(
                    0xAE,
                    &joined(vec![
                        ebml(0xD7, &[2]),
                        ebml(0x73C5, &[2]),
                        ebml(0x83, &[2]),
                        ebml(0x86, b"A_OPUS"),
                        ebml(
                            0xE1,
                            &joined(vec![ebml(0xB5, &48_000f64.to_be_bytes()), ebml(0x9F, &[2])]),
                        ),
                    ]),
                ),
            ]),
        );
        let tags = ebml(
            0x1254C367,
            &ebml(
                0x7373,
                &joined(
                    [
                        ("ARTIST", "Artista"),
                        ("ALBUM", "Álbum"),
                        ("DURATION", "00:00:12.345500000"),
                    ]
                    .map(|(name, value)| {
                        ebml(
                            0x67C8,
                            &joined(vec![
                                ebml(0x45A3, name.as_bytes()),
                                ebml(0x4487, value.as_bytes()),
                            ]),
                        )
                    })
                    .to_vec(),
                ),
            ),
        );
        let cluster = if unknown_cluster {
            vec![0x1f, 0x43, 0xb6, 0x75, 0xff, 0xe7, 0x81, 0]
        } else {
            ebml(0x1F43B675, &ebml(0xE7, &[0]))
        };
        joined(vec![
            header,
            ebml(
                0x18538067,
                &joined(vec![ebml(0x1549A966, &info), tracks, cluster, tags]),
            ),
        ])
    }

    #[test]
    fn imports_matroska_webm_and_repairs_zero_duration_without_replacing_user_data() {
        let dir = tempfile::tempdir().unwrap();
        let core = AppCore::open(dir.path().join("profile")).unwrap();
        for (extension, explicit_duration, unknown_cluster) in
            [("mkv", true, false), ("webm", false, true)]
        {
            let path = dir.path().join(format!("original.{extension}"));
            let bytes = mkv(
                if extension == "webm" {
                    "webm"
                } else {
                    "matroska"
                },
                explicit_duration,
                unknown_cluster,
            );
            std::fs::write(&path, &bytes).unwrap();
            let track = core.import_file(&path, ImportMode::Reference).unwrap();
            assert!((track.duration - 12.3455).abs() < 1e-8, "{track:?}");
            assert_eq!(track.title, "Vídeo de prueba");
            assert_eq!(track.artist, "Artista");
            assert_eq!(track.album, "Álbum");
            let rate: u32 = core
                .connection()
                .unwrap()
                .query_row(
                    "SELECT sample_rate FROM tracks WHERE id=?1",
                    [&track.id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rate, 48000);
            let playlist = core
                .create_playlist(&format!("Playlist {extension}"))
                .unwrap();
            core.add_to_playlist(&playlist.id, &track.id).unwrap();
            core.connection().unwrap().execute("UPDATE tracks SET duration=0,title='Mi título',artist='Mi artista',album='Mi álbum' WHERE id=?1",[&track.id]).unwrap();
            core.scan_library().unwrap();
            let repaired = core.track(&track.id).unwrap();
            assert_eq!(repaired.title, "Mi título");
            assert_eq!(repaired.artist, "Mi artista");
            assert_eq!(repaired.album, "Mi álbum");
            assert!((repaired.duration - 12.3455).abs() < 1e-8);
            assert_eq!(core.playlist_tracks(&playlist.id).unwrap()[0].id, track.id);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                core.import_file(&path, ImportMode::Reference).unwrap().id,
                track.id
            );
        }
    }

    fn mp4_box_bytes(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn video_only_mp4_uses_movie_timescale_without_an_audio_track() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("video.mp4");
        let mut mvhd = vec![0; 100];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&24_625u32.to_be_bytes());
        let bytes = joined(vec![
            mp4_box_bytes(b"ftyp", b"isom\0\0\0\0isom"),
            mp4_box_bytes(b"mdat", &[1, 2, 3]),
            mp4_box_bytes(b"moov", &mp4_box_bytes(b"mvhd", &mvhd)),
        ]);
        std::fs::write(&path, &bytes).unwrap();
        let core = AppCore::open(dir.path().join("profile")).unwrap();
        let track = core.import_file(&path, ImportMode::Reference).unwrap();
        assert_eq!(track.duration, 24.625);
        assert_eq!(track.media_type, "video");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn oversized_or_truncated_metadata_is_rejected_without_reading_media_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.mkv");
        let mut file = File::create(&path).unwrap();
        file.write_all(&ebml(
            0x1A45DFA3,
            &joined(vec![
                ebml(0x4282, b"matroska"),
                ebml(0x4287, &[4]),
                ebml(0x4285, &[2]),
            ]),
        ))
        .unwrap();
        file.write_all(&[0x18, 0x53, 0x80, 0x67, 0xff, 0x15, 0x49, 0xa9, 0x66])
            .unwrap();
        let size = (1u64 << 56) | (MAX_METADATA + 1);
        file.write_all(&size.to_be_bytes()).unwrap();
        let end = file.stream_position().unwrap() + MAX_METADATA + 1;
        file.set_len(end).unwrap();
        drop(file);
        assert!(probe(&path).is_none());
        std::fs::write(&path, &mkv("matroska", true, false)[..20]).unwrap();
        assert!(probe(&path).is_none());
    }
}
