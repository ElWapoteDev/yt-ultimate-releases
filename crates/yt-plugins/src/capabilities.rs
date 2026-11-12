use crate::{manifest, runtime, schema, PluginError, PluginHost, PluginManifest, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEvent {
    pub name: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDelivery {
    pub plugin_id: String,
    pub value: Option<Value>,
    pub error: Option<String>,
    pub library_changed: bool,
}

type HostCallback = Box<dyn FnMut(Value) -> Result<Value> + Send>;

impl PluginHost {
    /// Attach the application's in-process library. Guests only receive the
    /// declared JSON operations, never the connection or filesystem paths.
    pub fn with_library(mut self, library: yt_core::AppCore) -> Self {
        self.library = Some(library);
        self
    }

    pub fn permissions(&self, id: &str) -> Result<Vec<String>> {
        let encoded: Option<String> = self
            .database()?
            .query_row(
                "SELECT permissions_json FROM plugin_permissions WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(encoded
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or_default())
    }

    /// Explicit permission choice. Any change disables the plugin until enabled
    /// again, preventing a live guest from silently acquiring new capabilities.
    pub fn set_permissions(&self, id: &str, permissions: Vec<String>) -> Result<()> {
        let snapshot = self.snapshot(id)?;
        let unique = permissions.iter().collect::<HashSet<_>>();
        if unique.len() != permissions.len()
            || permissions
                .iter()
                .any(|p| !snapshot.manifest.permissions.contains(p))
        {
            return Err(PluginError::Invalid(
                "Permissions must be a subset of those declared by the plugin".into(),
            ));
        }
        let _execution = self
            .execution
            .lock()
            .map_err(|_| invalid("Plugin execution lock is unavailable"))?;
        let mut database = self.database()?;
        let tx = database.transaction()?;
        tx.execute("INSERT INTO plugin_permissions(id,permissions_json) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET permissions_json=excluded.permissions_json", params![id,serde_json::to_string(&permissions)?])?;
        tx.execute("UPDATE plugin_state SET enabled=0 WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn host_callback(
        &self,
        id: &str,
        manifest: &PluginManifest,
    ) -> Result<HostCallback> {
        self.host_callback_with_effects(id, manifest)
            .map(|(callback, _)| callback)
    }

    fn host_callback_with_effects(
        &self,
        id: &str,
        manifest: &PluginManifest,
    ) -> Result<(HostCallback, Arc<AtomicBool>)> {
        let declared = manifest.permissions.clone();
        let permissions = self
            .permissions(id)?
            .into_iter()
            .filter(|permission| declared.contains(permission))
            .collect::<Vec<_>>();
        let id = id.to_owned();
        let host = self.clone();
        let changed = Arc::new(AtomicBool::new(false));
        let effect = changed.clone();
        Ok((
            Box::new(move |request| {
                let write = matches!(
                    request["operation"].as_str(),
                    Some(
                        "library.set_favorite"
                            | "library.update_metadata"
                            | "library.create_playlist"
                            | "library.rename_playlist"
                            | "library.add_to_playlist"
                            | "library.remove_from_playlist"
                            | "library.reorder_playlist"
                    )
                );
                let result = host.handle_call(&id, &permissions, request);
                if write && result.is_ok() {
                    effect.store(true, Ordering::Relaxed);
                }
                result
            }),
            changed,
        ))
    }

    fn handle_call(&self, id: &str, permissions: &[String], request: Value) -> Result<Value> {
        let operation = field(&request, "operation")?;
        let permission = if operation.starts_with("state.") {
            "state"
        } else if matches!(
            operation,
            "library.list" | "library.track" | "library.playlists" | "library.playlist_tracks"
        ) {
            "library.read"
        } else if operation.starts_with("library.") {
            "library.write"
        } else {
            return Err(invalid("Unknown host operation"));
        };
        if !permissions.iter().any(|p| p == permission) {
            return Err(invalid(format!("Permission denied: {permission}")));
        }
        if operation.starts_with("state.") {
            return self.state_call(id, operation, &request);
        }
        let library = self
            .library
            .as_ref()
            .ok_or_else(|| invalid("Library capabilities are unavailable"))?;
        let core = |error: yt_core::CoreError| invalid(error.to_string());
        match operation {
            "library.list" | "library.playlist_tracks" => {
                let tracks = if operation == "library.list" {
                    let query = request["query"].as_str().unwrap_or("");
                    if query.len() > 512 {
                        return Err(invalid("Library query exceeds 512 bytes"));
                    }
                    library.library(query).map_err(core)?
                } else {
                    library
                        .playlist_tracks(field(&request, "playlist_id")?)
                        .map_err(core)?
                };
                let offset = request["offset"].as_u64().unwrap_or(0).min(1_000_000) as usize;
                let limit = request["limit"].as_u64().unwrap_or(50).clamp(1, 100) as usize;
                let total = tracks.len();
                let mut page = Vec::new();
                let mut size = 0;
                for track in tracks.iter().skip(offset).take(limit) {
                    let value = public_track(track);
                    let bytes = serde_json::to_vec(&value)?.len();
                    if size + bytes > 48 * 1024 {
                        break;
                    }
                    size += bytes;
                    page.push(value);
                }
                let next_offset = offset + page.len();
                Ok(json!({"tracks":page,"total":total,"offset":offset,"next_offset":next_offset}))
            }
            "library.track" => Ok(public_track(
                &library.track(field(&request, "id")?).map_err(core)?,
            )),
            "library.playlists" => Ok(serde_json::to_value(library.playlists().map_err(core)?)?),
            "library.set_favorite" => {
                let favorite = request["favorite"]
                    .as_bool()
                    .ok_or_else(|| invalid("favorite must be boolean"))?;
                library
                    .set_favorite(field(&request, "id")?, favorite)
                    .map_err(core)?;
                Ok(Value::Null)
            }
            "library.update_metadata" => {
                let title = field(&request, "title")?;
                let artist = field(&request, "artist")?;
                if [title, artist].iter().any(|value| value.len() > 4096) {
                    return Err(invalid("Metadata field exceeds 4096 bytes"));
                }
                let track = library.track(field(&request, "id")?).map_err(core)?;
                Ok(public_track(
                    &library
                        .update_track_metadata(
                            &track.id,
                            title,
                            artist,
                            &track.artwork_url,
                            &track.source_url,
                        )
                        .map_err(core)?,
                ))
            }
            "library.create_playlist" => Ok(serde_json::to_value(
                library
                    .create_playlist(field(&request, "name")?)
                    .map_err(core)?,
            )?),
            "library.rename_playlist" => {
                library
                    .rename_playlist(field(&request, "playlist_id")?, field(&request, "name")?)
                    .map_err(core)?;
                Ok(Value::Null)
            }
            "library.add_to_playlist" => {
                library
                    .add_to_playlist(field(&request, "playlist_id")?, field(&request, "id")?)
                    .map_err(core)?;
                Ok(Value::Null)
            }
            "library.remove_from_playlist" => {
                library
                    .remove_from_playlist(field(&request, "playlist_id")?, field(&request, "id")?)
                    .map_err(core)?;
                Ok(Value::Null)
            }
            "library.reorder_playlist" => {
                let ids: Vec<String> = serde_json::from_value(request["ids"].clone())?;
                if ids.len() > 1024 {
                    return Err(invalid("A plugin may reorder at most 1024 tracks per call"));
                }
                library
                    .reorder_playlist(field(&request, "playlist_id")?, &ids)
                    .map_err(core)?;
                Ok(Value::Null)
            }
            _ => Err(invalid("Unknown or unsupported library operation")),
        }
    }

    fn state_call(&self, id: &str, operation: &str, request: &Value) -> Result<Value> {
        let key = field(request, "key")?;
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err(invalid(
                "State keys must contain 1–128 bytes without control characters",
            ));
        }
        let mut database = self.database()?;
        match operation {
            "state.get" => {
                let encoded: Option<String> = database
                    .query_row(
                        "SELECT value_json FROM plugin_data WHERE plugin_id=?1 AND key=?2",
                        params![id, key],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(encoded
                    .map(|encoded| serde_json::from_str(&encoded))
                    .transpose()?
                    .unwrap_or(Value::Null))
            }
            "state.set" => {
                let encoded = serde_json::to_string(&request["value"])?;
                if encoded.len() > 48 * 1024 {
                    return Err(invalid("A plugin state value may not exceed 48 KiB"));
                }
                let tx = database.transaction()?;
                let (count,size): (u64,u64) = tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM plugin_data WHERE plugin_id=?1 AND key!=?2",params![id,key],|row|Ok((row.get(0)?,row.get(1)?)))?;
                if count >= 1024 || size + encoded.len() as u64 > 4 * 1024 * 1024 {
                    return Err(invalid("Plugin state quota exceeded (1024 keys / 4 MiB)"));
                }
                tx.execute("INSERT INTO plugin_data(plugin_id,key,value_json) VALUES(?1,?2,?3) ON CONFLICT(plugin_id,key) DO UPDATE SET value_json=excluded.value_json",params![id,key,encoded])?;
                tx.commit()?;
                Ok(Value::Null)
            }
            "state.delete" => {
                database.execute(
                    "DELETE FROM plugin_data WHERE plugin_id=?1 AND key=?2",
                    params![id, key],
                )?;
                Ok(Value::Null)
            }
            _ => Err(invalid("Unknown state operation")),
        }
    }

    /// Deliver a bounded event to explicitly enabled subscribers. Failures are
    /// isolated per plugin. No recursive emission or background guest survives
    /// this call; invoke on a worker so guest work never blocks the UI thread.
    pub fn dispatch_event(&self, event: PluginEvent) -> Result<Vec<EventDelivery>> {
        if !manifest::valid_id(&event.name) || serde_json::to_vec(&event.payload)?.len() > 32 * 1024
        {
            return Err(invalid("Invalid event name or payload larger than 32 KiB"));
        }
        let payload = redact(event.payload);
        let _execution = self
            .execution
            .lock()
            .map_err(|_| invalid("Plugin execution lock is unavailable"))?;
        let mut deliveries = Vec::new();
        for info in self
            .plugins()?
            .into_iter()
            .filter(|info| info.enabled && info.events.contains(&event.name))
        {
            let mut library_changed = false;
            let result = (|| {
                let snapshot = self.snapshot(&info.id)?;
                let saved = self
                    .state(&info.id)?
                    .filter(|saved| saved.enabled && saved.fingerprint == snapshot.fingerprint)
                    .ok_or_else(|| PluginError::Disabled(info.id.clone()))?;
                let permissions = self.permissions(&info.id)?;
                if !permissions.iter().any(|p| p == "events") {
                    return Err(invalid("Permission denied: events"));
                }
                schema::validate(&snapshot.manifest.config_schema, &saved.config, "config")?;
                let (callback, changed) =
                    self.host_callback_with_effects(&info.id, &snapshot.manifest)?;
                let result = runtime::execute(
                    &snapshot.bytes,
                    &json!({"command":"__event__","arguments":{},"config":saved.config,"event":{"name":event.name,"payload":payload}}),
                    callback,
                );
                library_changed = changed.load(Ordering::Relaxed);
                result
            })();
            let (value, error) = match result {
                Ok(value) => (Some(value), None),
                Err(error) => (None, Some(error.to_string())),
            };
            self.database()?.execute(
                "UPDATE plugin_state SET last_error=?1 WHERE id=?2",
                params![error.as_deref().unwrap_or(""), info.id],
            )?;
            deliveries.push(EventDelivery {
                plugin_id: info.id,
                value,
                error,
                library_changed,
            });
        }
        Ok(deliveries)
    }
}

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::Invalid(message.into())
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| invalid(format!("{name} must be a string")))
}
fn public_track(track: &yt_core::Track) -> Value {
    json!({"id":track.id,"title":track.title,"artist":track.artist,"album":track.album,"duration":track.duration,"media_type":track.media_type,"favorite":track.favorite,"missing":track.missing})
}
fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let secret = [
                        "authorization",
                        "cookie",
                        "cookies",
                        "password",
                        "passwd",
                        "token",
                        "access_token",
                        "refresh_token",
                        "auth_token",
                        "api_key",
                        "secret",
                        "client_secret",
                    ]
                    .contains(&lower.as_str());
                    (
                        key,
                        if secret {
                            json!("[redacted]")
                        } else {
                            redact(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        value => value,
    }
}
