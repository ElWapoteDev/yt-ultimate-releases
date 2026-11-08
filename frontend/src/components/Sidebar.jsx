import React from 'react';
import {
  House,
  Search,
  Music2,
  ListMusic,
  Download,
  Scissors,
  Settings,
  Menu,
} from 'lucide-react';
import { getDesktopBridge } from '../lib/desktop';
import { applicationMenuItems, showContextMenu, usesNativeMenus } from '../lib/contextMenu';
import { isWindows } from '../lib/platform';

const mainItems = [
  { id: 'home', icon: House, label: 'Inicio' },
  { id: 'search', icon: Search, label: 'Buscar' },
];

const libraryItems = [
  { id: 'library', icon: Music2, label: 'Canciones' },
  { id: 'playlists', icon: ListMusic, label: 'Playlists' },
  { id: 'stems', icon: Scissors, label: 'Stems' },
];

async function openApplicationMenu(event) {
  const bounds = event.currentTarget.getBoundingClientRect();
  const position = { x: bounds.left, y: bounds.bottom + 4 };
  const bridge = getDesktopBridge();
  if (usesNativeMenus(bridge)) {
    bridge?.showAppMenu?.(position);
    return;
  }
  const template = await bridge?.getAppMenu?.();
  showContextMenu(null, applicationMenuItems(template, bridge), position);
}

function NavButton({ item, active, onClick, badge }) {
  const Icon = item.icon;
  return (
    <button
      onClick={onClick}
      className={`sidebar-item ${active ? 'active' : ''}`}
    >
      <Icon className="sidebar-icon" size={16} strokeWidth={active ? 2.1 : 1.8} />
      <span className="sidebar-item__label">{item.label}</span>
      {badge > 0 && <span className="sidebar-badge">{badge}</span>}
    </button>
  );
}

export default function Sidebar({
  activeTab,
  setTab,
  translucent,
  activeDownloads,
  showAdvancedTools = false,
}) {
  return (
    <aside className={`sidebar ${translucent ? '' : 'sidebar--opaque'}`}>
      <div className="sidebar-drag">
        {isWindows && getDesktopBridge()?.showAppMenu && (
          <button
            type="button"
            className="sidebar-menu-button"
            onClick={openApplicationMenu}
            aria-label="Menú de la aplicación"
            title="Menú"
          >
            <Menu size={16} strokeWidth={1.8} />
          </button>
        )}
      </div>

      <div className="sidebar-scroll">
        <div>
          {mainItems.map((item) => (
            <NavButton
              key={item.id}
              item={item}
              active={activeTab === item.id}
              onClick={() => setTab(item.id)}
            />
          ))}
        </div>

        <div className="sidebar-section-label">Biblioteca</div>
        {libraryItems
          .filter((item) => showAdvancedTools || item.id !== 'stems')
          .map((item) => (
            <NavButton
              key={item.id}
              item={item}
              active={activeTab === item.id}
              onClick={() => setTab(item.id)}
            />
          ))}

        <div className="sidebar-spacer" />

        <NavButton
          item={{ id: 'downloads', icon: Download, label: 'Descargas' }}
          active={activeTab === 'downloads'}
          onClick={() => setTab('downloads')}
          badge={activeDownloads}
        />
        <NavButton
          item={{ id: 'settings', icon: Settings, label: 'Ajustes' }}
          active={activeTab === 'settings'}
          onClick={() => setTab('settings')}
        />
      </div>
    </aside>
  );
}
