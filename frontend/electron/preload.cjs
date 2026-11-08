const { contextBridge, ipcRenderer, webUtils } = require('electron');

const runtime = ipcRenderer.sendSync('desktop:get-runtime') || {};

function subscription(channel, callback) {
  if (typeof callback !== 'function') return () => {};
  const handler = (_event, ...args) => callback(...args);
  ipcRenderer.on(channel, handler);
  return () => ipcRenderer.removeListener(channel, handler);
}

contextBridge.exposeInMainWorld('ytuDesktop', {
  runtime: Object.freeze(runtime),
  getMeta: () => ipcRenderer.invoke('desktop:get-meta'),
  chooseMedia: (options) => ipcRenderer.invoke('desktop:choose-media', options),
  pathForFile: (file) => {
    try {
      return webUtils.getPathForFile(file);
    } catch {
      return '';
    }
  },
  revealInFinder: (filePath) => ipcRenderer.invoke('desktop:reveal-in-finder', filePath),
  openKnownFolder: (name) => ipcRenderer.invoke('desktop:open-known-folder', name),
  showAppMenu: (position) => ipcRenderer.invoke('desktop:show-app-menu', position),
  getAppMenu: () => ipcRenderer.invoke('desktop:get-app-menu'),
  clickAppMenuItem: (item) => ipcRenderer.send('desktop:click-app-menu-item', item),
  menuStyle: () => ipcRenderer.sendSync('desktop:menu-style'),
  popupMenu: (request) => ipcRenderer.send('desktop:popup-menu', request),
  editCommand: (command) => ipcRenderer.send('desktop:edit-command', command),
  copyText: (text) => ipcRenderer.send('desktop:copy-text', text),
  openPath: (filePath) => ipcRenderer.invoke('desktop:open-path', filePath),
  startLibraryDrag: (payload) => ipcRenderer.send('library:start-drag', payload),
  updatePlaybackState: (state) => ipcRenderer.send('desktop:update-playback-state', state),
  updateMenuConfiguration: (state) => ipcRenderer.send('desktop:update-menu-configuration', state),
  onMenuSelection: (callback) => subscription('desktop-menu-selection', callback),
  onMenuAction: (callback) => subscription('menu-action', callback),
  onOpenFiles: (callback) => subscription('desktop-open-files', callback),
  onBackendState: (callback) => subscription('backend-state', callback),
});
