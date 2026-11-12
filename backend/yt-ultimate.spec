from pathlib import Path
import os

from nodejs_wheel.executable import ROOT_DIR as NODE_ROOT

from PyInstaller.utils.hooks import collect_all, collect_submodules


backend_dir = Path(SPECPATH).resolve()

datas = []
binaries = []
hiddenimports = [
    "uvicorn.logging",
    "uvicorn.loops.auto",
    "uvicorn.protocols.http.auto",
    "uvicorn.protocols.websockets.auto",
    "uvicorn.lifespan.on",
]

for package in ("yt_dlp", "yt_dlp_ejs", "pytubefix", "audio_separator"):
    package_datas, package_binaries, package_hidden = collect_all(package)
    datas += package_datas
    binaries += package_binaries
    hiddenimports += package_hidden

# Keep Node at its package-relative location for both extraction engines.
# Collecting the whole wheel would unnecessarily ship npm and its toolchain.
node_relative_dir = Path("nodejs_wheel") if os.name == "nt" else Path("nodejs_wheel/bin")
node_source = Path(NODE_ROOT) / ("node.exe" if os.name == "nt" else "bin/node")
if not node_source.is_file():
    raise RuntimeError("The standalone Node runtime from nodejs-wheel is missing")
binaries.append((str(node_source), str(node_relative_dir)))
hiddenimports += collect_submodules("nodejs_wheel")
node_license = Path(NODE_ROOT) / "LICENSE"
if node_license.is_file():
    datas.append((str(node_license), "nodejs_wheel"))

hiddenimports += collect_submodules("ytultimate")

analysis = Analysis(
    [str(backend_dir / "launcher.py")],
    pathex=[str(backend_dir)],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=["tkinter", "matplotlib.tests", "numpy.tests"],
    noarchive=False,
    # Librosa decorates several helpers with numba cache=True. Numba requires
    # their real source paths at runtime; bytecode inside PYZ has no cache
    # locator and makes the frozen separator fail after loading the model.
    module_collection_mode={"librosa": "py"},
    optimize=1,
)

pyz = PYZ(analysis.pure)

executable = EXE(
    pyz,
    analysis.scripts,
    [],
    exclude_binaries=True,
    name="yt-ultimate-backend",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=True,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)

bundle = COLLECT(
    executable,
    analysis.binaries,
    analysis.datas,
    strip=False,
    upx=False,
    upx_exclude=[],
    name="yt-ultimate-backend",
)
