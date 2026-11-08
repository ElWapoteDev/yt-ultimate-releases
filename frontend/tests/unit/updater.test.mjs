import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { UpdateController } from '../../electron/updater.mjs';

class FakeUpdater extends EventEmitter {
  constructor() {
    super();
    this.results = [];
    this.installs = [];
  }

  async checkForUpdates() {
    const next = this.results.shift();
    if (next instanceof Error) {
      this.emit('error', next);
      throw next;
    }
    return next;
  }

  quitAndInstall(isSilent, isForceRunAfter) {
    this.installs.push({ isSilent, isForceRunAfter });
  }
}

function setup({ responses = [] } = {}) {
  const calls = [];
  const app = {
    getPath: () => mkdtempSync(path.join(tmpdir(), 'ytu-updater-')),
    getVersion: () => '4.2.0',
    relaunch: () => calls.push('relaunch'),
    quit: () => calls.push('quit'),
  };
  const messages = [];
  const dialog = {
    showMessageBox: async (...args) => {
      messages.push(args.at(-1));
      return { response: responses.shift() ?? 1 };
    },
    showErrorBox: (title) => calls.push(`error:${title}`),
  };
  const updater = new FakeUpdater();
  const controller = new UpdateController({
    app,
    dialog,
    getMainWindow: () => null,
    prepareInstall: async () => calls.push('prepare'),
    updater,
  });
  controller.start();
  return { controller, updater, messages, calls };
}

const available = (version) => ({
  isUpdateAvailable: true,
  updateInfo: { version },
  downloadPromise: Promise.resolve([]),
});

test('start configures background download and install on quit', () => {
  const { updater } = setup();
  assert.equal(updater.autoDownload, true);
  assert.equal(updater.autoInstallOnAppQuit, true);
  assert.equal(typeof updater.logger.info, 'function');
});

test('manual check reports when the installed version is current', async () => {
  const { controller, updater, messages } = setup();
  updater.results.push({ isUpdateAvailable: false, updateInfo: { version: '4.2.0' } });
  await controller.check({ interactive: true });
  assert.equal(controller.state, 'idle');
  assert.equal(messages.length, 1);
  assert.match(messages[0].message, /más reciente/);
});

test('background checks stay silent and failures return to idle', async () => {
  const { controller, updater, messages } = setup();
  updater.results.push(new Error('offline'));
  await controller.check();
  assert.equal(controller.state, 'idle');
  assert.equal(messages.length, 0);
});

test('a downloaded update restarts through the installer once the service stopped', async () => {
  const { controller, updater, messages, calls } = setup({ responses: [0] });
  updater.results.push(available('4.3.0'));
  await controller.check();
  assert.equal(controller.state, 'downloading');

  updater.emit('update-downloaded', { version: '4.3.0' });
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(messages[0].message, /4\.3\.0/);
  assert.deepEqual(messages[0].buttons, ['Reiniciar ahora', 'Más tarde']);
  assert.deepEqual(calls, ['prepare']);
  assert.deepEqual(updater.installs, [{ isSilent: false, isForceRunAfter: true }]);
  assert.equal(controller.state, 'installing');
});

test('postponing keeps the update for quit and a later manual check asks again', async () => {
  const { controller, updater, messages } = setup({ responses: [1, 1] });
  updater.results.push(available('4.3.0'));
  await controller.check();
  updater.emit('update-downloaded', { version: '4.3.0' });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(controller.state, 'downloaded');
  assert.equal(updater.installs.length, 0);

  await controller.check({ interactive: true });
  assert.equal(messages.length, 2);
  assert.equal(messages[1].title, 'Actualización lista');
});

test('a manual check during a download does not start another one', async () => {
  const { controller, updater, messages } = setup();
  updater.results.push(available('4.3.0'));
  await controller.check();
  await controller.check({ interactive: true });
  assert.equal(updater.results.length, 0);
  assert.equal(messages.length, 1);
  assert.match(messages[0].message, /Se está descargando YT Ultimate 4\.3\.0/);
});

test('a download error allows the next check to retry', async () => {
  const { controller, updater } = setup();
  updater.results.push(available('4.3.0'));
  await controller.check();
  updater.emit('error', new Error('connection reset'));
  assert.equal(controller.state, 'idle');
});

test('an installer failure relaunches the current version only once', async () => {
  const { controller, updater, calls } = setup({ responses: [0] });
  updater.quitAndInstall = () => updater.emit('error', new Error('No update filepath provided'));
  updater.results.push(available('4.3.0'));
  await controller.check();
  updater.emit('update-downloaded', { version: '4.3.0' });
  await new Promise((resolve) => setImmediate(resolve));
  // The quit handler of electron-updater fails the same way afterwards.
  updater.emit('error', new Error('No update filepath provided'));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(controller.state, 'failed');
  assert.deepEqual(calls, [
    'prepare',
    'error:No se pudo instalar la actualización',
    'relaunch',
    'quit',
  ]);
});
