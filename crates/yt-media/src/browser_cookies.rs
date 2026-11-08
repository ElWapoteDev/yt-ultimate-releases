//! User-initiated, read-only access to YouTube cookies. No process injection,
//! elevation, browser launching, credential export, or automatic discovery.
use crate::{MediaError, Result};
use rusqlite::{Connection, OpenFlags};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

mod crypto;
mod safari;

#[derive(Clone)]
pub struct BrowserCookieSession {
    browser: String,
    count: usize,
    header: Arc<Zeroizing<String>>,
}
impl std::fmt::Debug for BrowserCookieSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserCookieSession")
            .field("browser", &self.browser)
            .field("cookie_count", &self.count)
            .finish_non_exhaustive()
    }
}
impl BrowserCookieSession {
    pub fn browser_id(&self) -> &str {
        &self.browser
    }
    pub fn cookie_count(&self) -> usize {
        self.count
    }
    pub(crate) fn header(&self) -> String {
        self.header.as_str().to_owned()
    }
}

#[derive(Default)]
pub(super) struct Cookie {
    domain: String,
    name: String,
    value: String,
    path: String,
    expires: i64,
    secure: bool,
    http_only: bool,
}
impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cookie([redacted])")
    }
}
impl Drop for Cookie {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.value.zeroize();
    }
}

/// This blocking operation must only run after an explicit browser connection
/// action. `profile` is an optional user-selected profile directory/database.
/// Native Keychain prompts are honored; Windows App-Bound v20 is never bypassed.
pub fn read_browser_cookies(browser: &str, profile: Option<&Path>) -> Result<BrowserCookieSession> {
    if ![
        "chrome", "chromium", "edge", "firefox", "safari", "brave", "opera", "vivaldi", "whale",
    ]
    .contains(&browser)
    {
        return Err(invalid("El navegador seleccionado no es compatible."));
    }
    let home = dirs::home_dir().ok_or_else(|| invalid("No se encontró la carpeta del usuario."))?;
    let path = match profile {
        Some(path) => path.to_owned(),
        None => default_profile(browser, &home)?,
    };
    let cookies = match browser {
        "firefox" => read_firefox(&cookie_path(&path, &["cookies.sqlite"])?),
        "safari" => safari::read(&cookie_path(&path, &["Cookies.binarycookies"])?),
        _ => read_chromium(
            browser,
            &cookie_path(&path, &["Network/Cookies", "Cookies"])?,
        ),
    }?;
    session_from_cookies(browser, cookies)
}

fn invalid(message: &str) -> MediaError {
    MediaError::InvalidInput(message.into())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn accepted_domain(domain: &str) -> bool {
    matches!(
        domain,
        ".youtube.com" | "www.youtube.com" | ".www.youtube.com"
    )
}
fn session_from_cookies(browser: &str, cookies: Vec<Cookie>) -> Result<BrowserCookieSession> {
    let mut netscape = Zeroizing::new(String::new());
    for c in &cookies {
        if !accepted_domain(&c.domain)
            || c.path != "/"
            || (c.expires != 0 && (c.expires < 0 || c.expires as u64 <= now()))
        {
            continue;
        }
        if [&c.domain, &c.path, &c.name, &c.value]
            .iter()
            .any(|v| v.contains(['\t', '\r', '\n', '\0']))
        {
            return Err(invalid("El navegador entregó una cookie inválida."));
        }
        if netscape.len() + c.value.len() + c.name.len() > 65536 {
            return Err(invalid(
                "Las cookies de YouTube exceden el límite admitido.",
            ));
        }
        use std::fmt::Write;
        let _ = writeln!(
            netscape,
            "{}{}\t{}\t{}\t{}\t{}\t{}\t{}",
            if c.http_only { "#HttpOnly_" } else { "" },
            c.domain,
            if c.domain.starts_with('.') {
                "TRUE"
            } else {
                "FALSE"
            },
            c.path,
            if c.secure { "TRUE" } else { "FALSE" },
            c.expires.max(0),
            c.name,
            c.value
        );
    }
    let header = crate::connection::parse_netscape(&netscape, now())?;
    if header.is_empty() {
        return Err(invalid("No hay cookies vigentes de YouTube en este perfil. Inicia sesión en el navegador o elige otro perfil."));
    }
    Ok(BrowserCookieSession {
        browser: browser.into(),
        count: header.split("; ").count(),
        header: Arc::new(Zeroizing::new(header)),
    })
}

fn cookie_path(profile: &Path, names: &[&str]) -> Result<PathBuf> {
    if profile.is_file() {
        return Ok(profile.to_owned());
    }
    names
        .iter()
        .map(|name| profile.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            invalid(
                "No se encontró el archivo de cookies. Elige la carpeta del perfil del navegador.",
            )
        })
}
fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file=std::fs::File::open(path).map_err(|_|invalid("No se pudo leer el perfil. Comprueba los permisos del sistema y cierra el navegador si mantiene el archivo bloqueado."))?;
    if !file.metadata()?.is_file() {
        return Err(invalid("La ruta del perfil no corresponde a un archivo."));
    }
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(invalid(
            "El archivo del navegador supera el tamaño admitido.",
        ));
    }
    Ok(data)
}
fn database(path: &Path) -> Result<Connection> {
    let size = std::fs::metadata(path)
        .map_err(|_| invalid("No se encontró el perfil del navegador."))?
        .len();
    if size > 256 * 1024 * 1024 {
        return Err(invalid("La base de cookies supera 256 MiB."));
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db_error)?;
    db.busy_timeout(Duration::from_secs(1)).map_err(db_error)?;
    Ok(db)
}
fn db_error(_: rusqlite::Error) -> MediaError {
    invalid("No se pudo consultar el perfil. Cierra el navegador si mantiene la base bloqueada y vuelve a intentarlo.")
}
fn read_firefox(path: &Path) -> Result<Vec<Cookie>> {
    let db = database(path)?;
    let mut query=db.prepare("SELECT host,name,value,path,expiry,isSecure,isHttpOnly FROM moz_cookies WHERE host IN ('.youtube.com','www.youtube.com','.www.youtube.com') AND path='/' AND (expiry=0 OR expiry>?1) AND originAttributes='' LIMIT 1025").map_err(db_error)?;
    let rows = query
        .query_map([now() as i64], |r| {
            Ok(Cookie {
                domain: r.get(0)?,
                name: r.get(1)?,
                value: r.get(2)?,
                path: r.get(3)?,
                expires: r.get(4)?,
                secure: r.get(5)?,
                http_only: r.get(6)?,
            })
        })
        .map_err(db_error)?;
    let rows = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if rows.len() > 1024 {
        return Err(invalid("El perfil tiene demasiadas cookies de YouTube."));
    }
    Ok(rows)
}
fn read_chromium(browser: &str, path: &Path) -> Result<Vec<Cookie>> {
    let db = database(path)?;
    let version: i64 = db
        .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let partitioned: bool = db
        .prepare("SELECT top_frame_site_key FROM cookies LIMIT 0")
        .is_ok();
    let sql=format!("SELECT host_key,name,value,path,expires_utc,is_secure,is_httponly,encrypted_value FROM cookies WHERE host_key IN ('.youtube.com','www.youtube.com','.www.youtube.com') AND path='/' {} LIMIT 1025",if partitioned{"AND top_frame_site_key=''"}else{""});
    let mut query = db.prepare(&sql).map_err(db_error)?;
    let rows = query
        .query_map([], |r| {
            Ok((
                Cookie {
                    domain: r.get(0)?,
                    name: r.get(1)?,
                    value: r.get(2)?,
                    path: r.get(3)?,
                    expires: {
                        let micros: i64 = r.get(4)?;
                        if micros == 0 {
                            0
                        } else {
                            micros / 1_000_000 - 11_644_473_600
                        }
                    },
                    secure: r.get(5)?,
                    http_only: r.get(6)?,
                },
                r.get::<_, Vec<u8>>(7)?,
            ))
        })
        .map_err(db_error)?;
    let mut values = Vec::new();
    let mut key = None;
    for row in rows {
        let (mut cookie, encrypted) = row.map_err(db_error)?;
        if cookie.expires != 0 && (cookie.expires < 0 || cookie.expires as u64 <= now()) {
            continue;
        }
        if values.len() >= 1024 {
            return Err(invalid("El perfil tiene demasiadas cookies de YouTube."));
        }
        if cookie.value.is_empty() && !encrypted.is_empty() {
            if encrypted.starts_with(b"v20") {
                return Err(invalid("Este navegador protege sus cookies con App-Bound Encryption. Usa un archivo Netscape exportado por el navegador; no se elude esta protección."));
            }
            if key.is_none() {
                key = Some(crypto::key(browser, path)?);
            }
            cookie.value = crypto::decrypt(
                key.as_ref().map(|key| key.as_slice()).unwrap_or(&[]),
                &cookie.domain,
                &encrypted,
                version >= 24,
            )?;
        }
        values.push(cookie);
    }
    Ok(values)
}

fn default_profile(browser: &str, home: &Path) -> Result<PathBuf> {
    if browser == "safari" {
        if !cfg!(target_os = "macos") {
            return Err(invalid("Safari sólo está disponible en macOS."));
        }
        return [home.join("Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies"),home.join("Library/Cookies/Cookies.binarycookies")].into_iter().find(|p|p.exists()).ok_or_else(||invalid("No se encontró el perfil de Safari. Elige su archivo Cookies.binarycookies y autoriza el acceso en macOS."));
    }
    let base = browser_base(browser, home)?;
    if browser == "firefox" {
        return firefox_default(&base);
    }
    if base.join("Cookies").is_file() || base.join("Network/Cookies").is_file() {
        return Ok(base);
    }
    // Local State contains profile preference metadata, never a cookie value.
    let last = bounded_read(&base.join("Local State"), 4 * 1024 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| {
            v.pointer("/profile/last_used")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    let name = last
        .filter(|p| !p.is_empty() && !p.contains(['/', '\\']) && p != ".." && p != ".")
        .unwrap_or_else(|| "Default".into());
    Ok(base.join(name))
}
fn browser_base(browser: &str, home: &Path) -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        let folder = match browser {
            "chrome" => "Google/Chrome",
            "chromium" => "Chromium",
            "edge" => "Microsoft Edge",
            "firefox" => "Firefox",
            "brave" => "BraveSoftware/Brave-Browser",
            "opera" => "com.operasoftware.Opera",
            "vivaldi" => "Vivaldi",
            "whale" => "Naver/Whale",
            _ => return Err(invalid("Navegador no compatible.")),
        };
        return Ok(home.join("Library/Application Support").join(folder));
    }
    if cfg!(target_os = "windows") {
        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"));
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"));
        return Ok(match browser {
            "firefox" => roaming.join("Mozilla/Firefox"),
            "opera" => roaming.join("Opera Software/Opera Stable"),
            "chrome" => local.join("Google/Chrome/User Data"),
            "chromium" => local.join("Chromium/User Data"),
            "edge" => local.join("Microsoft/Edge/User Data"),
            "brave" => local.join("BraveSoftware/Brave-Browser/User Data"),
            "vivaldi" => local.join("Vivaldi/User Data"),
            "whale" => local.join("Naver/Naver Whale/User Data"),
            _ => return Err(invalid("Navegador no compatible.")),
        });
    }
    Err(invalid(
        "La conexión con navegadores está disponible en Windows y macOS.",
    ))
}
fn firefox_default(base: &Path) -> Result<PathBuf> {
    let bytes = bounded_read(&base.join("profiles.ini"), 1024 * 1024)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("El índice de perfiles de Firefox no es válido."))?;
    let mut sections = Vec::new();
    let mut current = std::collections::BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            if !current.is_empty() {
                sections.push(std::mem::take(&mut current));
            }
        } else if let Some((k, v)) = line.split_once('=') {
            current.insert(k.to_owned(), v.to_owned());
        }
    }
    sections.push(current);
    let profile = sections
        .iter()
        .filter(|p| p.contains_key("Path"))
        .max_by_key(|p| p.get("Default").is_some_and(|v| v == "1"))
        .ok_or_else(|| invalid("Firefox no tiene un perfil disponible."))?;
    let path = PathBuf::from(&profile["Path"]);
    Ok(if profile.get("IsRelative").is_some_and(|v| v == "0") {
        path
    } else {
        base.join(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn firefox_uses_only_live_unpartitioned_youtube_cookies_and_never_logs_values() {
        let temp = tempfile::tempdir().unwrap();
        let db = Connection::open(temp.path().join("cookies.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE moz_cookies(host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,originAttributes TEXT);
          INSERT INTO moz_cookies VALUES('.youtube.com','SID','fixture-secret','/',4102444800,1,1,'');
          INSERT INTO moz_cookies VALUES('.other.test','foreign','other-secret','/',4102444800,1,1,'');
          INSERT INTO moz_cookies VALUES('.youtube.com','expired','bad','/',1,1,1,'');
          INSERT INTO moz_cookies VALUES('.youtube.com','container','bad','/',4102444800,1,1,'^userContextId=1');
          INSERT INTO moz_cookies VALUES('.youtube.com','path','bad','/account',4102444800,1,1,'');").unwrap();
        drop(db);
        let session = read_browser_cookies("firefox", Some(temp.path())).unwrap();
        assert_eq!(session.header(), "SID=fixture-secret");
        assert_eq!(session.cookie_count(), 1);
        assert!(!format!("{session:?}").contains("fixture-secret"));
        assert!(read_browser_cookies("not-browser", Some(temp.path())).is_err());
    }
    #[test]
    fn chromium_v20_is_rejected_before_attempting_any_keychain_access() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Cookies");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE meta(key TEXT,value TEXT); INSERT INTO meta VALUES('version','24');
          CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,encrypted_value BLOB,top_frame_site_key TEXT);
          INSERT INTO cookies VALUES('.youtube.com','SID','','/',0,1,1,x'763230616263','');").unwrap();
        drop(db);
        let error = read_browser_cookies("chrome", Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("App-Bound"));
    }
    #[test]
    fn plaintext_chromium_fixture_filters_partitioned_rows_and_root_host_cookies() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Cookies");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE meta(key TEXT,value TEXT); INSERT INTO meta VALUES('version','24');
          CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,encrypted_value BLOB,top_frame_site_key TEXT);
          INSERT INTO cookies VALUES('www.youtube.com','SID','valid','/',0,1,1,x'','');
          INSERT INTO cookies VALUES('.youtube.com','partition','bad','/',0,1,1,x'','https://other.test');
          INSERT INTO cookies VALUES('youtube.com','host','bad','/',0,1,1,x'','');").unwrap();
        drop(db);
        let session = read_browser_cookies("chrome", Some(&path)).unwrap();
        assert_eq!(session.header(), "SID=valid");
    }
    #[test]
    fn firefox_profiles_respect_relative_default_in_windows_line_endings() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("profiles.ini"),"[Profile0]\r\nPath=Profiles/first\r\nIsRelative=1\r\n[Profile1]\r\nPath=Profiles/second\r\nIsRelative=1\r\nDefault=1\r\n").unwrap();
        assert_eq!(
            firefox_default(temp.path()).unwrap(),
            temp.path().join("Profiles/second")
        );
    }
}
