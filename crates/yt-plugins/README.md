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
`default_config`. See the bundled example for a complete manifest. Directories
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

Guests have **no linked imports**, including WASI, filesystem, process,
network, clock and randomness APIs. Start functions are rejected. Runtime limits
are 5,000,000 units of fuel, 64 MiB linear memory, 1 MiB interpreter value stack,
256 recursion depth, one memory, one table and 4,096 table entries. Files are
limited to 8 MiB per Wasm module and 256 KiB per manifest. JSON input is limited
to 64 KiB and output to 256 KiB. Wasmi's strict compilation limits also apply.
These are command plugins; background event subscriptions, UI injection and
library mutation APIs are not implemented in API v2.

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
