import React, { useEffect, useRef, useState, useCallback } from 'react';
import { usePlayer } from '../context/PlayerContext';
import { getDesktopBridge, revealFile } from '../lib/desktop';
import { showContextMenu } from '../lib/contextMenu';
import { trackChipmunkItems } from '../lib/trackMenu';
import { fileManagerName } from '../lib/platform';
import { formatTime, thumbUrl } from '../lib/format';
import {
  ListMusic,
  LoaderCircle,
  Music,
  MoreHorizontal,
  Pause,
  Play,
  PictureInPicture,
  Repeat,
  Repeat1,
  RotateCcw,
  Shuffle,
  SkipBack,
  SkipForward,
  Timer,
  Video,
  Volume1,
  Volume2,
  VolumeX,
  Radio,
} from 'lucide-react';

export default function PlayerBar({ queueOpen, onToggleQueue }) {
  const desktop = getDesktopBridge();
  const {
    currentMedia,
    isPlaying,
    isBuffering,
    isRecovering,
    playbackError,
    retryPlayback,
    togglePlay,
    progress,
    duration,
    seek,
    volume,
    setVolume,
    toggleMute,
    playbackRate,
    playbackRates,
    setPlaybackRate,
    shuffle,
    repeat,
    queue,
    isStreaming,
    skipNext,
    skipPrev,
    seekBy,
    toggleShuffle,
    cycleRepeat,
    setSleepTimer,
    openVideo,
    togglePictureInPicture,
  } = usePlayer();

  const trackRef = useRef(null);
  const [scrub, setScrub] = useState(null); // 0..1 while dragging
  const [optionsOpen, setOptionsOpen] = useState(false);
  const optionsRef = useRef(null);

  useEffect(() => {
    if (!optionsOpen) return undefined;
    const close = (event) => {
      if (!optionsRef.current?.contains(event.target)) setOptionsOpen(false);
    };
    window.addEventListener('pointerdown', close);
    return () => window.removeEventListener('pointerdown', close);
  }, [optionsOpen]);

  const ratioFromEvent = useCallback((e) => {
    const rect = trackRef.current.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
  }, []);

  const onPointerDown = useCallback((e) => {
    if (!duration) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    setScrub(ratioFromEvent(e));
  }, [duration, ratioFromEvent]);

  const onPointerMove = useCallback((e) => {
    if (scrub === null) return;
    setScrub(ratioFromEvent(e));
  }, [scrub, ratioFromEvent]);

  const onPointerUp = useCallback((e) => {
    if (scrub === null) return;
    seek(ratioFromEvent(e) * duration);
    setScrub(null);
  }, [scrub, duration, seek, ratioFromEvent]);

  const hasMedia = Boolean(currentMedia);
  const thumb = hasMedia ? thumbUrl(currentMedia) : '';
  const VolumeIcon = volume === 0 ? VolumeX : volume < 0.55 ? Volume1 : Volume2;
  const shownProgress = scrub !== null ? scrub * duration : progress;
  const pct = duration ? (shownProgress / duration) * 100 : 0;

  const RepeatIcon = repeat === 'one' ? Repeat1 : Repeat;
  const repeatTitle = repeat === 'off' ? 'Repetir' : repeat === 'all' ? 'Repetir todo' : 'Repetir uno';

  const onProgressKeyDown = (event) => {
    if (!hasMedia) return;
    if (event.key === 'ArrowLeft') {
      event.preventDefault();
      seekBy(event.shiftKey ? -30 : -5);
    } else if (event.key === 'ArrowRight') {
      event.preventDefault();
      seekBy(event.shiftKey ? 30 : 5);
    } else if (event.key === 'Home') {
      event.preventDefault();
      seek(0);
    } else if (event.key === 'End') {
      event.preventDefault();
      seek(duration);
    }
  };

  const openContextMenu = (event) => {
    showContextMenu(event, [
      { label: isPlaying ? 'Pausar' : 'Reproducir', enabled: hasMedia, onSelect: togglePlay },
      { label: 'Retroceder 10 segundos', enabled: hasMedia, onSelect: () => seekBy(-10) },
      { label: 'Adelantar 10 segundos', enabled: hasMedia, onSelect: () => seekBy(10) },
      'separator',
      ...(hasMedia && !isStreaming ? trackChipmunkItems(currentMedia) : []),
      'separator',
      {
        label: `Mostrar en ${fileManagerName}`,
        visible: Boolean(desktop),
        enabled: Boolean(currentMedia?.local_path),
        onSelect: () => void revealFile(currentMedia.local_path),
      },
    ]);
  };

  return (
    <div className="player-bar" onContextMenu={openContextMenu}>
      <div className="player-now">
        <div className={`player-artwork ${!thumb ? 'player-artwork--empty' : ''}`}>
          {thumb ? <img src={thumb} alt="" /> : <Music size={18} strokeWidth={1.6} />}
        </div>
        <div className="player-info">
          {hasMedia ? (
            <>
              <div className="player-song">
                {isStreaming && <Radio size={12} style={{ color: 'var(--accent)', flexShrink: 0 }} />}
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{currentMedia.name}</span>
                {isBuffering && <LoaderCircle size={12} className="spinner" aria-label="Cargando" />}
              </div>
              <div className="player-artist" role={playbackError ? 'alert' : 'status'} title={playbackError || undefined}>
                {playbackError || (isRecovering ? 'Reconectando stream…' : currentMedia.channel) || (currentMedia.type === 'audio' ? 'Audio' : 'Vídeo')}
              </div>
            </>
          ) : (
            <div className="player-song player-song--empty">Nada en reproducción</div>
          )}
        </div>
      </div>

      <div className="player-center">
        <div className="player-controls">
          <button
            onClick={toggleShuffle}
            className={`player-btn player-btn-small ${shuffle ? 'player-btn-active' : ''}`}
            title={shuffle ? 'Aleatorio activado' : 'Aleatorio'}
          >
            <Shuffle size={14} />
          </button>
          <button onClick={skipPrev} className="player-btn" title="Anterior" disabled={!hasMedia}>
            <SkipBack size={17} fill="currentColor" />
          </button>
          <button
            onClick={togglePlay}
            className="player-btn player-btn-play"
            title={isPlaying ? 'Pausar' : 'Reproducir'}
            disabled={!hasMedia}
          >
            {isBuffering
              ? <LoaderCircle size={16} className="spinner" />
              : isPlaying
              ? <Pause size={16} fill="currentColor" />
              : <Play size={16} fill="currentColor" style={{ marginLeft: 2 }} />}
          </button>
          <button onClick={skipNext} className="player-btn" title="Siguiente" disabled={queue.length === 0}>
            <SkipForward size={17} fill="currentColor" />
          </button>
          <button
            onClick={cycleRepeat}
            className={`player-btn player-btn-small ${repeat !== 'off' ? 'player-btn-active' : ''}`}
            title={repeatTitle}
          >
            <RepeatIcon size={14} />
          </button>
        </div>

        <div className="player-progress-section">
          <span className="player-time">{hasMedia ? formatTime(shownProgress) : '--:--'}</span>
          <div
            ref={trackRef}
            className={`player-progress-track ${scrub !== null ? 'scrubbing' : ''}`}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={() => setScrub(null)}
            onKeyDown={onProgressKeyDown}
            role="slider"
            tabIndex={hasMedia ? 0 : -1}
            aria-label="Posición de reproducción"
            aria-valuemin={0}
            aria-valuemax={Math.round(duration || 0)}
            aria-valuenow={Math.round(shownProgress || 0)}
            aria-valuetext={`${formatTime(shownProgress)} de ${formatTime(duration)}`}
          >
            <div className="player-progress-fill" style={{ width: `${pct}%` }} />
          </div>
          <span className="player-time">{hasMedia ? formatTime(duration) : '--:--'}</span>
        </div>
      </div>

      <div className="player-side">
        {playbackError && (
          <button
            type="button"
            onClick={retryPlayback}
            className="player-btn player-btn-small"
            title="Reintentar reproducción"
            aria-label="Reintentar reproducción"
            disabled={isRecovering}
          >
            <RotateCcw size={15} />
          </button>
        )}
        <div className="player-volume">
          <button
            className="player-btn player-btn-small"
            onClick={toggleMute}
            title={volume === 0 ? 'Activar sonido' : 'Silenciar'}
          >
            <VolumeIcon size={15} />
          </button>
          <input
            type="range"
            className="slider-volume"
            min="0"
            max="1"
            step="0.01"
            value={volume}
            style={{ '--vol': `${volume * 100}%` }}
            onChange={(e) => setVolume(Number(e.target.value))}
            aria-label="Volumen"
          />
        </div>
        {currentMedia?.type === 'video' && (
          <button
            type="button"
            onClick={openVideo}
            className="player-btn player-btn-small"
            title="Mostrar vídeo"
          >
            <Video size={15} />
          </button>
        )}
        <div className="player-options" ref={optionsRef}>
          <button
            type="button"
            onClick={() => setOptionsOpen((open) => !open)}
            className={`player-btn player-btn-small ${optionsOpen ? 'player-btn-active' : ''}`}
            title="Más controles"
            aria-expanded={optionsOpen}
          >
            <MoreHorizontal size={16} />
          </button>
          {optionsOpen && (
            <div className="player-options__popover">
              <div className="player-options__label">Velocidad</div>
              <div className="player-options__rates">
                {playbackRates.map((rate) => (
                  <button
                    type="button"
                    key={rate}
                    className={playbackRate === rate ? 'active' : ''}
                    onClick={() => setPlaybackRate(rate)}
                  >
                    {rate}×
                  </button>
                ))}
              </div>
              <div className="player-options__divider" />
              <button type="button" onClick={() => seekBy(-10)}>
                <RotateCcw size={14} /> Retroceder 10 s
              </button>
              {currentMedia?.type === 'video' && (
                <button type="button" onClick={togglePictureInPicture}>
                  <PictureInPicture size={14} /> Picture in Picture
                </button>
              )}
              <div className="player-options__divider" />
              <div className="player-options__label"><Timer size={13} /> Temporizador</div>
              <div className="player-options__rates">
                {[0, 15, 30, 60].map((minutes) => (
                  <button
                    type="button"
                    key={minutes}
                    onClick={() => setSleepTimer(minutes)}
                  >
                    {minutes ? `${minutes} min` : 'No'}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
        <button
          onClick={onToggleQueue}
          className={`player-btn player-btn-small queue-button ${queueOpen ? 'player-btn-active' : ''}`}
          title="Cola de reproducción"
        >
          <ListMusic size={15} />
          {queue.length > 0 && !queueOpen && <span className="queue-dot" />}
        </button>
      </div>
    </div>
  );
}
