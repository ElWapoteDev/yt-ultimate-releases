//! Files that ship beside the application's executable: helper programs,
//! native libraries and models, in the layouts the macOS bundle and the
//! Windows installer use. An environment variable overrides each one during
//! development, and debug builds also find models in the crate that owns them.

use std::path::{Path, PathBuf};

/// The program `name` beside the executable, or the path in `variable`.
pub(crate) fn program(variable: &str, name: &str) -> Option<PathBuf> {
    overridden(variable).or_else(|| {
        let path = executable_folder()?.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        path.is_file().then_some(path)
    })
}

/// The native library `file` in the macOS bundle's `Frameworks` folder or
/// beside the Windows executable, or the path in `variable`.
pub(crate) fn library(variable: &str, file: &str) -> Option<PathBuf> {
    overridden(variable).or_else(|| {
        let folder = executable_folder()?;
        [folder.join("../Frameworks").join(file), folder.join(file)]
            .into_iter()
            .find(|path| path.is_file())
    })
}

/// The model `file` in the application's `models` resources, or the path in
/// `variable`. Debug builds also look in `source`, a folder relative to this
/// crate's manifest, so the application runs from the source tree.
pub(crate) fn model(variable: &str, file: &str, source: &str) -> Option<PathBuf> {
    if let Some(path) = overridden(variable) {
        return Some(path);
    }
    let folder = executable_folder()?;
    let mut paths = vec![
        folder.join("../Resources/models").join(file),
        folder.join("resources/models").join(file),
        folder.join("models").join(file),
    ];
    if cfg!(debug_assertions) {
        paths.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(source)
                .join(file),
        );
    }
    paths.into_iter().find(|path| path.is_file())
}

/// The folder `name` in the application's `models` resources, or the path in
/// `variable`, when it exists.
pub(crate) fn models_folder(variable: &str, name: &str) -> Option<PathBuf> {
    if let Some(path) = overridden(variable) {
        return path.is_dir().then_some(path);
    }
    let folder = executable_folder()?;
    [
        folder.join("../Resources/models").join(name),
        folder.join("resources/models").join(name),
        folder.join("models").join(name),
    ]
    .into_iter()
    .find(|path| path.is_dir())
}

fn overridden(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}

fn executable_folder() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}
