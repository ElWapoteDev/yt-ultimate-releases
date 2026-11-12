import {
  BrowserWindow,
  Menu,
  app,
  dialog,
  nativeTheme,
  session,
  shell,
} from 'electron';
import { existsSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { BackendController } from './backend-controller.mjs';
import { registerDesktopIpc } from './ipc.mjs';
import { AppMenuController } from './menu.mjs';
import { isMac, isWindows } from './platform.mjs';

const currentDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(currentDir, '..');
const projectDir = path.resolve(frontendDir, '..');
const isDev = !app.isPackaged;
const isHeadless = process.argv.includes('--headless') || process.env.YTU_HEADLESS === '1';
const usesTestProfile = isHeadless && Boolean(process.env.YTU_TEST_USER_DATA);
const supportedOpenExtensions = new Set([
  '.mp3', '.mp4', '.m4a', '.webm', '.mkv', '.wav', '.flac', '.aac', '.ogg', '.opus', '.m3u', '.m3u8',
]);
// Shared by the macOS Dock menu and the Windows Jump List.
const QUICK_ACTIONS = Object.freeze([
  { id: 'import', label: 'Importar música…', action: ['importFiles'] },
  { id: 'search', label: 'Buscar', action: ['navigate', 'search'] },
  { id: 'play-pause', label: 'Reproducir / Pausar', action: ['togglePlay'] },
]);
const QUICK_ACTION_ARGUMENT = '--ytu-action=';
// Must match --titlebar-height in the renderer.
const TITLEBAR_HEIGHT = 52;
const QUIT_TIMEOUT_MS = 8_000;

let mainWindow = null;
let backend = null;
let updates = null;
let menuController = null;
let disposeIpc = null;
let quitting = false;
let backendStopped = false;
let applicationStarted = false;
let backendFailureDialogOpen = false;
const pendingOpenPaths = [];
const pendingQuickActions = [];

function packageMetadata() {
  try {
    return JSON.parse(readFileSync(path.join(frontendDir, 'package.json'), 'utf8'));
  } catch {
    return {};
  }
}

app.setName('YT Ultimate');
if (isWindows) {
  // Taskbar grouping, pinning and the Jump List follow the installer shortcut.
  app.setAppUserModelId(packageMetadata().ytuAppId || 'com.ytultimate.desktop');
}
if (usesTestProfile) {
  const testUserData = path.resolve(process.env.YTU_TEST_USER_DATA);
  const testCache = path.join(testUserData, 'Cache');
  const testSession = path.join(testUserData, 'Session');
  const testLogs = path.join(testUserData, 'Logs');
  [testUserData, testCache, testSession, testLogs].forEach((directory) => {
    mkdirSync(directory, { recursive: true });
  });
  app.setPath('userData', testUserData);
  app.setPath('cache', testCache);
  app.setPath('sessionData', testSession);
  app.setAppLogsPath(testLogs);
} else {
  app.setAppLogsPath();
}
app.setAboutPanelOptions({
  applicationName: 'YT Ultimate',
  applicationVersion: app.getVersion(),
  copyright: `Copyright © ${new Date().getFullYear()} YT Ultimate`,
  credits: `Tu biblioteca y reproductor multimedia para ${isWindows ? 'Windows' : 'macOS'}.`,
});

function backendCacheDirectory() {
  // Electron reports the shared Roaming root as the cache path on Windows;
  // regenerable data belongs in the local, non-roaming profile.
  if (isWindows && !usesTestProfile && process.env.LOCALAPPDATA) {
    return path.join(process.env.LOCALAPPDATA, app.getName(), 'Cache');
  }
  return app.getPath('cache');
}

// Packaged: [executable, ...files]; development: [electron, app path, ...files].
function launchArguments(argv) {
  const values = (argv || []).slice(1).filter(
    (value) => typeof value === 'string' && !value.startsWith('-'),
  );
  return isDev ? values.slice(1) : values;
}

function collectQuickActions(argv) {
  for (const value of argv || []) {
    if (typeof value !== 'string' || !value.startsWith(QUICK_ACTION_ARGUMENT)) continue;
    const id = value.slice(QUICK_ACTION_ARGUMENT.length);
    const quickAction = QUICK_ACTIONS.find((candidate) => candidate.id === id);
    if (quickAction) pendingQuickActions.push(quickAction.action);
  }
}

function collectOpenPaths(values) {
  for (const value of values || []) {
    let supported = false;
    if (typeof value === 'string' && existsSync(value)) {
      try {
        const stats = statSync(value);
        supported = stats.isDirectory()
          || (stats.isFile() && supportedOpenExtensions.has(path.extname(value).toLowerCase()));
      } catch {
        supported = false;
      }
    }
    if (
      supported
      && !pendingOpenPaths.includes(value)
    ) {
      pendingOpenPaths.push(value);
      app.addRecentDocument(value);
    }
  }
}

function flushOpenPaths() {
  if (!mainWindow || mainWindow.isDestroyed() || mainWindow.webContents.isLoading()) return;
  for (const action of pendingQuickActions.splice(0)) {
    mainWindow.webContents.send('menu-action', ...action);
  }
  if (!pendingOpenPaths.length) return;
  const paths = pendingOpenPaths.splice(0);
  mainWindow.webContents.send('desktop-open-files', paths);
}

app.on('open-file', (event, filePath) => {
  event.preventDefault();
  collectOpenPaths([filePath]);
  flushOpenPaths();
});

const hasSingleInstanceLock = app.requestSingleInstanceLock();
if (!hasSingleInstanceLock) {
  app.quit();
} else {
  if (!isMac) {
    // Windows passes associated files and Jump List tasks on the command line.
    collectOpenPaths(launchArguments(process.argv));
    collectQuickActions(process.argv);
  }
  app.on('second-instance', (_event, argv, workingDirectory) => {
    collectOpenPaths(
      launchArguments(argv).map((value) => path.resolve(workingDirectory || '', value)),
    );
    collectQuickActions(argv);
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.show();
      mainWindow.focus();
    }
    flushOpenPaths();
  });
}

function sendToRenderer(action, ...parameters) {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  mainWindow.webContents.send('menu-action', action, ...parameters);
}

function sendBackendState(state) {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  mainWindow.webContents.send('backend-state', state);
}

async function handleBackendState(state) {
  sendBackendState(state);
  if (
    state?.status !== 'error'
    || quitting
    || !applicationStarted
    || backendFailureDialogOpen
  ) return;

  backendFailureDialogOpen = true;
  const result = await dialog.showMessageBox(mainWindow || undefined, {
    type: 'error',
    title: 'El servicio local se detuvo',
    message: 'YT Ultimate perdió la conexión con su motor multimedia.',
    detail: state.message || 'Puedes reiniciarlo sin cerrar la aplicación.',
    buttons: ['Reiniciar servicio', 'Salir'],
    defaultId: 0,
    cancelId: 1,
  });
  backendFailureDialogOpen = false;

  if (result.response !== 0) {
    app.quit();
    return;
  }
  try {
    await backend.start();
  } catch (error) {
    dialog.showErrorBox(
      'No se pudo reiniciar YT Ultimate',
      error?.message || String(error),
    );
    app.quit();
  }
}

async function backendPath(name, fallback) {
  try {
    const paths = await backend.request('system/paths');
    return paths?.[name] || fallback;
  } catch {
    return fallback;
  }
}

function menuActions() {
  return {
    openLibraryFolder: async () => {
      const library = await backendPath(
        'library',
        path.join(app.getPath('music'), app.name),
      );
      await shell.openPath(library);
    },
    openPluginsFolder: async () => {
      const plugins = backend?.runtime?.paths?.plugins
        || path.join(app.getPath('userData'), 'Plugins');
      await shell.openPath(plugins);
    },
    openLogsFolder: async () => shell.openPath(app.getPath('logs')),
    openPluginDocumentation: async () => {
      const documentation = isDev
        ? path.join(projectDir, 'backend', 'PLUGIN_SDK.md')
        : path.join(process.resourcesPath, 'docs', 'PLUGIN_SDK.md');
      if (existsSync(documentation)) await shell.openPath(documentation);
      else sendToRenderer('navigate', 'settings', 'developer');
    },
    openHelp: async () => {
      const guide = isDev
        ? path.join(projectDir, 'README.md')
        : path.join(process.resourcesPath, 'docs', 'README.md');
      if (existsSync(guide)) await shell.openPath(guide);
      else sendToRenderer('navigate', 'settings');
    },
    toggleDevTools: () => mainWindow?.webContents.toggleDevTools(),
    reloadRenderer: () => mainWindow?.webContents.reload(),
    ...(updates ? { checkForUpdates: () => updates.check({ interactive: true }) } : {}),
  };
}

function shutdownTimeout() {
  return new Promise((resolve) => {
    setTimeout(resolve, QUIT_TIMEOUT_MS).unref();
  });
}

// Only the Windows installer publishes releases; macOS builds are unsigned and
// cannot apply updates in place.
function updatesEnabled() {
  return isWindows && !isDev && !isHeadless && process.env.YTU_DISABLE_UPDATES !== '1';
}

// Stops the service before the update installer replaces its files; the quit
// that follows then skips the shutdown wait in before-quit.
async function prepareUpdateInstall() {
  quitting = true;
  backendStopped = true;
  await Promise.race([backend?.stop(), shutdownTimeout()]);
}

async function createUpdateController() {
  if (!updatesEnabled()) return null;
  try {
    const { UpdateController } = await import('./updater.mjs');
    return new UpdateController({
      app,
      dialog,
      getMainWindow: () => mainWindow,
      prepareInstall: prepareUpdateInstall,
    });
  } catch (error) {
    // A broken updater must never keep the application from opening.
    console.error('[updates]', error);
    return null;
  }
}

function installSecurityPolicies(runtime) {
  session.defaultSession.setPermissionRequestHandler((_webContents, _permission, callback) => {
    callback(false);
  });

  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    const developmentSources = isDev
      ? " http://127.0.0.1:5173 ws://127.0.0.1:5173"
      : '';
    const policy = [
      "default-src 'self'",
      `script-src 'self'${isDev ? " 'unsafe-eval' 'unsafe-inline'" : ''}`,
      "style-src 'self' 'unsafe-inline'",
      `connect-src 'self' ${runtime.apiOrigin}${developmentSources}`,
      `img-src 'self' data: blob: https: ${runtime.apiOrigin}`,
      `media-src 'self' blob: ${runtime.apiOrigin}`,
      "font-src 'self' data:",
      "object-src 'none'",
      "base-uri 'self'",
      "frame-ancestors 'none'",
    ].join('; ');
    const headers = { ...details.responseHeaders };
    headers['Content-Security-Policy'] = [policy];
    callback({ responseHeaders: headers });
  });
}

function platformWindowOptions() {
  if (isMac) {
    return {
      titleBarStyle: 'hiddenInset',
      trafficLightPosition: { x: 20, y: 18 },
      vibrancy: 'sidebar',
      visualEffectState: 'active',
    };
  }
  // Native caption buttons drawn over the renderer's own title bar band; the
  // application menu opens from a sidebar button instead of a menu bar.
  return {
    titleBarStyle: 'hidden',
    titleBarOverlay: {
      color: '#18181a',
      symbolColor: '#f5f5f5',
      height: TITLEBAR_HEIGHT,
    },
  };
}

async function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 980,
    minHeight: 660,
    show: false,
    title: app.name,
    backgroundColor: '#121214',
    ...platformWindowOptions(),
    webPreferences: {
      preload: path.join(currentDir, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: true,
    },
  });

  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:\/\//i.test(url)) void shell.openExternal(url);
    return { action: 'deny' };
  });
  mainWindow.webContents.on('will-navigate', (event, url) => {
    const allowed = isDev
      ? url.startsWith('http://127.0.0.1:5173')
      : url.startsWith('file://');
    if (!allowed) {
      event.preventDefault();
      if (/^https?:\/\//i.test(url)) void shell.openExternal(url);
    }
  });
  mainWindow.webContents.on('will-attach-webview', (event) => event.preventDefault());
  mainWindow.webContents.on('did-finish-load', flushOpenPaths);
  mainWindow.once('ready-to-show', () => {
    if (!isHeadless) mainWindow?.show();
    flushOpenPaths();
  });
  mainWindow.on('closed', () => {
    mainWindow = null;
  });

  if (isDev) await mainWindow.loadURL('http://127.0.0.1:5173');
  else await mainWindow.loadFile(path.join(frontendDir, 'dist', 'index.html'));

  return mainWindow;
}

async function startApplication() {
  if (isWindows) nativeTheme.themeSource = 'dark';
  backend = new BackendController({
    app,
    projectDir,
    isDev,
    cacheDir: backendCacheDirectory(),
    onState: (state) => void handleBackendState(state),
  });
  const runtime = await backend.start();
  installSecurityPolicies(runtime);
  updates = await createUpdateController();

  menuController = new AppMenuController({
    app,
    isDev,
    send: sendToRenderer,
    actions: menuActions(),
  });
  menuController.rebuild();

  disposeIpc = registerDesktopIpc({
    getRuntime: () => backend?.runtime || runtime,
    getMainWindow: () => mainWindow,
    menuController,
  });

  if (isMac) {
    app.dock.setMenu(Menu.buildFromTemplate(QUICK_ACTIONS.map(({ label, action }) => ({
      label,
      click: () => sendToRenderer(...action),
    }))));
  } else if (isWindows && !isDev) {
    app.setUserTasks(QUICK_ACTIONS.map(({ id, label }) => ({
      program: process.execPath,
      arguments: `${QUICK_ACTION_ARGUMENT}${id}`,
      title: label,
      description: label,
      iconPath: process.execPath,
      iconIndex: 0,
    })));
  }

  await createWindow();
  applicationStarted = true;
  updates?.start();
}

if (hasSingleInstanceLock) {
  app.whenReady().then(startApplication).catch((error) => {
    dialog.showErrorBox(
      'No se pudo iniciar YT Ultimate',
      `${error?.message || error}\n\nConsulta ${app.getPath('logs')} para más detalles.`,
    );
    app.quit();
  });
}

app.on('activate', async () => {
  if (BrowserWindow.getAllWindows().length === 0 && backend?.runtime) {
    await createWindow();
  }
});

app.on('before-quit', (event) => {
  quitting = true;
  disposeIpc?.();
  disposeIpc = null;
  if (!isWindows || backendStopped || !backend?.process) {
    backend?.stop();
    return;
  }
  // Windows cannot signal the service, so quitting waits until it finished
  // its graceful shutdown or its process tree was terminated.
  event.preventDefault();
  backendStopped = true;
  void Promise.race([backend.stop(), shutdownTimeout()]).finally(() => app.quit());
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin' && !quitting) app.quit();
});
