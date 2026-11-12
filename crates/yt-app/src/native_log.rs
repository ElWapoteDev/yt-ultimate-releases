//! Bounded application event log. Disk writes never block the UI thread.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, SyncSender},
        OnceLock,
    },
};
const LIMIT: u64 = 2 * 1024 * 1024;
static WRITER: OnceLock<Option<SyncSender<(PathBuf, String)>>> = OnceLock::new();

pub(crate) fn record_log(profile: &Path, message: &str) {
    let writer = WRITER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<(PathBuf, String)>(256);
        std::thread::Builder::new()
            .name("ytu-log".into())
            .spawn(move || {
                for (profile, message) in receiver {
                    let _ = append(&profile, &message);
                }
            })
            .ok()
            .map(|_| sender)
    });
    if let Some(writer) = writer {
        // Never retain an unbounded error stream or wait behind a slow disk.
        let message = redact_urls(&message.chars().take(16_384).collect::<String>());
        let _ = writer.try_send((profile.to_owned(), message));
    }
}

fn redact_urls(mut message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    while let Some(separator) = message.find("://") {
        let start = message[..separator]
            .char_indices()
            .rev()
            .find(|(_, character)| {
                !character.is_ascii_alphanumeric() && !matches!(character, '+' | '-' | '.')
            })
            .map_or(0, |(index, character)| index + character.len_utf8());
        let end = message[separator + 3..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '\'' | '"' | '<' | '>' | '(' | ')')
            })
            .map_or(message.len(), |index| separator + 3 + index);
        output.push_str(&message[..start]);
        match url::Url::parse(&message[start..end]) {
            Ok(url) if url.host_str().is_some() => {
                output.push_str(url.scheme());
                output.push_str("://");
                output.push_str(url.host_str().unwrap_or_default());
                output.push_str("/[redacted]");
            }
            _ => output.push_str("[URL redacted]"),
        }
        message = &message[end..];
    }
    output.push_str(message);
    output
}
fn append(profile: &Path, message: &str) -> io::Result<()> {
    let directory = profile.join("Logs");
    fs::create_dir_all(&directory)?;
    let path = directory.join("application.log");
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= LIMIT) {
        let previous = directory.join("application.previous.log");
        if previous.exists() {
            fs::remove_file(previous)?;
        }
        fs::rename(&path, directory.join("application.previous.log"))?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    writeln!(
        log,
        "{timestamp} {}",
        message.replace('\r', "\\r").replace('\n', "\\n")
    )?;
    log.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotates_only_owned_log_and_preserves_neighboring_files() {
        let profile = tempfile::tempdir().unwrap();
        let logs = profile.path().join("Logs");
        fs::create_dir(&logs).unwrap();
        fs::write(logs.join("legacy.log"), "preserve").unwrap();
        let file = fs::File::create(logs.join("application.log")).unwrap();
        file.set_len(LIMIT).unwrap();
        append(profile.path(), "second\nline").unwrap();
        assert_eq!(
            fs::metadata(logs.join("application.previous.log"))
                .unwrap()
                .len(),
            LIMIT
        );
        assert!(fs::read_to_string(logs.join("application.log"))
            .unwrap()
            .contains("second\\nline"));
        assert_eq!(
            fs::read_to_string(logs.join("legacy.log")).unwrap(),
            "preserve"
        );
    }

    #[test]
    fn log_urls_never_retain_credentials_paths_queries_or_fragments() {
        let input = "proxy=https://alice:private-pass@proxy.example/private-path?token=secret-query#private-fragment; error (https://cdn.example/video/secret-id?sig=secret-signature) and socks5://bob:another-pass@localhost:1080/private";
        let clean = redact_urls(input);
        for secret in [
            "alice",
            "private-pass",
            "private-path",
            "secret-query",
            "private-fragment",
            "secret-id",
            "secret-signature",
            "bob",
            "another-pass",
            "1080/private",
        ] {
            assert!(!clean.contains(secret), "leaked {secret}");
        }
        assert!(clean.contains("https://proxy.example/[redacted]"));
        assert!(clean.contains("https://cdn.example/[redacted]"));
        assert!(clean.contains("socks5://localhost/[redacted]"));
        assert_eq!(
            redact_urls("Plain error without links"),
            "Plain error without links"
        );
        assert!(!redact_urls("bad https://invalid/%secret").contains("%secret"));
    }
}
