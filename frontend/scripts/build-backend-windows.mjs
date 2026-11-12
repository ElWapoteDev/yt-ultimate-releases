import { createHash } from 'node:crypto';
import { createReadStream, createWriteStream, existsSync } from 'node:fs';
import {
  copyFile,
  cp,
  mkdir,
  mkdtemp,
  readdir,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';
import { runCommand } from './lib/run-command.mjs';

// PyInstaller cannot cross-compile, so the Windows service is assembled from
// the official embeddable CPython and wheels published for win_amd64. The
// result runs unchanged on any 64-bit Windows 10/11 and can be built here.
const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const backendDir = path.resolve(frontendDir, '..', 'backend');
const downloadDir = path.join(backendDir, 'build', 'windows-downloads');
const outputDir = path.join(backendDir, 'build', 'windows', 'backend');
const pythonDir = path.join(outputDir, 'python');
const sitePackages = path.join(pythonDir, 'Lib', 'site-packages');
const appDir = path.join(outputDir, 'app');
const lockFile = path.join(backendDir, 'requirements-windows.txt');
const uv = process.env.YTU_UV || 'uv';

const PYTHON = Object.freeze({
  minor: '3.13',
  tag: '313',
  url: 'https://www.python.org/ftp/python/3.13.16/python-3.13.16-embed-amd64.zip',
  sha256: '97dae5274cc54867065e8d5a3226e48c35017ed332a0fdb0e27d5b5821961297',
});
// Microsoft's redistributable C++ runtime, deployed next to python.exe because
// torch and onnxruntime need msvcp140.dll and a fresh Windows may lack it.
const MSVC_RUNTIME = Object.freeze({
  url: 'https://files.pythonhosted.org/packages/fe/17/7a1eace5a7c6083bd99db861625c08bb37010dfbe90ed7a0194ae0281689/msvc_runtime-14.44.35112-cp313-cp313-win_amd64.whl',
  sha256: 'd4f6cf106aaf235f2a90952f9ec7de49e9946323880d051abf9745a6d4bf60bf',
});
// Build-time material that is never imported at runtime: C++ headers and
// import libraries, npm, and test suites (their long paths approach MAX_PATH).
const PRUNED_PATHS = Object.freeze([
  'torch/include',
  'torch/share',
  'nodejs_wheel/lib',
  'onnx/backend/test',
  'onnx/test',
  // 32-bit and ARM variants that x64 Windows never loads.
  'samplerate/_samplerate_data/libsamplerate-32bit.dll',
  'setuptools/cli.exe',
  'setuptools/cli-32.exe',
  'setuptools/cli-arm64.exe',
  'setuptools/gui.exe',
  'setuptools/gui-32.exe',
  'setuptools/gui-arm64.exe',
]);
const PRUNED_TEST_PACKAGES = Object.freeze([
  'networkx', 'numba', 'numpy', 'scipy', 'sklearn', 'sympy',
]);
const REQUIRED_FILES = Object.freeze([
  'python/python.exe',
  `python/python${PYTHON.tag}.dll`,
  'python/msvcp140.dll',
  'app/launcher.py',
  'app/ytultimate/__main__.py',
  'python/Lib/site-packages/fastapi/__init__.py',
  'python/Lib/site-packages/yt_dlp/__init__.py',
  'python/Lib/site-packages/audio_separator/utils/cli.py',
  'python/Lib/site-packages/torch/lib/torch_cpu.dll',
  'python/Lib/site-packages/nodejs_wheel/node.exe',
]);

async function sha256(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest('hex');
}

async function download({ url, sha256: expected }) {
  await mkdir(downloadDir, { recursive: true });
  const target = path.join(downloadDir, path.basename(new URL(url).pathname));
  if (existsSync(target) && (await sha256(target)) === expected) return target;

  process.stdout.write(`[backend:win] descargando ${url}\n`);
  const response = await fetch(url);
  if (!response.ok || !response.body) {
    throw new Error(`No se pudo descargar ${url} (HTTP ${response.status}).`);
  }
  const partial = `${target}.part`;
  await pipeline(Readable.fromWeb(response.body), createWriteStream(partial));
  const actual = await sha256(partial);
  if (actual !== expected) {
    await rm(partial, { force: true });
    throw new Error(`SHA-256 inesperado para ${url}: ${actual}`);
  }
  await rename(partial, target);
  return target;
}

// bsdtar extracts ZIP archives on macOS and on Windows 10 or later.
function extractZip(archive, destination) {
  return runCommand('tar', ['-xf', archive, '-C', destination], { label: 'tar' });
}

async function removeTestDirectories(root) {
  for (const entry of await readdir(root, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const child = path.join(root, entry.name);
    if (entry.name === 'tests') await rm(child, { recursive: true, force: true });
    else await removeTestDirectories(child);
  }
}

async function installPython() {
  const archive = await download(PYTHON);
  await mkdir(pythonDir, { recursive: true });
  await extractZip(archive, pythonDir);
  // The ._pth file makes the interpreter ignore PYTHONPATH, PYTHONHOME and the
  // registry, so a Python installed on the user's PC cannot interfere.
  await writeFile(
    path.join(pythonDir, `python${PYTHON.tag}._pth`),
    [`python${PYTHON.tag}.zip`, '.', 'Lib\\site-packages', '..\\app', 'import site', ''].join('\r\n'),
  );
}

async function installMsvcRuntime() {
  const wheel = await download(MSVC_RUNTIME);
  const temporary = await mkdtemp(path.join(downloadDir, 'msvc-'));
  try {
    await extractZip(wheel, temporary);
    const [dataDir] = (await readdir(temporary)).filter((name) => name.endsWith('.data'));
    const libraries = path.join(temporary, dataDir, 'data');
    for (const name of await readdir(libraries)) {
      if (name.toLowerCase().endsWith('.dll')) {
        await copyFile(path.join(libraries, name), path.join(pythonDir, name));
      }
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function installPackages() {
  await runCommand(uv, [
    'pip',
    'install',
    '--target', sitePackages,
    '--python-platform', 'x86_64-pc-windows-msvc',
    '--python-version', PYTHON.minor,
    '--only-binary', ':all:',
    '--require-hashes',
    '--no-deps',
    '--link-mode', 'copy',
    '--requirement', lockFile,
  ], { cwd: backendDir, label: 'uv pip install' });

  for (const relative of PRUNED_PATHS) {
    await rm(path.join(sitePackages, relative), { recursive: true, force: true });
  }
  for (const name of PRUNED_TEST_PACKAGES) {
    await removeTestDirectories(path.join(sitePackages, name));
  }
  const torchLibraries = path.join(sitePackages, 'torch', 'lib');
  for (const name of await readdir(torchLibraries)) {
    if (name.toLowerCase().endsWith('.lib')) await rm(path.join(torchLibraries, name));
  }
}

async function installApplication() {
  await mkdir(appDir, { recursive: true });
  await copyFile(path.join(backendDir, 'launcher.py'), path.join(appDir, 'launcher.py'));
  await cp(path.join(backendDir, 'ytultimate'), path.join(appDir, 'ytultimate'), {
    recursive: true,
    filter: (source) => !source.includes('__pycache__') && !source.endsWith('.pyc'),
  });
}

// Precompiled bytecode spares the first launch and the first separation from
// compiling thousands of modules. The service runs with -O, so level-1 files
// are generated; unchecked hashes stay valid even when the installer does not
// preserve file timestamps.
async function compileBytecode() {
  await runCommand(uv, [
    'run',
    '--no-project',
    '--python', PYTHON.minor,
    'python',
    '-c',
    [
      'import compileall, sys',
      'from py_compile import PycInvalidationMode',
      'for root in sys.argv[1:]:',
      '    compileall.compile_dir(root, quiet=2, workers=0, optimize=1,',
      '        invalidation_mode=PycInvalidationMode.UNCHECKED_HASH)',
    ].join('\n'),
    sitePackages,
    appDir,
  ], { cwd: backendDir, label: 'compileall' });
}

async function main() {
  if (!existsSync(lockFile)) throw new Error(`Falta ${lockFile}.`);
  await runCommand(uv, ['--version'], { label: 'uv' }).catch(() => {
    throw new Error('Se necesita uv (https://docs.astral.sh/uv/) para preparar el backend de Windows.');
  });

  await rm(path.dirname(outputDir), { recursive: true, force: true });
  await installPython();
  await installMsvcRuntime();
  await installPackages();
  await installApplication();
  await compileBytecode();

  const missing = REQUIRED_FILES.filter((relative) => !existsSync(path.join(outputDir, relative)));
  if (missing.length) throw new Error(`Backend de Windows incompleto: ${missing.join(', ')}`);
  process.stdout.write(`[backend:win] ✓ ${outputDir}\n`);
}

main().catch((error) => {
  process.stderr.write(`[backend:win] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
