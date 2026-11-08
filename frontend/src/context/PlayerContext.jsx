/* eslint-disable react-refresh/only-export-components */
import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { api, getApiError } from '../lib/api';
import { resourceUrl } from '../lib/runtime';
import {
  PlaybackRequests, STREAM_RECOVERY_LIMIT, isCanceledRequest,
  retryStreamPath, streamMedia, waitForRecovery,
} from '../lib/stream';

const PlayerContext = createContext(null);
const SESSION_KEY = 'ytu:player-session:v2';
const PREFERENCE_KEY = 'ytu:player-preferences:v2';
const HISTORY_LIMIT = 100;
const PLAYBACK_RATES = Object.freeze([0.75, 1, 1.25, 1.5, 2]);

function readStored(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : JSON.parse(raw);
  } catch {
    return fallback;
  }
}

function store(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Private browsing or a full storage quota should not break playback.
  }
}

function mediaIdentity(media) {
  return media?.id || media?.filename || media?._ytUrl || media?.path || '';
}

function sourceFor(media) {
  if (!media) return '';
  return resourceUrl(media._streamUrl || media.path || '');
}

function artworkFor(media) {
  return resourceUrl(media?.thumbnail || '');
}

export function PlayerProvider({ children }) {
  const savedPreferences = useMemo(
    () => readStored(PREFERENCE_KEY, {}),
    [],
  );
  const savedSession = useMemo(
    () => readStored(SESSION_KEY, {}),
    [],
  );

  const mediaRef = useRef(null);
  const stateRef = useRef({});
  const requestsRef = useRef(new PlaybackRequests());
  const sourceRevisionRef = useRef(0);
  const recoveryAttemptsRef = useRef(0);
  const wantsPlaybackRef = useRef(false);
  const resumeTimeRef = useRef(Number(savedSession.progress) || 0);
  const previousVolumeRef = useRef(
    typeof savedPreferences.volume === 'number' && savedPreferences.volume > 0
      ? savedPreferences.volume
      : 0.7,
  );

  const [currentMedia, setCurrentMedia] = useState(savedSession.currentMedia || null);
  const [queue, setQueue] = useState(Array.isArray(savedSession.queue) ? savedSession.queue : []);
  const [history, setHistory] = useState(Array.isArray(savedSession.history) ? savedSession.history : []);
  const [isPlaying, setIsPlaying] = useState(false);
  const [isBuffering, setIsBuffering] = useState(false);
  const [playbackError, setPlaybackError] = useState('');
  const [isRecovering, setIsRecovering] = useState(false);
  const [progress, setProgress] = useState(Number(savedSession.progress) || 0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolumeState] = useState(() => {
    const value = Number(savedPreferences.volume);
    return Number.isFinite(value) && value >= 0 && value <= 1 ? value : 0.7;
  });
  const [playbackRate, setPlaybackRateState] = useState(() => (
    PLAYBACK_RATES.includes(savedPreferences.playbackRate)
      ? savedPreferences.playbackRate
      : 1
  ));
  const [shuffle, setShuffle] = useState(savedPreferences.shuffle === true);
  const [repeat, setRepeat] = useState(
    ['off', 'all', 'one'].includes(savedPreferences.repeat)
      ? savedPreferences.repeat
      : 'off',
  );
  const [isStreaming, setIsStreaming] = useState(Boolean(savedSession.currentMedia?._isStream));
  const [videoOpen, setVideoOpen] = useState(false);
  const [sleepEndsAt, setSleepEndsAt] = useState(null);

  useEffect(() => {
    stateRef.current = {
      currentMedia,
      queue,
      history,
      repeat,
      shuffle,
      progress,
    };
  }, [currentMedia, history, progress, queue, repeat, shuffle]);

  const pausePlayback = useCallback(() => {
    wantsPlaybackRef.current = false;
    mediaRef.current?.pause();
    setIsPlaying(false);
  }, []);

  const playElement = useCallback(async () => {
    const element = mediaRef.current;
    if (!element || !element.getAttribute('src')) return;
    wantsPlaybackRef.current = true;
    const revision = sourceRevisionRef.current;
    setPlaybackError('');
    try {
      await element.play();
    } catch (error) {
      if (revision !== sourceRevisionRef.current || error?.name === 'AbortError') return;
      if (error?.name === 'NotAllowedError') {
        wantsPlaybackRef.current = false;
        setIsBuffering(false);
        setPlaybackError('Pulsa Reproducir para iniciar el audio');
      } else if (!element.error) {
        setIsBuffering(false);
        setPlaybackError('No se pudo iniciar la reproducción. Vuelve a intentarlo');
      }
    }
  }, []);

  const loadTrack = useCallback((media, options = {}) => {
    const element = mediaRef.current;
    if (!element || !media) return;
    const { pushHistory = true, autoplay = true, startAt = 0, recovering = false } = options;
    const previous = stateRef.current.currentMedia;

    requestsRef.current.cancel();
    sourceRevisionRef.current += 1;
    if (!recovering) recoveryAttemptsRef.current = 0;
    element.pause();
    wantsPlaybackRef.current = autoplay;
    if (
      pushHistory
      && previous
      && mediaIdentity(previous) !== mediaIdentity(media)
    ) {
      setHistory((items) => [...items, previous].slice(-HISTORY_LIMIT));
    }

    stateRef.current.currentMedia = media;
    setCurrentMedia(media);
    setIsRecovering(false);
    setIsStreaming(Boolean(media._streamUrl || media._isStream));
    setProgress(Math.max(0, Number(startAt) || 0));
    setDuration(0);
    setIsBuffering(true);
    setPlaybackError('');
    resumeTimeRef.current = Math.max(0, Number(startAt) || 0);
    element.src = sourceFor(media);
    element.load();
    if (autoplay) void playElement();
  }, [playElement]);

  const recoverStream = useCallback(async ({ manual = false } = {}) => {
    const media = stateRef.current.currentMedia;
    const element = mediaRef.current;
    if (!media?._ytUrl || !element || requestsRef.current.pending) return;
    if (manual) {
      recoveryAttemptsRef.current = 0;
      wantsPlaybackRef.current = true;
    }
    if (recoveryAttemptsRef.current >= STREAM_RECOVERY_LIMIT) {
      setIsBuffering(false);
      setIsRecovering(false);
      setPlaybackError('El stream sigue sin responder. Pulsa Reintentar para volver a conectarlo');
      return;
    }
    recoveryAttemptsRef.current += 1;
    const attempt = recoveryAttemptsRef.current;
    const startAt = element.currentTime || resumeTimeRef.current || stateRef.current.progress || 0;
    const request = requestsRef.current.begin();
    setIsRecovering(true);
    setIsBuffering(true);
    setIsPlaying(false);
    setPlaybackError('');
    try {
      if (!manual) await waitForRecovery(500 * 2 ** (attempt - 1), request.signal);
      const { data } = await api.get('/stream', {
        params: { url: media._ytUrl, refresh: true }, signal: request.signal,
      });
      if (!request.isCurrent()) return;
      const refreshed = streamMedia(media, data);
      refreshed._streamUrl = retryStreamPath(refreshed._streamUrl, sourceRevisionRef.current + 1);
      loadTrack(refreshed, {
        pushHistory: false, startAt, autoplay: wantsPlaybackRef.current, recovering: true,
      });
    } catch (error) {
      if (!request.isCurrent() || isCanceledRequest(error)) return;
      setIsRecovering(false);
      setIsBuffering(false);
      setPlaybackError(getApiError(error, 'No se pudo recuperar el stream. Pulsa Reintentar'));
    } finally {
      request.finish();
    }
  }, [loadTrack]);

  const retryPlayback = useCallback(() => {
    if (stateRef.current.currentMedia?._ytUrl) void recoverStream({ manual: true });
    else if (stateRef.current.currentMedia) {
      loadTrack(stateRef.current.currentMedia, {
        pushHistory: false, startAt: stateRef.current.progress || 0,
      });
    }
  }, [loadTrack, recoverStream]);

  const takeNext = useCallback(() => {
    const items = stateRef.current.queue;
    if (!items.length) return null;
    const index = stateRef.current.shuffle
      ? Math.floor(Math.random() * items.length)
      : 0;
    const next = items[index];
    setQueue((current) => [
      ...current.slice(0, index),
      ...current.slice(index + 1),
    ]);
    return next;
  }, []);

  const handleEnded = useCallback(() => {
    const element = mediaRef.current;
    const { repeat: repeatMode, history: played, currentMedia: current } = stateRef.current;
    if (!element) return;

    if (repeatMode === 'one') {
      element.currentTime = 0;
      void playElement();
      return;
    }

    const next = takeNext();
    if (next) {
      loadTrack(next);
      return;
    }

    if (repeatMode === 'all' && current) {
      const cycle = [...played, current];
      if (cycle.length) {
        setHistory([]);
        setQueue(cycle.slice(1));
        loadTrack(cycle[0], { pushHistory: false });
        return;
      }
    }

    setIsPlaying(false);
    setProgress(duration || 0);
  }, [duration, loadTrack, playElement, takeNext]);

  useEffect(() => {
    const element = mediaRef.current;
    if (!element) return undefined;

    const onLoadedMetadata = () => {
      setDuration(Number.isFinite(element.duration) ? element.duration : 0);
      const resumeAt = Math.min(
        Math.max(0, resumeTimeRef.current),
        Number.isFinite(element.duration) ? Math.max(0, element.duration - 0.25) : Infinity,
      );
      if (resumeAt > 0) element.currentTime = resumeAt;
      resumeTimeRef.current = 0;
      setIsBuffering(false);
    };
    const onTimeUpdate = () => {
      if (!requestsRef.current.pending) setProgress(element.currentTime || 0);
    };
    const onPlaying = () => {
      setIsPlaying(true);
      setIsBuffering(false);
      setPlaybackError('');
    };
    const onPause = () => setIsPlaying(false);
    const onPlay = () => { wantsPlaybackRef.current = true; };
    const onWaiting = () => setIsBuffering(true);
    const onCanPlay = () => setIsBuffering(false);
    const onError = () => {
      setIsPlaying(false);
      if (stateRef.current.currentMedia?._ytUrl) {
        void recoverStream();
        return;
      }
      setIsBuffering(false);
      setPlaybackError('El archivo no está disponible o usa un formato incompatible');
    };

    element.addEventListener('loadedmetadata', onLoadedMetadata);
    element.addEventListener('durationchange', onLoadedMetadata);
    element.addEventListener('timeupdate', onTimeUpdate);
    element.addEventListener('playing', onPlaying);
    element.addEventListener('play', onPlay);
    element.addEventListener('pause', onPause);
    element.addEventListener('waiting', onWaiting);
    element.addEventListener('stalled', onWaiting);
    element.addEventListener('canplay', onCanPlay);
    element.addEventListener('ended', handleEnded);
    element.addEventListener('error', onError);

    return () => {
      element.removeEventListener('loadedmetadata', onLoadedMetadata);
      element.removeEventListener('durationchange', onLoadedMetadata);
      element.removeEventListener('timeupdate', onTimeUpdate);
      element.removeEventListener('playing', onPlaying);
      element.removeEventListener('play', onPlay);
      element.removeEventListener('pause', onPause);
      element.removeEventListener('waiting', onWaiting);
      element.removeEventListener('stalled', onWaiting);
      element.removeEventListener('canplay', onCanPlay);
      element.removeEventListener('ended', handleEnded);
      element.removeEventListener('error', onError);
    };
  }, [handleEnded, recoverStream]);

  useEffect(() => {
    const element = mediaRef.current;
    if (!element || !currentMedia) return;
    element.volume = volume;
    element.playbackRate = playbackRate;
    if (!element.src) {
      element.src = sourceFor(currentMedia);
      element.load();
    }
  }, [currentMedia, playbackRate, volume]);

  useEffect(() => {
    store(PREFERENCE_KEY, { volume, playbackRate, shuffle, repeat });
  }, [volume, playbackRate, repeat, shuffle]);

  useEffect(() => {
    const timeout = window.setTimeout(() => {
      store(SESSION_KEY, {
        currentMedia,
        queue,
        history: history.slice(-HISTORY_LIMIT),
        progress,
      });
    }, 300);
    return () => window.clearTimeout(timeout);
  }, [currentMedia, history, progress, queue]);

  useEffect(() => {
    if (!sleepEndsAt) return undefined;
    const remaining = Math.max(0, sleepEndsAt - Date.now());
    const timeout = window.setTimeout(() => {
      pausePlayback();
      setSleepEndsAt(null);
    }, remaining);
    return () => window.clearTimeout(timeout);
  }, [pausePlayback, sleepEndsAt]);

  useEffect(() => () => {
    requestsRef.current.cancel();
    const element = mediaRef.current;
    if (element) {
      element.pause();
      element.removeAttribute('src');
      element.load();
    }
  }, []);

  const playMedia = useCallback((media) => {
    const element = mediaRef.current;
    if (!element) return;
    if (mediaIdentity(stateRef.current.currentMedia) === mediaIdentity(media)) {
      requestsRef.current.cancel();
      setIsRecovering(false);
      if (element.paused) void playElement();
      else pausePlayback();
      return;
    }
    loadTrack(media);
  }, [loadTrack, pausePlayback, playElement]);

  const streamFromUrl = useCallback(async (searchResult) => {
    const request = requestsRef.current.begin();
    setIsRecovering(false);
    try {
      const { data } = await api.get('/stream', {
        params: { url: searchResult.url }, signal: request.signal,
      });
      if (!request.isCurrent()) return false;
      loadTrack(streamMedia(searchResult, data));
      return true;
    } catch (error) {
      if (!request.isCurrent() || isCanceledRequest(error)) return false;
      throw error;
    } finally {
      request.finish();
    }
  }, [loadTrack]);

  const togglePlay = useCallback(() => {
    const element = mediaRef.current;
    if (!element || !stateRef.current.currentMedia) return;
    if (requestsRef.current.pending) {
      pausePlayback();
      return;
    }
    if (element.error || playbackError) {
      retryPlayback();
      return;
    }
    if (element.paused) void playElement();
    else pausePlayback();
  }, [pausePlayback, playbackError, playElement, retryPlayback]);

  const seek = useCallback((seconds) => {
    const element = mediaRef.current;
    if (!element) return;
    const maximum = Number.isFinite(element.duration) ? element.duration : Number(seconds);
    const next = Math.min(maximum, Math.max(0, Number(seconds) || 0));
    element.currentTime = next;
    setProgress(next);
  }, []);

  const seekBy = useCallback((delta) => {
    const element = mediaRef.current;
    if (element) seek((element.currentTime || 0) + delta);
  }, [seek]);

  const setVolume = useCallback((value) => {
    const next = Math.min(1, Math.max(0, Number(value) || 0));
    if (next > 0) previousVolumeRef.current = next;
    setVolumeState(next);
  }, []);

  const toggleMute = useCallback(() => {
    setVolumeState((current) => {
      if (current > 0) {
        previousVolumeRef.current = current;
        return 0;
      }
      return previousVolumeRef.current || 0.7;
    });
  }, []);

  const setPlaybackRate = useCallback((value) => {
    const next = PLAYBACK_RATES.includes(Number(value)) ? Number(value) : 1;
    setPlaybackRateState(next);
    if (mediaRef.current) mediaRef.current.playbackRate = next;
  }, []);

  const skipNext = useCallback(() => {
    const next = takeNext();
    if (next) loadTrack(next);
  }, [loadTrack, takeNext]);

  const skipPrev = useCallback(() => {
    const element = mediaRef.current;
    if (!element) return;
    const { history: played, currentMedia: current } = stateRef.current;
    if (element.currentTime > 3 || !played.length) {
      seek(0);
      return;
    }
    const previous = played[played.length - 1];
    setHistory((items) => items.slice(0, -1));
    if (current) setQueue((items) => [current, ...items]);
    loadTrack(previous, { pushHistory: false });
  }, [loadTrack, seek]);

  const playNext = useCallback((media) => {
    setQueue((items) => [media, ...items]);
  }, []);

  const playLater = useCallback((media) => {
    setQueue((items) => [...items, media]);
  }, []);

  const removeFromQueue = useCallback((index) => {
    setQueue((items) => [...items.slice(0, index), ...items.slice(index + 1)]);
  }, []);

  const clearQueue = useCallback(() => setQueue([]), []);

  const moveInQueue = useCallback((from, to) => {
    setQueue((items) => {
      if (
        from === to
        || from < 0
        || from >= items.length
        || to < 0
        || to >= items.length
      ) return items;
      const next = [...items];
      const [item] = next.splice(from, 1);
      next.splice(to, 0, item);
      return next;
    });
  }, []);

  const playQueueIndex = useCallback((index) => {
    const item = stateRef.current.queue[index];
    if (!item) return;
    setQueue((items) => [...items.slice(0, index), ...items.slice(index + 1)]);
    loadTrack(item);
  }, [loadTrack]);

  const playAll = useCallback((media, startIndex = 0, options = {}) => {
    if (!media.length) return;
    let list = [...media];
    let start = Math.min(Math.max(0, startIndex), list.length - 1);
    if (options.shuffled) {
      for (let index = list.length - 1; index > 0; index -= 1) {
        const random = Math.floor(Math.random() * (index + 1));
        [list[index], list[random]] = [list[random], list[index]];
      }
      start = 0;
    }
    const first = list[start];
    setHistory([]);
    setQueue([...list.slice(start + 1), ...list.slice(0, start)]);
    loadTrack(first, { pushHistory: false });
  }, [loadTrack]);

  const toggleShuffle = useCallback(() => setShuffle((value) => !value), []);
  const cycleRepeat = useCallback(() => {
    setRepeat((value) => (value === 'off' ? 'all' : value === 'all' ? 'one' : 'off'));
  }, []);

  const setSleepTimer = useCallback((minutes) => {
    const numeric = Number(minutes);
    setSleepEndsAt(numeric > 0 ? Date.now() + numeric * 60_000 : null);
  }, []);

  const openVideo = useCallback(() => {
    if (currentMedia?.type === 'video') setVideoOpen(true);
  }, [currentMedia]);

  const togglePictureInPicture = useCallback(async () => {
    const element = mediaRef.current;
    if (!element || currentMedia?.type !== 'video') return;
    try {
      if (document.pictureInPictureElement) await document.exitPictureInPicture();
      else if (document.pictureInPictureEnabled) await element.requestPictureInPicture();
    } catch {
      setPlaybackError('Picture in Picture no está disponible');
    }
  }, [currentMedia]);

  useEffect(() => {
    if (!('mediaSession' in navigator)) return;
    if (!currentMedia) {
      navigator.mediaSession.metadata = null;
      return;
    }
    const artwork = artworkFor(currentMedia);
    navigator.mediaSession.metadata = new window.MediaMetadata({
      title: currentMedia.name || currentMedia.title || '',
      artist: currentMedia.channel || currentMedia.artist || 'YT Ultimate',
      album: currentMedia.album || '',
      artwork: artwork ? [{ src: artwork }] : [],
    });
  }, [currentMedia]);

  useEffect(() => {
    if (!('mediaSession' in navigator)) return;
    navigator.mediaSession.playbackState = currentMedia
      ? (isPlaying ? 'playing' : 'paused')
      : 'none';
    if (duration > 0 && progress >= 0 && progress <= duration) {
      try {
        navigator.mediaSession.setPositionState({
          duration,
          playbackRate,
          position: Math.min(progress, duration),
        });
      } catch {
        // Chromium may reject position state while media metadata is changing.
      }
    }
  }, [currentMedia, duration, isPlaying, playbackRate, progress]);

  useEffect(() => {
    if (!('mediaSession' in navigator)) return undefined;
    const mediaSession = navigator.mediaSession;
    const safeHandler = (action, handler) => {
      try {
        mediaSession.setActionHandler(action, handler);
      } catch {
        // Some actions are not supported on older macOS releases.
      }
    };
    safeHandler('play', playElement);
    safeHandler('pause', pausePlayback);
    safeHandler('previoustrack', skipPrev);
    safeHandler('nexttrack', skipNext);
    safeHandler('seekbackward', (event) => seekBy(-(event.seekOffset || 10)));
    safeHandler('seekforward', (event) => seekBy(event.seekOffset || 10));
    safeHandler('seekto', (event) => {
      if (typeof event.seekTime === 'number') seek(event.seekTime);
    });
    return () => {
      [
        'play',
        'pause',
        'previoustrack',
        'nexttrack',
        'seekbackward',
        'seekforward',
        'seekto',
      ].forEach((action) => {
        try {
          mediaSession.setActionHandler(action, null);
        } catch {
          // Ignore unsupported action cleanup.
        }
      });
    };
  }, [pausePlayback, playElement, seek, seekBy, skipNext, skipPrev]);

  const value = useMemo(() => ({
    currentMedia,
    isPlaying,
    isBuffering,
    isRecovering,
    playbackError,
    retryPlayback,
    progress,
    duration,
    volume,
    playbackRate,
    playbackRates: PLAYBACK_RATES,
    shuffle,
    repeat,
    queue,
    history,
    isStreaming,
    sleepEndsAt,
    videoOpen,
    playMedia,
    togglePlay,
    seek,
    seekBy,
    setVolume,
    toggleMute,
    setPlaybackRate,
    streamFromUrl,
    skipNext,
    skipPrev,
    playNext,
    playLater,
    removeFromQueue,
    clearQueue,
    moveInQueue,
    playQueueIndex,
    playAll,
    toggleShuffle,
    cycleRepeat,
    setSleepTimer,
    openVideo,
    closeVideo: () => setVideoOpen(false),
    togglePictureInPicture,
  }), [
    clearQueue,
    currentMedia,
    cycleRepeat,
    duration,
    history,
    isBuffering,
    isRecovering,
    isPlaying,
    isStreaming,
    moveInQueue,
    openVideo,
    playbackError,
    playbackRate,
    playAll,
    playLater,
    playMedia,
    playNext,
    playQueueIndex,
    progress,
    queue,
    removeFromQueue,
    repeat,
    retryPlayback,
    seek,
    seekBy,
    setPlaybackRate,
    setSleepTimer,
    setVolume,
    shuffle,
    sleepEndsAt,
    skipNext,
    skipPrev,
    streamFromUrl,
    toggleMute,
    togglePictureInPicture,
    togglePlay,
    toggleShuffle,
    videoOpen,
    volume,
  ]);

  return (
    <PlayerContext.Provider value={value}>
      {children}
      <div
        className={`video-stage ${videoOpen && currentMedia?.type === 'video' ? 'video-stage--open' : ''}`}
        role={videoOpen ? 'dialog' : undefined}
        aria-label={videoOpen ? 'Reproductor de vídeo' : undefined}
        aria-hidden={!videoOpen}
      >
        <div className="video-stage__chrome">
          <span className="video-stage__title">{currentMedia?.name || 'Vídeo'}</span>
          <div className="video-stage__actions">
            <button type="button" onClick={togglePictureInPicture}>Picture in Picture</button>
            <button type="button" onClick={() => setVideoOpen(false)}>Cerrar</button>
          </div>
        </div>
        <video
          ref={mediaRef}
          className="video-stage__media"
          playsInline
          controls={videoOpen && currentMedia?.type === 'video'}
          preload="metadata"
        />
      </div>
    </PlayerContext.Provider>
  );
}

export function usePlayer() {
  const context = useContext(PlayerContext);
  if (!context) throw new Error('usePlayer must be used within PlayerProvider');
  return context;
}
