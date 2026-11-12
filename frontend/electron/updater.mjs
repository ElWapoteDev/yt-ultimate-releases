import { appendFile } from 'node:fs';
import path from 'node:path';
import electronUpdater from 'electron-updater';

// The first check lets the window and the local service settle; long sessions
// keep checking so a release published while the app is open is not missed.
const FIRST_CHECK_DELAY_MS = 20_000;
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

function describe(part) {
  if (part instanceof Error) return part.stack || part.message;
  return typeof part === 'string' ? part : JSON.stringify(part);
}

// electron-updater logs to the console by default, which a packaged Windows
// app discards; updater.log sits next to backend.log instead.
function fileLogger(logPath) {
  const write = (level) => (...parts) => {
    const line = `${new Date().toISOString()} [${level}] ${parts.map(describe).join(' ')}\n`;
    appendFile(logPath, line, () => {});
  };
  return { info: write('info'), warn: write('warn'), error: write('error'), debug() {} };
}

// Downloads releases from the publish target in electron-builder.windows.config.mjs
// in the background and asks before restarting; a postponed update installs
// silently when the app quits.
export class UpdateController {
  constructor({
    app,
    dialog,
    getMainWindow,
    prepareInstall,
    // Read lazily: the getter instantiates the updater for the current platform.
    updater = electronUpdater.autoUpdater,
  }) {
    this.app = app;
    this.updater = updater;
    this.dialog = dialog;
    this.getMainWindow = getMainWindow;
    this.prepareInstall = prepareInstall;
    // idle → checking → downloading → downloaded → installing (→ failed)
    this.state = 'idle';
    this.version = '';
    this.promptOpen = false;
    this.logger = fileLogger(path.join(app.getPath('logs'), 'updater.log'));
  }

  start() {
    this.updater.logger = this.logger;
    this.updater.autoDownload = true;
    this.updater.autoInstallOnAppQuit = true;
    this.updater.on('update-downloaded', (info) => {
      this.state = 'downloaded';
      this.version = info.version;
      void this.promptInstall();
    });
    this.updater.on('error', (error) => this.handleError(error));

    setTimeout(() => void this.check(), FIRST_CHECK_DELAY_MS).unref();
    setInterval(() => void this.check(), CHECK_INTERVAL_MS).unref();
  }

  showMessage(options) {
    const window = this.getMainWindow();
    return window && !window.isDestroyed()
      ? this.dialog.showMessageBox(window, options)
      : this.dialog.showMessageBox(options);
  }

  informDownloading() {
    return this.showMessage({
      type: 'info',
      title: 'Descargando actualización',
      message: `Se está descargando YT Ultimate ${this.version}.`,
      detail: 'Puedes seguir usando la aplicación; te avisaremos cuando esté lista para instalarse.',
      buttons: ['Aceptar'],
    });
  }

  async check({ interactive = false } = {}) {
    if (this.state === 'downloaded') {
      if (interactive) await this.promptInstall();
      return;
    }
    if (this.state === 'downloading') {
      if (interactive) await this.informDownloading();
      return;
    }
    if (this.state !== 'idle') return;

    this.state = 'checking';
    let result;
    try {
      result = await this.updater.checkForUpdates();
    } catch (error) {
      this.state = 'idle';
      if (interactive) {
        await this.showMessage({
          type: 'warning',
          title: 'No se pudo buscar actualizaciones',
          message: 'No fue posible consultar si hay una versión nueva.',
          detail: `Revisa tu conexión a internet e inténtalo de nuevo.\n\n${error?.message || error}`,
          buttons: ['Aceptar'],
        });
      }
      return;
    }

    if (!result?.isUpdateAvailable) {
      this.state = 'idle';
      if (interactive) {
        await this.showMessage({
          type: 'info',
          title: 'Sin actualizaciones',
          message: 'Ya tienes la versión más reciente.',
          detail: `YT Ultimate ${this.app.getVersion()}`,
          buttons: ['Aceptar'],
        });
      }
      return;
    }

    // A previously downloaded installer can finish before this resumes.
    if (this.state === 'checking') this.state = 'downloading';
    this.version = result.updateInfo.version;
    // Download failures are reported through the error event.
    result.downloadPromise?.catch(() => {});
    if (interactive && this.state === 'downloading') await this.informDownloading();
  }

  async promptInstall() {
    if (this.promptOpen || this.state !== 'downloaded') return;
    this.promptOpen = true;
    const { response } = await this.showMessage({
      type: 'info',
      title: 'Actualización lista',
      message: `YT Ultimate ${this.version} está lista para instalarse.`,
      detail: 'Reinicia ahora para usar la versión nueva. Si eliges más tarde, '
        + 'se instalará automáticamente cuando cierres la aplicación.',
      buttons: ['Reiniciar ahora', 'Más tarde'],
      defaultId: 0,
      cancelId: 1,
    });
    this.promptOpen = false;
    if (response === 0 && this.state === 'downloaded') await this.installNow();
  }

  async installNow() {
    this.state = 'installing';
    // The installer force-closes every process under the install folder, so
    // the local service gets its graceful shutdown first.
    try {
      await this.prepareInstall();
    } catch (error) {
      this.logger.warn('No se pudo detener el servicio antes de actualizar:', error);
    }
    this.updater.quitAndInstall(false, true);
  }

  handleError(error) {
    this.logger.error(error);
    if (this.state === 'installing') {
      // The service is already stopped: restart on the current version and let
      // the next check download the update again.
      this.state = 'failed';
      setImmediate(() => {
        this.dialog.showErrorBox(
          'No se pudo instalar la actualización',
          `YT Ultimate se reiniciará con la versión actual.\n\n${error?.message || error}`,
        );
        this.app.relaunch();
        this.app.quit();
      });
      return;
    }
    if (this.state === 'downloading') this.state = 'idle';
  }
}
