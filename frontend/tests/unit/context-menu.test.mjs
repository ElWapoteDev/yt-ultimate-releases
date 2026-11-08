import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import {
  applicationMenuItems,
  editMenuItems,
  normalizeMenuItems,
  registerContextMenuHost,
  serializeMenuItems,
  showContextMenu,
} from '../../src/lib/contextMenu.js';

afterEach(() => {
  delete globalThis.window;
});

function installBridge(bridge) {
  globalThis.window = { ytuDesktop: bridge };
}

function field({ tagName = 'INPUT', type = 'text', start = 0, end = 0, ...rest } = {}) {
  const element = {
    tagName, type, selectionStart: start, selectionEnd: end, disabled: false, readOnly: false, ...rest,
  };
  element.closest = () => element;
  return element;
}

const labels = (items) => items.map((item) => (item === 'separator' ? '—' : item.label));

test('hidden entries and stray separators are dropped at every level', () => {
  const items = normalizeMenuItems([
    'separator',
    { label: 'A' },
    { label: 'Oculto', visible: false },
    'separator',
    'separator',
    { label: 'Sub', submenu: ['separator', { label: 'B' }, 'separator'] },
    'separator',
  ]);
  assert.deepEqual(items.map((item) => item.label ?? item.type), ['A', 'separator', 'Sub']);
  assert.deepEqual(items[2].submenu.map((item) => item.label), ['B']);
});

test('serialized templates keep ids for actions and leave closures behind', () => {
  const play = () => {};
  const add = () => {};
  const { template, actions } = serializeMenuItems(normalizeMenuItems([
    { label: 'Reproducir', onSelect: play },
    'separator',
    { label: 'Playlist', submenu: [{ label: 'Favoritas', onSelect: add, checked: false }] },
    { label: 'Eliminar', enabled: false, onSelect: () => {} },
  ]));
  assert.deepEqual(template, [
    { id: '0', label: 'Reproducir', enabled: true },
    { type: 'separator' },
    { id: '2', label: 'Playlist', enabled: true, submenu: [
      { id: '2.0', label: 'Favoritas', enabled: true, checked: false },
    ] },
    { id: '3', label: 'Eliminar', enabled: false },
  ]);
  assert.equal(actions.get('0'), play);
  assert.equal(actions.get('2.0'), add);
  assert.equal(JSON.stringify(template).includes('function'), false);
});

test('native menus report the chosen id back to its action', () => {
  let request = null;
  let deliver = null;
  installBridge({
    menuStyle: () => 'native',
    popupMenu: (value) => { request = value; },
    onMenuSelection: (callback) => { deliver = callback; return () => {}; },
  });
  const chosen = [];
  showContextMenu(null, [
    { label: 'Uno', onSelect: () => chosen.push('uno') },
    { label: 'Dos', onSelect: () => chosen.push('dos') },
  ]);
  assert.deepEqual(request.template.map((item) => item.label), ['Uno', 'Dos']);
  deliver({ requestId: request.requestId + 1, id: '0' });
  deliver({ requestId: request.requestId, id: '1' });
  deliver({ requestId: request.requestId, id: '1' });
  assert.deepEqual(chosen, ['dos']);
});

test('page menus go to the in-page host at the pointer or a given position', () => {
  installBridge({ menuStyle: () => 'page', popupMenu: () => assert.fail('native popup') });
  const rendered = [];
  const unregister = registerContextMenuHost((menu) => rendered.push(menu));
  let prevented = false;
  showContextMenu(
    { clientX: 40, clientY: 70, preventDefault: () => { prevented = true; }, stopPropagation() {} },
    [{ label: 'Reproducir', onSelect() {} }],
  );
  showContextMenu(null, [{ label: 'Menú' }], { x: 12, y: 50 });
  showContextMenu(null, [{ label: 'Oculto', visible: false }]);
  unregister();
  assert.equal(prevented, true);
  assert.deepEqual(rendered.map(({ x, y, items }) => [x, y, labels(items)]), [
    [40, 70, ['Reproducir']],
    [12, 50, ['Menú']],
  ]);
});

test('text fields get cut, copy and paste according to their state', () => {
  assert.deepEqual(labels(editMenuItems(field())), ['Cortar', 'Copiar', 'Pegar', '—', 'Seleccionar todo']);
  const enabled = (items) => items.filter((item) => item !== 'separator').map((item) => item.enabled !== false);
  assert.deepEqual(enabled(editMenuItems(field())), [false, false, true, true]);
  assert.deepEqual(enabled(editMenuItems(field({ start: 0, end: 4 }))), [true, true, true, true]);
  assert.deepEqual(enabled(editMenuItems(field({ start: 0, end: 4, readOnly: true }))), [false, true, false, true]);
  assert.deepEqual(enabled(editMenuItems(field({ type: 'password', start: 0, end: 4 }))), [false, false, true, true]);
  assert.deepEqual(editMenuItems(field({ type: 'range' })), []);
});

test('selected page text offers copy and plain clicks offer nothing', () => {
  const text = { closest: () => null };
  assert.deepEqual(labels(editMenuItems(text, 'Headless Uno')), ['Copiar']);
  assert.deepEqual(editMenuItems(text, '   '), []);
});

test('edit commands run in the focused page through the bridge', () => {
  const commands = [];
  installBridge({ editCommand: (command) => commands.push(command) });
  const items = editMenuItems(field({ start: 1, end: 3 })).filter((item) => item !== 'separator');
  items.forEach((item) => item.onSelect());
  assert.deepEqual(commands, ['cut', 'copy', 'paste', 'selectAll']);
});

test('application menu items click the original item by id and label', () => {
  const clicks = [];
  const items = applicationMenuItems([
    { id: '0', label: 'Archivo', enabled: true, submenu: [
      { id: '0.0', label: 'Importar archivos…', enabled: true, accelerator: 'CmdOrCtrl+O' },
      { type: 'separator' },
      { id: '0.2', label: 'Aleatorio', enabled: true, checked: true },
    ] },
  ], { clickAppMenuItem: (item) => clicks.push(item) });
  const [file] = items;
  assert.equal(file.submenu[0].accelerator, 'CmdOrCtrl+O');
  assert.equal(file.submenu[1], 'separator');
  assert.equal(file.submenu[2].checked, true);
  file.submenu[0].onSelect();
  assert.deepEqual(clicks, [{ id: '0.0', label: 'Importar archivos…' }]);
});
