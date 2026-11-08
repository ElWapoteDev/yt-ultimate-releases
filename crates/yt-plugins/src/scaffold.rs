use crate::{PluginError, PluginHost, PluginManifest, Result, JOURNAL_PLUGIN_ID};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

impl PluginHost {
    /// Create a working, disabled Rust/Wasm project with editable Rust source,
    /// SDK and a precompiled starter. Never needs a compiler at application use.
    pub fn scaffold(&self, id: &str, name: &str, description: &str) -> Result<PathBuf> {
        let mut manifest: PluginManifest =
            serde_json::from_slice(include_bytes!("../examples/rust-greeter/plugin.json"))?;
        manifest.id = id.into();
        manifest.name = name.trim().into();
        manifest.description = description.trim().into();
        manifest.version = "0.1.0".into();
        manifest.validate(id)?;
        let encoded = serde_json::to_vec_pretty(&manifest)?;
        self.write_project(
            id,
            &[
                ("plugin.json", &encoded),
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
                (
                    "guest/src/sdk.rs",
                    include_bytes!("../examples/rust-journal/guest/src/sdk.rs"),
                ),
            ],
        )
    }

    pub fn install_journal_example(&self) -> Result<bool> {
        match self.write_project(
            JOURNAL_PLUGIN_ID,
            &[
                (
                    "plugin.json",
                    include_bytes!("../examples/rust-journal/plugin.json"),
                ),
                (
                    "plugin.wasm",
                    include_bytes!("../examples/rust-journal/plugin.wasm"),
                ),
                (
                    "README.md",
                    include_bytes!("../examples/rust-journal/README.md"),
                ),
                (
                    "guest/Cargo.toml",
                    include_bytes!("../examples/rust-journal/guest/Cargo.toml"),
                ),
                (
                    "guest/Cargo.lock",
                    include_bytes!("../examples/rust-journal/guest/Cargo.lock"),
                ),
                (
                    "guest/src/lib.rs",
                    include_bytes!("../examples/rust-journal/guest/src/lib.rs"),
                ),
                (
                    "guest/src/sdk.rs",
                    include_bytes!("../examples/rust-journal/guest/src/sdk.rs"),
                ),
            ],
        ) {
            Ok(_) => Ok(true),
            Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn write_project(&self, id: &str, files: &[(&str, &[u8])]) -> Result<PathBuf> {
        if !crate::manifest::valid_id(id) {
            return Err(PluginError::Invalid("Invalid plugin ID".into()));
        }
        let destination = self.root.join(id);
        fs::create_dir(&destination)?;
        let result = (|| -> Result<()> {
            for (name, bytes) in files {
                let path = destination.join(name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            // This invocation exclusively created the directory; no existing
            // plugin or library file is overwritten or removed.
            let _ = fs::remove_dir_all(&destination);
            return Err(error);
        }
        Ok(destination)
    }
}
