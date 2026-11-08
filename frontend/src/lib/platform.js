import { runtime } from './runtime';

export const isWindows = runtime.platform === 'win32';

// Wording for the system file manager and the computer in user-facing copy.
export const fileManagerName = isWindows ? 'el Explorador' : 'Finder';
export const deviceName = isWindows ? 'tu PC' : 'tu Mac';
