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
    Ok(extension)
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
