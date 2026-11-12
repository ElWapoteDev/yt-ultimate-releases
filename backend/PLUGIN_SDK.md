# YT Ultimate Plugin SDK v1

YT Ultimate plugins are an advanced, opt-in feature. A normal installation has
Developer Mode off, every discovered plugin starts disabled, and no third-party
Python file is imported. Enabling Developer Mode alone still does not enable a
plugin: the user must review and opt in to each plugin separately.

The stable v1 surface is exported from `ytultimate.plugins` and
`ytultimate.events`. Files, methods, and classes whose names start with `_` are
runtime implementation details and may change.

## Security model

A plugin is trusted, in-process Python code. It has the same operating-system
identity and effective access as YT Ultimate. It can import Python modules,
access files, start processes, or use the network without going through the SDK.
The manifest, path checks, declared permissions, timeouts, and error isolation
protect against mistakes and make intent reviewable; **they are not a sandbox**.

Only install source you trust. Review changes again before re-enabling or
reloading a plugin. For untrusted code, use a separate process/container with an
explicit IPC protocol; the v1 in-process SDK is not the right boundary.

Manifest permissions are declarations and future-facing capability gates. A
host SDK service can call `api.require_permission("library.write")`, but a raw
Python plugin can bypass SDK services. Use narrow permission identifiers such as:

- `library.read` and `library.write`
- `filesystem.external.read` and `filesystem.external.write`
- `network.access`
- `process.execute`
- `notifications.send`

Do not request a permission that the plugin does not need.

## Project layout

Each immediate child of the configured plugin directory is one plugin. The
folder name must exactly match the manifest ID:

```text
Plugins/
└── dev.example.tags/
    ├── plugin.json
    ├── plugin.py
    └── helpers.py
```

Symlink plugin folders and symlink entrypoints are rejected. Entrypoints must be
relative `.py` files inside the plugin folder. Relative imports are supported:
from `plugin.py`, use `from .helpers import ...`.

The included `backend/plugin-template` is functional reference source but is not
in the runtime plugin directory and is never activated automatically. Two
complete examples live under `backend/plugin-examples`: a minimal configurable
greeter and a modular workflow journal. Developer Mode can copy either example
into the user's plugin directory without overwriting an existing folder. The
copy remains disabled until the user explicitly enables it.

A safe non-overwriting scaffold can also be generated:

```bash
PYTHONPATH=backend python -m ytultimate.plugins.scaffold \
  --plugin-dir "/path/to/Plugins" \
  --name "Tag Tools" \
  dev.example.tags
```

## `plugin.json`

```json
{
  "manifest_version": 1,
  "id": "dev.example.tags",
  "name": "Tag Tools",
  "version": "1.0.0",
  "api_version": 1,
  "entrypoint": "plugin.py:setup",
  "description": "Adds configurable metadata tools.",
  "author": "Example",
  "license": "MIT",
  "homepage": "https://example.invalid",
  "permissions": ["library.read", "library.write"],
  "default_config": {
    "prefix": "[YT]"
  },
  "config_schema": {
    "type": "object",
    "properties": {
      "prefix": {
        "type": "string",
        "title": "Title prefix",
        "description": "Prepended to generated titles.",
        "minLength": 0,
        "maxLength": 30,
        "default": "[YT]"
      }
    },
    "additionalProperties": false
  }
}
```

Required fields are `manifest_version`, `id`, `name`, `version`,
`api_version`, and `entrypoint`.

- IDs are lowercase, begin with a letter, and contain letters, digits, `.`, `_`,
  or `-`.
- `version` uses semantic versioning.
- v1 requires both `manifest_version: 1` and `api_version: 1`.
- `entrypoint` has the form `relative/file.py:callable`.
- Unknown manifest fields are rejected unless prefixed with `x-`.
- Configuration and command values must be strict JSON-compatible data.

The dependency-free configuration validator supports the following JSON Schema
subset: `type`, `title`, `description`, `default`, `enum`, `properties`,
`required`, `additionalProperties`, `items`, `minimum`, `maximum`, `minLength`,
`maxLength`, `minItems`, `maxItems`, and `pattern`. Unsupported schema keywords
are rejected rather than silently ignored.

## Entrypoint and lifecycle

The entrypoint receives one `PluginAPI`. It may return `None` or one teardown
callable. Registrations form a transaction: if import or setup fails, no command
or hook from that setup becomes visible.

```python
def setup(api):
    @api.register_command(
        "rename",
        title="Build a title",
        description="Returns a title without changing the library.",
        timeout=10,
    )
    def rename(payload):
        source = str(payload.get("title", ""))
        return {"title": f"{api.config['prefix']} {source}".strip()}

    @api.register_hook("download.media_ready", priority=10)
    async def downloaded(event):
        api.log.info("Downloaded %s", event.payload.get("file_id"))

    @api.on_shutdown
    def close_resources():
        pass
```

Registrations are allowed only while `setup` runs. On disable, reload, developer
mode shutdown, or application shutdown:

1. Commands and hooks are unpublished first.
2. `api.on_shutdown` callbacks run in reverse registration order.
3. A teardown callable returned by `setup` also runs.
4. The plugin's isolated module namespace is removed.

Reload creates a fresh namespace and compiles the entrypoint from current source.
State and validated configuration remain intact.

## Plugin API v1

### Identity, config, paths, and logging

- `api.api_version`: integer `1`.
- `api.plugin_id`: the validated manifest ID.
- `api.manifest`: immutable manifest metadata.
- `api.config`: a defensive read-only mapping of current validated config.
- `api.get_config(key, default=None)`: defensive copy of one config value.
- `api.paths.root`: plugin source directory.
- `api.paths.data`: persistent private data directory for this plugin.
- `api.paths.data_path(*parts)`: traversal-safe path below the data directory.
- `api.log`: a standard `logging.LoggerAdapter`.
- `api.require_permission(permission)`: verify a declared SDK permission.

Config updates are persisted atomically. A loaded plugin sees new values the
next time it reads `api.config`; reload is not required.

### Commands

```python
api.register_command(
    name,
    handler=None,
    *,
    title="",
    description="",
    timeout=None,
)
```

The method supports direct use and decorator use. A handler receives one mapping
and returns a JSON-compatible value, synchronously or asynchronously. Command
IDs are always namespaced as `<plugin-id>.<local-name>`.

The host calls:

```python
value = await manager.execute_command(
    "dev.example.tags.rename",
    {"title": "Song"},
)
```

`execute_command` raises a structured `PluginError`. Transport layers can use
`await manager.run_command(...)`, which never throws plugin errors and returns a
`CommandExecutionResult` with `ok`, `value`/`error`, and `duration_ms`.

### Hooks and events

```python
api.register_hook(
    event_name,
    handler=None,
    *,
    priority=0,
    once=False,
    timeout=None,
)
```

A hook receives an immutable event envelope:

- `event.name`
- `event.payload`
- `event.source`
- `event.emitted_at` (Unix timestamp)
- `event.event_id`

Hooks for one event run concurrently. Priority controls deterministic result
ordering, not serial execution. One failure or timeout is returned as an
`EventHandlerError` and does not cancel other hooks. `once=True` removes the hook
before its first invocation. A plugin can emit an attributed event with:

```python
dispatch = await api.emit("dev.example.tags.finished", {"count": 3})
```

The manager guarantees these lifecycle events:

| Event | Payload |
| --- | --- |
| `plugin.loaded` | `plugin_id`, `version` |
| `plugin.disabled` | `plugin_id` |
| `plugin.config_changed` | `plugin_id`, `config` |

Application integrations should use the following stable event names when the
corresponding feature is present:

| Event | Typical payload |
| --- | --- |
| `app.started`, `app.stopping` | application/version metadata |
| `download.queued` | snapshot and source metadata |
| `download.progress` | `download_id`, normalized `progress` |
| `download.media_ready` | completed job snapshot and resulting path metadata |
| `playlist.queued`, `playlist.progress` | playlist job snapshot |
| `processing.stems.queued`, `processing.stems.progress`, `processing.stems.ready` | stem job snapshot |
| `processing.fingerprint.queued`, `processing.fingerprint.progress`, `processing.fingerprint.ready` | fingerprint job snapshot |

Plugins should ignore unknown payload keys and tolerate optional keys. Custom
plugin events should be namespaced with the plugin ID.

## Host integration

The application owns a single shared `EventBus` and `PluginManager`:

```python
from ytultimate.events import EventBus
from ytultimate.plugins import PluginManager

events = EventBus(default_timeout=10)
plugins = PluginManager(
    config.plugin_dir,
    data_dir=config.data_dir / "Plugin Data",
    developer_mode=developer_mode,
    event_bus=events,
)

await plugins.start()
await events.emit("app.started", {"version": "4.2.1"}, source="application")
```

At shutdown, emit `app.stopping` and then `await plugins.stop()`.

Useful manager operations:

- `discover()` validates folders without importing code.
- `start()` imports only enabled plugins and only in Developer Mode.
- `enable(id)`, `disable(id)`, and `reload(id)` manage lifecycle.
- `set_developer_mode(bool)` unloads immediately when switched off.
- `snapshots()` and `diagnostics` provide serializable UI state.
- `commands()` exposes command metadata.
- `get_config`, `set_config`, `configure`, and `reset_config` manage config.
- `refresh()` cleanly unloads, rescans, and reloads opted-in plugins.

`StateStore` is intentionally small:

```python
class StateStore(Protocol):
    def load(self, plugin_id: str) -> PluginState: ...
    def save(self, plugin_id: str, state: PluginState) -> None: ...
```

An application can adapt SQLite or its preference repository. Without an
adapter, `JsonStateStore` writes `.plugin-state-v1.json` using a same-directory
temporary file, `fsync`, restricted permissions, and atomic replacement.

## Timeouts and structured errors

Imports/setup, commands, hooks, and teardown have bounded waits. Async work is
cancelled on timeout. Python cannot forcibly terminate a synchronous function
already running in a thread; timed-out sync code may finish later. This is
another reason plugins must be trusted.

`PluginError.to_dict()` has a stable transport shape:

```json
{
  "code": "command_timeout",
  "message": "command exceeded 10 seconds",
  "recoverable": true,
  "plugin_id": "dev.example.tags",
  "details": {
    "command_id": "dev.example.tags.rename"
  }
}
```

Internal exception objects and tracebacks are retained for host logging but are
not serialized to clients.
