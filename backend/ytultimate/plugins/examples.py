"""Discovery and non-overwriting installation of bundled plugin examples."""

from __future__ import annotations

import json
import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .errors import PluginError, PluginErrorCode
from .manifest import MANIFEST_FILENAME, PluginManifest, validate_plugin_id


EXAMPLE_LEVELS = frozenset({"basic", "advanced"})


@dataclass(frozen=True, slots=True)
class PluginExample:
    """Validated example metadata plus its private source location."""

    manifest: PluginManifest
    level: str
    source: Path

    def to_dict(self, *, installed: bool) -> dict[str, Any]:
        return {
            "id": self.manifest.id,
            "name": self.manifest.name,
            "version": self.manifest.version,
            "description": self.manifest.description,
            "author": self.manifest.author,
            "level": self.level,
            "permissions": list(self.manifest.permissions),
            "installed": installed,
        }


def discover_plugin_examples(
    examples_dir: Path | str,
) -> tuple[PluginExample, ...]:
    """Validate every immediate example folder without importing its code."""

    root = Path(examples_dir).expanduser()
    if not root.is_dir():
        return ()
    if root.is_symlink():
        raise PluginError(
            PluginErrorCode.PATH_ESCAPE,
            "plugin example directory cannot be a symlink",
        )
    resolved_root = root.resolve(strict=True)
    examples: list[PluginExample] = []

    for candidate in sorted(root.iterdir(), key=lambda item: item.name):
        if candidate.name.startswith(".") or not candidate.is_dir():
            continue
        _reject_symlinks(candidate)
        manifest_path = candidate / MANIFEST_FILENAME
        if not manifest_path.is_file():
            raise PluginError(
                PluginErrorCode.INVALID_MANIFEST,
                "plugin example folder is missing plugin.json",
                details={"directory": candidate.name},
            )
        try:
            resolved_candidate = candidate.resolve(strict=True)
            resolved_candidate.relative_to(resolved_root)
            manifest = PluginManifest.from_file(manifest_path)
            if candidate.name != manifest.id:
                raise PluginError(
                    PluginErrorCode.INVALID_PLUGIN_ID,
                    "plugin example folder must exactly match its manifest ID",
                    plugin_id=manifest.id,
                    details={
                        "folder": candidate.name,
                        "expected": manifest.id,
                    },
                )
            manifest.resolve_entrypoint(candidate)
            level = _example_level(manifest_path, plugin_id=manifest.id)
        except PluginError:
            raise
        except OSError as exc:
            raise PluginError(
                PluginErrorCode.LOAD_FAILED,
                "plugin example could not be inspected",
                details={"directory": candidate.name},
                cause=exc,
            ) from exc
        examples.append(
            PluginExample(
                manifest=manifest,
                level=level,
                source=resolved_candidate,
            )
        )
    return tuple(examples)


def install_plugin_example(
    examples_dir: Path | str,
    plugin_dir: Path | str,
    plugin_id: str,
) -> Path:
    """Copy one bundled example into Plugins without replacing user files."""

    validate_plugin_id(plugin_id)
    example = next(
        (
            item
            for item in discover_plugin_examples(examples_dir)
            if item.manifest.id == plugin_id
        ),
        None,
    )
    if example is None:
        raise PluginError(
            PluginErrorCode.PLUGIN_NOT_FOUND,
            "bundled plugin example was not found",
            plugin_id=plugin_id,
        )

    root = Path(plugin_dir).expanduser()
    target: Path | None = None
    try:
        root.mkdir(parents=True, exist_ok=True)
        if root.is_symlink():
            raise PluginError(
                PluginErrorCode.PATH_ESCAPE,
                "plugin directory cannot be a symlink",
                plugin_id=plugin_id,
            )
        resolved_root = root.resolve(strict=True)
        target = resolved_root / plugin_id
        target.relative_to(resolved_root)
        shutil.copytree(example.source, target, symlinks=False)

        copied_manifest = PluginManifest.from_file(target / MANIFEST_FILENAME)
        if copied_manifest.id != plugin_id:
            raise PluginError(
                PluginErrorCode.INVALID_MANIFEST,
                "installed example ID does not match the requested plugin",
                plugin_id=plugin_id,
            )
        copied_manifest.resolve_entrypoint(target)
        _apply_private_permissions(target)
        return target
    except FileExistsError as exc:
        raise PluginError(
            PluginErrorCode.SCAFFOLD_EXISTS,
            f"plugin folder {plugin_id!r} already exists",
            plugin_id=plugin_id,
        ) from exc
    except PluginError:
        if target is not None and target.exists():
            shutil.rmtree(target, ignore_errors=True)
        raise
    except OSError as exc:
        if target is not None and target.exists():
            shutil.rmtree(target, ignore_errors=True)
        raise PluginError(
            PluginErrorCode.SCAFFOLD_FAILED,
            "plugin example could not be installed",
            plugin_id=plugin_id,
            cause=exc,
        ) from exc


def _example_level(manifest_path: Path, *, plugin_id: str) -> str:
    try:
        raw = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise PluginError(
            PluginErrorCode.INVALID_MANIFEST,
            "plugin example manifest is not valid UTF-8 JSON",
            plugin_id=plugin_id,
            cause=exc,
        ) from exc
    level = raw.get("x-example-level")
    if level not in EXAMPLE_LEVELS:
        raise PluginError(
            PluginErrorCode.INVALID_MANIFEST,
            "plugin example must declare x-example-level as basic or advanced",
            plugin_id=plugin_id,
        )
    return str(level)


def _reject_symlinks(root: Path) -> None:
    if root.is_symlink():
        raise PluginError(
            PluginErrorCode.PATH_ESCAPE,
            "plugin examples cannot contain symlinks",
            details={"directory": root.name},
        )
    for directory, directories, files in os.walk(root, followlinks=False):
        for name in [*directories, *files]:
            candidate = Path(directory) / name
            if candidate.is_symlink():
                raise PluginError(
                    PluginErrorCode.PATH_ESCAPE,
                    "plugin examples cannot contain symlinks",
                    details={"directory": root.name, "entry": name},
                )


def _apply_private_permissions(root: Path) -> None:
    root.chmod(0o700)
    for directory, directories, files in os.walk(root):
        for name in directories:
            (Path(directory) / name).chmod(0o700)
        for name in files:
            (Path(directory) / name).chmod(0o600)


__all__ = [
    "EXAMPLE_LEVELS",
    "PluginExample",
    "discover_plugin_examples",
    "install_plugin_example",
]
