//! Per-profile OS lock with an atomic, local-only second-launch mailbox.
//! The lock file is never unlinked: replacing a locked inode would permit two
//! primaries. The OS releases the lock after either normal exit or a crash.
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_MESSAGE: u64 = 1024 * 1024;
const MAX_FILES: usize = 256;
const MAX_REQUESTS: usize = 128;
const LIFETIME: Duration = Duration::from_secs(60);

pub(crate) struct InstanceGuard {
    _lock: File,
}

pub(crate) fn acquire(profile: &Path) -> io::Result<Option<InstanceGuard>> {
    acquire_with_paths(
        profile,
        &std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
    )
}

fn acquire_with_paths(profile: &Path, paths: &[PathBuf]) -> io::Result<Option<InstanceGuard>> {
    fs::create_dir_all(profile)?;
    let mailbox = directory(profile);
    fs::create_dir_all(&mailbox)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700))?;
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(mailbox.join("primary.lock"))?;
    match lock.try_lock() {
        Ok(()) => {
            atomic_write(
                &mailbox,
                "owner.json",
                &serde_json::to_vec(&std::process::id())?,
            )?;
            Ok(Some(InstanceGuard { _lock: lock }))
        }
        Err(TryLockError::WouldBlock) => {
            send(&mailbox, paths)?;
            permit_foreground(&mailbox);
            Ok(None)
        }
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn directory(profile: &Path) -> PathBuf {
    profile.join(".instance")
}

fn atomic_write(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary
        .persist(directory.join(name))
        .map_err(|error| error.error)?;
    Ok(())
}

fn send(mailbox: &Path, paths: &[PathBuf]) -> io::Result<()> {
    if fs::read_dir(mailbox)?
        .filter_map(Result::ok)
        .filter(|entry| is_request(&entry.file_name()))
        .count()
        >= MAX_REQUESTS
    {
        return Err(io::Error::other(
            "La instancia abierta tiene demasiadas solicitudes pendientes",
        ));
    }
    let paths = paths
        .iter()
        .take(MAX_FILES)
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({"created": now_seconds(), "paths": paths}))?;
    if bytes.len() as u64 > MAX_MESSAGE {
        return Err(io::Error::other(
            "La solicitud de archivos es demasiado grande",
        ));
    }
    let name = format!(
        "request-{}-{:032x}.json",
        std::process::id(),
        rand::random::<u128>()
    );
    atomic_write(mailbox, &name, &bytes)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn is_request(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with("request-") && name.ends_with(".json"))
}

/// Called on a blocking worker; empty vectors still request a window activation.
pub(crate) fn take_requests(profile: &Path) -> io::Result<Vec<Vec<PathBuf>>> {
    let mut requests = Vec::new();
    for entry in fs::read_dir(directory(profile))?.take(MAX_REQUESTS + 16) {
        let entry = entry?;
        if !is_request(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let parsed = read_request(&path);
        // Consume only our protocol messages; never unlink the lock/owner or a
        // user file elsewhere. A symlink is rejected and only the link removed.
        fs::remove_file(&path)?;
        if let Ok(Some(paths)) = parsed {
            requests.push(paths);
        }
    }
    Ok(requests)
}

fn read_request(path: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_MESSAGE {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MESSAGE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MESSAGE {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let Some(created) = value["created"].as_u64() else {
        return Ok(None);
    };
    let now = now_seconds();
    if created > now + 5 || now.saturating_sub(created) > LIFETIME.as_secs() {
        return Ok(None);
    }
    let Some(paths) = value["paths"].as_array() else {
        return Ok(None);
    };
    if paths.len() > MAX_FILES {
        return Ok(None);
    }
    let paths = paths
        .iter()
        .filter_map(|value| value.as_str())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .filter_map(|path| path.canonicalize().ok())
        .collect();
    Ok(Some(paths))
}

#[cfg(target_os = "windows")]
fn permit_foreground(mailbox: &Path) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(process: u32) -> i32;
    }
    if let Ok(bytes) = fs::read(mailbox.join("owner.json")) {
        if let Ok(process) = serde_json::from_slice::<u32>(&bytes) {
            // The explicitly launched secondary grants its foreground right to
            // the existing primary; the background process cannot grant itself.
            unsafe {
                AllowSetForegroundWindow(process);
            }
        }
    }
}
#[cfg(not(target_os = "windows"))]
fn permit_foreground(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_process_hands_off_and_crash_free_lock_release_allows_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile");
        let media = temporary.path().join("Áudio con espacios.wav");
        fs::write(&media, b"fixture").unwrap();
        let primary = acquire_with_paths(&profile, &[]).unwrap().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_instance::tests::subprocess_contender",
                "--nocapture",
            ])
            .env("YTU_INSTANCE_TEST_PROFILE", &profile)
            .env("YTU_INSTANCE_TEST_MEDIA", &media)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(
            take_requests(&profile).unwrap(),
            vec![vec![media.canonicalize().unwrap()]]
        );
        assert!(take_requests(&profile).unwrap().is_empty());
        assert!(
            acquire_with_paths(&temporary.path().join("another-profile"), &[])
                .unwrap()
                .is_some()
        );
        drop(primary);
        assert!(acquire_with_paths(&profile, &[]).unwrap().is_some());
    }

    #[test]
    fn subprocess_contender() {
        let Some(profile) = std::env::var_os("YTU_INSTANCE_TEST_PROFILE") else {
            return;
        };
        let media = PathBuf::from(std::env::var_os("YTU_INSTANCE_TEST_MEDIA").unwrap());
        assert!(acquire_with_paths(Path::new(&profile), &[media])
            .unwrap()
            .is_none());
    }

    #[test]
    fn focus_only_handoff_and_invalid_or_stale_messages_are_bounded() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path();
        let _primary = acquire_with_paths(profile, &[]).unwrap().unwrap();
        send(&directory(profile), &[]).unwrap();
        atomic_write(
            &directory(profile),
            "request-stale.json",
            br#"{"created":0,"paths":[]}"#,
        )
        .unwrap();
        atomic_write(&directory(profile), "request-invalid.json", b"bad json").unwrap();
        File::create(directory(profile).join("request-huge.json"))
            .unwrap()
            .set_len(MAX_MESSAGE + 1)
            .unwrap();
        assert_eq!(take_requests(profile).unwrap(), vec![Vec::<PathBuf>::new()]);
        assert!(directory(profile).join("primary.lock").exists());
        assert!(directory(profile).join("owner.json").exists());
    }
}
