use crate::{Result, UpdateError, MAX_INSTALLER_BYTES};
use semver::Version;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacArm64,
    MacX64,
    WindowsX64,
}

/// Editions have different OS requirements and never silently replace one another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Edition {
    #[default]
    Base,
    Full,
}

impl Platform {
    pub fn current() -> Result<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => Ok(Self::MacArm64),
            ("macos", "x86_64") => Ok(Self::MacX64),
            ("windows", "x86_64") => Ok(Self::WindowsX64),
            _ => Err(UpdateError::UnsupportedPlatform),
        }
    }

    pub(crate) fn accepts_name(self, name: &str, version: &Version, edition: Edition) -> bool {
        if self == Self::WindowsX64 {
            let suffix = if edition == Edition::Full {
                "_full"
            } else {
                ""
            };
            return name == format!("yt-ultimate_{version}_x64{suffix}-setup.exe");
        }
        let arch = if self == Self::MacArm64 {
            "aarch64"
        } else {
            "x64"
        };
        let suffix = if edition == Edition::Full {
            "_full"
        } else {
            ""
        };
        ["YT Ultimate", "YT.Ultimate", "YT-Ultimate"]
            .iter()
            .any(|product| name == format!("{product}_{version}_{arch}{suffix}.dmg"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    pub version: String,
    pub name: String,
    pub download_url: String,
    pub release_url: String,
    pub sha256: String,
    pub size: u64,
    pub platform: Platform,
    pub edition: Edition,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub digest: Option<String>,
}

pub(crate) struct Selected<'a> {
    pub version: Version,
    pub asset: &'a Asset,
}

pub(crate) fn valid_repository(repository: &str) -> bool {
    let parts = repository.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 100
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                && !part.contains("..")
        })
}

pub(crate) fn select_release<'a>(
    release: &'a Release,
    current: &Version,
    platform: Platform,
    edition: Edition,
    repository: &str,
) -> Result<Option<Selected<'a>>> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    if !version.pre.is_empty() || !version.cmp_precedence(current).is_gt() {
        return Ok(None);
    }
    if release.assets.len() > 256 {
        return Err(UpdateError::Metadata(
            "Demasiados archivos en la publicación".into(),
        ));
    }
    let matches = release
        .assets
        .iter()
        .filter(|asset| {
            asset.state == "uploaded" && platform.accepts_name(&asset.name, &version, edition)
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(UpdateError::Metadata(
            "Hay varios instaladores para la misma arquitectura".into(),
        ));
    }
    let Some(asset) = matches.first().copied() else {
        return Ok(None);
    };
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        return Err(UpdateError::Metadata(
            "Tamaño de instalador inválido".into(),
        ));
    }
    validate_asset_url(
        &asset.browser_download_url,
        repository,
        &release.tag_name,
        &asset.name,
    )?;
    Ok(Some(Selected { version, asset }))
}

fn github_url(repository: &str, parts: &[&str]) -> Result<Url> {
    if !valid_repository(repository) {
        return Err(UpdateError::Metadata("Repositorio inválido".into()));
    }
    let mut url =
        Url::parse("https://github.com").map_err(|e| UpdateError::Metadata(e.to_string()))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| UpdateError::Metadata("URL base inválida".into()))?;
        segments.extend(repository.split('/'));
        segments.extend(parts.iter().copied());
    }
    Ok(url)
}

pub(crate) fn release_url(repository: &str, tag: &str) -> Result<String> {
    Ok(github_url(repository, &["releases", "tag", tag])?.to_string())
}

pub(crate) fn tag_from_release_url(value: &str, repository: &str) -> Result<String> {
    let url = Url::parse(value).map_err(|e| UpdateError::Metadata(e.to_string()))?;
    let segments = url
        .path_segments()
        .ok_or_else(|| UpdateError::Metadata("URL de publicación inválida".into()))?
        .collect::<Vec<_>>();
    let tag = segments
        .last()
        .ok_or_else(|| UpdateError::Metadata("Falta el identificador de versión".into()))?
        .to_string();
    if release_url(repository, &tag)? != value {
        return Err(UpdateError::Metadata(
            "La publicación no pertenece al repositorio configurado".into(),
        ));
    }
    Ok(tag)
}

pub(crate) fn validate_asset_url(
    value: &str,
    repository: &str,
    tag: &str,
    name: &str,
) -> Result<()> {
    let url = Url::parse(value).map_err(|e| UpdateError::Metadata(e.to_string()))?;
    if url != github_url(repository, &["releases", "download", tag, name])? {
        return Err(UpdateError::Metadata(
            "El archivo no pertenece a esta publicación de GitHub".into(),
        ));
    }
    Ok(())
}

pub(crate) fn allowed_transport_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "api.github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

pub(crate) fn valid_sha256(value: &str) -> Result<String> {
    if value.len() != 64 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(UpdateError::Metadata("SHA-256 inválido".into()));
    }
    Ok(value.to_ascii_lowercase())
}

pub(crate) fn parse_digest(value: &str) -> Result<Option<String>> {
    if value.is_empty() {
        return Ok(None);
    }
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| UpdateError::Metadata("El servidor no publicó un digest SHA-256".into()))?;
    Ok(Some(valid_sha256(hex)?))
}

pub(crate) fn checksum_for(text: &str, filename: &str) -> Result<String> {
    let mut selected = None;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let split = line
            .find(char::is_whitespace)
            .ok_or_else(|| UpdateError::Metadata("Formato SHA256SUMS inválido".into()))?;
        let digest = valid_sha256(&line[..split])?;
        let name = line[split..]
            .trim_start()
            .strip_prefix('*')
            .unwrap_or_else(|| line[split..].trim_start());
        if name == filename && selected.replace(digest).is_some() {
            return Err(UpdateError::Metadata(
                "SHA256SUMS contiene entradas duplicadas".into(),
            ));
        }
    }
    selected.ok_or_else(|| {
        UpdateError::Metadata("El nombre del instalador no aparece en SHA256SUMS".into())
    })
}
