import {
  BrowserWindow,
  Menu,
  app,
  clipboard,
  dialog,
  ipcMain,
  nativeImage,
  shell,
} from 'electron';
import { existsSync, statSync } from 'node:fs';
import path from 'node:path';
import { isWindows } from './platform.mjs';

const MEDIA_EXTENSIONS = Object.freeze([
  'mp3', 'mp4', 'm4a', 'webm', 'mkv', 'wav', 'flac', 'aac', 'ogg', 'opus', 'm3u', 'm3u8',
]);

const OPENABLE_EXTENSIONS = new Set(MEDIA_EXTENSIONS.map((extension) => `.${extension}`));
const EDIT_COMMANDS = new Set(['cut', 'copy', 'paste', 'selectAll']);
const MAX_MENU_DEPTH = 4;
const MAX_COPY_LENGTH = 32_768;

const fallbackDragIcon = nativeImage.createFromDataURL(
  'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO5Xx4kAAAAASUVORK5CYII=',
);
// The system icon lookup is asynchronous; drags reuse it per file type.
const dragIcons = new Map();

function sourceWindow(event) {
  return BrowserWindow.fromWebContents(event.sender) || undefined;
}

// Native popups never appeared on Windows, so the renderer draws its menus
// there. YTU_MENU_STYLE overrides the choice for tests.
function menuStyle() {
  if (process.env.YTU_MENU_STYLE === 'page' || process.env.YTU_MENU_STYLE === 'native') {
    return process.env.YTU_MENU_STYLE;
  }
  return isWindows ? 'page' : 'native';
}

function isExistingFile(filePath) {
  try {
    return typeof filePath === 'string' && statSync(filePath).isFile();
  } catch {
    return false;
  }
}

async function dragIcon(filePath) {
  const extension = path.extname(filePath).toLowerCase();
  if (!dragIcons.has(extension)) {
    const icon = await app.getFileIcon(filePath, { size: 'normal' }).catch(() => fallbackDragIcon);
    dragIcons.set(extension, icon.isEmpty() ? fallbackDragIcon : icon);
  }
  return dragIcons.get(extension);
}

// Builds a native menu from the renderer's plain template; a click reports
// the item id back instead of running renderer code in this process.
function nativeMenuTemplate(items, onSelect, depth = 0) {
  if (!Array.isArray(items) || depth > MAX_MENU_DEPTH) return [];
  return items.map((item) => {
    if (item?.type === 'separator') return { type: 'separator' };
    const entry = {
      label: String(item?.label ?? ''),
      enabled: item?.enabled !== false,
    };
    if (typeof item?.checked === 'boolean') {
      entry.type = 'checkbox';
      entry.checked = item.checked;
    }
    if (Array.isArray(item?.submenu)) {
      entry.submenu = nativeMenuTemplate(item.submenu, onSelect, depth + 1);
    } else {
      entry.click = () => onSelect(String(item?.id ?? ''));
    }
    return entry;
  });
}

// The application menu as plain data for the in-page menu on Windows; ids
// are index paths resolved again when an item is chosen.
function serializeApplicationMenu(menu, prefix = '') {
  return (menu?.items || [])
    .filter((item) => item.visible !== false)
    .map((item) => {
      const id = `${prefix}${menu.items.indexOf(item)}`;
      if (item.type === 'separator') return { type: 'separator' };
      return {
        id,
        label: item.label,
        enabled: item.enabled !== false,
        ...(item.type === 'checkbox' || item.type === 'radio' ? { checked: item.checked } : {}),
        ...(typeof item.accelerator === 'string' ? { accelerator: item.accelerator } : {}),
        ...(item.submenu ? { submenu: serializeApplicationMenu(item.submenu, `${id}.`) } : {}),
      };
    });
}

function applicationMenuItem(id) {
  let menu = Menu.getApplicationMenu();
  let item = null;
  for (const part of String(id).split('.')) {
    item = menu?.items?.[Number(part)];
    if (!item) return null;
    menu = item.submenu;
  }
  return item;
}

export function registerDesktopIpc({
  getRuntime,
  getMainWindow,
  menuController,
}) {
  ipcMain.on('desktop:get-runtime', (event) => {
    event.returnValue = getRuntime();
  });

  ipcMain.handle('desktop:get-meta', () => {
    const runtime = getRuntime();
    return {
      isDesktop: true,
      platform: process.platform,
      appVersion: app.getVersion(),
      paths: runtime?.paths || {},
    };
  });

  ipcMain.handle('desktop:choose-media', async (event, options = {}) => {
    const folder = options.kind === 'folder';
    const result = await dialog.showOpenDialog(sourceWindow(event), {
      title: folder ? 'Importar carpeta de música' : 'Importar música',
      buttonLabel: 'Importar',
      properties: folder
        ? ['openDirectory', 'createDirectory']
        : ['openFile', 'multiSelections'],
      filters: folder
        ? undefined
        : [
            { name: 'Audio, vídeo y playlists', extensions: MEDIA_EXTENSIONS },
            { name: 'Todos los archivos', extensions: ['*'] },
          ],
    });
    if (result.canceled) return [];
    result.filePaths.forEach((filePath) => app.addRecentDocument(filePath));
    return result.filePaths;
  });

  ipcMain.handle('desktop:reveal-in-finder', async (_event, filePath) => {
    if (!filePath || !existsSync(filePath)) return false;
    shell.showItemInFolder(filePath);
    return true;
  });

  ipcMain.handle('desktop:show-app-menu', (event, position = {}) => (
    menuController.popup(sourceWindow(event), position)
  ));

  ipcMain.handle('desktop:open-known-folder', async (_event, name) => {
    const runtime = getRuntime();
    const allowed = new Set(['library', 'data', 'logs', 'plugins', 'models']);
    const folder = allowed.has(name) ? runtime?.paths?.[name] : '';
    if (!folder || !existsSync(folder)) return false;
    return (await shell.openPath(folder)) === '';
  });

  ipcMain.on('desktop:menu-style', (event) => {
    event.returnValue = menuStyle();
  });

  ipcMain.on('desktop:popup-menu', (event, request = {}) => {
    const { requestId } = request;
    try {
      const menu = Menu.buildFromTemplate(nativeMenuTemplate(request.template, (id) => {
        if (!event.sender.isDestroyed()) event.sender.send('desktop-menu-selection', { requestId, id });
      }));
      const position = Number.isFinite(request.x) && Number.isFinite(request.y)
        ? { x: Math.round(request.x), y: Math.round(request.y) }
        : {};
      menu.popup({ window: sourceWindow(event), ...position });
    } catch (error) {
      console.error('[menu]', error);
    }
  });

  ipcMain.on('desktop:edit-command', (event, command) => {
    if (EDIT_COMMANDS.has(command)) event.sender[command]();
  });

  ipcMain.on('desktop:copy-text', (_event, text) => {
    if (typeof text === 'string' && text) clipboard.writeText(text.slice(0, MAX_COPY_LENGTH));
  });

  // Limited to existing media files so the renderer cannot launch programs.
  ipcMain.handle('desktop:open-path', async (_event, filePath) => {
    if (!isExistingFile(filePath) || !OPENABLE_EXTENSIONS.has(path.extname(filePath).toLowerCase())) {
      return false;
    }
    return (await shell.openPath(filePath)) === '';
  });

  ipcMain.handle('desktop:get-app-menu', () => serializeApplicationMenu(Menu.getApplicationMenu()));

  ipcMain.on('desktop:click-app-menu-item', (event, { id, label } = {}) => {
    const item = applicationMenuItem(id);
    // The menu may have been rebuilt since it was read; never click a different item.
    if (!item || item.label !== label || item.enabled === false || item.submenu) return;
    item.click(undefined, sourceWindow(event), event.sender);
  });

  // The renderer cancels its own HTML drag first: starting a second, native
  // drag on top of Chromium's closed the whole app on Windows.
  ipcMain.on('library:start-drag', async (event, payload) => {
    if (!isExistingFile(payload?.localPath)) return;
    const icon = await dragIcon(payload.localPath);
    if (event.sender.isDestroyed()) return;
    try {
      event.sender.startDrag({ file: payload.localPath, icon });
    } catch (error) {
      console.error('[drag]', error);
    }
  });

  ipcMain.on('desktop:update-playback-state', (_event, state) => {
    menuController.update(state || {});
  });

  ipcMain.on('desktop:update-menu-configuration', (_event, configuration) => {
    menuController.update(configuration || {});
  });

  return () => {
    [
      'desktop:get-meta',
      'desktop:choose-media',
      'desktop:reveal-in-finder',
      'desktop:show-app-menu',
      'desktop:open-known-folder',
      'desktop:open-path',
      'desktop:get-app-menu',
    ].forEach((channel) => ipcMain.removeHandler(channel));
    [
      'desktop:get-runtime',
      'desktop:menu-style',
      'desktop:popup-menu',
      'desktop:edit-command',
      'desktop:copy-text',
      'desktop:click-app-menu-item',
      'library:start-drag',
      'desktop:update-playback-state',
      'desktop:update-menu-configuration',
    ].forEach((channel) => ipcMain.removeAllListeners(channel));
    getMainWindow()?.webContents?.removeAllListeners('will-navigate');
  };
}
