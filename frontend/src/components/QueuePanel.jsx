import React, { useCallback, useState } from 'react';
import { usePlayer } from '../context/PlayerContext';
import { showContextMenu } from '../lib/contextMenu';
import { thumbUrl } from '../lib/format';
import { Music, ListMusic, X, Radio } from 'lucide-react';
import { Thumb, EmptyState, PlayingIndicator } from './ui';

export default function QueuePanel({ onClose }) {
  const {
    currentMedia, isPlaying, queue, history,
    removeFromQueue, clearQueue, moveInQueue, playMedia, playQueueIndex,
  } = usePlayer();
  const [draggingIndex, setDraggingIndex] = useState(null);

  const playFromQueue = useCallback((index) => {
    playQueueIndex(index);
  }, [playQueueIndex]);

  const dropAt = useCallback((event, targetIndex) => {
    event.preventDefault();
    if (draggingIndex === null || draggingIndex === targetIndex) return;
    moveInQueue(draggingIndex, targetIndex);
    setDraggingIndex(null);
  }, [draggingIndex, moveInQueue]);

  const openContextMenu = (event, index) => {
    showContextMenu(event, [
      { label: 'Reproducir ahora', onSelect: () => playFromQueue(index) },
      { label: 'Mover al inicio', enabled: index > 0, onSelect: () => moveInQueue(index, 0) },
      'separator',
      { label: 'Quitar de la cola', onSelect: () => removeFromQueue(index) },
    ]);
  };

  return (
    <>
      <div className="queue-backdrop" onClick={onClose} />
      <aside className="queue-panel" role="dialog" aria-label="Cola de reproducción">
        <div className="queue-panel__header">
          <span className="queue-panel__title">Cola</span>
          <div style={{ display: 'flex', gap: 4, alignItems: 'center' }}>
            {queue.length > 0 && (
              <button onClick={clearQueue} className="btn-ghost" style={{ fontSize: 12, padding: '4px 8px' }}>
                Vaciar
              </button>
            )}
            <button onClick={onClose} className="btn-icon-sm" title="Cerrar (Esc)">
              <X size={15} />
            </button>
          </div>
        </div>

        <div className="queue-panel__scroll">
          {currentMedia && (
            <>
              <div className="queue-label">Reproduciendo</div>
              <div className="row row--active" style={{ cursor: 'default' }}>
                <Thumb src={thumbUrl(currentMedia)} size="sm" iconSize={14}>
                  {isPlaying && (
                    <div className="thumb-playing"><PlayingIndicator /></div>
                  )}
                </Thumb>
                <div className="row__meta">
                  <div className="row__title row__title--active" style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
                    {currentMedia._isStream && <Radio size={11} style={{ color: 'var(--accent)', flexShrink: 0 }} />}
                    <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{currentMedia.name}</span>
                  </div>
                  <div className="row__sub">{currentMedia.channel || currentMedia.size}</div>
                </div>
              </div>
            </>
          )}

          {queue.length > 0 && (
            <>
              <div className="queue-label">A continuación</div>
              {queue.map((item, idx) => (
                <div
                  key={`${item.id || item.filename}-${idx}`}
                  className={`row ${draggingIndex === idx ? 'row--dragging' : ''}`}
                  onClick={() => playFromQueue(idx)}
                  onContextMenu={(e) => openContextMenu(e, idx)}
                  draggable
                  onDragStart={(event) => {
                    event.dataTransfer.effectAllowed = 'move';
                    event.dataTransfer.setData('text/plain', String(idx));
                    setDraggingIndex(idx);
                  }}
                  onDragOver={(event) => {
                    event.preventDefault();
                    event.dataTransfer.dropEffect = 'move';
                  }}
                  onDrop={(event) => dropAt(event, idx)}
                  onDragEnd={() => setDraggingIndex(null)}
                  title="Reproducir ahora"
                >
                  <Thumb src={thumbUrl(item)} size="xs" iconSize={12} />
                  <div className="row__meta">
                    <div className="row__title" style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
                      {item._isStream && <Radio size={10} style={{ color: 'var(--text-tertiary)', flexShrink: 0 }} />}
                      <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{item.name}</span>
                    </div>
                    <div className="row__sub">{item.channel}</div>
                  </div>
                  <div className="row__actions">
                    <button
                      onClick={(e) => { e.stopPropagation(); removeFromQueue(idx); }}
                      className="btn-icon-sm"
                      title="Quitar de la cola"
                    >
                      <X size={13} />
                    </button>
                  </div>
                </div>
              ))}
            </>
          )}

          {history.length > 0 && (
            <>
              <div className="queue-label">Historial</div>
              {[...history].reverse().slice(0, 12).map((item, idx) => (
                <div
                  key={`hist-${item.id || item.filename}-${idx}`}
                  onClick={() => playMedia(item)}
                  className="row queue-row-history"
                  title="Volver a reproducir"
                >
                  <Thumb src={thumbUrl(item)} size="xs" iconSize={12} />
                  <div className="row__meta">
                    <div className="row__title">{item.name}</div>
                    <div className="row__sub">{item.channel}</div>
                  </div>
                </div>
              ))}
            </>
          )}

          {!currentMedia && queue.length === 0 && history.length === 0 && (
            <EmptyState
              icon={ListMusic}
              title="Cola vacía"
              hint="Reproduce algo o añade canciones con clic derecho → «Reproducir a continuación»."
              compact
            />
          )}
        </div>
      </aside>
    </>
  );
}
