# Rust / WebAssembly Plugin API v2

`yt-plugins` executes command plugins using the Rust Wasmi interpreter. It does
not embed Python, JavaScript or a JIT compiler. Existing Python API v1 plugins
remain on disk and are listed as incompatible; they are never executed.

```rust,ignore
let host = yt_plugins::PluginHost::open(profile.join("Plugins"))?;
let plugins = host.plugins()?;
host.install_example()?; // Copies the example and its Rust sources, disabled.
host.set_enabled("dev.ytultimate.rust-greeter", true)?; // Explicit user action.
let response = host.execute(
    "dev.ytultimate.rust-greeter", "greet", serde_json::json!({"name":"Alex"}),
)?;
```

Call `execute` and `set_enabled` on a worker thread. `PluginHost` is cloneable,
thread-safe and serializes guest execution. Configuration and enablement are
stored transactionally in `Plugins/.rust-plugin-state.sqlite3`. Use
`config(id)` and `set_config(id, value)` to inspect and change configuration.

## Enablement and compatibility

New plugins start disabled. Enabling validates the binary and pins the SHA-256
of its manifest and executable bytes. Modifying either requires explicit
enablement again. Changing only configuration does not enable a plugin. A
command already admitted can finish after the plugin is disabled; all commands
remain resource limited. No third-party plugin is downloaded automatically.

Each direct child directory is named exactly like its manifest ID and contains
`plugin.json`. A v2 manifest declares `manifest_version: 2`, `api_version: 2`,
`id`, `name`, `version`, `module`, `commands`, `config_schema` and
`default_config`, plus optional `permissions` and `events`. See the bundled example for a complete manifest. Directories
and modules cannot escape their plugin directory through path traversal or
symlinks. Invalid plugins appear with an error without hiding valid plugins.

The supported schema subset is `type`, `properties`, `required`, boolean
`additionalProperties`, `items`, `enum`, `minimum`, `maximum`, `minLength`,
`maxLength`, `minItems`, `maxItems`, plus annotation fields `default`, `title`,
`description` and `$schema`. Unknown assertion keywords are rejected. No remote
schema is fetched. Nesting is limited to 8, objects to 128 properties and arrays
to 1,024 elements. Defaults come from `default_config`, not implicit application
of a schema's `default` annotation.

## Guest ABI

Compile a Rust `cdylib` for `wasm32-unknown-unknown`. Export:

- `memory`: the guest's single linear memory.
- `ytu_alloc(length: i32) -> i32`: allocate an input buffer.
- `ytu_command(pointer: i32, length: i32) -> i64`: consume UTF-8 JSON and return
  `(output_pointer << 32) | output_length`.

Input is `{"command":"greet","arguments":{},"config":{...}}`. Output is a
UTF-8 JSON value; a `message` field is convenient for displaying a result. Each
call creates a fresh instance, so its entire memory is reclaimed afterwards.
No deallocation export is necessary. The host validates every buffer access.

The only permitted import is `ytu.host_call(i32,i32,i32,i32) -> i32`, the bounded
JSON SDK described below. WASI, filesystem, process, network, clock and randomness
imports remain unavailable. Start functions are rejected. Runtime limits
are 5,000,000 units of fuel, 64 MiB linear memory, 1 MiB interpreter value stack,
256 recursion depth, one memory, one table and 4,096 table entries. Files are
limited to 8 MiB per Wasm module and 256 KiB per manifest. JSON input is limited
to 64 KiB and output to 256 KiB. Wasmi's strict compilation limits also apply.
Each invocation can make at most 64 host calls, with 64 KiB request and response
buffers. Commands and subscribed events execute synchronously in fresh instances.
Persistent background guest tasks and UI injection are not part of this API.

## Example and verification

`examples/rust-greeter` includes both the Rust source and its compiled 76 KiB
Wasm binary. `install_example()` embeds and copies these files without replacing
an existing directory. A developer can rebuild the guest with an installed
`wasm32-unknown-unknown` Rust target; the distributed application needs no compiler.

`cargo test -p yt-plugins` executes the actual compiled Rust guest and covers
enablement persistence, changed-code invalidation, configuration, invalid
commands, fuel exhaustion, memory growth and output bounds, forbidden imports
and start functions, path/symlink confinement, legacy Python preservation and
non-destructive installation. No test opens a real user profile.

Wasmi API references: [fuel configuration](https://docs.rs/wasmi/2.0.0/wasmi/struct.Config.html),
[resource limits](https://docs.rs/wasmi/2.0.0/wasmi/struct.StoreLimitsBuilder.html),
[typed function calls](https://docs.rs/wasmi/2.0.0/wasmi/struct.TypedFunc.html).

## Granted host capabilities and events

Attach the library with `host.with_library(core.clone())`. The manifest declares
permissions from `library.read`, `library.write`, `state` and `events`. The UI must
show these permissions and record an explicit choice with
`set_permissions(id, Vec<String>)`; then `set_enabled(id, true)` validates all
requested permissions. Changing grants disables the plugin. Changed manifest or
Wasm bytes still require enablement again. Grants never permit filesystem paths,
network access, process execution or arbitrary SQL.

The optional `ytu.host_call` import accepts a UTF-8 JSON request in `(pointer,
length)` and a preallocated `(output_pointer, capacity)`; it returns response
length. Use the supplied Rust `sdk.rs`, which always provides a 64 KiB output
buffer. Responses are `{"ok":true,"value":...}` or `{"ok":false,"error":"..."}`.
Malformed buffers, exhausted call budgets and oversized responses trap. A guest
must not retry mutations merely because its response buffer was too small.

| Permission | Supported operations |
| --- | --- |
| `library.read` | `library.list` (`query`, `offset`, `limit`), `library.track` (`id`), `library.playlists`, `library.playlist_tracks` (`playlist_id`, `offset`, `limit`) |
| `library.write` | `library.set_favorite` (`id`, `favorite`), `library.update_metadata` (`id`, `title`, `artist`), `library.create_playlist` (`name`), `library.rename_playlist` (`playlist_id`, `name`), `library.add_to_playlist` / `library.remove_from_playlist` (`playlist_id`, `id`), `library.reorder_playlist` (`playlist_id`, `ids`) |
| `state` | `state.get` / `state.delete` (`key`), `state.set` (`key`, `value`) |
| `events` | Delivery of exact event names declared in `events` |

Library lists return `{tracks,total,offset,next_offset}` with at most 100 records
and 48 KiB per page. Returned tracks exclude filesystem paths and transport URLs.
Library writes change application records and playlists only; source media bytes
are never rewritten or deleted. Each write is committed independently; a later
guest failure does not undo earlier successful writes in that invocation.

State is namespaced by the host's plugin ID, stored transactionally in SQLite and
survives restart. Limits: 128-byte keys, 48 KiB per JSON value, 1,024 keys and 4 MiB
per plugin. There is no guest-supplied namespace or path.

Call `dispatch_event(PluginEvent { name, payload })` on a worker. Payloads are
limited to 32 KiB and common credential keys are redacted recursively. Delivery
uses `ytu_command` with `command: "__event__"` and an `event: {name,payload}`
field. Only enabled, granted, exact-name subscribers receive it. Returned
`EventDelivery` values isolate errors and include `library_changed` so the UI
refreshes only after a library mutation. No recursive guest event emission is
supported; host events are supplied by the application.

## Scaffolding and workflow journal

`scaffold(id, name, description)` creates a non-overwriting, disabled project with
Rust sources, SDK, manifest and working precompiled greeter. Editing a guest
requires a Rust toolchain and the `wasm32-unknown-unknown` target; using the
installed example does not.

`install_journal_example()` installs `JOURNAL_PLUGIN_ID` without overwriting an
existing plugin. It requires `state` and `events`, records lifecycle/download/
playlist/processing events, throttles progress, persists retained entries, and
provides summary/recent/JSONL-export/confirmed-clear commands. Its retained
journal is bounded to 40 KiB and 1–1,000 configured entries. This is a functional
Rust replacement example, not automatic execution of preserved Python v1 code.

Tests execute the compiled Rust journal and greeter, scoped state after restart,
permission denial and revocation, real library mutations without media changes,
secret redaction, progress throttling, non-overwriting scaffolding, host buffer
validation before writes and a host-call budget independent of Wasm fuel.
