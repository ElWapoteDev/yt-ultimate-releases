import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { BackendController } from '../../electron/backend-controller.mjs';

function fixture(context, { isDev = true } = {}) {
  const projectDir = mkdtempSync(path.join(tmpdir(), 'ytu-separator-runtime-'));
  const resources = path.join(projectDir, 'resources');
  const previousResources = process.resourcesPath;
  process.resourcesPath = resources;
  context.after(() => {
    if (previousResources === undefined) delete process.resourcesPath;
    else process.resourcesPath = previousResources;
    rmSync(projectDir, { recursive: true, force: true });
  });
  const file = (relative) => {
    const filename = path.join(projectDir, relative);
    mkdirSync(path.dirname(filename), { recursive: true });
    writeFileSync(filename, 'fixture');
    return filename;
  };
  const controller = new BackendController({ app: {}, projectDir, isDev });
  return { controller, file, projectDir, resources };
}

test('development separator uses the project interpreter and optimized launcher instead of the console script', {
  skip: process.platform === 'win32',
}, (context) => {
  const { controller, file } = fixture(context);
  const python = file('backend/venv/bin/python3');
  const launcher = file('backend/launcher.py');
  file('backend/venv/bin/audio-separator');
  const { separator, separatorArgs } = controller.binaryEnvironment();
  assert.equal(separator, python);
  assert.equal(separator, controller.command().executable);
  assert.deepEqual(separatorArgs, ['-O', '-X', 'utf8', '-u', launcher, '--audio-separator']);
  assert.ok(path.isAbsolute(separatorArgs.at(-2)));
  assert.ok(!separatorArgs.includes('-I'), 'The development venv must retain its site-packages');
  const environment = controller.binaryEnvironmentVariables();
  assert.equal(environment.YTU_SEPARATOR_PATH, python);
  assert.deepEqual(JSON.parse(environment.YTU_SEPARATOR_ARGS), separatorArgs);
});

test('Windows development uses its venv Python even when the separator console executable is absent', (context) => {
  const { controller, file } = fixture(context);
  const python = file('backend/venv/Scripts/python.exe');
  const launcher = file('backend/launcher.py');
  const result = controller.windowsBinaryEnvironment();
  assert.equal(result.separator, python);
  assert.deepEqual(result.separatorArgs, ['-O', '-X', 'utf8', '-u', launcher, '--audio-separator']);
  assert.ok(!result.separatorArgs.includes('-I'));
});

test('packaged Windows retains isolated optimized execution for the embedded interpreter', (context) => {
  const { controller, file } = fixture(context, { isDev: false });
  const python = file('resources/backend/python/python.exe');
  const launcher = file('resources/backend/app/launcher.py');
  const result = controller.windowsBinaryEnvironment();
  assert.equal(result.separator, python);
  assert.deepEqual(result.separatorArgs, ['-I', '-O', '-X', 'utf8', '-u', launcher, '--audio-separator']);
});

test('a packaged sidecar clears stale inherited separator arguments', {
  skip: process.platform === 'win32',
}, (context) => {
  const { controller, file } = fixture(context, { isDev: false });
  const separator = file('resources/backend/audio-separator');
  const previousEnvironment = {
    YTU_SEPARATOR_PATH: '/old/python',
    YTU_SEPARATOR_ARGS: '["-I","/old/launcher.py","--audio-separator"]',
  };
  const environment = { ...previousEnvironment, ...controller.binaryEnvironmentVariables() };
  assert.equal(environment.YTU_SEPARATOR_PATH, separator);
  assert.deepEqual(JSON.parse(environment.YTU_SEPARATOR_ARGS), []);
});

test('development does not fall back to a console script when the project interpreter is missing', (context) => {
  const { controller, file } = fixture(context);
  file('backend/venv/Scripts/audio-separator.exe');
  file('backend/launcher.py');
  assert.throws(() => controller.windowsBinaryEnvironment(), /python\.exe para desarrollo/);
});
