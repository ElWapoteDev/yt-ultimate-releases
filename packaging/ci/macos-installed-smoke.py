"""Disposable Actions runner only: inspect a real native window and bundled CPU runtime."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import sqlite3
import subprocess
import tempfile
import time


LIBRARIES = ('libtorch.dylib', 'libtorch_cpu.dylib', 'libc10.dylib', 'libomp.dylib')
EXECUTABLE = Path('Contents/MacOS/yt-ultimate')
MODEL = Path('Contents/Resources/models/bs-roformer-v1.pt')
MACH_O_FILES = (EXECUTABLE, *(Path('Contents/Frameworks') / name for name in LIBRARIES))


def run(arguments, **options):
    return subprocess.run(arguments, check=True, text=True, **options)


def version_tuple(value):
    fields = str(value).split('.')
    if not 1 <= len(fields) <= 3 or any(not field.isascii() or not field.isdecimal() for field in fields):
        raise RuntimeError(f'Invalid macOS deployment version: {value!r}')
    return tuple(int(field) for field in fields) + (0,) * (3 - len(fields))


def mach_o_minimums(output):
    """Read both modern and legacy deployment commands, including fat slices."""
    records = []
    current = None
    for line in output.splitlines():
        fields = line.strip().split()
        if len(fields) < 2:
            continue
        if fields[0] == 'cmd':
            current = None
            if fields[1] in ('LC_BUILD_VERSION', 'LC_VERSION_MIN_MACOSX'):
                current = {'command': fields[1]}
                records.append(current)
        elif current is not None:
            if fields[0] == 'platform':
                current['platform'] = fields[1]
            elif fields[0] == ('minos' if current['command'] == 'LC_BUILD_VERSION' else 'version'):
                version_tuple(fields[1])
                current['minimum'] = fields[1]
    if not records or any('minimum' not in record for record in records):
        raise RuntimeError('Mach-O has no complete macOS deployment command')
    return records


def check_minimums(records, declared):
    limit = version_tuple(declared)
    for record in records:
        if version_tuple(record['minimum']) > limit:
            raise RuntimeError(f"Mach-O requires macOS {record['minimum']}, above declared {declared}")


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def compare_payload(installed, built):
    hashes = {}
    for relative in (*MACH_O_FILES, MODEL):
        actual = sha256(installed / relative)
        expected = sha256(built / relative)
        hashes[str(relative)] = {'installed': actual, 'built': expected}
        if actual != expected:
            raise RuntimeError(f'DMG contains a different or stale file: {relative}; installed={actual}, built={expected}')
    return hashes


def install_from_dmg(dmg, temporary):
    mount = temporary / 'Mounted DMG'
    mount.mkdir()
    installed = temporary / 'Installed application with spaces' / 'YT Ultimate.app'
    installed.parent.mkdir()
    mounted = False
    try:
        run(['hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint', str(mount), str(dmg)])
        mounted = True
        source = mount / 'YT Ultimate.app'
        if not source.is_dir() or source.is_symlink():
            raise RuntimeError('The DMG does not contain YT Ultimate.app')
        run(['ditto', str(source), str(installed)])
    finally:
        if mounted or os.path.ismount(mount):
            run(['hdiutil', 'detach', str(mount)])
    return installed


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
    built_app = directory / 'YT Ultimate.app'
    evidence = {'version': version, 'window_observed': False, 'database_created': False,
                'single_instance_handoff': False, 'graceful_exit': False,
                'cpu_stems_passed': False, 'cpu_quality_passed': False,
                'dmg_installed_and_detached': False, 'payload_matches_build': False,
                'mach_o_minimums': {}}
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('DYLD_', 'LIBTORCH', 'PYTHON')) and key != 'CARGO_MANIFEST_DIR'}
    environment['PATH'] = '/usr/bin:/bin:/usr/sbin:/sbin'
    process = None
    try:
        dmgs = sorted(directory.glob('*.dmg'))
        if len(dmgs) != 1:
            raise RuntimeError(f'Expected exactly one DMG in {directory}, found {len(dmgs)}')
        dmg = dmgs[0]
        evidence['dmg'] = {'filename': dmg.name, 'sha256': sha256(dmg)}
        run(['hdiutil', 'verify', str(dmg)])
        with tempfile.TemporaryDirectory(prefix='YT Ultimate installed smoke ') as temporary:
            temporary = Path(temporary)
            app = install_from_dmg(dmg, temporary)
            evidence['dmg_installed_and_detached'] = True
            run(['codesign', '--verify', '--deep', '--strict', str(app)])
            with (app / 'Contents/Info.plist').open('rb') as source:
                info = plistlib.load(source)
            evidence['bundle_version'] = info.get('CFBundleShortVersionString')
            evidence['bundle_build_version'] = info.get('CFBundleVersion')
            if info.get('CFBundleShortVersionString') != version:
                raise RuntimeError(f"DMG application version {info.get('CFBundleShortVersionString')!r} differs from Cargo {version}")
            declared = info.get('LSMinimumSystemVersion')
            version_tuple(declared)
            evidence['declared_minimum_macos'] = declared
            evidence['payload_sha256'] = compare_payload(app, built_app)
            evidence['payload_matches_build'] = True
            for relative in MACH_O_FILES:
                output = subprocess.check_output(['otool', '-l', str(app / relative)], text=True)
                (results / f'{relative.name}.load-commands.txt').write_text(output)
                records = mach_o_minimums(output)
                evidence['mach_o_minimums'][str(relative)] = records
            for relative, records in evidence['mach_o_minimums'].items():
                try:
                    check_minimums(records, declared)
                except RuntimeError as error:
                    raise RuntimeError(f'{relative}: {error}') from error
            executable = app / EXECUTABLE
            frameworks = app / 'Contents/Frameworks'
            model = app / MODEL
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
            for name in LIBRARIES:
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
                    if base not in LIBRARIES:
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
