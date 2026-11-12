# Local Windows compiler patch

Source: `torch-sys` 0.25.0 from tch-rs commit `bbf49e6`, with the original MIT and Apache licenses retained.

The Windows C++ build passed `/p:DefineConstants=GLOG_USE_GLOG_EXPORT`, an MSBuild property argument, directly to the compiler. clang-cl treats it as a nonexistent input filename and fails. The patch uses `cc::Build::define("GLOG_USE_GLOG_EXPORT", None)` so cc emits the proper compiler definition for both MSVC and clang-cl. Native API source, generated bindings and LibTorch ABI remain unchanged.
