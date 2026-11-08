//! Desktop update checks and verified installer downloads. Installation is a
//! separate explicit call; there is no background or on-quit execution.

mod metadata;

pub use metadata::{Edition, Platform, UpdateInfo};
pub use tokio_util::sync::CancellationToken;

use futures_util::StreamExt;
use semver::Version;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub const RELEASE_REPOSITORY: &str = "ElWapoteDev/yt-ultimate-releases";
const MAX_INSTALLER_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, UpdateError>;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("No se pudo consultar la actualización: {0}")]
    Network(String),
    #[error("Metadatos de actualización inválidos: {0}")]
    Metadata(String),
    #[error("La descarga no coincide con el tamaño o SHA-256 publicado")]
    Integrity,
    #[error("La descarga fue cancelada")]
    Canceled,
    #[error("No hay actualizador disponible para esta plataforma")]
    UnsupportedPlatform,
    #[error("Archivo de actualización: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedUpdate {
    path: PathBuf,
    info: UpdateInfo,
}
impl VerifiedUpdate {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn info(&self) -> &UpdateInfo {
        &self.info
    }
}

#[derive(Clone)]
pub struct Updater {
    client: reqwest::Client,
    current: Version,
    cache_dir: PathBuf,
    repository: String,
    platform: Platform,
    edition: Edition,
}

impl Updater {
    pub fn new(current_version: &str, cache_dir: impl AsRef<Path>) -> Result<Self> {
        Self::with_edition(current_version, cache_dir, Edition::Base)
    }

    pub fn with_edition(
        current_version: &str,
        cache_dir: impl AsRef<Path>,
        edition: Edition,
    ) -> Result<Self> {
        Self::with_repository(current_version, cache_dir, RELEASE_REPOSITORY, edition)
    }

    /// Only expose this constructor through developer settings. Normal builds
    /// always use the existing application's GitHub release repository.
    pub fn for_developer_repository(
        current_version: &str,
        cache_dir: impl AsRef<Path>,
        repository: &str,
        edition: Edition,
    ) -> Result<Self> {
        Self::with_repository(current_version, cache_dir, repository, edition)
    }

    fn with_repository(
        current_version: &str,
        cache_dir: impl AsRef<Path>,
        repository: &str,
        edition: Edition,
    ) -> Result<Self> {
        if !metadata::valid_repository(repository) {
            return Err(UpdateError::Metadata("Repositorio inválido".into()));
        }
        let current =
            Version::parse(current_version).map_err(|e| UpdateError::Metadata(e.to_string()))?;
        let client = reqwest::Client::builder()
            .user_agent(format!("YT-Ultimate/{current_version}"))
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 8 {
                    return attempt.error("Too many update redirects");
                }
                if metadata::allowed_transport_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("Update redirect left trusted HTTPS hosts")
                }
            }))
            .build()
            .map_err(network)?;
        std::fs::create_dir_all(cache_dir.as_ref())?;
        let cache_dir = std::fs::canonicalize(cache_dir.as_ref())?;
        Ok(Self {
            client,
            current,
            cache_dir,
            repository: repository.into(),
            platform: Platform::current()?,
            edition,
        })
    }

    pub async fn check(&self) -> Result<Option<UpdateInfo>> {
        let api = format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.repository
        );
        let response = self
            .client
            .get(api)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .timeout(Duration::from_secs(45))
            .send()
            .await
            .map_err(network)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = bounded_response(response, 2 * 1024 * 1024).await?;
        let release: metadata::Release =
            serde_json::from_slice(&bytes).map_err(|e| UpdateError::Metadata(e.to_string()))?;
        let Some(selected) = metadata::select_release(
            &release,
            &self.current,
            self.platform,
            self.edition,
            &self.repository,
        )?
        else {
            return Ok(None);
        };
        let digest = selected
            .asset
            .digest
            .as_deref()
            .map(metadata::parse_digest)
            .transpose()?
            .flatten();
        let checksums = release
            .assets
            .iter()
            .filter(|asset| asset.name == "SHA256SUMS" && asset.state == "uploaded")
            .collect::<Vec<_>>();
        if checksums.len() > 1 {
            return Err(UpdateError::Metadata("SHA256SUMS duplicado".into()));
        }
        let manifest_digest = if let Some(asset) = checksums.first() {
            metadata::validate_asset_url(
                &asset.browser_download_url,
                &self.repository,
                &release.tag_name,
                &asset.name,
            )?;
            let response = self
                .client
                .get(&asset.browser_download_url)
                .timeout(Duration::from_secs(45))
                .send()
                .await
                .map_err(network)?;
            let text = bounded_response(response, 64 * 1024).await?;
            let text = std::str::from_utf8(&text)
                .map_err(|_| UpdateError::Metadata("SHA256SUMS no es UTF-8".into()))?;
            Some(metadata::checksum_for(text, &selected.asset.name)?)
        } else {
            None
        };
        if digest
            .as_ref()
            .zip(manifest_digest.as_ref())
            .is_some_and(|(a, b)| a != b)
        {
            return Err(UpdateError::Integrity);
        }
        let sha256 = manifest_digest.or(digest).ok_or_else(|| {
            UpdateError::Metadata("No se publicó SHA-256 para el instalador".into())
        })?;
        Ok(Some(UpdateInfo {
            version: selected.version.to_string(),
            name: selected.asset.name.clone(),
            download_url: selected.asset.browser_download_url.clone(),
            release_url: metadata::release_url(&self.repository, &release.tag_name)?,
            sha256,
            size: selected.asset.size,
            platform: self.platform,
            edition: self.edition,
        }))
    }

    pub async fn download<F>(
        &self,
        info: &UpdateInfo,
        cancel: CancellationToken,
        on_progress: F,
    ) -> Result<VerifiedUpdate>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        self.validate_info(info)?;
        if cancel.is_cancelled() {
            return Err(UpdateError::Canceled);
        }
        let directory = self.cache_dir.join(&info.version);
        tokio::fs::create_dir_all(&directory).await?;
        if !directory.canonicalize()?.starts_with(&self.cache_dir) {
            return Err(UpdateError::Metadata(
                "Ruta de descarga fuera de la caché".into(),
            ));
        }
        let destination = directory.join(&info.name);
        if destination.try_exists()? {
            self.verify_cached_async(destination.clone(), info.clone())
                .await?;
            if cancel.is_cancelled() {
                return Err(UpdateError::Canceled);
            }
            return Ok(VerifiedUpdate {
                path: destination,
                info: info.clone(),
            });
        }
        let response = tokio::select! {
            biased;
            _=cancel.cancelled()=>return Err(UpdateError::Canceled),
            response=self.client.get(&info.download_url).timeout(Duration::from_secs(3600)).send()=>response.map_err(network)?.error_for_status().map_err(network)?,
        };
        download_response(response, info, &destination, cancel, on_progress).await?;
        self.verify_cached_async(destination.clone(), info.clone())
            .await?;
        Ok(VerifiedUpdate {
            path: destination,
            info: info.clone(),
        })
    }

    /// Run only after an explicit user click on "Install". macOS opens the
    /// verified DMG in the system; Windows launches the verified NSIS installer.
    /// Closing the app or finishing a download never calls this method.
    pub fn open_installer(&self, update: &VerifiedUpdate) -> Result<()> {
        self.validate_info(&update.info)?;
        if update.info.platform != Platform::current()? {
            return Err(UpdateError::UnsupportedPlatform);
        }
        self.verify_cached(&update.path, &update.info)?;
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("/usr/bin/open")
                .arg(&update.path)
                .spawn()?;
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new(&update.path).spawn()?;
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            return Err(UpdateError::UnsupportedPlatform);
        }
        Ok(())
    }

    fn validate_info(&self, info: &UpdateInfo) -> Result<()> {
        let version =
            Version::parse(&info.version).map_err(|e| UpdateError::Metadata(e.to_string()))?;
        if !version.pre.is_empty()
            || !version.cmp_precedence(&self.current).is_gt()
            || info.platform != self.platform
            || info.edition != self.edition
            || info.size == 0
            || info.size > MAX_INSTALLER_BYTES
            || !info
                .platform
                .accepts_name(&info.name, &version, self.edition)
        {
            return Err(UpdateError::Metadata(
                "Versión, arquitectura o nombre de instalador no válido".into(),
            ));
        }
        metadata::valid_sha256(&info.sha256)?;
        let tag = metadata::tag_from_release_url(&info.release_url, &self.repository)?;
        if Version::parse(tag.strip_prefix('v').unwrap_or(&tag))
            .ok()
            .as_ref()
            != Some(&version)
        {
            return Err(UpdateError::Metadata(
                "La versión no coincide con la publicación".into(),
            ));
        }
        metadata::validate_asset_url(&info.download_url, &self.repository, &tag, &info.name)?;
        Ok(())
    }

    fn verify_cached(&self, path: &Path, info: &UpdateInfo) -> Result<()> {
        if path.file_name().and_then(|name| name.to_str()) != Some(&info.name)
            || std::fs::symlink_metadata(path)?.file_type().is_symlink()
            || !path.canonicalize()?.starts_with(&self.cache_dir)
        {
            return Err(UpdateError::Integrity);
        }
        verify_file(path, info)
    }

    async fn verify_cached_async(&self, path: PathBuf, info: UpdateInfo) -> Result<()> {
        let updater = self.clone();
        tokio::task::spawn_blocking(move || updater.verify_cached(&path, &info))
            .await
            .map_err(|e| UpdateError::Network(format!("La verificación se interrumpió: {e}")))?
    }
}

fn network(error: reqwest::Error) -> UpdateError {
    UpdateError::Network(error.without_url().to_string())
}

async fn bounded_response(response: reqwest::Response, maximum: usize) -> Result<Vec<u8>> {
    let response = response.error_for_status().map_err(network)?;
    if response
        .content_length()
        .is_some_and(|size| size > maximum as u64)
    {
        return Err(UpdateError::Metadata(
            "Respuesta del servidor demasiado grande".into(),
        ));
    }
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(network)?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(UpdateError::Metadata(
                "Respuesta del servidor demasiado grande".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn verify_file(path: &Path, info: &UpdateInfo) -> Result<()> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() != info.size {
        return Err(UpdateError::Integrity);
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != info.sha256 {
        return Err(UpdateError::Integrity);
    }
    Ok(())
}

async fn download_response<F>(
    response: reqwest::Response,
    info: &UpdateInfo,
    destination: &Path,
    cancel: CancellationToken,
    on_progress: F,
) -> Result<()>
where
    F: Fn(DownloadProgress) + Send + Sync,
{
    let response = response.error_for_status().map_err(network)?;
    if response
        .content_length()
        .is_some_and(|size| size != info.size)
    {
        return Err(UpdateError::Integrity);
    }
    let content_type = response
        .headers()
        .get("Content-Type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("text/") || content_type.starts_with("application/json") {
        return Err(UpdateError::Integrity);
    }
    let temporary = tempfile::Builder::new()
        .prefix(".ytu-update-")
        .suffix(".part")
        .tempfile_in(destination.parent().ok_or(UpdateError::Integrity)?)?;
    let mut file = tokio::fs::File::from_std(temporary.as_file().try_clone()?);
    let mut chunks = response.bytes_stream();
    let mut downloaded = 0u64;
    let mut hash = Sha256::new();
    on_progress(DownloadProgress {
        downloaded_bytes: 0,
        total_bytes: info.size,
    });
    loop {
        let next = tokio::select! {biased;_=cancel.cancelled()=>return Err(UpdateError::Canceled),chunk=chunks.next()=>chunk};
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(network)?;
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .ok_or(UpdateError::Integrity)?;
        if downloaded > info.size || downloaded > MAX_INSTALLER_BYTES {
            return Err(UpdateError::Integrity);
        }
        hash.update(&chunk);
        file.write_all(&chunk).await?;
        on_progress(DownloadProgress {
            downloaded_bytes: downloaded,
            total_bytes: info.size,
        });
    }
    if downloaded != info.size || format!("{:x}", hash.finalize()) != info.sha256 {
        return Err(UpdateError::Integrity);
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    if cancel.is_cancelled() {
        return Err(UpdateError::Canceled);
    }
    temporary
        .persist_noclobber(destination)
        .map_err(|error| UpdateError::Io(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests;
