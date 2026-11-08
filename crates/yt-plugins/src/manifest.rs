use crate::{schema, PluginError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Component, Path},
};

fn object_schema() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":true})
}
fn empty_object() -> Value {
    json!({})
}
fn default_module() -> String {
    "plugin.wasm".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginCommand {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "object_schema")]
    pub arguments_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub manifest_version: u32,
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default = "default_module")]
    pub module: String,
    pub commands: Vec<PluginCommand>,
    #[serde(default = "object_schema")]
    pub config_schema: Value,
    #[serde(default = "empty_object")]
    pub default_config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub compatible: bool,
    pub status: String,
    pub error: Option<String>,
    pub commands: Vec<PluginCommand>,
    pub config_schema: Value,
    pub config: Value,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id.as_bytes()[0].is_ascii_lowercase()
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
        && id.split(['.', '_', '-']).all(|segment| !segment.is_empty())
}

impl PluginManifest {
    pub(crate) fn validate(&self, directory: &str) -> Result<()> {
        let bad = |message: &str| PluginError::Invalid(message.into());
        if self.manifest_version != 2 || self.api_version != 2 {
            return Err(bad("Only WebAssembly Plugin API v2 is supported"));
        }
        if !valid_id(&self.id) || self.id != directory {
            return Err(bad("Plugin ID must be valid and match its directory name"));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 160
            || self.description.len() > 4096
            || self.version.is_empty()
            || self.version.len() > 64
        {
            return Err(bad("Invalid plugin name, version or description"));
        }
        let module = Path::new(&self.module);
        if module.as_os_str().is_empty()
            || self.module.len() > 240
            || module
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || module.extension().and_then(|s| s.to_str()) != Some("wasm")
        {
            return Err(bad(
                "Plugin module must be a relative .wasm path inside its directory",
            ));
        }
        if self.commands.is_empty() || self.commands.len() > 64 {
            return Err(bad("Plugin must declare 1–64 commands"));
        }
        let mut seen = HashSet::new();
        for command in &self.commands {
            if !valid_id(&command.id)
                || !seen.insert(&command.id)
                || command.label.trim().is_empty()
                || command.label.len() > 160
                || command.description.len() > 4096
            {
                return Err(bad("Invalid or duplicate plugin command"));
            }
            schema::check_schema(&command.arguments_schema, 0)?;
        }
        if self.config_schema["type"] != "object" {
            return Err(bad("Plugin configuration schema must describe an object"));
        }
        schema::check_schema(&self.config_schema, 0)?;
        schema::validate(&self.config_schema, &self.default_config, "config")?;
        Ok(())
    }
}
