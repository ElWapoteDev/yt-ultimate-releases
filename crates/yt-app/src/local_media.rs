use dioxus::desktop::{
    use_asset_handler,
    wry::http::{Request, Response, StatusCode},
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use yt_core::AppCore;

pub fn use_local_media(core: AppCore) {
    let artwork_core = core.clone();
    use_asset_handler("media", move |request, responder| {
        let core = core.clone();
        tokio::task::spawn_blocking(move || {
            responder.respond(media_response(&core, &request));
        });
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
}
