export const isMac = process.platform === 'darwin';
export const isWindows = process.platform === 'win32';

// Completes "Mostrar en …" labels with the system file manager.
export const fileManagerName = isMac
  ? 'Finder'
  : (isWindows ? 'el Explorador' : 'la carpeta');
