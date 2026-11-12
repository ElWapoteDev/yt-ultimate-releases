export const STREAM_RECOVERY_LIMIT = 2;

// Async results only belong to the selection that created them. Cancellation
// alone is insufficient: a transport may resolve after its signal is aborted.
export class PlaybackRequests {
  #current = null;

  get pending() { return this.#current !== null; }

  begin() {
    this.cancel();
    const controller = new AbortController();
    this.#current = controller;
    return {
      signal: controller.signal,
      isCurrent: () => this.#current === controller && !controller.signal.aborted,
      finish: () => {
        if (this.#current === controller) this.#current = null;
      },
    };
  }

  cancel() {
    this.#current?.abort();
    this.#current = null;
  }
}

export function isCanceledRequest(error) {
  return error?.name === 'AbortError'
    || error?.name === 'CanceledError'
    || error?.code === 'ERR_CANCELED';
}

export function streamMedia(item, data = {}) {
  const url = item._ytUrl || item.url;
  const identity = item._ytUrl ? (item.id || url) : `stream:${item.id || url}`;
  return {
    ...item,
    id: identity,
    name: data.title || item.name || item.title,
    filename: item.filename || `stream_${item.id || url}`,
    channel: data.channel || item.channel,
    thumbnail: data.thumbnail || item.thumbnail,
    duration: data.duration || item.duration || 0,
    type: 'audio',
    _streamUrl: data.proxy_path || `/stream/proxy?url=${encodeURIComponent(url)}`,
    _ytUrl: url,
    _isStream: true,
  };
}

export function retryStreamPath(path, revision) {
  // Preserve the authenticated local proxy. Signed upstream URLs never belong
  // in persisted player state or in the renderer's media source.
  return `${path}${path.includes('?') ? '&' : '?'}playback_retry=${revision}`;
}

export function waitForRecovery(delay, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => {
      clearTimeout(timer);
      reject(new DOMException('Playback selection changed', 'AbortError'));
    };
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', abort);
      resolve();
    }, delay);
    if (signal.aborted) abort();
    else signal.addEventListener('abort', abort, { once: true });
  });
}
