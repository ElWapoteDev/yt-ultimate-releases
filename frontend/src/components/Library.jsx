import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import toast from 'react-hot-toast';
import { usePlayer } from '../context/PlayerContext';
import { canDragOut, dragFileOut } from '../lib/desktop';
import { showContextMenu } from '../lib/contextMenu';
import { trackChipmunkItems, trackFileItems, trackPlaybackItems } from '../lib/trackMenu';
import { api, getApiError } from '../lib/api';
import { fingerprintCompletionSummary, fingerprintLocalComparisonSummary, fingerprintProfileSummary, fingerprintProgressLabel, fingerprintRecognitionSummary, fingerprintSummary, fingerprintVocalSummary, followFingerprintJob } from '../lib/fingerprint';
import { formatDuration, thumbUrl } from '../lib/format';
import { useMediaImport } from '../hooks/useMediaImport';
import { deviceName } from '../lib/platform';
import {
  FilePlus2, Grid3X3, List, MoreHorizontal, Music, Play, Search as SearchIcon, Shuffle, Video, X,
} from 'lucide-react';
import { Thumb, EmptyState, MediaCard, PlayingIndicator } from './ui';

export default function Library({ active, showAdvancedTools = false }) {
  const [files, setFiles] = useState([]);
  const [playlists, setPlaylists] = useState({});
  const [view, setView] = useState('grid');
  const [filter, setFilter] = useState('all');
  const [query, setQuery] = useState('');
  const [infoModal, setInfoModal] = useState(null);
  const fingerprintJobs = useRef(new Map());
  const { playMedia, playAll, playNext, playLater, currentMedia, isPlaying, togglePlay } = usePlayer();
  const { chooseAndImport } = useMediaImport();

  useEffect(() => {
    const jobs = fingerprintJobs.current;
    return () => {
      for (const [key, controller] of jobs) {
        controller.abort();
        toast.dismiss(`fingerprint-${key}`);
      }
      jobs.clear();
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const [libraryResponse, playlistResponse] = await Promise.all([
        api.get('/library'),
        api.get('/playlists'),
      ]);
      setFiles(libraryResponse.data);
      setPlaylists(playlistResponse.data);
    } catch {
      // backend not ready yet
    }
  }, []);

  // Refresh whenever the tab becomes visible
  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    (async () => {
      try {
        const [libraryResponse, playlistResponse] = await Promise.all([
          api.get('/library'),
          api.get('/playlists'),
        ]);
        if (!alive) return;
        setFiles(libraryResponse.data);
        setPlaylists(playlistResponse.data);
      } catch {
        // backend not ready yet
      }
    })();
    return () => { alive = false; };
  }, [active]);

  useEffect(() => {
    const refresh = () => void load();
    window.addEventListener('ytu:library-changed', refresh);
    return () => window.removeEventListener('ytu:library-changed', refresh);
  }, [load]);

  const addToPlaylist = useCallback(async (playlistName, file) => {
    try {
      await api.post('/playlists/add', {
        playlist_name: playlistName,
        track_id: file.id || '',
        filename: file.filename,
      });
      toast.success(`Añadido a "${playlistName}"`);
      const updated = await api.get('/playlists');
      setPlaylists(updated.data);
    } catch {
      toast.error('No se pudo añadir a la playlist');
    }
  }, []);

  const createPlaylistWith = useCallback(async (file) => {
    const name = window.prompt('Nombre de la nueva playlist:');
    if (!name) return;
    try {
      await api.post('/playlists', { name });
      await api.post('/playlists/add', {
        playlist_name: name,
        track_id: file.id || '',
        filename: file.filename,
      });
      toast.success(`Playlist "${name}" creada`);
      const updated = await api.get('/playlists');
      setPlaylists(updated.data);
    } catch {
      toast.error('Error al crear playlist');
    }
  }, []);

  const deleteFile = useCallback(async (file) => {
    if (!window.confirm('¿Eliminar este archivo permanentemente?')) return;
    try {
      await api.delete(`/library/${encodeURIComponent(file.id || file.filename)}`);
      toast.success('Archivo eliminado');
      await load();
    } catch (error) {
      // 409: Windows keeps the file locked while it is playing.
      toast.error(error?.response?.status === 409
        ? getApiError(error)
        : 'No se pudo eliminar el archivo');
    }
  }, [load]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return files
      .filter((file) => filter === 'all' || file.type === filter)
      .filter((file) => !q || `${file.name} ${file.channel} ${file.filename}`.toLowerCase().includes(q));
  }, [files, filter, query]);

  const handlePlay = useCallback((file) => {
    if (currentMedia?.filename === file.filename) { togglePlay(); return; }
    playMedia(file);
  }, [currentMedia, playMedia, togglePlay]);

  const showFileInfo = useCallback((file) => {
    setInfoModal(file);
  }, []);

  const canSeparate = useCallback((file) => (
    file
    && (file.type === 'audio' || file.type === 'video')
    && file.playlist !== 'Separaciones'
  ), []);

  const canFingerprint = useCallback((file) => (
    file
    && file.type === 'audio'
    && file.playlist !== 'Protegidos'
  ), []);

  const startSeparation = useCallback(async (file) => {
    if (!canSeparate(file)) return;
    try {
      await api.post('/separator/jobs', {
        track_id: file.id || '',
        filename: file.filename,
        local_path: file.local_path,
        title: file.name,
        output_format: 'FLAC',
      });
      toast.success('Separación iniciada');
    } catch (e) {
      toast.error(e.response?.data?.detail || 'No se pudo iniciar la separación');
    }
  }, [canSeparate]);

  const startFingerprint = useCallback(async (file, intensity = 'strong') => {
    if (!canFingerprint(file)) return;
    const key = file.id || file.local_path || file.filename;
    if (fingerprintJobs.current.has(key)) return;
    const controller = new AbortController();
    fingerprintJobs.current.set(key, controller);
    const toastId = `fingerprint-${key}`;
    toast.loading(intensity === 'quality'
      ? 'CALIDAD: separando voz e instrumental…'
      : 'Modificando audio y verificando huella…', { id: toastId });
    try {
      const { data } = await api.post('/fingerprint/jobs', {
        track_id: file.id || '',
        filename: file.filename,
        local_path: file.local_path,
        title: file.name,
        intensity,
      }, { signal: controller.signal });
      const job = await followFingerprintJob(
        async () => (await api.get(`/fingerprint/jobs/${encodeURIComponent(data.id)}`, {
          signal: controller.signal,
        })).data,
        (progress) => toast.loading(
          `${fingerprintProgressLabel(progress.phase, intensity)}… ${Math.round(progress.progress || 0)}%`,
          { id: toastId },
        ),
        { signal: controller.signal },
      );
      if (job.status === 'error') {
        toast.error(job.error || 'No se pudo modificar el fingerprint', { id: toastId });
      } else if (job.status === 'canceled') {
        toast('Modificación cancelada', { id: toastId });
      } else {
        window.dispatchEvent(new CustomEvent('ytu:library-changed', { detail: job.output }));
        const summary = fingerprintCompletionSummary(job.output?.fingerprint_verification);
        if (job.output?.registration_error) {
          toast.error(`Archivo creado, pero no se pudo añadir a Canciones: ${job.output.registration_error}`, { id: toastId });
        } else {
          toast(summary, { id: toastId, duration: 9000 });
        }
      }
    } catch (e) {
      if (controller.signal.aborted) toast.dismiss(toastId);
      else toast.error(e.response ? getApiError(e) : e.message || 'No se pudo modificar el fingerprint', { id: toastId });
    } finally {
      fingerprintJobs.current.delete(key);
    }
  }, [canFingerprint]);

  const openContextMenu = (event, file) => {
    const playlistNames = Object.keys(playlists);
    showContextMenu(event, [
      ...trackPlaybackItems(file, { play: handlePlay, playNext, playLater }),
      'separator',
      {
        label: 'Añadir a playlist',
        submenu: [
          ...(playlistNames.length
            ? playlistNames.map((name) => ({ label: name, onSelect: () => addToPlaylist(name, file) }))
            : [{ label: 'Sin playlists', enabled: false }]),
          'separator',
          { label: 'Nueva playlist…', onSelect: () => createPlaylistWith(file) },
        ],
      },
      'separator',
      {
        label: 'Separar vocales e instrumental',
        visible: showAdvancedTools,
        enabled: canSeparate(file),
        onSelect: () => startSeparation(file),
      },
      {
        label: 'Modificar fingerprint',
        visible: showAdvancedTools,
        enabled: canFingerprint(file),
        submenu: [
          { label: 'Intenso · FLAC', onSelect: () => startFingerprint(file, 'strong') },
          { label: 'Equilibrado · FLAC', onSelect: () => startFingerprint(file, 'balanced') },
          { label: 'CALIDAD · FLAC', onSelect: () => startFingerprint(file, 'quality') },
        ],
      },
      ...trackChipmunkItems(file),
      'separator',
      { label: 'Obtener información', onSelect: () => showFileInfo(file) },
      ...trackFileItems(file),
      'separator',
      { label: 'Eliminar de la biblioteca…', danger: true, onSelect: () => deleteFile(file) },
    ]);
  };

  const hasLibrary = files.length > 0;

  return (
    <div className="page-shell">
      <div className="toolbar">
        <div className="toolbar__left">
          <h1 className="page-title">Canciones</h1>
          <span className="page-count">{filtered.length}</span>
        </div>
        <div className="toolbar__right">
          {filtered.length > 0 && (
            <>
              <button onClick={() => playAll(filtered, 0)} className="btn-accent" title="Reproducir en orden">
                <Play size={13} fill="currentColor" /> Reproducir
              </button>
              <button
                onClick={() => playAll(filtered, 0, { shuffled: true })}
                className="btn-secondary"
                title="Reproducir en orden aleatorio"
              >
                <Shuffle size={13} />
              </button>
            </>
          )}
          <button
            type="button"
            onClick={() => chooseAndImport('files')}
            className="btn-secondary"
            title="Importar archivos o carpetas"
          >
            <FilePlus2 size={13} /> Importar
          </button>
          <div className="segmented">
            {[
              { id: 'all', label: 'Todo' },
              { id: 'audio', label: 'Audio' },
              { id: 'video', label: 'Vídeo' },
            ].map((f) => (
              <button
                key={f.id}
                onClick={() => setFilter(f.id)}
                className={`segment ${filter === f.id ? 'active' : ''}`}
              >
                {f.label}
              </button>
            ))}
          </div>
          <button
            onClick={() => setView((v) => (v === 'grid' ? 'list' : 'grid'))}
            className="btn-icon"
            title={view === 'grid' ? 'Ver como lista' : 'Ver como cuadrícula'}
          >
            {view === 'grid' ? <List size={15} /> : <Grid3X3 size={15} />}
          </button>
        </div>
      </div>

      {hasLibrary && (
        <div className="input-search-wrap" style={{ marginBottom: 22, maxWidth: 360 }}>
          <SearchIcon className="input-search-icon" size={14} />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Filtrar canciones"
            className="input-search"
          />
          {query && (
            <button onClick={() => setQuery('')} className="input-search-clear" aria-label="Limpiar">
              ×
            </button>
          )}
        </div>
      )}

      {!hasLibrary && (
        <EmptyState
          icon={Music}
          title="Aún no hay música"
          hint={`Descarga desde Buscar o añade música que ya tengas en ${deviceName}.`}
          action={(
            <button type="button" className="btn-accent" onClick={() => chooseAndImport('files')}>
              <FilePlus2 size={14} /> Importar música
            </button>
          )}
        />
      )}

      {hasLibrary && !filtered.length && (
        <EmptyState
          icon={SearchIcon}
          title="Nada coincide"
          hint="Prueba con otro término o cambia el filtro."
          compact
        />
      )}

      {view === 'grid' && filtered.length > 0 && (
        <div className="media-grid">
          {filtered.map((file) => {
            const isCurrent = currentMedia?.filename === file.filename;
            return (
              <MediaCard
                key={file.id || file.filename}
                file={file}
                title={file.name}
                subtitle={file.channel || file.size}
                isCurrent={isCurrent}
                isPlaying={isCurrent && isPlaying}
                icon={file.type === 'audio' ? Music : Video}
                draggable={canDragOut(file)}
                onDragStart={(e) => dragFileOut(e, file)}
                onContextMenu={(e) => openContextMenu(e, file)}
                onClick={() => handlePlay(file)}
              />
            );
          })}
        </div>
      )}

      {view === 'list' && filtered.length > 0 && (
        <div>
          {filtered.map((file, idx) => {
            const isCurrent = currentMedia?.filename === file.filename;
            const playing = isCurrent && isPlaying;

            return (
              <div
                key={file.id || file.filename}
                draggable={canDragOut(file)}
                onDragStart={(e) => dragFileOut(e, file)}
                onContextMenu={(e) => openContextMenu(e, file)}
                onClick={() => handlePlay(file)}
                className={`row ${isCurrent ? 'row--active' : ''}`}
              >
                <span className="row__index">
                  {playing ? <span style={{ color: 'var(--accent)' }}><PlayingIndicator /></span> : idx + 1}
                </span>
                <Thumb src={thumbUrl(file)} size="sm" icon={file.type === 'audio' ? Music : Video} />
                <div className="row__meta">
                  <div className={`row__title ${isCurrent ? 'row__title--active' : ''}`}>{file.name}</div>
                  <div className="row__sub">{file.channel || file.filename}</div>
                </div>
                <span className="row__detail">{formatDuration(file.duration)}</span>
                <span className="row__detail" style={{ width: 56, textAlign: 'right' }}>{file.size}</span>
                <button
                  onClick={(e) => { e.stopPropagation(); openContextMenu(e, file); }}
                  className="btn-icon-sm"
                  title="Más opciones"
                >
                  <MoreHorizontal size={15} />
                </button>
              </div>
            );
          })}
        </div>
      )}

      {infoModal && (
        <div className="modal-backdrop" onClick={() => setInfoModal(null)}>
          <div className="modal-panel" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <div style={{ display: 'flex', gap: 14, minWidth: 0, flex: 1 }}>
                <Thumb src={thumbUrl(infoModal)} size="lg" iconSize={20} />
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div className="modal-title">{infoModal.name}</div>
                  <div style={{ fontSize: 13, color: 'var(--text-secondary)', marginTop: 2 }}>
                    {infoModal.channel || 'Desconocido'}
                  </div>
                </div>
              </div>
              <button onClick={() => setInfoModal(null)} className="btn-icon-sm" title="Cerrar">
                <X size={14} />
              </button>
            </div>
            <div>
              {[
                ['Archivo', infoModal.filename],
                ['Tipo', infoModal.type === 'audio' ? 'Audio' : 'Vídeo'],
                ['Tamaño', infoModal.size],
                ['Duración', formatDuration(infoModal.duration)],
                ['Ruta', infoModal.local_path],
                ...(infoModal.metadata?.fingerprint_profile ? [
                  ['Variación', fingerprintProfileSummary(infoModal.metadata.fingerprint_profile)],
                ] : []),
                ...(infoModal.metadata?.fingerprint_profile?.intensity === 'quality'
                  || infoModal.metadata?.fingerprint_profile?.vocals_preserved ? [
                  ['Voz', fingerprintVocalSummary(infoModal.metadata.fingerprint_profile)],
                ] : []),
              ].map(([label, val]) => (
                <div key={label} className="info-row">
                  <span className="info-row__label">{label}</span>
                  <span className="info-row__value">{val || '—'}</span>
                </div>
              ))}
              {infoModal.metadata?.fingerprint_verification && (
                <>
                  <div className="info-row">
                    <span className="info-row__label">Fingerprint</span>
                    <span className="info-row__value">
                      {fingerprintSummary(infoModal.metadata.fingerprint_verification)}{' '}
                      Esto no garantiza que otros sistemas de reconocimiento no identifiquen el audio.
                    </span>
                  </div>
                  {fingerprintLocalComparisonSummary(infoModal.metadata.fingerprint_verification) && (
                    <div className="info-row">
                      <span className="info-row__label">Coincidencia local de bits</span>
                      <span className="info-row__value">
                        {fingerprintLocalComparisonSummary(infoModal.metadata.fingerprint_verification)}
                      </span>
                    </div>
                  )}
                  <div className="info-row">
                    <span className="info-row__label">Reconocimiento externo</span>
                    <span className="info-row__value">
                      {fingerprintRecognitionSummary(infoModal.metadata.fingerprint_verification)}
                    </span>
                  </div>
                </>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
