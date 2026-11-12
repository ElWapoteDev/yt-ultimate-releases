use yt_core::{AppCore, ImportMode};

#[test]
fn scans_ignore_unpublished_media_without_blocking_explicit_imports() {
    let profile = tempfile::tempdir().unwrap();
    let core = AppCore::open(profile.path()).unwrap();
    let original = core.import_bytes("song.wav", &[1, 2, 3, 4]).unwrap();
    let partial = core.library_dir().join(".ytu-publish-interrupted.wav");
    std::fs::write(&partial, [1, 2, 3]).unwrap();
    core.scan_library().unwrap();
    let tracks = core.library("").unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id, original.id);
    assert!(partial.exists());
    core.import_file(&partial, ImportMode::Reference).unwrap();
    assert_eq!(core.library("").unwrap().len(), 2);
}
