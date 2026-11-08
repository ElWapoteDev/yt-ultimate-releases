import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { constants as fsConstants } from 'node:fs';
import {
  access,
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  realpath,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { _electron as electron } from '@playwright/test';
import {
  findPackagedApp,
  lipoArchitecture,
  pathExists,
  requestedMacArchitecture,
} from '../../scripts/lib/packaged-app.mjs';

const execFileAsync = promisify(execFile);
const frontendDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../..',
);
const packageDocument = JSON.parse(
  await readFile(path.join(frontendDir, 'package.json'), 'utf8'),
);
const productName = process.env.YTU_PRODUCT_NAME || 'YT Ultimate';
const launchTimeoutMs = 150_000;

function report(message) {
  process.stdout.write(`[packaged-smoke] ✓ ${message}\n`);
}

function createWaveBuffer({
  durationSeconds = 1,
  frequency = 440,
  sampleRate = 44_100,
} = {}) {
  const sampleCount = Math.floor(durationSeconds * sampleRate);
  const dataLength = sampleCount * 2;
  const buffer = Buffer.alloc(44 + dataLength);

  buffer.write('RIFF', 0, 'ascii');
  buffer.writeUInt32LE(36 + dataLength, 4);
  buffer.write('WAVE', 8, 'ascii');
  buffer.write('fmt ', 12, 'ascii');
  buffer.writeUInt32LE(16, 16);
  buffer.writeUInt16LE(1, 20);
  buffer.writeUInt16LE(1, 22);
  buffer.writeUInt32LE(sampleRate, 24);
  buffer.writeUInt32LE(sampleRate * 2, 28);
  buffer.writeUInt16LE(2, 32);
  buffer.writeUInt16LE(16, 34);
  buffer.write('data', 36, 'ascii');
  buffer.writeUInt32LE(dataLength, 40);

  for (let index = 0; index < sampleCount; index += 1) {
    const sample = Math.sin((2 * Math.PI * frequency * index) / sampleRate);
    buffer.writeInt16LE(Math.round(sample * 0.2 * 32_767), 44 + index * 2);
  }
  return buffer;
}

async function withTimeout(promise, timeoutMs, description) {
  let timeout;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timeout = setTimeout(() => {
          reject(new Error(`${description} excedió ${timeoutMs} ms`));
        }, timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}

async function requireFile(filePath, {
  executable = false,
  minimumBytes = 1,
} = {}) {
  const details = await stat(filePath);
  assert.ok(details.isFile(), `No es un archivo regular: ${filePath}`);
  assert.ok(
    details.size >= minimumBytes,
    `Archivo incompleto (${details.size} bytes): ${filePath}`,
  );
  if (executable) await access(filePath, fsConstants.X_OK);
  return details;
}

async function requireDirectory(directory) {
  const details = await stat(directory);
  assert.ok(details.isDirectory(), `No es un directorio: ${directory}`);
}

async function assertNoGeneratedPythonFiles(root) {
  const pending = [root];
  while (pending.length) {
    const directory = pending.pop();
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const candidate = path.join(directory, entry.name);
      assert.notEqual(
        entry.name,
        '__pycache__',
        `El paquete contiene caché Python generado: ${candidate}`,
      );
      assert.ok(
        !entry.name.endsWith('.pyc'),
        `El paquete contiene bytecode Python generado: ${candidate}`,
      );
      if (entry.isDirectory()) pending.push(candidate);
    }
  }
}

async function commandOutput(executable, args, timeout = 60_000) {
  const { stdout, stderr } = await execFileAsync(executable, args, {
    timeout,
    maxBuffer: 4 * 1024 * 1024,
    encoding: 'utf8',
  });
  return `${stdout || ''}\n${stderr || ''}`.trim();
}

async function plistValue(infoPlist, key) {
  const output = await commandOutput(
    '/usr/bin/plutil',
    ['-extract', key, 'raw', '-o', '-', infoPlist],
    10_000,
  );
  return output.trim();
}

async function validateBundle(appPath) {
  assert.equal(process.platform, 'darwin', 'Este smoke test requiere macOS.');

  const contents = path.join(appPath, 'Contents');
  const resources = path.join(contents, 'Resources');
  const executableName = path.basename(appPath, '.app');
  const paths = {
    info: path.join(contents, 'Info.plist'),
    application: path.join(contents, 'MacOS', executableName),
    asar: path.join(resources, 'app.asar'),
    icon: path.join(resources, 'icon.icns'),
    backend: path.join(resources, 'backend', 'yt-ultimate-backend'),
    separator: path.join(resources, 'backend', 'audio-separator'),
    backendInternal: path.join(resources, 'backend', '_internal'),
    ffmpeg: path.join(resources, 'bin', 'ffmpeg'),
    ffprobe: path.join(resources, 'bin', 'ffprobe'),
    fpcalc: path.join(resources, 'bin', 'fpcalc'),
    nativeLicenses: path.join(resources, 'bin', 'licenses'),
    models: path.join(resources, 'models'),
    readme: path.join(resources, 'docs', 'README.md'),
    pluginSdk: path.join(resources, 'docs', 'PLUGIN_SDK.md'),
    pluginTemplate: path.join(resources, 'docs', 'plugin-template'),
    pluginExamples: path.join(resources, 'docs', 'plugin-examples'),
  };

  await Promise.all([
    requireFile(paths.info, { minimumBytes: 100 }),
    requireFile(paths.application, { executable: true, minimumBytes: 10_000 }),
    requireFile(paths.asar, { minimumBytes: 1_000 }),
    requireFile(paths.icon, { minimumBytes: 1_000 }),
    requireFile(paths.backend, { executable: true, minimumBytes: 100_000 }),
    requireFile(paths.separator, { executable: true, minimumBytes: 100_000 }),
    requireFile(paths.ffmpeg, { executable: true, minimumBytes: 1_000_000 }),
    requireFile(paths.ffprobe, { executable: true, minimumBytes: 1_000_000 }),
    requireFile(paths.fpcalc, { executable: true, minimumBytes: 100_000 }),
    requireFile(paths.readme, { minimumBytes: 100 }),
    requireFile(paths.pluginSdk, { minimumBytes: 100 }),
    requireDirectory(paths.backendInternal),
    requireDirectory(paths.nativeLicenses),
    requireDirectory(paths.models),
    requireDirectory(paths.pluginTemplate),
    requireDirectory(paths.pluginExamples),
  ]);

  await Promise.all([
    requireFile(path.join(
      paths.pluginExamples,
      'dev.ytultimate.basic-greeter',
      'plugin.json',
    ), { minimumBytes: 100 }),
    requireFile(path.join(
      paths.pluginExamples,
      'dev.ytultimate.basic-greeter',
      'plugin.py',
    ), { minimumBytes: 100 }),
    requireFile(path.join(
      paths.pluginExamples,
      'dev.ytultimate.workflow-journal',
      'plugin.json',
    ), { minimumBytes: 100 }),
    requireFile(path.join(
      paths.pluginExamples,
      'dev.ytultimate.workflow-journal',
      'plugin.py',
    ), { minimumBytes: 100 }),
    requireFile(path.join(
      paths.pluginExamples,
      'dev.ytultimate.workflow-journal',
      'journal.py',
    ), { minimumBytes: 100 }),
    assertNoGeneratedPythonFiles(paths.pluginTemplate),
    assertNoGeneratedPythonFiles(paths.pluginExamples),
  ]);

  const [licenseEntries, modelEntries] = await Promise.all([
    readdir(paths.nativeLicenses, { withFileTypes: true }),
    readdir(paths.models, { withFileTypes: true }),
  ]);
  assert.ok(
    licenseEntries.filter((entry) => entry.isFile()).length >= 4,
    'Faltan licencias de FFmpeg/FFprobe dentro del paquete.',
  );
  const checkpoint = modelEntries.find(
    (entry) => entry.isFile() && entry.name.endsWith('.ckpt'),
  );
  assert.ok(checkpoint, 'Falta el checkpoint del separador dentro del paquete.');
  await requireFile(path.join(paths.models, checkpoint.name), {
    minimumBytes: 1_000_000,
  });
  const modelConfiguration = path.join(
    paths.models,
    `${path.basename(checkpoint.name, '.ckpt')}.yaml`,
  );
  await requireFile(modelConfiguration, { minimumBytes: 100 });

  const [bundleExecutable, bundleVersion, bundleIdentifier] = await Promise.all([
    plistValue(paths.info, 'CFBundleExecutable'),
    plistValue(paths.info, 'CFBundleShortVersionString'),
    plistValue(paths.info, 'CFBundleIdentifier'),
  ]);
  assert.equal(bundleExecutable, executableName);
  assert.equal(bundleVersion, packageDocument.version);
  assert.match(bundleIdentifier, /^[A-Za-z0-9.-]+$/);

  const [applicationType, backendType, ffmpegType] = await Promise.all([
    commandOutput('/usr/bin/file', ['-b', paths.application], 10_000),
    commandOutput('/usr/bin/file', ['-b', paths.backend], 10_000),
    commandOutput('/usr/bin/file', ['-b', paths.ffmpeg], 10_000),
  ]);
  assert.match(applicationType, /Mach-O/);
  assert.match(backendType, /Mach-O/);
  assert.match(ffmpegType, /Mach-O/);

  const expectedArchitecture = lipoArchitecture(
    requestedMacArchitecture(),
  );
  const architectureTargets = [
    paths.application,
    paths.backend,
    paths.separator,
    paths.ffmpeg,
    paths.ffprobe,
    paths.fpcalc,
  ];
  const architectureReports = await Promise.all(
    architectureTargets.map(async (target) => ({
      target,
      architectures: await commandOutput(
        '/usr/bin/lipo',
        ['-archs', target],
        10_000,
      ),
    })),
  );
  for (const architecture of architectureReports) {
    assert.ok(
      architecture.architectures.split(/\s+/).includes(expectedArchitecture),
      `${architecture.target} no incluye ${expectedArchitecture}: `
        + architecture.architectures,
    );
  }

  const [
    backendVersion,
    separatorVersion,
    ffmpegVersion,
    ffprobeVersion,
    fpcalcVersion,
  ] = await Promise.all([
    commandOutput(paths.backend, ['--version'], 120_000),
    commandOutput(paths.separator, ['--version'], 180_000),
    commandOutput(paths.ffmpeg, ['-version'], 30_000),
    commandOutput(paths.ffprobe, ['-version'], 30_000),
    commandOutput(paths.fpcalc, ['-version'], 30_000),
  ]);
  assert.match(backendVersion, /^YT Ultimate \d+\.\d+\.\d+/m);
  assert.match(separatorVersion, /audio-separator \d+\.\d+\.\d+/m);
  assert.match(ffmpegVersion, /^ffmpeg version /m);
  assert.match(ffprobeVersion, /^ffprobe version /m);
  assert.match(fpcalcVersion, /fpcalc version 1\.6\.2/);

  report(`estructura y sidecars válidos en ${appPath}`);
  return paths;
}

async function readDiagnostics(userDataDir) {
  const candidates = [
    path.join(userDataDir, 'Logs', 'backend.log'),
    path.join(userDataDir, 'backend.log'),
  ];
  for (const candidate of candidates) {
    if (!(await pathExists(candidate))) continue;
    const content = await readFile(candidate, 'utf8').catch(() => '');
    if (content) return `\n\nÚltimas líneas de ${candidate}:\n${content.slice(-8_000)}`;
  }
  return '';
}

async function launchSmoke(appPath, applicationExecutable) {
  const sandbox = await realpath(
    await mkdtemp(path.join(os.tmpdir(), 'ytu-packaged-smoke-')),
  );
  const userDataDir = path.join(sandbox, 'User Data');
  const libraryDir = path.join(sandbox, 'Library');
  const fixturesDir = path.join(sandbox, 'Fixtures');
  await Promise.all([
    mkdir(userDataDir, { recursive: true }),
    mkdir(libraryDir, { recursive: true }),
    mkdir(fixturesDir, { recursive: true }),
  ]);
  const fixture = path.join(fixturesDir, 'Packaged Smoke.wav');
  await writeFile(fixture, createWaveBuffer());

  let application;
  let failure;
  const rendererErrors = [];
  try {
    application = await withTimeout(
      electron.launch({
        executablePath: applicationExecutable,
        args: ['--headless'],
        env: {
          ...process.env,
          YTU_HEADLESS: '1',
          YTU_TEST_USER_DATA: userDataDir,
          YTU_LIBRARY_DIR: libraryDir,
        },
        timeout: launchTimeoutMs,
      }),
      launchTimeoutMs,
      'El lanzamiento de la aplicación empaquetada',
    );

    const page = await withTimeout(
      application.firstWindow(),
      launchTimeoutMs,
      'La creación de BrowserWindow',
    );
    page.setDefaultTimeout(30_000);
    page.on('pageerror', (error) => {
      rendererErrors.push(error.stack || error.message);
    });
    page.on('console', (message) => {
      if (message.type() === 'error') rendererErrors.push(message.text());
    });

    await page.waitForURL(/^file:/, { timeout: 30_000 });
    await page.locator('.app-root').waitFor({ state: 'visible' });

    const runtimeCheck = await page.evaluate(async () => {
      const runtime = window.ytuDesktop?.runtime;
      const response = await fetch(`${runtime.apiBaseUrl}/system/health`, {
        headers: { 'X-YTU-Token': runtime.authToken },
      });
      return {
        runtime,
        status: response.status,
        health: await response.json(),
        meta: await window.ytuDesktop.getMeta(),
      };
    });
    assert.equal(runtimeCheck.status, 200);
    assert.equal(runtimeCheck.health.status, 'ok');
    assert.equal(runtimeCheck.health.database_integrity, 'ok');
    assert.equal(
      runtimeCheck.health.instance_id,
      runtimeCheck.runtime.instanceId,
    );
    assert.equal(runtimeCheck.health.paths.library, libraryDir);
    assert.equal(runtimeCheck.health.paths.data, userDataDir);
    assert.equal(
      runtimeCheck.health.paths.cache,
      path.join(userDataDir, 'Cache'),
    );
    assert.equal(
      runtimeCheck.health.paths.logs,
      path.join(userDataDir, 'Logs'),
    );
    assert.equal(
      runtimeCheck.health.paths.plugins,
      path.join(userDataDir, 'Plugins'),
    );
    assert.equal(runtimeCheck.runtime.paths.data, userDataDir);
    assert.equal(runtimeCheck.runtime.appVersion, packageDocument.version);
    assert.equal(runtimeCheck.meta.isDesktop, true);
    assert.equal(runtimeCheck.runtime.isDesktop, true);
    assert.match(
      runtimeCheck.runtime.apiOrigin,
      /^http:\/\/127\.0\.0\.1:\d+$/,
    );
    assert.match(runtimeCheck.runtime.authToken, /^[A-Za-z0-9_-]{40,}$/);
    assert.equal(runtimeCheck.health.engines.ffmpeg, true);
    assert.equal(runtimeCheck.health.engines.ffprobe, true);

    const unauthorized = await fetch(
      `${runtimeCheck.runtime.apiBaseUrl}/system/health`,
    );
    assert.equal(unauthorized.status, 401);
    report('backend empaquetado saludable y protegido por token efímero');

    const bundledPluginCheck = await page.evaluate(async () => {
      const runtime = window.ytuDesktop.runtime;
      const request = async (pathname, { method = 'GET', body } = {}) => {
        const response = await fetch(`${runtime.apiBaseUrl}${pathname}`, {
          method,
          headers: {
            ...(body ? { 'Content-Type': 'application/json' } : {}),
            'X-YTU-Token': runtime.authToken,
          },
          body: body ? JSON.stringify(body) : undefined,
        });
        return {
          status: response.status,
          body: await response.json(),
        };
      };

      const settings = await request('/settings', {
        method: 'PATCH',
        body: { developer_mode: true },
      });
      const catalog = await request('/developer/plugins/examples');
      const install = await request(
        '/developer/plugins/examples/dev.ytultimate.basic-greeter/install',
        { method: 'POST' },
      );
      const enable = await request(
        '/developer/plugins/dev.ytultimate.basic-greeter/enabled',
        { method: 'PUT', body: { enabled: true } },
      );
      const command = await request(
        '/developer/plugins/dev.ytultimate.basic-greeter/commands/greet',
        {
          method: 'POST',
          body: { arguments: { name: 'Paquete' } },
        },
      );
      return {
        settings,
        catalog,
        install,
        enable,
        command,
      };
    });
    assert.equal(bundledPluginCheck.settings.status, 200);
    assert.equal(bundledPluginCheck.catalog.status, 200);
    assert.equal(bundledPluginCheck.catalog.body.count, 2);
    assert.deepEqual(
      bundledPluginCheck.catalog.body.examples.map((item) => item.level).sort(),
      ['advanced', 'basic'],
    );
    assert.equal(bundledPluginCheck.install.status, 201);
    assert.equal(bundledPluginCheck.install.body.plugin.status, 'disabled');
    assert.equal(bundledPluginCheck.enable.status, 200);
    assert.equal(bundledPluginCheck.enable.body.status, 'loaded');
    assert.equal(bundledPluginCheck.command.status, 200);
    assert.equal(
      bundledPluginCheck.command.body.result.message,
      'Hola, Paquete!',
    );
    await access(path.join(
      userDataDir,
      'Plugins',
      'dev.ytultimate.basic-greeter',
      'plugin.py',
    ));
    report('catálogo empaquetado, instalación y ejecución de plugins válidos');

    const imported = await page.evaluate(async (fixturePath) => {
      const runtime = window.ytuDesktop.runtime;
      const response = await fetch(`${runtime.apiBaseUrl}/library/import`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-YTU-Token': runtime.authToken,
        },
        body: JSON.stringify({
          paths: [fixturePath],
          mode: 'copy',
          playlist_name: '',
        }),
      });
      return {
        status: response.status,
        body: await response.json(),
      };
    }, fixture);
    assert.equal(imported.status, 200);
    assert.equal(imported.body.imported_count, 1);
    assert.equal(imported.body.errors.length, 0);
    assert.equal(imported.body.imported.length, 1);
    assert.ok(
      imported.body.imported[0].local_path.startsWith(libraryDir),
      'El archivo importado debe quedar dentro de la biblioteca aislada.',
    );
    await Promise.all([
      access(fixture),
      access(imported.body.imported[0].local_path),
    ]);

    await page.getByRole('button', {
      name: 'Canciones',
      exact: true,
    }).click();
    await page.evaluate(() => {
      window.dispatchEvent(new CustomEvent('ytu:library-changed'));
    });
    await page.locator('.media-card').waitFor({ state: 'visible' });
    assert.equal(await page.locator('.media-card').count(), 1);
    await page.getByRole('button', {
      name: 'Packaged Smoke',
    }).waitFor({ state: 'visible' });
    assert.deepEqual(rendererErrors, []);
    report('renderer de producción e importación local funcionan sin errores');
  } catch (error) {
    failure = error;
    const diagnostics = await readDiagnostics(userDataDir);
    throw new Error(
      `Falló el smoke test de ${appPath}: ${error.message}${diagnostics}`,
      { cause: error },
    );
  } finally {
    if (application) {
      await withTimeout(
        application.close(),
        15_000,
        'El cierre de la aplicación empaquetada',
      ).catch(() => {});
    }
    if (process.env.YTU_KEEP_SMOKE_DATA === '1') {
      process.stdout.write(`[packaged-smoke] datos conservados en ${sandbox}\n`);
    } else {
      await rm(sandbox, { recursive: true, force: true }).catch(() => {});
    }
    if (failure) process.exitCode = 1;
  }
}

async function main() {
  const appPath = await findPackagedApp({ frontendDir, productName });
  report(`artefacto localizado: ${appPath}`);
  const resources = await validateBundle(appPath);
  await launchSmoke(appPath, resources.application);
  report('smoke test empaquetado completado');
}

main().catch((error) => {
  process.stderr.write(`[packaged-smoke] ✗ ${error.stack || error}\n`);
  process.exitCode = 1;
});
