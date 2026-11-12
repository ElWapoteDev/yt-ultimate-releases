use crate::{AppCore, CoreError, MediaMetadata, Result};
use image::{ImageFormat, ImageReader, Limits};
use lofty::{
    file::{TaggedFile, TaggedFileExt},
    picture::PictureType,
};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Write},
    path::PathBuf,
};

pub const MAX_ARTWORK_BYTES: usize = 16 * 1024 * 1024;

/// Validate actual image bytes before serving or embedding them. SVG and remote
/// references are not accepted. Decoding is bounded independently of file size.
pub fn validate_artwork(bytes: &[u8]) -> Result<&'static str> {
    decode_artwork(bytes).map(|(_, extension)| extension)
}

/// Decode artwork with the same bounds as [`validate_artwork`] and box-filter it
/// to at most `side`×`side` opaque RGB samples, for colour analysis that must not
/// scale with the cover's resolution.
pub fn artwork_samples(bytes: &[u8], side: u32) -> Result<Vec<[u8; 3]>> {
    let (image, _) = decode_artwork(bytes)?;
    let reduced = image::imageops::thumbnail(&image.to_rgba8(), side.max(1), side.max(1));
    Ok(reduced
        .pixels()
        .filter(|pixel| pixel[3] >= 128)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect())
}

/// Width-to-height ratios of YouTube video thumbnails: widescreen frames and
/// the letterboxed 4:3 sizes.
const VIDEO_FRAME_RATIOS: [f64; 2] = [16.0 / 9.0, 4.0 / 3.0];
/// Relative difference within which a cover counts as one of those ratios.
const VIDEO_FRAME_TOLERANCE: f64 = 0.02;

/// Whether cover bytes are shaped like a video frame (16:9 or 4:3) rather
/// than square album art. Only the image header is read, under the format and
/// size bounds of [`validate_artwork`].
pub fn is_video_frame(bytes: &[u8]) -> Result<bool> {
    let (format, _) = artwork_format(bytes)?;
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| CoreError::Invalid(format!("Carátula inválida: {e}")))?;
    if width == 0 || height == 0 {
        return Err(CoreError::Invalid(
            "La carátula no tiene dimensiones válidas".into(),
        ));
    }
    let ratio = f64::from(width) / f64::from(height);
    Ok(VIDEO_FRAME_RATIOS
        .iter()
        .any(|frame| (ratio / frame - 1.0).abs() <= VIDEO_FRAME_TOLERANCE))
}

/// The allowlisted format of cover bytes and its file extension.
fn artwork_format(bytes: &[u8]) -> Result<(ImageFormat, &'static str)> {
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
        return Err(CoreError::Invalid(
            "La carátula debe ocupar entre 1 byte y 16 MiB".into(),
        ));
    }
    let format = image::guess_format(bytes).map_err(|e| CoreError::Invalid(e.to_string()))?;
    let extension = match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        ImageFormat::Gif => "gif",
        ImageFormat::WebP => "webp",
        ImageFormat::Bmp => "bmp",
        _ => {
            return Err(CoreError::Invalid(
                "Formato de carátula no compatible".into(),
            ));
        }
    };
    Ok((format, extension))
}

fn decode_artwork(bytes: &[u8]) -> Result<(image::DynamicImage, &'static str)> {
    let (format, extension) = artwork_format(bytes)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| CoreError::Invalid(format!("Carátula inválida: {e}")))?;
    if image.width() == 0 || image.height() == 0 {
        return Err(CoreError::Invalid(
            "La carátula no tiene dimensiones válidas".into(),
        ));
    }
    Ok((image, extension))
}

impl AppCore {
    pub(crate) fn cache_tag_artwork(&self, tagged: &TaggedFile) -> Option<String> {
        let mut pictures = tagged
            .tags()
            .iter()
            .flat_map(|tag| tag.pictures())
            .collect::<Vec<_>>();
        pictures.sort_by_key(|picture| picture.pic_type() != PictureType::CoverFront);
        pictures.into_iter().find_map(|picture| {
            self.cache_artwork(picture.data()).ok().map(|path| {
                format!(
                    "artwork:{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )
            })
        })
    }

    pub(crate) fn refresh_embedded_artwork(&self, id: &str, path: &std::path::Path) {
        let needs_artwork = self
            .connection()
            .ok()
            .and_then(|db| {
                db.query_row(
                    "SELECT artwork_path='' FROM tracks WHERE id=?1",
                    [id],
                    |row| row.get::<_, bool>(0),
                )
                .ok()
            })
            .unwrap_or(false);
        if !needs_artwork {
            return;
        }
        if let Ok(tagged) = lofty::read_from_path(path)
            && let Some(descriptor) = self.cache_tag_artwork(&tagged)
            && let Ok(db) = self.connection()
        {
            let _ = db.execute(
                "UPDATE tracks SET artwork_path=?1 WHERE id=?2 AND artwork_path=''",
                rusqlite::params![descriptor, id],
            );
        }
    }

    /// Store validated content under a hash-derived name; callers cannot select
    /// a path and neither symlinks nor existing unrelated bytes are overwritten.
    pub fn cache_artwork(&self, bytes: &[u8]) -> Result<PathBuf> {
        let extension = validate_artwork(bytes)?;
        let directory = self.profile_dir().join("Artwork");
        std::fs::create_dir_all(&directory)?;
        let directory = directory.canonicalize()?;
        if !directory.starts_with(self.profile_dir()) {
            return Err(CoreError::Invalid(
                "La carpeta de carátulas sale del perfil".into(),
            ));
        }
        let digest = format!("{:x}", Sha256::digest(bytes));
        let destination = directory.join(format!("{digest}.{extension}"));
        let verify_existing = || -> Result<()> {
            let metadata = std::fs::symlink_metadata(&destination)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != bytes.len() as u64
            {
                return Err(CoreError::Invalid(
                    "La ruta de carátula existente no es válida".into(),
                ));
            }
            if std::fs::read(&destination)? != bytes {
                return Err(CoreError::Invalid(
                    "La carátula existente no coincide con su contenido".into(),
                ));
            }
            Ok(())
        };
        if destination.try_exists()? {
            verify_existing()?;
            return Ok(destination);
        }
        let mut file = tempfile::Builder::new()
            .prefix(".ytu-artwork-")
            .tempfile_in(&directory)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        if let Err(error) = file.persist_noclobber(&destination) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.error.into());
            }
            verify_existing()?;
        }
        Ok(destination)
    }

    pub fn media_metadata(&self, id: &str) -> Result<MediaMetadata> {
        let track = self.track(id)?;
        self.refresh_embedded_artwork(id, &track.path);
        Ok(MediaMetadata {
            artwork_path: self.artwork_path(id)?,
            ..MediaMetadata::from(&track)
        })
    }

    /// Associate downloaded artwork with an existing record after validating it.
    pub fn set_track_artwork(&self, id: &str, bytes: &[u8]) -> Result<PathBuf> {
        self.track(id)?;
        let path = self.cache_artwork(bytes)?;
        let descriptor = format!(
            "artwork:{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        self.connection()?.execute(
            "UPDATE tracks SET artwork_path=?1,updated_at=?2 WHERE id=?3",
            rusqlite::params![descriptor, crate::now(), id],
        )?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn samples_are_bounded_by_the_requested_side_and_keep_colours() {
        let bytes = png(400, 200, |x, _| {
            if x < 200 {
                [230, 40, 30, 255]
            } else {
                [20, 60, 220, 255]
            }
        });
        let samples = artwork_samples(&bytes, 32).unwrap();
        assert!(!samples.is_empty() && samples.len() <= 32 * 32);
        assert!(
            samples
                .iter()
                .any(|[r, g, b]| *r > 200 && *g < 80 && *b < 80)
        );
        assert!(samples.iter().any(|[r, _, b]| *b > 180 && *r < 60));
    }

    #[test]
    fn transparent_pixels_are_not_sampled_and_invalid_bytes_are_rejected() {
        let bytes = png(16, 16, |x, _| {
            if x < 8 {
                [255, 255, 255, 0]
            } else {
                [10, 200, 90, 255]
            }
        });
        let samples = artwork_samples(&bytes, 16).unwrap();
        assert!(samples.iter().all(|[r, g, _]| *g > 150 && *r < 60));
        assert!(artwork_samples(b"not an image", 16).is_err());
        assert!(artwork_samples(&[], 16).is_err());
    }

    #[test]
    fn video_frames_are_told_apart_from_album_covers() {
        let opaque = |_, _| [40, 40, 40, 255];
        for (width, height) in [(1280, 720), (480, 360), (120, 90), (853, 479)] {
            assert!(is_video_frame(&png(width, height, opaque)).unwrap());
        }
        for (width, height) in [(1400, 1400), (500, 480), (720, 1280)] {
            assert!(!is_video_frame(&png(width, height, opaque)).unwrap());
        }
        assert!(is_video_frame(b"not an image").is_err());
    }
}
