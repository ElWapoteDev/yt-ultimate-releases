use crate::{AppCore, CoreError, Result, now};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub audio_quality: String,
    pub video_quality: String,
    pub auto_thumbnail: bool,
    pub import_mode: String,
    pub default_download_type: String,
    pub max_concurrent_downloads: u8,
    pub max_concurrent_processing: u8,
    pub download_socket_timeout: u16,
    pub download_retries: u8,
    pub filename_template: String,
    pub preserve_source_metadata: bool,
    pub show_advanced_tools: bool,
    pub developer_mode: bool,
    pub scan_library_on_startup: bool,
    pub download_subtitles: bool,
    pub theme: String,
    pub language: String,
    /// Unknown and historical preferences survive serialization and saving.
    /// Runtime records in the `rust.` namespace use their dedicated APIs.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_quality: "192".into(),
            video_quality: "best".into(),
            auto_thumbnail: true,
            import_mode: "copy".into(),
            default_download_type: "audio".into(),
            max_concurrent_downloads: 3,
            max_concurrent_processing: 1,
            download_socket_timeout: 20,
            download_retries: 2,
            filename_template: "{title}".into(),
            preserve_source_metadata: true,
            show_advanced_tools: false,
            developer_mode: false,
            scan_library_on_startup: true,
            download_subtitles: false,
            theme: "system".into(),
            language: "es".into(),
            extra: Map::new(),
        }
    }
}

pub(crate) fn validate_setting(key: &str, value: &Value) -> Result<()> {
    let choices: &[&str] = match key {
        "audio_quality" => &["128", "192", "256", "320", "best"],
        "video_quality" => &["best", "360", "480", "720", "1080", "1440", "2160", "4320"],
        "import_mode" => &["copy", "reference"],
        "default_download_type" => &["audio", "video"],
        "theme" => &["system", "light", "dark"],
        "language" => &["es", "en"],
        _ => &[],
    };
    if !choices.is_empty() && !value.as_str().is_some_and(|v| choices.contains(&v)) {
        return Err(CoreError::Invalid(format!("Invalid value for {key}")));
    }
    if matches!(
        key,
        "auto_thumbnail"
            | "preserve_source_metadata"
            | "show_advanced_tools"
            | "developer_mode"
            | "scan_library_on_startup"
            | "download_subtitles"
            | "plugin_auto_reload"
            | "allow_unsigned_plugins"
    ) && !value.is_boolean()
    {
        return Err(CoreError::Invalid(format!("{key} must be true or false")));
    }
    let range = match key {
        "max_concurrent_downloads" => Some(1..=8),
        "max_concurrent_processing" => Some(1..=4),
        "max_workers" => Some(1..=32),
        "download_socket_timeout" => Some(1..=120),
        "download_retries" => Some(0..=10),
        _ => None,
    };
    if let Some(range) = range
        && !value.as_u64().is_some_and(|v| range.contains(&v))
    {
        return Err(CoreError::Invalid(format!(
            "{key} is outside its allowed range"
        )));
    }
    if key == "filename_template"
        && !value
            .as_str()
            .is_some_and(|v| !v.is_empty() && v.len() <= 200 && !v.contains('\0'))
    {
        return Err(CoreError::Invalid("Invalid filename template".into()));
    }
    Ok(())
}

impl AppCore {
    pub fn settings(&self) -> Result<Settings> {
        let db = self.connection()?;
        let mut query = db.prepare("SELECT key,value_json FROM settings ORDER BY key")?;
        let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut values = serde_json::to_value(Settings::default())?
            .as_object()
            .cloned()
            .unwrap_or_default();
        for row in rows {
            let (key, raw) = row?;
            // Runtime state changes independently of the preferences editor.
            // Returning it here would put a stale snapshot in Settings::extra.
            if key.starts_with("rust.") {
                continue;
            }
            // Corrupt known preferences fall back independently, rather than
            // preventing the entire application from starting.
            if let Ok(value) = serde_json::from_str(&raw)
                && validate_setting(&key, &value).is_ok()
            {
                values.insert(key, value);
            }
        }
        Ok(serde_json::from_value(Value::Object(values))?)
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let values = serde_json::to_value(settings)?;
        let values = values
            .as_object()
            .ok_or_else(|| CoreError::Invalid("Settings must be an object".into()))?;
        for (key, value) in values {
            if key.starts_with("rust.") {
                continue;
            }
            validate_setting(key, value)?;
        }
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        for (key, value) in values {
            // Also ignore older/deserialized Settings snapshots that still
            // contain runtime keys. Preserve their latest database values.
            if key.starts_with("rust.") {
                continue;
            }
            tx.execute("INSERT INTO settings(key,value_json,updated_at) VALUES(?1,?2,?3)
                ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
                rusqlite::params![key, serde_json::to_string(value)?, now()])?;
        }
        tx.commit()?;
        Ok(())
    }
}
