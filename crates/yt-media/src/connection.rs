//! Explicit connection preferences. Cookie contents never enter Debug output.
use crate::{network_error, MediaError, Result};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct ConnectionOptions {
    pub socket_timeout_seconds: u64,
    pub proxy: Option<String>,
    /// A Netscape cookie file explicitly selected by the user. Browser stores
    /// are never inspected, decrypted, or exported by this module.
    pub cookies_file: Option<PathBuf>,
}
impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            socket_timeout_seconds: 30,
            proxy: None,
            cookies_file: None,
        }
    }
}
impl std::fmt::Debug for ConnectionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionOptions")
            .field("socket_timeout_seconds", &self.socket_timeout_seconds)
            .field("proxy_configured", &self.proxy.is_some())
            .field("cookies_configured", &self.cookies_file.is_some())
            .finish()
    }
}
impl ConnectionOptions {
    pub(crate) fn client(&self, youtube_only: bool) -> Result<reqwest::Client> {
        self.client_with_policy(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 8 || (youtube_only && !is_youtube_url(attempt.url())) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
    }
    pub(crate) fn client_with_policy(
        &self,
        policy: reqwest::redirect::Policy,
    ) -> Result<reqwest::Client> {
        if !(1..=300).contains(&self.socket_timeout_seconds) {
            return Err(MediaError::InvalidInput(
                "El tiempo de conexión debe estar entre 1 y 300 segundos.".into(),
            ));
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(self.socket_timeout_seconds))
            .read_timeout(Duration::from_secs(self.socket_timeout_seconds))
            .redirect(policy);
        if let Some(proxy) = self.proxy.as_deref().filter(|v| !v.trim().is_empty()) {
            let parsed = url::Url::parse(proxy).map_err(|_| {
                MediaError::InvalidInput("La dirección del proxy no es válida.".into())
            })?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(MediaError::InvalidInput(
                    "El proxy debe usar HTTP o HTTPS.".into(),
                ));
            }
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| {
                MediaError::InvalidInput("La dirección del proxy no es válida.".into())
            })?);
        }
        builder.build().map_err(network_error)
    }
    pub(crate) fn cookie_header(&self) -> Result<Option<String>> {
        let Some(path) = &self.cookies_file else {
            return Ok(None);
        };
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() || meta.len() > 1024 * 1024 {
            return Err(MediaError::InvalidInput(
                "El archivo de cookies debe ser texto Netscape de hasta 1 MiB.".into(),
            ));
        }
        let text = std::fs::read_to_string(path)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cookies = parse_netscape(&text, now)?;
        if cookies.is_empty() {
            return Err(MediaError::InvalidInput(
                "El archivo no contiene cookies vigentes para YouTube.".into(),
            ));
        }
        Ok(Some(cookies))
    }
}
pub(crate) fn is_youtube_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some("youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com")
        )
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}
pub(crate) fn parse_netscape(text: &str, now: u64) -> Result<String> {
    let mut cookies = std::collections::BTreeMap::new();
    for raw in text.lines() {
        let line = raw.strip_prefix("#HttpOnly_").unwrap_or(raw);
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 7 {
            return Err(MediaError::InvalidInput(
                "El archivo no tiene formato Netscape válido.".into(),
            ));
        }
        let domain = fields[0].trim_start_matches('.').to_ascii_lowercase();
        // Innertube requests use www.youtube.com; do not widen host-only cookies
        // or path-restricted credentials to a different origin/path.
        if !matches!(domain.as_str(), "youtube.com" | "www.youtube.com")
            || (domain == "youtube.com" && !fields[1].eq_ignore_ascii_case("TRUE"))
            || fields[2] != "/"
        {
            continue;
        }
        let expires = fields[4].parse::<u64>().map_err(|_| {
            MediaError::InvalidInput("El archivo contiene una fecha de cookie no válida.".into())
        })?;
        if expires != 0 && expires <= now {
            continue;
        }
        let name = fields[5];
        let value = fields[6];
        if name.is_empty()
            || name
                .bytes()
                .any(|b| !b.is_ascii_graphic() || b"()<>@,;:\\\"/[]?={} ".contains(&b))
            || value.bytes().any(|b| !b.is_ascii_graphic() || b == b';')
        {
            return Err(MediaError::InvalidInput(
                "El archivo contiene una cookie no válida.".into(),
            ));
        }
        cookies.insert(name, value);
    }
    let value = cookies
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    if value.len() > 32768 {
        return Err(MediaError::InvalidInput(
            "Las cookies de YouTube exceden el tamaño permitido.".into(),
        ));
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookies_are_explicit_expiring_and_origin_scoped() {
        let data="# Netscape HTTP Cookie File\n#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t200\tSID\tsecret\n.evil.test\tTRUE\t/\tTRUE\t200\tother\tno\n.youtube.com\tTRUE\t/\tTRUE\t99\texpired\tno\n.youtube.com\tTRUE\t/account\tTRUE\t200\tpath\tno\nyoutube.com\tFALSE\t/\tTRUE\t200\thost\tno";
        assert_eq!(parse_netscape(data, 100).unwrap(), "SID=secret");
        assert!(!format!(
            "{:?}",
            ConnectionOptions {
                proxy: Some("http://user:secret@proxy.test".into()),
                ..Default::default()
            }
        )
        .contains("secret"));
        assert!(!is_youtube_url(
            &url::Url::parse("https://youtube.com.evil.test/").unwrap()
        ));
    }
    #[test]
    fn rejects_cookie_header_injection() {
        assert!(parse_netscape(".youtube.com\tTRUE\t/\tTRUE\t0\tSID\tsecret;bad=x", 0).is_err());
    }
}
