use dioxus::desktop::{
    use_asset_handler,
    wry::http::{Request, Response, StatusCode},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use yt_core::AppCore;

pub fn use_local_media(core: AppCore) {
    let artwork_core = core.clone();
    let subtitle_core = core.clone();
    use_asset_handler("preview", move |request, responder| {
        tokio::task::spawn_blocking(move || responder.respond(preview_response(&request)));
    });
    use_asset_handler("media", move |request, responder| {
        let core = core.clone();
        tokio::task::spawn_blocking(move || {
            responder.respond(media_response(&core, &request));
        });
    });
    use_asset_handler("subtitle", move |request, responder| {
        let core = subtitle_core.clone();
        tokio::task::spawn_blocking(move || responder.respond(subtitle_response(&core, &request)));
    });
    use_asset_handler("artwork", move |request, responder| {
        let core = artwork_core.clone();
        tokio::task::spawn_blocking(move || {
            let response = request
                .uri()
                .path()
                .strip_prefix("/artwork/")
                .filter(|id| {
                    !id.is_empty()
                        && id
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                })
                .and_then(|id| core.artwork_path(id).ok().flatten())
                .map(|path| file_response(&path, &request))
                .unwrap_or_else(|| failure(StatusCode::NOT_FOUND));
            responder.respond(response);
        });
    });
}

pub fn media_url(id: &str) -> String {
    format!("/media/{id}")
}

#[derive(Clone)]
enum PreparedAsset {
    Remote(Arc<yt_media::PreparedVideo>),
    Local(Arc<yt_media::PreparedLocalVideo>),
}
impl PreparedAsset {
    fn path(&self) -> &Path {
        match self {
            Self::Remote(video) => video.path(),
            Self::Local(video) => video.path(),
        }
    }
}
static PREPARED_VIDEO: Mutex<Option<(String, PreparedAsset)>> = Mutex::new(None);

/// Only complete, application-owned media can enter this ephemeral registry.
/// Replacing or clearing it drops the previous temporary file after in-flight
/// range requests release their own reference.
pub fn register_prepared_video(video: Arc<yt_media::PreparedVideo>) -> String {
    register_prepared(PreparedAsset::Remote(video))
}

pub fn register_prepared_local_video(video: Arc<yt_media::PreparedLocalVideo>) -> String {
    register_prepared(PreparedAsset::Local(video))
}

fn register_prepared(video: PreparedAsset) -> String {
    let token = format!("{:032x}", rand::random::<u128>());
    let url = format!("/preview/{token}");
    *PREPARED_VIDEO.lock().unwrap_or_else(|e| e.into_inner()) = Some((token, video));
    url
}

pub fn clear_prepared_video() {
    *PREPARED_VIDEO.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn preview_response(request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let entry = PREPARED_VIDEO
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match entry {
        Some((token, video)) => preview_file_response(&token, video.path(), request),
        None => failure(StatusCode::NOT_FOUND),
    }
}

fn preview_file_response(
    token: &str,
    path: &Path,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    if request.uri().path().strip_prefix("/preview/") != Some(token)
        || request.uri().query().is_some()
    {
        return failure(StatusCode::NOT_FOUND);
    }
    file_response(path, request)
}

/// A validated caption source for a native HTML video track element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSubtitleTrack {
    pub src: String,
    pub language: String,
    pub label: String,
    pub default: bool,
}
const MAX_SUBTITLE_BYTES: u64 = 8 * 1024 * 1024;
fn valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn registered_subtitles(core: &AppCore, id: &str) -> Option<(serde_json::Value, Vec<PathBuf>)> {
    if !valid_identifier(id) || core.track(id).is_err() {
        return None;
    }
    let saved = core
        .setting(&format!("rust.download.{id}"))
        .ok()
        .flatten()?;
    if saved["subtitles"].as_array()?.len() > 256 {
        return None;
    }
    let paths = serde_json::from_value(saved["subtitles"].clone()).ok()?;
    Some((saved, paths))
}
fn validated_subtitle_path(core: &AppCore, path: &Path) -> Option<PathBuf> {
    let library = std::fs::canonicalize(core.library_dir()).ok()?;
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.starts_with(&library)
        || !canonical.extension()?.to_str()?.eq_ignore_ascii_case("vtt")
    {
        return None;
    }
    let mut file = File::open(&canonical).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SUBTITLE_BYTES {
        return None;
    }
    let mut prefix = [0; 9];
    let count = file.read(&mut prefix).ok()?;
    let prefix = &prefix[..count];
    let prefix = prefix.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(prefix);
    if !prefix.starts_with(b"WEBVTT")
        || (prefix.len() > 6 && !matches!(prefix[6], b' ' | b'\t' | b'\r' | b'\n'))
    {
        return None;
    }
    Some(canonical)
}
pub fn subtitle_tracks(core: &AppCore, id: &str) -> Vec<LocalSubtitleTrack> {
    let Some((saved, paths)) = registered_subtitles(core, id) else {
        return Vec::new();
    };
    let preferred = saved
        .pointer("/preferences/language")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("es");
    let mut tracks: Vec<_> = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let path = validated_subtitle_path(core, path)?;
            let stem = path.file_stem()?.to_str()?;
            let language = stem
                .rsplit_once('.')
                .map(|(_, l)| l)
                .filter(|l| {
                    !l.is_empty()
                        && l.len() <= 32
                        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
                .unwrap_or("und")
                .to_owned();
            let label = match language.as_str() {
                "es" => "Español",
                "en" => "English",
                "pt" => "Português",
                "fr" => "Français",
                "de" => "Deutsch",
                "ja" => "日本語",
                _ => &language,
            }
            .to_owned();
            Some(LocalSubtitleTrack {
                src: format!("/subtitle/{id}/{index}"),
                language,
                label,
                default: false,
            })
        })
        .collect();
    if let Some(index) = tracks
        .iter()
        .position(|t| {
            t.language.eq_ignore_ascii_case(preferred)
                || t.language
                    .split_once('-')
                    .is_some_and(|(base, _)| base.eq_ignore_ascii_case(preferred))
        })
        .or_else(|| (!tracks.is_empty()).then_some(0))
    {
        tracks[index].default = true;
    }
    tracks
}
fn subtitle_response(core: &AppCore, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != "GET" && request.method() != "HEAD" {
        return failure(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(tail) = request.uri().path().strip_prefix("/subtitle/") else {
        return failure(StatusCode::NOT_FOUND);
    };
    let mut parts = tail.split('/');
    let Some(id) = parts.next() else {
        return failure(StatusCode::NOT_FOUND);
    };
    let Some(index) = parts
        .next()
        .filter(|s| !s.is_empty() && s.len() <= 10 && s.bytes().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok())
    else {
        return failure(StatusCode::NOT_FOUND);
    };
    if parts.next().is_some() {
        return failure(StatusCode::NOT_FOUND);
    }
    let Some((_, paths)) = registered_subtitles(core, id) else {
        return failure(StatusCode::NOT_FOUND);
    };
    let Some(path) = paths
        .get(index)
        .and_then(|path| validated_subtitle_path(core, path))
    else {
        return failure(StatusCode::NOT_FOUND);
    };
    let Ok(mut file) = File::open(path) else {
        return failure(StatusCode::NOT_FOUND);
    };
    let Ok(metadata) = file.metadata() else {
        return failure(StatusCode::NOT_FOUND);
    };
    if !metadata.is_file() || metadata.len() > MAX_SUBTITLE_BYTES {
        return failure(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut body = Vec::new();
    if request.method() != "HEAD" {
        if file
            .by_ref()
            .take(MAX_SUBTITLE_BYTES + 1)
            .read_to_end(&mut body)
            .is_err()
        {
            return failure(StatusCode::INTERNAL_SERVER_ERROR);
        }
        if body.len() as u64 > MAX_SUBTITLE_BYTES {
            return failure(StatusCode::PAYLOAD_TOO_LARGE);
        }
        if body.len() as u64 != metadata.len() {
            return failure(StatusCode::INTERNAL_SERVER_ERROR);
        }
        if std::str::from_utf8(&body).is_err() {
            return failure(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/vtt; charset=utf-8")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "private, no-cache")
        .header("Content-Length", metadata.len().to_string())
        .body(body)
        .expect("valid subtitle response")
}

fn failure(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .body(Vec::new())
        .expect("valid status")
}

fn media_response(core: &AppCore, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != "GET" && request.method() != "HEAD" {
        return failure(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(id) = request.uri().path().strip_prefix("/media/") else {
        return failure(StatusCode::NOT_FOUND);
    };
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return failure(StatusCode::NOT_FOUND);
    }
    let Ok(track) = core.track(id) else {
        return failure(StatusCode::NOT_FOUND);
    };
    file_response(&track.path, request)
}

fn mime_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "mkv" => Some("video/x-matroska"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "wav" => Some("audio/wav"),
        "flac" => Some("audio/flac"),
        "aac" => Some("audio/aac"),
        "ogg" | "opus" => Some("audio/ogg"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn parse_range(header: &str, length: u64) -> Option<(u64, u64)> {
    if length == 0 {
        return None;
    }
    let value = header.strip_prefix("bytes=")?;
    if value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        return (suffix > 0).then(|| (length.saturating_sub(suffix), length - 1));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end && start < length).then_some((start, end))
}

fn file_response(path: &Path, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != "GET" && request.method() != "HEAD" {
        return failure(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(mime) = mime_type(path) else {
        return failure(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    };
    let Ok(mut file) = File::open(path) else {
        return failure(StatusCode::NOT_FOUND);
    };
    let Ok(metadata) = file.metadata() else {
        return failure(StatusCode::NOT_FOUND);
    };
    if !metadata.is_file() {
        return failure(StatusCode::NOT_FOUND);
    }
    let length = metadata.len();
    let range = request.headers().get("Range");
    let (status, start, end) = if let Some(range) = range {
        let Some((start, end)) = range.to_str().ok().and_then(|s| parse_range(s, length)) else {
            return Response::builder()
                .status(416)
                .header("Content-Range", format!("bytes */{length}"))
                .body(Vec::new())
                .expect("valid range response");
        };
        // WebKit requests open-ended ranges. Small valid partial responses keep
        // playback memory bounded; the next range continues from this endpoint.
        (
            StatusCode::PARTIAL_CONTENT,
            start,
            end.min(start.saturating_add(8 * 1024 * 1024 - 1)),
        )
    } else {
        (StatusCode::OK, 0, length.saturating_sub(1))
    };
    let count = if length == 0 { 0 } else { end - start + 1 };
    if request.method() != "HEAD" && count > 512 * 1024 * 1024 {
        return failure(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", count.to_string())
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "private, no-cache");
    if range.is_some() {
        builder = builder.header("Content-Range", format!("bytes {start}-{end}/{length}"));
    }
    let mut bytes = Vec::new();
    if request.method() != "HEAD" {
        if file.seek(SeekFrom::Start(start)).is_err()
            || file.take(count).read_to_end(&mut bytes).is_err()
        {
            return failure(StatusCode::INTERNAL_SERVER_ERROR);
        }
        if bytes.len() as u64 != count {
            return failure(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    builder.body(bytes).expect("valid media response")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_video_endpoint_accepts_only_the_registered_token_and_ranges() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        let request = Request::builder()
            .uri(format!("/preview/{token}"))
            .header("Range", "bytes=2-4")
            .body(Vec::new())
            .unwrap();
        let response = preview_file_response(token, &path, &request);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body(), b"234");
        assert_eq!(response.headers()["Content-Type"], "video/mp4");
        for uri in [
            "/preview/other".to_owned(),
            format!("/preview/{token}/extra"),
            format!("/preview/{token}?path=/other.mp4"),
            "/preview/../other.mp4".to_owned(),
        ] {
            let request = Request::builder().uri(uri).body(Vec::new()).unwrap();
            assert_eq!(
                preview_file_response(token, &path, &request).status(),
                StatusCode::NOT_FOUND
            );
        }
        let request = Request::builder()
            .method("POST")
            .uri(format!("/preview/{token}"))
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            preview_file_response(token, &path, &request).status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    #[test]
    fn bounded_open_and_suffix_ranges() {
        assert_eq!(parse_range("bytes=2-4", 10), Some((2, 4)));
        assert_eq!(parse_range("bytes=8-", 10), Some((8, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        for invalid in [
            "bytes=10-",
            "bytes=7-2",
            "bytes=-0",
            "bytes=0-1,3-4",
            "items=0-4",
        ] {
            assert_eq!(parse_range(invalid, 10), None);
        }
    }
    #[test]
    fn media_ranges_return_exact_bytes_and_head_has_no_body() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.mp4");
        std::fs::write(&file, b"0123456789").unwrap();
        let request = Request::builder()
            .header("Range", "bytes=2-5")
            .body(vec![])
            .unwrap();
        let response = file_response(&file, &request);
        assert_eq!(response.status(), 206);
        assert_eq!(response.body(), b"2345");
        assert_eq!(response.headers()["Content-Range"], "bytes 2-5/10");
        let head = file_response(
            &file,
            &Request::builder().method("HEAD").body(vec![]).unwrap(),
        );
        assert_eq!(head.headers()["Content-Length"], "10");
        assert!(head.body().is_empty());
    }
    #[test]
    fn unregistered_paths_and_other_methods_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let core = AppCore::open(dir.path()).unwrap();
        for uri in [
            "/media/../../etc/passwd",
            "/media/unknown",
            "/media/a%2fb",
            "/elsewhere",
        ] {
            let response =
                media_response(&core, &Request::builder().uri(uri).body(vec![]).unwrap());
            assert_eq!(response.status(), 404);
        }
        let response = media_response(
            &core,
            &Request::builder()
                .method("POST")
                .uri("/media/id")
                .body(vec![])
                .unwrap(),
        );
        assert_eq!(response.status(), 405);
    }
    fn subtitle_fixture() -> (tempfile::TempDir, AppCore, String) {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        std::fs::create_dir_all(core.library_dir()).unwrap();
        let path = core.library_dir().join("movie.mp4");
        std::fs::write(&path, b"fixture media").unwrap();
        let track = core
            .import_file(&path, yt_core::ImportMode::Reference)
            .unwrap();
        (profile, core, track.id)
    }
    #[test]
    fn registered_subtitles_serve_vtt_with_safe_headers_and_language_selection() {
        let (_profile, core, id) = subtitle_fixture();
        let es = core.library_dir().join("movie.es.vtt");
        let en = core.library_dir().join("movie.en.vtt");
        let spanish = b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHola\n";
        let english = b"WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHello\n";
        std::fs::write(&es, spanish).unwrap();
        std::fs::write(&en, english).unwrap();
        core.set_setting(
            &format!("rust.download.{id}"),
            &serde_json::json!({"preferences":{"language":"en"},"subtitles":[es,en]}),
        )
        .unwrap();
        let tracks = subtitle_tracks(&core, &id);
        assert_eq!(tracks.len(), 2);
        assert!(!tracks[0].default);
        assert!(tracks[1].default);
        assert_eq!(tracks[1].language, "en");
        let request = Request::builder().uri(&tracks[1].src).body(vec![]).unwrap();
        let response = subtitle_response(&core, &request);
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), english);
        assert_eq!(
            response.headers()["Content-Type"],
            "text/vtt; charset=utf-8"
        );
        assert_eq!(response.headers()["X-Content-Type-Options"], "nosniff");
        let response = subtitle_response(
            &core,
            &Request::builder()
                .method("HEAD")
                .uri(&tracks[0].src)
                .body(vec![])
                .unwrap(),
        );
        assert!(response.body().is_empty());
        assert_eq!(
            response.headers()["Content-Length"],
            spanish.len().to_string()
        );
        assert_eq!(
            subtitle_response(
                &core,
                &Request::builder()
                    .method("POST")
                    .uri(&tracks[0].src)
                    .body(vec![])
                    .unwrap()
            )
            .status(),
            405
        );
    }
    #[test]
    fn subtitle_endpoint_rejects_arbitrary_paths_bad_content_and_oversized_files() {
        let (profile, core, id) = subtitle_fixture();
        let external = profile.path().join("secret.vtt");
        std::fs::write(&external, b"WEBVTT\nsecret").unwrap();
        let invalid = core.library_dir().join("invalid.vtt");
        std::fs::write(&invalid, b"<html>not captions</html>").unwrap();
        let large = core.library_dir().join("large.vtt");
        std::fs::write(&large, b"WEBVTT\n").unwrap();
        File::options()
            .write(true)
            .open(&large)
            .unwrap()
            .set_len(MAX_SUBTITLE_BYTES + 1)
            .unwrap();
        core.set_setting(
            &format!("rust.download.{id}"),
            &serde_json::json!({"subtitles":[external,invalid,large]}),
        )
        .unwrap();
        for suffix in ["0", "1", "2", "999", "../0", "0/else", "%2e%2e", "-1"] {
            let response = subtitle_response(
                &core,
                &Request::builder()
                    .uri(format!("/subtitle/{id}/{suffix}"))
                    .body(vec![])
                    .unwrap(),
            );
            assert_eq!(response.status(), 404, "{suffix}");
            assert!(response.body().is_empty());
        }
        assert!(subtitle_tracks(&core, &id).is_empty());
        #[cfg(unix)]
        {
            let linked = core.library_dir().join("escape.es.vtt");
            std::os::unix::fs::symlink(&external, &linked).unwrap();
            core.set_setting(
                &format!("rust.download.{id}"),
                &serde_json::json!({"subtitles":[linked]}),
            )
            .unwrap();
            assert_eq!(
                subtitle_response(
                    &core,
                    &Request::builder()
                        .uri(format!("/subtitle/{id}/0"))
                        .body(vec![])
                        .unwrap()
                )
                .status(),
                404
            );
        }
    }
}
