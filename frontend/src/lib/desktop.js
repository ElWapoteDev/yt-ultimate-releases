export function getDesktopBridge() {
  if (typeof window === 'undefined') {
    return null;
  }
  return window.ytuDesktop ?? null;
}

export async function getDesktopMeta() {
  const bridge = getDesktopBridge();
  if (!bridge?.getMeta) {
    return { isDesktop: false, platform: 'web' };
  }
  return bridge.getMeta();
}

export async function chooseMediaFiles(options = {}) {
  const bridge = getDesktopBridge();
  if (!bridge?.chooseMedia) return [];
  return bridge.chooseMedia(options);
}

export function pathsFromDroppedFiles(files) {
  const bridge = getDesktopBridge();
  if (!bridge?.pathForFile) return [];
  return Array.from(files || [])
    .map((file) => bridge.pathForFile(file))
    .filter(Boolean);
}

export function revealFile(filePath) {
  const bridge = getDesktopBridge();
  if (!filePath || !bridge?.revealInFinder) return Promise.resolve(false);
  return bridge.revealInFinder(filePath);
}

export function openWithDefaultApp(filePath) {
  const bridge = getDesktopBridge();
  if (!filePath || !bridge?.openPath) return Promise.resolve(false);
  return bridge.openPath(filePath);
}

export function copyText(text) {
  const bridge = getDesktopBridge();
  if (bridge?.copyText) bridge.copyText(text);
  else void navigator.clipboard?.writeText(text).catch(() => {});
}

// A song dragged out of the library comes back over the window as a file
// drag; it must not offer to import that same file again.
let outgoingDrag = false;

export function isOutgoingDrag() {
  return outgoingDrag;
}

export function endOutgoingDrag() {
  outgoingDrag = false;
}

export function canDragOut(file) {
  return Boolean(file?.local_path && getDesktopBridge()?.startLibraryDrag);
}

// Hands the drag to the system as a real file. Chromium's own HTML drag is
// cancelled first: running both at once closed the app on Windows.
export function dragFileOut(event, file) {
  event.preventDefault();
  if (!canDragOut(file)) return;
  outgoingDrag = true;
  getDesktopBridge().startLibraryDrag({ localPath: file.local_path, filename: file.filename });
}
