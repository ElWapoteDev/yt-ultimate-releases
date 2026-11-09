#!/usr/bin/env bash
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true && $(uname -s) == Darwin && $(uname -m) == arm64 ]] || {
  echo 'Use a disposable native Apple Silicon Actions runner.' >&2; exit 1;
}
: "${RUNNER_TEMP:?}" "${GITHUB_ENV:?}"
native="$RUNNER_TEMP/ytu-macos-arm64-native"
mkdir -p "$native"
if [[ ! -f "$native/libtorch/include/torch/csrc/api/include/torch/torch.h" ]]; then
  archive="$native/libtorch-arm64.zip"
  curl --fail --location --retry 3 --output "$archive" \
    https://download.pytorch.org/libtorch/cpu/libtorch-macos-arm64-2.12.0.zip
  printf '%s  %s\n' 177bf6c4c9e9bc6c8983bd362b7f10031e5ffabfc7fb01e1f173b3a4d4a1f355 "$archive" | shasum -a 256 -c -
  unzip -q "$archive" -d "$native"
  rm "$archive"
fi
test "$(cat "$native/libtorch/build-version")" = '2.12.0'
# These are identical to the four runtime libraries already exercised locally.
cat <<'HASHES' | (cd "$native/libtorch/lib" && shasum -a 256 -c -)
6febc0d8a2e9b79151cb4d8710d8d06673b17ad2c4941efb4b841a18817ae898  libtorch.dylib
11d7233921e78a4f67611aaf3707e2f1d7bb8c212f0098144a97503a228d465a  libtorch_cpu.dylib
8c55f52bb7a347ed1bc4775add814a94fc6588267799902781615e6ba71fd7a8  libc10.dylib
6256bee09e93c28d71c65711cc69224d69994c6965648b628b70a22772fe98d4  libomp.dylib
HASHES
for name in libtorch.dylib libtorch_cpu.dylib libc10.dylib libomp.dylib; do
  lipo "$native/libtorch/lib/$name" -verify_arch arm64
done
model="$native/bs-roformer-v1.pt"
if [[ ! -f "$model" ]]; then
  dmg="$native/published-model.dmg"
  mount="$native/model-mount"
  curl --fail --location --retry 3 --output "$dmg" \
    'https://github.com/ElWapoteDev/yt-ultimate-releases/releases/download/v5.0.0/YT.Ultimate_5.0.0_aarch64_full.dmg'
  printf '%s  %s\n' b900e381f7a300ad8ef85b7e8036e0156abe69fe27a044bff9a4d9593478f632 "$dmg" | shasum -a 256 -c -
  mkdir -p "$mount"
  hdiutil attach -readonly -nobrowse -mountpoint "$mount" "$dmg"
  trap 'hdiutil detach "$mount" || true' EXIT
  cp "$mount/YT Ultimate.app/Contents/Resources/models/bs-roformer-v1.pt" "$model"
  hdiutil detach "$mount"
  trap - EXIT
  rm "$dmg"
fi
printf '%s  %s\n' 31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926 "$model" | shasum -a 256 -c -
printf '%s\n' "LIBTORCH=$native/libtorch" "DYLD_LIBRARY_PATH=$native/libtorch/lib" \
  "YTU_PACKAGE_STEMS_MODEL=$model" >> "$GITHUB_ENV"
