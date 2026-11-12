const browserFallback = {
  apiBaseUrl: import.meta.env.VITE_API_BASE_URL || 'http://127.0.0.1:8000/api/v1',
  apiOrigin: import.meta.env.VITE_API_ORIGIN || 'http://127.0.0.1:8000',
  authToken: import.meta.env.VITE_API_TOKEN || '',
  isDesktop: false,
  platform: 'web',
  appVersion: import.meta.env.VITE_APP_VERSION || 'dev',
};

const desktopRuntime = typeof window !== 'undefined' ? window.ytuDesktop?.runtime : null;

export const runtime = Object.freeze({
  ...browserFallback,
  ...(desktopRuntime || {}),
});

export function apiUrl(path = '', query = undefined) {
  const normalized = path ? `/${String(path).replace(/^\/+/, '')}` : '';
  const url = new URL(`${runtime.apiBaseUrl}${normalized}`);
  if (query) {
    Object.entries(query).forEach(([key, value]) => {
      if (value !== undefined && value !== null && value !== '') {
        url.searchParams.set(key, String(value));
      }
    });
  }
  return url.toString();
}

export function resourceUrl(path) {
  if (!path) return '';
  if (/^(https?:|blob:|data:)/i.test(path)) return path;
  const url = new URL(path, runtime.apiOrigin);
  if (runtime.authToken) url.searchParams.set('token', runtime.authToken);
  return url.toString();
}
