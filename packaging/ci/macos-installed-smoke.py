"""Disposable Actions runner only: inspect a real native window and bundled CPU runtime."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import time


def run(arguments, **options):
    return subprocess.run(arguments, check=True, text=True, **options)


def has_window(pid):
    cg = ctypes.CDLL('/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics')
    cf = ctypes.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')
    cg.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    cg.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
    cf.CFArrayGetCount.argtypes = [ctypes.c_void_p]
    cf.CFArrayGetCount.restype = ctypes.c_long
    cf.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]
    cf.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
    cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    cf.CFDictionaryGetValue.restype = ctypes.c_void_p
    cf.CFNumberGetValue.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p]
    cf.CFNumberGetValue.restype = ctypes.c_bool
    cf.CFRelease.argtypes = [ctypes.c_void_p]
    owner_key = ctypes.c_void_p.in_dll(cg, 'kCGWindowOwnerPID').value
    layer_key = ctypes.c_void_p.in_dll(cg, 'kCGWindowLayer').value
    windows = cg.CGWindowListCopyWindowInfo(1, 0)  # On-screen windows, metadata only.
    if not windows:
        return False
    try:
        for index in range(cf.CFArrayGetCount(windows)):
            item = cf.CFArrayGetValueAtIndex(windows, index)
            owner = cf.CFDictionaryGetValue(item, owner_key)
            layer = cf.CFDictionaryGetValue(item, layer_key)
            owner_number, layer_number = ctypes.c_longlong(), ctypes.c_longlong()
            if owner and layer:
                cf.CFNumberGetValue(owner, 4, ctypes.byref(owner_number))
                cf.CFNumberGetValue(layer, 4, ctypes.byref(layer_number))
                if owner_number.value == pid and layer_number.value == 0:
                    return True
    finally:
        cf.CFRelease(windows)
    return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--package-root', required=True, type=Path)
    parser.add_argument('--results', required=True, type=Path)
    args = parser.parse_args()
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.uname().sysname != 'Darwin':
        raise RuntimeError('This smoke test is restricted to a disposable macOS Actions runner')
    results = args.results.resolve()
    results.mkdir(parents=True, exist_ok=True)
    # Select the workspace version explicitly, even if a cache retains older output.
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1']))
    version = next(package['version'] for package in metadata['packages'] if package['name'] == 'yt-app')
    directory = args.package_root.resolve() / version / 'full'
    app = directory / 'YT Ultimate.app'
    executable = app / 'Contents/MacOS/yt-ultimate'
    frameworks = app / 'Contents/Frameworks'
    model = app / 'Contents/Resources/models/bs-roformer-v1.pt'
    evidence = {'version': version, 'window_observed': False, 'database_created': False,
                'single_instance_handoff': False, 'graceful_exit': False,
                'cpu_stems_passed': False, 'cpu_quality_passed': False}
    run(['codesign', '--verify', '--deep', '--strict', str(app)])
    for dmg in directory.glob('*.dmg'):
        run(['hdiutil', 'verify', str(dmg)])
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('DYLD_', 'LIBTORCH', 'PYTHON')) and key != 'CARGO_MANIFEST_DIR'}
    environment['PATH'] = '/usr/bin:/bin:/usr/sbin:/sbin'
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix='ytu-intel-installed-') as temporary:
            temporary = Path(temporary)
            profile = temporary / 'profile'
            environment['YTU_DATA_DIR'] = str(profile)
            environment['YTU_LIBRARY_DIR'] = str(temporary / 'library')
            with (results / 'app.stdout.log').open('w') as stdout, (results / 'app.stderr.log').open('w') as stderr:
                process = subprocess.Popen([str(executable)], env=environment, stdout=stdout, stderr=stderr)
                deadline = time.monotonic() + 60
                database = profile / 'yt-ultimate.sqlite3'
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise RuntimeError(f'Application exited early: {process.returncode}')
                    evidence['window_observed'] = has_window(process.pid)
                    evidence['database_created'] = database.is_file()
                    if evidence['window_observed'] and evidence['database_created']:
                        break
                    time.sleep(0.5)
                if not all(evidence[key] for key in ('window_observed', 'database_created')):
                    raise RuntimeError('Expected a native on-screen window and isolated database')
                with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
                    assert connection.execute("select name from sqlite_master where type='table' and name='tracks'").fetchone()
                second = run([str(executable)], env=environment, timeout=10, capture_output=True)
                evidence['single_instance_handoff'] = second.returncode == 0 and process.poll() is None
                assert evidence['single_instance_handoff']
                # Standard application quit event; no UI clicks or user profile.
                run(['/usr/bin/osascript', '-e', 'tell application id "com.ytultimate.desktop" to quit'], timeout=15, capture_output=True)
                process.wait(timeout=30)
                evidence['graceful_exit'] = process.returncode == 0
                assert evidence['graceful_exit'], f'Unexpected exit status: {process.returncode}'

            isolated_lib = temporary / 'lib'
            isolated_bin = temporary / 'bin'
            isolated_lib.mkdir()
            isolated_bin.mkdir()
            libraries = ('libtorch.dylib', 'libtorch_cpu.dylib', 'libc10.dylib', 'libomp.dylib')
            for name in libraries:
                shutil.copy2(frameworks / name, isolated_lib / name)
            for name, arguments, field in [('stems_probe', [str(model), '9'], 'cpu_stems_passed'),
                                            ('quality_probe', [str(model)], 'cpu_quality_passed')]:
                binary = isolated_bin / name
                shutil.copy2(Path('target/release/examples') / name, binary)
                dependencies = subprocess.check_output(['otool', '-L', str(binary)], text=True).splitlines()[1:]
                for line in dependencies:
                    dependency = line.strip().split(' (', 1)[0]
                    if dependency.startswith(('/usr/lib/', '/System/Library/')):
                        continue
                    base = dependency.rsplit('/', 1)[-1]
                    if base not in libraries:
                        raise RuntimeError(f'Unexpected probe dependency: {dependency}')
                    run(['install_name_tool', '-change', dependency, f'@executable_path/../lib/{base}', str(binary)])
                # Clear development RPATHs: these probes must use the copied bundle libraries.
                load_commands = subprocess.check_output(['otool', '-l', str(binary)], text=True).splitlines()
                for index, line in enumerate(load_commands):
                    if line.strip() == 'cmd LC_RPATH':
                        path = load_commands[index + 2].strip().removeprefix('path ').rsplit(' (offset ', 1)[0]
                        run(['install_name_tool', '-delete_rpath', path, str(binary)])
                run(['codesign', '--force', '--sign', '-', str(binary)])
                with (results / f'{name}.stdout.log').open('w') as stdout, (results / f'{name}.stderr.log').open('w') as stderr:
                    run([str(binary), *arguments], env=environment, stdout=stdout, stderr=stderr, timeout=600)
                evidence[field] = True
    except Exception as error:
        evidence['failure'] = str(error)
        raise
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        (results / 'result.json').write_text(json.dumps(evidence, indent=2) + '\n')
        print(json.dumps(evidence, indent=2))


if __name__ == '__main__':
    main()
