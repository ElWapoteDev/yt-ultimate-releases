from __future__ import annotations

import json
import unittest
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest import mock

from ytultimate.database import Database
from ytultimate.services import library as library_module
from ytultimate.services.library import (
    LibraryService,
    MediaInUseError,
    UnsafeLibraryPathError,
)
from ytultimate.services.playlists import (
    PlaylistConflictError,
    PlaylistService,
)
from ytultimate.services.settings import SettingsService


class ServiceTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.config = SimpleNamespace(
            database_path=self.root / "state.sqlite3",
            library_dir=self.root / "Library",
            thumbnail_dir=self.root / "Artwork",
            ffprobe_path="",
            developer_mode_override=None,
        )
        self.database = Database(self.config)
        self.library = LibraryService(self.config, self.database)

    def tearDown(self) -> None:
        self.database.close()
        self.temporary_directory.cleanup()

    @staticmethod
    def _media(path: Path, content: bytes = b"media") -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path


class LibraryTests(ServiceTestCase):
    def test_scan_reconciles_nested_media_and_reports_missing_files(self) -> None:
        song = self._media(
            self.config.library_dir / "Artist" / "Song.mp3",
            b"audio",
        )
        self._media(self.config.library_dir / "Clip.mp4", b"video")
        self._media(self.config.library_dir / ".hidden.mp3")
        self._media(self.config.library_dir / ".private" / "secret.flac")
        (self.config.library_dir / "notes.txt").write_text(
            "not media",
            encoding="utf-8",
        )

        summary = self.library.reconcile()
        self.assertEqual(summary["discovered"], 2)
        self.assertEqual(summary["added"], 2)
        self.assertEqual(summary["total"], 2)

        tracks = self.library.list_tracks(order="title")
        self.assertEqual(len(tracks), 2)
        identifiers = {track["id"] for track in tracks}
        self.assertEqual(
            {str(uuid.UUID(identifier)) for identifier in identifiers},
            identifiers,
        )
        nested = next(
            track for track in tracks if track["filename"] == "Song.mp3"
        )
        self.assertEqual(nested["relative_path"], "Artist/Song.mp3")
        self.assertEqual(nested["path"], "/files/Artist/Song.mp3")
        self.assertEqual(nested["type"], "audio")

        song.unlink()
        second_summary = self.library.reconcile()
        self.assertEqual(second_summary["missing"], 1)
        self.assertEqual(len(self.library.list_tracks()), 1)
        self.assertEqual(
            len(self.library.list_tracks(include_missing=True)),
            2,
        )
        stats = self.library.stats()
        self.assertEqual(stats["total_files"], 1)
        self.assertEqual(stats["video_count"], 1)
        self.assertEqual(stats["missing_count"], 1)

    def test_copy_reference_unique_names_and_non_destructive_defaults(
        self,
    ) -> None:
        source_directory = self.root / "Incoming"
        source = self._media(source_directory / "Song.mp3", b"one")
        external = self._media(source_directory / "External.flac", b"two")

        copied = self.library.import_file(source, probe=False)
        copied_again = self.library.import_file(source, probe=False)
        self.assertTrue(source.exists())
        self.assertEqual(copied["filename"], "Song.mp3")
        self.assertEqual(copied_again["filename"], "Song (2).mp3")
        self.assertNotEqual(copied["id"], copied_again["id"])

        referenced = self.library.import_file(
            external,
            mode="reference",
            probe=False,
        )
        self.assertEqual(referenced["storage_kind"], "reference")
        self.assertIsNone(referenced["relative_path"])
        deletion = self.library.delete_track(referenced["id"])
        self.assertFalse(deletion["file_deleted"])
        self.assertTrue(external.exists())

        copied_path = Path(copied["local_path"])
        deletion = self.library.delete_track(copied["id"])
        self.assertTrue(deletion["file_deleted"])
        self.assertFalse(copied_path.exists())

    def test_windows_delete_retries_then_reports_file_in_use(self) -> None:
        song = self.library.import_file(
            self._media(self.root / "Incoming" / "Playing.mp3"),
            probe=False,
        )
        song_path = Path(song["local_path"])
        real_replace = library_module.os.replace
        attempts: list[Path] = []

        def locked_once(source, destination):
            attempts.append(Path(source))
            if len(attempts) == 1:
                raise PermissionError(32, "in use")
            return real_replace(source, destination)

        with (
            mock.patch.object(library_module, "_WINDOWS_FILE_LOCKS", True),
            mock.patch.object(library_module.time, "sleep"),
            mock.patch.object(library_module.os, "replace", side_effect=locked_once),
        ):
            deletion = self.library.delete_track(song["id"])
        self.assertTrue(deletion["file_deleted"])
        self.assertEqual(attempts, [song_path, song_path])
        self.assertFalse(song_path.exists())

        locked = self.library.import_file(
            self._media(self.root / "Incoming" / "Locked.mp3"),
            probe=False,
        )
        locked_path = Path(locked["local_path"])
        with (
            mock.patch.object(library_module, "_WINDOWS_FILE_LOCKS", True),
            mock.patch.object(library_module.time, "sleep"),
            mock.patch.object(
                library_module.os,
                "replace",
                side_effect=PermissionError(32, "in use"),
            ),
            self.assertRaises(MediaInUseError),
        ):
            self.library.delete_track(locked["id"])
        self.assertTrue(locked_path.exists())
        self.assertEqual(
            self.library.get_track(locked["id"])["id"],
            locked["id"],
        )

    def test_folder_import_preserves_structure_and_resolves_artwork(self) -> None:
        folder = self.root / "Album"
        self._media(folder / "Disc 1" / "Track.m4a")
        result = self.library.import_paths([folder], probe=False)
        self.assertEqual(result["imported"], 1)
        track = result["tracks"][0]
        self.assertEqual(
            track["relative_path"],
            "Album/Disc 1/Track.m4a",
        )
        media_path = self.library.resolve_content(track["id"])
        artwork = media_path.with_suffix(".jpg")
        artwork.write_bytes(b"image")
        self.assertEqual(
            self.library.resolve_artwork(track["id"]),
            artwork.resolve(),
        )

    def test_m3u_import_resolves_local_entries_and_preserves_membership(self) -> None:
        folder = self.root / "Incoming Mix"
        first = self._media(folder / "First.mp3", b"one")
        second = self._media(folder / "nested" / "Second.flac", b"two")
        playlist = folder / "Road Trip.m3u8"
        playlist.write_text(
            "\ufeff#EXTM3U\n"
            "#EXTINF:1,First\n"
            "First.mp3\n"
            "nested/Second.flac\n"
            "https://example.invalid/remote.mp3\n",
            encoding="utf-8",
        )

        result = self.library.import_paths([playlist], probe=False)

        self.assertEqual(result["imported"], 2)
        self.assertEqual(len(result["playlists"]["Road Trip"]), 2)
        self.assertEqual(
            {track["filename"] for track in result["tracks"]},
            {first.name, second.name},
        )
        self.assertEqual(len(result["errors"]), 1)
        self.assertIn("Remote playlist", result["errors"][0]["error"])

    def test_content_resolution_rejects_tampered_relative_paths(self) -> None:
        source = self._media(self.config.library_dir / "Safe.mp3")
        track = self.library.register_path(source, probe=False)
        outside = self._media(self.root / "outside.mp3")
        self.database.execute(
            "UPDATE tracks SET relative_path = ? WHERE id = ?",
            ("../outside.mp3", track["id"]),
        )
        with self.assertRaises(UnsafeLibraryPathError):
            self.library.resolve_content(track["id"])
        self.assertTrue(outside.exists())

    def test_legacy_json_migration_is_repeatable(self) -> None:
        self._media(self.config.library_dir / "Legacy.mp3")
        (self.config.library_dir / "metadata.json").write_text(
            json.dumps(
                {
                    "Legacy.mp3": {
                        "title": "Imported title",
                        "channel": "Imported artist",
                        "duration": 42,
                    }
                }
            ),
            encoding="utf-8",
        )
        (self.config.library_dir / "playlists.json").write_text(
            json.dumps({"Favorites": ["Legacy.mp3"]}),
            encoding="utf-8",
        )
        (self.config.library_dir / "settings.json").write_text(
            json.dumps({"audio_quality": "320"}),
            encoding="utf-8",
        )

        first = self.library.migrate_legacy_json()
        second = self.library.migrate_legacy_json()
        self.assertEqual(first["playlist_memberships"], 1)
        self.assertEqual(second["playlist_memberships"], 0)
        track = self.library.list_tracks()[0]
        self.assertEqual(track["title"], "Imported title")
        self.assertEqual(track["artist"], "Imported artist")
        self.assertEqual(track["duration"], 42)
        self.assertEqual(
            PlaylistService(self.database).as_legacy_map(),
            {"Favorites": ["Legacy.mp3"]},
        )
        self.assertEqual(
            self.database.get_setting("audio_quality"),
            "320",
        )


class PlaylistAndSettingsTests(ServiceTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.track_ids = []
        for index in range(3):
            path = self._media(
                self.config.library_dir / f"Track {index}.mp3",
                bytes([index]),
            )
            self.track_ids.append(
                self.library.register_path(path, probe=False)["id"]
            )

    def test_playlist_crud_order_and_legacy_adapter(self) -> None:
        service = PlaylistService(self.database)
        playlist = service.create("Mix")
        with self.assertRaises(PlaylistConflictError):
            service.create("mix")
        service.add_track(playlist["id"], self.track_ids[0])
        service.add_track(playlist["id"], self.track_ids[1])
        state = service.add_track(
            playlist["id"],
            self.track_ids[2],
            position=1,
        )
        self.assertEqual(
            [track["id"] for track in state["tracks"]],
            [self.track_ids[0], self.track_ids[2], self.track_ids[1]],
        )
        self.assertTrue(
            service.remove_track(playlist["id"], self.track_ids[2])
        )
        reordered = service.reorder_tracks(
            playlist["id"],
            [self.track_ids[1], self.track_ids[0]],
        )
        self.assertEqual(
            [track["position"] for track in reordered["tracks"]],
            [0, 1],
        )
        self.assertEqual(
            service.as_legacy_map(),
            {"Mix": ["Track 1.mp3", "Track 0.mp3"]},
        )
        self.assertTrue(service.delete(playlist["id"]))
        self.assertEqual(service.list(), [])

    def test_track_delete_cascades_and_compacts_playlist_positions(self) -> None:
        service = PlaylistService(self.database)
        playlist = service.create("Cascade")
        for track_id in self.track_ids:
            service.add_track(playlist["id"], track_id)
        self.library.delete_track(self.track_ids[1])
        state = service.get(playlist["id"])
        self.assertEqual(
            [track["position"] for track in state["tracks"]],
            [0, 1],
        )

    def test_settings_are_typed_validated_and_override_aware(self) -> None:
        service = SettingsService(self.config, self.database)
        self.assertEqual(service.get("audio_quality"), "192")
        updated = service.update(
            {
                "audio_quality": "320",
                "video_quality": "1080",
                "developer_mode": True,
                "max_workers": 8,
            }
        )
        self.assertEqual(updated["max_workers"], 8)
        with self.assertRaises(TypeError):
            service.update({"max_workers": True})
        with self.assertRaises(ValueError):
            service.update({"audio_quality": "999"})
        with self.assertRaises(KeyError):
            service.update({"unknown": "value"})

        self.config.developer_mode_override = False
        self.assertFalse(service.get("developer_mode"))
        description = {
            item["key"]: item for item in service.describe()
        }
        self.assertTrue(description["developer_mode"]["overridden"])
        service.reset(["audio_quality"])
        self.assertEqual(service.get("audio_quality"), "192")


if __name__ == "__main__":
    unittest.main()
