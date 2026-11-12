# Workflow Journal Rust

A real Rust/Wasm plugin with explicit `state` and `events` permissions. It records
subscribed application events, throttles progress notifications, redacts sensitive
payload keys in the host, and retains a configurable journal. Commands provide a
summary, recent entries, JSONL export and an explicitly confirmed clear action.
State survives application restarts and is isolated from other plugins.

The journal retains at most the configured 1–1000 entries and 40 KiB of event
records. Individual payloads larger than 4 KiB are marked truncated. These bounds
keep guest calls and persisted state within the interpreter's resource limits.
This example does not access arbitrary files, network or Python.

Rebuild after editing with `cargo build --release --target wasm32-unknown-unknown`
inside `guest`, then copy `guest/target/wasm32-unknown-unknown/release/ytu_journal.wasm`
to `plugin.wasm`. Any manifest or binary change requires explicit enablement again.

The host SDK is `guest/src/sdk.rs`. `ytu.host_call` accepts JSON operations such as
`state.get`, `state.set`, `state.delete`, `library.list`, `library.set_favorite` and
playlist membership operations. Library operations require the corresponding
`library.read` or `library.write` permission in the manifest and user grant.
