//! Resource-limited, explicitly enabled WebAssembly plugins. Guests receive
//! JSON input and return JSON output; no ambient host capabilities are linked.

mod manifest;
mod runtime;
mod schema;

pub use manifest::{PluginCommand, PluginInfo, PluginManifest};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

pub const EXAMPLE_PLUGIN_ID: &str = "dev.ytultimate.rust-greeter";

#[derive(Clone)]
pub struct PluginHost {
    root: Arc<PathBuf>,
    database: Arc<Mutex<Connection>>,
    execution: Arc<Mutex<()>>,
}

struct Snapshot {
    manifest: PluginManifest,
    bytes: Vec<u8>,
    fingerprint: String,
}
struct SavedState {
    enabled: bool,
    fingerprint: String,
    config: Value,
    error: Option<String>,
}

impl PluginHost {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root.as_ref())?;
        let path = root.join(".rust-plugin-state.sqlite3");
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(PluginError::Invalid(
                "Plugin state may not be a symbolic link".into(),
            ));
        }
        let database = Connection::open(path)?;
        database.busy_timeout(Duration::from_secs(5))?;
        database.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE IF NOT EXISTS plugin_state(id TEXT PRIMARY KEY,enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN(0,1)),fingerprint TEXT NOT NULL DEFAULT '',config_json TEXT NOT NULL DEFAULT '{}',last_error TEXT NOT NULL DEFAULT '');")?;
        Ok(Self {
            root: Arc::new(root),
            database: Arc::new(Mutex::new(database)),
            execution: Arc::new(Mutex::new(())),
        })
    }

    pub fn directory(&self) -> &Path {
        self.root.as_ref()
    }

    fn database(&self) -> Result<MutexGuard<'_, Connection>> {
        self.database
            .lock()
            .map_err(|_| PluginError::Invalid("Plugin state lock is unavailable".into()))
    }

    fn state(&self, id: &str) -> Result<Option<SavedState>> {
        let row: Option<(bool, String, String, String)> = self
            .database()?
            .query_row(
                "SELECT enabled,fingerprint,config_json,last_error FROM plugin_state WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|(enabled, fingerprint, config, error)| {
            Ok(SavedState {
                enabled,
                fingerprint,
                config: serde_json::from_str(&config)?,
                error: (!error.is_empty()).then_some(error),
            })
        })
        .transpose()
    }

    fn snapshot(&self, id: &str) -> Result<Snapshot> {
        if !manifest::valid_id(id) {
            return Err(PluginError::Invalid("Invalid plugin ID".into()));
        }
        let directory = self.root.join(id);
        if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(PluginError::Invalid(
                "Plugin directories may not be symbolic links".into(),
            ));
        }
        let directory = directory
            .canonicalize()
            .map_err(|_| PluginError::NotFound(id.into()))?;
        if !directory.starts_with(self.root.as_ref()) {
            return Err(PluginError::Invalid(
                "Plugin directory escapes its root".into(),
            ));
        }
        let manifest_bytes =
            read_contained(&directory, &directory.join("plugin.json"), 256 * 1024)?;
        let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes)?;
        manifest.validate(id)?;
        let bytes = read_contained(
            &directory,
            &directory.join(&manifest.module),
            runtime::MAX_MODULE_BYTES,
        )?;
        if !bytes.starts_with(b"\0asm\x01\0\0\0") {
            return Err(PluginError::Invalid(
                "Plugin module is not a WebAssembly binary".into(),
            ));
        }
        let mut digest = Sha256::new();
        digest.update((manifest_bytes.len() as u64).to_le_bytes());
        digest.update(&manifest_bytes);
        digest.update(&bytes);
        Ok(Snapshot {
            manifest,
            bytes,
            fingerprint: format!("{:x}", digest.finalize()),
        })
    }

    /// Discovery never executes guest code and never enables a new plugin.
    pub fn plugins(&self) -> Result<Vec<PluginInfo>> {
        let mut entries = fs::read_dir(self.root.as_ref())?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut plugins = Vec::new();
        for entry in entries {
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if plugins.len() >= 128 {
                return Err(PluginError::Invalid(
                    "Plugin directory contains more than 128 plugins".into(),
                ));
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if id.starts_with('.') {
                continue;
            }
            match self.snapshot(&id) {
                Ok(snapshot) => {
                    let saved = self.state(&id)?;
                    let config = saved
                        .as_ref()
                        .map(|saved| saved.config.clone())
                        .unwrap_or_else(|| snapshot.manifest.default_config.clone());
                    let config_error =
                        schema::validate(&snapshot.manifest.config_schema, &config, "config")
                            .err()
                            .map(|error| error.to_string());
                    let changed = saved.as_ref().is_some_and(|state| {
                        !state.fingerprint.is_empty() && state.fingerprint != snapshot.fingerprint
                    });
                    let enabled = saved.as_ref().is_some_and(|state| {
                        state.enabled && state.fingerprint == snapshot.fingerprint
                    }) && config_error.is_none();
                    let manifest = snapshot.manifest;
                    plugins.push(PluginInfo {
                        id: manifest.id,
                        name: manifest.name,
                        version: manifest.version,
                        description: manifest.description,
                        enabled,
                        compatible: true,
                        status: if changed {
                            "changed"
                        } else if enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                        .into(),
                        error: config_error.or_else(|| saved.and_then(|saved| saved.error)),
                        commands: manifest.commands,
                        config_schema: manifest.config_schema,
                        config,
                    });
                }
                Err(error) => {
                    let raw = read_contained(
                        &entry.path(),
                        &entry.path().join("plugin.json"),
                        256 * 1024,
                    )
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .unwrap_or(Value::Null);
                    let legacy = raw["api_version"].as_u64() == Some(1)
                        || raw["entrypoint"]
                            .as_str()
                            .is_some_and(|entry| entry.contains(".py"));
                    plugins.push(PluginInfo{id:id.clone(),name:raw["name"].as_str().unwrap_or(&id).into(),version:raw["version"].as_str().unwrap_or_default().into(),description:raw["description"].as_str().unwrap_or_default().into(),enabled:false,compatible:false,status:if legacy{"legacy"}else{"invalid"}.into(),error:Some(if legacy{"Plugin Python API v1 conservado; requiere una versión WebAssembly API v2".into()}else{error.to_string()}),commands:vec![],config_schema:json!({}),config:json!({})});
                }
            }
        }
        Ok(plugins)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        if !manifest::valid_id(id) {
            return Err(PluginError::Invalid("Invalid plugin ID".into()));
        }
        if !enabled {
            self.database()?
                .execute("UPDATE plugin_state SET enabled=0 WHERE id=?1", [id])?;
            return Ok(());
        }
        let snapshot = self.snapshot(id)?;
        runtime::validate_module(&snapshot.bytes)?;
        let config = self
            .state(id)?
            .map(|state| state.config)
            .unwrap_or_else(|| snapshot.manifest.default_config.clone());
        schema::validate(&snapshot.manifest.config_schema, &config, "config")?;
        self.database()?.execute("INSERT INTO plugin_state(id,enabled,fingerprint,config_json,last_error) VALUES(?1,1,?2,?3,'') ON CONFLICT(id) DO UPDATE SET enabled=1,fingerprint=excluded.fingerprint,config_json=excluded.config_json,last_error=''",params![id,snapshot.fingerprint,serde_json::to_string(&config)?])?;
        Ok(())
    }

    pub fn config(&self, id: &str) -> Result<Value> {
        let snapshot = self.snapshot(id)?;
        Ok(self
            .state(id)?
            .map(|state| state.config)
            .unwrap_or(snapshot.manifest.default_config))
    }

    pub fn set_config(&self, id: &str, config: Value) -> Result<()> {
        let snapshot = self.snapshot(id)?;
        let encoded = serde_json::to_string(&config)?;
        if encoded.len() > runtime::MAX_INPUT_BYTES {
            return Err(PluginError::Invalid(
                "Plugin configuration exceeds 64 KiB".into(),
            ));
        }
        schema::validate(&snapshot.manifest.config_schema, &config, "config")?;
        self.database()?.execute("INSERT INTO plugin_state(id,config_json) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET config_json=excluded.config_json",params![id,encoded])?;
        Ok(())
    }

    pub fn execute(&self, id: &str, command: &str, arguments: Value) -> Result<Value> {
        // One bounded guest runs at a time so many callers cannot multiply the
        // memory cap. UI callers should execute this on a background worker.
        let _execution = self
            .execution
            .lock()
            .map_err(|_| PluginError::Invalid("Plugin execution lock is unavailable".into()))?;
        let snapshot = self.snapshot(id)?;
        let saved = self
            .state(id)?
            .filter(|state| state.enabled && state.fingerprint == snapshot.fingerprint)
            .ok_or_else(|| PluginError::Disabled(id.into()))?;
        let command = snapshot
            .manifest
            .commands
            .iter()
            .find(|declared| declared.id == command)
            .ok_or_else(|| PluginError::NotFound(format!("command {command}")))?;
        schema::validate(&command.arguments_schema, &arguments, "arguments")?;
        schema::validate(&snapshot.manifest.config_schema, &saved.config, "config")?;
        let result = runtime::execute(
            &snapshot.bytes,
            &json!({"command":command.id,"arguments":arguments,"config":saved.config}),
        );
        let error = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
        self.database()?.execute(
            "UPDATE plugin_state SET last_error=?1 WHERE id=?2",
            params![error, id],
        )?;
        result
    }

    /// Install the bundled Rust example without replacing an existing plugin.
    /// It remains disabled until `set_enabled` is explicitly called.
    pub fn install_example(&self) -> Result<bool> {
        let destination = self.root.join(EXAMPLE_PLUGIN_ID);
        match fs::create_dir(&destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        let files: [(&str, &[u8]); 6] = [
            (
                "plugin.json",
                include_bytes!("../examples/rust-greeter/plugin.json"),
            ),
            (
                "plugin.wasm",
                include_bytes!("../examples/rust-greeter/plugin.wasm"),
            ),
            (
                "README.md",
                include_bytes!("../examples/rust-greeter/README.md"),
            ),
            (
                "guest/Cargo.toml",
                include_bytes!("../examples/rust-greeter/guest/Cargo.toml"),
            ),
            (
                "guest/Cargo.lock",
                include_bytes!("../examples/rust-greeter/guest/Cargo.lock"),
            ),
            (
                "guest/src/lib.rs",
                include_bytes!("../examples/rust-greeter/guest/src/lib.rs"),
            ),
        ];
        for (name, bytes) in files {
            if let Some(parent) = destination.join(name).parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination.join(name))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        Ok(true)
    }
}

fn read_contained(root: &Path, path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let root = root.canonicalize()?;
    let path = path.canonicalize()?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(PluginError::Invalid(
            "Plugin file escapes its directory".into(),
        ));
    }
    let file = File::open(path)?;
    if file.metadata()?.len() > maximum as u64 {
        return Err(PluginError::Invalid(
            "Plugin file exceeds the size limit".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(PluginError::Invalid(
            "Plugin file exceeds the size limit".into(),
        ));
    }
    Ok(bytes)
}

pub type Result<T> = std::result::Result<T, PluginError>;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Plugin file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Plugin state: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Plugin JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("Plugin not found: {0}")]
    NotFound(String),
    #[error("Plugin must be explicitly enabled after installation or changes: {0}")]
    Disabled(String),
    #[error("Plugin execution failed: {0}")]
    Runtime(String),
}
