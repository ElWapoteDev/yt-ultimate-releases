//! A helper mode of the same Rust executable waits for the UI process to exit.
//! No installer runs while the process still holds its handoff lock.
use crate::{Edition, Result, UpdateError, UpdateInfo, Updater, VerifiedUpdate};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const HELPER_ARGUMENT: &str = "--yt-apply-update";

/// Keep this alive until the application exits. Dropping it permits installation.
/// It owns no user data and never deletes the downloaded installer.
pub struct InstallGuard {
    _lock: File,
    child: std::process::Child,
}

impl InstallGuard {
    /// Revoke a handoff before closing, for example if a new job started.
    pub fn abort(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Serialize, Deserialize)]
struct Handoff {
    current_version: String,
    repository: String,
    edition: Edition,
    cache: PathBuf,
    executable: PathBuf,
    installer: PathBuf,
    update: UpdateInfo,
    restart: bool,
    parent_pid: u32,
}

impl Updater {
    /// Verify again and start an acknowledged helper. The caller must retain the
    /// guard, stop its jobs gracefully, and close its UI before installation runs.
    pub fn handoff_install(&self, update: &VerifiedUpdate, restart: bool) -> Result<InstallGuard> {
        self.validate_info(&update.info)?;
        self.verify_cached(&update.path, &update.info)?;
        let executable = std::env::current_exe()?.canonicalize()?;
        #[cfg(target_os = "macos")]
        macos::installed_bundle(&executable)?;
        let directory = tempfile::Builder::new()
            .prefix("install-")
            .tempdir_in(&self.cache_dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.path().join("parent.lock"))?;
        lock.try_lock()
            .map_err(|e| UpdateError::Installation(e.to_string()))?;
        let handoff = Handoff {
            current_version: self.current.to_string(),
            repository: self.repository.clone(),
            edition: self.edition,
            cache: self.cache_dir.clone(),
            executable: executable.clone(),
            installer: update.path.clone(),
            update: update.info.clone(),
            restart,
            parent_pid: std::process::id(),
        };
        let manifest = directory.path().join("install.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)?;
        serde_json::to_writer(&mut file, &handoff)
            .map_err(|e| UpdateError::Installation(e.to_string()))?;
        file.sync_all()?;
        let mut child = Command::new(executable)
            .arg(HELPER_ARGUMENT)
            .arg(&manifest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if directory.path().join("ready").is_file() {
                // The helper removes only its own transaction after completion.
                let _ = directory.keep();
                return Ok(InstallGuard { _lock: lock, child });
            }
            if child.try_wait()?.is_some() || Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(UpdateError::Installation(
                    "El asistente de instalación no pudo iniciarse; la aplicación sigue abierta"
                        .into(),
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Call before initializing the UI or single-instance handling in `main`.
/// Returns `None` during ordinary launches, or the helper's process exit code.
pub fn handle_update_arguments() -> Option<i32> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARGUMENT)) {
        return None;
    }
    let Some(path) = arguments.next() else {
        return Some(2);
    };
    if arguments.next().is_some() {
        return Some(2);
    }
    Some(match run_helper(Path::new(&path)) {
        Ok(()) => 0,
        Err(_) => 1,
    })
}

fn run_helper(manifest: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(manifest)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 32 * 1024 {
        return Err(UpdateError::Integrity);
    }
    let handoff: Handoff = serde_json::from_reader(File::open(manifest)?)
        .map_err(|e| UpdateError::Installation(e.to_string()))?;
    let directory = manifest
        .parent()
        .ok_or(UpdateError::Integrity)?
        .canonicalize()?;
    let cache = handoff.cache.canonicalize()?;
    if directory.parent() != Some(cache.as_path())
        || !directory
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("install-"))
        || manifest.file_name().and_then(|n| n.to_str()) != Some("install.json")
        || std::env::current_exe()?.canonicalize()? != handoff.executable
    {
        return Err(UpdateError::Integrity);
    }
    let updater = Updater::with_repository(
        &handoff.current_version,
        &cache,
        &handoff.repository,
        handoff.edition,
    )?;
    updater.validate_info(&handoff.update)?;
    let lock_path = directory.join("parent.lock");
    if std::fs::symlink_metadata(&lock_path)?
        .file_type()
        .is_symlink()
    {
        return Err(UpdateError::Integrity);
    }
    let lock = OpenOptions::new().read(true).write(true).open(lock_path)?;
    File::create(directory.join("ready"))?.sync_all()?;
    // Scope teardown can drop the guard just before process termination. Wait
    // for the actual parent process as well, so no native worker is still alive.
    let parent_exited = wait_for_parent(&lock, Duration::from_secs(300))
        .and_then(|()| wait_for_parent_process(handoff.parent_pid, Duration::from_secs(300)));
    let can_restart = parent_exited.is_ok();
    let result = parent_exited.and_then(|()| {
        // The application has exited. Recheck both identity and payload after the wait.
        updater.verify_cached(&handoff.installer, &handoff.update)?;
        install(&handoff)
    });
    log_result(&cache, &handoff.update.version, &result);
    if result.is_err() && can_restart {
        // Installation failures restore/reopen the existing application. The
        // helper argument is not forwarded, so this is a normal UI launch.
        let _ = Command::new(&handoff.executable).spawn();
    }
    drop(lock);
    let _ = std::fs::remove_dir_all(&directory);
    result
}

fn wait_for_parent_process(pid: u32, timeout: Duration) -> Result<()> {
    if pid == 0 || pid == std::process::id() {
        return Err(UpdateError::Integrity);
    }
    let deadline = Instant::now() + timeout;
    loop {
        if !process_alive(pid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(UpdateError::Installation(
                "El proceso de la aplicación sigue activo".into(),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> Result<bool> {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let pid = i32::try_from(pid).map_err(|_| UpdateError::Integrity)?;
    // SAFETY: signal zero only queries existence; it never sends a signal.
    if unsafe { kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(3) {
        Ok(false)
    } else {
        Err(error.into())
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> Result<bool> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    // SAFETY: SYNCHRONIZE grants observation only; handles are always closed.
    let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(87) {
            Ok(false)
        } else {
            Err(error.into())
        };
    }
    let status = unsafe { WaitForSingleObject(handle, 0) };
    let error = std::io::Error::last_os_error();
    unsafe {
        CloseHandle(handle);
    }
    match status {
        0 => Ok(false),
        0x102 => Ok(true),
        _ => Err(error.into()),
    }
}

fn wait_for_parent(lock: &File, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(UpdateError::Installation(
                    "La aplicación no terminó a tiempo".into(),
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn install(handoff: &Handoff) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new(&handoff.installer);
        command.arg("/S");
        if handoff.restart {
            command.arg("/R");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        macos::install(handoff)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = handoff;
        Err(UpdateError::UnsupportedPlatform)
    }
}

fn log_result(cache: &Path, version: &str, result: &Result<()>) {
    let message = match result {
        Ok(()) => format!("Instalación de la versión {version} iniciada correctamente"),
        Err(error) => format!(
            "No se instaló la versión {version}: {error}. Se conservó la aplicación anterior."
        ),
    };
    if let Ok(mut file) = tempfile::NamedTempFile::new_in(cache) {
        if serde_json::to_writer(
            &mut file,
            &serde_json::json!({"message":message,"success":result.is_ok()}),
        )
        .is_ok()
            && file.as_file().sync_all().is_ok()
        {
            let _ = file.persist(cache.join("last-install.json"));
        }
    }
    if let Ok(mut log) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(cache.join("updater.log"))
    {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Never serialize the manifest, signed URLs, browser/session data, or paths.
        let outcome = if result.is_ok() {
            "installer-handoff-completed"
        } else {
            "installation-failed; previous application reopened"
        };
        let _ = writeln!(log, "{seconds} version={version} {outcome}");
    }
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_waits_until_parent_lock_is_released() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lock");
        let parent = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        parent.try_lock().unwrap();
        let child = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        assert!(wait_for_parent(&child, Duration::ZERO).is_err());
        drop(parent);
        wait_for_parent(&child, Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn helper_rejects_manifest_symlink_before_reading() {
        #[cfg(unix)]
        {
            let directory = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink("missing", directory.path().join("install.json")).unwrap();
            assert!(matches!(
                run_helper(&directory.path().join("install.json")),
                Err(UpdateError::Integrity)
            ));
        }
    }

    #[test]
    fn process_wait_never_treats_a_live_process_as_finished() {
        assert!(process_alive(std::process::id()).unwrap());
        assert!(wait_for_parent_process(std::process::id(), Duration::ZERO).is_err());
    }
}
