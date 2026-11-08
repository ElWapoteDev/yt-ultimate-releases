# Saludador Rust — Plugin API v2

This example is compiled from `guest/src/lib.rs` to `wasm32-unknown-unknown`.
It has no filesystem, network or process imports. It starts disabled after
installation; enable it explicitly in the application before running `greet`.

The optional command argument is `{"name":"Alex"}`. An empty object uses
`mundo`. Configuration is `{"greeting":"Hola","uppercase":false}`.

Build from this plugin's directory:

```sh
cargo build --release --target wasm32-unknown-unknown \
  --manifest-path guest/Cargo.toml --locked
```

Copy `guest/target/wasm32-unknown-unknown/release/ytu_greeter.wasm` to
`plugin.wasm` next to `plugin.json`. Changed code or manifest requires explicit
enablement again. No runtime compiler is required to run the resulting plugin.
