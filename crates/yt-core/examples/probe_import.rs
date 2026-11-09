//! Read-only source import probe against an isolated, disposable library profile.
use sha2::{Digest, Sha256};
use std::{error::Error, path::PathBuf};
use yt_core::{AppCore, ImportMode};

fn main() -> Result<(), Box<dyn Error>> {
    let profile = tempfile::tempdir()?;
    let core = AppCore::open(profile.path())?;
    for path in std::env::args_os().skip(1).map(PathBuf::from) {
        let before = Sha256::digest(std::fs::read(&path)?);
        let track = core.import_file(&path, ImportMode::Reference)?;
        assert_eq!(before, Sha256::digest(std::fs::read(&path)?));
        println!("{}", serde_json::to_string(&track)?);
    }
    Ok(())
}
