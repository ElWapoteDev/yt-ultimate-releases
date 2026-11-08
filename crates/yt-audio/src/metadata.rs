use crate::{check, codec, AudioError, AudioFormat, MediaMetadata, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use lofty::{
    config::WriteOptions,
    error::ErrorKind,
    file::TaggedFileExt,
    id3::v2::Id3v2Tag,
    ogg::VorbisComments,
    picture::{MimeType, Picture, PictureType},
    tag::{Accessor, ItemKey, ItemValue, Tag, TagExt, TagItem, TagType},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use yt_media::CancellationToken;

const ARCHIVE_PREFIX: &str = "YTULTIMATE_SOURCE_TAG_";
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;

/// Copy complete source tags/pictures onto an already generated result. The
/// source is never modified. Tag writing happens on a private copy and replaces
/// the result only after it succeeds; use this for processed variants/stems.
pub fn copy_metadata(
    input: &Path,
    output: &Path,
    metadata: &MediaMetadata,
    cancel: &CancellationToken,
) -> Result<()> {
    check(cancel)?;
    if input.canonicalize()? == output.canonicalize()? {
        return Err(AudioError::Codec(
            "El original y el resultado deben ser archivos distintos".into(),
        ));
    }
    if std::fs::symlink_metadata(output)?.file_type().is_symlink() {
        return Err(AudioError::Codec(
            "El resultado no puede ser un enlace simbólico".into(),
        ));
    }
    let format = match output
        .extension()
        .and_then(|v| v.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp3") => AudioFormat::Mp3(192),
        Some("wav") => AudioFormat::Wav,
        Some("flac") => AudioFormat::Flac,
        Some("m4a") => AudioFormat::M4a(192),
        Some("aac") => AudioFormat::Aac(192),
        Some("ogg") => AudioFormat::Ogg(192),
        Some("opus") => AudioFormat::Opus(192),
        _ => {
            return Err(AudioError::Codec(
                "El resultado debe ser MP3, WAV, FLAC, M4A, AAC, OGG u OPUS".into(),
            ))
        }
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".ytu-tags-")
        .tempfile_in(parent)?;
    let mut reader = File::open(output)?;
    let mut buffer = [0; 128 * 1024];
    loop {
        check(cancel)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        temporary.write_all(&buffer[..count])?;
    }
    drop(reader);
    // The output may already have tags (e.g. a previously processed result).
    // Replace its metadata, rather than retaining unrelated old ID3v1/APE tags.
    temporary.seek(SeekFrom::Start(0))?;
    let existing = lofty::read_from(temporary.as_file_mut()).map_err(codec)?;
    let kinds = existing
        .tags()
        .iter()
        .map(Tag::tag_type)
        .collect::<Vec<_>>();
    for kind in kinds {
        if kind == TagType::Mp4Ilst {
            continue;
        }
        check(cancel)?;
        temporary.seek(SeekFrom::Start(0))?;
        kind.remove_from(temporary.as_file_mut()).map_err(codec)?;
    }
    write_metadata(input, temporary.as_file_mut(), format, metadata, cancel)?;
    temporary.as_file().sync_all()?;
    check(cancel)?;
    temporary
        .persist(output)
        .map_err(|error| AudioError::Io(error.error))?;
    Ok(())
}

pub(crate) fn write_metadata(
    input: &Path,
    destination: &mut File,
    format: AudioFormat,
    overrides: &MediaMetadata,
    cancel: &CancellationToken,
) -> Result<()> {
    check(cancel)?;
    let kind = match format {
        AudioFormat::Flac | AudioFormat::Ogg(_) | AudioFormat::Opus(_) => TagType::VorbisComments,
        AudioFormat::M4a(_) => TagType::Mp4Ilst,
        _ => TagType::Id3v2,
    };
    let tagged = if overrides.preserve_source_tags {
        match lofty::read_from_path(input) {
            Ok(file) => Some(file),
            // Some supported decoders (e.g. WebM) have no Lofty tag reader.
            Err(error) if matches!(error.kind(), ErrorKind::UnknownFormat) => None,
            Err(error) => {
                return Err(codec(format!(
                    "No se pudieron leer las etiquetas originales: {error}"
                )))
            }
        }
    } else {
        None
    };
    let mut tag = tagged
        .as_ref()
        .and_then(|file| file.tag(kind))
        .cloned()
        .unwrap_or_else(|| Tag::new(kind));
    if let Some(file) = &tagged {
        let primary = file.primary_tag().or_else(|| file.first_tag());
        let mut tags = primary.into_iter().collect::<Vec<_>>();
        tags.extend(
            file.tags().iter().filter(|candidate| {
                !primary.is_some_and(|primary| std::ptr::eq(primary, *candidate))
            }),
        );
        for source in tags {
            check(cancel)?;
            if source.tag_type() != kind {
                archive_nonportable(source, &mut tag)?;
            }
            let missing = source
                .items()
                .filter(|item| tag.get(item.key()).is_none())
                .cloned()
                .collect::<Vec<_>>();
            for item in missing {
                if matches!(item.value(), ItemValue::Binary(_)) && kind != TagType::Id3v2 {
                    continue;
                }
                if !tag.push(item.clone()) {
                    if let ItemValue::Text(_) | ItemValue::Locator(_) = item.value() {
                        let key = item
                            .key()
                            .map_key(source.tag_type(), true)
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("{:?}", item.key()));
                        if !key.contains('=') && key.bytes().all(|b| (32..=125).contains(&b)) {
                            tag.push_unchecked(TagItem::new(
                                ItemKey::Unknown(key),
                                item.value().clone(),
                            ));
                        }
                    }
                }
            }
            for picture in source.pictures() {
                if !tag.pictures().iter().any(|existing| existing == picture) {
                    tag.push_picture(picture.clone());
                }
            }
        }
    }
    if let Some(title) = &overrides.title {
        tag.set_title(title.clone());
    }
    if let Some(artist) = &overrides.artist {
        tag.set_artist(artist.clone());
    }
    if let Some(album) = &overrides.album {
        tag.set_album(album.clone());
    }
    if let Some(url) = &overrides.source_url {
        tag.remove_key(&ItemKey::AudioSourceUrl);
        if !url.is_empty() {
            let (key, value) = match kind {
                TagType::Mp4Ilst => (
                    ItemKey::Unknown("----:com.ytultimate:SOURCE_URL".into()),
                    ItemValue::Text(url.clone()),
                ),
                TagType::VorbisComments => (
                    ItemKey::Unknown("SOURCE_URL".into()),
                    ItemValue::Text(url.clone()),
                ),
                _ => (ItemKey::AudioSourceUrl, ItemValue::Locator(url.clone())),
            };
            tag.insert_unchecked(TagItem::new(key, value));
        }
    }
    if let Some(path) = &overrides.artwork_path {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(yt_core::MAX_ARTWORK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        let extension = yt_core::validate_artwork(&bytes).map_err(codec)?;
        let picture = Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::from_str(match extension {
                "jpg" => "image/jpeg",
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "bmp" => "image/bmp",
                _ => unreachable!("validated artwork format"),
            })),
            None,
            bytes.clone(),
        );
        if !tag.pictures().iter().any(|existing| {
            existing.pic_type() == PictureType::CoverFront && existing.data() == bytes
        }) {
            tag.remove_picture_type(PictureType::CoverFront);
            tag.push_picture(picture);
        }
    }
    if kind == TagType::Mp4Ilst {
        // ILST unknown text belongs in a freeform atom. Bare ID3/Vorbis names
        // are otherwise silently dropped by the container's conversion.
        let items = tag.items().cloned().collect::<Vec<_>>();
        for item in items {
            let (key, value) = match (item.key(), item.value()) {
                (ItemKey::Unknown(name), value)
                    if !name.starts_with("----:") && name.chars().count() != 4 =>
                {
                    (
                        ItemKey::Unknown(format!("----:com.ytultimate:{name}")),
                        value.clone(),
                    )
                }
                (key, ItemValue::Locator(value)) => (key.clone(), ItemValue::Text(value.clone())),
                _ => continue,
            };
            tag.remove_key(item.key());
            tag.push_unchecked(TagItem::new(key, value));
        }
    }
    let bytes = tag
        .pictures()
        .iter()
        .map(|picture| picture.data().len())
        .sum::<usize>()
        + tag
            .items()
            .map(|item| match item.value() {
                ItemValue::Text(value) | ItemValue::Locator(value) => value.len(),
                ItemValue::Binary(value) => value.len(),
            })
            .sum::<usize>();
    if bytes > MAX_METADATA_BYTES {
        return Err(codec(
            "Las etiquetas y carátulas superan el límite de 64 MiB",
        ));
    }
    check(cancel)?;
    if tag.is_empty() && !tag.has_format_specific_items() && kind != TagType::Mp4Ilst {
        return Ok(());
    }
    destination.seek(SeekFrom::Start(0))?;
    if format == AudioFormat::Flac {
        ensure_flac_metadata_block(destination, cancel)?;
        destination.seek(SeekFrom::Start(0))?;
    }
    let options = WriteOptions::default().respect_read_only(false);
    match kind {
        TagType::Id3v2 => Id3v2Tag::from(tag)
            .save_to(destination, options)
            .map_err(codec)?,
        TagType::VorbisComments => VorbisComments::from(tag)
            .save_to(destination, options)
            .map_err(codec)?,
        TagType::Mp4Ilst => write_mp4_metadata(destination, lofty::mp4::Ilst::from(tag), cancel)?,
        _ => unreachable!("output metadata kind is fixed by the audio format"),
    }
    check(cancel)
}

// Lofty's ILST writer loads the whole media file. Keep audio on disk and move
// it in bounded chunks, placing metadata first for immediate player discovery.
fn write_mp4_metadata(
    file: &mut File,
    tag: lofty::mp4::Ilst,
    cancel: &CancellationToken,
) -> Result<()> {
    use shiguredo_mp4::{
        boxes::{Co64Box, MoovBox, UnknownBox},
        BoxType, Decode, Either, Encode,
    };
    let length = file.metadata()?.len();
    let mut position = 0u64;
    let mut location = None;
    let mut insertion = None;
    while position < length {
        check(cancel)?;
        file.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let short = u32::from_be_bytes(header[..4].try_into().unwrap());
        let size = match short {
            0 => return Err(codec("M4A con tamaño de caja indeterminado")),
            1 => {
                let mut bytes = [0; 8];
                file.read_exact(&mut bytes)?;
                u64::from_be_bytes(bytes)
            }
            _ => short as u64,
        };
        if size < 8 || position.checked_add(size).is_none_or(|end| end > length) {
            return Err(codec("Caja M4A inválida"));
        }
        if &header[4..] == b"moov" && location.replace((position, size)).is_some() {
            return Err(codec("M4A con múltiples índices"));
        }
        if position == 0 && &header[4..] == b"ftyp" {
            insertion = Some(size);
        }
        position += size;
    }
    let (offset, size) = location.ok_or_else(|| codec("M4A sin índice"))?;
    let insertion = insertion.ok_or_else(|| codec("M4A sin cabecera"))?;
    if size > MAX_METADATA_BYTES as u64 {
        return Err(codec("El índice M4A supera 64 MiB"));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut original = vec![0u8; size as usize];
    file.read_exact(&mut original)?;
    let (mut moov, _) = MoovBox::decode(&original).map_err(codec)?;
    let mut ilst = Vec::new();
    tag.dump_to(&mut ilst, WriteOptions::default())
        .map_err(codec)?;
    let mut handler = vec![0u8; 8];
    handler.extend(b"mdirappl");
    handler.extend([0; 9]);
    let mut meta = vec![0; 4];
    meta.extend(mp4_box(b"hdlr", &handler)?);
    meta.extend(ilst);
    let meta = mp4_box(b"meta", &meta)?;
    let mut user_data = Vec::new();
    for old in moov
        .unknown_boxes
        .iter()
        .filter(|b| b.box_type == BoxType::Normal(*b"udta"))
    {
        let mut cursor = 0;
        while cursor < old.payload.len() {
            check(cancel)?;
            let (child, count) = UnknownBox::decode(&old.payload[cursor..]).map_err(codec)?;
            if count == 0 {
                return Err(codec("Metadatos M4A inválidos"));
            }
            if child.box_type != BoxType::Normal(*b"meta") {
                user_data.extend_from_slice(&old.payload[cursor..cursor + count]);
            }
            cursor += count;
        }
    }
    user_data.extend(meta);
    let (udta, _) = UnknownBox::decode(&mp4_box(b"udta", &user_data)?).map_err(codec)?;
    moov.unknown_boxes
        .retain(|b| b.box_type != BoxType::Normal(*b"udta"));
    moov.unknown_boxes.push(udta);
    for track in &mut moov.trak_boxes {
        let table = &mut track.mdia_box.minf_box.stbl_box.stco_or_co64_box;
        if let Either::A(value) = table {
            *table = Either::B(Co64Box {
                chunk_offsets: value.chunk_offsets.iter().map(|v| *v as u64).collect(),
            });
        }
    }
    let metadata_size = moov.encode_to_vec().map_err(codec)?.len() as u64;
    for track in &mut moov.trak_boxes {
        if let Either::B(table) = &mut track.mdia_box.minf_box.stbl_box.stco_or_co64_box {
            for position in &mut table.chunk_offsets {
                if *position < insertion {
                    return Err(codec("Offset M4A inválido"));
                }
                *position = position
                    .checked_add(metadata_size)
                    .ok_or_else(|| codec("M4A demasiado grande"))?;
            }
        }
    }
    let encoded = moov.encode_to_vec().map_err(codec)?;
    if encoded.len() > MAX_METADATA_BYTES {
        return Err(codec("Los metadatos M4A superan 64 MiB"));
    }
    check(cancel)?;
    let media_end = if offset + size == length {
        offset
    } else {
        file.seek(SeekFrom::Start(offset + 4))?;
        file.write_all(b"free")?;
        length
    };
    let mut end = media_end;
    let mut buffer = vec![0u8; 128 * 1024];
    while end > insertion {
        check(cancel)?;
        let start = end.saturating_sub(buffer.len() as u64).max(insertion);
        let count = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        file.seek(SeekFrom::Start(start + metadata_size))?;
        file.write_all(&buffer[..count])?;
        end = start;
    }
    file.seek(SeekFrom::Start(insertion))?;
    file.write_all(&encoded)?;
    file.set_len(media_end + metadata_size)?;
    Ok(())
}
fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>> {
    let size = u32::try_from(payload.len() + 8).map_err(codec)?;
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.extend(size.to_be_bytes());
    bytes.extend(kind);
    bytes.extend(payload);
    Ok(bytes)
}

// flacenc emits a STREAMINFO-only header. Lofty 0.22 does not clear that
// header's last-block flag when adding comments, which hides the new tags.
// Introduce a valid zero-length padding block before invoking its writer.
// Move audio backwards in bounded chunks; this is always a private output.
fn ensure_flac_metadata_block(file: &mut File, cancel: &CancellationToken) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0; 8];
    file.read_exact(&mut header)?;
    if &header[..4] != b"fLaC" || header[4] & 0x7f != 0 || header[5..] != [0, 0, 34] {
        return Err(codec("Cabecera FLAC inválida"));
    }
    if header[4] & 0x80 == 0 {
        return Ok(());
    }
    let length = file.metadata()?.len();
    if length < 42 {
        return Err(codec("Cabecera FLAC incompleta"));
    }
    file.set_len(length + 4)?;
    let mut end = length;
    let mut buffer = [0u8; 128 * 1024];
    while end > 42 {
        check(cancel)?;
        let start = end.saturating_sub(buffer.len() as u64).max(42);
        let count = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        file.seek(SeekFrom::Start(start + 4))?;
        file.write_all(&buffer[..count])?;
        end = start;
    }
    file.seek(SeekFrom::Start(42))?;
    file.write_all(&[0x81, 0, 0, 0])?;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&[0])?;
    Ok(())
}

// Container-specific binary frames cannot always be represented in a different
// tag format. Retain their original serialized tag under a reserved text field
// instead of silently discarding them; normal mapped fields remain usable by
// ordinary players. Pictures are already copied byte-for-byte separately.
fn archive_nonportable(source: &Tag, target: &mut Tag) -> Result<()> {
    let needs_archive = source.has_format_specific_items()
        || source.items().any(|item| {
            matches!(item.value(), ItemValue::Binary(_))
                || item.key().map_key(target.tag_type(), false).is_none()
        });
    if !needs_archive {
        return Ok(());
    }
    let archive_key =
        ItemKey::Unknown(format!("{ARCHIVE_PREFIX}{:?}", source.tag_type()).to_ascii_uppercase());
    if target.get(&archive_key).is_some() {
        return Ok(());
    }
    let mut original = source.clone();
    original.retain(
        |item| !matches!(item.key(),ItemKey::Unknown(key) if key.starts_with(ARCHIVE_PREFIX)),
    );
    while !original.pictures().is_empty() {
        original.remove_picture(0);
    }
    let mut bytes = Vec::new();
    if source.tag_type() == TagType::Id3v2 {
        Id3v2Tag::from(original)
            .dump_to(&mut bytes, WriteOptions::default())
            .map_err(codec)?;
    } else {
        original
            .dump_to(&mut bytes, WriteOptions::default())
            .map_err(codec)?;
    }
    if bytes.len() > MAX_METADATA_BYTES / 2 {
        return Err(codec(
            "Las etiquetas originales son demasiado grandes para conservarlas",
        ));
    }
    target.push_unchecked(TagItem::new(
        archive_key,
        ItemValue::Text(STANDARD.encode(bytes)),
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{convert, convert_with_metadata};
    use rodio::Source;

    fn tagged_wave(path: &Path) {
        let mut wave = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 44100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..44100 {
            wave.write_sample((4000.0 * (frame as f64 * 0.05).sin()) as i16)
                .unwrap();
        }
        wave.finalize().unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        for (key, value) in [
            (ItemKey::TrackTitle, "Canción original"),
            (ItemKey::TrackArtist, "Artista"),
            (ItemKey::AlbumTitle, "Álbum"),
            (ItemKey::AlbumArtist, "Grupo"),
            (ItemKey::Composer, "Compositor"),
            (ItemKey::Genre, "Rock"),
            (ItemKey::RecordingDate, "2024-02-03"),
            (ItemKey::TrackNumber, "3"),
            (ItemKey::TrackTotal, "12"),
            (ItemKey::DiscNumber, "1"),
            (ItemKey::DiscTotal, "2"),
            (ItemKey::Comment, "Comentario íntegro"),
            (ItemKey::Lyrics, "Primera línea\nSegunda línea"),
        ] {
            tag.insert_text(key, value.into());
        }
        tag.push_unchecked(TagItem::new(
            ItemKey::Unknown("CUSTOM_NOTE".into()),
            ItemValue::Text("Preservar también".into()),
        ));
        let mut picture = Picture::from_reader(
            &mut include_bytes!("../../yt-core/tests/fixtures/cover.png").as_slice(),
        )
        .unwrap();
        picture.set_pic_type(PictureType::CoverFront);
        tag.push_picture(picture);
        let mut tag = Id3v2Tag::from(tag);
        tag.insert(lofty::id3::v2::Frame::Private(
            lofty::id3::v2::PrivateFrame::new("yt-ultimate.test".into(), vec![0, 1, 2, 255, 128]),
        ));
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    #[test]
    fn real_exports_preserve_rich_tags_cover_and_decodable_audio() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.wav");
        tagged_wave(&input);
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
            let output = temp.path().join(format!("output.{}", format.extension()));
            convert(&input, &output, format, &CancellationToken::new(), |_| {})
                .unwrap_or_else(|error| panic!("{format:?}: {error}"));
            let file = lofty::read_from_path(&output).unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let kind = match format {
                AudioFormat::Flac | AudioFormat::Ogg(_) | AudioFormat::Opus(_) => {
                    TagType::VorbisComments
                }
                AudioFormat::M4a(_) => TagType::Mp4Ilst,
                _ => TagType::Id3v2,
            };
            let tag = file.tag(kind).unwrap_or_else(|| {
                panic!(
                    "missing {kind:?} for {format:?}; tags={:?}",
                    file.tags()
                        .iter()
                        .map(|tag| tag.tag_type())
                        .collect::<Vec<_>>()
                )
            });
            for (key, expected) in [
                (ItemKey::TrackTitle, "Canción original"),
                (ItemKey::TrackArtist, "Artista"),
                (ItemKey::AlbumTitle, "Álbum"),
                (ItemKey::AlbumArtist, "Grupo"),
                (ItemKey::Composer, "Compositor"),
                (ItemKey::Genre, "Rock"),
                (ItemKey::RecordingDate, "2024-02-03"),
                (ItemKey::TrackNumber, "3"),
                (ItemKey::TrackTotal, "12"),
                (ItemKey::DiscNumber, "1"),
                (ItemKey::DiscTotal, "2"),
                (ItemKey::Comment, "Comentario íntegro"),
                (ItemKey::Lyrics, "Primera línea\nSegunda línea"),
            ] {
                assert_eq!(tag.get_string(&key), Some(expected), "{format:?}: {key:?}");
            }
            assert_eq!(
                tag.get_string(&ItemKey::Unknown(
                    if kind == TagType::Mp4Ilst {
                        "----:com.ytultimate:CUSTOM_NOTE"
                    } else {
                        "CUSTOM_NOTE"
                    }
                    .into()
                )),
                Some("Preservar también")
            );
            assert!(tag
                .pictures()
                .iter()
                .any(|picture| (picture.pic_type() == PictureType::CoverFront
                    || kind == TagType::Mp4Ilst)
                    && picture.data() == include_bytes!("../../yt-core/tests/fixtures/cover.png")));
            if kind != TagType::Id3v2 {
                let archived = tag
                    .get_string(&ItemKey::Unknown(
                        if kind == TagType::Mp4Ilst {
                            "----:com.ytultimate:YTULTIMATE_SOURCE_TAG_ID3V2"
                        } else {
                            "YTULTIMATE_SOURCE_TAG_ID3V2"
                        }
                        .into(),
                    ))
                    .unwrap();
                let archived = STANDARD.decode(archived).unwrap();
                assert!(archived.windows(4).any(|bytes| bytes == b"PRIV"));
                assert!(archived
                    .windows(5)
                    .any(|bytes| bytes == [0, 1, 2, 255, 128]));
            } else {
                let specific = Id3v2Tag::from(tag.clone());
                assert!(specific.into_iter().any(|frame| matches!(frame, lofty::id3::v2::Frame::Private(frame) if frame.private_data == [0, 1, 2, 255, 128])));
            }
            let audio = yt_media::decode_file(&output).unwrap();
            let rate = if matches!(format, AudioFormat::Opus(_)) {
                48000
            } else {
                44100
            };
            assert_eq!(audio.sample_rate(), rate);
            assert!(audio.count() >= rate as usize - 1024, "{format:?}");
            assert_eq!(std::fs::read(&input).unwrap(), original);
        }
    }

    #[test]
    fn download_overrides_and_metadata_failure_are_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.wav");
        tagged_wave(&input);
        let cover = temp.path().join("cover.png");
        std::fs::write(
            &cover,
            include_bytes!("../../yt-core/tests/fixtures/cover.png"),
        )
        .unwrap();
        let metadata = MediaMetadata {
            title: Some("Downloaded title".into()),
            artist: Some("Channel".into()),
            source_url: Some("https://www.youtube.com/watch?v=abcdefghijk".into()),
            artwork_path: Some(cover),
            preserve_source_tags: false,
            ..MediaMetadata::default()
        };
        let output = temp.path().join("download.mp3");
        convert_with_metadata(
            &input,
            &output,
            AudioFormat::Mp3(128),
            &metadata,
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();
        let tagged = lofty::read_from_path(&output).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Downloaded title"));
        assert_eq!(tag.artist().as_deref(), Some("Channel"));
        assert!(tag.get_string(&ItemKey::Composer).is_none());
        assert_eq!(tag.pictures().len(), 1);
        let original_output = std::fs::read(&output).unwrap();
        let bad_cover = temp.path().join("invalid.png");
        std::fs::write(&bad_cover, b"not an image").unwrap();
        let invalid = MediaMetadata {
            artwork_path: Some(bad_cover),
            ..MediaMetadata::default()
        };
        assert!(copy_metadata(&input, &output, &invalid, &CancellationToken::new()).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), original_output);
        let failed = temp.path().join("failed.flac");
        assert!(convert_with_metadata(
            &input,
            &failed,
            AudioFormat::Flac,
            &invalid,
            &CancellationToken::new(),
            |_| {}
        )
        .is_err());
        assert!(!failed.exists());
        assert!(copy_metadata(&input, &input, &metadata, &CancellationToken::new()).is_err());
        copy_metadata(
            &input,
            &output,
            &MediaMetadata::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            lofty::read_from_path(&output)
                .unwrap()
                .primary_tag()
                .unwrap()
                .title()
                .as_deref(),
            Some("Canción original")
        );
        let strip = MediaMetadata {
            preserve_source_tags: false,
            ..MediaMetadata::default()
        };
        copy_metadata(&input, &output, &strip, &CancellationToken::new()).unwrap();
        assert!(lofty::read_from_path(&output).unwrap().tags().is_empty());
        assert!(yt_media::decode_file(&output).unwrap().count() >= 44100);
    }

    #[test]
    fn additional_formats_metadata_replacement_keeps_audio_and_failure_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.wav");
        tagged_wave(&input);
        for format in [
            AudioFormat::M4a(192),
            AudioFormat::Aac(192),
            AudioFormat::Ogg(192),
            AudioFormat::Opus(192),
        ] {
            let output = dir.path().join(format!("output.{}", format.extension()));
            convert(&input, &output, format, &CancellationToken::new(), |_| {}).unwrap();
            let before = yt_media::decode_file(&output).unwrap().collect::<Vec<_>>();
            let metadata = MediaMetadata {
                title: Some("Título actualizado".into()),
                source_url: Some("https://www.youtube.com/watch?v=abcdefghijk".into()),
                ..MediaMetadata::default()
            };
            copy_metadata(&input, &output, &metadata, &CancellationToken::new())
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let tagged = lofty::read_from_path(&output).unwrap();
            let tag = tagged.primary_tag().unwrap();
            assert_eq!(tag.title().as_deref(), Some("Título actualizado"));
            assert!(tag.items().any(|item|matches!(item.value(),ItemValue::Text(value)|ItemValue::Locator(value) if value=="https://www.youtube.com/watch?v=abcdefghijk")),"{format:?}: source URL");
            assert!(tag
                .pictures()
                .iter()
                .any(|p| p.data() == include_bytes!("../../yt-core/tests/fixtures/cover.png")));
            assert_eq!(
                yt_media::decode_file(&output).unwrap().collect::<Vec<_>>(),
                before,
                "{format:?}"
            );
            let bytes = std::fs::read(&output).unwrap();
            let invalid = MediaMetadata {
                artwork_path: Some(dir.path().join("missing.png")),
                ..MediaMetadata::default()
            };
            assert!(copy_metadata(&input, &output, &invalid, &CancellationToken::new()).is_err());
            assert_eq!(std::fs::read(&output).unwrap(), bytes);
            copy_metadata(
                &input,
                &output,
                &MediaMetadata {
                    preserve_source_tags: false,
                    ..MediaMetadata::default()
                },
                &CancellationToken::new(),
            )
            .unwrap();
            assert!(lofty::read_from_path(&output)
                .unwrap()
                .primary_tag()
                .is_none_or(|t| t.title().is_none()));
            assert_eq!(
                yt_media::decode_file(&output).unwrap().collect::<Vec<_>>(),
                before
            );
        }
    }
}
