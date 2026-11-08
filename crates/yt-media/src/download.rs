use crate::{network_error, CancellationToken, MediaError, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

impl DownloadProgress {
    pub fn fraction(self) -> Option<f64> {
        self.total_bytes
            .filter(|total| *total > 0)
            .map(|total| (self.downloaded_bytes as f64 / total as f64).min(1.0))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadReceipt {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone)]
pub struct Downloader {
    client: reqwest::Client,
}

impl Downloader {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(40))
            .redirect(reqwest::redirect::Policy::limited(8))
            .build()
            .map_err(network_error)?;
        Ok(Self { client })
    }

    /// Write into a sibling temporary file and atomically publish only after
    /// EOF. Existing destinations are never overwritten, including races.
    pub async fn download<F>(
        &self,
        url: &str,
        destination: &Path,
        cancel: CancellationToken,
        on_progress: F,
    ) -> Result<DownloadReceipt>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        self.download_sized(url, destination, None, cancel, on_progress)
            .await
    }

    /// Validate the complete file against a size from the media resolver.
    pub async fn download_sized<F>(
        &self,
        url: &str,
        destination: &Path,
        expected_bytes: Option<u64>,
        cancel: CancellationToken,
        on_progress: F,
    ) -> Result<DownloadReceipt>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        let url = url::Url::parse(url)
            .map_err(|_| MediaError::InvalidInput("El enlace de descarga no es válido.".into()))?;
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(MediaError::InvalidInput(
                "La descarga necesita un enlace HTTP o HTTPS sin credenciales.".into(),
            ));
        }
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        if destination.try_exists()? {
            return Err(MediaError::DestinationExists);
        }
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        tokio::fs::create_dir_all(parent).await?;
        let temporary = tempfile::Builder::new()
            .prefix(".ytu-download-")
            .suffix(".part")
            .tempfile_in(parent)?;
        let mut file = tokio::fs::File::from_std(temporary.as_file().try_clone()?);
        let mut downloaded_bytes = 0u64;
        let youtube_cdn = url
            .host_str()
            .is_some_and(|host| host.ends_with(".googlevideo.com"));
        let signed_size = if youtube_cdn {
            url.query_pairs()
                .find(|(key, _)| key == "clen")
                .and_then(|(_, value)| value.parse::<u64>().ok())
                .filter(|size| *size > 0)
        } else {
            None
        };
        let mut total_bytes = expected_bytes.or(signed_size);
        if total_bytes == Some(0) {
            return Err(MediaError::IncompleteDownload);
        }
        const CHUNK_BYTES: u64 = 1024 * 1024;
        loop {
            let requested_end = downloaded_bytes.saturating_add(CHUNK_BYTES - 1).min(
                total_bytes
                    .map(|total: u64| total.saturating_sub(1))
                    .unwrap_or(u64::MAX),
            );
            let mut request_url = url.clone();
            let query_range = youtube_cdn && total_bytes.is_some();
            if query_range {
                // These CDN clients require query ranges for later chunks.
                request_url
                    .query_pairs_mut()
                    .append_pair("range", &format!("{downloaded_bytes}-{requested_end}"))
                    .append_pair("rn", &(downloaded_bytes / CHUNK_BYTES).to_string());
            }
            let mut request = self
                .client
                .get(request_url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity");
            if !query_range {
                request = request.header(
                    reqwest::header::RANGE,
                    format!("bytes={downloaded_bytes}-{requested_end}"),
                );
            }
            let response = tokio::select! {
                _ = cancel.cancelled() => return Err(MediaError::Cancelled),
                response = request.send() => response.map_err(network_error)?.error_for_status().map_err(network_error)?,
            };
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if content_type.starts_with("text/") || content_type.starts_with("application/json") {
                return Err(MediaError::IncompleteDownload);
            }
            let http_range = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            let ranged = http_range || query_range;
            let segment_length = if query_range {
                // Query ranges select the response resource itself. Applying
                // an additional HTTP Range would slice the chunk a second
                // time, so the request above deliberately sends only one.
                Some(requested_end - downloaded_bytes + 1)
            } else if http_range {
                let (start, end, total) = parse_content_range(
                    response
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        .ok_or(MediaError::IncompleteDownload)?,
                )?;
                if start != downloaded_bytes
                    || end > requested_end
                    || total_bytes.is_some_and(|previous| previous != total)
                {
                    return Err(MediaError::IncompleteDownload);
                }
                total_bytes = Some(total);
                Some(end - start + 1)
            } else {
                if downloaded_bytes > 0 {
                    return Err(MediaError::IncompleteDownload);
                }
                if response
                    .content_length()
                    .zip(total_bytes)
                    .is_some_and(|(received, expected)| received != expected)
                {
                    return Err(MediaError::IncompleteDownload);
                }
                total_bytes = total_bytes.or(response.content_length());
                total_bytes
            };
            on_progress(DownloadProgress {
                downloaded_bytes,
                total_bytes,
            });
            let mut body = response.bytes_stream();
            let segment_start = downloaded_bytes;
            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(MediaError::Cancelled),
                    chunk = body.next() => chunk,
                };
                let Some(chunk) = next else {
                    break;
                };
                let chunk = chunk.map_err(network_error)?;
                downloaded_bytes = downloaded_bytes
                    .checked_add(chunk.len() as u64)
                    .ok_or(MediaError::IncompleteDownload)?;
                if segment_length.is_some_and(|length| downloaded_bytes - segment_start > length) {
                    return Err(MediaError::IncompleteDownload);
                }
                file.write_all(&chunk).await?;
                on_progress(DownloadProgress {
                    downloaded_bytes,
                    total_bytes,
                });
            }
            if downloaded_bytes == segment_start
                || segment_length.is_some_and(|length| downloaded_bytes - segment_start != length)
            {
                return Err(MediaError::IncompleteDownload);
            }
            if !ranged || total_bytes == Some(downloaded_bytes) {
                break;
            }
        }
        if total_bytes.is_some_and(|total| total != downloaded_bytes) {
            return Err(MediaError::IncompleteDownload);
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        temporary.persist_noclobber(destination).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                MediaError::DestinationExists
            } else {
                MediaError::Io(error.error)
            }
        })?;
        Ok(DownloadReceipt {
            path: destination.to_owned(),
            bytes: downloaded_bytes,
        })
    }
}

fn parse_content_range(value: &str) -> Result<(u64, u64, u64)> {
    let invalid = || MediaError::IncompleteDownload;
    let (range, total) = value
        .strip_prefix("bytes ")
        .ok_or_else(invalid)?
        .split_once('/')
        .ok_or_else(invalid)?;
    let (start, end) = range.split_once('-').ok_or_else(invalid)?;
    let (start, end, total): (u64, u64, u64) = (
        start.parse().map_err(|_| invalid())?,
        end.parse().map_err(|_| invalid())?,
        total.parse().map_err(|_| invalid())?,
    );
    if start > end || end >= total || total == 0 {
        return Err(invalid());
    }
    Ok((start, end, total))
}

/// Portable filename, including Windows reserved names and trailing dots.
pub fn safe_file_name(title: &str, extension: &str) -> String {
    let mut name = String::new();
    for c in title.chars() {
        let safe = if c.is_control()
            || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        {
            '_'
        } else {
            c
        };
        if name.len() + safe.len_utf8() > 180 {
            break;
        }
        name.push(safe);
    }
    let trimmed = name.trim_matches([' ', '.']);
    let stem = if trimmed.is_empty() { "media" } else { trimmed };
    let root = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    let reserved = matches!(root.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (root.len() == 4
            && (root.starts_with("COM") || root.starts_with("LPT"))
            && matches!(root.as_bytes()[3], b'1'..=b'9'));
    let stem = if reserved {
        format!("_{stem}")
    } else {
        stem.to_owned()
    };
    let extension: String = extension
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{}", extension.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_portable() {
        assert_eq!(safe_file_name("CON", "mp3"), "_CON.mp3");
        assert_eq!(safe_file_name("../test: file?", ".MP4"), "_test_ file_.mp4");
        assert_eq!(safe_file_name("...", "wav"), "media.wav");
        assert_eq!(
            safe_file_name("Canción bonita", "m4a"),
            "Canción bonita.m4a"
        );
        assert_eq!(safe_file_name("LPT9.data", "ogg"), "_LPT9.data.ogg");
        assert!(safe_file_name(&"🎵".repeat(100), "m4a").len() < 200);
    }
}
