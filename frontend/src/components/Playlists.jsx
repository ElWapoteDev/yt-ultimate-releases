import React, { useEffect, useMemo, useState, useCallback } from 'react';
import { api } from '../lib/api';
import toast from 'react-hot-toast';
import { usePlayer } from '../context/PlayerContext';
import { thumbUrl } from '../lib/format';
import { Plus, Trash2, X, Music, ListMusic, Play, Shuffle } from 'lucide-react';
import { Thumb, EmptyState, PlayingIndicator } from './ui';
import { canDragOut, dragFileOut } from '../lib/desktop';
import { showContextMenu } from '../lib/contextMenu';
import { trackChipmunkItems, trackFileItems, trackPlaybackItems } from '../lib/trackMenu';

function PlaylistCover({ tracks, size = 'xs', iconSize = 12 }) {
  const covers = useMemo(() => {
    const seen = new Set();
    const out = [];
    for (const t of tracks) {
      const src = thumbUrl(t);
      if (src && !seen.has(src)) {
        seen.add(src);
        out.push(src);
      }
      if (out.length === 4) break;
    }
    return out;
  }, [tracks]);

  if (covers.length >= 4) {
    return (
      <div className={`thumb thumb--${size}`}>
        <div className="mosaic">
          {covers.map((src) => <img key={src} src={src} alt="" loading="lazy" />)}
        </div>
      </div>
    );
  }
  return <Thumb src={covers[0] || ''} size={size} icon={Music} iconSize={iconSize} />;
}

export default function Playlists({ active }) {
  const [playlists, setPlaylists] = useState({});
  const [lib, setLib] = useState([]);
  const [sel, setSel] = useState(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const {
    playMedia, playAll, playNext, playLater, currentMedia, isPlaying, togglePlay,
  } = usePlayer();

  const load = useCallback(async () => {
    try {
      const [a, b] = await Promise.all([api.get('/playlists'), api.get('/library')]);
      setPlaylists(a.data);
      setLib(b.data);
    } catch {
      // backend not ready
    }
  }, []);

  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    (async () => {
      try {
        const [a, b] = await Promise.all([api.get('/playlists'), api.get('/library')]);
        if (!alive) return;
        setPlaylists(a.data);
        setLib(b.data);
      } catch {
        // backend not ready
      }
    })();
    return () => { alive = false; };
  }, [active]);

  const create = async () => {
    if (!name.trim()) return;
    try {
      await api.post('/playlists', { name: name.trim() });
      toast.success('Playlist creada');
      setSel(name.trim());
      setName('');
      setCreating(false);
      load();
    } catch {
      toast.error('Ya existe una playlist con ese nombre');
    }
  };

  const del = async (n) => {
    if (!window.confirm(`¿Eliminar la playlist "${n}"?`)) return;
    await api.delete(`/playlists/${encodeURIComponent(n)}`);
    if (sel === n) setSel(null);
    load();
  };

  const removeTrack = async (pl, fn) => {
    await api.post('/playlists/remove', { playlist_name: pl, filename: fn });
    load();
  };

  const getFile = useCallback((fn) => lib.find((f) => f.filename === fn), [lib]);

  const handlePlay = (file) => {
    if (currentMedia?.filename === file.filename) togglePlay();
    else playMedia(file);
  };

  const openTrackMenu = (event, playlist, file) => {
    showContextMenu(event, [
      ...trackPlaybackItems(file, { play: handlePlay, playNext, playLater }),
      'separator',
      { label: 'Quitar de la playlist', onSelect: () => removeTrack(playlist, file.filename) },
      'separator',
      ...trackChipmunkItems(file),
      'separator',
      ...trackFileItems(file),
    ]);
  };

  const names = Object.keys(playlists);
  const selTracks = useMemo(
    () => (sel ? (playlists[sel] || []).map(getFile).filter(Boolean) : []),
    [sel, playlists, getFile],
  );

  return (
    <div className="split-page">
      <div className="split-page__top">
        <div className="toolbar" style={{ paddingBottom: 10 }}>
          <div className="toolbar__left">
            <h1 className="page-title">Playlists</h1>
            <span className="page-count">{names.length}</span>
          </div>
          <div className="toolbar__right">
            <button onClick={() => setCreating((c) => !c)} className="btn-secondary">
              <Plus size={13} /> Nueva
            </button>
          </div>
        </div>
        {creating && (
          <div className="anim-fade" style={{ display: 'flex', gap: 8, maxWidth: 420, paddingBottom: 10 }}>
            <div className="input-search-wrap">
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') create();
                  if (e.key === 'Escape') { setCreating(false); setName(''); }
                }}
                placeholder="Nombre de la playlist…"
                autoFocus
                className="input-search"
                style={{ paddingLeft: 12 }}
              />
            </div>
            <button onClick={create} className="btn-accent" disabled={!name.trim()}>Crear</button>
            <button onClick={() => { setCreating(false); setName(''); }} className="btn-icon" title="Cancelar">
              <X size={14} />
            </button>
          </div>
        )}
      </div>

      <div className="split-page__body">
        <aside className="playlist-rail">
          {!names.length && (
            <EmptyState icon={ListMusic} title="Sin playlists" hint="Crea la primera con «Nueva»." compact />
          )}
          {names.map((n) => {
            const trackFiles = (playlists[n] || []).map(getFile).filter(Boolean);
            const count = playlists[n]?.length || 0;
            const isSel = sel === n;

            return (
              <div
                key={n}
                onClick={() => setSel(n)}
                className={`row ${isSel ? 'row--active' : ''}`}
              >
                <PlaylistCover tracks={trackFiles} />
                <div className="row__meta">
                  <div className={`row__title ${isSel ? 'row__title--active' : ''}`}>{n}</div>
                  <div className="row__sub">{count} {count === 1 ? 'canción' : 'canciones'}</div>
                </div>
                <div className="row__actions">
                  <button
                    onClick={(e) => { e.stopPropagation(); del(n); }}
                    className="btn-icon-sm btn-icon--danger"
                    title="Eliminar playlist"
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
              </div>
            );
          })}
        </aside>

        <div className="split-page__detail">
          {!sel ? (
            <EmptyState
              icon={ListMusic}
              title="Selecciona una playlist"
              hint="Añade canciones desde Canciones con clic derecho → «Añadir a playlist»."
            />
          ) : (
            <div className="anim-fade" key={sel}>
              <div className="collection-hero">
                <div style={{ width: 148, height: 148, flexShrink: 0 }}>
                  <PlaylistCover tracks={selTracks} size="xl" iconSize={30} />
                </div>
                <div className="collection-hero__meta">
                  <span className="collection-hero__eyebrow">Playlist</span>
                  <span className="collection-hero__title">{sel}</span>
                  <span className="collection-hero__sub">
                    {selTracks.length} {selTracks.length === 1 ? 'canción' : 'canciones'}
                  </span>
                  {selTracks.length > 0 && (
                    <div className="collection-hero__actions">
                      <button onClick={() => playAll(selTracks, 0)} className="btn-accent">
                        <Play size={13} fill="currentColor" /> Reproducir
                      </button>
                      <button
                        onClick={() => playAll(selTracks, 0, { shuffled: true })}
                        className="btn-secondary"
                        title="Reproducir en orden aleatorio"
                      >
                        <Shuffle size={13} />
                      </button>
                    </div>
                  )}
                </div>
              </div>

              {!selTracks.length ? (
                <EmptyState
                  icon={Music}
                  title="Playlist vacía"
                  hint="Añade canciones desde Canciones con clic derecho."
                  compact
                />
              ) : (
                selTracks.map((f, idx) => {
                  const isCurrent = currentMedia?.filename === f.filename;
                  const playing = isCurrent && isPlaying;

                  return (
                    <div
                      key={`${f.filename}-${idx}`}
                      onClick={() => handlePlay(f)}
                      onContextMenu={(event) => openTrackMenu(event, sel, f)}
                      draggable={canDragOut(f)}
                      onDragStart={(event) => dragFileOut(event, f)}
                      className={`row ${isCurrent ? 'row--active' : ''}`}
                    >
                      <span className="row__index">
                        {playing ? <span style={{ color: 'var(--accent)' }}><PlayingIndicator /></span> : idx + 1}
                      </span>
                      <Thumb src={thumbUrl(f)} size="xs" iconSize={12} />
                      <div className="row__meta">
                        <div className={`row__title ${isCurrent ? 'row__title--active' : ''}`}>{f.name}</div>
                        <div className="row__sub">{f.channel || f.size}</div>
                      </div>
                      <div className="row__actions">
                        <button
                          onClick={(e) => { e.stopPropagation(); removeTrack(sel, f.filename); }}
                          className="btn-icon-sm"
                          title="Quitar de la playlist"
                        >
                          <X size={13} />
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
