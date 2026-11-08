use std::{
    collections::HashSet,
    env,
    error::Error,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
mod windows;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
const PACKAGER_VERSION: &str = "0.11.8";
const XWIN_VERSION: &str = "0.23.1";
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
const TORCH_LIBRARIES: &[&str] = &[
    "libtorch.dylib",
    "libtorch_cpu.dylib",
    "libc10.dylib",
    "libomp.dylib",
];
const MODEL_SHA256: &str = "31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    env::set_current_dir(root)?;
    let mut args = env::args().skip(1);
    let task = args.next().unwrap_or_else(|| "help".into());
    if task == "sign-local-macos" {
        let app = PathBuf::from(args.next().ok_or("Expected a generated app bundle")?);
        if env::var("CARGO_PACKAGER_FORMAT").is_ok_and(|format| format != "dmg") {
            return Ok(());
        }
        if !app
            .canonicalize()?
            .starts_with(root.join("target/packages").canonicalize()?)
        {
            return Err("Local signing is restricted to generated project packages".into());
        }
        return sign_local_macos(&app);
    }
    if task == "help" || task == "--help" {
        println!("cargo run -p xtask -- check\ncargo run -p xtask -- prepare [--signed] [--target {WINDOWS_TARGET}]\ncargo run -p xtask -- package [--signed] [--formats app,dmg|nsis,wix] [--target {WINDOWS_TARGET}]\ncargo run -p xtask -- verify [--signed] [--formats app,dmg|nsis,wix] [--target {WINDOWS_TARGET}]\n\nPackages the native host, or cross-compiles Windows NSIS from macOS with cargo-xwin {XWIN_VERSION}. Output: target/packages/<target>/<version>/\nInstall cargo-packager {PACKAGER_VERSION} first, or set CARGO_PACKAGER to its path.");
        println!("Add --native-stems on macOS or Windows x64 to bundle the model and LibTorch (LIBTORCH is required; Windows also needs YTU_VC_RUNTIME_DIR); output goes into the full/ subdirectory.");
        return Ok(());
    }
    if task == "check" {
        if args.next().is_some() {
            return Err("check does not accept arguments".into());
        }
        for arguments in [
            vec!["fmt", "--all", "--", "--check"],
            vec!["test", "--workspace", "--locked"],
            vec![
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ] {
            command("cargo", arguments)?;
        }
        return Ok(());
    }
    if task != "package" && task != "prepare" && task != "verify" {
        return Err(format!("Unknown task: {task}").into());
    }
    let host = rust_host()?;
    let mut target = host.clone();
    let mut signed = false;
    let mut native_stems = false;
    let mut requested_formats = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--signed" => signed = true,
            "--native-stems" => native_stems = true,
            "--formats" => {
                requested_formats = Some(
                    args.next()
                        .ok_or("--formats needs a comma-separated list")?,
                );
            }
            "--target" => target = args.next().ok_or("--target needs a target triple")?,
            _ => return Err(format!("Unknown option: {arg}").into()),
        }
    }
    let cross_windows =
        target != host && host.ends_with("apple-darwin") && target == WINDOWS_TARGET;
    if target != host && !cross_windows {
        return Err("Only macOS to Windows x64 cross-packaging is supported".into());
    }
    if cross_windows && signed {
        return Err("Sign Windows installers on a Windows host; cross-packaging produces unsigned NSIS only".into());
    }
    let mac_stems = native_stems && target.ends_with("apple-darwin");
    let allowed: &[&str] = if target.ends_with("apple-darwin") {
        &["app", "dmg"]
    } else if cross_windows || native_stems && target == WINDOWS_TARGET {
        &["nsis"]
    } else if target == WINDOWS_TARGET {
        &["nsis", "wix"]
    } else {
        return Err(format!(
            "No installer configuration for {target}; use macOS or Windows x64 MSVC"
        )
        .into());
    };
    let formats = requested_formats.unwrap_or_else(|| allowed.join(","));
    if formats.is_empty() || formats.split(',').any(|format| !allowed.contains(&format)) {
        return Err(format!("Valid formats for {target}: {}", allowed.join(",")).into());
    }
    if signed {
        check_signing(&host)?;
    }
    let target_dir = root.join("target");
    let build_binaries_dir = if cross_windows {
        target_dir.join(&target).join("release")
    } else {
        target_dir.join("release")
    };
    let mut out_dir = target_dir
        .join("packages")
        .join(&target)
        .join(env!("CARGO_PKG_VERSION"));
    if native_stems {
        out_dir = out_dir.join("full");
    }
    if task == "verify" {
        verify_outputs(&out_dir, &formats, signed)?;
        if mac_stems {
            verify_native_stems_resources(&out_dir)?;
        }
        println!("Package verification passed: {}", out_dir.display());
        return Ok(());
    }
    fs::create_dir_all(&out_dir)?;
    let generated = target_dir.join("packaging");
    fs::create_dir_all(&generated)?;
    let stage_dir = generated.join(&target).join("full");
    let binaries_dir = if mac_stems {
        stage_dir.join("bin")
    } else {
        build_binaries_dir.clone()
    };
    let (resources, frameworks) = if mac_stems {
        let model = package_model(root)?;
        let resources = format!(
            "[{{\"src\":{},\"target\":\"models/bs-roformer-v1.pt\"}},{{\"src\":{},\"target\":\"licenses\"}}]",
            json(&model.to_string_lossy()), json(&root.join("packaging/licenses").to_string_lossy())
        );
        let frameworks = format!(
            "[{}]",
            TORCH_LIBRARIES
                .iter()
                .map(|name| json(&stage_dir.join("Frameworks").join(name).to_string_lossy()))
                .collect::<Vec<_>>()
                .join(",")
        );
        (resources, frameworks)
    } else if native_stems {
        let model = package_model(root)?;
        let mut resources = vec![
            format!(
                "{{\"src\":{},\"target\":\"models/bs-roformer-v1.pt\"}}",
                json(&model.to_string_lossy())
            ),
            format!(
                "{{\"src\":{},\"target\":\"licenses\"}}",
                json(&root.join("packaging/licenses").to_string_lossy())
            ),
        ];
        for path in windows::native_files()? {
            resources.push(format!(
                "{{\"src\":{},\"target\":{}}}",
                json(&path.to_string_lossy()),
                json(
                    &path
                        .file_name()
                        .ok_or("Missing DLL name")?
                        .to_string_lossy()
                )
            ));
        }
        (format!("[{}]", resources.join(",")), "[]".into())
    } else {
        (
            format!(
                "[{{\"src\":{},\"target\":\"licenses\"}}]",
                json(&root.join("packaging/licenses").to_string_lossy())
            ),
            "[]".into(),
        )
    };
    let template = fs::read_to_string(root.join("packaging/packager.json.in"))?;
    let before_package = if mac_stems && !signed {
        let shell_quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        json(&format!(
            "{} sign-local-macos {}",
            shell_quote(&env::current_exe()?.to_string_lossy()),
            shell_quote(&out_dir.join("YT Ultimate.app").to_string_lossy())
        ))
    } else {
        "null".into()
    };
    let preinstall = if target == WINDOWS_TARGET && formats.split(',').any(|f| f == "nsis") {
        let bootstrapper = windows::webview2_bootstrapper(root)?;
        // Escape literal NSIS source text; no user path is executed by a shell.
        let path = bootstrapper
            .to_string_lossy()
            .replace('$', "$$")
            .replace('"', "$\\\"");
        fs::read_to_string(root.join("packaging/windows-webview2.nsh"))?
            .replace("@WEBVIEW2_BOOTSTRAPPER@", &path)
    } else {
        String::new()
    };
    let config = template
        .replace("@VERSION@", &json(env!("CARGO_PKG_VERSION")))
        .replace("@BINARIES_DIR@", &json(&binaries_dir.to_string_lossy()))
        .replace("@OUTPUT_DIR@", &json(&out_dir.to_string_lossy()))
        .replace("@HOST@", &json(&target))
        .replace("@BEFORE_PACKAGE@", &before_package)
        .replace(
            "@NSIS_TEMPLATE@",
            &json(
                &root
                    .join("packaging/windows-installer.nsi")
                    .to_string_lossy(),
            ),
        )
        .replace("@RESOURCES@", &resources)
        .replace("@FRAMEWORKS@", &frameworks)
        .replace(
            "@MINIMUM_MACOS@",
            &json(if native_stems { "14.0" } else { "12.0" }),
        )
        .replace(
            "@ICNS@",
            &json(&root.join("packaging/icons/icon.icns").to_string_lossy()),
        )
        .replace(
            "@ICO@",
            &json(&root.join("packaging/icons/icon.ico").to_string_lossy()),
        )
        .replace(
            "@PNG@",
            &json(&root.join("packaging/icons/icon.png").to_string_lossy()),
        )
        .replace("@NSIS_PREINSTALL@", &json(&preinstall))
        .replace(
            "@APPLE_IDENTITY@",
            &optional_json(if signed {
                env::var("APPLE_SIGNING_IDENTITY").ok()
            } else {
                None
            }),
        )
        .replace(
            "@WINDOWS_THUMBPRINT@",
            &optional_json(
                signed
                    .then(|| env::var("WINDOWS_CERTIFICATE_THUMBPRINT").ok())
                    .flatten(),
            ),
        )
        .replace(
            "@WINDOWS_TIMESTAMP@",
            &optional_json(
                signed
                    .then(|| env::var("WINDOWS_TIMESTAMP_URL").ok())
                    .flatten(),
            ),
        );
    let variant = if native_stems { "-full" } else { "" };
    let config_path = generated.join(format!("packager-{target}{variant}.json"));
    fs::write(&config_path, config)?;
    println!("Packaging configuration: {}", config_path.display());
    if task == "prepare" {
        return Ok(());
    }

    let local_tool = target_dir
        .join("packager-tools/bin")
        .join(if cfg!(windows) {
            "cargo-packager.exe"
        } else {
            "cargo-packager"
        });
    let packager = env::var_os("CARGO_PACKAGER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if local_tool.is_file() {
                local_tool
            } else {
                PathBuf::from("cargo-packager")
            }
        });
    let version = Command::new(&packager).arg("--version").output()
        .map_err(|_| format!("Install the packager with: cargo install cargo-packager --version {PACKAGER_VERSION} --locked --root target/packager-tools"))?;
    if !version.status.success()
        || String::from_utf8_lossy(&version.stdout)
            .split_whitespace()
            .last()
            != Some(PACKAGER_VERSION)
    {
        return Err(format!("Expected cargo-packager {PACKAGER_VERSION}").into());
    }
    let mut build = if cross_windows {
        let xwin = env::var_os("CARGO_XWIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let local = target_dir.join("packager-tools/bin/cargo-xwin");
                if local.is_file() {
                    local
                } else {
                    PathBuf::from("cargo-xwin")
                }
            });
        let version = Command::new(&xwin).arg("--version").output()?;
        if !version.status.success()
            || String::from_utf8_lossy(&version.stdout)
                .split_whitespace()
                .last()
                != Some(XWIN_VERSION)
        {
            return Err(format!("Expected cargo-xwin {XWIN_VERSION}; install it with cargo install --locked cargo-xwin --version {XWIN_VERSION} --root target/packager-tools").into());
        }
        for tool in [
            "clang-cl", "lld-link", "llvm-rc", "cmake", "ninja", "makensis",
        ] {
            let option = if tool == "makensis" {
                "-VERSION"
            } else {
                "--version"
            };
            // llvm-rc has no --version but a nonzero exit still proves it can be started.
            Command::new(tool)
                .arg(option)
                .output()
                .map_err(|_| format!("Missing {tool} in PATH; see docs/RUST_PACKAGING.md"))?;
        }
        Command::new(xwin)
    } else {
        Command::new("cargo")
    };
    build
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "yt-app",
            "--bin",
            "yt-ultimate",
            "--target-dir",
        ])
        .arg(&target_dir);
    if cross_windows {
        build.args(["--target", &target]);
    }
    if native_stems {
        let libtorch = env::var_os("LIBTORCH")
            .ok_or("Set LIBTORCH to the matching native LibTorch 2.12 distribution")?;
        build
            .args(["--features", "native-stems"])
            .env("LIBTORCH", libtorch);
    }
    if target == WINDOWS_TARGET {
        // The base edition embeds CRT. LibTorch's shared C++ ABI uses the
        // matching dynamic CRT, whose DLLs are included beside the executable.
        let previous = env::var("RUSTFLAGS").unwrap_or_default();
        build.env(
            "RUSTFLAGS",
            format!(
                "{previous} -C target-feature={}crt-static",
                if native_stems { "-" } else { "+" }
            ),
        );
        build.env_remove("CARGO_ENCODED_RUSTFLAGS");
    } else if target.ends_with("apple-darwin") {
        build.env(
            "MACOSX_DEPLOYMENT_TARGET",
            if native_stems { "14.0" } else { "12.0" },
        );
    }
    success(&mut build)?;
    let executable = build_binaries_dir.join(if target == WINDOWS_TARGET {
        "yt-ultimate.exe"
    } else {
        "yt-ultimate"
    });
    if !executable.is_file() {
        return Err(format!("Missing built executable: {}", executable.display()).into());
    }
    if target == WINDOWS_TARGET {
        verify_windows_executable(&executable)?;
        if native_stems {
            windows::verify_dependencies(&executable, &windows::native_files()?)?;
        }
    }
    if mac_stems {
        stage_native_stems(&executable, &stage_dir)?;
    }
    let mut package = Command::new(packager);
    // Ensure the signing hook runs after app creation and before DMG assembly.
    let package_formats = if mac_stems && formats.split(',').any(|format| format == "dmg") {
        "app,dmg"
    } else {
        &formats
    };
    package
        .arg("--config")
        .arg(config_path)
        .args(["--formats", package_formats]);
    // CI exposes optional secrets as empty strings; the packager treats even
    // an empty APPLE_KEYCHAIN_PROFILE as an explicitly selected credential.
    for setting in [
        "APPLE_KEYCHAIN_PROFILE",
        "APPLE_ID",
        "APPLE_PASSWORD",
        "APPLE_TEAM_ID",
        "APPLE_API_KEY",
        "APPLE_API_ISSUER",
        "APPLE_API_KEY_PATH",
        "APPLE_CERTIFICATE",
        "APPLE_CERTIFICATE_PASSWORD",
    ] {
        if !signed || !nonempty(setting) {
            package.env_remove(setting);
        }
    }
    success(&mut package)?;
    if mac_stems && !signed && !formats.split(',').any(|format| format == "dmg") {
        sign_local_macos(&out_dir.join("YT Ultimate.app"))?;
    }
    if mac_stems && formats.split(',').any(|format| format == "dmg") {
        let arch = if target.starts_with("aarch64-") {
            "aarch64"
        } else {
            "x64"
        };
        let name = format!("YT Ultimate_{}_{}", env!("CARGO_PKG_VERSION"), arch);
        fs::rename(
            out_dir.join(format!("{name}.dmg")),
            out_dir.join(format!("{name}_full.dmg")),
        )?;
    }
    if native_stems && target == WINDOWS_TARGET {
        let prefix = format!("yt-ultimate_{}_x64", env!("CARGO_PKG_VERSION"));
        if formats.split(',').any(|format| format == "nsis") {
            fs::rename(
                out_dir.join(format!("{prefix}-setup.exe")),
                out_dir.join(format!("{prefix}_full-setup.exe")),
            )?;
        }
        if formats.split(',').any(|format| format == "wix") {
            return Err("Full Windows MSI naming and runtime deployment must be configured before using WiX; use --formats nsis".into());
        }
    }
    verify_outputs(&out_dir, &formats, signed)?;
    if mac_stems {
        verify_native_stems_resources(&out_dir)?;
    }
    write_checksums(&out_dir, &formats)?;
    println!("Installers created: {}", out_dir.display());
    if !signed {
        println!("These are local unsigned builds; signing/notarization has not been performed.");
    }
    Ok(())
}

fn sign_local_macos(app: &Path) -> Result {
    for name in TORCH_LIBRARIES {
        command(
            "codesign",
            [
                OsStr::new("--force"),
                OsStr::new("--sign"),
                OsStr::new("-"),
                OsStr::new("--options"),
                OsStr::new("0"),
                OsStr::new("--timestamp=none"),
                app.join("Contents/Frameworks").join(name).as_os_str(),
            ],
        )?;
    }
    // Ad-hoc identities have no Apple Team ID. Hardened library validation
    // therefore cannot authorize their dylibs. Developer ID builds use the
    // packager's hardened signature instead; local builds use plain ad-hoc.
    command(
        "codesign",
        [
            OsStr::new("--force"),
            OsStr::new("--sign"),
            OsStr::new("-"),
            OsStr::new("--options"),
            OsStr::new("0"),
            OsStr::new("--timestamp=none"),
            app.as_os_str(),
        ],
    )?;
    command(
        "codesign",
        [
            OsStr::new("--verify"),
            OsStr::new("--deep"),
            OsStr::new("--strict"),
            app.as_os_str(),
        ],
    )?;
    Ok(())
}

fn write_checksums(directory: &Path, formats: &str) -> Result {
    let extensions: Vec<_> = formats
        .split(',')
        .filter(|format| *format != "app")
        .map(|format| match format {
            "nsis" => "exe",
            "wix" => "msi",
            other => other,
        })
        .collect();
    let mut files = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.sort();
    let mut manifest = String::new();
    for path in files {
        if !path.is_file()
            || !path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| extensions.contains(&ext))
        {
            continue;
        }
        let output = if cfg!(windows) {
            Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", "(Get-FileHash -Algorithm SHA256 -LiteralPath $env:YTU_CHECKSUM_PATH).Hash.ToLowerInvariant()"]).env("YTU_CHECKSUM_PATH", &path).output()?
        } else {
            Command::new("shasum")
                .args(["-a", "256"])
                .arg(&path)
                .output()?
        };
        let hash = String::from_utf8(output.stdout)?
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !output.status.success()
            || hash.len() != 64
            || !hash.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return Err("Could not calculate installer SHA-256".into());
        }
        manifest.push_str(&format!(
            "{hash}  {}\n",
            path.file_name()
                .ok_or("Missing installer name")?
                .to_string_lossy()
        ));
    }
    if !manifest.is_empty() {
        fs::write(directory.join("SHA256SUMS"), manifest)?;
    }
    Ok(())
}

fn package_model(root: &Path) -> Result<PathBuf> {
    let path = env::var_os("YTU_PACKAGE_STEMS_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("crates/yt-media/models/bs-roformer-v1.pt"));
    verify_model(&path)?;
    Ok(path.canonicalize()?)
}

fn verify_model(path: &Path) -> Result {
    let output = if cfg!(windows) {
        Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", "(Get-FileHash -Algorithm SHA256 -LiteralPath $env:YTU_CHECKSUM_PATH).Hash.ToLowerInvariant()"]).env("YTU_CHECKSUM_PATH", path).output()?
    } else {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()?
    };
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            != Some(MODEL_SHA256)
    {
        return Err(format!(
            "Missing or unexpected BS-RoFormer model: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn stage_native_stems(executable: &Path, stage: &Path) -> Result {
    let libtorch = PathBuf::from(env::var_os("LIBTORCH").ok_or("LIBTORCH is required")?);
    let frameworks = stage.join("Frameworks");
    let binaries = stage.join("bin");
    fs::create_dir_all(&frameworks)?;
    fs::create_dir_all(&binaries)?;
    for name in TORCH_LIBRARIES {
        let target = frameworks.join(name);
        fs::copy(libtorch.join("lib").join(name), &target)?;
        command(
            "install_name_tool",
            [
                OsStr::new("-id"),
                OsStr::new(&format!("@rpath/{name}")),
                target.as_os_str(),
            ],
        )?;
        relocate_macos_dependencies(&target, false)?;
    }
    let binary = binaries.join("yt-ultimate");
    fs::copy(executable, &binary)?;
    relocate_macos_dependencies(&binary, true)?;
    command(
        "install_name_tool",
        [
            OsStr::new("-add_rpath"),
            OsStr::new("@executable_path/../Frameworks"),
            binary.as_os_str(),
        ],
    )?;
    Ok(())
}

fn relocate_macos_dependencies(path: &Path, executable: bool) -> Result {
    let output = Command::new("otool").arg("-L").arg(path).output()?;
    if !output.status.success() {
        return Err("Could not inspect native LibTorch dependencies".into());
    }
    for dependency in String::from_utf8(output.stdout)?
        .lines()
        .skip(if executable { 1 } else { 2 })
    {
        let dependency = dependency.trim().split(" (").next().unwrap_or("");
        if dependency.starts_with("/System/Library/") || dependency.starts_with("/usr/lib/") {
            continue;
        }
        let name = Path::new(dependency)
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or("Invalid dependency path")?;
        if !TORCH_LIBRARIES.contains(&name) {
            return Err(format!("Unexpected native dependency: {dependency}").into());
        }
        let new = format!(
            "{}/{name}",
            if executable {
                "@executable_path/../Frameworks"
            } else {
                "@loader_path"
            }
        );
        command(
            "install_name_tool",
            [
                OsStr::new("-change"),
                OsStr::new(dependency),
                OsStr::new(&new),
                path.as_os_str(),
            ],
        )?;
    }
    let output = Command::new("otool").arg("-l").arg(path).output()?;
    if !output.status.success() {
        return Err("Could not inspect native library rpaths".into());
    }
    let mut rpath = false;
    for line in String::from_utf8(output.stdout)?.lines() {
        let line = line.trim();
        if line == "cmd LC_RPATH" {
            rpath = true;
        } else if rpath {
            if let Some(value) = line.strip_prefix("path ") {
                let value = value.split(" (offset ").next().unwrap_or(value);
                command(
                    "install_name_tool",
                    [
                        OsStr::new("-delete_rpath"),
                        OsStr::new(value),
                        path.as_os_str(),
                    ],
                )?;
                rpath = false;
            }
        }
    }
    Ok(())
}

fn verify_native_stems_resources(output: &Path) -> Result {
    let app = output.join("YT Ultimate.app");
    verify_model(&app.join("Contents/Resources/models/bs-roformer-v1.pt"))?;
    for name in TORCH_LIBRARIES {
        if !app.join("Contents/Frameworks").join(name).is_file() {
            return Err(format!("Missing native inference library: {name}").into());
        }
    }
    command(
        "codesign",
        [
            OsStr::new("--verify"),
            OsStr::new("--deep"),
            OsStr::new("--strict"),
            app.as_os_str(),
        ],
    )?;
    Ok(())
}

fn check_signing(host: &str) -> Result {
    let required: &[&str] = if host.ends_with("apple-darwin") {
        let has_notarization_credentials = nonempty("APPLE_KEYCHAIN_PROFILE")
            || (nonempty("APPLE_ID") && nonempty("APPLE_PASSWORD") && nonempty("APPLE_TEAM_ID"))
            || (nonempty("APPLE_API_KEY")
                && nonempty("APPLE_API_ISSUER")
                && nonempty("APPLE_API_KEY_PATH"));
        if !has_notarization_credentials {
            return Err("Signed macOS packages require notarization credentials; see docs/RUST_PACKAGING.md".into());
        }
        &["APPLE_SIGNING_IDENTITY"]
    } else {
        &["WINDOWS_CERTIFICATE_THUMBPRINT", "WINDOWS_TIMESTAMP_URL"]
    };
    for variable in required {
        if !nonempty(variable) {
            return Err(format!("Missing signing setting: {variable}").into());
        }
    }
    if host.ends_with("apple-darwin") && env::var("APPLE_SIGNING_IDENTITY").as_deref() == Ok("-") {
        return Err("--signed requires a Developer ID identity, not an ad-hoc signature".into());
    }
    Ok(())
}

fn nonempty(name: &str) -> bool {
    env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn rust_host() -> Result<String> {
    let result = Command::new("rustc").arg("-vV").output()?;
    if !result.status.success() {
        return Err("rustc -vV failed".into());
    }
    String::from_utf8(result.stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .ok_or_else(|| "Could not identify Rust host".into())
}

fn command<I, S>(program: &str, args: I) -> Result
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    success(Command::new(program).args(args))
}

fn success(command: &mut Command) -> Result {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed with {status}",
            command.get_program().to_string_lossy()
        )
        .into())
    }
}

fn verify_outputs(directory: &Path, formats: &str, signed: bool) -> Result {
    let files = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    for format in formats.split(',') {
        let extension = match format {
            "wix" => "msi",
            "nsis" => "exe",
            other => other,
        };
        let matches: Vec<_> = files
            .iter()
            .filter(|path| path.extension() == Some(OsStr::new(extension)))
            .collect();
        if matches.is_empty() {
            return Err(format!("Packaging succeeded without producing .{extension}").into());
        }
        for path in matches {
            if extension == "app" {
                if !path.join("Contents/MacOS/yt-ultimate").is_file() {
                    return Err("The .app is missing its Rust executable".into());
                }
                command(
                    "plutil",
                    [
                        OsStr::new("-lint"),
                        path.join("Contents/Info.plist").as_os_str(),
                    ],
                )?;
                verify_macos_bundle(path)?;
                if signed {
                    command(
                        "codesign",
                        [
                            OsStr::new("--verify"),
                            OsStr::new("--deep"),
                            OsStr::new("--strict"),
                            path.as_os_str(),
                        ],
                    )?;
                    command(
                        "xcrun",
                        [
                            OsStr::new("stapler"),
                            OsStr::new("validate"),
                            path.as_os_str(),
                        ],
                    )?;
                }
            } else if fs::metadata(path)?.len() == 0 {
                return Err(format!("Empty installer: {}", path.display()).into());
            } else if extension == "dmg" {
                command("hdiutil", [OsStr::new("verify"), path.as_os_str()])?;
            } else if signed && cfg!(windows) {
                success(Command::new("powershell")
                    .args(["-NoProfile", "-NonInteractive", "-Command", "$signature = Get-AuthenticodeSignature -LiteralPath $env:YTU_SIGNATURE_PATH; if ($signature.Status -ne 'Valid') { Write-Error $signature.StatusMessage; exit 1 }"])
                    .env("YTU_SIGNATURE_PATH", path))?;
            }
        }
    }
    Ok(())
}

fn verify_windows_executable(executable: &Path) -> Result {
    let bytes = fs::read(executable)?;
    if bytes.get(..2) != Some(b"MZ") {
        return Err("The Windows application is not a PE executable".into());
    }
    let pointer: [u8; 4] = bytes
        .get(0x3c..0x40)
        .ok_or("Missing PE header offset")?
        .try_into()?;
    let pe = u32::from_le_bytes(pointer) as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0")
        || bytes.get(pe + 4..pe + 6) != Some(&[0x64, 0x86])
        || bytes.get(pe + 24..pe + 26) != Some(&[0x0b, 0x02])
        || bytes.get(pe + 92..pe + 94) != Some(&[2, 0])
    {
        return Err("Expected a Windows x64 PE32+ GUI application".into());
    }
    println!(
        "Windows x64 GUI executable verified: {}",
        executable.display()
    );
    Ok(())
}

fn verify_macos_bundle(bundle: &Path) -> Result {
    let host = rust_host()?;
    let architecture = if host.starts_with("aarch64-") {
        "arm64"
    } else {
        "x86_64"
    };
    let executable = bundle.join("Contents/MacOS/yt-ultimate");
    let mut pending = vec![executable.clone()];
    let mut inspected = HashSet::new();
    while let Some(binary) = pending.pop() {
        if !inspected.insert(binary.clone()) {
            continue;
        }
        command(
            "lipo",
            [
                binary.as_os_str(),
                OsStr::new("-verify_arch"),
                OsStr::new(architecture),
            ],
        )?;
        let dependencies = Command::new("otool").arg("-L").arg(&binary).output()?;
        if !dependencies.status.success() {
            return Err("Could not inspect packaged native dependencies".into());
        }
        for line in String::from_utf8(dependencies.stdout)?
            .lines()
            .skip(if binary == executable { 1 } else { 2 })
        {
            let dependency = line.trim().split(" (").next().unwrap_or("");
            if dependency.starts_with("/System/Library/") || dependency.starts_with("/usr/lib/") {
                continue;
            }
            let bundled = if let Some(relative) = dependency.strip_prefix("@executable_path/") {
                executable.parent().unwrap().join(relative)
            } else if let Some(relative) = dependency.strip_prefix("@rpath/") {
                bundle.join("Contents/Frameworks").join(relative)
            } else if let Some(relative) = dependency.strip_prefix("@loader_path/") {
                binary.parent().unwrap().join(relative)
            } else {
                return Err(format!(
                    "The app links to an external build-machine library: {dependency}"
                )
                .into());
            };
            let resolved = bundled
                .canonicalize()
                .map_err(|_| format!("Missing bundled library: {dependency}"))?;
            if !resolved.starts_with(bundle.canonicalize()?) {
                return Err(format!("Library escapes the app bundle: {dependency}").into());
            }
            pending.push(resolved);
        }
    }
    let mut files = Vec::new();
    collect_outputs(&bundle.join("Contents"), &mut files)?;
    for path in files {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if matches!(name.as_str(), "electron" | "node" | "python" | "python3")
            || name.starts_with("python3.")
            || name.contains("torch_python")
            || matches!(extension.as_str(), "asar" | "py" | "pyc" | "pyd")
        {
            return Err(format!(
                "Legacy runtime unexpectedly included in the Rust app: {}",
                path.display()
            )
            .into());
        }
    }
    println!(
        "Verified {architecture} app, bundled/system dependencies and absence of legacy runtimes."
    );
    Ok(())
}

fn collect_outputs(directory: &Path, output: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("app")) {
            output.push(path);
        } else if path.is_dir() {
            collect_outputs(&path, output)?;
        } else {
            output.push(path);
        }
    }
    Ok(())
}

fn optional_json(value: Option<String>) -> String {
    value
        .map(|value| json(&value))
        .unwrap_or_else(|| "null".into())
}

fn json(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_paths_and_installer_source_are_escaped_without_shell_interpolation() {
        assert_eq!(
            json("C:\\Users\\A B\\\"app\"\n$INSTDIR"),
            "\"C:\\\\Users\\\\A B\\\\\\\"app\\\"\\n$INSTDIR\""
        );
    }
}
