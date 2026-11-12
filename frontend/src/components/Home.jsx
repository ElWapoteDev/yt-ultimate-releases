import React, { useState, useEffect, useMemo } from 'react';
import { usePlayer } from '../context/PlayerContext';
import { greeting } from '../lib/format';
import { api } from '../lib/api';
import { Search, ArrowRight, Music2 } from 'lucide-react';
import { MediaCard, EmptyState } from './ui';
import { canDragOut, dragFileOut } from '../lib/desktop';
import { showContextMenu } from '../lib/contextMenu';
import { trackChipmunkItems, trackFileItems, trackPlaybackItems } from '../lib/trackMenu';

export default function Home({ changeTab, active }) {
  const [files, setFiles] = useState(null); // null = loading
  const {
    playMedia, playNext, playLater, currentMedia, isPlaying, togglePlay, history,
  } = usePlayer();

  useEffect(() => {
    if (!active) return;
    api.get('/library')
      .then((r) => setFiles(r.data))
      .catch(() => setFiles([]));
  }, [active]);

  const recent = useMemo(() => (files || []).slice(0, 12), [files]);

  const recentlyPlayed = useMemo(() => {
    const seen = new Set();
    const out = [];
    for (let i = history.length - 1; i >= 0 && out.length < 6; i -= 1) {
      const item = history[i];
      if (seen.has(item.filename)) continue;
      seen.add(item.filename);
      out.push(item);
    }
    return out;
  }, [history]);

  const handlePlay = (file) => {
    if (currentMedia?.filename === file.filename) togglePlay();
    else playMedia(file);
  };

  const openContextMenu = (event, file) => {
    showContextMenu(event, [
      ...trackPlaybackItems(file, { play: handlePlay, playNext, playLater }),
      'separator',
      ...trackChipmunkItems(file),
      'separator',
      ...trackFileItems(file),
    ]);
  };

  const loaded = files !== null;
  const isEmpty = loaded && files.length === 0;

  return (
    <div className="page-shell">
      <div className="home-hero">
        <h1 className="home-hero__greeting">{greeting()}</h1>
        <div className="home-hero__sub">Tu música, sin distracciones.</div>
      </div>

      {isEmpty && (
        <EmptyState
          icon={Music2}
          title="Empieza tu biblioteca"
          hint="Busca cualquier canción, artista o enlace de YouTube y descárgala para escucharla aquí, incluso sin conexión."
          action={(
            <button onClick={() => changeTab('search')} className="btn-accent">
              <Search size={14} /> Buscar música
            </button>
          )}
        />
      )}

      {recentlyPlayed.length > 0 && (
        <div className="section">
          <div className="section-header">
            <h2 className="section-title">Escuchado recientemente</h2>
          </div>
          <div className="media-grid">
            {recentlyPlayed.map((file) => (
              <MediaCard
                key={file.filename}
                file={file}
                title={file.name}
                subtitle={file.channel}
                isCurrent={currentMedia?.filename === file.filename}
                isPlaying={currentMedia?.filename === file.filename && isPlaying}
                onClick={() => handlePlay(file)}
                onContextMenu={(event) => openContextMenu(event, file)}
                draggable={canDragOut(file)}
                onDragStart={(event) => dragFileOut(event, file)}
              />
            ))}
          </div>
        </div>
      )}

      {recent.length > 0 && (
        <div className="section">
          <div className="section-header">
            <h2 className="section-title">Añadido recientemente</h2>
            <button onClick={() => changeTab('library')} className="section-action">
              Ver todo <ArrowRight size={13} />
            </button>
          </div>
          <div className="media-grid">
            {recent.map((file) => (
              <MediaCard
                key={file.filename}
                file={file}
                title={file.name}
                subtitle={file.channel || file.size}
                isCurrent={currentMedia?.filename === file.filename}
                isPlaying={currentMedia?.filename === file.filename && isPlaying}
                onClick={() => handlePlay(file)}
                onContextMenu={(event) => openContextMenu(event, file)}
                draggable={canDragOut(file)}
                onDragStart={(event) => dragFileOut(event, file)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
