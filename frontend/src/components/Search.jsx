import React, { useState, useRef, useEffect, useCallback } from 'react';
import toast from 'react-hot-toast';
import { usePlayer } from '../context/PlayerContext';
import { copyText } from '../lib/desktop';
import { showContextMenu } from '../lib/contextMenu';
import { api, getApiError } from '../lib/api';
import { isCanceledRequest, streamMedia } from '../lib/stream';
import {
  Search as SearchIcon, Loader2, Music, Video, Play, Pause,
  ListMusic, FolderDown, ListPlus,
} from 'lucide-react';
import { Thumb, EmptyState, PlayingIndicator } from './ui';

const CURRENT_YEAR = new Date().getFullYear();
const CATEGORIES = [
  { label: 'Tendencias', query: `trending music ${CURRENT_YEAR}` },
  { label: 'Pop', query: `pop hits ${CURRENT_YEAR}` },
  { label: 'Reggaeton', query: `reggaeton ${CURRENT_YEAR} mix` },
  { label: 'Rock', query: 'classic rock greatest hits' },
  { label: 'Lo-Fi', query: 'lofi hip hop beats' },
  { label: 'Electrónica', query: 'electronic dance music' },
  { label: 'Jazz', query: 'jazz classics playlist' },
  { label: 'K-Pop', query: `kpop hits ${CURRENT_YEAR}` },
];

export default function Search() {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);
  const [activeChip, setActiveChip] = useState(null);
  const [streamingId, setStreamingId] = useState(null);
  const [playlistInfo, setPlaylistInfo] = useState(null);
  const [loadingPlaylist, setLoadingPlaylist] = useState(false);
  const [downloadingPlaylist, setDownloadingPlaylist] = useState(false);
  const inputRef = useRef(null);
  const { streamFromUrl, playNext, currentMedia, isPlaying, togglePlay } = usePlayer();

  // Cmd+F / menu "Buscar" focuses this input
  useEffect(() => {
    const focus = () => {
      inputRef.current?.focus();
      inputRef.current?.select();
    };
    window.addEventListener('ytu:focus-search', focus);
    return () => window.removeEventListener('ytu:focus-search', focus);
  }, []);

  const isPlaylistUrl = (url) => /[?&]list=/.test(url);

  const runSearch = useCallback(async (term, chipLabel = null) => {
    const q = term.trim();
    if (!q) return;
    setActiveChip(chipLabel);
    setPlaylistInfo(null);

    if (isPlaylistUrl(q)) {
      setLoadingPlaylist(true);
      setSearched(true);
      setResults([]);
      try {
        const { data } = await api.get('/playlist_info', { params: { url: q } });
        setPlaylistInfo(data);
      } catch {
        toast.error('Error al obtener la playlist');
      }
      setLoadingPlaylist(false);
      return;
    }

    setLoading(true);
    setSearched(true);
    try {
      const { data } = await api.get('/search', { params: { q } });
      setResults(data);
    } catch {
      toast.error('Error de búsqueda');
    }
    setLoading(false);
  }, []);

  const doSearch = () => runSearch(query);

  const exploreCategory = (cat) => {
    setQuery('');
    runSearch(cat.query, cat.label);
  };

  const downloadPlaylist = async (type) => {
    setDownloadingPlaylist(true);
    try {
      await api.post('/download_playlist', { url: query.trim(), type });
      toast.success(`Descargando playlist como ${type === 'audio' ? 'MP3' : 'MP4'}…`);
    } catch {
      toast.error('Error al iniciar descarga de playlist');
    }
    setDownloadingPlaylist(false);
  };

  const download = (item, type) => {
    toast.promise(
      api.post('/download_quick', { url: item.url, type, title: item.title, thumbnail: item.thumbnail }),
      {
        loading: type === 'audio' ? 'Añadiendo MP3…' : 'Añadiendo vídeo…',
        success: 'Añadido a Descargas',
        error: 'Error al descargar',
      }
    );
  };

  const streamPreview = useCallback(async (item) => {
    if (currentMedia?.filename === `stream_${item.id}`) {
      togglePlay();
      return;
    }
    setStreamingId(item.id);
    try {
      await streamFromUrl(item);
    } catch (error) {
      if (!isCanceledRequest(error)) toast.error(getApiError(error, 'No se pudo reproducir'));
    }
    setStreamingId((current) => current === item.id ? null : current);
  }, [currentMedia, togglePlay, streamFromUrl]);

  const handlePlayNext = useCallback((item) => {
    playNext(streamMedia(item));
    toast.success('Se reproducirá a continuación');
  }, [playNext]);

  const openContextMenu = (event, item) => {
    showContextMenu(event, [
      { label: 'Reproducir', onSelect: () => streamPreview(item) },
      { label: 'Reproducir a continuación', onSelect: () => handlePlayNext(item) },
      'separator',
      { label: 'Descargar como MP3', onSelect: () => download(item, 'audio') },
      { label: 'Descargar como MP4', onSelect: () => download(item, 'video') },
      'separator',
      { label: 'Copiar enlace', enabled: Boolean(item.url), onSelect: () => copyText(item.url) },
      {
        label: 'Abrir en YouTube',
        enabled: /^https?:\/\//i.test(item.url || ''),
        onSelect: () => window.open(item.url, '_blank', 'noopener'),
      },
    ]);
  };

  const clear = () => {
    setQuery('');
    setResults([]);
    setSearched(false);
    setActiveChip(null);
    setPlaylistInfo(null);
    inputRef.current?.focus();
  };

  const busy = loading || loadingPlaylist;

  return (
    <div className="page-shell">
      <div className="page-header">
        <h1 className="page-title">Buscar</h1>
      </div>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 14, marginBottom: 28 }}>
        <div className="input-search-wrap">
          <SearchIcon className="input-search-icon" size={15} />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') doSearch(); }}
            placeholder="Canciones, artistas o enlaces de YouTube"
            className="input-search input-search--hero"
            autoFocus
          />
          {query && (
            <button onClick={clear} className="input-search-clear" aria-label="Limpiar">
              ×
            </button>
          )}
        </div>

        <div className="chip-row">
          {CATEGORIES.map((cat) => (
            <button
              key={cat.label}
              onClick={() => exploreCategory(cat)}
              className={`chip ${activeChip === cat.label ? 'active' : ''}`}
            >
              {cat.label}
            </button>
          ))}
        </div>
      </div>

      {busy && (
        <EmptyState
          icon={null}
          title={loadingPlaylist ? 'Cargando playlist…' : 'Buscando…'}
          action={<Loader2 className="spinner" size={22} style={{ color: 'var(--accent)', order: -1 }} />}
        />
      )}

      {!busy && playlistInfo && (
        <div className="anim-fade">
          <div className="collection-hero">
            <Thumb src={playlistInfo.thumbnail} size="xl" icon={ListMusic} iconSize={30} />
            <div className="collection-hero__meta">
              <span className="collection-hero__eyebrow">Playlist de YouTube</span>
              <span className="collection-hero__title">{playlistInfo.title}</span>
              <span className="collection-hero__sub">
                {playlistInfo.channel ? `${playlistInfo.channel} · ` : ''}{playlistInfo.count} vídeos
              </span>
              <div className="collection-hero__actions">
                <button
                  onClick={() => downloadPlaylist('audio')}
                  disabled={downloadingPlaylist}
                  className="btn-accent"
                >
                  <FolderDown size={14} /> Descargar todo en MP3
                </button>
                <button
                  onClick={() => downloadPlaylist('video')}
                  disabled={downloadingPlaylist}
                  className="btn-secondary"
                >
                  MP4
                </button>
              </div>
            </div>
          </div>

          {playlistInfo.entries.map((item, i) => (
            <div key={item.id || i} className="row">
              <span className="row__index">{i + 1}</span>
              <Thumb src={item.thumbnail} size="sm" />
              <div className="row__meta">
                <div className="row__title">{item.title}</div>
                <div className="row__sub">{item.channel}</div>
              </div>
              {item.duration && <span className="row__detail">{item.duration}</span>}
              <div className="row__actions">
                <button onClick={() => download(item, 'audio')} className="btn-icon" title="Descargar MP3">
                  <Music size={14} />
                </button>
                <button onClick={() => download(item, 'video')} className="btn-icon" title="Descargar vídeo">
                  <Video size={14} />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {!busy && searched && !results.length && !playlistInfo && (
        <EmptyState
          icon={SearchIcon}
          title="Sin resultados"
          hint="Intenta con otros términos o pega un enlace de YouTube."
        />
      )}

      {!busy && results.length > 0 && (
        <div className="anim-fade">
          <div className="section-header">
            <h2 className="section-title">{activeChip || 'Resultados'}</h2>
            <span className="section-count">{results.length}</span>
          </div>
          {results.map((item) => {
            const isCurrentStream = currentMedia?.filename === `stream_${item.id}`;
            const isStreamPlaying = isCurrentStream && isPlaying;
            const isLoading = streamingId === item.id;

            return (
              <div
                key={item.id}
                className={`row ${isCurrentStream ? 'row--active' : ''}`}
                onClick={() => streamPreview(item)}
                onContextMenu={(e) => openContextMenu(e, item)}
                title="Reproducir sin descargar"
              >
                <Thumb src={item.thumbnail} size="lg">
                  {item.duration && <span className="thumb-duration">{item.duration}</span>}
                  {isStreamPlaying && (
                    <div className="thumb-playing"><PlayingIndicator /></div>
                  )}
                </Thumb>
                <div className="row__meta">
                  <div className={`row__title ${isCurrentStream ? 'row__title--active' : ''}`}>{item.title}</div>
                  <div className="row__sub">{item.channel}{item.views ? ` · ${item.views}` : ''}</div>
                </div>
                <div className="row__actions">
                  <button
                    onClick={(e) => { e.stopPropagation(); streamPreview(item); }}
                    className="btn-icon"
                    title={isStreamPlaying ? 'Pausar' : 'Reproducir (stream)'}
                    disabled={isLoading}
                  >
                    {isLoading ? (
                      <Loader2 size={14} className="spinner" />
                    ) : isStreamPlaying ? (
                      <Pause size={14} fill="currentColor" />
                    ) : (
                      <Play size={14} fill="currentColor" />
                    )}
                  </button>
                  <button
                    onClick={(e) => { e.stopPropagation(); handlePlayNext(item); }}
                    className="btn-icon"
                    title="Reproducir a continuación"
                  >
                    <ListPlus size={14} />
                  </button>
                  <button
                    onClick={(e) => { e.stopPropagation(); download(item, 'audio'); }}
                    className="btn-icon"
                    title="Descargar MP3"
                  >
                    <Music size={14} />
                  </button>
                  <button
                    onClick={(e) => { e.stopPropagation(); download(item, 'video'); }}
                    className="btn-icon"
                    title="Descargar vídeo"
                  >
                    <Video size={14} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {!busy && !searched && (
        <EmptyState
          icon={SearchIcon}
          title="¿Qué quieres escuchar?"
          hint="Escribe y pulsa Enter, pega un enlace de YouTube, o explora un género. Toca un resultado para escucharlo al instante, sin descargar."
        />
      )}
    </div>
  );
}
