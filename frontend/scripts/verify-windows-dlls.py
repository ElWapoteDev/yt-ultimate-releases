"""Static DLL closure check for the unpacked Windows application.

Every PE file must be x64 and each imported DLL must resolve either to a
Windows system library or to a bundled file that exports every imported name.
This catches "DLL not found" and "entry point not found" failures without a
Windows machine.

Usage: uv run --no-project --with pefile python verify-windows-dlls.py <win-unpacked>
"""

from __future__ import annotations

import sys
from collections import defaultdict
from pathlib import Path

import pefile

PE_SUFFIXES = {".exe", ".dll", ".pyd", ".node"}
SYSTEM_PREFIXES = ("api-ms-win-", "ext-ms-")
# Libraries that ship with every supported Windows 10/11 installation.
SYSTEM_DLLS = frozenset(
    name + ".dll"
    for name in """
    advapi32 authz avicap32 avrt bcrypt bcryptprimitives cabinet cfgmgr32 comctl32 comdlg32
    credui crypt32 cryptbase cryptnet d2d1 d3d9 d3d11 d3d12 dbgeng dbghelp dcomp
    dhcpcsvc dnsapi dsound dwmapi dwrite dxcore dxgi dxva2 esent evr fwpuclnt gdi32
    gdiplus glu32 hid iertutil imagehlp imm32 iphlpapi kernel32 kernelbase ksuser
    mf mfplat mfreadwrite mpr msimg32 msvcrt mswsock ncrypt netapi32 normaliz
    ntdll ntdsapi ole32 oleacc oleaut32 opengl32 pdh powrprof profapi propsys psapi
    rpcrt4 secur32 setupapi shcore shell32 shlwapi sspicli tdh ucrtbase
    uiautomationcore urlmon user32 userenv usp10 uxtheme version wer wevtapi
    winhttp wininet winmm winspool.drv winsta wintrust winusb wkscli wldap32 ws2_32
    wsock32 wtsapi32 xinput1_4 dbgcore bluetoothapis mmdevapi ninput d3dcompiler_47
    """.split()
) | {"winspool.drv", "bthprops.cpl"}
# Optional accelerators whose absence the importing package handles itself.
OPTIONAL_DLLS = {"tbb12.dll": "numba recurre a OpenMP o workqueue sin la capa TBB"}
# Export tables of OpenBLAS and libtorch exceed pefile's default limit.
MAX_SYMBOL_EXPORTS = 2_000_000
# Directories that packages register with os.add_dll_directory() at import time.
REGISTERED_DIRECTORIES = ("torch/lib",)


def is_system(name: str) -> bool:
    return name.startswith(SYSTEM_PREFIXES) or name in SYSTEM_DLLS


class Bundle:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.files = [
            path
            for path in root.rglob("*")
            if path.is_file() and path.suffix.lower() in PE_SUFFIXES
        ]
        self.python_dir = next(
            (path.parent for path in self.files if path.name.lower() == "python.exe"),
            None,
        )
        self.app_dir = next(
            (
                path.parent
                for path in self.files
                if path.suffix.lower() == ".exe" and path.parent == root
            ),
            root,
        )
        self.registered = {
            path.parent
            for path in self.files
            if path.parent.name.endswith(".libs")
            or any(path.parent.as_posix().endswith(suffix) for suffix in REGISTERED_DIRECTORIES)
        }
        self._by_directory: dict[Path, dict[str, Path]] = defaultdict(dict)
        for path in self.files:
            self._by_directory[path.parent][path.name.lower()] = path
        self._exports: dict[Path, frozenset[str]] = {}

    def search_directories(self, importer: Path) -> list[Path]:
        # Python loads extension modules with LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
        # and the default directories; Electron binaries load from the app dir.
        directories = [importer.parent]
        if self.python_dir is not None and self.python_dir in importer.parents:
            directories.append(self.python_dir)
        else:
            directories.append(self.app_dir)
        directories.extend(sorted(self.registered))
        return directories

    def resolve(self, importer: Path, name: str) -> Path | None:
        for directory in self.search_directories(importer):
            library = self._by_directory[directory].get(name)
            if library is not None:
                return library
        return None

    def exports(self, library: Path) -> frozenset[str]:
        if library not in self._exports:
            pe = pefile.PE(str(library), fast_load=True, max_symbol_exports=MAX_SYMBOL_EXPORTS)
            pe.parse_data_directories(
                directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXPORT"]]
            )
            directory = getattr(pe, "DIRECTORY_ENTRY_EXPORT", None)
            names = {
                symbol.name.decode("latin-1")
                for symbol in (directory.symbols if directory else [])
                if symbol.name
            }
            self._exports[library] = frozenset(names)
            pe.close()
        return self._exports[library]


def main(root: Path) -> int:
    bundle = Bundle(root)
    problems: list[str] = []
    delayed_missing: dict[str, set[str]] = defaultdict(set)
    for path in sorted(bundle.files):
        relative = path.relative_to(root).as_posix()
        try:
            pe = pefile.PE(str(path), fast_load=True)
        except pefile.PEFormatError as error:
            problems.append(f"{relative}: no es un PE válido ({error})")
            continue
        if pe.FILE_HEADER.Machine != pefile.MACHINE_TYPE["IMAGE_FILE_MACHINE_AMD64"]:
            problems.append(f"{relative}: arquitectura 0x{pe.FILE_HEADER.Machine:04x}, se esperaba x64")
        pe.parse_data_directories(
            directories=[
                pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"],
                pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT"],
            ]
        )
        for entry in getattr(pe, "DIRECTORY_ENTRY_IMPORT", []):
            name = entry.dll.decode("latin-1").lower()
            if is_system(name):
                continue
            library = bundle.resolve(path, name)
            if library is None and name in OPTIONAL_DLLS:
                print(f"[dlls] opcional: {relative} → {name} ({OPTIONAL_DLLS[name]})")
                continue
            if library is None:
                problems.append(f"{relative}: falta {name}")
                continue
            exported = bundle.exports(library)
            missing = sorted(
                symbol.name.decode("latin-1")
                for symbol in entry.imports
                if symbol.name and symbol.name.decode("latin-1") not in exported
            )
            if missing:
                problems.append(
                    f"{relative}: {library.relative_to(root).as_posix()} no exporta "
                    + ", ".join(missing[:5])
                    + (" …" if len(missing) > 5 else "")
                )
        for entry in getattr(pe, "DIRECTORY_ENTRY_DELAY_IMPORT", []):
            name = entry.dll.decode("latin-1").lower()
            if not is_system(name) and bundle.resolve(path, name) is None:
                delayed_missing[name].add(relative)
        pe.close()

    for name, importers in sorted(delayed_missing.items()):
        print(f"[dlls] aviso: {name} (carga diferida) no está incluida; la usan {len(importers)} archivo(s)")
    for problem in problems:
        print(f"[dlls] ✗ {problem}")
    print(f"[dlls] {len(bundle.files)} binarios revisados, {len(problems)} problemas")
    return 1 if problems else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        raise SystemExit(2)
    raise SystemExit(main(Path(sys.argv[1]).resolve()))
