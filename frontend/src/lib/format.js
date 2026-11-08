import { resourceUrl } from './runtime';

export function formatTime(seconds) {
  if (!seconds || Number.isNaN(seconds)) return '0:00';
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

export function formatDuration(seconds) {
  if (!seconds) return '--:--';
  return formatTime(seconds);
}

export function thumbUrl(file) {
  if (!file?.thumbnail) return '';
  return resourceUrl(file.thumbnail);
}

export function greeting() {
  const h = new Date().getHours();
  if (h < 6) return 'Buenas noches';
  if (h < 12) return 'Buenos días';
  if (h < 20) return 'Buenas tardes';
  return 'Buenas noches';
}
