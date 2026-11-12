#!/usr/bin/env bash
set -euo pipefail

# This build is intentionally restricted to a disposable native Intel runner.
[[ ${GITHUB_ACTIONS:-} == true && $(uname -s) == Darwin && $(uname -m) == x86_64 ]] || {
  echo 'Run this script only on a disposable macos-15-intel Actions runner.' >&2; exit 1;
}
: "${RUNNER_TEMP:?}" "${GITHUB_WORKSPACE:?}"
prefix="$RUNNER_TEMP/libtorch-intel-2.12.0"
work="$RUNNER_TEMP/ytu-libtorch-intel-build"
logs="$GITHUB_WORKSPACE/target/ci-results/intel-libtorch"
mkdir -p "$prefix" "$work/source" "$logs"

# Keep the selected macOS SDK. Only remove other preinstalled Xcodes on this
# disposable runner when available storage is too small for source + objects.
available=$(df -Pk "$RUNNER_TEMP" | awk 'NR==2 {print $4}')
if (( available < 16000000 )); then
  selected=$(cd "$(xcode-select -p)" && pwd -P)
  for candidate in /Applications/Xcode*.app; do
    [[ -d "$candidate/Contents/Developer" ]] || continue
    resolved=$(cd "$candidate/Contents/Developer" && pwd -P)
    if [[ "$resolved" != "$selected" ]]; then sudo rm -rf "$candidate"; fi
  done
fi
df -h "$RUNNER_TEMP"
export HOMEBREW_NO_AUTO_UPDATE=1
brew list --versions cmake >/dev/null || brew install cmake
brew list --versions ninja >/dev/null || brew install ninja
brew list --versions libomp >/dev/null || brew install libomp
omp=$(brew --prefix libomp)

archive="$work/pytorch-v2.12.0.tar.gz"
curl --fail --location --retry 3 --output "$archive" \
  https://github.com/pytorch/pytorch/releases/download/v2.12.0/pytorch-v2.12.0.tar.gz
printf '%s  %s\n' 7cc1deb309f402ad67e9f45bbe311a40def4db19d66fddb9b01950f9bfc5ccb1 "$archive" | shasum -a 256 -c -
tar -xzf "$archive" -C "$work/source" --strip-components 1
rm "$archive"
test -f "$work/source/third_party/pocketfft/pocketfft_hdronly.h"
python3 -m venv "$work/build-tools"
"$work/build-tools/bin/python" -m pip install --disable-pip-version-check \
  'pyyaml==6.0.3' 'typing_extensions==4.15.0' 'packaging==25.0' \
  'jinja2==3.1.6' 'setuptools==80.9.0'
export PYTORCH_BUILD_VERSION=2.12.0 PYTORCH_BUILD_NUMBER=1
export MACOSX_DEPLOYMENT_TARGET=14.0
cmake -S "$work/source" -B "$work/objects" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" \
  -DCMAKE_OSX_DEPLOYMENT_TARGET=14.0 -DCMAKE_INSTALL_RPATH=@loader_path \
  -DPython_EXECUTABLE="$work/build-tools/bin/python" \
  -DBUILD_SHARED_LIBS=ON -DBUILD_PYTHON=OFF -DBUILD_TEST=OFF -DATEN_NO_TEST=ON \
  -DBUILD_BINARY=OFF -DBUILD_LAZY_TS_BACKEND=OFF -DUSE_NUMPY=OFF \
  -DUSE_CUDA=OFF -DUSE_XPU=OFF -DUSE_ROCM=OFF -DUSE_MPS=OFF \
  -DUSE_DISTRIBUTED=OFF -DUSE_KINETO=OFF -DUSE_ITT=OFF -DUSE_NATIVE_ARCH=OFF \
  -DUSE_MKLDNN=OFF -DUSE_NNPACK=OFF -DUSE_PYTORCH_QNNPACK=OFF \
  -DUSE_XNNPACK=OFF -DUSE_FBGEMM=OFF -DBLAS=vecLib -DUSE_OPENMP=ON \
  -DOpenMP_ROOT="$omp" -DOpenMP_C_LIB_NAMES=omp -DOpenMP_CXX_LIB_NAMES=omp \
  -DOpenMP_omp_LIBRARY="$omp/lib/libomp.dylib" \
  "-DOpenMP_C_FLAGS=-Xpreprocessor -fopenmp -I$omp/include" \
  "-DOpenMP_CXX_FLAGS=-Xpreprocessor -fopenmp -I$omp/include" \
  2>&1 | tee "$logs/configure.log"
cmake --build "$work/objects" --parallel 2 --target install 2>&1 | tee "$logs/build.log"
cp "$omp/lib/libomp.dylib" "$prefix/lib/libomp.dylib"
cp "$work/source/LICENSE" "$prefix/LICENSE"
printf '%s\n' 'PyTorch v2.12.0 / 0d62256a2b23365f8e1604297eb23a6545102aa8' \
  'Official source SHA256: 7cc1deb309f402ad67e9f45bbe311a40def4db19d66fddb9b01950f9bfc5ccb1' \
  'CPU x86_64; requested deployment target macOS14; Release; C++20; Accelerate/vecLib; PocketFFT; OpenMP.' \
  'Python is a build tool only. GPU, distributed and quantized acceleration backends disabled.' \
  'The model and application require separate validation; this cache alone is not a supported installer.' > "$prefix/BUILD-PROVENANCE.txt"
touch "$prefix/BUILD-COMPILED"
# Save this prefix in Actions before any post-build validation. Its cache key
# depends only on this compilation recipe, so a probe fix reuses the build.
rm -rf "$work"
df -h "$RUNNER_TEMP"
