use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};
use yt_media::{CancellationToken, Downloader, MediaError};

fn server(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(&response);
    });
    format!("http://{address}/audio.m4a")
}

#[tokio::test]
async fn download_is_complete_and_never_overwrites_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("song.m4a");
    let url = server(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mp4\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef".to_vec());
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let downloader = Downloader::new().unwrap();
    let result = downloader
        .download(
            &url,
            &destination,
            CancellationToken::new(),
            move |progress| captured.lock().unwrap().push(progress),
        )
        .await
        .unwrap();
    assert_eq!(result.bytes, 6);
    assert_eq!(std::fs::read(&destination).unwrap(), b"abcdef");
    assert_eq!(events.lock().unwrap().last().unwrap().downloaded_bytes, 6);
    let repeated = downloader
        .download(&url, &destination, CancellationToken::new(), |_| {})
        .await;
    assert!(matches!(repeated, Err(MediaError::DestinationExists)));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn cancellation_cleans_partial_files() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("song.m4a");
    let url = server(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mp4\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef".to_vec());
    let cancel = CancellationToken::new();
    let callback_cancel = cancel.clone();
    let result = Downloader::new()
        .unwrap()
        .download(&url, &destination, cancel, move |progress| {
            if progress.downloaded_bytes > 0 {
                callback_cancel.cancel();
            }
        })
        .await;
    assert!(matches!(result, Err(MediaError::Cancelled)));
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn interrupted_or_html_responses_do_not_become_library_files() {
    for response in [
        b"HTTP/1.1 200 OK\r\nContent-Type: audio/mp4\r\nContent-Length: 500\r\nConnection: close\r\n\r\nshort".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror".to_vec(),
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("song.m4a");
        let url = server(response);
        let result = Downloader::new().unwrap().download(&url, &destination, CancellationToken::new(), |_| {}).await;
        assert!(result.is_err());
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn cancelled_download_does_not_create_destination_directory() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = Downloader::new()
        .unwrap()
        .download(
            "https://example.com/file",
            &dir.path().join("new/song"),
            cancel,
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(MediaError::Cancelled)));
    assert!(!dir.path().join("new").exists());
}

#[tokio::test]
async fn joins_validated_byte_ranges_and_uses_total_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for (expected, body, content_range) in [
            ("range: bytes=0-1048575", "abc", "bytes 0-2/6"),
            ("range: bytes=3-5", "def", "bytes 3-5/6"),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let count = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..count])
                .to_lowercase()
                .contains(expected));
            write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mp4\r\nContent-Length: 3\r\nContent-Range: {content_range}\r\nConnection: close\r\n\r\n{body}").unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("ranged.m4a");
    let result = Downloader::new()
        .unwrap()
        .download(
            &format!("http://{address}/media"),
            &target,
            CancellationToken::new(),
            |progress| assert_eq!(progress.total_bytes, Some(6)),
        )
        .await
        .unwrap();
    assert_eq!(result.bytes, 6);
    assert_eq!(std::fs::read(target).unwrap(), b"abcdef");
}

#[tokio::test]
async fn refuses_mismatched_content_ranges() {
    let url = server(b"HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mp4\r\nContent-Length: 3\r\nContent-Range: bytes 1-3/6\r\nConnection: close\r\n\r\nabc".to_vec());
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("ranged.m4a");
    let result = Downloader::new()
        .unwrap()
        .download(&url, &target, CancellationToken::new(), |_| {})
        .await;
    assert!(matches!(result, Err(MediaError::IncompleteDownload)));
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
