# Local build patch

Source: crates.io `mp3lame-sys` 0.1.11, with its bundled LAME 3.100 source and original licenses retained.

The upstream build script selects autotools or the direct MSVC build using the host's `cfg(unix)`/`cfg(windows)`. When cargo-xwin builds a Windows executable from macOS, that selects autotools and attempts to execute a Windows configuration test on macOS.

This patch selects the direct C build when `TARGET` ends in `windows-msvc` and makes the `cc` build dependency available on every host. Native Unix targets retain the original autotools build. Codec source and Rust FFI remain unchanged. The Windows package links the C runtime statically.
