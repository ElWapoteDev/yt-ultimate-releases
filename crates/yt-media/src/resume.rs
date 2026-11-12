//! Restart-safe downloads. Checkpoints contain identity and a digest, never a
//! signed URL, cookie, proxy credential, or authorization header.
use crate::{
    CancellationToken, DownloadProgress, DownloadReceipt, Downloader, MediaError, ResolvedMedia,
    Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeKey {
    pub media_id: String,
    pub itag: u32,
    pub total_bytes: u64,
    #[serde(default)]
    pub audio_track_id: Option<String>,
    #[serde(default)]
    pub dynamic_range_compressed: bool,
}
impl ResumeKey {
    pub fn for_media(media: &ResolvedMedia) -> Result<Self> {
        let url = url::Url::parse(&media.stream_url).map_err(|_| MediaError::FormatUnavailable)?;
        let itag = url
            .query_pairs()
            .find(|(k, _)| k == "itag")
            .and_then(|(_, v)| v.parse().ok())
            .ok_or(MediaError::FormatUnavailable)?;
        let result = Self {
            media_id: media.media.id.clone(),
            itag,
            total_bytes: media.content_length.ok_or(MediaError::FormatUnavailable)?,
            audio_track_id: media.audio_track_id.clone(),
            dynamic_range_compressed: media.dynamic_range_compressed,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<()> {
        if self.media_id.is_empty()
            || self.media_id.len() > 128
            || !self
                .media_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.itag == 0
            || self.total_bytes == 0
            || self
                .audio_track_id
                .as_ref()
                .is_some_and(|v| v.len() > 128 || v.chars().any(char::is_control))
        {
            return Err(MediaError::InvalidInput(
                "La identidad de la descarga no es válida.".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeStatus {
    pub key: ResumeKey,
    pub completed_bytes: u64,
}
#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    key: ResumeKey,
    completed_bytes: u64,
    sha256: String,
}
fn paths(destination: &Path) -> Result<(PathBuf, PathBuf)> {
    let name = destination
        .file_name()
        .ok_or_else(|| {
            MediaError::InvalidInput("El destino necesita un nombre de archivo.".into())
        })?
        .to_string_lossy();
    Ok((
        destination.with_file_name(format!(".{name}.ytu-part")),
        destination.with_file_name(format!(".{name}.ytu-resume.json")),
    ))
}
fn read_checkpoint(path: &Path) -> Result<Checkpoint> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > 16384 {
        return Err(MediaError::IncompleteDownload);
    }
    let value: Checkpoint = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| MediaError::IncompleteDownload)?;
    value.key.validate()?;
    if value.version != 1
        || value.completed_bytes > value.key.total_bytes
        || value.sha256.len() != 64
    {
        return Err(MediaError::IncompleteDownload);
    }
    Ok(value)
}
fn write_checkpoint(path: &Path, value: &Checkpoint, initial: bool) -> Result<()> {
    use std::io::Write;
    let mut temporary = tempfile::Builder::new()
        .prefix(".ytu-checkpoint-")
        .tempfile_in(path.parent().unwrap_or(Path::new(".")))?;
    serde_json::to_writer(temporary.as_file_mut(), value).map_err(std::io::Error::other)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    if initial {
        temporary.persist_noclobber(path).map_err(|e| e.error)?;
    } else {
        temporary.persist(path).map_err(|e| e.error)?;
    }
    Ok(())
}
impl Downloader {
    pub fn resume_status(&self, destination: &Path) -> Result<Option<ResumeStatus>> {
        let (part, checkpoint) = paths(destination)?;
        if !checkpoint.try_exists()? && !part.try_exists()? {
            return Ok(None);
        }
        let value = read_checkpoint(&checkpoint)?;
        let meta = std::fs::symlink_metadata(part)?;
        if !meta.file_type().is_file() || meta.len() < value.completed_bytes {
            return Err(MediaError::IncompleteDownload);
        }
        Ok(Some(ResumeStatus {
            key: value.key,
            completed_bytes: value.completed_bytes,
        }))
    }
    /// Remove only the partial file whose checkpoint matches this job. An active
    /// download's OS file lock prevents deletion while it is writing.
    pub fn discard_resume(&self, destination: &Path, key: &ResumeKey) -> Result<()> {
        let (part, path) = paths(destination)?;
        if !part.try_exists()? && !path.try_exists()? {
            return Ok(());
        }
        let checkpoint = read_checkpoint(&path)?;
        if &checkpoint.key != key {
            return Err(MediaError::InvalidInput(
                "La descarga parcial pertenece a otro contenido.".into(),
            ));
        }
        if !std::fs::symlink_metadata(&part)?.file_type().is_file() {
            return Err(MediaError::IncompleteDownload);
        }
        let file = OpenOptions::new().read(true).write(true).open(&part)?;
        file.try_lock()
            .map_err(|_| MediaError::InvalidInput("La descarga todavía está activa.".into()))?;
        std::fs::remove_file(part)?;
        std::fs::remove_file(path)?;
        Ok(())
    }
    /// A cancellation pauses and retains validated bytes. Use `discard_resume`
    /// after this future finishes to explicitly cancel and erase this job.
    /// Resolve a fresh signed URL on restart and reuse the same ResumeKey.
    pub async fn download_resumable<F>(
        &self,
        url: &str,
        destination: &Path,
        key: ResumeKey,
        cancel: CancellationToken,
        on_progress: F,
    ) -> Result<DownloadReceipt>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        key.validate()?;
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        if destination.try_exists()? {
            return Err(MediaError::DestinationExists);
        }
        let url = url::Url::parse(url)
            .map_err(|_| MediaError::InvalidInput("El enlace no es válido.".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(MediaError::InvalidInput(
                "La descarga necesita HTTP sin credenciales.".into(),
            ));
        }
        let (part, checkpoint_path) = paths(destination)?;
        tokio::fs::create_dir_all(
            destination
                .parent()
                .filter(|v| !v.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .await?;
        let exists = checkpoint_path.try_exists()?;
        let checkpoint = if exists {
            read_checkpoint(&checkpoint_path)?
        } else {
            Checkpoint {
                version: 1,
                key: key.clone(),
                completed_bytes: 0,
                sha256: format!("{:x}", Sha256::digest([])),
            }
        };
        if checkpoint.key != key {
            return Err(MediaError::InvalidInput("El formato o tamaño cambió; descarta la descarga parcial antes de volver a empezar.".into()));
        }
        if exists && !std::fs::symlink_metadata(&part)?.file_type().is_file() {
            return Err(MediaError::IncompleteDownload);
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(!exists)
            .open(&part)?;
        lock.try_lock().map_err(|_| {
            MediaError::InvalidInput("Esta descarga ya está activa en otra ventana.".into())
        })?;
        if !exists {
            if let Err(error) = write_checkpoint(&checkpoint_path, &checkpoint, true) {
                drop(lock);
                let _ = std::fs::remove_file(&part);
                return Err(error);
            }
        }
        let mut file = tokio::fs::File::from_std(lock.try_clone()?);
        let mut digest = Sha256::new();
        let mut verified = 0;
        let mut buffer = vec![0; 64 * 1024];
        while verified < checkpoint.completed_bytes {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            let count = buffer
                .len()
                .min((checkpoint.completed_bytes - verified) as usize);
            file.read_exact(&mut buffer[..count]).await?;
            digest.update(&buffer[..count]);
            verified += count as u64;
        }
        if format!("{:x}", digest.clone().finalize()) != checkpoint.sha256 {
            return Err(MediaError::IncompleteDownload);
        }
        // A crash may leave bytes written after the last durable checkpoint.
        // Only truncate those after validating the committed prefix digest.
        file.set_len(checkpoint.completed_bytes).await?;
        file.seek(std::io::SeekFrom::Start(checkpoint.completed_bytes))
            .await?;
        let mut completed = checkpoint.completed_bytes;
        on_progress(DownloadProgress {
            downloaded_bytes: completed,
            total_bytes: Some(key.total_bytes),
        });
        while completed < key.total_bytes {
            let end = (completed + 1024 * 1024 - 1).min(key.total_bytes - 1);
            let bytes = tokio::select! {_ = cancel.cancelled()=>return Err(MediaError::Cancelled),result=crate::streaming::fetch_range(&self.client,&url,completed,end,key.total_bytes)=>result?};
            file.write_all(&bytes).await?;
            file.flush().await?;
            file.sync_data().await?;
            digest.update(&bytes);
            completed += bytes.len() as u64;
            write_checkpoint(
                &checkpoint_path,
                &Checkpoint {
                    version: 1,
                    key: key.clone(),
                    completed_bytes: completed,
                    sha256: format!("{:x}", digest.clone().finalize()),
                },
                false,
            )?;
            on_progress(DownloadProgress {
                downloaded_bytes: completed,
                total_bytes: Some(key.total_bytes),
            });
        }
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        file.sync_all().await?;
        drop(file);
        // Use atomic non-replacing rename on Windows/macOS as well as filesystems
        // without hard links. Failed publication must retain the durable partial.
        let mut partial = tempfile::TempPath::try_from_path(part.clone())?;
        partial.disable_cleanup(true);
        partial.persist_noclobber(destination).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                MediaError::DestinationExists
            } else {
                MediaError::Io(error.error)
            }
        })?;
        let _ = std::fs::remove_file(&checkpoint_path);
        drop(lock);
        Ok(DownloadReceipt {
            path: destination.to_owned(),
            bytes: completed,
        })
    }
}
