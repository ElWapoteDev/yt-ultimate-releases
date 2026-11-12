use crate::{AppCore, CoreError, Result, now};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::ops::RangeInclusive;

/// How two songs blend during a crossfade.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossfadeCurve {
    /// Constant perceived loudness through the blend.
    #[default]
    EqualPower,
    /// Straight-line gains; the middle of the blend dips slightly.
    Linear,
    /// Slow start and end (smoothstep); the blend feels shorter.
    Smooth,
}

impl CrossfadeCurve {
    pub const ALL: [Self; 3] = [Self::EqualPower, Self::Linear, Self::Smooth];
}

/// How the DJ joins two songs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DjStyle {
    /// A crossfade at the end of each song.
    Classic,
    /// A beat-matched mix placed on the songs' bars, from their analysis; a
    /// crossfade whenever no mix is ready.
    #[default]
    Professional,
    /// Mashups by stems where the songs allow them (the vocal model must be
    /// installed): both instrumentals blend, one song's vocals ride over the
    /// other's instrumental and the other's vocals take over; professional
    /// mixes otherwise. Older settings named it `generative` or `hybrid`.
    #[serde(alias = "generative", alias = "hybrid")]
    Mashup,
}

impl DjStyle {
    pub const ALL: [Self; 3] = [Self::Classic, Self::Professional, Self::Mashup];

    /// Whether the style plans mixes from the songs' analyses (every style
    /// but the classic crossfade).
    pub fn plans_mixes(self) -> bool {
        self != Self::Classic
    }
}

/// How long the DJ's mixes last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixLength {
    Short,
    #[default]
    Medium,
    Long,
}

impl MixLength {
    pub const ALL: [Self; 3] = [Self::Short, Self::Medium, Self::Long];

    /// Bars during which both songs sound in a mix of this length (the
    /// incoming song's rise plus the outgoing song's fade); the planner
    /// shortens it when the songs need a quicker change.
    pub const fn bars(self) -> u8 {
        match self {
            Self::Short => 4,
            Self::Medium => 8,
            Self::Long => 16,
        }
    }
}

/// Playback continuation and transitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackPreferences {
    /// Continue with related music when the queue runs out.
    pub autoplay: bool,
    /// Blend consecutive songs with a crossfade instead of a cut.
    pub dj_mode: bool,
    /// Length of automatic crossfades, in seconds.
    pub crossfade_seconds: u8,
    pub crossfade_curve: CrossfadeCurve,
    /// Also blend, briefly, when the user skips or picks another song.
    pub crossfade_on_skip: bool,
    pub dj_style: DjStyle,
    /// Length of the DJ's beat-matched mixes.
    pub dj_length: MixLength,
    /// How energetic the DJ's mixes are, from soft (0) to
    /// `DJ_INTENSITY.end()`.
    pub dj_intensity: u8,
    /// Show on the progress bar where the next planned mix starts and how
    /// long it lasts.
    pub dj_show_mix: bool,
    /// With the mashup style, fuse every pair with the generative model,
    /// whatever its tempos and keys; otherwise only pairs that suit it.
    pub dj_always_fuse: bool,
}

impl PlaybackPreferences {
    /// Accepted crossfade lengths, in seconds.
    pub const CROSSFADE_SECONDS: RangeInclusive<u8> = 1..=12;
    pub const DEFAULT_CROSSFADE_SECONDS: u8 = 6;
    /// Accepted DJ intensities.
    pub const DJ_INTENSITY: RangeInclusive<u8> = 0..=4;
    pub const DEFAULT_DJ_INTENSITY: u8 = 2;

    pub fn validate(&self) -> Result<()> {
        if !Self::CROSSFADE_SECONDS.contains(&self.crossfade_seconds) {
            return Err(CoreError::Invalid(
                "crossfade_seconds is outside its allowed range".into(),
            ));
        }
        if !Self::DJ_INTENSITY.contains(&self.dj_intensity) {
            return Err(CoreError::Invalid(
                "dj_intensity is outside its allowed range".into(),
            ));
        }
        Ok(())
    }
}

impl Default for PlaybackPreferences {
    fn default() -> Self {
        Self {
            autoplay: true,
            dj_mode: false,
            crossfade_seconds: Self::DEFAULT_CROSSFADE_SECONDS,
            crossfade_curve: CrossfadeCurve::default(),
            crossfade_on_skip: true,
            dj_style: DjStyle::default(),
            dj_length: MixLength::default(),
            dj_intensity: Self::DEFAULT_DJ_INTENSITY,
            dj_show_mix: true,
            dj_always_fuse: false,
        }
    }
}

/// Home recommendations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryPreferences {
    /// Ask YouTube for music related to what the user plays. When false,
    /// recommendations come from the local library only and discovery makes
    /// no network requests.
    pub online: bool,
}

impl Default for DiscoveryPreferences {
    fn default() -> Self {
        Self { online: true }
    }
}

/// Lyrics of what plays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsPreferences {
    /// While the lyrics view is open, look the song up in the LRCLIB online
    /// catalog by its title, artist and duration. When false, only lyrics
    /// stored with library files are shown and lyrics make no network
    /// requests.
    pub online: bool,
}

impl Default for LyricsPreferences {
    fn default() -> Self {
        Self { online: true }
    }
}

/// Visual preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearancePreferences {
    /// Tint the interface with the main color of the current artwork.
    pub dynamic_color: bool,
    /// Look up the album cover of what plays online (by its video id) and
    /// show it instead of the video thumbnail.
    pub album_artwork: bool,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            dynamic_color: true,
            album_artwork: true,
        }
    }
}

/// Where the user is, to show concerts near them: the zone found from the
/// device's location while `detect` is on, otherwise the city and country the
/// user typed. Only names are kept, never the position, and they are matched
/// against event locations locally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "StoredRegion")]
pub struct RegionPreferences {
    pub city: String,
    pub country: String,
    /// Fill `city` and `country` from the device's location, with the
    /// operating system's permission. On by default; a profile that typed a
    /// place before this preference existed reads as off and keeps its place.
    pub detect: bool,
}

impl Default for RegionPreferences {
    fn default() -> Self {
        Self {
            city: String::new(),
            country: String::new(),
            detect: true,
        }
    }
}

/// A stored region group, where `detect` is missing in profiles written
/// before it existed.
#[derive(Default, Deserialize)]
#[serde(default)]
struct StoredRegion {
    city: String,
    country: String,
    detect: Option<bool>,
}

impl From<StoredRegion> for RegionPreferences {
    fn from(stored: StoredRegion) -> Self {
        let mut region = Self {
            city: stored.city,
            country: stored.country,
            detect: true,
        };
        region.detect = stored.detect.unwrap_or(!region.is_set());
        region
    }
}

impl RegionPreferences {
    /// Longest accepted city or country name, in characters.
    pub const MAX_CHARS: usize = 80;

    pub fn validate(&self) -> Result<()> {
        for value in [&self.city, &self.country] {
            if value.chars().count() > Self::MAX_CHARS || value.chars().any(char::is_control) {
                return Err(CoreError::Invalid("Invalid region".into()));
            }
        }
        Ok(())
    }

    /// Whether a city or a country is known, typed or detected.
    pub fn is_set(&self) -> bool {
        !self.city.trim().is_empty() || !self.country.trim().is_empty()
    }

    /// Whether `place` (an event's city and region as a provider prints it)
    /// names the user's city or, when no city was given, their country, as
    /// whole words ignoring accents, case, spacing and punctuation.
    pub fn matches(&self, place: &str) -> bool {
        let wanted = if self.city.trim().is_empty() {
            &self.country
        } else {
            &self.city
        };
        crate::contains_words(place, wanted)
    }
}

fn parse_group<T: DeserializeOwned>(key: &str, value: &Value) -> Result<T> {
    if !value.is_object() {
        return Err(CoreError::Invalid(format!("{key} must be an object")));
    }
    serde_json::from_value(value.clone())
        .map_err(|error| CoreError::Invalid(format!("Invalid value for {key}: {error}")))
}

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
    pub playback: PlaybackPreferences,
    pub discovery: DiscoveryPreferences,
    pub appearance: AppearancePreferences,
    pub region: RegionPreferences,
    pub lyrics: LyricsPreferences,
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
            playback: PlaybackPreferences::default(),
            discovery: DiscoveryPreferences::default(),
            appearance: AppearancePreferences::default(),
            region: RegionPreferences::default(),
            lyrics: LyricsPreferences::default(),
            extra: Map::new(),
        }
    }
}

pub(crate) fn validate_setting(key: &str, value: &Value) -> Result<()> {
    match key {
        "playback" => return parse_group::<PlaybackPreferences>(key, value)?.validate(),
        "discovery" => return parse_group::<DiscoveryPreferences>(key, value).map(drop),
        "appearance" => return parse_group::<AppearancePreferences>(key, value).map(drop),
        "region" => return parse_group::<RegionPreferences>(key, value)?.validate(),
        "lyrics" => return parse_group::<LyricsPreferences>(key, value).map(drop),
        _ => {}
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_match_whole_place_words_ignoring_accents() {
        let city = RegionPreferences {
            city: "Ciudad de Mexico".into(),
            country: "México".into(),
            ..RegionPreferences::default()
        };
        assert!(city.matches("Ciudad de México, CDMX"));
        assert!(!city.matches("Monterrey, NL, México"));
        let country = RegionPreferences {
            city: " ".into(),
            country: "mexico".into(),
            ..RegionPreferences::default()
        };
        assert!(country.matches("Monterrey, NL, México"));
        assert!(!RegionPreferences::default().matches("Guadalajara"));
        let short = RegionPreferences {
            city: "León".into(),
            ..RegionPreferences::default()
        };
        assert!(short.matches("León, Guanajuato"));
        assert!(!short.matches("Leonberg, Alemania"));
    }

    #[test]
    fn regions_detect_unless_an_older_profile_typed_a_place() {
        let region = |value| serde_json::from_value::<RegionPreferences>(value).unwrap();
        assert!(RegionPreferences::default().detect);
        assert!(region(serde_json::json!({})).detect);
        assert!(region(serde_json::json!({"city": " ", "country": ""})).detect);
        let typed = region(serde_json::json!({"city": "Monterrey", "country": "México"}));
        assert!(!typed.detect);
        assert_eq!(typed.city, "Monterrey");
        let chosen = region(serde_json::json!({"city": "Lima", "country": "Perú", "detect": true}));
        assert!(chosen.detect);
        let off = region(serde_json::json!({"city": "", "country": "", "detect": false}));
        assert!(!off.detect);
        assert_eq!(region(serde_json::to_value(&chosen).unwrap()), chosen);
        assert!(validate_setting("region", &serde_json::json!({"detect": "yes"})).is_err());
    }

    #[test]
    fn preference_groups_round_trip_and_corrupt_groups_fall_back() {
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        let defaults = core.settings().unwrap();
        assert!(defaults.playback.autoplay);
        assert!(!defaults.playback.dj_mode);
        assert!(defaults.discovery.online);
        assert!(defaults.lyrics.online);
        assert!(defaults.appearance.dynamic_color);
        assert!(defaults.appearance.album_artwork);
        let mut edited = defaults.clone();
        edited.appearance.album_artwork = false;
        edited.playback.dj_mode = true;
        edited.playback.crossfade_seconds = 9;
        edited.playback.crossfade_curve = CrossfadeCurve::Smooth;
        edited.playback.dj_style = DjStyle::Classic;
        edited.playback.dj_length = MixLength::Long;
        edited.playback.dj_intensity = *PlaybackPreferences::DJ_INTENSITY.end();
        edited.playback.dj_style = DjStyle::Mashup;
        edited.discovery.online = false;
        core.save_settings(&edited).unwrap();
        assert_eq!(core.settings().unwrap(), edited);
        core.connection()
            .unwrap()
            .execute(
                "UPDATE settings SET value_json='{\"crossfade_seconds\":99}' WHERE key='playback'",
                [],
            )
            .unwrap();
        let recovered = core.settings().unwrap();
        assert_eq!(recovered.playback, PlaybackPreferences::default());
        assert!(!recovered.discovery.online);
    }

    #[test]
    fn invalid_preference_groups_are_rejected_before_saving() {
        for (key, value) in [
            ("playback", serde_json::json!({"crossfade_seconds": 0})),
            ("playback", serde_json::json!({"crossfade_curve": "bounce"})),
            ("playback", serde_json::json!({"dj_intensity": 5})),
            ("playback", serde_json::json!({"dj_style": "remix"})),
            ("playback", serde_json::json!({"dj_length": 16})),
            ("playback", serde_json::json!(true)),
            ("discovery", serde_json::json!({"online": "yes"})),
            ("lyrics", serde_json::json!({"online": 1})),
            ("appearance", serde_json::json!([])),
        ] {
            assert!(validate_setting(key, &value).is_err(), "{key}: {value}");
        }
        assert!(validate_setting("playback", &serde_json::json!({})).is_ok());
        let mut settings = Settings::default();
        settings.playback.crossfade_seconds = *PlaybackPreferences::CROSSFADE_SECONDS.end() + 1;
        let profile = tempfile::tempdir().unwrap();
        let core = AppCore::open(profile.path()).unwrap();
        assert!(core.save_settings(&settings).is_err());
    }
}
