use rusqlite::{Connection, params};
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;
use yt_core::{AppCore, ImportMode, JobStatus, PlaybackState};

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

#[test]
fn schema_one_profiles_add_exclusions_without_changing_media_or_preferences() {
    let temp = TempDir::new().unwrap();
    let core = AppCore::open(temp.path()).unwrap();
    let track = core
        .import_bytes("existing.wav", b"existing media")
        .unwrap();
    core.set_setting("legacy.custom", &json!({"keep":true}))
        .unwrap();
    drop(core);
    let db = Connection::open(temp.path().join("yt-ultimate.sqlite3")).unwrap();
    db.execute_batch(
        "DROP TABLE rust_library_exclusions; DELETE FROM rust_schema_migrations WHERE version=2;",
    )
    .unwrap();
    drop(db);

    let reopened = AppCore::open(temp.path()).unwrap();
    assert_eq!(reopened.track(&track.id).unwrap().path, track.path);
    assert_eq!(
        reopened.setting("legacy.custom").unwrap(),
        Some(json!({"keep":true}))
    );
    reopened.remove_track(&track.id).unwrap();
    assert!(reopened.scan_library().unwrap().tracks.is_empty());
    assert_eq!(reopened.integrity_check().unwrap(), "ok");
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
    core.record_playback(&track.id, 0.4).unwrap();
    assert_eq!(core.load_playback().unwrap(), state);
    assert_eq!(core.history(10).unwrap()[0].track.id, track.id);
    core.remove_track(&track.id).unwrap();
    assert!(core.history(10).unwrap().is_empty());
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
    db.execute_batch("DROP TABLE rust_schema_migrations;DROP TABLE rust_import_sources;DROP TABLE rust_track_preferences;DROP TABLE rust_playback_history;DROP TABLE rust_library_exclusions;
        CREATE TRIGGER trg_playlist_tracks_compact_after_delete AFTER DELETE ON playlist_tracks BEGIN
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
