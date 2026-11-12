use crate::{
    CancellationToken, DownloadProgress, Downloader, MediaError, ResolvedMedia, Result, ResumeKey,
    SearchResult, VideoQuality, YoutubeClient,
};
use std::{path::Path, time::Duration};

/// A complete temporary H.264/AAC MP4 for WebView playback. Dropping the last
/// owner removes its files; this never imports or writes into the library.
#[derive(Debug)]
pub struct PreparedVideo {
    pub media: SearchResult,
    pub height: u32,
    pub quality_label: String,
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl PreparedVideo {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl YoutubeClient {
    /// Prepare separate original video/audio tracks without a subprocess. The
    /// progress callback reports transferred bytes; completion awaits muxing.
    pub async fn prepare_video<F>(
        &self,
        input: &str,
        quality: VideoQuality,
        cancel: CancellationToken,
        on_progress: F,
    ) -> Result<PreparedVideo>
    where
        F: Fn(DownloadProgress) + Send + Sync,
    {
        let resolved = self
            .resolve_video_preparation(input, quality, cancel.clone())
            .await?;
        let directory = tempfile::Builder::new().prefix("ytu-preview-").tempdir()?;
        let downloader = Downloader {
            client: self.http_client()?,
        };
        let video_bytes = resolved
            .video
            .content_length
            .ok_or(MediaError::FormatUnavailable)?;
        let audio_bytes = resolved
            .audio
            .as_ref()
            .map(|a| a.content_length.ok_or(MediaError::FormatUnavailable))
            .transpose()?
            .unwrap_or(0);
        let total_bytes = video_bytes
            .checked_add(audio_bytes)
            .ok_or(MediaError::IncompleteDownload)?;
        let video_path = directory.path().join("video.mp4");
        prepare_track(
            self,
            &downloader,
            resolved.video,
            &video_path,
            cancel.clone(),
            |bytes| {
                on_progress(DownloadProgress {
                    downloaded_bytes: bytes,
                    total_bytes: Some(total_bytes),
                })
            },
        )
        .await?;
        let file_name = if let Some(audio) = resolved.audio {
            let audio_path = directory.path().join("audio.m4a");
            prepare_track(
                self,
                &downloader,
                audio,
                &audio_path,
                cancel.clone(),
                |bytes| {
                    on_progress(DownloadProgress {
                        downloaded_bytes: video_bytes + bytes,
                        total_bytes: Some(total_bytes),
                    })
                },
            )
            .await?;
            let destination = directory.path().join("ready.mp4");
            let mux_cancel = cancel.clone();
            // Move the directory into the blocking worker too: dropping an
            // awaiting task must not remove files while native muxing uses them.
            let directory = tokio::task::spawn_blocking(move || {
                crate::mux_mp4(&video_path, &audio_path, &destination, mux_cancel, |_| {})?;
                std::fs::remove_file(video_path)?;
                std::fs::remove_file(audio_path)?;
                Ok::<_, MediaError>(directory)
            })
            .await
            .map_err(|_| {
                MediaError::Processing("La preparación del vídeo se interrumpió.".into())
            })??;
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            return Ok(PreparedVideo {
                media: resolved.media,
                height: resolved.height,
                quality_label: resolved.quality_label,
                path: directory.path().join("ready.mp4"),
                _directory: directory,
            });
        } else {
            "video.mp4"
        };
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        Ok(PreparedVideo {
            media: resolved.media,
            height: resolved.height,
            quality_label: resolved.quality_label,
            path: directory.path().join(file_name),
            _directory: directory,
        })
    }
}

async fn prepare_track(
    youtube: &YoutubeClient,
    downloader: &Downloader,
    mut media: ResolvedMedia,
    path: &Path,
    cancel: CancellationToken,
    on_progress: impl Fn(u64) + Send + Sync,
) -> Result<()> {
    let key = ResumeKey::for_media(&media)?;
    for attempt in 0..3 {
        match downloader
            .download_resumable(&media.stream_url, path, key.clone(), cancel.clone(), |p| {
                on_progress(p.downloaded_bytes)
            })
            .await
        {
            Ok(_) => return Ok(()),
            Err(MediaError::Network(_) | MediaError::IncompleteDownload) if attempt < 2 => {
                tokio::select! {_ = cancel.cancelled()=>return Err(MediaError::Cancelled),_=tokio::time::sleep(Duration::from_millis(300 << attempt))=>{}}
                media = youtube
                    .resolve_resume(&media.media.url, &key, cancel.clone())
                    .await?;
            }
            Err(error) => return Err(error),
        }
    }
    Err(MediaError::IncompleteDownload)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn precancelled_preparation_never_opens_network_or_creates_output() {
        let youtube = YoutubeClient::new().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            youtube
                .prepare_video(
                    "abcdefghijk",
                    VideoQuality::MaxHeight(360),
                    cancel,
                    |_| panic!("cancelled progress")
                )
                .await,
            Err(MediaError::Cancelled)
        ));
    }
    #[test]
    fn prepared_media_is_owned_until_last_reference_and_never_published() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("video.mp4");
        std::fs::write(&path, b"complete").unwrap();
        let media = SearchResult {
            id: "abcdefghijk".into(),
            url: "https://www.youtube.com/watch?v=abcdefghijk".into(),
            title: "Preview".into(),
            thumbnail: String::new(),
            duration_seconds: 3,
            channel: String::new(),
            view_count: 0,
        };
        let video = std::sync::Arc::new(PreparedVideo {
            media,
            height: 360,
            quality_label: "360p".into(),
            path: path.clone(),
            _directory: directory,
        });
        let second = video.clone();
        assert_eq!(video.path(), path);
        drop(video);
        assert!(path.is_file());
        drop(second);
        assert!(!path.exists());
    }
}
