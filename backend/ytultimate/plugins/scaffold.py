"""Safe, dependency-free plugin project generator."""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path

from .errors import PluginError, PluginErrorCode
from .manifest import MANIFEST_VERSION, SDK_API_VERSION, validate_plugin_id


_PLUGIN_SOURCE = '''"""Generated YT Ultimate developer plugin."""


def setup(api):
    """Register this plugin's hooks and commands."""

    @api.register_command(
        "greet",
        title="Send a greeting",
        description="Returns a configured greeting for the supplied name.",
    )
    def greet(payload):
        name = str(payload.get("name", "mundo")).strip() or "mundo"
        return {"message": f"{api.config['greeting']}, {name}!"}

    @api.register_hook("app.started")
    def on_app_started(event):
        api.log.info("Plugin ready for event %s", event.event_id)

    @api.on_shutdown
    def shutdown():
        api.log.info("Plugin stopped")
'''

_README = """# {name}

Generated for YT Ultimate Plugin API v1.

1. Edit `plugin.py` and `plugin.json`.
2. Put this folder in YT Ultimate's developer plugin directory.
3. Enable Developer Mode.
4. Review the source, then explicitly enable `{plugin_id}`.

The plugin is trusted in-process Python code. Manifest permissions document
intent and gate host SDK capabilities; they are not an operating-system sandbox.
"""


def create_plugin_scaffold(
    plugin_dir: Path | str,
    plugin_id: str,
    *,
    name: str | None = None,
    description: str = "A YT Ultimate developer plugin.",
) -> Path:
    """Create a never-enabled, non-overwriting plugin folder.

    The plugin ID is also the exact folder name, which prevents path traversal
    and makes discovery deterministic.
    """

    validate_plugin_id(plugin_id)
    display_name = (name or _title_from_id(plugin_id)).strip()
    if not display_name or len(display_name) > 120:
        raise ValueError("name must contain between 1 and 120 characters")
    description = description.strip()
    if len(description) > 1_000:
        raise ValueError("description must not exceed 1000 characters")

    root = Path(plugin_dir).expanduser()
    try:
        root.mkdir(parents=True, exist_ok=True)
        if root.is_symlink():
            raise PluginError(
                PluginErrorCode.PATH_ESCAPE,
                "plugin directory cannot be a symlink",
            )
        resolved_root = root.resolve(strict=True)
        target = resolved_root / plugin_id
        target.relative_to(resolved_root)
        target.mkdir(mode=0o700, exist_ok=False)
    except FileExistsError as exc:
        raise PluginError(
            PluginErrorCode.SCAFFOLD_EXISTS,
            f"plugin folder {plugin_id!r} already exists",
            plugin_id=plugin_id,
        ) from exc
    except PluginError:
        raise
    except OSError as exc:
        raise PluginError(
            PluginErrorCode.SCAFFOLD_FAILED,
            "plugin folder could not be created",
            plugin_id=plugin_id,
            cause=exc,
        ) from exc

    manifest = {
        "manifest_version": MANIFEST_VERSION,
        "id": plugin_id,
        "name": display_name,
        "version": "0.1.0",
        "api_version": SDK_API_VERSION,
        "entrypoint": "plugin.py:setup",
        "description": description,
        "author": "",
        "license": "",
        "homepage": "",
        "permissions": [],
        "default_config": {"greeting": "Hola"},
        "config_schema": {
            "type": "object",
            "properties": {
                "greeting": {
                    "type": "string",
                    "title": "Greeting",
                    "description": "Text placed before the requested name.",
                    "minLength": 1,
                    "maxLength": 80,
                    "default": "Hola",
                }
            },
            "additionalProperties": False,
        },
    }
    try:
        (target / "plugin.json").write_text(
            json.dumps(
                manifest,
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        (target / "plugin.py").write_text(_PLUGIN_SOURCE, encoding="utf-8")
        (target / "README.md").write_text(
            _README.format(name=display_name, plugin_id=plugin_id),
            encoding="utf-8",
        )
        for file_path in target.iterdir():
            file_path.chmod(0o600)
    except OSError as exc:
        # This directory was created by this call and the target ID was verified.
        # Cleaning it cannot affect pre-existing user content.
        shutil.rmtree(target, ignore_errors=True)
        raise PluginError(
            PluginErrorCode.SCAFFOLD_FAILED,
            "plugin scaffold files could not be written",
            plugin_id=plugin_id,
            cause=exc,
        ) from exc
    return target


def _title_from_id(plugin_id: str) -> str:
    leaf = plugin_id.rsplit(".", 1)[-1]
    return leaf.replace("-", " ").replace("_", " ").title()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Create a YT Ultimate Plugin API v1 project")
    parser.add_argument("plugin_id", help="Lowercase ID, for example dev.example.tags")
    parser.add_argument("--plugin-dir", required=True, type=Path)
    parser.add_argument("--name")
    parser.add_argument("--description", default="A YT Ultimate developer plugin.")
    args = parser.parse_args(argv)
    try:
        target = create_plugin_scaffold(
            args.plugin_dir,
            args.plugin_id,
            name=args.name,
            description=args.description,
        )
    except (PluginError, ValueError) as exc:
        parser.exit(2, f"error: {exc}\n")
    print(target)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


__all__ = ["create_plugin_scaffold", "main"]
