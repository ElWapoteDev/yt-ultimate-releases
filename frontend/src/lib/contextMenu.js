import { getDesktopBridge } from './desktop.js';

// One description drives every context menu. macOS shows it as a native menu;
// Windows draws it in the page (ContextMenuHost), because native popups did
// not appear there. Items are objects
//   { label, onSelect, enabled, checked, danger, accelerator, submenu, visible }
// or the string 'separator'.

let renderHost = null;
let pending = null;
let nextRequestId = 1;
let unsubscribeSelection = null;

export function registerContextMenuHost(render) {
  renderHost = render;
  return () => {
    if (renderHost === render) renderHost = null;
  };
}

// Drops hidden entries and leading, trailing or repeated separators.
export function normalizeMenuItems(items) {
  const normalized = [];
  for (const item of items || []) {
    if (!item || item.visible === false) continue;
    if (item === 'separator' || item.type === 'separator') {
      if (normalized.length && normalized.at(-1).type !== 'separator') {
        normalized.push({ type: 'separator' });
      }
      continue;
    }
    if (item.submenu) {
      normalized.push({ ...item, submenu: normalizeMenuItems(item.submenu) });
    } else {
      normalized.push(item);
    }
  }
  while (normalized.at(-1)?.type === 'separator') normalized.pop();
  return normalized;
}

// Splits the actions from the plain template the main process can build.
export function serializeMenuItems(items, actions = new Map(), prefix = '') {
  const template = items.map((item, index) => {
    if (item.type === 'separator') return { type: 'separator' };
    const id = `${prefix}${index}`;
    const entry = { id, label: String(item.label ?? ''), enabled: item.enabled !== false };
    if (typeof item.checked === 'boolean') entry.checked = item.checked;
    if (item.accelerator) entry.accelerator = String(item.accelerator);
    if (item.submenu) {
      entry.submenu = serializeMenuItems(item.submenu, actions, `${id}.`).template;
    } else if (typeof item.onSelect === 'function') {
      actions.set(id, item.onSelect);
    }
    return entry;
  });
  return { template, actions };
}

export function usesNativeMenus(bridge = getDesktopBridge()) {
  if (!bridge?.popupMenu) return false;
  try {
    return bridge.menuStyle?.() !== 'page';
  } catch {
    return true;
  }
}

function listenForSelections(bridge) {
  if (unsubscribeSelection || !bridge?.onMenuSelection) return;
  unsubscribeSelection = bridge.onMenuSelection(({ requestId, id } = {}) => {
    if (!pending || pending.requestId !== requestId) return;
    const action = pending.actions.get(id);
    pending = null;
    action?.();
  });
}

// `position` places the menu under a button; otherwise it opens at the pointer.
export function showContextMenu(event, items, position = null) {
  event?.preventDefault?.();
  event?.stopPropagation?.();
  const visible = normalizeMenuItems(items);
  if (!visible.length) return false;

  const bridge = getDesktopBridge();
  if (usesNativeMenus(bridge)) {
    const { template, actions } = serializeMenuItems(visible);
    const requestId = nextRequestId;
    nextRequestId += 1;
    pending = { requestId, actions };
    listenForSelections(bridge);
    bridge.popupMenu({
      requestId,
      template,
      ...(position ? { x: position.x, y: position.y } : {}),
    });
    return true;
  }

  if (!renderHost) return false;
  renderHost({
    x: position?.x ?? event?.clientX ?? 0,
    y: position?.y ?? event?.clientY ?? 0,
    items: visible,
  });
  return true;
}

const TEXT_INPUT_TYPES = new Set(['', 'text', 'search', 'url', 'email', 'tel', 'number', 'password']);

function editableTarget(target) {
  const element = target?.closest?.('input, textarea, [contenteditable]:not([contenteditable="false"])');
  if (!element) return null;
  if (element.tagName === 'INPUT' && !TEXT_INPUT_TYPES.has(element.type)) return null;
  return element;
}

function hasFieldSelection(element) {
  try {
    return element.selectionStart !== null && element.selectionStart !== element.selectionEnd;
  } catch {
    return false;
  }
}

function runEditCommand(command) {
  const bridge = getDesktopBridge();
  if (bridge?.editCommand) {
    bridge.editCommand(command);
    return;
  }
  document.execCommand(command);
}

// Cut, copy and paste for text fields and selected text, which Electron does
// not provide on its own.
export function editMenuItems(target, selectionText = '') {
  const field = editableTarget(target);
  if (!field) {
    return selectionText.trim()
      ? [{ label: 'Copiar', onSelect: () => runEditCommand('copy') }]
      : [];
  }
  const isField = field.tagName === 'INPUT' || field.tagName === 'TEXTAREA';
  const selected = isField ? hasFieldSelection(field) : Boolean(selectionText.trim());
  const writable = !field.disabled && !field.readOnly;
  const secret = field.type === 'password';
  return [
    { label: 'Cortar', enabled: selected && writable && !secret, onSelect: () => runEditCommand('cut') },
    { label: 'Copiar', enabled: selected && !secret, onSelect: () => runEditCommand('copy') },
    { label: 'Pegar', enabled: writable, onSelect: () => runEditCommand('paste') },
    'separator',
    { label: 'Seleccionar todo', onSelect: () => runEditCommand('selectAll') },
  ];
}

// Turns the serialized application menu into items whose choice runs the
// original menu item in the main process.
export function applicationMenuItems(template, bridge = getDesktopBridge()) {
  return (template || []).map((item) => {
    if (item.type === 'separator') return 'separator';
    const entry = {
      label: item.label,
      enabled: item.enabled,
      checked: item.checked,
      accelerator: item.accelerator,
    };
    if (item.submenu) entry.submenu = applicationMenuItems(item.submenu, bridge);
    else entry.onSelect = () => bridge?.clickAppMenuItem?.({ id: item.id, label: item.label });
    return entry;
  });
}
