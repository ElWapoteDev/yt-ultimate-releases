use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use yt_media::{CancellationToken, Downloader, MediaError, ResumeKey};
struct RangeServer {
    url: String,
    requests: Arc<Mutex<Vec<u64>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl RangeServer {
    fn new(bytes: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/file.m4a", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = [0; 4096];
                let n = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
                let Some((a, b)) = request
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|v| v.split_once('-'))
                else {
                    continue;
                };
                let start = a.parse::<usize>().unwrap();
                let end = b.parse::<usize>().unwrap();
                recorded.lock().unwrap().push(start as u64);
                let header=format!("HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mp4\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",end-start+1,start,end,bytes.len());
                let _ = stream
                    .write_all(header.as_bytes())
                    .and_then(|_| stream.write_all(&bytes[start..=end]));
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for RangeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn key() -> ResumeKey {
    ResumeKey {
        media_id: "fixture".into(),
        itag: 140,
        audio_track_id: None,
        dynamic_range_compressed: false,
        total_bytes: 2 * 1024 * 1024 + 123,
    }
}
async fn pause(server: &RangeServer, destination: &std::path::Path) {
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let result = Downloader::new()
        .unwrap()
        .download_resumable(&server.url, destination, key(), cancel, move |p| {
            if p.downloaded_bytes >= 1024 * 1024 {
                stop.cancel();
            }
        })
        .await;
    assert!(matches!(result, Err(MediaError::Cancelled)), "{result:?}");
    assert!(!destination.exists());
}
#[tokio::test]
async fn interrupted_job_resumes_after_restart_and_recovers_uncommitted_tail() {
    let bytes: Vec<_> = (0..key().total_bytes).map(|i| (i % 251) as u8).collect();
    let server = RangeServer::new(bytes.clone());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("song.m4a");
    pause(&server, &output).await;
    let downloader = Downloader::new().unwrap();
    let status = downloader.resume_status(&output).unwrap().unwrap();
    assert_eq!(status.completed_bytes, 1024 * 1024);
    let mut partial = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(".song.m4a.ytu-part"))
        .unwrap();
    partial.write_all(b"uncommitted crash tail").unwrap();
    drop(partial);
    let receipt = downloader
        .download_resumable(
            &server.url,
            &output,
            key(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(receipt.bytes, key().total_bytes);
    assert_eq!(std::fs::read(output).unwrap(), bytes);
    assert_eq!(
        *server.requests.lock().unwrap(),
        vec![0, 1024 * 1024, 2 * 1024 * 1024]
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
#[tokio::test]
async fn corrupt_or_mismatched_partials_are_never_published() {
    let server = RangeServer::new(vec![42; key().total_bytes as usize]);
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("song.m4a");
    pause(&server, &output).await;
    let downloader = Downloader::new().unwrap();
    let mut wrong = key();
    wrong.itag = 141;
    assert!(downloader
        .download_resumable(
            &server.url,
            &output,
            wrong.clone(),
            CancellationToken::new(),
            |_| {}
        )
        .await
        .is_err());
    assert!(downloader.discard_resume(&output, &wrong).is_err());
    let part = dir.path().join(".song.m4a.ytu-part");
    let mut file = std::fs::OpenOptions::new().write(true).open(&part).unwrap();
    file.write_all(b"corrupted").unwrap();
    drop(file);
    assert!(matches!(
        downloader
            .download_resumable(
                &server.url,
                &output,
                key(),
                CancellationToken::new(),
                |_| {}
            )
            .await,
        Err(MediaError::IncompleteDownload)
    ));
    assert!(!output.exists());
    assert_eq!(*server.requests.lock().unwrap(), vec![0]);
    downloader.discard_resume(&output, &key()).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn publication_race_preserves_original_and_keeps_complete_resume_data() {
    let bytes = vec![17; key().total_bytes as usize];
    let server = RangeServer::new(bytes.clone());
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("song.m4a");
    let other = output.clone();
    let total = key().total_bytes;
    let downloader = Downloader::new().unwrap();
    let result = downloader
        .download_resumable(
            &server.url,
            &output,
            key(),
            CancellationToken::new(),
            move |p| {
                if p.downloaded_bytes == total {
                    std::fs::write(&other, b"user original").unwrap();
                }
            },
        )
        .await;
    assert!(matches!(result, Err(MediaError::DestinationExists)));
    assert_eq!(std::fs::read(&output).unwrap(), b"user original");
    assert_eq!(
        downloader
            .resume_status(&output)
            .unwrap()
            .unwrap()
            .completed_bytes,
        total
    );
    std::fs::remove_file(&output).unwrap();
    let count = server.requests.lock().unwrap().len();
    downloader
        .download_resumable(
            &server.url,
            &output,
            key(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    assert_eq!(server.requests.lock().unwrap().len(), count);
}
