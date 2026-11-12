use crate::Result;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

const WEBVIEW2_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
const WEBVIEW2_SHA256: &str = "cf2f2445fef0c5c14e372045f84998771eeb8bb01d59be4afd00692fa160082e";

pub fn webview2_bootstrapper(root: &Path) -> Result<PathBuf> {
    let path = env::var_os("YTU_WEBVIEW2_BOOTSTRAPPER")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/native-deps/webview2/MicrosoftEdgeWebView2Setup.exe"));
    if !path.is_file() {
        if env::var_os("YTU_WEBVIEW2_BOOTSTRAPPER").is_some() {
            return Err(format!("WebView2 bootstrapper not found: {}", path.display()).into());
        }
        fs::create_dir_all(
            path.parent()
                .ok_or("Missing bootstrapper parent directory")?,
        )?;
        let partial = path.with_extension("exe.download");
        crate::download(WEBVIEW2_URL, &partial)
            .map_err(|_| "Could not download the official WebView2 bootstrapper")?;
        verify_webview2_bootstrapper(&partial)?;
        fs::rename(partial, &path)?;
    }
    verify_webview2_bootstrapper(&path)?;
    Ok(path.canonicalize()?)
}

fn verify_webview2_bootstrapper(path: &Path) -> Result {
    if cfg!(windows) {
        let output = crate::powershell().args([
            "-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference='Stop'; $signature=Get-AuthenticodeSignature -LiteralPath $env:YTU_BOOTSTRAPPER_PATH; if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '(^|, *)CN=Microsoft Corporation(,|$)') { throw 'WebView2 bootstrapper must have a valid Microsoft signature' }"
        ]).env("YTU_BOOTSTRAPPER_PATH", path).output()?;
        if !output.status.success() {
            return Err(format!(
                "Could not verify WebView2 Authenticode signature: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
    }
    if crate::file_sha256(path)? != WEBVIEW2_SHA256 {
        return Err(format!("Unexpected WebView2 bootstrapper: {}. The official download is SHA-256 pinned; review Microsoft's updated binary before updating WEBVIEW2_SHA256.", path.display()).into());
    }
    Ok(())
}

pub fn native_files() -> Result<Vec<PathBuf>> {
    let libtorch = PathBuf::from(
        env::var_os("LIBTORCH")
            .ok_or("Full Windows builds require Windows LibTorch 2.12 in LIBTORCH")?,
    );
    let runtime = PathBuf::from(env::var_os("YTU_VC_RUNTIME_DIR").ok_or("Full Windows builds require the x64 Microsoft VC runtime DLL directory in YTU_VC_RUNTIME_DIR")?);
    let mut files = BTreeMap::new();
    for directory in [libtorch.join("lib"), runtime] {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if !path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
                || name.contains("python")
                || name.contains("stubs")
            {
                continue;
            }
            if files.insert(name.clone(), path).is_some() {
                return Err(format!("Duplicate native Windows DLL: {name}").into());
            }
        }
    }
    for required in [
        "c10.dll",
        "torch.dll",
        "torch_cpu.dll",
        "libiomp5md.dll",
        "msvcp140.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
    ] {
        if !files.contains_key(required) {
            return Err(format!("Missing required native Windows DLL: {required}").into());
        }
    }
    Ok(files.into_values().collect())
}

pub fn verify_dependencies(executable: &Path, libraries: &[PathBuf]) -> Result {
    let available: BTreeMap<_, _> = libraries
        .iter()
        .map(|path| {
            (
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase(),
                path,
            )
        })
        .collect();
    for path in std::iter::once(executable).chain(libraries.iter().map(PathBuf::as_path)) {
        let dependencies = imports(path)?;
        println!("{} imports: {}", path.display(), dependencies.join(", "));
        let missing: Vec<_> = dependencies
            .iter()
            .filter(|name| !system_dll(name) && !available.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            return Err(format!("{} needs unbundled DLLs: {missing:?}", path.display()).into());
        }
    }
    println!("Verified Windows x64 native DLLs and their transitive imports; Python is excluded.");
    Ok(())
}

fn system_dll(name: &str) -> bool {
    name.starts_with("api-ms-win-")
        || name.starts_with("ext-ms-win-")
        || matches!(
            name,
            "kernel32.dll"
                | "kernelbase.dll"
                | "ntdll.dll"
                | "advapi32.dll"
                | "bcrypt.dll"
                | "bcryptprimitives.dll"
                | "user32.dll"
                | "gdi32.dll"
                | "shell32.dll"
                | "shlwapi.dll"
                | "ole32.dll"
                | "oleaut32.dll"
                // Windows Runtime activation/HSTRING API, supplied by Windows 8+.
                // https://learn.microsoft.com/windows/win32/api/roapi/nf-roapi-getactivationfactory
                | "combase.dll"
                // Windows Imaging Component, supplied by Windows Vista+.
                // https://learn.microsoft.com/windows/win32/api/wincodec/nf-wincodec-wicconvertbitmapsource
                | "windowscodecs.dll"
                | "comctl32.dll"
                | "dwmapi.dll"
                | "ws2_32.dll"
                | "mswsock.dll"
                | "wsock32.dll"
                | "rpcrt4.dll"
                | "crypt32.dll"
                | "wintrust.dll"
                | "secur32.dll"
                | "iphlpapi.dll"
                | "setupapi.dll"
                | "normaliz.dll"
                | "winhttp.dll"
                | "winmm.dll"
                | "version.dll"
                | "psapi.dll"
                | "dbghelp.dll"
                | "userenv.dll"
                | "ucrtbase.dll"
                | "msvcrt.dll"
                | "powrprof.dll"
                | "synchronization.dll"
                | "ntasn1.dll"
                | "ncrypt.dll"
                | "imm32.dll"
                | "comdlg32.dll"
                | "netapi32.dll"
                | "imagehlp.dll"
        )
}

fn imports(path: &Path) -> Result<Vec<String>> {
    let bytes = fs::read(path)?;
    let word = |offset| -> Result<u16> {
        Ok(u16::from_le_bytes(
            bytes
                .get(offset..offset + 2)
                .ok_or("Truncated PE field")?
                .try_into()?,
        ))
    };
    let dword = |offset| -> Result<u32> {
        Ok(u32::from_le_bytes(
            bytes
                .get(offset..offset + 4)
                .ok_or("Truncated PE field")?
                .try_into()?,
        ))
    };
    let pe = dword(0x3c)? as usize;
    if bytes.get(..2) != Some(b"MZ")
        || bytes.get(pe..pe + 4) != Some(b"PE\0\0")
        || word(pe + 4)? != 0x8664
        || word(pe + 24)? != 0x20b
    {
        return Err(format!("Expected a Windows x64 native image: {}", path.display()).into());
    }
    let sections = pe + 24 + word(pe + 20)? as usize;
    let rva = |address: u32| -> Result<usize> {
        for index in 0..word(pe + 6)? as usize {
            let section = sections + 40 * index;
            let start = dword(section + 12)?;
            let size = dword(section + 16)?;
            if address >= start && address - start < size {
                let offset = dword(section + 20)? as usize + (address - start) as usize;
                if offset < bytes.len() {
                    return Ok(offset);
                }
            }
        }
        Err(format!("Invalid PE address {address:#x} in {}", path.display()).into())
    };
    let mut names = Vec::new();
    // Both normal imports and delay-load imports need to resolve on the target.
    for (directory, stride, name_field) in [(1usize, 20usize, 12usize), (13, 32, 4)] {
        let address = dword(pe + 24 + 112 + directory * 8)?;
        if address == 0 {
            continue;
        }
        let mut descriptor = rva(address)?;
        for _ in 0..1024 {
            let fields = bytes
                .get(descriptor..descriptor + stride)
                .ok_or("Truncated PE import descriptor")?;
            if fields.iter().all(|value| *value == 0) {
                break;
            }
            if directory == 13 && dword(descriptor)? & 1 == 0 {
                return Err("Unsupported absolute-address delay import".into());
            }
            let name = rva(dword(descriptor + name_field)?)?;
            let tail = bytes.get(name..).ok_or("Invalid PE import name")?;
            let end = tail
                .iter()
                .take(512)
                .position(|byte| *byte == 0)
                .ok_or("Invalid PE DLL name")?;
            names.push(std::str::from_utf8(&tail[..end])?.to_ascii_lowercase());
            descriptor += stride;
        }
    }
    Ok(names)
}
