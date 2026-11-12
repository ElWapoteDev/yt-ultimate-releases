use rusqlite::{Connection, params};
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use yt_core::{
    AppCore, ImportMode, JobStatus, ListenEnd, ListenSource, ListenTarget, MediaItem, PlaybackState,
};

fn wav(path: &Path) {
    let samples = 8000u32;
    let bytes = samples * 2;
    let mut output = Vec::new();
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&(36 + bytes).to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16u32.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&8000u32.to_le_bytes());
    output.extend_from_slice(&16000u32.to_le_bytes());
    output.extend_from_slice(&2u16.to_le_bytes());
    output.extend_from_slice(&16u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&bytes.to_le_bytes());
    output.resize(44 + bytes as usize, 0);
    std::fs::write(path, output).unwrap();
}

#[test]
fn embedded_artwork_is_extracted_without_modifying_source_and_is_reused() {
    use lofty::{
        config::WriteOptions,
        picture::{Picture, PictureType},
        tag::{Tag, TagExt, TagType},
    };
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("tagged.wav");
    wav(&source);
    let cover = include_bytes!("fixtures/cover.png");
    let mut picture = Picture::from_reader(&mut cover.as_slice()).unwrap();
    picture.set_pic_type(PictureType::CoverFront);
    let mut tag = Tag::new(TagType::Id3v2);
    tag.push_picture(picture);
    tag.save_to_path(&source, WriteOptions::default()).unwrap();
    let original = std::fs::read(&source).unwrap();
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let track = core.import_file(&source, ImportMode::Reference).unwrap();
    let artwork = core.artwork_path(&track.id).unwrap().unwrap();
    assert!(artwork.starts_with(core.profile_dir().join("Artwork")));
    assert_eq!(std::fs::read(&artwork).unwrap(), cover);
    assert_eq!(core.cache_artwork(cover).unwrap(), artwork);
    assert_eq!(
        core.media_metadata(&track.id).unwrap().artwork_path,
        Some(artwork.clone())
    );
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(core.profile_dir().join("Artwork"))
            .unwrap()
            .count(),
        1
    );
    assert!(core.cache_artwork(b"<svg onload='unsafe'/>").is_err());
    assert!(core.cache_artwork(&cover[..25]).is_err());
    assert_eq!(std::fs::read(artwork).unwrap(), cover);
}

#[cfg(unix)]
#[test]
fn artwork_cache_refuses_symlinks_outside_profile() {
    let profile = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let core = AppCore::open(profile.path()).unwrap();
    std::os::unix::fs::symlink(outside.path(), profile.path().join("Artwork")).unwrap();
    assert!(
        core.cache_artwork(include_bytes!("fixtures/cover.png"))
            .is_err()
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn cleanup_removes_only_stale_owned_partials_and_preserves_registered_files() {
    use std::{
        fs::FileTimes,
        time::{Duration, SystemTime},
    };
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let artwork = core
        .cache_artwork(include_bytes!("fixtures/cover.png"))
        .unwrap();
    let update_dir = temp.path().join("Updates/6.0.0");
    std::fs::create_dir_all(&update_dir).unwrap();
    let old = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
    let make_old = |path: &Path| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();
    };
    let stale_update = update_dir.join(".ytu-update-abandoned.part");
    let stale_artwork = temp.path().join("Artwork/.ytu-artwork-abandoned");
    let fresh = update_dir.join(".ytu-update-live.part");
    let unknown = update_dir.join("keep.part");
    let installer = update_dir.join("installer.exe");
    let media = temp.path().join("Artwork/.ytu-artwork-original.wav");
    for path in [&stale_update, &stale_artwork, &fresh, &unknown, &installer] {
        std::fs::write(path, b"12345").unwrap();
    }
    wav(&media);
    core.import_file(&media, ImportMode::Reference).unwrap();
    for path in [
        &stale_update,
        &stale_artwork,
        &unknown,
        &installer,
        &media,
        &artwork,
    ] {
        make_old(path);
    }
    #[cfg(unix)]
    let link = {
        let link = update_dir.join(".ytu-update-link.part");
        std::os::unix::fs::symlink(&unknown, &link).unwrap();
        link
    };
    let job = core.create_job("download", &json!({})).unwrap();
    assert!(core.clean_temporary_files().is_err());
    assert!(stale_update.exists());
    core.cancel_job(&job.id).unwrap();
    let report = core.clean_temporary_files().unwrap();
    assert_eq!(report.files_removed, 2);
    assert_eq!(report.bytes_removed, 10);
    assert!(report.errors.is_empty());
    assert!(!stale_update.exists() && !stale_artwork.exists());
    for path in [&fresh, &unknown, &installer, &media, &artwork] {
        assert!(path.exists(), "{}", path.display());
    }
    #[cfg(unix)]
    assert!(
        std::fs::symlink_metadata(link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(core.clean_temporary_files().unwrap().files_removed, 0);
}

#[test]
fn exported_playlist_roundtrips_order_unicode_relative_and_absolute_paths() {
    let temp = TempDir::new().unwrap();
    let library = temp.path().join("library");
    std::fs::create_dir(&library).unwrap();
    let relative = library.join("#Canción con espacios.wav");
    let external = temp.path().join("External song.wav");
    wav(&relative);
    wav(&external);
    let core = AppCore::open_with_library(temp.path().join("profile"), &library).unwrap();
    let first = core.import_file(&relative, ImportMode::Reference).unwrap();
    let second = core.import_file(&external, ImportMode::Reference).unwrap();
    let missing = core
        .import_bytes("missing.wav", b"unavailable media")
        .unwrap();
    let playlist = core.create_playlist("Música").unwrap();
    for track in [&second, &first, &missing] {
        core.add_to_playlist(&playlist.id, &track.id).unwrap();
    }
    std::fs::remove_file(&missing.path).unwrap();
    let destination = library.join("Exportada.m3u8");
    let report = core
        .export_playlist_m3u8(&playlist.id, &destination)
        .unwrap();
    assert_eq!(report.exported_tracks, 2);
    assert_eq!(report.skipped.len(), 1);
    let content = std::fs::read_to_string(&destination).unwrap();
    assert!(content.starts_with("#EXTM3U\n#PLAYLIST:Música\n"));
    assert!(content.contains("./#Canción con espacios.wav"));
    assert!(content.contains("External song.wav"));
    assert!(
        core.export_playlist_m3u8(&playlist.id, &destination)
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), content);
    let imported = core
        .import_paths(&[destination], ImportMode::Reference)
        .unwrap();
    assert!(imported.errors.is_empty());
    let exported = imported
        .playlists
        .iter()
        .find(|p| p.name == "Exportada")
        .unwrap();
    let ids = core
        .playlist_tracks(&exported.id)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![second.id, first.id]);
}

#[test]
fn imported_media_is_probed_deduplicated_and_never_deleted() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("Canción.wav");
    wav(&source);
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let imported = core.import_file(&source, ImportMode::Copy).unwrap();
    assert!((imported.duration - 1.0).abs() < 0.01);
    assert_eq!(imported.media_type, "audio");
    assert_ne!(source, imported.path);
    assert_eq!(
        core.import_file(&source, ImportMode::Copy).unwrap().id,
        imported.id
    );
    assert_eq!(core.library("CANCIÓN").unwrap().len(), 1);
    core.set_favorite(&imported.id, true).unwrap();
    assert!(core.track(&imported.id).unwrap().favorite);
    core.remove_track(&imported.id).unwrap();
    assert!(source.is_file());
    assert!(imported.path.is_file());
    assert!(core.library("").unwrap().is_empty());
}

#[test]
fn removed_media_stays_excluded_after_restart_until_an_explicit_import() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.wav");
    wav(&source);
    let profile = temp.path().join("profile");
    let core = AppCore::open(&profile).unwrap();
    let imported = core.import_file(&source, ImportMode::Copy).unwrap();
    core.remove_track(&imported.id).unwrap();
    assert!(imported.path.is_file());
    assert!(core.scan_library().unwrap().tracks.is_empty());
    drop(core);

    let reopened = AppCore::open(&profile).unwrap();
    let scanned = reopened.scan_library().unwrap();
    assert!(scanned.tracks.is_empty());
    assert!(scanned.errors.is_empty());
    assert!(reopened.library("").unwrap().is_empty());
    let restored = reopened
        .import_paths(std::slice::from_ref(&imported.path), ImportMode::Reference)
        .unwrap();
    assert_eq!(restored.tracks.len(), 1);
    let restored_id = restored.tracks[0].id.clone();
    assert_ne!(restored_id, imported.id);
    assert_eq!(reopened.scan_library().unwrap().tracks[0].id, restored_id);
    reopened.remove_track(&restored_id).unwrap();
    assert!(reopened.scan_library().unwrap().tracks.is_empty());
    // The single-file API is explicit too, including imports made by other callers.
    assert!(
        reopened
            .import_file(&imported.path, ImportMode::Reference)
            .is_ok()
    );
    assert_eq!(reopened.library("").unwrap().len(), 1);
    assert!(source.is_file());
}

/// Build a profile exactly as an earlier Rust release left it: the legacy
/// baseline, the historical migrations up to `version`, one managed track
/// that is a favorite with two plays, and a custom preference.
fn historical_rust_profile(version: i64) -> (TempDir, String) {
    let temp = TempDir::new().unwrap();
    let library = temp.path().join("Library");
    std::fs::create_dir(&library).unwrap();
    wav(&library.join("existing.wav"));
    let db = Connection::open(temp.path().join("yt-ultimate.sqlite3")).unwrap();
    db.execute_batch(include_str!("../src/schema.sql")).unwrap();
    db.execute_batch("CREATE TABLE rust_schema_migrations(version INTEGER PRIMARY KEY,applied_at REAL NOT NULL);")
        .unwrap();
    db.execute_batch(include_str!("../src/migrations/0001_rust_tables.sql"))
        .unwrap();
    if version >= 2 {
        db.execute_batch(include_str!(
            "../src/migrations/0002_library_exclusions.sql"
        ))
        .unwrap();
    }
    for applied in 1..=version {
        db.execute(
            "INSERT INTO rust_schema_migrations(version,applied_at) VALUES(?1,1)",
            [applied],
        )
        .unwrap();
    }
    db.execute_batch(
        "INSERT INTO tracks(id,storage_kind,relative_path,filename,title,artist,media_type,duration,created_at,updated_at)
           VALUES('existing','managed','existing.wav','existing.wav','Existing','Artist','audio',1,1,5);
         INSERT INTO rust_track_preferences(track_id,favorite) VALUES('existing',1);
         INSERT INTO rust_playback_history(track_id,played_at,position_seconds) VALUES('existing',10,0),('existing',20,0.5);
         INSERT INTO settings(key,value_json,updated_at) VALUES('legacy.custom','{\"keep\":true}',1);",
    )
    .unwrap();
    (temp, "existing".into())
}

#[test]
fn earlier_rust_profiles_migrate_to_the_current_schema_keeping_user_data() {
    for version in [1, 2] {
        let (temp, id) = historical_rust_profile(version);
        let core = AppCore::open(temp.path()).unwrap();
        assert!(!core.track(&id).unwrap().missing);
        assert!(core.track(&id).unwrap().favorite, "schema {version}");
        let favorites = core.favorites().unwrap();
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].added_at, 5.0);
        let signals = core.listening_signals(10).unwrap();
        assert_eq!(signals.len(), 2);
        assert_eq!(signals[0].started_at, 20.0);
        assert_eq!(signals[0].end, ListenEnd::Unknown);
        assert_eq!(signals[0].source, ListenSource::User);
        assert_eq!(signals[0].artist, "Artist");
        assert_eq!(
            core.setting("legacy.custom").unwrap(),
            Some(json!({"keep":true}))
        );
        let backups = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("pre-schema-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1, "schema {version}");
        let backup = Connection::open(backups[0].path()).unwrap();
        assert_eq!(
            backup
                .query_row("SELECT COUNT(*) FROM rust_playback_history", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            2
        );
        core.remove_track(&id).unwrap();
        assert!(core.scan_library().unwrap().tracks.is_empty());
        assert!(core.favorites().unwrap().is_empty());
        assert!(core.listening_signals(10).unwrap().is_empty());
        assert_eq!(core.integrity_check().unwrap(), "ok");
        drop(core);
        AppCore::open(temp.path()).unwrap();
        let backups_after_reopen = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("pre-schema-") && name.ends_with(".sqlite3")
            })
            .count();
        assert_eq!(backups_after_reopen, 1);
    }
}

#[test]
fn m3u_preserves_order_and_merges_reimports_without_duplicates() {
    let temp = TempDir::new().unwrap();
    wav(&temp.path().join("first.wav"));
    wav(&temp.path().join("second.wav"));
    let playlist = temp.path().join("My playlist.m3u8");
    std::fs::write(&playlist, "\u{feff}#EXTM3U\nsecond.wav\nfirst.wav\nsecond.wav\nhttps://example.invalid/audio.mp3\nmissing.wav\n").unwrap();
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let report = core
        .import_paths(std::slice::from_ref(&playlist), ImportMode::Copy)
        .unwrap();
    assert_eq!(report.tracks.len(), 2);
    assert_eq!(report.errors.len(), 2);
    assert_eq!(report.playlists.len(), 1);
    let imported = &report.playlists[0];
    assert_eq!(imported.track_count, 2);
    assert_eq!(
        core.playlist_tracks(&imported.id)
            .unwrap()
            .iter()
            .map(|track| track.title.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
    core.import_paths(&[playlist], ImportMode::Copy).unwrap();
    assert_eq!(core.playlists().unwrap().len(), 1);
    assert_eq!(core.playlists().unwrap()[0].track_count, 2);
}

#[test]
fn m3u_reuses_existing_playlist_with_sqlite_case_insensitive_name() {
    let temp = TempDir::new().unwrap();
    wav(&temp.path().join("song.wav"));
    let playlist_path = temp.path().join("mix.m3u");
    std::fs::write(&playlist_path, "song.wav\n").unwrap();
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let original = core.create_playlist("MiX").unwrap();
    let report = core
        .import_paths(&[playlist_path], ImportMode::Copy)
        .unwrap();
    assert!(report.errors.is_empty());
    assert_eq!(report.playlists.len(), 1);
    assert_eq!(report.playlists[0].id, original.id);
    assert_eq!(report.playlists[0].name, "MiX");
    assert_eq!(report.playlists[0].track_count, 1);
    assert_eq!(core.playlists().unwrap().len(), 1);
}

#[test]
fn concurrent_imports_share_one_record_and_managed_copy() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("song.wav");
    wav(&source);
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let workers = (0..8)
        .map(|_| {
            let core = core.clone();
            let source = source.clone();
            std::thread::spawn(move || core.import_file(&source, ImportMode::Copy).unwrap().id)
        })
        .collect::<Vec<_>>();
    let ids = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 1);
    assert_eq!(std::fs::read_dir(core.library_dir()).unwrap().count(), 1);
}

#[test]
fn playlist_reorder_is_atomic_and_validates_permutations() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let mut ids = Vec::new();
    for i in 0..4 {
        let file = temp.path().join(format!("{i}.wav"));
        wav(&file);
        ids.push(core.import_file(&file, ImportMode::Reference).unwrap().id);
    }
    let playlist = core.create_playlist("Favorites").unwrap();
    for id in &ids {
        core.add_to_playlist(&playlist.id, id).unwrap();
        core.add_to_playlist(&playlist.id, id).unwrap();
    }
    assert_eq!(core.playlists().unwrap()[0].track_count, 4);
    assert!(
        core.reorder_playlist(&playlist.id, &[ids[0].clone(), ids[0].clone()])
            .is_err()
    );
    let reversed = ids.iter().rev().cloned().collect::<Vec<_>>();
    core.reorder_playlist(&playlist.id, &reversed).unwrap();
    assert_eq!(
        core.playlist_tracks(&playlist.id)
            .unwrap()
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>(),
        reversed
    );
    core.remove_from_playlist(&playlist.id, &ids[2]).unwrap();
    core.remove_track(&ids[1]).unwrap();
    assert_eq!(
        core.playlist_tracks(&playlist.id)
            .unwrap()
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>(),
        vec![ids[3].clone(), ids[0].clone()]
    );
    core.delete_playlist(&playlist.id).unwrap();
    assert!(core.playlists().unwrap().is_empty());
    assert_eq!(core.library("").unwrap().len(), 3);
    assert_eq!(core.integrity_check().unwrap(), "ok");
}

#[test]
fn settings_are_validated_and_unknown_preferences_survive() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    core.set_setting("legacy.custom", &json!({"value":"retained"}))
        .unwrap();
    let mut settings = core.settings().unwrap();
    settings.audio_quality = "320".into();
    core.save_settings(&settings).unwrap();
    assert_eq!(
        core.settings().unwrap().extra["legacy.custom"],
        json!({"value":"retained"})
    );
    settings.audio_quality = "invalid".into();
    settings.language = "en".into();
    assert!(core.save_settings(&settings).is_err());
    assert_eq!(core.settings().unwrap().language, "es");
    assert!(
        core.set_setting("max_concurrent_downloads", &json!(0))
            .is_err()
    );
    assert!(core.set_setting("developer_mode", &json!("true")).is_err());
}

#[test]
fn saving_stale_preferences_preserves_current_playback_and_runtime_records() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let original = PlaybackState {
        queue: vec!["old-track".into()],
        current_index: Some(0),
        position_seconds: 10.0,
        ..PlaybackState::default()
    };
    core.save_playback(&original).unwrap();
    core.set_setting("rust.future-state", &json!({"generation":1}))
        .unwrap();
    core.set_setting("legacy.custom", &json!({"preserved":true}))
        .unwrap();
    let mut preferences = core.settings().unwrap();
    assert!(!preferences.extra.contains_key("rust.playback"));
    assert!(!preferences.extra.contains_key("rust.future-state"));
    // Simulate a preferences editor or serialized snapshot from before the fix.
    preferences.extra.insert(
        "rust.playback".into(),
        serde_json::to_value(&original).unwrap(),
    );
    preferences
        .extra
        .insert("rust.future-state".into(), json!({"generation":1}));
    let current = PlaybackState {
        queue: vec!["current-track".into(), "upcoming-track".into()],
        current_index: Some(0),
        position_seconds: 120.0,
        volume: 0.4,
        ..PlaybackState::default()
    };
    core.save_playback(&current).unwrap();
    core.set_setting("rust.future-state", &json!({"generation":2}))
        .unwrap();
    preferences.audio_quality = "320".into();
    core.save_settings(&preferences).unwrap();
    drop(core);

    let reopened = AppCore::open(temp.path()).unwrap();
    assert_eq!(reopened.load_playback().unwrap(), current);
    assert_eq!(
        reopened.setting("rust.future-state").unwrap(),
        Some(json!({"generation":2}))
    );
    assert_eq!(reopened.settings().unwrap().audio_quality, "320");
    assert_eq!(
        reopened.settings().unwrap().extra["legacy.custom"],
        json!({"preserved":true})
    );
}

#[test]
fn jobs_survive_restart_and_terminal_transitions_are_guarded() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let active = core
        .create_job("download", &json!({"url":"https://example.invalid/media"}))
        .unwrap();
    core.update_job(
        &active.id,
        JobStatus::Running,
        20.0,
        "Downloading",
        &json!({}),
        "",
    )
    .unwrap();
    let finished = core.create_job("download", &json!({})).unwrap();
    core.update_job(
        &finished.id,
        JobStatus::Completed,
        99.0,
        "Done",
        &json!({"path":"file.wav"}),
        "",
    )
    .unwrap();
    assert!(
        core.update_job(
            &finished.id,
            JobStatus::Running,
            10.0,
            "Again",
            &json!({}),
            ""
        )
        .is_err()
    );
    assert!(
        core.update_job(
            &active.id,
            JobStatus::Running,
            f64::NAN,
            "Invalid",
            &json!({}),
            ""
        )
        .is_err()
    );
    drop(core);
    let reopened = AppCore::open(temp.path()).unwrap();
    assert_eq!(
        reopened.job(&active.id).unwrap().status,
        JobStatus::Interrupted
    );
    assert_eq!(
        reopened.job(&finished.id).unwrap().status,
        JobStatus::Completed
    );
    assert_eq!(reopened.job(&finished.id).unwrap().progress, 100.0);
    assert_eq!(reopened.jobs().unwrap().len(), 2);
}

#[test]
fn playback_and_history_persist_without_stale_memberships() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let source = temp.path().join("song.wav");
    wav(&source);
    let track = core.import_file(&source, ImportMode::Reference).unwrap();
    let state = PlaybackState {
        queue: vec![track.id.clone()],
        current_index: Some(0),
        position_seconds: 0.4,
        ..Default::default()
    };
    core.save_playback(&state).unwrap();
    core.begin_listen(ListenTarget::Local(&track.id), ListenSource::User)
        .unwrap();
    assert_eq!(core.load_playback().unwrap(), state);
    assert!(matches!(
        core.recently_played(10).unwrap().as_slice(),
        [MediaItem::Local(played)] if played.id == track.id
    ));
    core.remove_track(&track.id).unwrap();
    assert!(core.recently_played(10).unwrap().is_empty());
}

#[test]
fn remote_playback_survives_restart_without_becoming_library_media() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let mut legacy = serde_json::to_value(PlaybackState::default()).unwrap();
    legacy.as_object_mut().unwrap().remove("remote_tracks");
    legacy.as_object_mut().unwrap().remove("session_history");
    core.set_setting("rust.playback", &legacy).unwrap();
    assert!(core.load_playback().unwrap().remote_tracks.is_empty());
    assert!(core.load_playback().unwrap().session_history.is_empty());
    let track = yt_core::Track {
        id: "youtube:abcdefghijk".into(),
        title: "Remote song".into(),
        artist: "Channel".into(),
        album: String::new(),
        filename: String::new(),
        path: Default::default(),
        media_type: "audio".into(),
        extension: String::new(),
        duration: 42.0,
        size_bytes: 0,
        artwork_url: String::new(),
        favorite: false,
        missing: false,
        created_at: 0.0,
        source_url: "https://www.youtube.com/watch?v=abcdefghijk".into(),
    };
    let state = PlaybackState {
        queue: vec![track.id.clone()],
        session_history: vec![track.id.clone()],
        remote_tracks: vec![track],
        current_index: Some(0),
        position_seconds: 2.5,
        ..Default::default()
    };
    core.save_playback(&state).unwrap();
    drop(core);
    let reopened = AppCore::open(temp.path()).unwrap();
    assert_eq!(reopened.load_playback().unwrap(), state);
    assert!(reopened.library("").unwrap().is_empty());
}

#[test]
fn legacy_v4_profile_is_backed_up_and_preserves_ids_plugins_and_order() {
    let temp = TempDir::new().unwrap();
    let library = temp.path().join("legacy-library");
    std::fs::create_dir(&library).unwrap();
    let source = library.join("legacy.wav");
    wav(&source);
    let db = Connection::open(temp.path().join("yt-ultimate.sqlite3")).unwrap();
    db.execute_batch(include_str!("../src/schema.sql")).unwrap();
    db.execute_batch("CREATE TRIGGER trg_playlist_tracks_compact_after_delete AFTER DELETE ON playlist_tracks BEGIN
        UPDATE playlist_tracks SET position=position-1 WHERE playlist_id=OLD.playlist_id AND position>OLD.position;END;").unwrap();
    for i in 0..4 {
        db.execute("INSERT INTO tracks(id,storage_kind,relative_path,filename,title,media_type,created_at,updated_at)
            VALUES(?1,'managed',?2,?2,?1,'audio',1,1)",params![format!("legacy-{i}"),if i==0{"legacy.wav".to_owned()}else{format!("missing-{i}.wav")}]).unwrap();
    }
    db.execute("INSERT INTO playlists(id,name,created_at,updated_at)VALUES('playlist-legacy','My music',1,1)",[]).unwrap();
    for i in 0..4 {
        db.execute("INSERT INTO playlist_tracks(playlist_id,track_id,position,added_at)VALUES('playlist-legacy',?1,?2,1)",params![format!("legacy-{i}"),i]).unwrap();
    }
    db.execute("INSERT INTO plugin_states(plugin_id,enabled,config_json,installed_at,updated_at)VALUES('user.plugin',1,'{\"a\":1}',1,1)",[]).unwrap();
    db.execute(
        "INSERT INTO settings(key,value_json,updated_at)VALUES('audio_quality','\"256\"',1)",
        [],
    )
    .unwrap();
    drop(db);
    let core = AppCore::open_with_library(temp.path(), &library).unwrap();
    assert!(!core.track("legacy-0").unwrap().missing);
    assert!(core.track("legacy-1").unwrap().missing);
    assert_eq!(core.settings().unwrap().audio_quality, "256");
    core.reorder_playlist(
        "playlist-legacy",
        &[
            "legacy-3".into(),
            "legacy-2".into(),
            "legacy-1".into(),
            "legacy-0".into(),
        ],
    )
    .unwrap();
    core.remove_track("legacy-2").unwrap();
    assert_eq!(core.playlist_tracks("playlist-legacy").unwrap().len(), 3);
    let backups = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("pre-rust-"))
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(backups[0].path()).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    let actual = Connection::open(temp.path().join("yt-ultimate.sqlite3")).unwrap();
    assert_eq!(
        actual
            .query_row(
                "SELECT config_json FROM plugin_states WHERE plugin_id='user.plugin'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "{\"a\":1}"
    );
    assert_eq!(core.integrity_check().unwrap(), "ok");
}

#[test]
fn invalid_legacy_paths_cannot_escape_managed_library() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let outside = temp.path().join("outside.wav");
    wav(&outside);
    let db = Connection::open(temp.path().join("yt-ultimate.sqlite3")).unwrap();
    db.execute("INSERT INTO tracks(id,storage_kind,relative_path,filename,title,media_type,created_at,updated_at)
        VALUES('escape','managed','../outside.wav','outside.wav','unsafe','audio',1,1)",[]).unwrap();
    let track = core.track("escape").unwrap();
    assert!(track.missing);
    assert!(track.path.as_os_str().is_empty());
}

#[test]
fn imports_m3u_relative_entries_and_reports_errors_without_losing_successes() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let source = temp.path().join("song.wav");
    wav(&source);
    let playlist = temp.path().join("list.m3u8");
    std::fs::write(
        &playlist,
        "#EXTM3U\nsong.wav\nsong.wav\nmissing.wav\nhttps://example.invalid/a.mp3\n",
    )
    .unwrap();
    let report = core.import_paths(&[playlist], ImportMode::Copy).unwrap();
    assert_eq!(report.tracks.len(), 1);
    assert_eq!(report.errors.len(), 2);
    assert_eq!(core.library("").unwrap().len(), 1);
}

#[test]
fn legacy_artwork_descriptors_resolve_only_inside_allowed_roots() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path().join("profile")).unwrap();
    let source = temp.path().join("song.wav");
    wav(&source);
    let track = core.import_file(&source, ImportMode::Copy).unwrap();
    let artwork = core.profile_dir().join("Artwork");
    std::fs::create_dir(&artwork).unwrap();
    let cover = artwork.join("cover.jpg");
    std::fs::write(&cover, b"image fixture").unwrap();
    let db = Connection::open(core.profile_dir().join("yt-ultimate.sqlite3")).unwrap();
    db.execute(
        "UPDATE tracks SET artwork_path='artwork:cover.jpg' WHERE id=?1",
        [&track.id],
    )
    .unwrap();
    assert_eq!(
        core.artwork_path(&track.id).unwrap(),
        Some(cover.canonicalize().unwrap())
    );
    let outside = temp.path().join("outside.jpg");
    std::fs::write(&outside, b"private fixture").unwrap();
    db.execute(
        "UPDATE tracks SET artwork_path=?1 WHERE id=?2",
        params![format!("external:{}", outside.display()), track.id],
    )
    .unwrap();
    assert_eq!(core.artwork_path(&track.id).unwrap(), None);
    db.execute(
        "UPDATE tracks SET artwork_path='library:../../outside.jpg' WHERE id=?1",
        [&track.id],
    )
    .unwrap();
    assert_eq!(core.artwork_path(&track.id).unwrap(), None);
}

#[test]
fn unsupported_existing_schemas_are_rejected_without_relabeling_or_modification() {
    for version in [3, 99] {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("yt-ultimate.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL);CREATE TABLE preserved(value TEXT);
            INSERT INTO preserved VALUES('keep me');").unwrap();
        db.execute(
            "INSERT INTO schema_migrations(version,name) VALUES(?1,'existing')",
            [version],
        )
        .unwrap();
        assert!(AppCore::open(temp.path()).is_err());
        assert_eq!(
            db.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            version
        );
        assert_eq!(
            db.query_row("SELECT value FROM preserved", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "keep me"
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='rust_schema_migrations'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    let temp = TempDir::new().unwrap();
    let db = Connection::open(temp.path().join("yt-ultimate.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE unknown(value TEXT);INSERT INTO unknown VALUES('retained');")
        .unwrap();
    assert!(AppCore::open(temp.path()).is_err());
    assert_eq!(
        db.query_row("SELECT value FROM unknown", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "retained"
    );
}

#[test]
fn track_metadata_merges_preserve_legacy_values_and_survive_restart() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let track = core.import_bytes("profile.wav", b"fixture").unwrap();
    core.merge_track_metadata(
        &track.id,
        &serde_json::json!({"legacy":{"nested":true},"fingerprint_profile":{"intensity":"strong"}}),
    )
    .unwrap();
    core.merge_track_metadata(
        &track.id,
        &serde_json::json!({"fingerprint_verification":{"status":"changed"}}),
    )
    .unwrap();
    assert!(
        core.merge_track_metadata(&track.id, &serde_json::json!([]))
            .is_err()
    );
    drop(core);
    let core = AppCore::open(temp.path()).unwrap();
    let metadata = core.track_metadata(&track.id).unwrap();
    assert_eq!(metadata["legacy"]["nested"], true);
    assert_eq!(metadata["fingerprint_profile"]["intensity"], "strong");
    assert_eq!(metadata["fingerprint_verification"]["status"], "changed");
    assert_eq!(std::fs::read(track.path).unwrap(), b"fixture");
}

#[test]
fn confirmed_file_deletion_removes_memberships_but_record_removal_preserves_bytes() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let first = core.import_bytes("delete.wav", b"delete fixture").unwrap();
    let second = core.import_bytes("keep.wav", b"keep fixture").unwrap();
    let playlist = core.create_playlist("Deletion test").unwrap();
    core.add_to_playlist(&playlist.id, &first.id).unwrap();
    core.add_to_playlist(&playlist.id, &second.id).unwrap();
    core.delete_track_file(&first.id).unwrap();
    assert!(!first.path.exists());
    assert!(core.track(&first.id).is_err());
    assert_eq!(core.playlist_tracks(&playlist.id).unwrap()[0].id, second.id);
    core.remove_track(&second.id).unwrap();
    assert_eq!(std::fs::read(second.path).unwrap(), b"keep fixture");
}

#[test]
fn file_deletion_refuses_a_replaced_directory_and_keeps_playlist_membership() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let track = core.import_bytes("replaced.wav", b"fixture").unwrap();
    let playlist = core.create_playlist("Keep this entry").unwrap();
    core.add_to_playlist(&playlist.id, &track.id).unwrap();
    std::fs::remove_file(&track.path).unwrap();
    std::fs::create_dir(&track.path).unwrap();
    std::fs::write(track.path.join("untouched.txt"), b"untouched").unwrap();
    assert!(core.delete_track_file(&track.id).is_err());
    assert_eq!(core.playlist_tracks(&playlist.id).unwrap()[0].id, track.id);
    assert_eq!(
        std::fs::read(track.path.join("untouched.txt")).unwrap(),
        b"untouched"
    );
}
