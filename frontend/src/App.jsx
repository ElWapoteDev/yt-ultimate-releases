import React, { useEffect, useState, useCallback, useRef, useLayoutEffect } from 'react';
import { PlayerProvider, usePlayer } from './context/PlayerContext';
import toast, { Toaster } from 'react-hot-toast';
import { getDesktopMeta, getDesktopBridge } from './lib/desktop';
import { api } from './lib/api';
import { isWindows } from './lib/platform';
import { useMediaImport } from './hooks/useMediaImport';
import Sidebar from './components/Sidebar';
import Home from './components/Home';
import Search from './components/Search';
import Library from './components/Library';
import Playlists from './components/Playlists';
import Settings from './components/Settings';
import PlayerBar from './components/PlayerBar';
import Downloads from './components/Downloads';
import QueuePanel from './components/QueuePanel';
import Stems from './components/Stems';
import ImportDropZone from './components/ImportDropZone';
import ContextMenuHost from './components/ContextMenuHost';
import ChipmunkDialog from './components/ChipmunkDialog';
import './App.css';

const pages = {
  home: Home,
  search: Search,
  library: Library,
  playlists: Playlists,
  stems: Stems,
  downloads: Downloads,
  settings: Settings,
};

const TAB_ORDER = ['home', 'search', 'library', 'playlists', 'stems', 'downloads', 'settings'];

// Old tab ids that may still arrive from the native menu
const TAB_ALIASES = { radio: 'search', explore: 'search' };

// Keeps a page mounted and remembers its scroll position while hidden
function PageHost({ active, children }) {
  const ref = useRef(null);
  const pos = useRef(0);

  useLayoutEffect(() => {
    if (active && ref.current) ref.current.scrollTop = pos.current;
  }, [active]);

  return (
    <div
      ref={ref}
      className="page-host"
      hidden={!active}
      onScroll={(e) => { if (active) pos.current = e.currentTarget.scrollTop; }}
    >
      {children}
    </div>
  );
}

function AppInner() {
  const [tab, setTabState] = useState('home');
  const [navigationSection, setNavigationSection] = useState('');
  const [visited, setVisited] = useState(() => new Set(['home']));
  const [queueOpen, setQueueOpen] = useState(false);
  const [desktopMeta, setDesktopMeta] = useState({ isDesktop: false, platform: 'web' });
  const [activeDownloads, setActiveDownloads] = useState(0);
  const [userSettings, setUserSettings] = useState({
    developer_mode: false,
    show_advanced_tools: false,
  });
  const [pluginCommands, setPluginCommands] = useState([]);
  const {
    currentMedia,
    isPlaying,
    togglePlay,
    skipNext,
    skipPrev,
    seekBy,
    volume,
    setVolume,
    toggleMute,
    shuffle,
    repeat,
    playbackRate,
    setPlaybackRate,
    toggleShuffle,
    cycleRepeat,
    openVideo,
  } = usePlayer();
  const desktop = getDesktopBridge();
  const { importPaths, chooseAndImport } = useMediaImport();

  useEffect(() => {
    getDesktopMeta().then(setDesktopMeta).catch(() => {});
  }, []);

  const refreshApplicationSettings = useCallback(async () => {
    try {
      const { data } = await api.get('/settings');
      setUserSettings(data);
      if (data.developer_mode) {
        const response = await api.get('/developer/plugins').catch(() => ({ data: [] }));
        const plugins = Array.isArray(response.data)
          ? response.data
          : response.data.plugins || [];
        setPluginCommands(plugins.flatMap((plugin) => (
          (plugin.commands || []).map((command) => ({
            ...command,
            plugin_id: plugin.id,
            enabled: plugin.status === 'loaded' || plugin.loaded === true,
          }))
        )));
      } else {
        setPluginCommands([]);
      }
    } catch {
      // Backend startup failures are reported by the desktop shell.
    }
  }, []);

  useEffect(() => {
    const initialLoad = window.setTimeout(refreshApplicationSettings, 0);
    window.addEventListener('ytu:settings-changed', refreshApplicationSettings);
    return () => {
      window.clearTimeout(initialLoad);
      window.removeEventListener('ytu:settings-changed', refreshApplicationSettings);
    };
  }, [refreshApplicationSettings]);

  useEffect(() => {
    desktop?.updateMenuConfiguration?.({
      developerMode: Boolean(userSettings.developer_mode),
      showAdvancedTools: Boolean(userSettings.show_advanced_tools),
      pluginCommands,
    });
  }, [desktop, pluginCommands, userSettings]);

  const setTab = useCallback((nextTab, section = '') => {
    const resolved = TAB_ALIASES[nextTab] || nextTab;
    if (!pages[resolved]) return;
    setNavigationSection(resolved === 'settings' ? section : '');
    setVisited((prev) => {
      if (prev.has(resolved)) return prev;
      const next = new Set(prev);
      next.add(resolved);
      return next;
    });
    setTabState(resolved);
  }, []);

  const goToSearch = useCallback(() => {
    setTab('search');
    // Let the page render before focusing its input
    requestAnimationFrame(() => window.dispatchEvent(new CustomEvent('ytu:focus-search')));
  }, [setTab]);

  // Lightweight poll that feeds the sidebar downloads badge
  useEffect(() => {
    let active = true;
    const check = async () => {
      if (!active || document.hidden) return;
      try {
        const { data } = await api.get('/downloads');
        if (!active) return;
        const running = data.filter((d) => !['completed', 'error', 'canceled'].includes(d.status)).length;
        setActiveDownloads(running);
      } catch {
        if (active) setActiveDownloads(0);
      }
    };
    check();
    const iv = setInterval(check, 4000);
    return () => { active = false; clearInterval(iv); };
  }, []);

  // Native menu actions
  useEffect(() => {
    if (!desktop?.onMenuAction) return;
    return desktop.onMenuAction((action, param, section) => {
      switch (action) {
        case 'togglePlay': togglePlay(); break;
        case 'skipNext': skipNext(); break;
        case 'skipPrev': skipPrev(); break;
        case 'seekForward': seekBy(10); break;
        case 'seekBackward': seekBy(-10); break;
        case 'volumeUp': setVolume(Math.min(1, volume + 0.1)); break;
        case 'volumeDown': setVolume(Math.max(0, volume - 0.1)); break;
        case 'toggleMute': toggleMute(); break;
        case 'toggleShuffle': toggleShuffle(); break;
        case 'cycleRepeat': cycleRepeat(); break;
        case 'setPlaybackRate': setPlaybackRate(Number(param)); break;
        case 'showVideo': openVideo(); break;
        case 'importFiles': void chooseAndImport('files'); break;
        case 'importFolder': void chooseAndImport('folder'); break;
        case 'preferences': setTab('settings'); break;
        case 'reloadPlugins':
          window.dispatchEvent(new CustomEvent('ytu:reload-plugins'));
          break;
        case 'runPluginCommand':
          if (param?.pluginId && param?.command) {
            void api.post(
              `/developer/plugins/${encodeURIComponent(param.pluginId)}/commands/${encodeURIComponent(param.command)}`,
              { arguments: {} },
            ).then(({ data }) => {
              toast.success(data?.message || 'Comando ejecutado');
            }).catch(() => toast.error('El comando del plugin falló'));
          }
          break;
        case 'toggleQueue': setQueueOpen((open) => !open); break;
        case 'navigate':
          if (param === 'queue') setQueueOpen((open) => !open);
          else if (param === 'search') goToSearch();
          else if (param && (param !== 'stems' || userSettings.show_advanced_tools)) {
            setTab(param, section);
          }
          break;
      }
    });
  }, [
    chooseAndImport,
    cycleRepeat,
    desktop,
    goToSearch,
    openVideo,
    seekBy,
    setPlaybackRate,
    setTab,
    setVolume,
    skipNext,
    skipPrev,
    toggleMute,
    togglePlay,
    toggleShuffle,
    userSettings.show_advanced_tools,
    volume,
  ]);

  useEffect(() => {
    if (!desktop?.onOpenFiles) return undefined;
    return desktop.onOpenFiles((paths) => {
      void importPaths(paths).then((result) => {
        if (result) setTab('library');
      });
    });
  }, [desktop, importPaths, setTab]);

  useEffect(() => {
    desktop?.updatePlaybackState?.({
      hasMedia: Boolean(currentMedia),
      isPlaying,
      shuffle,
      repeat,
      playbackRate,
      isVideo: currentMedia?.type === 'video',
    });
  }, [currentMedia, desktop, isPlaying, playbackRate, repeat, shuffle]);

  // Keyboard shortcuts
  useEffect(() => {
    const handler = (e) => {
      const tag = e.target.tagName;
      const typing = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || e.target.isContentEditable;

      if (e.key === 'Escape') {
        setQueueOpen(false);
        return;
      }

      if (typing) return;

      if (e.code === 'Space') {
        e.preventDefault();
        togglePlay();
        return;
      }

      if ((e.metaKey || e.ctrlKey) && e.key === 'f') {
        e.preventDefault();
        goToSearch();
        return;
      }

      if ((e.metaKey || e.ctrlKey) && e.key >= '1' && e.key <= '9') {
        const idx = parseInt(e.key, 10) - 1;
        const visibleTabs = userSettings.show_advanced_tools
          ? TAB_ORDER
          : TAB_ORDER.filter((id) => id !== 'stems');
        if (visibleTabs[idx]) {
          e.preventDefault();
          setTab(visibleTabs[idx]);
        }
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [setTab, togglePlay, goToSearch, userSettings.show_advanced_tools]);

  return (
    <div className="app-root">
      <div className="app-body">
        <Sidebar
          activeTab={tab}
          setTab={setTab}
          translucent={desktopMeta.isDesktop && !isWindows}
          activeDownloads={activeDownloads}
          showAdvancedTools={Boolean(userSettings.show_advanced_tools)}
        />

        <main className="main-wrap">
          <div className="content-drag" />
          {TAB_ORDER.filter((id) => visited.has(id)).map((id) => {
            const Page = pages[id];
            return (
              <PageHost key={id} active={tab === id}>
                <Page
                  changeTab={setTab}
                  active={tab === id}
                  section={id === 'settings' ? navigationSection : ''}
                  showAdvancedTools={Boolean(userSettings.show_advanced_tools)}
                />
              </PageHost>
            );
          })}

          {queueOpen && <QueuePanel onClose={() => setQueueOpen(false)} />}
        </main>
      </div>

      <PlayerBar queueOpen={queueOpen} onToggleQueue={() => setQueueOpen((open) => !open)} />
      <ImportDropZone
        onImport={(paths) => importPaths(paths).then((result) => {
          if (result) setTab('library');
        })}
      />
      <ContextMenuHost />
      <ChipmunkDialog />

      <Toaster
        position="top-center"
        toastOptions={{
          style: {
            background: 'rgba(40, 40, 43, 0.82)',
            backdropFilter: 'blur(40px) saturate(180%)',
            WebkitBackdropFilter: 'blur(40px) saturate(180%)',
            color: 'rgba(255,255,255,0.95)',
            borderRadius: 10,
            fontSize: 13,
            fontWeight: 500,
            padding: '8px 14px',
            border: '0.5px solid rgba(255,255,255,0.10)',
            boxShadow: '0 12px 40px rgba(0,0,0,0.5)',
          },
          success: { iconTheme: { primary: '#30d158', secondary: '#fff' } },
          error: { iconTheme: { primary: '#ff453a', secondary: '#fff' } },
        }}
      />
    </div>
  );
}

export default function App() {
  return (
    <PlayerProvider>
      <AppInner />
    </PlayerProvider>
  );
}
