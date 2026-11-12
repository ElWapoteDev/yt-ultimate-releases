import React, { useCallback, useEffect, useMemo, useState } from 'react';
import toast from 'react-hot-toast';
import {
  Activity,
  Blocks,
  Check,
  ChevronRight,
  Code2,
  FolderOpen,
  Gauge,
  HardDrive,
  Music,
  Plus,
  RefreshCw,
  ShieldAlert,
  Trash2,
  Video,
  Wrench,
  X,
} from 'lucide-react';
import { api, getApiError } from '../lib/api';
import { getDesktopBridge } from '../lib/desktop';
import { runtime } from '../lib/runtime';
import { fileManagerName } from '../lib/platform';

const AUDIO_OPTIONS = Object.freeze([
  { val: '128', label: '128 kbps', desc: 'Ahorro de espacio' },
  { val: '192', label: '192 kbps', desc: 'Equilibrado' },
  { val: '256', label: '256 kbps', desc: 'Alta calidad' },
  { val: '320', label: '320 kbps', desc: 'Máxima calidad' },
]);

const VIDEO_OPTIONS = Object.freeze([
  { val: 'best', label: 'Máxima disponible', desc: 'Usa la mejor fuente compatible' },
  { val: '2160', label: '2160p', desc: '4K' },
  { val: '1080', label: '1080p', desc: 'Full HD' },
  { val: '720', label: '720p', desc: 'Ahorro de espacio' },
]);

function SettingsChoice({ active, label, description, onClick }) {
  return (
    <button type="button" onClick={onClick} className="settings-row">
      <span className="settings-row__label">
        <span className="settings-row__title">{label}</span>
        <span className="settings-row__sub">{description}</span>
      </span>
      <span className={`settings-row__check ${active ? 'active' : ''}`}>
        <Check size={11} strokeWidth={3} />
      </span>
    </button>
  );
}

function ToggleRow({ title, description, checked, onChange, tone = 'default' }) {
  return (
    <button
      type="button"
      className={`settings-row settings-row--toggle settings-row--${tone}`}
      onClick={() => onChange(!checked)}
      role="switch"
      aria-checked={checked}
    >
      <span className="settings-row__label">
        <span className="settings-row__title">{title}</span>
        <span className="settings-row__sub">{description}</span>
      </span>
      <span className={`settings-switch ${checked ? 'active' : ''}`}>
        <span />
      </span>
    </button>
  );
}

function PluginCard({ plugin, onToggle }) {
  const loaded = plugin.status === 'loaded' || plugin.loaded === true;
  const enabled = plugin.enabled === true;
  return (
    <div className="plugin-card">
      <div className="plugin-card__icon"><Blocks size={17} /></div>
      <div className="plugin-card__body">
        <div className="plugin-card__title">
          {plugin.name || plugin.id}
          <span className={`plugin-status ${loaded ? 'plugin-status--ok' : enabled ? 'plugin-status--warn' : ''}`}>
            {loaded ? 'Activo' : enabled ? 'Con error' : 'Inactivo'}
          </span>
        </div>
        <div className="plugin-card__meta">
          {plugin.id} · v{plugin.version || '0.0.0'}
        </div>
        {plugin.description && <div className="plugin-card__description">{plugin.description}</div>}
        {plugin.error && <div className="plugin-card__error">{plugin.error}</div>}
      </div>
      <button
        type="button"
        className={`settings-switch ${enabled ? 'active' : ''}`}
        role="switch"
        aria-checked={enabled}
        aria-label={`${enabled ? 'Desactivar' : 'Activar'} ${plugin.name || plugin.id}`}
        onClick={() => onToggle(plugin, !enabled)}
      >
        <span />
      </button>
    </div>
  );
}

function PluginExampleCard({ example, installing, onInstall }) {
  const levelLabel = example.level === 'advanced' ? 'Avanzado' : 'Básico';
  const permissionCount = Array.isArray(example.permissions) ? example.permissions.length : 0;

  return (
    <div className="plugin-example-card">
      <div className="plugin-example-card__body">
        <div className="plugin-example-card__eyebrow">
          <span className={`plugin-example-level plugin-example-level--${example.level}`}>
            {levelLabel}
          </span>
          <span>Ejemplo oficial</span>
        </div>
        <div className="plugin-example-card__title">{example.name || example.id}</div>
        <div className="plugin-example-card__description">{example.description}</div>
        <div className="plugin-example-card__meta">
          {permissionCount
            ? `${permissionCount} ${permissionCount === 1 ? 'permiso declarado' : 'permisos declarados'}`
            : 'Sin permisos especiales'}
          {' · '}
          v{example.version}
        </div>
      </div>
      <button
        type="button"
        className="btn-secondary plugin-example-card__action"
        disabled={example.installed || installing}
        onClick={() => onInstall(example)}
      >
        {installing && <RefreshCw size={12} className="spinner" />}
        {example.installed ? 'Instalado' : installing ? 'Instalando…' : 'Instalar'}
      </button>
    </div>
  );
}

export default function Settings({ active, section = '' }) {
  const desktop = getDesktopBridge();
  const [settings, setSettings] = useState({
    audio_quality: '192',
    video_quality: 'best',
    auto_thumbnail: true,
    max_concurrent_downloads: 2,
    max_concurrent_processing: 1,
    filename_template: '{title}',
    show_advanced_tools: false,
    developer_mode: false,
  });
  const [stats, setStats] = useState(null);
  const [health, setHealth] = useState(null);
  const [plugins, setPlugins] = useState([]);
  const [pluginExamples, setPluginExamples] = useState([]);
  const [loadingPlugins, setLoadingPlugins] = useState(false);
  const [installingExamples, setInstallingExamples] = useState(() => new Set());
  const [showScaffold, setShowScaffold] = useState(false);
  const [scaffold, setScaffold] = useState({ plugin_id: '', name: '', description: '' });

  const loadPlugins = useCallback(async () => {
    setLoadingPlugins(true);
    try {
      const [pluginsResponse, examplesResponse] = await Promise.allSettled([
        api.get('/developer/plugins'),
        api.get('/developer/plugins/examples'),
      ]);
      if (pluginsResponse.status === 'fulfilled') {
        const pluginData = pluginsResponse.value.data;
        setPlugins(Array.isArray(pluginData) ? pluginData : pluginData.plugins || []);
      } else {
        setPlugins([]);
      }
      if (examplesResponse.status === 'fulfilled') {
        const exampleData = examplesResponse.value.data;
        setPluginExamples(
          Array.isArray(exampleData) ? exampleData : exampleData.examples || [],
        );
      } else {
        setPluginExamples([]);
      }
    } finally {
      setLoadingPlugins(false);
    }
  }, []);

  const load = useCallback(async () => {
    try {
      const [settingsResponse, statsResponse, healthResponse] = await Promise.all([
        api.get('/settings'),
        api.get('/library/stats'),
        api.get('/system/health'),
      ]);
      setSettings(settingsResponse.data);
      setStats(statsResponse.data);
      setHealth(healthResponse.data);
      if (settingsResponse.data.developer_mode) await loadPlugins();
    } catch {
      // The desktop shell reports a backend that cannot start.
    }
  }, [loadPlugins]);

  useEffect(() => {
    if (active) void load();
  }, [active, load]);

  useEffect(() => {
    if (active && section === 'plugin-scaffold' && settings.developer_mode) {
      setShowScaffold(true);
    }
  }, [active, section, settings.developer_mode]);

  useEffect(() => {
    const reload = () => void loadPlugins();
    window.addEventListener('ytu:reload-plugins', reload);
    return () => window.removeEventListener('ytu:reload-plugins', reload);
  }, [loadPlugins]);

  const update = useCallback(async (patch) => {
    const previous = settings;
    const next = { ...settings, ...patch };
    setSettings(next);
    try {
      const { data } = await api.patch('/settings', patch);
      setSettings(data);
      window.dispatchEvent(new CustomEvent('ytu:settings-changed', { detail: data }));
      return data;
    } catch (error) {
      setSettings(previous);
      toast.error(getApiError(error, 'No se pudo guardar el ajuste'));
      return null;
    }
  }, [settings]);

  const toggleDeveloperMode = useCallback(async (enabled) => {
    if (
      enabled
      && !window.confirm(
        'El modo desarrollador puede ejecutar plugins Python con acceso a tus archivos. '
        + 'Actívalo sólo si entiendes y confías en su código.',
      )
    ) return;
    const saved = await update({ developer_mode: enabled });
    if (saved?.developer_mode) await loadPlugins();
    else {
      setPlugins([]);
      setPluginExamples([]);
    }
  }, [loadPlugins, update]);

  const clean = async () => {
    try {
      const { data } = await api.post('/library/clean');
      toast.success(data.cleaned
        ? `${data.cleaned} archivos temporales eliminados`
        : 'No había archivos temporales');
    } catch (error) {
      toast.error(getApiError(error, 'No se pudo limpiar la biblioteca'));
    }
  };

  const reloadPlugins = async () => {
    const toastId = toast.loading('Recargando plugins…');
    try {
      await api.post('/developer/plugins/reload');
      await loadPlugins();
      toast.success('Plugins recargados', { id: toastId });
      window.dispatchEvent(new CustomEvent('ytu:settings-changed'));
    } catch (error) {
      toast.error(getApiError(error, 'No se pudieron recargar los plugins'), { id: toastId });
    }
  };

  const togglePlugin = async (plugin, enabled) => {
    try {
      await api.put(`/developer/plugins/${encodeURIComponent(plugin.id)}/enabled`, { enabled });
      await loadPlugins();
      window.dispatchEvent(new CustomEvent('ytu:settings-changed'));
    } catch (error) {
      toast.error(getApiError(error, 'No se pudo cambiar el plugin'));
    }
  };

  const installExample = async (example) => {
    setInstallingExamples((current) => {
      const next = new Set(current);
      next.add(example.id);
      return next;
    });
    const toastId = toast.loading(`Instalando ${example.name || example.id}…`);
    try {
      await api.post(
        `/developer/plugins/examples/${encodeURIComponent(example.id)}/install`,
      );
      await loadPlugins();
      toast.success('Ejemplo instalado; puedes activarlo cuando quieras', { id: toastId });
    } catch (error) {
      toast.error(getApiError(error, 'No se pudo instalar el ejemplo'), { id: toastId });
    } finally {
      setInstallingExamples((current) => {
        const next = new Set(current);
        next.delete(example.id);
        return next;
      });
    }
  };

  const createPlugin = async () => {
    try {
      const { data } = await api.post('/developer/plugins/scaffold', scaffold);
      toast.success(`Plugin creado en ${data.path || 'la carpeta de plugins'}`);
      setScaffold({ plugin_id: '', name: '', description: '' });
      setShowScaffold(false);
      await loadPlugins();
    } catch (error) {
      toast.error(getApiError(error, 'No se pudo crear el plugin'));
    }
  };

  const engineRows = useMemo(() => {
    const engines = health?.engines || {};
    return [
      ['yt-dlp', engines.ytdlp],
      ['FFmpeg', engines.ffmpeg],
      ['FFprobe', engines.ffprobe],
      ['Separador', engines.separator],
    ];
  }, [health]);

  return (
    <div className="page-shell page-shell--narrow">
      <div className="page-header">
        <h1 className="page-title">Ajustes</h1>
        <div className="page-subtitle">Los cambios se guardan automáticamente</div>
      </div>

      <section className="settings-section">
        <div className="settings-section__header">
          <div className="settings-section__icon"><Music size={15} /></div>
          <div>
            <div className="settings-section__title">Calidad de audio</div>
            <div className="settings-section__sub">Para nuevas descargas MP3</div>
          </div>
        </div>
        {AUDIO_OPTIONS.map((option) => (
          <SettingsChoice
            key={option.val}
            active={settings.audio_quality === option.val}
            label={option.label}
            description={option.desc}
            onClick={() => update({ audio_quality: option.val })}
          />
        ))}
      </section>

      <section className="settings-section">
        <div className="settings-section__header">
          <div className="settings-section__icon"><Video size={15} /></div>
          <div>
            <div className="settings-section__title">Calidad de vídeo</div>
            <div className="settings-section__sub">Para nuevas descargas MP4</div>
          </div>
        </div>
        {VIDEO_OPTIONS.map((option) => (
          <SettingsChoice
            key={option.val}
            active={settings.video_quality === option.val}
            label={option.label}
            description={option.desc}
            onClick={() => update({ video_quality: option.val })}
          />
        ))}
      </section>

      <section className="settings-section">
        <div className="settings-section__header">
          <div className="settings-section__icon"><HardDrive size={15} /></div>
          <div>
            <div className="settings-section__title">Biblioteca</div>
            <div className="settings-section__sub">
              {health?.paths?.library || runtime.paths?.library || 'Administrada por YT Ultimate'}
            </div>
          </div>
        </div>
        {stats && (
          <div className="settings-stat-grid">
            {[
              { label: 'Archivos', value: stats.total_files },
              { label: 'Canciones', value: stats.audio_count },
              { label: 'Espacio', value: stats.total_size },
            ].map((row) => (
              <div key={row.label} className="settings-stat">
                <div className="settings-stat__value">{row.value}</div>
                <div className="settings-stat__label">{row.label}</div>
              </div>
            ))}
          </div>
        )}
        <button type="button" onClick={() => desktop?.openKnownFolder?.('library')} className="settings-row">
          <span className="settings-row__main">
            <FolderOpen size={15} />
            <span className="settings-row__label">
              <span className="settings-row__title">Abrir en {fileManagerName}</span>
            </span>
          </span>
          <ChevronRight size={13} />
        </button>
        <button type="button" onClick={clean} className="settings-row">
          <span className="settings-row__main">
            <Trash2 size={15} />
            <span className="settings-row__label">
              <span className="settings-row__title">Limpiar temporales</span>
              <span className="settings-row__sub">Elimina descargas incompletas y cachés obsoletos</span>
            </span>
          </span>
          <ChevronRight size={13} />
        </button>
      </section>

      <section className="settings-section">
        <div className="settings-section__header">
          <div className="settings-section__icon"><Activity size={15} /></div>
          <div>
            <div className="settings-section__title">Conexión con YouTube</div>
            <div className="settings-section__sub">
              Si YouTube pide iniciar sesión, elige las cookies de tu navegador o un archivo Netscape.
            </div>
          </div>
        </div>
        <div className="advanced-grid">
          <label className="advanced-grid__wide">
            <span>Cookies del navegador</span>
            <select
              aria-label="Cookies del navegador"
              value={settings.youtube_cookies_browser || ''}
              onChange={(event) => update({
                youtube_cookies_browser: event.target.value,
                ...(event.target.value ? { youtube_cookie_file: '' } : {}),
              })}
            >
              <option value="">Sin navegador</option>
              {['chrome', 'chromium', 'edge', 'firefox', 'safari', 'brave', 'opera', 'vivaldi', 'whale'].map((browser) => (
                <option key={browser} value={browser}>{browser}</option>
              ))}
            </select>
          </label>
          <label className="advanced-grid__wide">
            <span>Archivo de cookies</span>
            <input
              key={settings.youtube_cookie_file || 'empty-cookie-file'}
              type="text"
              aria-label="Archivo de cookies"
              defaultValue={settings.youtube_cookie_file || ''}
              placeholder="Ruta al archivo cookies.txt"
              onBlur={(event) => {
                const value = event.target.value.trim();
                if (value === (settings.youtube_cookie_file || '')) return;
                void update({
                  youtube_cookie_file: value,
                  ...(value ? { youtube_cookies_browser: '' } : {}),
                });
              }}
            />
          </label>
          <label>
            <span>Reintentos de descarga</span>
            <input
              key={`retries:${settings.download_retries}`}
              type="number"
              min="0"
              max="10"
              defaultValue={settings.download_retries ?? 2}
              onBlur={(event) => {
                if (event.target.checkValidity() && event.target.value !== '') {
                  void update({ download_retries: Number(event.target.value) });
                }
              }}
            />
          </label>
          <label>
            <span>Espera de red (segundos)</span>
            <input
              key={`timeout:${settings.download_socket_timeout}`}
              type="number"
              min="1"
              max="120"
              defaultValue={settings.download_socket_timeout ?? 20}
              onBlur={(event) => {
                if (event.target.checkValidity() && event.target.value !== '') {
                  void update({ download_socket_timeout: Number(event.target.value) });
                }
              }}
            />
          </label>
        </div>
      </section>

      <details className="settings-disclosure" open={settings.developer_mode || undefined}>
        <summary>
          <span><Wrench size={15} /> Opciones avanzadas</span>
          <ChevronRight size={14} />
        </summary>
        <div className="settings-disclosure__content">
          <ToggleRow
            title="Herramientas avanzadas"
            description="Muestra Stems, fingerprint y controles técnicos"
            checked={Boolean(settings.show_advanced_tools)}
            onChange={(checked) => update({ show_advanced_tools: checked })}
          />
          <ToggleRow
            title="Modo desarrollador"
            description="Activa plugins locales, logs, diagnósticos y el menú Desarrollo"
            checked={Boolean(settings.developer_mode)}
            onChange={toggleDeveloperMode}
            tone="warning"
          />

          {settings.show_advanced_tools && (
            <div className="advanced-grid">
              <label>
                <span>Descargas simultáneas</span>
                <input
                  type="number"
                  min="1"
                  max="8"
                  value={settings.max_concurrent_downloads || 2}
                  onChange={(event) => update({ max_concurrent_downloads: Number(event.target.value) })}
                />
              </label>
              <label>
                <span>Procesos pesados</span>
                <input
                  type="number"
                  min="1"
                  max="4"
                  value={settings.max_concurrent_processing || 1}
                  onChange={(event) => update({ max_concurrent_processing: Number(event.target.value) })}
                />
              </label>
              <label className="advanced-grid__wide">
                <span>Plantilla de archivo</span>
                <input
                  type="text"
                  value={settings.filename_template || '{title}'}
                  onChange={(event) => setSettings((current) => ({
                    ...current,
                    filename_template: event.target.value,
                  }))}
                  onBlur={(event) => update({ filename_template: event.target.value || '{title}' })}
                />
              </label>
            </div>
          )}
        </div>
      </details>

      {settings.developer_mode && (
        <>
          <section className="settings-section settings-section--developer">
            <div className="settings-section__header">
              <div className="settings-section__icon"><Code2 size={15} /></div>
              <div>
                <div className="settings-section__title">Plugins</div>
                <div className="settings-section__sub">
                  Código local de confianza · API v1
                </div>
              </div>
              <div className="settings-section__actions">
                <button type="button" className="btn-icon-sm" onClick={reloadPlugins} title="Recargar">
                  <RefreshCw size={14} className={loadingPlugins ? 'spinner' : ''} />
                </button>
                <button type="button" className="btn-icon-sm" onClick={() => setShowScaffold(true)} title="Crear plugin">
                  <Plus size={14} />
                </button>
              </div>
            </div>

            <div className="developer-warning">
              <ShieldAlert size={15} />
              Los plugins pueden leer o modificar archivos con tus permisos. Instala sólo código que hayas revisado.
            </div>

            {pluginExamples.length > 0 && (
              <div className="plugin-examples">
                <div className="plugin-examples__header">
                  <div>
                    <div className="plugin-examples__title">Ejemplos incluidos</div>
                    <div className="plugin-examples__description">
                      Aprende desde un comando sencillo hasta eventos, estado y tareas asíncronas.
                    </div>
                  </div>
                </div>
                <div className="plugin-examples__grid">
                  {pluginExamples.map((example) => (
                    <PluginExampleCard
                      key={example.id}
                      example={example}
                      installing={installingExamples.has(example.id)}
                      onInstall={installExample}
                    />
                  ))}
                </div>
              </div>
            )}

            {!plugins.length && !loadingPlugins && (
              <div className="settings-empty">
                No hay plugins instalados. Elige un ejemplo o crea uno desde la plantilla.
              </div>
            )}
            {plugins.map((plugin) => (
              <PluginCard key={plugin.id} plugin={plugin} onToggle={togglePlugin} />
            ))}
            <button type="button" className="settings-row" onClick={() => desktop?.openKnownFolder?.('plugins')}>
              <span className="settings-row__main">
                <FolderOpen size={15} />
                <span className="settings-row__label">
                  <span className="settings-row__title">Abrir carpeta de plugins</span>
                  <span className="settings-row__sub">{runtime.paths?.plugins || 'Application Support'}</span>
                </span>
              </span>
              <ChevronRight size={13} />
            </button>
          </section>

          <section className="settings-section">
            <div className="settings-section__header">
              <div className="settings-section__icon"><Activity size={15} /></div>
              <div>
                <div className="settings-section__title">Diagnóstico</div>
                <div className="settings-section__sub">
                  Backend {health?.version || '—'} · instancia {health?.instance_id?.slice(0, 8) || '—'}
                </div>
              </div>
            </div>
            <div className="engine-grid">
              {engineRows.map(([name, available]) => (
                <div key={name} className="engine-row">
                  <Gauge size={13} />
                  <span>{name}</span>
                  <span className={available ? 'engine-ok' : 'engine-missing'}>
                    {available ? 'Disponible' : 'No disponible'}
                  </span>
                </div>
              ))}
            </div>
            <button type="button" className="settings-row" onClick={() => desktop?.openKnownFolder?.('logs')}>
              <span className="settings-row__main">
                <FolderOpen size={15} />
                <span className="settings-row__label">
                  <span className="settings-row__title">Abrir logs</span>
                </span>
              </span>
              <ChevronRight size={13} />
            </button>
          </section>
        </>
      )}

      <div className="settings-footer">
        YT Ultimate {runtime.appVersion} · Backend {health?.version || 'sin conexión'}
      </div>

      {showScaffold && (
        <div className="modal-backdrop" onClick={() => setShowScaffold(false)}>
          <div className="modal-panel plugin-scaffold" onClick={(event) => event.stopPropagation()}>
            <div className="modal-header">
              <div>
                <div className="modal-title">Crear plugin</div>
                <div className="page-subtitle">Genera una plantilla lista para editar</div>
              </div>
              <button type="button" className="btn-icon-sm" onClick={() => setShowScaffold(false)}>
                <X size={14} />
              </button>
            </div>
            <label>
              <span>ID</span>
              <input
                value={scaffold.plugin_id}
                placeholder="mi.plugin"
                onChange={(event) => setScaffold((current) => ({ ...current, plugin_id: event.target.value }))}
              />
            </label>
            <label>
              <span>Nombre</span>
              <input
                value={scaffold.name}
                placeholder="Mi plugin"
                onChange={(event) => setScaffold((current) => ({ ...current, name: event.target.value }))}
              />
            </label>
            <label>
              <span>Descripción</span>
              <textarea
                value={scaffold.description}
                placeholder="Qué hace este plugin"
                onChange={(event) => setScaffold((current) => ({ ...current, description: event.target.value }))}
              />
            </label>
            <div className="modal-actions">
              <button type="button" className="btn-secondary" onClick={() => setShowScaffold(false)}>Cancelar</button>
              <button
                type="button"
                className="btn-accent"
                disabled={!scaffold.plugin_id || !scaffold.name}
                onClick={createPlugin}
              >
                <Plus size={13} /> Crear
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
