#!/usr/bin/env bash
set -euo pipefail

# Post-build checks are separate from the expensive source compilation/cache.
[[ ${GITHUB_ACTIONS:-} == true && $(uname -s) == Darwin && $(uname -m) == x86_64 ]] || {
  echo 'Run this script only on a disposable macos-15-intel Actions runner.' >&2; exit 1;
}
: "${RUNNER_TEMP:?}" "${GITHUB_WORKSPACE:?}"
prefix="$RUNNER_TEMP/libtorch-intel-2.12.0"
work="$RUNNER_TEMP/ytu-libtorch-intel-validation"
logs="$GITHUB_WORKSPACE/target/ci-results/intel-libtorch"
test -f "$prefix/BUILD-COMPILED"
mkdir -p "$work" "$logs"
for name in libtorch.dylib libtorch_cpu.dylib libc10.dylib libomp.dylib; do
  library="$prefix/lib/$name"
  lipo "$library" -verify_arch x86_64
  install_name_tool -id "@rpath/$name" "$library"
  while IFS= read -r dependency; do
    case "$dependency" in /usr/lib/*|/System/Library/*) continue ;; esac
    base=${dependency##*/}
    case "$base" in libtorch.dylib|libtorch_cpu.dylib|libc10.dylib|libomp.dylib) ;;
      *) echo "Unaccounted runtime dependency: $dependency" >&2; exit 1 ;;
    esac
    [[ "$dependency" == "@rpath/$name" ]] && continue
    install_name_tool -change "$dependency" "@loader_path/$base" "$library"
  done < <(otool -L "$library" | tail -n +2 | awk '{print $1}')
  codesign --force --sign - "$library"
done
cat > "$work/probe.cpp" <<'CPP'
#include <torch/torch.h>
#include <iostream>
int main() {
  auto x = torch::ones({2, 3});
  auto y = torch::matmul(x, x.transpose(0, 1));
  if (y.sum().item<float>() != 12.0f) return 1;
  std::cout << "Native Intel LibTorch CPU matrix probe passed\n";
}
CPP
c++ -std=c++20 "$work/probe.cpp" -I"$prefix/include" \
  -I"$prefix/include/torch/csrc/api/include" -L"$prefix/lib" \
  -Wl,-rpath,"$prefix/lib" -ltorch -ltorch_cpu -lc10 -o "$work/probe"
env -u DYLD_LIBRARY_PATH -u PYTHONPATH "$work/probe" | tee "$logs/cpp-probe.log"
python3 - "$prefix" "$logs" <<'PYVERIFY'
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
spec = importlib.util.spec_from_file_location('smoke', 'packaging/ci/macos-installed-smoke.py')
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
records = {}
for name in smoke.LIBRARIES:
    output = subprocess.check_output(['otool', '-l', str(Path(sys.argv[1]) / 'lib' / name)], text=True)
    (Path(sys.argv[2]) / (name + '.load-commands.txt')).write_text(output)
    records[name] = smoke.mach_o_minimums(output)
    smoke.check_minimums(records[name], '14.0')
(Path(sys.argv[2]) / 'deployment-minimums.json').write_text(json.dumps(records, indent=2) + '\n')
PYVERIFY
(cd "$prefix" && shasum -a 256 lib/{libtorch,libtorch_cpu,libc10,libomp}.dylib) > "$logs/dylibs.sha256"
cp "$logs/dylibs.sha256" "$prefix/"
touch "$prefix/BUILD-COMPLETE"
rm -rf "$work"
