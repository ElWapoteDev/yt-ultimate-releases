use crate::{AppCore, CleanupReport, CoreError, ImportFailure, Result};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const MINIMUM_AGE: Duration = Duration::from_secs(24 * 60 * 60);

impl AppCore {
    /// Remove abandoned application-owned partial files older than one day.
    /// Original media, final artwork, verified installers, symlinks and unknown
    /// files are never candidates. Does not inspect the operating system's temp
    /// directory or legacy library files whose ownership cannot be established.
    pub fn clean_temporary_files(&self) -> Result<CleanupReport> {
        let active: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE status IN ('pending','queued','running','cancelling','processing'))",
            [], |row| row.get(0),
        )?;
        if active {
            return Err(CoreError::Invalid(
                "Espera a que terminen los trabajos antes de limpiar temporales".into(),
            ));
        }
        let mut protected = HashSet::new();
        for track in self.library("")? {
            if let Ok(path) = track.path.canonicalize() {
                protected.insert(path);
            }
            if let Some(path) = self.artwork_path(&track.id)? {
                protected.insert(path);
            }
        }
        let mut report = CleanupReport::default();
        for (directory, update) in [("Artwork", false), ("Updates", true)] {
            self.clean_directory(
                &self.profile_dir().join(directory),
                update,
                usize::from(update),
                &protected,
                &mut report,
            );
        }
        Ok(report)
    }

    fn clean_directory(
        &self,
        directory: &Path,
        update: bool,
        depth: usize,
        protected: &HashSet<PathBuf>,
        report: &mut CleanupReport,
    ) {
        let result = (|| -> Result<()> {
            let metadata = match std::fs::symlink_metadata(directory) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Ok(());
            }
            let root = directory.canonicalize()?;
            if !root.starts_with(self.profile_dir()) {
                return Ok(());
            }
            for entry in std::fs::read_dir(directory)? {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        report.errors.push(ImportFailure {
                            path: directory.to_owned(),
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                let path = entry.path();
                let result = (|| -> Result<()> {
                    let metadata = std::fs::symlink_metadata(&path)?;
                    if metadata.file_type().is_symlink() {
                        return Ok(());
                    }
                    if metadata.is_dir() && depth > 0 {
                        self.clean_directory(&path, update, depth - 1, protected, report);
                        return Ok(());
                    }
                    let filename = entry.file_name();
                    let filename = filename.to_string_lossy();
                    let recognized = if update {
                        filename.starts_with(".ytu-update-") && filename.ends_with(".part")
                    } else {
                        filename.starts_with(".ytu-artwork-")
                    };
                    if !recognized || !metadata.is_file() {
                        return Ok(());
                    }
                    let path = path.canonicalize()?;
                    if !path.starts_with(&root) || protected.contains(&path) {
                        return Ok(());
                    }
                    let age = SystemTime::now()
                        .duration_since(metadata.modified()?)
                        .unwrap_or_default();
                    if age < MINIMUM_AGE {
                        return Ok(());
                    }
                    std::fs::remove_file(&path)?;
                    report.files_removed += 1;
                    report.bytes_removed += metadata.len();
                    Ok(())
                })();
                if let Err(error) = result {
                    report.errors.push(ImportFailure {
                        path,
                        message: error.to_string(),
                    });
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            report.errors.push(ImportFailure {
                path: directory.to_owned(),
                message: error.to_string(),
            });
        }
    }
}
