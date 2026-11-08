import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { open, readFile, readdir, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import asar from '@electron/asar';
import windowsConfig, { releasesRepository } from '../electron-builder.windows.config.mjs';
import { windowsInstallerPath } from './build-windows-installer.mjs';
import { pathExists } from './lib/packaged-app.mjs';
import { runCommand } from './lib/run-command.mjs';
import { readUpdateYaml, sha512Base64 } from './lib/update-info.mjs';

// Static checks for the Windows package built on another platform: layout,
// architecture, bundled sources and the DLL closure of every binary.
const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const backendDir = path.resolve(frontendDir, '..', 'backend');
const unpackedDir = path.join(frontendDir, windowsConfig.directories.output, 'win-unpacked');
const resourcesDir = path.join(unpackedDir, 'resources');
const AMD64 = 0x8664;

function report(message) {
  process.stdout.write(`[windows-package] ✓ ${message}\n`);
}

async function peMachine(filePath) {
  const handle = await open(filePath, 'r');
  try {
    const header = Buffer.alloc(4096);
    await handle.read(header, 0, header.length, 0);
    assert.equal(header.toString('latin1', 0, 2), 'MZ', `${filePath} no es un ejecutable PE`);
    const offset = header.readUInt32LE(0x3c);
    assert.equal(header.toString('latin1', offset, offset + 4), 'PE\0\0', `${filePath} no es PE`);
    return header.readUInt16LE(offset + 4);
  } finally {
    await handle.close();
  }
}

async function assertWindowsX64(filePath) {
  assert.equal(await peMachine(filePath), AMD64, `${filePath} no es x64`);
}

async function sha256(filePath) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest('hex');
}

async function pythonSources(root, relative = '') {
  const files = [];
  for (const entry of await readdir(path.join(root, relative), { withFileTypes: true })) {
    const child = path.join(relative, entry.name);
    if (entry.isDirectory() && entry.name !== '__pycache__') {
      files.push(...await pythonSources(root, child));
    } else if (entry.isFile() && entry.name.endsWith('.py')) {
      files.push(child);
    }
  }
  return files;
}

async function main() {
  const { productName } = windowsConfig;
  const executable = path.join(unpackedDir, `${productName}.exe`);
  await assertWindowsX64(executable);
  report(`${productName}.exe es x64`);

  const packaged = JSON.parse(
    asar.extractFile(path.join(resourcesDir, 'app.asar'), 'package.json').toString('utf8'),
  );
  assert.equal(packaged.ytuAppId, windowsConfig.appId, 'Falta el AppUserModelID empaquetado');
  assert.ok(
    asar.listPackage(path.join(resourcesDir, 'app.asar')).some(
      (entry) => entry.replaceAll('\\', '/') === '/dist/index.html',
    ),
    'El renderer no está en app.asar',
  );
  report('app.asar incluye renderer y metadatos');

  const asarEntries = new Set(
    asar.listPackage(path.join(resourcesDir, 'app.asar')).map((entry) => entry.replaceAll('\\', '/')),
  );
  for (const entry of ['/electron/updater.mjs', '/node_modules/electron-updater/package.json']) {
    assert.ok(asarEntries.has(entry), `Falta ${entry} en app.asar`);
  }
  const updateConfig = await readUpdateYaml(path.join(resourcesDir, 'app-update.yml'));
  for (const key of ['provider', 'owner', 'repo']) {
    assert.equal(updateConfig[key], releasesRepository[key], `app-update.yml: ${key} incorrecto`);
  }
  assert.ok(updateConfig.updaterCacheDirName, 'app-update.yml no define updaterCacheDirName');
  report(`actualizaciones desde ${releasesRepository.owner}/${releasesRepository.repo}`);

  const python = path.join(resourcesDir, 'backend', 'python');
  await assertWindowsX64(path.join(python, 'python.exe'));
  const pathConfiguration = await readFile(path.join(python, 'python313._pth'), 'utf8');
  for (const line of ['python313.zip', 'Lib\\site-packages', '..\\app', 'import site']) {
    assert.ok(pathConfiguration.split(/\r?\n/).includes(line), `python313._pth no contiene ${line}`);
  }
  report('Python embebido configurado');

  const sources = await pythonSources(path.join(backendDir, 'ytultimate'));
  for (const relative of sources) {
    const [expected, actual] = await Promise.all([
      sha256(path.join(backendDir, 'ytultimate', relative)),
      sha256(path.join(resourcesDir, 'backend', 'app', 'ytultimate', relative)),
    ]);
    assert.equal(actual, expected, `Backend desactualizado: ${relative}. Ejecuta npm run backend:build:win`);
  }
  assert.equal(
    await sha256(path.join(resourcesDir, 'backend', 'app', 'launcher.py')),
    await sha256(path.join(backendDir, 'launcher.py')),
    'launcher.py desactualizado',
  );
  assert.ok(
    await pathExists(path.join(
      resourcesDir, 'backend', 'app', 'ytultimate', '__pycache__', '__main__.cpython-313.opt-1.pyc',
    )),
    'Falta el bytecode optimizado que usa python -O',
  );
  report(`backend actualizado (${sources.length} módulos)`);

  for (const tool of ['ffmpeg.exe', 'ffprobe.exe', 'fpcalc.exe']) {
    await assertWindowsX64(path.join(resourcesDir, 'bin', tool));
  }
  for (const macOnly of ['bin/ffmpeg', 'bin/ffprobe', 'bin/fpcalc', 'backend/yt-ultimate-backend']) {
    assert.ok(!(await pathExists(path.join(resourcesDir, macOnly))), `Binario de macOS incluido: ${macOnly}`);
  }
  report('FFmpeg, FFprobe y Chromaprint para Windows x64');

  const modelDir = path.join(backendDir, 'models', 'audio-separator');
  for (const name of await readdir(modelDir)) {
    const [source, packagedModel] = await Promise.all([
      stat(path.join(modelDir, name)),
      stat(path.join(resourcesDir, 'models', name)),
    ]);
    assert.equal(packagedModel.size, source.size, `Modelo incompleto: ${name}`);
  }
  for (const document of ['PLUGIN_SDK.md', 'README.md', 'plugin-template/plugin.json']) {
    assert.ok(await pathExists(path.join(resourcesDir, 'docs', document)), `Falta docs/${document}`);
  }
  report('modelo de separación y documentación');

  await runCommand('uv', [
    'run',
    '--no-project',
    '--with', 'pefile',
    'python',
    path.join(currentDir, 'verify-windows-dlls.py'),
    unpackedDir,
  ], { label: 'verify-windows-dlls' });
  report('todas las dependencias DLL se resuelven');

  const installer = await windowsInstallerPath();
  // The NSIS stub is a 32-bit executable that installs the 64-bit payload.
  assert.ok([0x14c, AMD64].includes(await peMachine(installer)), 'Instalador inválido');
  const [installerDetails, executableDetails] = await Promise.all([stat(installer), stat(executable)]);
  assert.ok(installerDetails.size > 100_000_000, 'El instalador está incompleto');
  assert.ok(
    installerDetails.mtimeMs >= executableDetails.mtimeMs - 2_000,
    'El instalador es anterior a la aplicación verificada',
  );
  report(
    `${path.basename(installer)} (${installerDetails.size} bytes) sha256=${await sha256(installer)}`,
  );

  // latest.yml is what installed copies download first; it must describe this
  // exact installer under the asset name it will have on GitHub.
  const updateInfo = await readUpdateYaml(path.join(path.dirname(installer), 'latest.yml'));
  assert.equal(updateInfo.version, packaged.version, 'latest.yml anuncia otra versión');
  assert.equal(updateInfo.path, path.basename(installer), 'latest.yml apunta a otro archivo');
  assert.equal(updateInfo.sha512, await sha512Base64(installer), 'latest.yml no coincide con el instalador');
  assert.ok(await pathExists(`${installer}.blockmap`), 'Falta el blockmap de actualizaciones diferenciales');
  report(`latest.yml ${updateInfo.version} coincide con el instalador`);
}

main().catch((error) => {
  process.stderr.write(`[windows-package] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
