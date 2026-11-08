"""CI-only Electron schema-v4 fixture; Python is not shipped with the Rust app."""

import argparse
import hashlib
import json
import sqlite3
import sys
import wave
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_rows(db):
    assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    assert db.execute("SELECT MAX(version) FROM schema_migrations").fetchone()[0] == 4
    assert db.execute("SELECT id FROM tracks ORDER BY id").fetchall() == [
        ("legacy-managed",), ("legacy-reference",)
    ]
    assert db.execute("SELECT track_id FROM playlist_tracks WHERE playlist_id='legacy-playlist' ORDER BY position").fetchall() == [
        ("legacy-reference",), ("legacy-managed",)
    ]
    assert json.loads(db.execute("SELECT value_json FROM settings WHERE key='audio_quality'").fetchone()[0]) == "256"
    assert json.loads(db.execute("SELECT config_json FROM plugin_states WHERE plugin_id='legacy.example'").fetchone()[0]) == {"user_setting": "preserved"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["seed", "before", "after"])
    parser.add_argument("--profile", required=True, type=Path)
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    args = parser.parse_args()
    database = args.profile / "yt-ultimate.sqlite3"
    manifest = args.evidence / "legacy-file-hashes.json"
    if args.mode == "seed":
        if database.exists():
            raise RuntimeError("Refusing to overwrite an existing profile")
        args.profile.mkdir(parents=True, exist_ok=True)
        args.library.mkdir(parents=True, exist_ok=True)
        args.evidence.mkdir(parents=True, exist_ok=True)
        managed = args.library / "legacy.wav"
        external = args.library.parent / "referenced.wav"
        for path in (managed, external):
            with wave.open(str(path), "wb") as output:
                output.setparams((2, 2, 44100, 0, "NONE", "not compressed"))
                output.writeframes(b"\0\0\0\0" * 44100)
        plugin = args.profile / "Plugins" / "legacy.example" / "plugin.py"
        plugin.parent.mkdir(parents=True)
        plugin.write_text("# Preserved legacy source; never executed by Rust.\n", encoding="utf-8")
        sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "backend"))
        from ytultimate.database import Database
        legacy = Database(database)
        with legacy.connection:
            legacy.connection.execute("INSERT INTO tracks(id,storage_kind,relative_path,filename,title,media_type,created_at,updated_at) VALUES('legacy-managed','managed','legacy.wav','legacy.wav','Legacy managed','audio',1,1)")
            legacy.connection.execute("INSERT INTO tracks(id,storage_kind,external_path,filename,title,media_type,created_at,updated_at) VALUES('legacy-reference','reference',?,'referenced.wav','Legacy reference','audio',1,1)", (str(external.resolve()),))
            legacy.connection.execute("INSERT INTO playlists(id,name,created_at,updated_at) VALUES('legacy-playlist','Preserved playlist',1,1)")
            for index, track in enumerate(("legacy-reference", "legacy-managed")):
                legacy.connection.execute("INSERT INTO playlist_tracks(playlist_id,track_id,position,added_at) VALUES('legacy-playlist',?,?,1)", (track, index))
            legacy.connection.execute("INSERT INTO plugin_states(plugin_id,enabled,config_json,installed_at,updated_at) VALUES('legacy.example',0,?,1,1)", (json.dumps({"user_setting": "preserved"}),))
            for key, value in (("audio_quality", "256"), ("scan_library_on_startup", False)):
                legacy.connection.execute("INSERT INTO settings(key,value_json,updated_at) VALUES(?,?,1)", (key, json.dumps(value)))
        legacy.close()
        manifest.write_text(json.dumps({str(path.resolve()): digest(path) for path in (managed, external, plugin)}, indent=2), encoding="utf-8")
    hashes = json.loads(manifest.read_text(encoding="utf-8"))
    for path, expected in hashes.items():
        assert digest(Path(path)) == expected, f"Preserved file changed: {path}"
    with sqlite3.connect(database) as db:
        verify_rows(db)
        tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if args.mode == "after":
            assert "rust_schema_migrations" in tables
            assert db.execute("SELECT MAX(version) FROM rust_schema_migrations").fetchone()[0] >= 2
        else:
            assert "rust_schema_migrations" not in tables
    backups = list(args.profile.glob("pre-rust-*.sqlite3"))
    if args.mode == "after":
        assert len(backups) == 1, f"Expected one pre-Rust backup, found {len(backups)}"
        with sqlite3.connect(backups[0]) as backup:
            verify_rows(backup)
            assert backup.execute("SELECT COUNT(*) FROM sqlite_master WHERE name='rust_schema_migrations'").fetchone()[0] == 0
    else:
        assert not backups
    print(json.dumps({"phase": args.mode, "profile": str(args.profile), "legacy_rows_preserved": True, "file_hashes_preserved": True, "backup_verified": args.mode == "after"}))


if __name__ == "__main__":
    main()
