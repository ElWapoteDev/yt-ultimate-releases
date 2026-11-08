use super::*;
use core_foundation::url::CFURL;
use security_framework::os::macos::code_signing::{Flags, SecRequirement, SecStaticCode};
use std::os::unix::fs::{symlink, PermissionsExt};

const BUNDLE_ID: &str = "com.ytultimate.desktop";
const MAX_BUNDLE_BYTES: u64 = 12 * 1024 * 1024 * 1024;
const MAX_BUNDLE_ENTRIES: usize = 100_000;

pub(super) fn installed_bundle(executable: &Path) -> Result<PathBuf> {
    let contents = executable.parent().filter(|p| p.file_name().is_some_and(|n| n == "MacOS"))
        .and_then(Path::parent).filter(|p| p.file_name().is_some_and(|n| n == "Contents"))
        .ok_or_else(|| UpdateError::Installation("La actualización automática requiere ejecutar la aplicación instalada en una carpeta .app".into()))?;
    let bundle = contents
        .parent()
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or(UpdateError::Integrity)?;
    bundle_info(bundle, None)?;
    Ok(bundle.to_path_buf())
}

pub(super) fn install(handoff: &Handoff) -> Result<()> {
    let target = installed_bundle(&handoff.executable)?;
    let mount = tempfile::Builder::new()
        .prefix("mounted-")
        .tempdir_in(&handoff.cache)?;
    let mut image = MountedImage {
        path: mount.path().to_path_buf(),
        attached: false,
    };
    let mut attach = Command::new("/usr/bin/hdiutil");
    attach
        .args([
            "attach",
            "-readonly",
            "-nobrowse",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(mount.path())
        .arg(&handoff.installer);
    run_bounded(&mut attach, Duration::from_secs(120))?;
    image.attached = true;
    let apps = std::fs::read_dir(mount.path())?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "app"))
        .collect::<Vec<_>>();
    if apps.len() != 1 {
        return Err(UpdateError::Installation(
            "El instalador debe contener una sola aplicación".into(),
        ));
    }
    validate_bundle(&apps[0], &handoff.update.version)?;
    // A same-volume staging directory makes the final rename atomic. Failure to
    // write alongside an admin-owned installation leaves the old bundle intact.
    let parent = target.parent().ok_or(UpdateError::Integrity)?;
    let staging = tempfile::Builder::new()
        .prefix(".ytu-install-")
        .tempdir_in(parent)?;
    let replacement = staging
        .path()
        .join(target.file_name().ok_or(UpdateError::Integrity)?);
    copy_bundle(&apps[0], &replacement)?;
    validate_bundle(&replacement, &handoff.update.version)?;
    let backup = staging.path().join("previous.app");
    let result = replace_bundle(&replacement, &target, &backup, || {
        validate_bundle(&target, &handoff.update.version)?;
        if handoff.restart {
            run_bounded(
                Command::new("/usr/bin/open").arg("-n").arg(&target),
                Duration::from_secs(20),
            )?;
        }
        Ok(())
    });
    if result.is_err() {
        // Retain recovery material if rollback itself was prevented by an I/O
        // failure. Never let a TempDir destructor remove the previous bundle.
        let _ = staging.keep();
        return result;
    }
    // Only our own previous bundle/staging directory is removed here. Profile,
    // library, preferences, and the original DMG are outside this transaction.
    Ok(())
}

fn bundle_info(bundle: &Path, version: Option<&str>) -> Result<()> {
    if std::fs::symlink_metadata(bundle)?.file_type().is_symlink() {
        return Err(UpdateError::Integrity);
    }
    let info_path = bundle.join("Contents/Info.plist");
    if std::fs::symlink_metadata(&info_path)?.len() > 1024 * 1024 {
        return Err(UpdateError::Integrity);
    }
    let value = plist::Value::from_file(&info_path)
        .map_err(|_| UpdateError::Installation("Info.plist no válido".into()))?;
    let info = value.as_dictionary().ok_or(UpdateError::Integrity)?;
    let text = |key: &str| info.get(key).and_then(plist::Value::as_string);
    if text("CFBundleIdentifier") != Some(BUNDLE_ID)
        || text("CFBundlePackageType") != Some("APPL")
        || version.is_some_and(|v| text("CFBundleShortVersionString") != Some(v))
    {
        return Err(UpdateError::Installation(
            "La identidad o versión de la aplicación no coincide".into(),
        ));
    }
    let executable = text("CFBundleExecutable").ok_or(UpdateError::Integrity)?;
    if executable.is_empty()
        || Path::new(executable).components().count() != 1
        || executable == "."
        || executable == ".."
    {
        return Err(UpdateError::Integrity);
    }
    let path = bundle.join("Contents/MacOS").join(executable);
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(UpdateError::Integrity);
    }
    Ok(())
}

fn validate_bundle(bundle: &Path, version: &str) -> Result<()> {
    bundle_info(bundle, Some(version))?;
    let url = CFURL::from_path(bundle, true).ok_or(UpdateError::Integrity)?;
    let code = SecStaticCode::from_path(&url, Flags::NONE).map_err(|_| {
        UpdateError::Installation("La aplicación no tiene una firma de código válida".into())
    })?;
    // Accept the project's ad-hoc signature while validating every sealed file.
    // Requiring a Developer ID would incorrectly reject the shipped edition.
    let requirement: SecRequirement = format!("identifier \"{BUNDLE_ID}\"")
        .parse()
        .map_err(|_| UpdateError::Integrity)?;
    code.check_validity(
        Flags::CHECK_ALL_ARCHITECTURES | Flags::STRICT_VALIDATE,
        &requirement,
    )
    .map_err(|_| {
        UpdateError::Installation("La firma de la aplicación o sus recursos no es válida".into())
    })
}

fn copy_bundle(source: &Path, destination: &Path) -> Result<()> {
    let root = source.canonicalize()?;
    let mut entries = 0usize;
    let mut bytes = 0u64;
    copy_entry(source, destination, &root, &mut entries, &mut bytes)
}

fn copy_entry(
    source: &Path,
    destination: &Path,
    root: &Path,
    entries: &mut usize,
    bytes: &mut u64,
) -> Result<()> {
    *entries += 1;
    if *entries > MAX_BUNDLE_ENTRIES {
        return Err(UpdateError::Integrity);
    }
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let link = std::fs::read_link(source)?;
        if link.is_absolute() || !source.canonicalize()?.starts_with(root) {
            return Err(UpdateError::Installation(
                "El bundle contiene un enlace fuera de la aplicación".into(),
            ));
        }
        symlink(link, destination)?;
    } else if metadata.is_dir() {
        std::fs::create_dir(destination)?;
        for child in std::fs::read_dir(source)? {
            let child = child?;
            copy_entry(
                &child.path(),
                &destination.join(child.file_name()),
                root,
                entries,
                bytes,
            )?;
        }
        std::fs::set_permissions(destination, metadata.permissions())?;
    } else if metadata.is_file() {
        *bytes = bytes
            .checked_add(metadata.len())
            .ok_or(UpdateError::Integrity)?;
        if *bytes > MAX_BUNDLE_BYTES {
            return Err(UpdateError::Integrity);
        }
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)?;
        if std::io::copy(&mut input, &mut output)? != metadata.len() {
            return Err(UpdateError::Integrity);
        }
        output.sync_all()?;
        std::fs::set_permissions(destination, metadata.permissions())?;
    } else {
        return Err(UpdateError::Integrity);
    }
    Ok(())
}

fn replace_bundle(
    replacement: &Path,
    target: &Path,
    backup: &Path,
    validate: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if backup.try_exists()? {
        return Err(UpdateError::Integrity);
    }
    swap_paths(replacement, target)?;
    if let Err(error) = validate() {
        swap_paths(replacement, target)?;
        return Err(error);
    }
    std::fs::rename(replacement, backup)?;
    Ok(())
}

fn swap_paths(first: &Path, second: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn renamex_np(
            from: *const std::ffi::c_char,
            to: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    let first =
        std::ffi::CString::new(first.as_os_str().as_bytes()).map_err(|_| UpdateError::Integrity)?;
    let second = std::ffi::CString::new(second.as_os_str().as_bytes())
        .map_err(|_| UpdateError::Integrity)?;
    // SAFETY: both null-terminated paths remain alive. Darwin RENAME_SWAP (2)
    // atomically exchanges the same-volume bundles without an absent-app gap.
    if unsafe { renamex_np(first.as_ptr(), second.as_ptr(), 2) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn run_bounded(command: &mut Command, timeout: Duration) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(UpdateError::Installation(format!(
                    "La utilidad del sistema terminó con {status}"
                )))
            };
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(UpdateError::Installation(
                "La utilidad del sistema tardó demasiado".into(),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

struct MountedImage {
    path: PathBuf,
    attached: bool,
}
impl Drop for MountedImage {
    fn drop(&mut self) {
        if self.attached {
            let _ = run_bounded(
                Command::new("/usr/bin/hdiutil")
                    .arg("detach")
                    .arg(&self.path),
                Duration::from_secs(30),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Creates and mounts an isolated test DMG with macOS hdiutil; never uses the installed app"]
    fn isolated_dmg_install_preserves_profile_and_replaces_only_test_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("disk-source");
        let target = directory.path().join("installed/Fixture.app");
        let cache = directory.path().join("cache");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir(&cache).unwrap();
        let packaged = source.join("Fixture.app");
        test_bundle(&packaged, "5.9.0");
        test_bundle(&target, "5.8.0");
        let profile = directory.path().join("installed/library.flac");
        std::fs::write(&profile, "existing personal media").unwrap();
        let image = cache.join("fixture.dmg");
        run_bounded(
            Command::new("/usr/bin/hdiutil")
                .args([
                    "create",
                    "-format",
                    "UDZO",
                    "-volname",
                    "YT Updater Fixture",
                    "-srcfolder",
                ])
                .arg(&source)
                .arg(&image),
            Duration::from_secs(60),
        )
        .unwrap();
        let handoff = Handoff {
            current_version: "5.8.0".into(),
            repository: crate::RELEASE_REPOSITORY.into(),
            edition: Edition::Base,
            cache,
            executable: target.join("Contents/MacOS/fixture"),
            installer: image.clone(),
            restart: false,
            parent_pid: std::process::id(),
            update: UpdateInfo {
                version: "5.9.0".into(),
                name: "fixture.dmg".into(),
                download_url: String::new(),
                release_url: String::new(),
                sha256: String::new(),
                size: std::fs::metadata(image).unwrap().len(),
                platform: crate::Platform::current().unwrap(),
                edition: Edition::Base,
            },
        };
        install(&handoff).unwrap();
        validate_bundle(&target, "5.9.0").unwrap();
        assert_eq!(
            std::fs::read_to_string(profile).unwrap(),
            "existing personal media"
        );
        assert!(std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ytu-install-")));
    }

    fn test_bundle(bundle: &Path, version: &str) {
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        std::fs::copy("/usr/bin/true", bundle.join("Contents/MacOS/fixture")).unwrap();
        let mut info = plist::Dictionary::new();
        for (key, value) in [
            ("CFBundleIdentifier", BUNDLE_ID),
            ("CFBundleExecutable", "fixture"),
            ("CFBundlePackageType", "APPL"),
            ("CFBundleShortVersionString", version),
            ("CFBundleVersion", version),
        ] {
            info.insert(key.into(), plist::Value::String(value.into()));
        }
        plist::Value::Dictionary(info)
            .to_file_xml(bundle.join("Contents/Info.plist"))
            .unwrap();
        std::fs::write(bundle.join("Contents/Resources/sealed.txt"), "original").unwrap();
        run_bounded(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(bundle),
            Duration::from_secs(20),
        )
        .unwrap();
    }

    #[test]
    fn validates_ad_hoc_bundle_identity_version_and_sealed_resources() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("Fixture.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        std::fs::copy("/usr/bin/true", bundle.join("Contents/MacOS/fixture")).unwrap();
        let mut info = plist::Dictionary::new();
        for (key, value) in [
            ("CFBundleIdentifier", BUNDLE_ID),
            ("CFBundleExecutable", "fixture"),
            ("CFBundlePackageType", "APPL"),
            ("CFBundleShortVersionString", "5.9.0"),
            ("CFBundleVersion", "5.9.0"),
        ] {
            info.insert(key.into(), plist::Value::String(value.into()));
        }
        plist::Value::Dictionary(info)
            .to_file_xml(bundle.join("Contents/Info.plist"))
            .unwrap();
        let resource = bundle.join("Contents/Resources/sealed.txt");
        std::fs::write(&resource, "original").unwrap();
        run_bounded(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&bundle),
            Duration::from_secs(20),
        )
        .unwrap();
        validate_bundle(&bundle, "5.9.0").unwrap();
        assert!(validate_bundle(&bundle, "5.9.1").is_err());
        let copied = directory.path().join("Copied.app");
        copy_bundle(&bundle, &copied).unwrap();
        validate_bundle(&copied, "5.9.0").unwrap();
        std::fs::write(&resource, "tampered").unwrap();
        assert!(validate_bundle(&bundle, "5.9.0").is_err());
    }
    #[test]
    fn replacement_rolls_back_without_touching_sibling_data() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("YT.app");
        let replacement = directory.path().join("next.app");
        let backup = directory.path().join("old.app");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(target.join("version"), "old").unwrap();
        std::fs::write(replacement.join("version"), "new").unwrap();
        std::fs::write(directory.path().join("library.mp3"), "user data").unwrap();
        assert!(replace_bundle(&replacement, &target, &backup, || Err(
            UpdateError::Integrity
        ))
        .is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("version")).unwrap(),
            "old"
        );
        replace_bundle(&replacement, &target, &backup, || Ok(())).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("version")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(backup.join("version")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("library.mp3")).unwrap(),
            "user data"
        );
    }
    #[test]
    fn copy_preserves_internal_links_permissions_and_rejects_external_links() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("binary"), "app").unwrap();
        std::fs::set_permissions(
            source.join("binary"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink("binary", source.join("alias")).unwrap();
        let destination = directory.path().join("copy");
        copy_bundle(&source, &destination).unwrap();
        assert_eq!(
            std::fs::read_link(destination.join("alias")).unwrap(),
            Path::new("binary")
        );
        assert_eq!(
            std::fs::metadata(destination.join("binary"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        symlink("/bin/sh", source.join("outside")).unwrap();
        assert!(copy_bundle(&source, &directory.path().join("rejected")).is_err());
    }
}
