import { Menu } from 'electron';
import { fileManagerName, isMac } from './platform.mjs';

const NAVIGATION_ITEMS = Object.freeze([
  ['Inicio', 'home', '1'],
  ['Buscar', 'search', '2'],
  ['Canciones', 'library', '3'],
  ['Playlists', 'playlists', '4'],
  ['Stems', 'stems', '5'],
  ['Descargas', 'downloads', '6'],
  ['Ajustes', 'settings', '7'],
]);

const PLAYBACK_RATES = Object.freeze([0.75, 1, 1.25, 1.5, 2]);

// Electron labels roles in English outside macOS, whose native titles stay.
function roleItem(role, label, options = {}) {
  return isMac ? { role, ...options } : { role, label, ...options };
}

export class AppMenuController {
  constructor({ app, isDev, send, actions }) {
    this.app = app;
    this.isDev = isDev;
    this.send = send;
    this.actions = actions;
    this.state = {
      hasMedia: false,
      isPlaying: false,
      isVideo: false,
      shuffle: false,
      repeat: 'off',
      playbackRate: 1,
      developerMode: false,
      showAdvancedTools: false,
      pluginCommands: [],
    };
  }

  update(patch = {}) {
    this.state = { ...this.state, ...patch };
    this.rebuild();
  }

  playbackTemplate() {
    const state = this.state;
    return {
      label: 'Reproducción',
      submenu: [
        {
          label: state.isPlaying ? 'Pausar' : 'Reproducir',
          enabled: state.hasMedia,
          click: () => this.send('togglePlay'),
        },
        {
          label: 'Anterior',
          enabled: state.hasMedia,
          accelerator: 'CmdOrCtrl+Alt+Left',
          click: () => this.send('skipPrev'),
        },
        {
          label: 'Siguiente',
          enabled: state.hasMedia,
          accelerator: 'CmdOrCtrl+Alt+Right',
          click: () => this.send('skipNext'),
        },
        { type: 'separator' },
        {
          label: 'Retroceder 10 segundos',
          enabled: state.hasMedia,
          accelerator: 'CmdOrCtrl+Shift+Left',
          click: () => this.send('seekBackward'),
        },
        {
          label: 'Adelantar 10 segundos',
          enabled: state.hasMedia,
          accelerator: 'CmdOrCtrl+Shift+Right',
          click: () => this.send('seekForward'),
        },
        { type: 'separator' },
        {
          label: 'Aleatorio',
          type: 'checkbox',
          checked: state.shuffle,
          accelerator: 'Alt+CmdOrCtrl+S',
          click: () => this.send('toggleShuffle'),
        },
        {
          label: state.repeat === 'one' ? 'Repetir una' : 'Repetir',
          type: 'checkbox',
          checked: state.repeat !== 'off',
          accelerator: 'Alt+CmdOrCtrl+R',
          click: () => this.send('cycleRepeat'),
        },
        {
          label: 'Velocidad',
          submenu: PLAYBACK_RATES.map((rate) => ({
            label: `${rate}×`,
            type: 'radio',
            checked: state.playbackRate === rate,
            click: () => this.send('setPlaybackRate', rate),
          })),
        },
        { type: 'separator' },
        {
          label: 'Silenciar',
          enabled: state.hasMedia,
          accelerator: 'CmdOrCtrl+Shift+M',
          click: () => this.send('toggleMute'),
        },
        {
          label: 'Mostrar vídeo',
          visible: state.isVideo,
          enabled: state.isVideo,
          click: () => this.send('showVideo'),
        },
      ],
    };
  }

  developerTemplate() {
    if (!this.state.developerMode) return null;
    const pluginItems = this.state.pluginCommands.length
      ? this.state.pluginCommands.map((command) => ({
          label: command.label || command.name,
          enabled: command.enabled !== false,
          click: () => this.send('runPluginCommand', {
            pluginId: command.plugin_id,
            command: command.name,
          }),
        }))
      : [{ label: 'No hay comandos registrados', enabled: false }];

    return {
      label: 'Desarrollo',
      submenu: [
        {
          label: 'Abrir carpeta de plugins',
          accelerator: 'Alt+CmdOrCtrl+P',
          click: () => this.actions.openPluginsFolder(),
        },
        {
          label: 'Recargar plugins',
          accelerator: 'Alt+CmdOrCtrl+Shift+R',
          click: () => this.send('reloadPlugins'),
        },
        {
          label: 'Crear plugin…',
          click: () => this.send('navigate', 'settings', 'plugin-scaffold'),
        },
        { type: 'separator' },
        { label: 'Comandos de plugins', submenu: pluginItems },
        { type: 'separator' },
        {
          label: 'Abrir logs',
          click: () => this.actions.openLogsFolder(),
        },
        {
          label: 'Documentación del SDK',
          click: () => this.actions.openPluginDocumentation(),
        },
        { type: 'separator' },
        {
          label: 'Herramientas de desarrollo',
          accelerator: 'Alt+CmdOrCtrl+I',
          click: () => this.actions.toggleDevTools(),
        },
        {
          label: 'Recargar interfaz',
          accelerator: 'CmdOrCtrl+Alt+R',
          click: () => this.actions.reloadRenderer(),
        },
      ],
    };
  }

  rebuild() {
    const preferences = {
      label: 'Ajustes…',
      accelerator: 'CmdOrCtrl+,',
      click: () => this.send('preferences'),
    };
    const template = [
      isMac
        ? {
            label: this.app.name,
            submenu: [
              { role: 'about' },
              preferences,
              { type: 'separator' },
              { role: 'services' },
              { type: 'separator' },
              { role: 'hide' },
              { role: 'hideOthers' },
              { role: 'unhide' },
              { type: 'separator' },
              { role: 'quit' },
            ],
          }
        : null,
      {
        label: 'Archivo',
        submenu: [
          {
            label: 'Importar archivos…',
            accelerator: 'CmdOrCtrl+O',
            click: () => this.send('importFiles'),
          },
          {
            label: 'Importar carpeta…',
            accelerator: 'CmdOrCtrl+Shift+O',
            click: () => this.send('importFolder'),
          },
          // Windows lists recent files in the taskbar Jump List instead.
          ...(isMac
            ? [{
                label: 'Abrir recientes',
                role: 'recentDocuments',
                submenu: [{ role: 'clearRecentDocuments' }],
              }]
            : []),
          { type: 'separator' },
          {
            label: `Mostrar biblioteca en ${fileManagerName}`,
            accelerator: 'CmdOrCtrl+Shift+L',
            click: () => this.actions.openLibraryFolder(),
          },
          { type: 'separator' },
          ...(isMac
            ? [{ role: 'close' }]
            : [preferences, { type: 'separator' }, { role: 'quit', label: 'Salir' }]),
        ],
      },
      {
        label: 'Edición',
        submenu: [
          roleItem('undo', 'Deshacer'),
          roleItem('redo', 'Rehacer'),
          { type: 'separator' },
          roleItem('cut', 'Cortar'),
          roleItem('copy', 'Copiar'),
          roleItem('paste', 'Pegar'),
          roleItem('selectAll', 'Seleccionar todo'),
        ],
      },
      this.playbackTemplate(),
      {
        label: 'Visualización',
        submenu: [
          ...NAVIGATION_ITEMS
            .filter(([, id]) => this.state.showAdvancedTools || id !== 'stems')
            .map(([label, id, key]) => ({
              label,
              accelerator: `CmdOrCtrl+${key}`,
              click: () => this.send('navigate', id),
            })),
          { type: 'separator' },
          {
            label: 'Cola de reproducción',
            accelerator: 'CmdOrCtrl+0',
            click: () => this.send('toggleQueue'),
          },
          { type: 'separator' },
          roleItem('resetZoom', 'Tamaño real'),
          roleItem('zoomIn', 'Acercar'),
          roleItem('zoomOut', 'Alejar'),
          { type: 'separator' },
          roleItem('togglefullscreen', 'Pantalla completa'),
          ...(this.isDev && !this.state.developerMode
            ? [
                { type: 'separator' },
                roleItem('reload', 'Recargar'),
                roleItem('toggleDevTools', 'Herramientas de desarrollo'),
              ]
            : []),
        ],
      },
      {
        label: 'Ventana',
        submenu: isMac
          ? [
              { role: 'minimize' },
              { role: 'zoom' },
              { type: 'separator' },
              { role: 'front' },
            ]
          : [
              { role: 'minimize', label: 'Minimizar' },
              { role: 'close', label: 'Cerrar ventana' },
            ],
      },
      this.developerTemplate(),
      roleItem('help', 'Ayuda', {
        submenu: [
          {
            label: 'Guía de YT Ultimate',
            click: () => this.actions.openHelp(),
          },
          {
            label: 'Reportar un problema…',
            click: () => this.send('navigate', 'settings', 'diagnostics'),
          },
          ...(isMac
            ? []
            : [
                { type: 'separator' },
                ...(this.actions.checkForUpdates
                  ? [{
                      label: 'Buscar actualizaciones…',
                      click: () => this.actions.checkForUpdates(),
                    }]
                  : []),
                { role: 'about', label: `Acerca de ${this.app.name}` },
              ]),
        ],
      }),
    ].filter(Boolean);

    Menu.setApplicationMenu(Menu.buildFromTemplate(template));
  }

  // Frameless Windows windows have no menu bar, so the renderer opens the
  // application menu from a button; accelerators work either way.
  popup(window, position = {}) {
    const menu = Menu.getApplicationMenu();
    if (!menu || !window) return false;
    const x = Number.isFinite(position?.x) ? Math.round(position.x) : undefined;
    const y = Number.isFinite(position?.y) ? Math.round(position.y) : undefined;
    menu.popup(x === undefined || y === undefined ? { window } : { window, x, y });
    return true;
  }
}
